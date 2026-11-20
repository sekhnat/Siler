#!/usr/bin/env bash
# Builds the FFmpeg that K-Star bundles: a pinned upstream release, static, with only what
# K-Star uses (media-design §12). The result goes to app/src-tauri/resources/ (gitignored).
#
# Needs: gcc, make, tar, xz, curl, sha256sum. The release is checked against the SHA-256
# pinned below; its PGP signature (ffmpeg-<version>.tar.xz.asc, key ffmpeg-devel@ffmpeg.org)
# can be checked by hand with `gpg --verify`.
set -euo pipefail

FFMPEG_VERSION=9.0.2
FFMPEG_SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build="$root/target/ffmpeg-build"
dest="$root/app/src-tauri/resources"
jobs="$(nproc 2>/dev/null || echo 4)"

mkdir -p "$build/src" "$dest"
tarball="$build/ffmpeg-$FFMPEG_VERSION.tar.xz"
if [ ! -f "$tarball" ] || ! echo "$FFMPEG_SHA256  $tarball" | sha256sum -c --status; then
    echo "==> Downloading FFmpeg $FFMPEG_VERSION"
    curl -fL --retry 3 -o "$tarball" "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"
fi
echo "$FFMPEG_SHA256  $tarball" | sha256sum -c --status || {
    echo "The download doesn't match the pinned SHA-256; refusing to build." >&2
    exit 1
}

src="$build/src/ffmpeg-$FFMPEG_VERSION"
if [ ! -d "$src" ]; then
    echo "==> Unpacking"
    tar -C "$build/src" -xf "$tarball"
fi

# What K-Star needs (§12.3) and what its fixtures and reconstruction use. Nothing that needs a
# library: no network, no zlib, no hardware. Subtitle encoders are for MP4's mov_text.
demuxers="mpegts,mov,matroska,aac,mp3,ac3,eac3,webvtt,srt,concat"
muxers="mp4,matroska,mpegts,webvtt,srt"
decoders="h264,hevc,aac,mp3,ac3,eac3,opus,webvtt,subrip,movtext"
parsers="h264,hevc,aac,ac3,mpegaudio,opus"
bsfs="aac_adtstoasc,h264_mp4toannexb,hevc_mp4toannexb,extract_extradata,null"
encoders="movtext,webvtt,srt,subrip"

out="$build/install"
rm -rf "$build/obj" "$out"
mkdir -p "$build/obj"
cd "$build/obj"

echo "==> Configuring"
"$src/configure" \
    --prefix="$out" \
    --disable-everything \
    --disable-autodetect \
    --disable-doc \
    --disable-debug \
    --disable-network \
    --disable-x86asm \
    --disable-shared \
    --enable-static \
    --enable-small \
    --enable-ffmpeg \
    --enable-ffprobe \
    --disable-ffplay \
    --enable-protocol=file \
    --enable-protocol=pipe \
    --enable-demuxer="$demuxers" \
    --enable-muxer="$muxers" \
    --enable-decoder="$decoders" \
    --enable-parser="$parsers" \
    --enable-bsf="$bsfs" \
    --enable-encoder="$encoders" \
    --extra-ldflags=-static \
    --extra-cflags=-O2 \
    >configure.log 2>&1 || {
        tail -40 configure.log >&2
        exit 1
    }

echo "==> Building with $jobs jobs"
make -j"$jobs" >make.log 2>&1 || {
    tail -60 make.log >&2
    exit 1
}
make install >install.log 2>&1

cp "$out/bin/ffmpeg" "$out/bin/ffprobe" "$dest/"
chmod 755 "$dest/ffmpeg" "$dest/ffprobe"

echo "==> Built: $("$dest/ffmpeg" -hide_banner -version | head -1)"
echo "    $(file -b "$dest/ffmpeg" | cut -c1-120)"
echo "    installed to $dest"
