# Development

## Requirements

- Rust (stable) and Cargo
- Node.js and pnpm
- WebKitGTK 4.1 and the libraries Tauri needs. On Debian/Ubuntu:
  ```sh
  sudo apt install build-essential curl file libwebkit2gtk-4.1-dev \
    libayatana-appindicator3-dev librsvg2-dev libssl-dev libxdo-dev
  ```
- For building the bundled FFmpeg: `gcc`, `make`, `tar`, `xz`, `curl` and `sha256sum`. `scripts/build-ffmpeg.sh` downloads the pinned release, checks its SHA-256 and builds it into `app/src-tauri/resources/`.

## Everyday commands

```sh
# All Rust tests: the engine runs against a local test server
# (ranges, throttling, dropped connections, 503 caps, redirects, ETag changes…)
cargo test --workspace

# Lints and formatting, as CI expects them
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check

# The app with hot reload
cd app && pnpm install && pnpm tauri dev

# Type-check the UI
cd app && pnpm check

# Extension tests
node --test extension/tests/

# Release build, the bundled FFmpeg (first time), and registration of the
# browser host, the menu entry and the icons
./scripts/install-dev.sh
```

`install-dev.sh` leaves `siler`, `siler-nmhost`, `ffmpeg` and `ffprobe` together in `target/release/`.

## Media tests

- The media tests use small fixture streams in `crates/siler-core/tests/media` (HLS muxed, split, AES-128, fMP4, DASH, a DRM-marked manifest). `scripts/gen_media_fixtures.sh` regenerates them; it needs an FFmpeg with libx264 and AAC, and openssl. The outputs are committed, so the tests themselves never run it.
- `scripts/serve-media-fixtures.sh` serves the same fixtures on `http://127.0.0.1:8099/` for trying Siler by hand. Allow local streams in Settings → Media first.
- Tests that need FFmpeg use `SILER_FFMPEG` (an absolute path). `SILER_REQUIRE_FFMPEG=1` makes them fail instead of skipping when it's missing:
  ```sh
  SILER_FFMPEG=$PWD/app/src-tauri/resources/ffmpeg cargo test --workspace
  ```
- Real CDNs: this test downloads the lowest quality of a few public test streams (Apple, Unified Streaming, Akamai, Shaka, Mux, an AES-128 stream) and checks the files with `ffprobe`. It needs the network, so plain `cargo test` skips it. `SILER_REAL_CDN_ONLY=<text>` picks streams by name or URL.
  ```sh
  SILER_REAL_CDN=1 SILER_FFMPEG=… cargo test -p siler-core --test real_cdn -- --ignored --nocapture
  ```

## The extension

- Signing for permanent installs: see [Browser extension → Signing your own build](browser-extension.md#signing-your-own-build).
- `scripts/vendor-extension-libs.sh path/to/yt-dlp-source.zip` refreshes the YouTube solver and its parser libraries in `extension/media/youtube/vendor/`, unmodified and checked against yt-dlp's recorded hash.

## Releases

Every release carries:

- **The AppImage** (`Siler-<version>-x86_64.AppImage`) with its `.zsync` and `.sha256`. The `AppImage` workflow (`.github/workflows/appimage.yml`) builds it when a release is published and attaches it. It builds on the oldest supported Ubuntu, runs the tests with the bundled FFmpeg, and puts the native host, FFmpeg and the signed extension inside. The extension is downloaded from the release itself, so publish the release with its `.xpi` attached. The workflow can also be run by hand for an existing tag; the tag must match the app's version.
- **A tarball** (`siler-<version>-linux-x86_64.tar.gz`) of the same programs for running from a folder: `siler`, `siler-nmhost`, `ffmpeg`, `ffprobe`, the signed `siler-integration.xpi`, an `INSTALL.txt`, the FFmpeg license notice and this README.
- **The signed extension** (`siler-integration-<version>.xpi`), signed by Mozilla as unlisted.
- **`SHA256SUMS`** for the tarball and the extension.

**Updates:** the AppImage carries zsync update information (`gh-releases-zsync|sekhnat|Siler|latest|Siler-*-x86_64.AppImage.zsync`). AppImage updaters (AppImageUpdate, AppImageLauncher, Gear Lever, AppManager) fetch only the parts that changed. `scripts/appimage-zsync.sh` writes the information and makes the `.zsync` file.

To cut a release:

1. Set the version in `Cargo.toml` (`workspace.package`), `app/package.json` and `app/src-tauri/tauri.conf.json`. If the extension changed, raise its version in `extension/manifest.json` and sign it.
2. Commit, then tag it `v<version>` and push the tag.
3. Build the tarball from a release build (`./scripts/install-dev.sh`).
4. Publish the GitHub release with the notes, the tarball, the `.xpi` and `SHA256SUMS`. The workflow adds the AppImage.
