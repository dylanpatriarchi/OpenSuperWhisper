# Tauri rewrite (Windows + macOS)

Plan for porting ItalianSuperWhisper from a native macOS Swift/SwiftUI app to a
Tauri 2.x app (Rust backend + React/TypeScript webview), to support Windows
alongside macOS. Lives on `feature/tauri-rewrite`, branched from `main` at
`ed5c01c`. This is a **parallel rewrite, not a migration** — the Swift app on
`main` is untouched, keeps its own bundle id, and there is no expectation of
migrating an existing Swift installation's `recordings.sqlite` or downloaded
models into the new app.

## Decisions already made

- **Scope**: full feature parity target (core dictation, `ItalianTextCorrector`,
  LLM reformulation), not an MVP-only first cut.
- **Reformulation engine**: MLX (`mlx-community/gemma-4-e2b-it-4bit`) is
  Apple-Silicon/Metal-only and has no Windows equivalent. Replace with
  **llama.cpp + a GGUF model**, chosen empirically in Phase 7.
- **Frontend**: React + TypeScript.
- **App identity**: new Tauri app gets its own identifier
  (`com.dylanpatriarchi.italiansuperwhisper`, `tauri-app/src-tauri/tauri.conf.json`),
  deliberately different from the Swift app's legacy
  `ru.starmel.OpenSuperWhisper` (see the Swift-era docs on `main`).
- **`FluidAudioEngine`** (the alternate CoreML-based ASR engine, Swift-only,
  no cross-platform equivalent) is dropped from scope. whisper.cpp becomes the
  only ASR engine — this is implied by the Windows-compat goal itself, not an
  extra cut.
- **`agent/`** (original author's automation harness) was removed from this
  branch along with the rest of the Swift-era tree; it still exists on `main`.

## Still open (decide when the relevant phase starts, not before)

- ~~Exact GGUF model for reformulation~~ **decided**: `gemma-2-2b-it`
  Q4_K_M (bartowski GGUF build), validated empirically against the three
  canonical test sentences (see phase 7 below).
- `whisper-rs` crate vs hand-rolled bindgen against `Bridge.h`/`whisper.h` —
  depends on whether `whisper-rs` exposes the VAD API
  (`whisper_vad_init_from_file_with_params`, `whisper_vad_segments_from_samples`)
  and custom abort/progress callbacks that `WhisperEngine.swift` relies on.
- Whether to vendor the existing `libwhisper/whisper.cpp` submodule pin into
  the Rust build or let a crate pull its own bundled whisper.cpp.
- macOS native glue language for CGEventTap/Carbon-hotkey/AX-caret/NSPanel-level
  code: pure Rust (`core-graphics`, `objc2`/`objc2-app-kit`) vs a small Swift/ObjC
  static lib FFI'd from Rust. Attempt pure Rust first; unproven until Phase 4.
- Code signing (macOS Developer ID, Windows Authenticode) — cost/process
  decision, not technical, revisit at Phase 8.

## Reusable near-1:1 via Rust FFI

- `libwhisper/whisper.cpp` (git submodule + CMake build) — same submodule
  builds on Windows/Linux with CPU/CUDA/Vulkan backends instead of Metal.
- ggml `.bin` model validation (magic `0x67676d6c`, min size 1MB) and the
  resumable-download logic in `WhisperModelManager.swift`.
- Silero VAD (`ggml-silero-v5.1.2.bin`) — same whisper.cpp C API.
- llama.cpp itself — C/C++, mature Rust bindings, used for reformulation.

## Needs a full rewrite (macOS-only APIs, no Windows equivalent)

- **Audio capture**: `AudioRecorder.swift` (`AVAudioRecorder`) and
  `MicrophoneService.swift` (`AVCaptureDevice` + raw CoreAudio device
  enumeration/Bluetooth-transport-type detection) → a Rust `cpal`-based
  capture pipeline. Bluetooth/Continuity-mic heuristics have no WASAPI
  equivalent — use a simplified cross-platform fallback (retry-on-empty-buffer)
  instead of device-name sniffing.
- **Audio resampling**: `WhisperEngine.swift`'s `AVAudioFile`/`AVAudioConverter`
  usage → `symphonia` (decode) + `rubato` (resample) or `hound` (WAV).
- **Global hotkeys**: `ShortcutManager.swift` + `ModifierKeyMonitor.swift`
  (CGEventTap on `.cgSessionEventTap`) + `MouseButtonMonitor.swift` (CGEventTap
  on `.defaultTap`) — three trigger modes (regular shortcut, modifier-only
  hold, mouse-button hold) unlikely to be covered out of the box by crates like
  `global-hotkey`/`rdev`. Expect to hand-roll via `core-graphics` event taps on
  macOS and `SetWindowsHookEx`(WH_KEYBOARD_LL/WH_MOUSE_LL) via the `windows`
  crate on Windows, behind a shared `HotkeyBackend` trait.
- **Permissions**: `PermissionsManager.swift` (mic, Accessibility via
  `AXIsProcessTrusted()`, Input Monitoring via `IOHIDCheckAccess`) → a
  `PermissionsBackend` trait; Windows side is just mic consent.
- **Paste simulation**: `Utils/ClipboardUtil.swift` — CGEvent-synthesized
  Cmd+V with keyboard-layout-aware keycode resolution (`TISCopyCurrentKeyboardInputSource`/
  `UCKeyTranslate`) → `PasteSimulator` trait; Windows via `SendInput` (Ctrl+V,
  no layout complexity needed). The OS-agnostic half (1.5s deferred restore
  guarded by clipboard change-count, burst-dictation coalescing) should be
  written once in shared Rust code, not duplicated per OS.
- **Floating recording indicator**: `Indicator/IndicatorWindowManager.swift`
  (borderless/click-through/always-on-top/all-spaces `NSPanel`) → a Tauri
  `WebviewWindow` plus native HWND tweaks on Windows (`WS_EX_TRANSPARENT|WS_EX_LAYERED`)
  and NSPanel-level tweaks on macOS, reachable via Tauri's `hwnd()`/`ns_window()`.
  Caret-position anchoring (`FocusUtils.swift`, Accessibility APIs) has no exact
  Windows equivalent — try `IUIAutomation`/`GetGUIThreadInfo`, or fall back to
  centering near the cursor.
- **Tray/dock lifecycle**: `OpenSuperWhisperApp.swift`'s `NSStatusItem` +
  dynamic `.accessory`/`.regular` policy switching — low risk, Tauri has
  built-in `tauri::tray` + dock/taskbar visibility APIs for this.

## Fully portable to React/TS (no native complexity)

`ContentView.swift`, `Settings.swift` (~1885 lines — model management/download
UI, hotkey config, storage settings), `Onboarding/OnboardingView.swift`,
`FileDropHandler.swift` (Tauri has built-in drag-drop events).

## Data layer

`Models/Recording.swift` (GRDB/SQLite, migrations `v1` → `v2_add_status` →
`v3_add_raw_transcription`, additive/column-existence-checked) → `rusqlite`
with the same additive-migration idiom, stored under Tauri's app-data dir.
`AppPreferences.swift` (flat UserDefaults, ~25 keys) → `tauri-plugin-store`
or a key-value table in the same SQLite DB.

## ReformulationService — safety contract to preserve exactly

Ported into the Rust/llama.cpp reformulation crate in Phase 7, contract locked
by `ReformulationServiceTests.swift`:

1. Trim; empty response → return original.
2. Strip known preamble strings ("Testo riscritto:", "Testo pulito:",
   "Riscrittura:", "Output:").
3. Strip a **balanced** wrapping quote pair only if open/close chars match
   (ASCII `"`, typographic `“ ”`/`« »`) AND no unmatched quote char appears
   inside — never strip quotes from a sentence that merely contains quoted
   speech without being fully wrapped by it.
4. Length ceiling `max(originalLength * 3, 120)` — if the cleaned response
   exceeds it, discard and return the original (guards against the model
   answering/summarizing instead of rewriting).
5. Deterministic decoding (temperature 0.0, max tokens ~512).
6. **Cancellation ordering invariant**: check cancellation after reformulation
   completes but before any audio-file move or DB commit — once that move
   happens there's no cleanup path. Reformulation failure or cancellation must
   always fall back to the raw ASR text, never lose or block it. The save must
   be awaited, not fire-and-forget.
7. `rawTranscription` persisted only when it differs from the final text.

## ItalianTextCorrector — small, low risk

~150 lines, port 1:1 into a Rust module (`HashMap` + `regex` crate):
`unambiguousAccents` table (~40 entries, deliberately excludes ambiguous pairs
like `e`/`è`, `si`/`sì` — keep the exclusion list, don't "helpfully" restore
them), 4 always-wrong-spelling regex rules, whitespace/punctuation
normalization (only before punctuation, never after — avoids corrupting
decimals/URLs), capitalization-preserving accent restoration.

## Phases

Each phase is an independently shippable/testable milestone.

0. **Scaffold** — branch + empty Tauri 2.x shell (`tauri-app/`), coexisting
   with the untouched Swift project. **✅ done**
1. **whisper.cpp FFI proof-of-concept** — a Rust CLI that loads a `.bin` model
   and transcribes a fixed WAV, matching `WhisperEngine.swift`'s call sequence
   (VAD gating, abort/progress callbacks, `[MUSIC]`/`[BLANK_AUDIO]` stripping).
   **✅ done** (`crates/whisper-engine`, verified against jfk.wav incl.
   mid-run abort; note: whisper-rs 0.16.0's `set_abort_callback_safe` has an
   upstream bug worked around in `transcribe.rs` — see the comment there).
2. **macOS core loop** — record → transcribe → paste, minimal UI.
   **✅ done** (`crates/audio-capture` on cpal+rubato, `crates/paste-sim`
   with mock-tested restore/coalescing, Tauri commands + React UI; mic smoke
   test verified against real hardware).
3. **Windows port of the same core loop** — same crates, WASAPI via `cpal`,
   `SendInput`-based paste. **⏳ pending**: CI compiles and tests on
   `windows-latest`; `paste-sim`'s Windows `KeySender` still returns
   `Unsupported` and needs the `SendInput` implementation + a manual test on
   a real Windows machine.
4. **Global hotkeys + floating indicator + tray**, both OSes.
   **🟡 partial**: hold-to-record/toggle hotkey done via
   tauri-plugin-global-shortcut (default Alt+Backquote, 0.3s threshold ported
   from `ShortcutManager.swift`); tray icon with open/quit done. Missing:
   modifier-only and mouse-button trigger modes (need CGEventTap /
   low-level hooks), the floating click-through indicator window, and the
   double-press-to-trigger option.
5. **Settings/data-layer/history UI parity** — **🟡 mostly done**:
   `crates/recordings-store` (SQLite, v1→v3 migrations, 12 tests),
   `crates/model-manager` (catalog + validated resumable downloads, 10
   tests), settings.json persistence, history and model-management UI,
   bundled tiny+VAD models provisioned on first run. Missing: onboarding
   flow, file-drop transcription, audio playback in history, retention
   settings UI.
6. **ItalianTextCorrector port** — **✅ done** (`crates/italian-corrector`,
   12 tests ported 1:1).
7. **llama.cpp reformulation port** — **✅ done**: sanitize() contract and
   prompt ported and tested; llama-cpp-2 engine (Metal on macOS, greedy
   decoding, chat-template with Gemma fallback) wired into the pipeline with
   lazy per-path caching, raw transcription persisted only when it differs.
   **Empirically validated** with gemma-2-2b-it Q4_K_M against the three
   canonical sentences (the "nobody has ever run the model" risk the Swift
   handoff flagged is now closed):
   - *"Sposta la call di domani dopo il meeting con il team, tanto la
     deadline del budget è venerdì."* → unchanged, all anglicisms survive.
   - *"Domani alle 10 non ci sarò, ah no, non è vero, alle 10 e mezza."* →
     *"Domani alle 10.30 non ci sarò."* (self-correction resolved; note the
     stylistic "10.30" normalization of "10 e mezza").
   - Already-clean text → byte-identical.
8. **Polish/packaging** — **🟡 partial**: `.app`/`.dmg` bundle builds and
   launches (storage provisioning verified), real app icon, macOS 14
   minimum, `NSMicrophoneUsageDescription` declared, CI running tests on
   macOS+Windows. Missing: code signing decisions, Windows installer
   validation, release automation.

## Verification per phase

See the phase list above for what "done" means concretely; each phase should
be manually exercised end-to-end on its target OS(es) before moving on, and
pure-logic pieces (`sanitize()`, `ItalianTextCorrector`, ggml validation,
migrations) get unit tests ported 1:1 from the existing Swift test files
(`ReformulationServiceTests.swift`, `ItalianTextCorrectorTests.swift`).
