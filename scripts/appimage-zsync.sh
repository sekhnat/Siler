#!/usr/bin/env bash
# Makes an AppImage updatable with zsync: writes its update information into the
# runtime's .upd_info section and makes the .zsync file published beside it.
#
#   scripts/appimage-zsync.sh K-Star-0.6.1-x86_64.AppImage
#
# Updaters (AppImageUpdate, AppImageLauncher, Gear Lever, AppManager) read the update
# information, find the newest release's .zsync file on GitHub and download only the
# blocks that changed. They download without logging in, so updates work only while the
# releases are public.
#
# Needs readelf (binutils) and zsyncmake (zsync).
set -euo pipefail

appimage="${1:?usage: $0 <AppImage>}"
repo="${KSTAR_UPDATE_REPO:-sekhnat/K-Star}"
info="gh-releases-zsync|${repo%%/*}|${repo#*/}|latest|K-Star-*-x86_64.AppImage.zsync"

# The section's offset in the file and its size (hex), from the runtime's ELF header.
# Fields are counted from the name: "[ 5]" splits in two for small section numbers.
read -r offset size < <(
  readelf -S --wide "$appimage" |
    awk '{ for (i = 1; i < NF; i++) if ($i == ".upd_info") print $(i + 3), $(i + 4) }'
) || true
if [[ -z "${offset:-}" ]]; then
  echo "error: $appimage has no .upd_info section (not a type 2 AppImage?)" >&2
  exit 1
fi
offset=$((16#$offset))
size=$((16#$size))
if (( ${#info} >= size )); then
  echo "error: the update information is longer than the $size-byte section" >&2
  exit 1
fi

# Clear the section, then write the text; the rest stays zero.
dd if=/dev/zero of="$appimage" bs=1 seek="$offset" count="$size" conv=notrunc status=none
printf '%s' "$info" | dd of="$appimage" bs=1 seek="$offset" conv=notrunc status=none

written="$(dd if="$appimage" bs=1 skip="$offset" count="$size" status=none | tr -d '\0')"
if [[ "$written" != "$info" ]]; then
  echo "error: the update information didn't read back" >&2
  exit 1
fi

# The .zsync names the AppImage relatively, so it is found beside the .zsync file.
dir="$(dirname "$appimage")"
name="$(basename "$appimage")"
(cd "$dir" && zsyncmake -u "$name" -o "$name.zsync" "$name" >/dev/null)

echo "Update information: $info"
echo "Made: $dir/$name.zsync"
