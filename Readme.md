# ItalianSuperWhisper 🇮🇹

**On-device dictation for macOS and Windows, built around Italian.**

Press a shortcut, speak, and the text is typed into whatever app you have in
front of you. Everything happens **on your machine**: no API keys, no cloud
inference. The network is used only to download models.

A fork of [Starmel/OpenSuperWhisper](https://github.com/Starmel/OpenSuperWhisper).
This branch is a ground-up rewrite on **Tauri 2** (Rust backend, React/TypeScript
UI) so the same app runs on macOS and Windows.

---

## Why "built around Italian"

It does not mean other languages were removed — Whisper still supports all of
them, and they remain selectable. It means **product decisions are made by
looking at Italian**:

- **English loanwords are not errors.** In spoken Italian, "meeting", "call",
  "deadline" and "budget" are normal. They are not translated and not
  "corrected" — they are transcribed as you said them.
- **Automatic Italian correction**, described below.

## The two correction layers

Text coming out of the engine passes through two layers, kept deliberately
separate.

### 1. Deterministic corrector — always on

`italian-corrector` runs on every Italian dictation. No model call,
microseconds, nothing to configure. It contains **only always-true rules**:
accents whose unaccented form is not a word (`perche → perché`), spellings
that are wrong in any context (`pò → po'`, `qual' → qual`), and spacing around
punctuation. Ambiguous cases (`e`/`è`, `si`/`sì`) are deliberately excluded —
a rule that can be wrong is not a rule this layer wants.

### 2. LLM reformulation — opt-in

A local GGUF instruct model (gemma-2-2b-it by default, via llama.cpp) removes
spoken self-corrections: *"domani alle 10, ah no, alle 10 e mezza"* becomes
*"domani alle 10 e mezza"*. Off by default; a multi-hundred-MB download when
you enable it. The design defends one invariant everywhere: **the dictation is
never lost** — empty, overlong or failed rewrites fall back to the raw text,
the raw transcription is saved before anything is pasted, and it is kept in
the history whenever it differs from the rewrite.

## How it works

| Piece | Crate | Notes |
| --- | --- | --- |
| Audio capture | `crates/audio-capture` | cpal, any input device, resampled to 16 kHz mono |
| Transcription | `crates/whisper-engine` | whisper.cpp via whisper-rs, Silero VAD gate, cancellable |
| Italian rules | `crates/italian-corrector` | deterministic, tested 1:1 |
| Reformulation | `crates/reformulation` | llama.cpp (Metal on macOS, CPU elsewhere), strict sanitize contract |
| Paste | `crates/paste-sim` | Cmd+V via CGEvent / Ctrl+V via SendInput, clipboard restored after 1.5s |
| History | `crates/recordings-store` | SQLite, additive migrations |
| Model downloads | `crates/model-manager` | resumable, validated before install |

The app itself (`src-tauri/`) wires these behind a global hotkey
(default **Alt+`**, hold-to-record or tap-to-toggle), a floating recording
indicator, a tray icon and a small settings/history UI.

## Running it

Prereqs: Rust (stable), Node 20+, CMake. macOS additionally needs Xcode
Command Line Tools.

```sh
cd tauri-app
npm install
npm run tauri dev     # development
npm run tauri build   # .app/.dmg (macOS), .msi/NSIS (Windows)
```

First run provisions a bundled `ggml-tiny.en` model; download a proper
multilingual model (Turbo V3) from the in-app model manager for real Italian
use. On macOS, grant **Accessibility** when prompted — without it the paste
keystroke is silently dropped.

Tests:

```sh
cd tauri-app
cargo test --workspace
```

## License

MIT, like the upstream project. See `LICENSE`.
