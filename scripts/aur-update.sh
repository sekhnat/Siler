#!/usr/bin/env bash
# Points the AUR package at a release: sets pkgver, the tarball's sha256 (from the release's
# .sha256 file, or the local dist/ tarball), and regenerates .SRCINFO.
#   scripts/aur-update.sh 0.8.0
# Then push packaging/aur/siler-bin (PKGBUILD and .SRCINFO) to the AUR repository.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="${1:?usage: aur-update.sh <version>}"
pkg="$root/packaging/aur/siler-bin"
name="siler-$version-linux-x86_64.tar.gz"

if [ -f "$root/dist/$name" ]; then
  sum="$(sha256sum "$root/dist/$name" | cut -d' ' -f1)"
else
  sum="$(curl -fsSL "https://github.com/sekhnat/Siler/releases/download/v$version/$name.sha256" | cut -d' ' -f1)"
fi
[[ "$sum" =~ ^[0-9a-f]{64}$ ]] || { echo "no checksum for $name" >&2; exit 1; }

sed -i -e "s/^pkgver=.*/pkgver=$version/" -e "s/^pkgrel=.*/pkgrel=1/" \
       -e "s/^sha256sums=.*/sha256sums=('$sum')/" "$pkg/PKGBUILD"
(cd "$pkg" && makepkg --printsrcinfo > .SRCINFO)
echo "updated $pkg for $version ($sum)"
