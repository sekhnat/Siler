# Architecture

Siler is a Rust download engine with a Tauri 2 + Svelte 5 desktop app on top. A browser extension sends it downloads through a small native messaging host, and keeps a second connection open so Siler can ask it for fresh links.

```
Browser extension ──native messaging (stdio)──► siler-nmhost ──Unix socket──► Siler app (Tauri)
  hand-overs: starts the app if needed                                          ├─ siler-core engine
  subscription: waits for the app, never starts it                              ├─ SQLite (queue, settings)
                                                                                ├─ Svelte UI, tray panel
                                                                                └─ siler-pac (proxy scripts)
```

## Repository layout

| Path | What it is |
| --- | --- |
| `crates/siler-core` | The download engine (library). It probes the URL, splits the file into segments and fetches them over several connections. When a connection finishes, it takes over the second half of the largest remaining segment. The crate also handles retries with backoff, connection-limit detection (429/503), rate limiting, queues, scheduling, media downloads, checksums, mirrors, metalinks, the site grabber and SQLite persistence. |
| `crates/siler-media` | Pure parsers and helpers for media downloads: HLS and DASH manifests, MP4 boxes, stream selection, protection (DRM) detection, AES-128, and WebVTT/TTML subtitles. No I/O. |
| `crates/siler-ipc` | The message types and framing shared by the app and the native host. Framing is a 4-byte little-endian length followed by JSON, the same as WebExtension native messaging. |
| `crates/siler-nmhost` | The native messaging host. It relays browser messages to `$XDG_RUNTIME_DIR/siler/siler.sock`, launching `siler --background` if the app isn't running. A subscribing connection (for fresh links) is relayed both ways for as long as the browser keeps it, and waits for the app instead of starting it. |
| `crates/siler-pac` | The proxy script helper: runs one PAC script for one address in QuickJS and prints the answer. See [Proxies](#proxies). |
| `app/` | The Tauri 2 desktop app: Svelte 5 UI, tray icon, panel and menu (`src-tauri/src/tray/`), skins (`app/skins/`, `src-tauri/src/skins.rs`), notifications, socket server (`src-tauri/src/ipc_server.rs`) and the browsers' fresh-link channel (`src-tauri/src/links.rs`). |
| `extension/` | The MV3 extension for Firefox and Chromium: download capture, video detection and site recognisers, fresh links (`links.js`), context menus, popup and options page. See [Browser extension](browser-extension.md). |
| `scripts/` | Development and release helpers: `install-dev.sh`, `sign-extension.sh`, `build-ffmpeg.sh`, `appimage-zsync.sh` and others. See [Development](development.md). |

## Download engine

- Data is written into a preallocated `<name>.siler.part` file using positional writes. Every 32 MiB a connection asks the kernel to start writing its part to disk, so the flush when the download finishes is short; that flush runs off the async runtime, and the download is only marked finished once its data is on disk. Progress is checkpointed about once a second, on pause, on failure and on quit, and the database's write-ahead log is folded back in whenever nothing is downloading. A checkpoint never claims more than is written: pauses, failures and quitting flush the partial file to disk before its progress is saved, and **Extra-safe resume** (Settings → Transfers) does the same before every checkpoint, so a power cut can't leave a resumed download with gaps. That flush runs off the async runtime, one at a time, and saves the progress from before it.
- Resuming re-probes the URL. If the size, ETag or Last-Modified changed, the download starts over and shows a notice.
- How bytes are fetched is a transport per URL scheme (`crates/siler-core/src/transport/`): HTTP(S) with reqwest, FTP/FTPS with a small client of Siler's own, and SFTP with russh. A transport answers two questions, what a file is (`probe`) and its bytes from one offset to another (`open_range`); segments, connection limits, retries, mirrors, speed limits and the quota are the same for all of them.
- The HTTP client is HTTP/1.1 only and sends `Accept-Encoding: identity`. This makes each connection a real TCP connection, and keeps byte ranges pointing at the file itself.
- Every HTTP request goes through one place (`crates/siler-core/src/session/`), which follows redirects one hop at a time, picks the proxy for each hop, scopes the browser's cookies and headers to each hop's server, and answers 401 and 407 with a login: Basic, Digest, NTLM or Negotiate (Kerberos). NTLM authenticates a connection, not a request, so its handshake runs on a client with a single connection of its own, which is reused once a response's body has ended; the first handshake per server runs alone, and a refused password isn't tried again.
- An expired link (403, 404, 410, a 401 that asks for no login, or a stream whose links stop working) is refreshed in up to three steps, described in [Using Siler](usage.md#managing-downloads). A download waiting for a fresh link counts as busy, so an action when downloads finish waits for it.
- FTP uses passive mode (EPSV, else PASV, always to the control connection's host), `REST` for ranges and `MDTM` in place of an ETag. A range that ends before the file does closes its connections; a whole file returns its logged-in control connection to a small pool. FTP and SSH connections go through the proxy the route names (`transport/dial.rs`): SOCKS4, SOCKS4a or SOCKS5 directly, an HTTP or HTTPS proxy with `CONNECT`, or an FTP proxy (`USER user@server`).
- A connection limit per server is shared by all of that server's downloads, with exceptions for single servers.
- Mirrors and multi-source downloads: other addresses of the same file share the work, each checked first for the same size and range support. A mirror that keeps failing is dropped and its work moves to the others. Servers' `Link: rel=duplicate` headers (RFC 6249) add mirrors automatically.
- Metalinks (RFC 5854 `.meta4` and 3.0 `.metalink`) become the files they list, with their mirrors, checksums and piece hashes. With piece hashes, only damaged pieces are fetched again.
- Checksums (MD5, SHA-1, SHA-256, SHA-512, or a server's `Digest`/`Repr-Digest` header) are checked before the file gets its final name. A file that doesn't match is kept until you choose **Download again** or **Keep it anyway**.
- ZIP preview lists the files inside a ZIP on a server with two small range requests. Single files can be fetched out of it (only their compressed bytes), unpacked and checked against their CRC-32.
- Scheduling runs while Siler is running (in the tray is fine). It can't wake a sleeping or switched-off computer: that needs root (rtcwake) and is left out.
- Media downloads (HLS, DASH, HDS, live recordings, RTMP and MMS) are described in [Video and audio streams](media.md).

## Proxies

- `proxy/router.rs` chooses a route for every request and every redirect hop: direct, a proxy, or a list to try in order. A proxy that fails is skipped for five minutes. Clients are kept per route.
- Settings, the environment and the KDE and GNOME settings give a fixed proxy, a script, or "detect". Detecting (`proxy/wpad.rs`) asks NetworkManager over D-Bus for the DHCP server's WPAD answer (option 252), then tries `wpad.<domain>` for up to four of the network's domains, never under a public suffix (by the Public Suffix List).
- Scripts run in `siler-pac`, a separate process: QuickJS with only the standard PAC functions, no network or file access, a 32 MiB heap, a 4-second interrupt, and process limits (6 s of CPU, 512 MiB of memory, no files written, `no_new_privs`). It gets the script, the address and the host on stdin, with an empty environment apart from the time zone, and is killed after 12 seconds. Siler looks for it in `SILER_PAC`, beside its own program, then in `/usr/lib/siler/`. Answers are cached for five minutes.
- A script that fails stops the download (error `proxy-script`) instead of connecting directly; only "detect" connects directly when it finds no script.
- A proxy that asks for an NTLM or Negotiate login on `CONNECT` needs several round trips on one connection, which reqwest can't do. Only that case goes through a tunnel Siler opens itself: an abstract Unix socket that accepts only Siler's own process.

## Security and privacy

- The database is mode 0600 because it stores forwarded cookies, which are needed to resume. Cookies are never sent to the UI.
- The socket is mode 0600 and only accepts peers with the same UID.
- Partial files (`.siler.part`, a stream's `.part` files, the scratch file a ZIP entry is unpacked into) are mode 0600 until they are published, when they get the usual mode for a new file. Siler never writes through a symbolic link or into a file with other names (a hard link) at those paths: a new partial is only created where nothing is, a partial is continued only when it is a plain file with one name, and anything else stops the download with an error naming the path (`unsafe-part`). It isn't removed, since it may not be Siler's. These checks are on path names, so they can't beat someone who can write to the download folder and races them; keep downloads in folders only you (and people you trust) can write to.
- Logins are kept in memory and, when you choose to save them, in the desktop keyring (Secret Service API: KWallet, GNOME Keyring, KeePassXC…). They are never written to the database. A login belongs to a scheme, server and port and is only sent there; redirects to another host don't carry it, and an https login never goes to http. A login saved for the manual proxy is never sent to a proxy that a script chose. The keyring is read only when a server asks for a login, or at start when a manual proxy has a username.
- The cookies, headers and form data a download came with from the browser go only to the same scheme, server and port (or to its https upgrade). Other servers a download is redirected to get the user agent and a referrer without a path. Streams may use them on the same site (by the Public Suffix List).
- Kerberos tickets and NTLM answers go only to a site or proxy you approved in the login window (with a password, or **Use my Kerberos ticket**), by scheme, server and port; a proxy that a script or detection found is asked about like any other. Tickets are never delegated. The MIT Kerberos library is loaded when it is first needed, so Siler runs without it.
- To find a fresh link, the browser only opens the download's page in a background tab and reads its links; it never clicks or submits anything, and never acts for a private window. Siler checks the new link gives the same file before it continues.
- Skins are JSON with known colour names and hex values only, 64 KB at most, so an imported skin can't add styles or scripts.
- SFTP server keys are checked against `~/.ssh/known_hosts` and Siler's own list; an unknown key waits for the user to trust it, a changed one is refused.
- The update check asks `api.github.com` for the latest release once a day (or never, if turned off) and sends nothing about the user. An AppImage update is used only when its SHA-256 matches the release's checksum file.
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
| Imported skins | `~/.local/share/app.siler.Siler/skins/` |
| Proxy script helper | beside `siler`, or `/usr/lib/siler/siler-pac` for packages |

## Database schema

The schema migrates on start:

| Version | Adds |
| --- | --- |
| v2 | Queue position and conflict policy |
| v3 | Named queues and keeping files up to date |
| v4 | Media downloads |
| v5 | Checksums, mirrors, archive entries and site-grabber projects |

A database moved to a newer schema can't be opened by an older version of Siler.
