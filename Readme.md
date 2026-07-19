# ItalianSuperWhisper 🇮🇹

**On-device dictation for macOS, built around Italian.**

Press a shortcut, speak, and the text is typed into whatever app you have in front of you.
Everything happens **on your Mac**: no API keys, no cloud inference. The network is used
only to download models the first time.

A fork of [Starmel/OpenSuperWhisper](https://github.com/Starmel/OpenSuperWhisper), which is
a general-purpose multilingual app. Here the centre of gravity is Italian.

---

## Why "built around Italian"

It does not mean other languages were removed: Whisper and Parakeet still support all of
them, and they remain selectable. It means **product decisions are made by looking at
Italian**.

Concretely:

- **English loanwords are not errors.** In spoken Italian, "meeting", "call", "deadline"
  and "budget" are normal. They are not translated and not "corrected" — they are
  transcribed as you said them. The same goes for words borrowed from any other language.
- **No CJK-specific logic.** Upstream's CJK autocorrect (spacing between Chinese/Japanese
  and Latin characters, full-width punctuation) was removed: it does not apply to Italian,
  and it dragged the entire Rust toolchain into the build.
- **Automatic Italian correction**, described below.

## The two correction layers

Text coming out of the model passes through two layers, kept deliberately separate.

### 1. Deterministic corrector — always on

`ItalianTextCorrector` runs on **every** dictation. It makes no model call, so it costs
microseconds and has nothing to configure.

It contains **only always-true rules**:

- accents whose unaccented form is not an Italian word:
  `perche` → `perché`, `piu` → `più`, `puo` → `può`, `citta` → `città`
- spellings that are never correct: `pò` → `po'`, `qual'è` → `qual è`,
  `daccordo` → `d'accordo`, `un'altro` → `un altro`
- spaces before punctuation, and doubled spaces

Anything requiring context is **excluded on purpose**. `e`/`è`, `si`/`sì`, `la`/`là`,
`da`/`dà`, `ne`/`né`, `se`/`sé` are all pairs of valid spellings: a context-free rule would
silently change the meaning of the sentence. For the same reason the accent table also
leaves out `pero` (the pear tree), `meta` (the goal), `giacche` (the jackets), `te` (the
pronoun) and `eta` (the Greek letter).

It runs only when the language is set **explicitly** to Italian, not to "auto": with
"auto" the language is unknown, and these rules must never run on another language.

### 2. Local-LLM reformulation — optional

A separate layer, off by default, that cleans up spoken self-corrections. From
*"domani alle 10 non ci sarò, ah no, non è vero, alle 10.30"* to
*"domani non ci sarò alle 10.30"*.

Whisper and Parakeet are **transcription** models: they faithfully report what you said,
and will never do this job. It takes a separate model, also running locally
(Gemma 4 E2B via MLX).

Enable it in **Settings → Transcription → Riformulazione**. Once on, it is automatic: it
runs on every dictation, with no extra gesture. Worth knowing before you turn it on:

- the first dictation downloads a model of several GB;
- every dictation takes a few seconds longer;
- the raw text is stored in the database next to the rewritten one, and every model
  failure (model not loading, empty response, off-topic response) falls back to the
  original transcription. A failed reformulation never costs you the dictation.

> **A rule that holds for both layers:** the raw text is always kept alongside the
> corrected one. Silently altering what you dictated is worse than doing nothing.

## Features

- Local recording and transcription
- Two engines: [Whisper](https://github.com/ggerganov/whisper.cpp) and
  [Parakeet](https://github.com/AntinomyCollective/FluidAudio), with in-app model downloads
- Global shortcut — a key combination or a single modifier (left ⌘, right ⌥, Fn…)
- Mouse trigger — middle button or the extra thumb buttons
- Hold-to-record mode: hold to record, release to stop
- Drag and drop audio files, with a processing queue
- Microphone selection: built-in, external, Bluetooth, iPhone (Continuity)
- Multiple languages with auto-detection — Italian is the focus, not a restriction

## Requirements

- macOS 14 or later
- Apple Silicon Mac (ARM64)

## Running it

### 1. Full Xcode

You need **Xcode**, not just the Command Line Tools: the build runs `xcodebuild` against
`OpenSuperWhisper.xcodeproj`. Check with `xcodebuild -version`; if it replies that Xcode is
required, point the toolchain at it:

```shell
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
```

### 2. Build dependencies

```shell
brew install cmake libomp ruby
gem install xcpretty
```

`xcpretty` is optional: it only makes the build output readable.

### 3. Clone and build

```shell
git clone git@github.com:dylanpatriarchi/ItalianSuperWhisper.git
cd ItalianSuperWhisper
git submodule update --init --recursive
./run.sh build
```

The submodule step is **not optional**: without it `libwhisper/whisper.cpp` stays empty and
linking fails with `library 'ggml-metal' not found`.

### 4. Sign the app

The build produces an unsigned app, and macOS will not let you grant Accessibility
permission (needed for the global shortcut and for pasting the text) until it carries a
signature. Sign it ad-hoc:

```shell
codesign --force --sign - \
  --entitlements OpenSuperWhisper/OpenSuperWhisper.entitlements \
  build/Build/Products/Debug/OpenSuperWhisper.app
```

### 5. Launch

```shell
open build/Build/Products/Debug/OpenSuperWhisper.app
```

On first launch you will need to grant **Accessibility** and **Microphone** in System
Settings, and download a model from the app's settings.

If anything goes wrong, `.github/workflows/build.yml` is the CI workflow and shows the
exact sequence that runs automatically on every push.

## Models

Italian needs a **multilingual** model: the ones suffixed `.en` (`tiny.en`, `base.en`…) are
English-only.

From the app's settings:

- **Turbo V3 large** (~1.6 GB) — a reasonable default, good results in Italian
- **Parakeet v3** — lighter and faster, also supports Italian

Both engines ship in the app, so you can compare them on your own voice and keep whichever
serves you better.

Whisper models can also be downloaded by hand from the
[whisper.cpp Hugging Face repository](https://huggingface.co/ggerganov/whisper.cpp/tree/main)
and placed in the app's models directory.

## Fixes over upstream

Applied in [#1](https://github.com/dylanpatriarchi/ItalianSuperWhisper/pull/1):

- **Cancelling a transcription now works.** The cancellation flag was set and cleared in
  the same call, and the Parakeet engine never stored its own task: cancelling left the
  work running to completion.
- **Two transcriptions can no longer enter the same whisper context concurrently.**
- **The progress callback can no longer outlive the object it points at**, which could free
  memory still referenced by whisper.cpp.
- **Downloaded models are validated.** Previously only the HTTP status was checked: an
  error page could be stored as a model and reported as installed forever.
- **Back-to-back dictations no longer destroy the clipboard**, which used to be restored to
  the *previous transcription* instead of the user's own contents.
- **Recording before the model has loaded now says so**, instead of silently discarding the
  dictation.
- Event taps no longer leak a mach port on every shortcut change.
- `run.sh` no longer reports success when the build actually failed.

Applied in [#5](https://github.com/dylanpatriarchi/ItalianSuperWhisper/pull/5):

- **A missing microphone is reported before a loading model.** Both conditions block
  recording, but only one resolves itself: someone with no input device was told
  "Loading model..." and left waiting for something that would never help.

## Roadmap

- [x] Remove the CJK autocorrect and the Rust toolchain from the build
- [x] Low-latency deterministic Italian corrector
- [x] Local-LLM reformulation, optional and automatic once enabled
- [x] Keep the raw text in the database, next to the corrected one
- [ ] Compare Whisper and Parakeet on Italian to pick the default
- [ ] Run the test suite in CI, not just the build

Inherited from upstream:

- [ ] Streaming transcription
- [ ] Custom dictionary / keyword boosting
- [ ] Intel Mac compatibility
- [ ] Agent mode

## Contributing

Pull requests and issues are welcome.

## License

MIT. See the [LICENSE](LICENSE) file.
