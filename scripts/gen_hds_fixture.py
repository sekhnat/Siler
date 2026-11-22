#!/usr/bin/env python3
"""Packs an FLV file into an Adobe HDS stream for the media tests: F4F fragments (one per
keyframe-started group of tags, in an mdat box), a bootstrap (abst) numbering them, and an
F4M manifest. Usage: gen_hds_fixture.py <source.flv> <out dir>"""
import pathlib
import struct
import sys

src, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
data = src.read_bytes()
assert data[:3] == b"FLV", "not an FLV file"
pos = struct.unpack(">I", data[5:9])[0] + 4  # header, then PreviousTagSize0

tags = []  # (is_video_keyframe, timestamp_ms, bytes)
while pos + 11 <= len(data):
    kind = data[pos]
    size = int.from_bytes(data[pos + 1:pos + 4], "big")
    ts = int.from_bytes(data[pos + 4:pos + 7], "big") | (data[pos + 7] << 24)
    end = pos + 11 + size + 4
    key = kind == 9 and (data[pos + 11] >> 4) == 1
    tags.append((key, ts, data[pos:end]))
    pos = end

# Fragments start at video keyframes; the metadata tag goes with the first one.
frags = []  # [start ms, tags, has a keyframe]
for key, ts, raw in tags:
    if not frags or (key and frags[-1][2]):
        frags.append([ts if frags else 0, [], False])
    frags[-1][1].append(raw)
    frags[-1][2] |= key
starts = [f[0] for f in frags]
last_ts = tags[-1][1]


def box(kind, body):
    return struct.pack(">I", len(body) + 8) + kind + body


for i, (start, raws, _) in enumerate(frags, 1):
    afra = b"\0" * 8
    (out / f"v_300Seg1-Frag{i}").write_bytes(box(b"afra", afra) + box(b"mdat", b"".join(raws)))

durations = [max(b - a, 1) for a, b in zip(starts, starts[1:] + [last_ts + 100])]
asrt = b"\0\0\0\0" + b"\0" + struct.pack(">I", 1) + struct.pack(">II", 1, len(frags))
runs = b"".join(struct.pack(">IQI", i, s, d) for i, (s, d) in enumerate(zip(starts, durations), 1))
afrt = b"\0\0\0\0" + struct.pack(">I", 1000) + b"\0" + struct.pack(">I", len(frags)) + runs
abst = (
    b"\0\0\0\0" + struct.pack(">I", 1) + b"\0" + struct.pack(">I", 1000)
    + struct.pack(">QQ", last_ts, 0) + b"\0" + b"\0" + b"\0" + b"\0" + b"\0"
    + b"\x01" + box(b"asrt", asrt) + b"\x01" + box(b"afrt", afrt)
)
(out / "boot.abst").write_bytes(box(b"abst", abst))
(out / "manifest.f4m").write_text(
    '<?xml version="1.0" encoding="UTF-8"?>\n'
    '<manifest xmlns="http://ns.adobe.com/f4m/1.0">\n'
    "  <id>fixture</id>\n  <streamType>recorded</streamType>\n"
    f"  <duration>{(last_ts + 100) / 1000:.1f}</duration>\n"
    '  <bootstrapInfo profile="named" id="boot" url="boot.abst"/>\n'
    '  <media url="v_300" bitrate="300" width="64" height="64" bootstrapInfoId="boot"/>\n'
    "</manifest>\n"
)
print(f"{len(frags)} fragments")
