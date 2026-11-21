#!/usr/bin/env bash
# Generates the tiny media fixtures the media tests and the manual-test server use
# (crates/siler-core/tests/media). Needs an FFmpeg with libx264 and aac, and openssl.
# The outputs are committed, so the tests themselves never need to run this.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$root/crates/siler-core/tests/media"
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

# Fragmented MP4 HLS under AES-128, with the init segment encrypted too (RFC 8216 4.3.2.5):
# "iv" states the IV on the key line; "seq" leaves it out (media sequence 5), so the init uses
# the next segment's sequence number and each segment its own.
mkdir -p "$out/aes-fmp4/iv" "$out/aes-fmp4/seq"
iv_fixed="0f0e0d0c0b0a09080706050403020100"
openssl enc -aes-128-cbc -K "$key_hex" -iv "$iv_fixed" \
    -in "$out/fmp4/init.mp4" -out "$out/aes-fmp4/iv/init.mp4"
openssl enc -aes-128-cbc -K "$key_hex" -iv "$(printf '%032x' 5)" \
    -in "$out/fmp4/init.mp4" -out "$out/aes-fmp4/seq/init.mp4"
for i in 0 1 2; do
    openssl enc -aes-128-cbc -K "$key_hex" -iv "$iv_fixed" \
        -in "$out/fmp4/seg$i.m4s" -out "$out/aes-fmp4/iv/seg$i.m4s"
    openssl enc -aes-128-cbc -K "$key_hex" -iv "$(printf '%032x' $((i + 5)))" \
        -in "$out/fmp4/seg$i.m4s" -out "$out/aes-fmp4/seq/seg$i.m4s"
done
for v in iv seq; do
    if [ "$v" = iv ]; then seq=0; ivattr=",IV=0x$iv_fixed"; else seq=5; ivattr=""; fi
    {
        printf '#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n'
        printf '#EXT-X-MEDIA-SEQUENCE:%s\n' "$seq"
        printf '#EXT-X-KEY:METHOD=AES-128,URI="../../aes/key.bin"%s\n' "$ivattr"
        printf '#EXT-X-MAP:URI="init.mp4"\n'
        for i in 0 1 2; do printf '#EXTINF:2.000000,\nseg%s.m4s\n' "$i"; done
        printf '#EXT-X-ENDLIST\n'
    } > "$out/aes-fmp4/$v/index.m3u8"
done

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

# Subtitles carried inside MP4 (written box by box: FFmpeg can't write `wvtt` or `stpp`).
#   sub-fmp4/   HLS: the fMP4 stream moved 10 s into its timeline (every tfdt + 10 s), with a
#               `wvtt` subtitle rendition on the same timeline. One cue crosses a fragment
#               boundary, one runs past the end of the video, one starts after it.
#   dash/subs.mpd  the DASH stream with an English `wvtt` track as one whole file (as Shaka
#               Packager writes it) and a German `stpp` (TTML) track in segments.
mkdir -p "$out/sub-fmp4"
python3 - "$out" <<'PY'
import pathlib, struct, sys
out = pathlib.Path(sys.argv[1])

def bx(kind, body): return struct.pack(">I", len(body) + 8) + kind + body
def full(kind, v, flags, body): return bx(kind, struct.pack(">I", (v << 24) | flags) + body)
def boxes(d):
    o = 0
    while o + 8 <= len(d):
        n, t = struct.unpack(">I4s", d[o:o + 8]); yield o, t, n; o += n

def init(entry, track_id, timescale, sample_entry=b""):
    tkhd = bytes(8) + struct.pack(">I", track_id) + bytes(60)
    mdhd = bytes(8) + struct.pack(">I", timescale) + bytes(8)
    stsd = struct.pack(">I", 1) + bx(entry, bytes(6) + struct.pack(">H", 1) + sample_entry)
    stbl = (full(b"stsd", 0, 0, stsd) + full(b"stts", 0, 0, bytes(4)) + full(b"stsc", 0, 0, bytes(4))
            + full(b"stsz", 0, 0, bytes(8)) + full(b"stco", 0, 0, bytes(4)))
    hdlr = bytes(4) + (b"text" if entry == b"wvtt" else b"subt") + bytes(12) + b"\0"
    mdia = bx(b"mdia", full(b"mdhd", 0, 0, mdhd) + full(b"hdlr", 0, 0, hdlr) + bx(b"minf", bx(b"stbl", stbl)))
    trak = bx(b"trak", full(b"tkhd", 0, 3, tkhd) + mdia)
    mvex = bx(b"mvex", full(b"trex", 0, 0, struct.pack(">IIIII", track_id, 1, 0, 0, 0)))
    return bx(b"ftyp", b"iso6\0\0\0\0iso6cmfc") + bx(b"moov", trak + mvex)

def fragment(seq, track_id, time, samples):
    tfhd = full(b"tfhd", 0, 0x020000, struct.pack(">I", track_id))
    tfdt = full(b"tfdt", 1, 0, struct.pack(">Q", time))
    trun_len = 8 + 4 + 4 + 4 + 8 * len(samples)
    moof_len = 8 + 16 + 8 + len(tfhd) + len(tfdt) + trun_len
    trun = struct.pack(">II", len(samples), moof_len + 8) + b"".join(struct.pack(">II", d, len(s)) for d, s in samples)
    moof = bx(b"moof", full(b"mfhd", 0, 0, struct.pack(">I", seq)) + bx(b"traf", tfhd + tfdt + full(b"trun", 0, 0x301, trun)))
    assert len(moof) == moof_len
    return moof + bx(b"mdat", b"".join(s for _, s in samples))

def cue(text, iden=""):
    body = (bx(b"iden", iden.encode()) if iden else b"") + bx(b"payl", text.encode())
    return bx(b"vttc", body)
EMPTY = bx(b"vtte", b"")

def wvtt_samples(cues, start, end):
    """Samples (ms durations) covering [start, end) ms, cut at every cue edge."""
    edges = sorted({start, end, *[t for c in cues for t in c[:2] if start < t < end]})
    out = []
    for a, b in zip(edges, edges[1:]):
        live = [c for c in cues if c[0] <= a and c[1] >= b]
        out.append((b - a, b"".join(cue(c[2], c[3]) for c in live) or EMPTY))
    return out

# --- HLS: the fMP4 stream 10 s into its timeline -------------------------------------------
src = out / "fmp4"
dst = out / "sub-fmp4"
v_init = (src / "init.mp4").read_bytes()
scales = {}
for o, t, n in boxes(v_init):
    if t == b"moov":
        moov = v_init[o + 8:o + n]
        for o2, t2, n2 in boxes(moov):
            if t2 == b"trak":
                trak = moov[o2 + 8:o2 + n2]
                tid = scale = None
                for o3, t3, n3 in boxes(trak):
                    if t3 == b"tkhd":
                        v = trak[o3 + 8]; tid = struct.unpack(">I", trak[o3 + 8 + (20 if v else 12):][:4])[0]
                    if t3 == b"mdia":
                        mdia = trak[o3 + 8:o3 + n3]
                        for o4, t4, n4 in boxes(mdia):
                            if t4 == b"mdhd":
                                v = mdia[o4 + 8]; scale = struct.unpack(">I", mdia[o4 + 8 + (20 if v else 12):][:4])[0]
                scales[tid] = scale
(dst / "init.mp4").write_bytes(v_init)
SHIFT = 10
for i in range(3):
    seg = bytearray((src / f"seg{i}.m4s").read_bytes())
    for o, t, n in boxes(bytes(seg)):
        if t != b"moof":
            continue
        for o2, t2, n2 in boxes(bytes(seg[o + 8:o + n])):
            if t2 != b"traf":
                continue
            base = o + 8 + o2 + 8
            traf = bytes(seg[base:base + n2 - 8])
            tid = None
            for o3, t3, n3 in boxes(traf):
                if t3 == b"tfhd":
                    tid = struct.unpack(">I", traf[o3 + 12:o3 + 16])[0]
            for o3, t3, n3 in boxes(traf):
                if t3 == b"tfdt":
                    at = base + o3 + 12
                    if traf[o3 + 8] == 1:
                        struct.pack_into(">Q", seg, at, struct.unpack(">Q", seg[at:at + 8])[0] + SHIFT * scales[tid])
                    else:
                        struct.pack_into(">I", seg, at, struct.unpack(">I", seg[at:at + 4])[0] + SHIFT * scales[tid])
    (dst / f"seg{i}.m4s").write_bytes(bytes(seg))
(dst / "v.m3u8").write_text((src / "index.m3u8").read_text())
# Cue times in ms on the shifted timeline: (start, end, text, id).
cues = [(10500, 11500, "one", "1"), (11800, 12600, "two, across a fragment", "2"),
        (15500, 17000, "past the end", "3"), (16500, 17500, "after the end", "4")]
(dst / "init-s.mp4").write_bytes(init(b"wvtt", 1, 1000, bx(b"vttC", b"WEBVTT")))
for i in range(3):
    a = 10000 + 2000 * i
    b = a + 2000 if i < 2 else 18000
    (dst / f"s{i}.m4s").write_bytes(fragment(i + 1, 1, a, wvtt_samples(cues, a, b)))
(dst / "s.m3u8").write_text("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:4\n#EXT-X-PLAYLIST-TYPE:VOD\n"
    "#EXT-X-MAP:URI=\"init-s.mp4\"\n#EXTINF:2.0,\ns0.m4s\n#EXTINF:2.0,\ns1.m4s\n#EXTINF:4.0,\ns2.m4s\n#EXT-X-ENDLIST\n")
(dst / "master.m3u8").write_text("#EXTM3U\n#EXT-X-VERSION:7\n"
    "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"s\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,URI=\"s.m3u8\"\n"
    "#EXT-X-STREAM-INF:BANDWIDTH=300000,CODECS=\"avc1.64000a,mp4a.40.2\",RESOLUTION=64x64,SUBTITLES=\"s\"\nv.m3u8\n")

# --- DASH: a whole `wvtt` file and a segmented `stpp` track ---------------------------------
dash = out / "dash"
en = [(500, 1500, "hello", ""), (1800, 2600, "across", ""), (5500, 7000, "past the end", "")]
whole = init(b"wvtt", 1, 1000, bx(b"vttC", b"WEBVTT"))
for i in range(3):
    a = 2000 * i
    whole += fragment(i + 1, 1, a, wvtt_samples(en, a, a + 2000 if i < 2 else 8000))
(dash / "text-en.mp4").write_bytes(whole)
ns = b"http://www.w3.org/ns/ttml"
(dash / "text-de-init.mp4").write_bytes(init(b"stpp", 1, 1000, ns + b"\0\0\0"))
de = [(1000, 1900, "eins"), (2500, 3500, "zwei"), (4200, 4800, "drei")]
for i in range(3):
    a, b = 2000 * i, 2000 * (i + 1)
    ps = "".join(f'<p begin="{s/1000:.3f}s" end="{e/1000:.3f}s">{t}</p>' for s, e, t in de if s < b and e > a)
    doc = f'<?xml version="1.0" encoding="UTF-8"?><tt xmlns="http://www.w3.org/ns/ttml" xml:lang="de"><body><div>{ps}</div></body></tt>'
    (dash / f"text-de-{i + 1}.m4s").write_bytes(fragment(i + 1, 1, a, [(2000, doc.encode())]))
mpd = (dash / "manifest.mpd").read_text()
text_sets = (
    '\t\t<AdaptationSet id="2" contentType="text" lang="en">\n'
    '\t\t\t<Role schemeIdUri="urn:mpeg:dash:role:2011" value="subtitle" />\n'
    '\t\t\t<Representation id="t-en" bandwidth="500" codecs="wvtt" mimeType="application/mp4">\n'
    '\t\t\t\t<BaseURL>text-en.mp4</BaseURL>\n\t\t\t\t<SegmentBase timescale="1000" />\n'
    '\t\t\t</Representation>\n\t\t</AdaptationSet>\n'
    '\t\t<AdaptationSet id="3" contentType="text" lang="de">\n'
    '\t\t\t<Representation id="t-de" bandwidth="500" codecs="stpp" mimeType="application/mp4">\n'
    '\t\t\t\t<SegmentTemplate timescale="1000" duration="2000" startNumber="1" initialization="text-de-init.mp4" media="text-de-$Number$.m4s" />\n'
    '\t\t\t</Representation>\n\t\t</AdaptationSet>\n')
assert mpd.count("\t</Period>") == 1
(dash / "subs.mpd").write_text(mpd.replace("\t</Period>", text_sets + "\t</Period>"))
PY

# The same DASH stream, marked as CENC-encrypted in every adaptation set: Siler must refuse it.
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
