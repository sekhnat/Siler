#!/usr/bin/env bash
# Serves the media test fixtures on http://127.0.0.1:8099/ for trying Siler by hand:
#   http://127.0.0.1:8099/muxed/index.m3u8   one muxed HLS stream (video and audio together)
#   http://127.0.0.1:8099/split/master.m3u8  video, English and German audio, subtitles
#   http://127.0.0.1:8099/aes/index.m3u8     AES-128 encrypted HLS (key from /aes/key.bin)
#   http://127.0.0.1:8099/fmp4/index.m3u8    fragmented MP4 HLS
#   http://127.0.0.1:8099/dash/manifest.mpd  DASH with a timeline
#   http://127.0.0.1:8099/drm/manifest.mpd   DASH marked as DRM (Siler refuses it)
#   http://127.0.0.1:8099/aes-fmp4/iv/index.m3u8  fragmented MP4 under AES-128, init segment encrypted too
#   http://127.0.0.1:8099/sub-fmp4/master.m3u8    WebVTT subtitles inside fMP4, timeline starting 10 s in
#   http://127.0.0.1:8099/dash/subs.mpd           DASH with WebVTT (wvtt) and TTML (stpp) subtitles inside MP4
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 -m http.server 8099 --bind 127.0.0.1 --directory "$root/crates/siler-core/tests/media"
