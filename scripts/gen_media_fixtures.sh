#!/usr/bin/env bash
# Generates the tiny media fixtures the media tests and the manual-test server use
# (crates/kstar-core/tests/media). Needs an FFmpeg with libx264 and aac, and openssl.
# The outputs are committed, so the tests themselves never need to run this.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$root/crates/kstar-core/tests/media"
ff="${FFMPEG:-ffmpeg}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

rm -rf "$out"
mkdir -p "$out"/{muxed,split,aes,fmp4,dash,drm}

# Six seconds of 64x64 video (10 fps, a keyframe every 2 s) with a 440 Hz tone (English) and
# an 880 Hz tone (German, used as a separate track).
"$ff" -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=64x64:rate=10" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000" \
    -t 6 -c:v libx264 -pix_fmt yuv420p -g 20 -keyint_min 20 -sc_threshold 0 \
    -c:a aac -b:a 64k -f mpegts "$work/full.ts"
"$ff" -hide_banner -loglevel error -y \
    -f lavfi -i "sine=frequency=880:sample_rate=48000" -t 6 \
    -c:a aac -b:a 64k -f mpegts "$work/de.ts"
"$ff" -hide_banner -loglevel error -y -i "$work/full.ts" -an -c:v copy -f mpegts "$work/video.ts"

# Muxed HLS: video and audio in the same TS segments.
"$ff" -hide_banner -loglevel error -y -i "$work/full.ts" -c copy \
    -f hls -hls_time 2 -hls_playlist_type vod -hls_list_size 0 \
    -hls_segment_filename "$out/muxed/seg%d.ts" "$out/muxed/index.m3u8"

# Split HLS: video-only variant; English and German audio renditions; WebVTT subtitles.
"$ff" -hide_banner -loglevel error -y -i "$work/video.ts" -c copy \
    -f hls -hls_time 2 -hls_playlist_type vod -hls_list_size 0 \
    -hls_segment_filename "$out/split/v%d.ts" "$out/split/v.m3u8"
mkdir -p "$out/split/audio-en" "$out/split/audio-de"
"$ff" -hide_banner -loglevel error -y -i "$work/full.ts" -vn -c copy \
    -f hls -hls_time 2 -hls_playlist_type vod -hls_list_size 0 \
    -hls_segment_filename "$out/split/audio-en/a%d.ts" "$out/split/audio-en/index.m3u8"
"$ff" -hide_banner -loglevel error -y -i "$work/de.ts" -vn -c copy \
    -f hls -hls_time 2 -hls_playlist_type vod -hls_list_size 0 \
    -hls_segment_filename "$out/split/audio-de/a%d.ts" "$out/split/audio-de/index.m3u8"
# Subtitles as packagers write them: cue times on the stream's timeline, every segment
# mapped to the video's first MPEG-TS timestamp. One cue crosses a segment boundary, so it is
# in two segments (it must come out once).
mkdir -p "$out/split/subs"
start_pts=$("${FFPROBE:-ffprobe}" -v error -select_streams v -show_entries stream=start_time \
    -of csv=p=0 "$out/split/v0.ts" | python3 -c 'import sys; print(round(float(sys.stdin.read().split()[0]) * 90000))')
for i in 0 1 2; do
    {
        echo "WEBVTT"
        echo "X-TIMESTAMP-MAP=MPEGTS:$start_pts,LOCAL:00:00:00.000"
        echo
        echo "00:00:0$((i * 2)).500 --> 00:00:0$((i * 2 + 1)).500"
        echo "Hello, segment $i"
        echo
        if [ "$i" -lt 2 ]; then
            # From 1.8 s to 2.3 s: in segment 0 and again in segment 1.
            echo "00:00:01.800 --> 00:00:02.300 align:start"
            echo "across 2 s"
            echo
        fi
    } > "$out/split/subs/s$i.vtt"
done
{
    echo "#EXTM3U"
    echo "#EXT-X-VERSION:3"
    echo "#EXT-X-TARGETDURATION:2"
    echo "#EXT-X-PLAYLIST-TYPE:VOD"
    echo "#EXT-X-MEDIA-SEQUENCE:0"
    for i in 0 1 2; do echo "#EXTINF:2.000000,"; echo "s$i.vtt"; done
    echo "#EXT-X-ENDLIST"
} > "$out/split/subs/index.m3u8"
cat > "$out/split/master.m3u8" <<'EOF'
#EXTM3U
#EXT-X-VERSION:4
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="English",LANGUAGE="en",DEFAULT=YES,AUTOSELECT=YES,CHANNELS="2",URI="audio-en/index.m3u8"
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="Deutsch",LANGUAGE="de",DEFAULT=NO,AUTOSELECT=YES,CHANNELS="2",URI="audio-de/index.m3u8"
#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID="subs",NAME="English",LANGUAGE="en",DEFAULT=YES,AUTOSELECT=YES,URI="subs/index.m3u8"
#EXT-X-STREAM-INF:BANDWIDTH=300000,CODECS="avc1.42c00d,mp4a.40.2",RESOLUTION=64x64,AUDIO="aud",SUBTITLES="subs"
v.m3u8
EOF

# AES-128: each TS segment encrypted with openssl (PKCS#7), IV = the media sequence number.
key_hex="00112233445566778899aabbccddeeff"
python3 -c 'import binascii,sys; sys.stdout.buffer.write(binascii.unhexlify(sys.argv[1]))' "$key_hex" > "$out/aes/key.bin"
for i in 0 1 2; do
    iv_hex=$(printf '%032x' "$i")
    openssl enc -aes-128-cbc -K "$key_hex" -iv "$iv_hex" \
        -in "$out/muxed/seg$i.ts" -out "$out/aes/seg$i.ts"
done
cat > "$out/aes/index.m3u8" <<'EOF'
#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:2
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-KEY:METHOD=AES-128,URI="key.bin"
#EXTINF:2.000000,
seg0.ts
#EXTINF:2.000000,
seg1.ts
#EXTINF:2.000000,
seg2.ts
#EXT-X-ENDLIST
EOF

# Fragmented MP4 HLS (CMAF-style init segment and fragments).
"$ff" -hide_banner -loglevel error -y -i "$work/full.ts" -c copy -bsf:a aac_adtstoasc \
    -f hls -hls_time 2 -hls_playlist_type vod -hls_list_size 0 \
    -hls_segment_type fmp4 -hls_fmp4_init_filename init.mp4 \
    -hls_segment_filename "$out/fmp4/seg%d.m4s" "$out/fmp4/index.m3u8"

# DASH with a SegmentTemplate and timeline, from FFmpeg's own muxer. It is fed a regular MP4
# (whose AAC configuration is complete), not MPEG-TS or a fragmented MP4 written before it.
"$ff" -hide_banner -loglevel error -y -i "$work/full.ts" -c copy -bsf:a aac_adtstoasc \
    "$work/full.mp4"
"$ff" -hide_banner -loglevel error -y -i "$work/full.mp4" -map 0:v -map 0:a -c copy \
    -f dash -seg_duration 2 -use_template 1 -use_timeline 1 \
    -init_seg_name 'init-$RepresentationID$.m4s' \
    -media_seg_name 'chunk-$RepresentationID$-$Number%05d$.m4s' \
    -adaptation_sets "id=0,streams=v id=1,streams=a" \
    "$out/dash/manifest.mpd"

# The same DASH stream, marked as CENC-encrypted in every adaptation set: K-Star must refuse it.
cp "$out/dash/"*.m4s "$out/drm/"
python3 - "$out" <<'PY'
import pathlib, re, sys
out = pathlib.Path(sys.argv[1])
mpd = (out / "dash" / "manifest.mpd").read_text()
cenc = '<ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc"/>'
drm, n = re.subn(r"(<AdaptationSet[^>]*>)", lambda m: m.group(1) + cenc, mpd)
assert n >= 2, "expected an AdaptationSet for each track"
(out / "drm" / "manifest.mpd").write_text(drm)
PY

# A segment that is a web page (an expired link answered with 200): served by the test server.
printf '<!DOCTYPE html><html><body>Session expired</body></html>\n' > "$out/drm/expired.html"

echo "Wrote $(find "$out" -type f | wc -l) files to $out ($(du -sh "$out" | cut -f1))"
