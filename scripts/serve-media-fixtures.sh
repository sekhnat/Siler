#!/usr/bin/env bash
# Serves the media test fixtures on http://127.0.0.1:8099/ for trying K-Star by hand:
#   http://127.0.0.1:8099/muxed/index.m3u8   one muxed HLS stream (video and audio together)
#   http://127.0.0.1:8099/split/master.m3u8  video, English and German audio, subtitles
#   http://127.0.0.1:8099/aes/index.m3u8     AES-128 encrypted HLS (key from /aes/key.bin)
#   http://127.0.0.1:8099/fmp4/index.m3u8    fragmented MP4 HLS
#   http://127.0.0.1:8099/dash/manifest.mpd  DASH with a timeline
#   http://127.0.0.1:8099/drm/manifest.mpd   DASH marked as DRM (K-Star refuses it)
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 -m http.server 8099 --bind 127.0.0.1 --directory "$root/crates/kstar-core/tests/media"
