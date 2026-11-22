#!/usr/bin/env bash
# Makes the release tarball, siler-<version>-linux-x86_64.tar.gz, from a release build:
#   target/release/siler, siler-nmhost and siler-pac, the bundled FFmpeg
#   (app/src-tauri/resources),
#   the signed Firefox extension (its path as the first argument, or the one
#   scripts/sign-extension.sh left in ~/.local/share/siler), the Chromium extension
#   (built here), the desktop entry, icons, native messaging manifests for a system-wide
#   install (packages), INSTALL.txt, the README and FFmpeg's license.
# The AUR package (packaging/aur/siler-bin) installs from this tarball.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)"
xpi="${1:-${XDG_DATA_HOME:-$HOME/.local/share}/siler/siler-integration.xpi}"
name="siler-$version"
out="$root/dist/$name"

for f in "$root/target/release/siler" "$root/target/release/siler-nmhost" \
         "$root/target/release/siler-pac" \
         "$root/app/src-tauri/resources/ffmpeg" "$root/app/src-tauri/resources/ffprobe" "$xpi"; do
  [ -f "$f" ] || { echo "missing: $f" >&2; exit 1; }
done
license="$root/app/src-tauri/resources/ffmpeg-LICENSE.txt"
[ -f "$license" ] || { echo "missing FFmpeg's license: run scripts/build-ffmpeg.sh" >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out"
install -m755 "$root/target/release/siler" "$root/target/release/siler-nmhost" \
  "$root/target/release/siler-pac" \
  "$root/app/src-tauri/resources/ffmpeg" "$root/app/src-tauri/resources/ffprobe" "$out/"
install -m644 "$xpi" "$out/siler-integration.xpi"

node "$root/scripts/build-extension.mjs" chromium >/dev/null
cp -r "$root/dist/extension-chromium" "$out/chromium-extension"

install -Dm644 "$root/packaging/siler.desktop" "$out/share/applications/siler.desktop"
icons="$root/app/src-tauri/icons"
for pair in 32x32:32x32.png 64x64:64x64.png 128x128:128x128.png 256x256:128x128@2x.png 512x512:icon.png; do
  install -Dm644 "$icons/${pair#*:}" "$out/share/icons/hicolor/${pair%%:*}/apps/siler.png"
done
install -Dm644 "$root/app/assets/logo.svg" "$out/share/icons/hicolor/scalable/apps/siler.svg"

mkdir -p "$out/native-messaging"
for family in firefox chromium; do
  "$root/target/release/siler" --print-host-manifest "$family" /usr/bin/siler-nmhost \
    > "$out/native-messaging/$family.json"
done

install -m644 "$root/README.md" "$out/README.md"
install -m644 "$license" "$out/LICENSE.FFmpeg"
cat > "$out/INSTALL.txt" <<TXT
Siler $version for Linux (x86-64)

Run it from this folder:
  ./siler --install-integration   registers Siler with your browsers and the app menu
  ./siler                         starts it

Keep the folder where it is: the browsers start ./siler-nmhost from here.
It needs WebKitGTK 4.1 and libayatana-appindicator (most desktops have them).
Optional: gtk-layer-shell for the tray panel on Wayland, and MIT Kerberos
(libgssapi_krb5) for Kerberos logins. Keep siler-pac beside siler: it runs proxy scripts.

Browser extension:
  Firefox and its relatives: Siler → Settings → Browser → Add to Firefox, or open
  siler-integration.xpi with Firefox.
  Chrome, Chromium, Brave, Vivaldi, Edge, Opera: Siler → Settings → Browser → Set up,
  then load the chromium-extension folder with "Load unpacked" (Developer mode).

share/ and native-messaging/ are for packages (system-wide installs).
TXT

tar -C "$root/dist" -czf "$root/dist/$name-linux-x86_64.tar.gz" "$name"
(cd "$root/dist" && sha256sum "$name-linux-x86_64.tar.gz" > "$name-linux-x86_64.tar.gz.sha256")
echo "$root/dist/$name-linux-x86_64.tar.gz"
