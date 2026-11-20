# K-Star

A Linux-first download manager with a Firefox extension. The long-term target is feature parity with Internet Download Manager.

**Milestone 1 (this code)** includes:
- Segmented, multi-connection HTTP(S) downloads with dynamic splitting. The connection count can be changed while a download runs.
- Pause and resume, including after a restart or crash. Passing network failures are retried, honouring the server's `Retry-After`.
- Named queues, each with its own limit on downloads at a time: "Download later", Start/Stop queue, reordering, Start now, and moving downloads between queues.
- Scheduled queues: start (and optionally stop) at set times on chosen days of the week, or once. A queue whose time has come starts when K-Star starts.
- When a queue (or everything) has finished: quit K-Star, or put the computer to sleep, hibernate or shut it down after a one-minute countdown that can be cancelled.
- Keeping files up to date: a finished download can be checked every hour, day or week, and a newer version on the server replaces the file once it has downloaded.
- Clipboard monitoring (off by default): copying a link to a file with a known extension offers it in the Add window. Works on KDE/wlroots Wayland (data-control protocol) and X11.
- Sounds for a finished or failed download and a finished queue, from the desktop's sound theme or chosen files (off by default).
- Expired links can be replaced without losing progress ("Refresh link"). There are also Restart and Download again actions.
- Categories with editable file types and folders.
- A rule for when a file already exists: ask, keep both, replace, or skip.
- Global and per-download speed limits, including custom values.
- A connection limit per server, shared by all of that server's downloads, with exceptions for single servers.
- Logins: when a server asks for a username and password (Basic or Digest), K-Star asks once and can save the login in the desktop keyring (KWallet, GNOME Keyring, KeePassXC… through the Secret Service API). `user:password@` in a link works too.
- Proxy: none, the desktop's (environment variables, KDE or GNOME settings), or a manual HTTP/SOCKS5 proxy with an optional login.
- Keeps the computer awake while downloads run (logind sleep inhibitor; can be turned off).
- A Tauri + Svelte desktop app with:
  - a right-click menu, Properties (rename, move), sorting, and keyboard navigation;
  - drag-and-drop and multi-link paste;
  - a KDE/SNI tray with live tooltip, and clickable notifications;
  - a confirmation before quitting, and remembered window size;
  - finished files can be dragged out of the list into a file manager or another app.
- A Firefox extension (also LibreWolf, Waterfox, Floorp, Zen). It captures downloads, form (POST) downloads included, with their cookies and referrer, and:
  - says why when a download stays in Firefox;
  - has a per-site switch;
  - lets you review "Download all links" in K-Star;
  - finds the videos pages play (HLS, DASH and plain files) and hands them to K-Star from a Download button over the video, the toolbar popup or the right-click menu, with recognisers for YouTube, Vimeo, Facebook, Instagram and TikTok.

## Architecture

```
Firefox extension ──native messaging (stdio)──► kstar-nmhost ──Unix socket──► K-Star app (Tauri)
                                                 starts the app if needed      ├─ kstar-core engine
                                                                               ├─ SQLite (queue, settings)
                                                                               └─ Svelte UI
```

| Path | What it is |
| --- | --- |
| `crates/kstar-core` | The download engine (library). It probes the URL, splits the file into segments and fetches them over several connections. When a connection finishes, it takes over the second half of the largest remaining segment. The crate also handles retries with backoff, connection-limit detection (429/503), rate limiting, the queue, and SQLite persistence. |
| `crates/kstar-ipc` | The message types and framing shared by the app and the native host. Framing is a 4-byte little-endian length followed by JSON, the same as WebExtension native messaging. |
| `crates/kstar-nmhost` | The native messaging host. It relays browser messages to `$XDG_RUNTIME_DIR/kstar/kstar.sock`, launching `kstar --background` if the app isn't running. |
| `app/` | The Tauri 2 desktop app: Svelte 5 UI, tray icon, notifications, socket server (`src-tauri/src/ipc_server.rs`). |
| `extension/` | The Firefox MV3 extension: download capture, context menus, popup and options page. |
| `scripts/install-dev.sh` | Builds the app and the native host, then registers the host with Firefox. |

### Engine notes
- Data is written into a preallocated `<name>.kstar.part` file using positional writes. Progress is checkpointed about once a second, on pause and on quit.
- Resuming re-probes the URL. If the size, ETag or Last-Modified changed, the download starts over and shows a notice.
- The HTTP client is HTTP/1.1 only and sends `Accept-Encoding: identity`. This makes each connection a real TCP connection, and keeps byte ranges pointing at the file itself.
- The database (`~/.local/share/app.kstar.KStar/kstar.db`) is mode 0600 because it stores forwarded cookies, which are needed to resume. Cookies are never sent to the UI.
- The socket is mode 0600 and only accepts peers with the same UID.
- Scheduling runs while K-Star is running (in the tray is fine). It can't wake a sleeping or switched-off computer: that needs root (rtcwake) and is left out.
- Media downloads (HLS and DASH streams) read the manifest again at every attempt, so expired signed links are replaced by the playlist's own refresh. Downloaded pieces are kept in a hidden work folder (`.<name>.kstar.media`) beside the output until it is published, and never fetched twice.
- Media playlists are checked before anything is fetched: a manifest's own links must be `http(s)`, and by default may not point at this computer or the local network (Settings → Media can allow it). Names are checked in the same lookup the connection uses, so a name can't pass the check with a public address and then connect to a private one (DNS rebinding). A proxy on this computer still works. Redirects are followed one hop at a time and each hop is checked the same way. The cookies and headers the browser sent go only to the stream's own site, not to other servers its playlist names. DRM-protected streams are refused, and K-Star never contacts a license server. Live streams and HDS, generic playlists, SAMPLE-AES and subtitles carried inside WebM are not downloaded yet.
- Logins are kept in memory and, when you choose to save them, in the keyring; they are never written to the database. A login is only sent to the server it belongs to (redirects to another host don't carry it). The keyring is read only when a server asks for a login, or at start when a manual proxy has a username.

## Development

Requirements: Rust (stable), Node and pnpm, WebKitGTK 4.1, libayatana-appindicator (for the tray).

```sh
# Engine tests (local axum test server: ranges, throttling, drops, 503 caps, redirects, ETag changes…)
cargo test --workspace

# Run the app with hot reload
cd app && pnpm install && pnpm tauri dev

# Type-check the UI
cd app && pnpm check

# Release build + register the native host (all Firefox-based browsers found),
# the application menu entry and the icons
./scripts/install-dev.sh

# Sign the extension (unlisted on addons.mozilla.org) so it stays installed
./scripts/sign-extension.sh
```

### Using K-Star
- **Command line:** `kstar [OPTIONS] [URL…]` adds links to a running K-Star, or starts it (`kstar --help` lists the options). The `.desktop` entry passes links dropped on the launcher the same way.
  - `--silent` adds without the Add window; `--background` starts in the tray.
  - `--dir PATH` and `--filename NAME` choose where a link is saved (`--filename` needs exactly one link).
  - `--queue` adds the links as "Download later"; `--start-queue` starts everything waiting in the main queue.
  - `--queue=NAME` and `--start-queue=NAME` do the same for a named queue (only the `=` form takes a name, any case). An unknown name is refused.
  - Example: `kstar --queue=Night --silent --dir ~/ISOs https://example.com/a.iso`, and later `kstar --start-queue=Night`.
- **Logs:** written to `~/.local/state/kstar/logs/`, as `kstar.<date>.log` (kept for a week), `nmhost.log`, and `kstar-stderr.log` when the browser started K-Star. Settings → General → **Open log folder** opens the folder.
- **Data:** stored in `~/.local/share/app.kstar.KStar/kstar.db`. The schema migrates on start (v1 → v2 adds the queue position and conflict policy; v2 → v3 adds named queues and keeping files up to date; v3 → v4 adds media downloads).
- **Video and audio streams:** paste an HLS (`.m3u8`) or DASH (`.mpd`) address into the Add window (or on the command line, or copy one: the clipboard watcher offers it). K-Star lists the video qualities, audio languages and subtitles, and downloads the ones you pick into one file. Streams are muxed with **FFmpeg**: K-Star ships a static build made from a pinned release by `scripts/build-ffmpeg.sh` (only the codecs, containers and filters it needs; no re-encoding). `scripts/install-dev.sh` builds it the first time and puts `ffmpeg` and `ffprobe` next to the app. Without FFmpeg, single-track downloads still work; combining video and audio needs it. Settings → Media shows which FFmpeg is in use; `KSTAR_FFMPEG=/path/to/ffmpeg` overrides it. The bundled build enables nothing that needs `--enable-gpl`, so it is LGPL-2.1-or-later; it runs as a separate program. The build checks the release tarball against a pinned SHA-256 (its PGP signature can be checked by hand).
  - Output is MP4 when every codec fits it, and MKV otherwise (Opus audio, VP9, unknown codecs, or subtitles kept inside the file); the Add window says why. Subtitles go inside the file by default, or beside it as `<name>.<language>.vtt`. WebVTT and TTML subtitles are read from plain files and from inside MP4 (`wvtt`, `stpp`, which K-Star reads itself because FFmpeg can't), and timed to the video. No cue runs past the end of the picture and sound, so the subtitles never make the file longer.
  - AES-128 streams are decrypted as they download, including encrypted initialisation segments (`EXT-X-MAP` under an `EXT-X-KEY`). Each key is fetched once and kept only in memory.
  - While a stream downloads, its pieces sit in the work folder. FFmpeg then reads each track's pieces in order through a named pipe (under `$XDG_RUNTIME_DIR/kstar/media/`), so no joined copy is written. If a track can't be read from a pipe (an MP4 with its index at the end), K-Star joins it into one file and runs FFmpeg again. The pieces stay until the output is written, so a download needs about twice the stream's size on disk at the end.
  - Real CDNs: `KSTAR_REAL_CDN=1 KSTAR_FFMPEG=… cargo test -p kstar-core --test real_cdn -- --ignored --nocapture` downloads the lowest quality of a few public test streams (Apple, Unified Streaming, Akamai, Shaka, Mux, an AES-128 stream) and checks the files with `ffprobe`. It needs the network, so plain `cargo test` skips it; `KSTAR_REAL_CDN_ONLY=<text>` picks streams by name or URL.
  - Test streams: `scripts/gen_media_fixtures.sh` regenerates the small fixtures under `crates/kstar-core/tests/media`, which the media tests use.
- **If K-Star can't start** (e.g. the database can't be opened), the window shows the error instead of the app disappearing.

### Firefox extension
1. Run `./scripts/install-dev.sh`. Alternatively, open K-Star → Settings → Browser → **Install**; this needs `kstar-nmhost` next to the `kstar` binary. The host manifest goes to `~/.mozilla/native-messaging-hosts/`, which Firefox, Zen and Floorp read. A copy goes to `~/.librewolf/`, `~/.waterfox/`, `~/.floorp/` and `~/.zen/` when those browsers are installed.
2. Install the extension permanently. Release builds of Firefox only keep signed extensions:
   1. Create an API key at <https://addons.mozilla.org/developers/addon/api/key/>.
   2. Put it in `~/.config/kstar/amo.env` (`AMO_JWT_ISSUER=…`, `AMO_JWT_SECRET=…`, then `chmod 600`).
   3. Run `./scripts/sign-extension.sh`. It uploads the extension for an **unlisted** signature: it is signed but not published.
   4. Click K-Star → Settings → Browser → **Add to Firefox**.

   Every signing needs a new `version` in `extension/manifest.json`.
3. For a quick test instead, open `about:debugging#/runtime/this-firefox`, choose **Load Temporary Add-on** and pick `extension/manifest.json`. It is removed when Firefox restarts. `npx web-ext run -s extension` also works, except on distributions that lock `devtools.debugger.remote-enabled`, such as CachyOS.
4. On the welcome page, grant access to all sites. Firefox treats MV3 host permissions as optional.

How capture works:
- **Before Firefox sees it:** downloads are recognised from the response headers and cancelled before Firefox creates a download, so its dialog and download panel never appear.
- **Which downloads:** a download is handed over when its extension or MIME type matches the rules on the options page.
- **Bypassing:** hold **Alt** while clicking a link to let Firefox download it instead.
- **Forms:** POST downloads are taken too. The form body (up to 1 MiB, file uploads excluded) is replayed for every connection.
- **New tabs:** a tab that was opened only for the download is closed once K-Star has it.
- **Per site:** the toolbar popup has a "Capture on this site" switch.
- **Fallback:** if K-Star can't be reached, the download stays in Firefox. If the hand-over fails, Firefox downloads it again itself. Either way a notification says why; this can be turned off in the options.
- **Download all links:** sends every link on the page or in the selection to K-Star. Links that look like files are pre-selected, and the batch window can filter by type or name.
- **Versions:** the extension and the app exchange a protocol version. If either is out of date, the popup, the options page and K-Star's settings say which one to update.
- **"Always ask where to save files":** if this Firefox setting is on, the hand-over happens after the save dialog.

### Videos on web pages
- **Finding them:** the extension watches what a page's player loads. HLS and DASH manifests (by address or content type) and whole video or audio files (512 KB and up; pieces of streams are skipped) are listed per tab, with their number on the toolbar button. An HLS master playlist is listed once, without the playlists it names.
- **Download button:** pointing at a video (240×135 or larger) shows a **Download** button at its top right. It runs in the extension's own world, in a closed shadow root, so pages can't style or script it. The popup's "Videos on this page" list and the right-click menu's **Download this video with K-Star** do the same.
- **Hand-over:** the video goes to K-Star's Add window with the page's cookies, referrer and user agent. A stream opens the quality, audio and subtitle picker; a file opens the usual form. The window says which site it came from.
- **Site recognisers**, each switchable in the options (ported from yt-dlp's extractors, which read the same data):
  - **YouTube:** the extension asks YouTube's player API from the page, as the clients yt-dlp uses by default without a PO token (`tv_samsung`, then `visionos`). It finishes the signed links with YouTube's own player code, run by yt-dlp's [ejs](https://github.com/yt-dlp/ejs) solver inside a blank frame of the page (`extension/media/youtube/vendor/`, copied unmodified by `scripts/vendor-extension-libs.sh`; see `SOURCES.md`). Before handing over, it checks that one link answers and falls back to the next client. K-Star receives a DASH manifest of every format (up to 4K, H.264/VP9/AV1, AAC/Opus) with captions as WebVTT. It fetches the files in 10 MiB ranges, as yt-dlp does, and waits out a pre-roll ad when YouTube asks for that.
  - **Vimeo:** the player config's HLS stream, with its subtitle files. Videos whose owners turned on DRM are refused, like any DRM stream.
  - **Facebook** and **Instagram:** the DASH manifest the page's data carries (read from its embedded JSON and its GraphQL answers), or the plain video file when there is none.
  - **TikTok:** the best H.264 file of the video, with the page's cookies and referrer, which TikTok's servers require.
- **Names:** videos are named by their title (slashes become "⧸", as yt-dlp writes them).
- **Limits:** DRM-protected videos are refused. Live streams aren't recorded. Links from a page expire (YouTube's after a few hours): a download whose links expired says so, and the video has to be sent from the page again. Content that needs a login works only when you are logged in in Firefox. Sites change their players; recogniser fixes come with extension updates.
- **Mozilla review:** the YouTube solver compiles YouTube's player script with `Function`. `web-ext lint` reports this as `DANGEROUS_EVAL`, a warning that doesn't stop unlisted signing.
- **Tests:** `node --test extension/tests/` checks the request rules and each recogniser against recorded, scrubbed answers in `extension/tests/fixtures/`. The app's DASH parser reads the same YouTube manifest snapshot in its own tests.
- **Protocol 2:** the extension 0.3.0 and K-Star 0.6.0 speak protocol 2, which adds these video details to a hand-over. An older app still takes plain downloads.

## Roadmap
- **M2:** done in 0.6: videos found by the Firefox extension, with YouTube, Vimeo, Facebook, Instagram and TikTok. HLS and DASH downloads with a quality, audio and subtitle picker and a bundled FFmpeg came in 0.5.
- **M3:** checksum verification, mirrors and multi-source downloads, import/export, site grabber.
- **M4:** FTP/SFTP, a Chromium extension, Flatpak/AUR packaging (Flatpak Firefox needs another native-host path), autostart.
