#!/usr/bin/env bash
# Builds Siler (release), then registers it with the desktop and with Firefox (and
# LibreWolf, Waterfox, Floorp, Zen when installed): native messaging host, application
# menu entry and icons.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The bundled FFmpeg (media-design §12): built once, from the pinned release.
if [ ! -x "$root/app/src-tauri/resources/ffmpeg" ] || [ ! -x "$root/app/src-tauri/resources/ffprobe" ]; then
  echo "==> Building the bundled FFmpeg (first time: a few minutes)"
  "$root/scripts/build-ffmpeg.sh"
fi

echo "==> Building the app"
cd "$root/app"
pnpm install --frozen-lockfile
pnpm tauri build --no-bundle

# The FFmpeg Siler looks for next to its executable.
cp "$root/app/src-tauri/resources/ffmpeg" "$root/app/src-tauri/resources/ffprobe" "$root/target/release/"

echo "==> Building the native messaging host and the proxy script helper"
cd "$root"
cargo build --release -p siler-nmhost -p siler-pac

echo "==> Registering"
"$root/target/release/siler" --install-integration

echo
echo "Next steps:"
echo "  1. Start the app:      $root/target/release/siler"
echo "  2. Install the extension permanently: scripts/sign-extension.sh, then"
echo "     Siler → Settings → Browser → Add to Firefox."
echo "     For a quick test instead: about:debugging#/runtime/this-firefox →"
echo "     Load Temporary Add-on → extension/manifest.json (gone after a Firefox restart)."
