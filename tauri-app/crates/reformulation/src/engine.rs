//! llama.cpp inference for the reformulation service, replacing the Swift
//! MLX engine (Apple-only) with a cross-platform GGUF backend.
//!
//! The split mirrors the crate layout: `lib.rs` owns the prompt, the decoding
//! constants and [`sanitize`]; this module owns everything that touches a
//! model. The safety contract carries over: [`ReformulationEngine::
//! reformulate_sanitized`] can degrade to returning the dictation unchanged,
//! but it can never lose it.
//!
//! Decoding is greedy ([`TEMPERATURE`] is 0.0 — rewriting wants determinism,
//! not variety), capped at [`MAX_TOKENS`] new tokens, stopping at any
//! end-of-generation token.
//!
//! # Integration test
//!
//! `tests/real_model.rs` holds an `#[ignore]`d test that runs the three
//! HANDOFF.md sentences through a real model:
//!
//! ```sh
//! REFORMULATION_TEST_MODEL=/path/to/gemma-2-2b-it-Q4_K_M.gguf \
//!     cargo test -p reformulation --test real_model -- --ignored --nocapture
//! ```

use std::fs::File;
use std::io::Read;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use crate::{sanitize, INSTRUCTIONS, MAX_TOKENS};

/// Every GGUF file starts with these four bytes (`gguf_magic` in ggml).
const GGUF_MAGIC: [u8; 4] = *b"GGUF";

/// Smallest plausible instruct model. Same reasoning as the ggml check in
/// `whisper-engine`: captive-portal pages, CDN error bodies and Git-LFS
/// pointer files are all far under 1 MB.
const MINIMUM_PLAUSIBLE_MODEL_SIZE: u64 = 1_048_576;

/// Fits the ~300-token system prompt, a dictation (a dictation is spoken
/// text — a few hundred tokens at the outside) and [`MAX_TOKENS`] of output
/// with generous slack, while keeping the KV cache small.
const N_CTX: u32 = 4096;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("the model file is only {0} bytes; that is an error page or a Git-LFS pointer, not a model")]
    TooSmall(u64),
    #[error("{0} is not a GGUF model file")]
    NotAGgufFile(PathBuf),
    #[error("failed to initialize the llama.cpp backend: {0}")]
    Backend(String),
    #[error("failed to load the model: {0}")]
    ModelLoad(String),
    #[error("failed to create the llama.cpp context: {0}")]
    Context(String),
    #[error("failed to tokenize the prompt: {0}")]
    Tokenize(String),
    #[error("the prompt is {got} tokens but the engine can accept at most {max}")]
    PromptTooLong { got: usize, max: usize },
    #[error("llama.cpp failed while generating: {0}")]
    Decode(String),
    #[error("failed to read the model file: {0}")]
    Io(#[from] std::io::Error),
}

/// Checks that `path` plausibly holds a GGUF model: at least 1 MB and
/// starting with the `GGUF` magic. The analogue of `whisper-engine`'s ggml
/// check — it exists so a bad download is rejected with a clear message
/// instead of a cryptic llama.cpp load failure.
pub fn validate_gguf(path: &Path) -> Result<(), EngineError> {
    let size = std::fs::metadata(path)?.len();
    if size < MINIMUM_PLAUSIBLE_MODEL_SIZE {
        return Err(EngineError::TooSmall(size));
    }
    let mut head = [0u8; 4];
    File::open(path)?.read_exact(&mut head)?;
    if head != GGUF_MAGIC {
        return Err(EngineError::NotAGgufFile(path.to_path_buf()));
    }
    Ok(())
}

/// The Gemma-2 chat format, written out by hand. Gemma has no system role,
/// so the instructions are prepended to the single user turn — exactly what
/// llama.cpp's built-in gemma template does with a system message.
///
/// This is both the fallback when the model ships no usable chat template
/// and the pure, model-free function the prompt-shape tests run against.
pub fn build_prompt(dictation: &str) -> String {
    format!(
        "<start_of_turn>user\n{INSTRUCTIONS}\n\n{}<end_of_turn>\n<start_of_turn>model\n",
        dictation.trim()
    )
}

/// llama.cpp allows exactly one live backend per process, so it lives in a
/// process-wide static rather than in the engine. The mutex only serializes
/// first-time initialization; steady-state lookups don't touch it.
fn backend() -> Result<&'static LlamaBackend, EngineError> {
    static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();
    static INIT: Mutex<()> = Mutex::new(());

    if let Some(backend) = BACKEND.get() {
        return Ok(backend);
    }
    let _guard = INIT.lock().expect("backend init lock poisoned");
    if let Some(backend) = BACKEND.get() {
        return Ok(backend);
    }
    let mut backend = LlamaBackend::init().map_err(|e| EngineError::Backend(e.to_string()))?;
    // llama.cpp logs every layer of every model load to stderr; a dictation
    // app has no use for that, and real failures surface as Err anyway.
    backend.void_logs();
    Ok(BACKEND.get_or_init(|| backend))
}

/// A loaded GGUF instruct model. Construction reads the whole model into
/// memory (seconds, gigabytes) — callers decide when to pay that; this type
/// does nothing lazily.
///
/// One llama.cpp context is created per [`reformulate`](Self::reformulate)
/// call. Contexts are cheap next to a dictation-length generation, and a
/// fresh context means a fresh KV cache: no state leaks between dictations.
pub struct ReformulationEngine {
    model: LlamaModel,
    backend: &'static LlamaBackend,
}

impl ReformulationEngine {
    pub fn load(model_path: &Path) -> Result<Self, EngineError> {
        validate_gguf(model_path)?;
        let backend = backend()?;

        let model_params = LlamaModelParams::default();
        // Offload every layer to Metal; a 2-4B model fits comfortably.
        // Elsewhere the crate is built without a GPU feature, so the
        // parameter would be ignored and is left at its default.
        #[cfg(target_os = "macos")]
        let model_params = model_params.with_n_gpu_layers(1_000_000);

        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .map_err(|e| EngineError::ModelLoad(e.to_string()))?;
        Ok(Self { model, backend })
    }

    /// Runs one dictation through the model and returns the *raw* generated
    /// text. The caller is expected to pass it through [`sanitize`] (or use
    /// [`reformulate_sanitized`](Self::reformulate_sanitized)).
    pub fn reformulate(&mut self, dictation: &str) -> Result<String, EngineError> {
        let prompt = self.render_prompt(dictation);

        let tokens = self
            .model
            .str_to_token(&prompt, AddBos::Always)
            .map_err(|e| EngineError::Tokenize(e.to_string()))?;

        let mut ctx = self
            .model
            .new_context(
                self.backend,
                LlamaContextParams::default().with_n_ctx(NonZeroU32::new(N_CTX)),
            )
            .map_err(|e| EngineError::Context(e.to_string()))?;

        // The prompt must fit in one decode call (n_batch) and must leave
        // room in the context window for MAX_TOKENS of output.
        let max_prompt = (ctx.n_batch() as usize).min(ctx.n_ctx() as usize - MAX_TOKENS as usize);
        if tokens.len() > max_prompt {
            return Err(EngineError::PromptTooLong {
                got: tokens.len(),
                max: max_prompt,
            });
        }

        let mut batch = LlamaBatch::new(tokens.len().max(1), 1);
        batch
            .add_sequence(&tokens, 0, false)
            .map_err(|e| EngineError::Decode(e.to_string()))?;
        ctx.decode(&mut batch)
            .map_err(|e| EngineError::Decode(e.to_string()))?;

        // Greedy = the TEMPERATURE 0.0 the Swift engine ran with: rewriting
        // is deterministic work, sampling would only add variation.
        let mut sampler = LlamaSampler::greedy();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut output = String::new();
        let mut pos = tokens.len() as i32;

        for _ in 0..MAX_TOKENS {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            if self.model.is_eog_token(token) {
                break;
            }
            let piece = self
                .model
                .token_to_piece(token, &mut decoder, false, None)
                .map_err(|e| EngineError::Decode(e.to_string()))?;
            output.push_str(&piece);

            batch.clear();
            batch
                .add(token, pos, &[0], true)
                .map_err(|e| EngineError::Decode(e.to_string()))?;
            pos += 1;
            ctx.decode(&mut batch)
                .map_err(|e| EngineError::Decode(e.to_string()))?;
        }

        Ok(output)
    }

    /// [`reformulate`](Self::reformulate) followed by [`sanitize`], falling
    /// back to the dictation itself on *any* engine error. This is the
    /// crate's safety contract end to end: whatever breaks — a poisoned
    /// context, a decode failure, a model answering instead of rewriting —
    /// the user's dictation survives.
    pub fn reformulate_sanitized(&mut self, dictation: &str) -> String {
        match self.reformulate(dictation) {
            Ok(raw) => sanitize(&raw, dictation),
            Err(_) => dictation.to_string(),
        }
    }

    /// Prefer the chat template embedded in the GGUF (correct for whatever
    /// model the user downloaded); fall back to the hand-written Gemma-2
    /// format. For Gemma models the two agree: llama.cpp's gemma template
    /// merges the system message into the user turn, exactly like
    /// [`build_prompt`].
    fn render_prompt(&self, dictation: &str) -> String {
        let messages = [
            LlamaChatMessage::new("system".into(), INSTRUCTIONS.into()),
            LlamaChatMessage::new("user".into(), dictation.trim().into()),
        ];
        if let [Ok(system), Ok(user)] = messages {
            if let Ok(template) = self.model.chat_template(None) {
                if let Ok(prompt) =
                    self.model
                        .apply_chat_template(&template, &[system, user], true)
                {
                    return prompt;
                }
            }
        }
        build_prompt(dictation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // validate_gguf

    fn write_temp(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    fn plausibly_sized(head: &[u8; 4]) -> Vec<u8> {
        let mut bytes = vec![0u8; MINIMUM_PLAUSIBLE_MODEL_SIZE as usize + 16];
        bytes[0..4].copy_from_slice(head);
        bytes
    }

    #[test]
    fn accepts_large_file_with_gguf_magic() {
        let f = write_temp(&plausibly_sized(b"GGUF"));
        validate_gguf(f.path()).unwrap();
    }

    #[test]
    fn rejects_small_file_even_with_magic() {
        let f = write_temp(b"GGUFtiny");
        assert!(matches!(
            validate_gguf(f.path()).unwrap_err(),
            EngineError::TooSmall(8)
        ));
    }

    #[test]
    fn rejects_large_file_without_magic() {
        // A ggml whisper model is exactly the plausible-looking wrong file.
        let f = write_temp(&plausibly_sized(b"lmgg"));
        assert!(matches!(
            validate_gguf(f.path()).unwrap_err(),
            EngineError::NotAGgufFile(_)
        ));
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let err = validate_gguf(Path::new("/nonexistent/model.gguf")).unwrap_err();
        assert!(matches!(err, EngineError::Io(_)));
    }

    // build_prompt — the shape the model actually sees. Gemma-2 format:
    // no system role, instructions folded into the user turn.

    #[test]
    fn prompt_is_a_single_user_turn_ending_with_an_open_model_turn() {
        let prompt = build_prompt("sposta la call");
        assert!(prompt.starts_with("<start_of_turn>user\n"));
        assert!(prompt.ends_with("<end_of_turn>\n<start_of_turn>model\n"));
        assert_eq!(prompt.matches("<start_of_turn>").count(), 2);
    }

    #[test]
    fn prompt_puts_the_instructions_before_the_dictation() {
        let prompt = build_prompt("sposta la call");
        let instructions_at = prompt.find(INSTRUCTIONS).expect("instructions missing");
        let dictation_at = prompt.find("sposta la call").expect("dictation missing");
        assert!(instructions_at < dictation_at);
    }

    /// Whisper output arrives with stray whitespace; the prompt must not
    /// smuggle it into the turn structure.
    #[test]
    fn prompt_trims_the_dictation() {
        let prompt = build_prompt("  sposta la call \n");
        assert!(prompt.contains("sposta la call<end_of_turn>"));
    }
}
