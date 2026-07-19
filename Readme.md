# OpenSuperWhisper

OpenSuperWhisper is a macOS application that provides real-time audio transcription using the Whisper model. It offers a seamless way to record and transcribe audio with customizable settings and keyboard shortcuts.

> **This is a fork** of [Starmel/OpenSuperWhisper](https://github.com/Starmel/OpenSuperWhisper) with a set of correctness fixes applied on top. See [Fixes in this fork](#fixes-in-this-fork). Transcription is fully on-device — there is no API key and no network inference; the network is used only to download models.

<p align="center">
<img src="docs/image.png" width="400" /> <img src="docs/image_indicator.png" width="400" />
</p>

## Features

- 🎙️ Real-time audio recording and transcription
- 🧠 Two transcription engines: [Whisper](https://github.com/ggerganov/whisper.cpp) and [Parakeet](https://github.com/AntinomyCollective/FluidAudio) — download models directly from the app
- ⌨️ Global keyboard shortcuts — key combination or single modifier key (e.g. Left ⌘, Right ⌥, Fn)
- 🖱️ Mouse button trigger — bind the middle or an extra (thumb) mouse button to start/stop recording
- ✊ Hold-to-record mode — hold the shortcut, modifier key or mouse button to record, release to stop
- 📁 Drag & drop audio files for transcription with queue processing
- 🎤 Microphone selection — switch between built-in, external, Bluetooth and iPhone (Apple Continuity) mics from the menu bar
- 🌍 Support for multiple languages with auto-detection

## Installation

The Homebrew formula and the releases page below install **upstream**, not this fork:

```shell
brew update # Optional
brew install opensuperwhisper
```

Or from [GitHub releases page](https://github.com/Starmel/OpenSuperWhisper/releases).

To run this fork, build it locally — see [Building locally](#building-locally).

## Requirements

- macOS (Apple Silicon/ARM64)

## Support

If you encounter any issues or have questions, please:
1. Check the existing issues in the repository
2. Create a new issue with detailed information about your problem
3. Include system information and logs when reporting bugs

## Building locally

You need the **full Xcode**, not just the Command Line Tools — the build uses
`xcodebuild` against `OpenSuperWhisper.xcodeproj`. Check with `xcodebuild -version`;
if it reports that it requires Xcode, point the toolchain at it:

    sudo xcode-select -s /Applications/Xcode.app/Contents/Developer

Then:

    git clone git@github.com:dylanpatriarchi/OpenSuperWhisper.git
    cd OpenSuperWhisper
    git submodule update --init --recursive
    brew install cmake libomp ruby
    gem install xcpretty
    ./run.sh build

The submodule step is not optional: without it `libwhisper/whisper.cpp` is empty and
the link fails with `library 'ggml-metal' not found`.

The built app is unsigned. macOS will not let you grant it Accessibility (needed for
the global hotkey and auto-paste) until it carries a signature, so sign it ad-hoc:

    codesign --force --sign - \
      --entitlements OpenSuperWhisper/OpenSuperWhisper.entitlements \
      build/Build/Products/Debug/OpenSuperWhisper.app

In case of problems, consult `.github/workflows/build.yml` which is our CI workflow
where the app gets built automatically on GitHub's CI.

## Fixes in this fork

Applied in [#1](https://github.com/dylanpatriarchi/OpenSuperWhisper/pull/1):

- **Cancelling a transcription now works.** The cancellation flag was set and cleared
  in the same call, and the Parakeet engine never stored its task, so cancelling left
  the workload running to completion.
- **Two transcriptions can no longer enter one whisper context concurrently.**
- **The progress callback can no longer outlive the object it points at**, which could
  free memory still referenced by whisper.cpp.
- **Corrupt model downloads are rejected.** Previously only the HTTP status was checked,
  so an error page could be stored as a model and reported as installed forever.
- **Back-to-back dictations no longer destroy the clipboard**, which used to be restored
  to the *previous transcription* instead of the user's own contents.
- **Recording before the model has loaded now says so** instead of silently discarding
  the dictation.
- Event taps no longer leak a mach port on every hotkey settings change.
- `run.sh` no longer reports success when the build actually failed.

## Contributing

Contributions are welcome! Please feel free to submit pull requests or create issues for bugs and feature requests.

### Contribution TODO list

- [ ] Streaming transcription
- [ ] Custom dictionary / keyword boosting ([#19](https://github.com/Starmel/OpenSuperWhisper/issues/19))
- [ ] Intel macOS compatibility ([#15](https://github.com/Starmel/OpenSuperWhisper/issues/15))
- [ ] Agent mode ([#14](https://github.com/Starmel/OpenSuperWhisper/issues/14))
- [x] Background app ([#8](https://github.com/Starmel/OpenSuperWhisper/issues/8))
- [x] Support long-press single key audio recording ([#18](https://github.com/Starmel/OpenSuperWhisper/issues/18))

## License

OpenSuperWhisper is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.

## Whisper Models

You can download Whisper model files (`.bin`) from the [Whisper.cpp Hugging Face repository](https://huggingface.co/ggerganov/whisper.cpp/tree/main). Place the downloaded `.bin` files in the app's models directory. On first launch, the app will attempt to copy a default model automatically, but you can add more models manually.

### Hebrew (ivrit.ai)

For Hebrew transcription, download the **"Turbo V3 Hebrew"** model from Settings → Model. It is [ivrit.ai](https://www.ivrit.ai/)'s Hebrew fine-tune of `whisper-large-v3-turbo` ([whisper-large-v3-turbo-ggml](https://huggingface.co/ivrit-ai/whisper-large-v3-turbo-ggml)) — the same base model as the other "Turbo V3" entries, but tuned for Hebrew. Selecting it automatically sets the input language to Hebrew, which these models require to be set explicitly.
