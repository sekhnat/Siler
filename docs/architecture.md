# Architecture

Siler is a Rust download engine with a Tauri 2 + Svelte 5 desktop app on top. A Firefox extension sends it downloads through a small native messaging host.

```
Firefox extension ──native messaging (stdio)──► siler-nmhost ──Unix socket──► Siler app (Tauri)
                                                 starts the app if needed      ├─ siler-core engine
                                                                               ├─ SQLite (queue, settings)
                                                                               └─ Svelte UI
```

## Repository layout

| Path | What it is |
| --- | --- |
| `crates/siler-core` | The download engine (library). It probes the URL, splits the file into segments and fetches them over several connections. When a connection finishes, it takes over the second half of the largest remaining segment. The crate also handles retries with backoff, connection-limit detection (429/503), rate limiting, queues, scheduling, media downloads, checksums, mirrors, metalinks, the site grabber and SQLite persistence. |
| `crates/siler-media` | Pure parsers and helpers for media downloads: HLS and DASH manifests, MP4 boxes, stream selection, protection (DRM) detection, AES-128, and WebVTT/TTML subtitles. No I/O. |
| `crates/siler-ipc` | The message types and framing shared by the app and the native host. Framing is a 4-byte little-endian length followed by JSON, the same as WebExtension native messaging. |
| `crates/siler-nmhost` | The native messaging host. It relays browser messages to `$XDG_RUNTIME_DIR/siler/siler.sock`, launching `siler --background` if the app isn't running. |
| `app/` | The Tauri 2 desktop app: Svelte 5 UI, tray icon, notifications, socket server (`src-tauri/src/ipc_server.rs`). |
| `extension/` | The Firefox MV3 extension: download capture, video detection and site recognisers, context menus, popup and options page. See [Browser extension](browser-extension.md). |
| `scripts/` | Development and release helpers: `install-dev.sh`, `sign-extension.sh`, `build-ffmpeg.sh`, `appimage-zsync.sh` and others. See [Development](development.md). |

## Download engine

- Data is written into a preallocated `<name>.siler.part` file using positional writes. Progress is checkpointed about once a second, on pause and on quit.
- Resuming re-probes the URL. If the size, ETag or Last-Modified changed, the download starts over and shows a notice.
- The HTTP client is HTTP/1.1 only and sends `Accept-Encoding: identity`. This makes each connection a real TCP connection, and keeps byte ranges pointing at the file itself.
- A connection limit per server is shared by all of that server's downloads, with exceptions for single servers.
- Mirrors and multi-source downloads: other addresses of the same file share the work, each checked first for the same size and range support. A mirror that keeps failing is dropped and its work moves to the others. Servers' `Link: rel=duplicate` headers (RFC 6249) add mirrors automatically.
- Metalinks (RFC 5854 `.meta4` and 3.0 `.metalink`) become the files they list, with their mirrors, checksums and piece hashes. With piece hashes, only damaged pieces are fetched again.
- Checksums (MD5, SHA-1, SHA-256, SHA-512, or a server's `Digest`/`Repr-Digest` header) are checked before the file gets its final name. A file that doesn't match is kept until you choose **Download again** or **Keep it anyway**.
- ZIP preview lists the files inside a ZIP on a server with two small range requests. Single files can be fetched out of it (only their compressed bytes), unpacked and checked against their CRC-32.
- Scheduling runs while Siler is running (in the tray is fine). It can't wake a sleeping or switched-off computer: that needs root (rtcwake) and is left out.
- Media downloads (HLS and DASH) are described in [Video and audio streams](media.md).

## Security and privacy

- The database is mode 0600 because it stores forwarded cookies, which are needed to resume. Cookies are never sent to the UI.
- The socket is mode 0600 and only accepts peers with the same UID.
- Logins are kept in memory and, when you choose to save them, in the desktop keyring (Secret Service API: KWallet, GNOME Keyring, KeePassXC…). They are never written to the database. A login is only sent to the server it belongs to; redirects to another host don't carry it. The keyring is read only when a server asks for a login, or at start when a manual proxy has a username.
- Media playlists are checked before anything is fetched. Their links must be `http(s)` and by default may not point at this computer or the local network. See [Media safety](media.md#safety).
- The command after each download runs without a shell, one download at a time. `{path}`, `{dir}`, `{name}`, `{url}` and `{size}` are filled in inside each word, so a file name with spaces, quotes or `;` stays one argument and can't run anything else.

## Files on disk

| What | Where |
| --- | --- |
| Database | `~/.local/share/app.siler.Siler/siler.db` |
| Logs | `~/.local/state/siler/logs/` |
| IPC socket | `$XDG_RUNTIME_DIR/siler/siler.sock` |
| Named pipes for FFmpeg | `$XDG_RUNTIME_DIR/siler/media/` |
| Browser host copy (AppImage) | `~/.local/share/siler/bin/` |
| Native host manifest | `~/.mozilla/native-messaging-hosts/com.siler.nmhost.json` (and the other browsers' folders) |
| Menu entry and icons | `~/.local/share/applications/siler.desktop`, `~/.local/share/icons/hicolor/*/apps/siler.*` |
| Start when you log in | `~/.config/autostart/siler.desktop` (runs `siler --background`) |
| Extension handed to Firefox | `~/.cache/siler/siler-integration.xpi` |

## Database schema

The schema migrates on start:

| Version | Adds |
| --- | --- |
| v2 | Queue position and conflict policy |
| v3 | Named queues and keeping files up to date |
| v4 | Media downloads |
| v5 | Checksums, mirrors, archive entries and site-grabber projects |

A database moved to a newer schema can't be opened by an older version of Siler.
