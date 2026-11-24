# HTTP/2 connections per server: real-CDN benchmark

**HTTP/2 connections per server** (Settings → Transfers) is off by default. These are the numbers behind that default. They come from `crates/siler-core/tests/real_h2.rs`:

```sh
SILER_REAL_H2=1 cargo test --release -p siler-core --test real_h2 -- --ignored --nocapture
```

Each run uses a fresh engine, so its connections start cold. The settings were run one after another for each file.

## 2026-11-24, home connection (about 200 Mbit/s), one round

| File | Connections | HTTP/2 slots | MB/s | Protocol | Retries | Fetched twice |
|---|---|---|---|---|---|---|
| kernel.org (Fastly), 148 MB | 8 | off | 3.7 | HTTP/1.1 | 0 | 0.0 MB |
| kernel.org (Fastly) | 8 | 1 | 0.6 | HTTP/2 | 0 | 0.0 MB |
| kernel.org (Fastly) | 8 | 4 | 2.2 | HTTP/2 | 0 | 0.0 MB |
| kernel.org (Fastly) | 8 | 16 | 2.8 | HTTP/2 | 0 | 0.0 MB |
| kernel.org (Fastly) | 1 | 4 | 0.9 | HTTP/1.1 | 0 | 0.0 MB |
| dl.google.com, 153 MB | 8 | off | 12.8 | HTTP/1.1 | 0 | 0.1 MB |
| dl.google.com | 8 | 1 | 8.1 | HTTP/2 | 0 | 0.0 MB |
| dl.google.com | 8 | 4 | 13.6 | HTTP/2 | 0 | 0.0 MB |
| dl.google.com | 8 | 16 | 15.3 | HTTP/2 | 0 | 0.1 MB |
| dl.google.com | 1 | 4 | 12.5 | HTTP/1.1 | 0 | 0.0 MB |
| download.blender.org, 352 MB | 8 | off | 23.2 | HTTP/1.1 | 0 | 0.2 MB |
| download.blender.org | 8 | 1 | 24.7 | HTTP/2 | 0 | 0.0 MB |
| download.blender.org | 8 | 4 | 22.6 | HTTP/2 | 0 | 0.0 MB |
| download.blender.org | 8 | 16 | 25.8 | HTTP/2 | 0 | 0.0 MB |
| download.blender.org | 1 | 4 | 7.6 | HTTP/1.1 | 0 | 0.0 MB |

## What it shows

- **No setting beats HTTP/1.1 clearly.** With 16 slots, dl.google.com and blender came out 8–20 % ahead, which is within one round's noise. kernel.org was slower on every HTTP/2 setting.
- **Few slots are worse where the CDN limits speed per connection.** kernel.org (Fastly) gives each connection a little speed: 8 streams on one HTTP/2 connection got 0.6 MB/s against 3.7 MB/s over 8 HTTP/1.1 connections. Google behaves the same at 1 slot (8.1 vs 12.8). That's the per-connection throttling the spec listed as risk (a).
- **On a fast line the flow-control windows aren't the limit.** At 8 streams, blender reached the line's speed at every setting, including 1 slot. The window ceiling HTTP/2 slots exist to lift only appears with much more bandwidth × latency than this line has.
- **A lone stream stays on HTTP/1.1 even with slots on,** as designed (the `1 | 4` rows). Its speed is what one connection gets from each CDN.
- **HTTP/2 caused no retries and no waste on any CDN.**

**Decision:** the default stays off. Turning it on is worth it only on a line where 8 HTTP/1.1 connections can't fill the bandwidth, and then with many slots (16). Before any change of default, this needs more rounds and a faster or farther line.
