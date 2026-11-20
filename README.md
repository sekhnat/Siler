# K-Star

A Linux-first download manager with a Firefox extension. The long-term target is feature parity with Internet Download Manager.

**Milestone 1 (this code)** includes:
- Segmented, multi-connection HTTP(S) downloads with dynamic splitting. The connection count can be changed while a download runs.
- Pause and resume, including after a restart or crash. Passing network failures are retried, honouring the server's `Retry-After`.
- A persistent queue: "Download later", Start/Stop queue, reordering, and Start now.
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
  - lets you review "Download all links" in K-Star.

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
  - `--queue` adds the links as "Download later"; `--start-queue` starts everything waiting in the queue.
  - Example: `kstar --queue --silent --dir ~/ISOs https://example.com/a.iso`, and later `kstar --start-queue`.
- **Logs:** written to `~/.local/state/kstar/logs/`, as `kstar.<date>.log` (kept for a week), `nmhost.log`, and `kstar-stderr.log` when the browser started K-Star. Settings → General → **Open log folder** opens the folder.
- **Data:** stored in `~/.local/share/app.kstar.KStar/kstar.db`. The schema migrates on start (v1 → v2 adds the queue position and conflict policy).
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

## Roadmap
- **M2:** video grabber (yt-dlp/ffmpeg) and HLS/DASH, clipboard monitoring, scheduler and named queues, post-download actions.
- **M3:** checksum verification, mirrors and multi-source downloads, import/export, site grabber.
- **M4:** FTP/SFTP, a Chromium extension, Flatpak/AUR packaging (Flatpak Firefox needs another native-host path), autostart.
