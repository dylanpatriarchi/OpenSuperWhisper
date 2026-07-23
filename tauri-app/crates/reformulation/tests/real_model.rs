//! Integration test against a real GGUF instruct model. Ignored by default
//! because it needs a multi-gigabyte model on disk.
//!
//! Run it with:
//!
//! ```sh
//! REFORMULATION_TEST_MODEL=/path/to/gemma-2-2b-it-Q4_K_M.gguf \
//!     cargo test -p reformulation --test real_model -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use reformulation::engine::ReformulationEngine;

/// The three sentences from docs/HANDOFF.md "Suggested test sentences":
/// anglicisms must survive, self-corrections must be cleaned, already-clean
/// text must come out unchanged.
const HANDOFF_SENTENCES: [&str; 3] = [
    "Sposta la call di domani dopo il meeting con il team, tanto la deadline del budget è venerdì.",
    "Domani alle 10 non ci sarò, ah no, non è vero, alle 10 e mezza.",
    "Il progetto procede bene e la consegna resta fissata per venerdì.",
];

#[test]
#[ignore = "needs a real GGUF model: set REFORMULATION_TEST_MODEL and run with --ignored"]
fn reformulates_the_handoff_sentences() {
    let model_path = PathBuf::from(
        std::env::var_os("REFORMULATION_TEST_MODEL")
            .expect("set REFORMULATION_TEST_MODEL to a GGUF instruct model path"),
    );
    let mut engine =
        ReformulationEngine::load(&model_path).expect("failed to load the GGUF model");

    for dictation in HANDOFF_SENTENCES {
        let result = engine.reformulate_sanitized(dictation);
        println!("dictation: {dictation}");
        println!("rewritten: {result}\n");
        assert!(
            !result.trim().is_empty(),
            "reformulate_sanitized must never return an empty string"
        );
    }
}
