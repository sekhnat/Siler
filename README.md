<div align="center">

<img src="app/assets/logo.svg" alt="Siler logo" width="96" height="96">

# Siler

**A fast, beautiful download manager for Linux, with browser extensions for Firefox and Chromium that hand it your downloads and videos.**

[![Latest release](https://img.shields.io/github/v/release/sekhnat/Siler?style=flat-square&color=7c5cff&label=release)](https://github.com/sekhnat/Siler/releases/latest)
[![Linux x86_64](https://img.shields.io/badge/Linux-x86__64-5b6cff?style=flat-square&logo=linux&logoColor=white)](#install)
[![Firefox extension](https://img.shields.io/badge/Firefox-extension-ff7139?style=flat-square&logo=firefoxbrowser&logoColor=white)](docs/browser-extension.md)
[![Chromium extension](https://img.shields.io/badge/Chromium-extension-4285f4?style=flat-square&logo=googlechrome&logoColor=white)](docs/browser-extension.md#chrome-chromium-brave-vivaldi-edge-opera)
[![Rust](https://img.shields.io/badge/Rust-engine-b7410e?style=flat-square&logo=rust&logoColor=white)](docs/architecture.md)
[![Tauri 2 + Svelte 5](https://img.shields.io/badge/Tauri_2-Svelte_5-22d3ee?style=flat-square&logo=tauri&logoColor=white)](docs/architecture.md)

[**Download**](https://github.com/sekhnat/Siler/releases/latest) · [**Browser extension**](docs/browser-extension.md) · [**Documentation**](#documentation)

<br>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/main-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="docs/images/main-light.png">
  <img src="docs/images/main-light.png" alt="Siler's main window: three downloads running at 12 MB/s, one paused, one waiting in a night queue, and finished files sorted into categories" width="880">
</picture>

</div>

## Highlights

**Fast and resilient**
- Segmented downloads over many connections, with dynamic splitting: a free connection takes over half of the largest remaining piece. The connection count can change while a download runs.
- Pause and resume across restarts and crashes; passing network failures are retried, honouring `Retry-After`.
- Mirrors and multi-source downloads, and metalinks with piece hashes.
- **Expired links refresh themselves**: Siler asks the original address again, or the browser the link came from finds it on its page in a background tab. Siler checks that the file is still the same and continues where it was.
- HTTP(S), **FTP, FTPS and SFTP**, through the same proxy; SFTP checks the server's key the way `ssh` does.

**Video and audio**
- HLS, DASH and Adobe HDS streams with a picker for quality, audio languages and subtitles, muxed into MP4 or MKV by a bundled FFmpeg, with no re-encoding.
- **Live streams are recorded** (HLS, DASH, RTMP and MMS) until you press **Finish**; plain M3U, PLS and ASX playlists open as lists of links.
- AES-128 streams and WebVTT/TTML subtitles, timed to the video. DRM-protected streams are refused.
- From the browser: YouTube, Vimeo, Facebook, Instagram, TikTok and any page that plays HLS, DASH or a video file.

**Browser integration**
- Firefox, LibreWolf, Waterfox, Floorp and Zen, and Chrome, Chromium, Brave, Vivaldi, Edge and Opera. Downloads come with their cookies, referrer and form data; in Firefox they are taken before its own dialog appears.
- A **Download** button over videos, **Download all links**, a per-site switch, and a clear message whenever a download stays in the browser.

**Organised and automated**
- Named queues with schedules, and an action when they finish: quit, sleep, hibernate or shut down.
- Categories with their own folders, numbered batches (`img[001-120].jpg`), import and export (links, JSON, metalinks, IDM `.ef2`), and clipboard monitoring.
- Keep files up to date, a site grabber that can also save pages for offline reading, and a command to run after each download.

**In control**
- Global and per-download speed limits, per-server connection limits and a data quota (so many MB every so many hours).
- Checksums (MD5 to SHA-512, and the server's `Digest` header), **Verify checksum…** for any file, and ZIP preview to fetch just the files you need from an archive.
- **Proxies:** the desktop's, HTTP, HTTPS, SOCKS4/4a and SOCKS5, an FTP proxy, **proxy scripts (PAC)** and **automatic detection (WPAD)**, with failover. Scripts run in a separate helper with tight limits on time and memory.
- **Logins:** Basic, Digest, **NTLM** and **Kerberos (Negotiate)**, for sites and proxies, saved in the desktop keyring.

**Your way**
- Cards or a **Details** table with the columns you choose, resizable and in your order, sorted either way; a toolbar you arrange.
- **Skins:** seven built in, each with light and dark colours, or your own from a JSON file.
- Checks for a new version once a day (you can turn it off); the AppImage updates itself after checking the release's checksum.
- Ready for translation: every word is in a catalog ([Translating Siler](docs/translating.md)).

**At home on Linux**
- **A tray panel:** click the tray icon to see each download's progress and speed, pause, resume or cancel them, and set a speed limit. The icon fills a progress ring, and the right-click menu lists the running downloads.
- Starts with your session if you like, waiting in the tray; notifications and sounds; keeps the computer awake while downloads run; drag finished files out of the list. Wayland and X11.

<table>
  <tr>
    <td align="center" valign="top" width="50%"><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/add-video-dark.png"><source media="(prefers-color-scheme: light)" srcset="docs/images/add-video-light.png"><img src="docs/images/add-video-light.png" alt="The Add window with a stream's qualities, audio languages and subtitles" width="400"></picture><br><sub><b>Pick the quality, audio and subtitles of a stream</b></sub></td>
    <td align="center" valign="top" width="50%"><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/settings-dark.png"><source media="(prefers-color-scheme: light)" srcset="docs/images/settings-light.png"><img src="docs/images/settings-light.png" alt="Settings: connections per download, simultaneous downloads, connections per server, speed limit" width="400"></picture><br><sub><b>Tune connections, limits and quotas</b></sub></td>
  </tr>
  <tr>
    <td align="center" valign="top" width="50%"><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/tray-panel-dark.png"><source media="(prefers-color-scheme: light)" srcset="docs/images/tray-panel-light.png"><img src="docs/images/tray-panel-light.png" alt="The tray panel: three downloads with their progress, speed and time left, one waiting and one just finished" width="300"></picture><br><sub><b>Watch and control downloads from the tray</b></sub></td>
    <td align="center" valign="top" width="50%"><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/skins-dark.png"><source media="(prefers-color-scheme: light)" srcset="docs/images/skins-light.png"><img src="docs/images/skins-light.png" alt="Settings → Appearance: seven skins to choose from, with Import and Export" width="400"></picture><br><sub><b>Choose a skin, or make your own</b></sub></td>
  </tr>
</table>

## Install

Siler runs on 64-bit Linux. Each [release](https://github.com/sekhnat/Siler/releases/latest) offers:

**AppImage (recommended)** — everything inside, including the browser host and FFmpeg.

```sh
chmod +x Siler-*-x86_64.AppImage
./Siler-*-x86_64.AppImage --install-integration   # menu entry, icons and the Firefox host
```

Keep the file where it is: the menu entry and the browser host point at it. To have Siler start when you log in, turn on Settings → General → **Start when you log in**. Siler tells you when a new version is out and updates the AppImage itself (**Update and restart**); AppImage updaters (AppImageUpdate, Gear Lever, AppImageLauncher, AppManager) work too, downloading only the changed parts. If you replace or move the file by hand, start it once.

**Arch Linux (AUR)** — `siler-bin`, built from the release tarball: `yay -S siler-bin` (or your AUR helper). It installs Siler, its FFmpeg, both browser extensions and the browser hosts system-wide; updates come with your system's.

**Tarball** — the same programs in a folder (`siler-<version>-linux-x86_64.tar.gz`). Unpack it where it will stay and run `./siler --install-integration`. It needs WebKitGTK 4.1, plus libayatana-appindicator for the tray. Optional: gtk-layer-shell for the tray panel on Wayland, and the MIT Kerberos library (`krb5`) for Kerberos logins.

## Browser extension

<picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/extension-popup-dark.png"><source media="(prefers-color-scheme: light)" srcset="docs/images/extension-popup-light.png"><img src="docs/images/extension-popup-light.png" alt="The extension's toolbar popup: connected to Siler, with active downloads, speed and the capture switch" width="300" align="right"></picture>

1. In Siler, open Settings → Browser and click **Add to Firefox**. The AppImage and the tarball include the extension, signed by Mozilla. You can also download `siler-integration-<version>.xpi` from the [latest release](https://github.com/sekhnat/Siler/releases/latest) and open it with Firefox.
2. On the welcome page, grant access to all sites.

That's it: matching downloads now go to Siler. Hold **Alt** while clicking a link to let Firefox handle it, or turn capture off for a site from the toolbar popup.

**Chrome, Chromium, Brave, Vivaldi, Edge, Opera:** in Siler, open Settings → Browser and click **Set up** under the Chromium browsers. Siler copies the extension to a folder; open the browser's extensions page, turn on **Developer mode**, click **Load unpacked** and choose that folder. (It will be in the Chrome Web Store, too.) More in [Browser extension](docs/browser-extension.md).

<br clear="right">

## Quick start

```sh
siler https://example.com/file.iso                         # open the Add window for a link
siler --silent --dir ~/ISOs https://example.com/a.iso      # add without asking
siler --queue=Night --silent https://example.com/big.iso   # wait in the "Night" queue
siler --start-queue=Night                                  # …and start it later
siler --import links.txt                                   # links, metalinks, JSON or IDM .ef2
```

When Siler is already running, the command line goes to it. See [Using Siler](docs/usage.md) for every option and keyboard shortcut.

## Documentation

| Guide | What's in it |
| --- | --- |
| [Using Siler](docs/usage.md) | Adding downloads, the command line, queues and scheduling, keyboard shortcuts, logs and troubleshooting |
| [Video and audio streams](docs/media.md) | HLS, DASH and HDS, live recording, RTMP and MMS, playlists, the picker, FFmpeg, output formats, subtitles and safety checks |
| [Browser extension](docs/browser-extension.md) | Installing, how capture works, videos on pages, site recognisers and signing your own build |
| [Architecture](docs/architecture.md) | How the pieces fit, the download engine, security and privacy, files on disk |
| [Development](docs/development.md) | Building from source, tests and the release process |
| [Translating Siler](docs/translating.md) | Adding a language to the app and the extension |

## Building from source

You need Rust (stable), Node.js with pnpm, and WebKitGTK 4.1 development files ([details](docs/development.md#requirements)).

```sh
git clone https://github.com/sekhnat/Siler.git && cd Siler
cargo test --workspace                     # engine and app tests
cd app && pnpm install && pnpm tauri dev   # run with hot reload
cd .. && ./scripts/install-dev.sh          # release build + desktop and Firefox registration
```

## Roadmap

- Translations (the catalogs are ready: see [Translating Siler](docs/translating.md))
- The Chromium extension in the Chrome Web Store
- FTP folder listings, recording live streams from the start of their time-shift window

## License

Copyright © sekhnat. All rights reserved.

The source code is published so it can be read and reviewed; no license is granted to copy, modify or redistribute it.

Third-party components keep their own licenses:
- **FFmpeg**, bundled with releases, is LGPL-2.1-or-later and runs as a separate program. `scripts/build-ffmpeg.sh` builds it from the official release.
- The extension's YouTube solver is **yt-dlp/ejs** (Unlicense), with **meriyah** (ISC) and **astring** (MIT); see [`SOURCES.md`](extension/media/youtube/vendor/SOURCES.md).

## Acknowledgements

Siler is built with [Tauri](https://tauri.app), [Svelte](https://svelte.dev), [Rust](https://www.rust-lang.org) and [FFmpeg](https://ffmpeg.org). The site recognisers follow [yt-dlp](https://github.com/yt-dlp/yt-dlp)'s extractors. Icons by [Lucide](https://lucide.dev), type set in [Inter](https://rsms.me/inter/).
