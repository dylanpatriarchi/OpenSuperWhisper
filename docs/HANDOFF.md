# Handoff

State of the fork as of the Italian-focus work. Read this first if you are
picking the project up cold.

## What this fork is

`dylanpatriarchi/ItalianSuperWhisper` — a fork of `Starmel/OpenSuperWhisper`,
a local macOS dictation app. The goal of the fork is to be good at **Italian**:
strip what Italian does not need, and add local correction of Italian
dictations. Transcription still runs entirely on-device; the network is only
used to download models.

## What landed

Six PRs, all merged to `main`. There is no open work and no stray branch —
`main` is the only branch, local and remote.

- **#1** — verified concurrency, memory-safety and UX bug fixes (cancellation,
  concurrent whisper context, progress-callback lifetime, model-download
  validation, clipboard restore, record-before-loaded, mach-port leak,
  `run.sh` exit status).
- **#2** — focused the fork on Italian: removed the CJK autocorrect and the
  Rust toolchain it dragged in.
- **#3, #4** — rename to ItalianSuperWhisper; README rewritten (later moved to
  English in a follow-up).
- **#5** — the local-LLM reformulation feature (details below), plus the
  fixes its three review rounds surfaced.
- **#6** — scrubbed remaining upstream references, fixed the broken release
  scripts, changed the user-facing name (not the bundle id — see Caveats).

## The two correction layers

Text out of the engine passes through two deliberately separate layers.

1. **`ItalianTextCorrector`** (`OpenSuperWhisper/Utils/ItalianTextCorrector.swift`)
   — deterministic, always-on, microseconds. Only always-true rules
   (accents whose unaccented form is not a word, never-correct spellings,
   spacing). Ambiguous cases (`e`/`è`, `si`/`sì`, …) are excluded on purpose.
   Runs only when the language is explicitly Italian.

2. **`ReformulationService`** (`OpenSuperWhisper/Engines/ReformulationService.swift`)
   — the optional LLM layer, off by default. Removes spoken self-corrections.
   Details below.

## Reformulation: how it is wired

- **Preference**: `reformulationEnabled` in `AppPreferences.swift` (off by
  default). Settings toggle is in the Transcription tab.
- **Model**: `LLMRegistry.gemma4_e2b_it_4bit`
  (`mlx-community/gemma-4-e2b-it-4bit`) via MLX, loaded lazily on first use —
  a multi-GB download. Deterministic decoding (`temperature: 0.0`).
- **Flow**: `IndicatorViewModel.startDecoding()` → `reformulateIfEnabled()`.
  The raw engine output is stored in `Recording.rawTranscription`, the rewrite
  in `Recording.transcription`. Raw is stored only when it differs from the
  rewrite (`IndicatorWindow.swift`, `rawTranscription: text == rawText ? nil : rawText`).
- **Never lose the dictation** — the invariant the whole design defends:
  - `ReformulationService.sanitize` falls back to the original on an empty
    response, a response 3×+ the input length (an *answer*, not a rewrite),
    or a model failure. It strips scaffolding prefixes and wrapping quotes
    (ASCII, `“ ”`, `« »`), but only a quote pair that genuinely wraps the
    whole string.
  - The save is awaited (`RecordingStore.addRecordingSync`), not
    fire-and-forget, so a rewrite is never pasted while its raw text went
    unsaved.
  - Cancellation is real: the decoding task is held in `decodingTask` and
    cancelled by `cleanup()` / `cancelRecording()`, checked before the audio
    is moved or anything is pasted.
- **DB migration**: `v3_add_raw_transcription` in `Recording.swift`, additive
  and nullable — existing v1/v2 databases survive untouched.
- **Prompt**: Italian system prompt in `ReformulationService.instructions`,
  which explicitly forbids translating anglicisms and forbids answering.

## The one thing left to do — and it is not code

**Nobody has ever run the model.** The code is on `main`, reviewed and tested,
but the reformulation has never rewritten a single sentence. The real risk is
behavioural, not structural: a small model may translate anglicisms ("call" →
"chiamata") despite the prompt forbidding it, which would defeat the feature's
purpose. This can only be found by enabling the toggle and dictating.

Suggested test sentences (the first two matter most):

1. Anglicisms must survive: *"Sposta la call di domani dopo il meeting con il
   team, tanto la deadline del budget è venerdì."*
2. Self-correction must be cleaned: *"Domani alle 10 non ci sarò, ah no, non è
   vero, alle 10 e mezza."*
3. Already-clean text must come out unchanged.

## Caveats and known issues

- **Bundle id is deliberately unchanged** (`ru.starmel.OpenSuperWhisper`).
  `Bundle.main.bundleIdentifier` derives the Application Support path holding
  `recordings.sqlite` and the downloaded models (`Recording.swift`,
  `WhisperModelManager.swift`). Renaming it orphans every transcription and
  forces a multi-GB re-download, on top of losing granted Accessibility. That
  is a data migration, not a rename — do it deliberately or not at all. Only
  the *display* name was changed.
- **CI builds but does not test.** `.github/workflows/build.yml` only compiles.
  That is how the microphone-guard bug (fixed in #5) went unnoticed. Adding a
  test step is on the roadmap; it needs the two environment-dependent test
  failures below handled first.
- **Pre-existing failing tests** (not from this work, fail in isolation):
  - `MicrophoneServiceBluetoothTests.testBluetoothDetection_MACAddress`
  - `MicrophoneServiceRequiresConnectionTests.testRequiresConnection_Bluetooth`
    Both live in `MicrophoneService.swift` and only pass with a specific
    Bluetooth device connected (the MAC-address branch queries CoreAudio for a
    device the tests do not create).
  - `ClipboardUtilPasteIntegrationTests` paste tests are flaky under parallel
    execution — they change the system keyboard layout and interfere with each
    other. They pass in isolation.
- **MLX needs the Metal toolchain.** MLX compiles Metal shaders; the build
  fails with `cannot execute tool 'metal'` without it. CI installs it
  (`xcodebuild -downloadComponent MetalToolchain`); a local machine needs it
  once too.
- **`agent/`** is the original author's automation harness, repointed at this
  repo rather than deleted. It is of no real use here — decide separately
  whether it should exist at all.

## Building and running

See the README's "Running it" section (full Xcode, `brew install cmake libomp
ruby`, submodule init, `./run.sh build`, ad-hoc codesign). Release builds:
`docs/release_build.md`. whisper.cpp build: `docs/build_whisper.md`.
