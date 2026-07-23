# Release build

A signed, notarized release is cut with `notarize_app.sh` (build + sign +
notarize + DMG) or `make_release.sh`, which wraps it and also creates the
GitHub release and uploads the assets.

Both require an Apple **Developer ID Application** certificate and, for
notarization, a `notarytool` keychain profile. They are keyed to whoever is
releasing, so two values must be set in the environment first — the scripts
abort with a message if either is missing:

```shell
export DEVELOPMENT_TEAM="YOURTEAMID"        # Apple Developer Team ID
export KEYCHAIN_PROFILE="your-notary-profile"
```

Create the keychain profile once with:

```shell
xcrun notarytool store-credentials "your-notary-profile" \
  --apple-id "you@example.com" --team-id "YOURTEAMID"
```

Then build and notarize:

```shell
./notarize_app.sh "Developer ID Application: Your Name (YOURTEAMID)"
```

Or do the whole release — version bump, tag, GitHub release, asset upload:

```shell
./make_release.sh 1.2.3 "Developer ID Application: Your Name (YOURTEAMID)" "$GITHUB_TOKEN"
```

The GitHub token is optional; without it the script builds and tags but skips
the release upload. `make_release.sh` prints a Homebrew cask block at the end
for whoever maintains a tap — the project does not ship one.

> Note: unlike the release build, the day-to-day `./run.sh build` is unsigned
> and needs none of this. See the README's "Running it" section.
