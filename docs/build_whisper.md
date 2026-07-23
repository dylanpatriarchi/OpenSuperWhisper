# How whisper.cpp is built

The app links whisper.cpp via the `libwhisper/whisper.cpp` git submodule.
You do **not** build it by hand — `run.sh` configures it as an Xcode-generated
CMake project before building the app:

```shell
cmake -G Xcode -B libwhisper/build -S libwhisper
```

`libwhisper/CMakeLists.txt` pulls in the submodule and exposes the `whisper`
target (with the Metal and GGML backends) to the Xcode project. So the only
thing you have to do yourself is initialise the submodule:

```shell
git submodule update --init --recursive
```

Skip it and `libwhisper/whisper.cpp` is empty, so the link fails with
`library 'ggml-metal' not found`. See the README's "Running it" section for
the full build flow.
