# Using Siler

## Adding downloads

- **Add URL** (Ctrl+N) opens the Add window. Pasting links anywhere in the window (Ctrl+V) or dropping them on it opens them too, one link or many.
- **From the browser:** the [browser extension](browser-extension.md) (Firefox or Chromium) hands downloads and videos over by itself.
- **FTP, FTPS and SFTP:** `ftp://`, `ftps://` (explicit TLS) and `sftp://` links work like web links: many connections, pause and resume, mirrors. FTP logs in as `anonymous` unless the link or a saved login says otherwise. SFTP logs in with the password you give, the desktop's SSH agent, or your `~/.ssh/id_*` keys without a passphrase. `sftp://host/~/file` is relative to your home folder. Folders can't be downloaded yet.
- **Clipboard monitoring** (Settings → General, off by default): copying a link to a file with a known extension offers it in the Add window. Works on KDE/wlroots Wayland (data-control protocol) and X11.
- **Numbered batches** (Ctrl+B): `img[001-120].jpg`, `[a-z]`, `[0-100:5]` and `{jpg,png}`, reviewed before anything downloads.
- **Import** (More menu → **Import list…**): plain lists of links, Siler JSON (names, folders, queues, checksums, mirrors), metalinks and IDM's `.ef2` export. **Export list…** writes the same formats; cookies are exported only when you ask.
- **Site grabber** (More menu): follows a site's pages to a chosen depth (this server, the whole site or one folder; robots.txt respected unless turned off) and finds the files that match a filter of types, extensions and sizes. It can also save the pages themselves, with their pictures, styles and scripts, for offline browsing, links rewritten to the saved copies. Projects can be saved, find only new files, and run on a schedule.

## Command line

```
siler [OPTIONS] [URL…]
```

Adds links to a running Siler, or starts it. The `.desktop` entry passes links dropped on the launcher the same way.

| Option | What it does |
| --- | --- |
| `--background` | Start without showing the window (tray only). |
| `--silent` | Add the links without the Add window. |
| `--queue[=NAME]` | Add the links to a queue without starting them ("Download later"). Without a name, the main queue. |
| `--start-queue[=NAME]` | Start the downloads waiting in a queue. Without a name, the main queue. |
| `--dir PATH` | Save the links in this folder. |
| `--filename NAME` | Save the link under this name (exactly one link). |
| `--import FILE` | Put the downloads listed in a file (a list of links, a metalink, Siler JSON or IDM's `.ef2`) into the Add window. A `.meta4` or `.metalink` file can also be given on its own. |
| `--install-integration` | Register the menu entry, the icons and the browser host, then exit. |
| `-h`, `--help` | Show the help. |
| `-V`, `--version` | Show the version. |

Queue names are only taken in the `=` form (`--queue=Night`), in any case. An unknown name is refused.

```sh
# Add an ISO to the "Night" queue, without the Add window
siler --queue=Night --silent --dir ~/ISOs https://example.com/a.iso

# Later, start that queue
siler --start-queue=Night

# Download a list of links
siler --import links.txt
```

## Queues and scheduling

- Named queues, each with its own limit on downloads at a time. Downloads can be reordered, started now, or moved between queues.
- A queue can start (and optionally stop) at set times on chosen days of the week, or once. A queue whose time has come starts when Siler starts.
- When a queue (or everything) has finished, Siler can quit, or put the computer to sleep, hibernate or shut it down after a one-minute countdown that can be cancelled.
- Scheduling runs while Siler is running, in the tray is fine. It can't wake a sleeping or switched-off computer.

## Managing downloads

- **Expired links** are refreshed by themselves when a server answers 403, 404, 410 or 401 (without asking for a login) to a download that had already started or waited in a queue, or when a stream's links stop working (Settings → Transfers → **Refresh expired links automatically**, on by default):
  1. Siler asks the address the download started from again, with its cookies, for a new redirect.
  2. If that fails and the download came from the browser extension, the same browser opens the page it came from in a muted background tab, finds the same link (or the same video) without clicking anything, hands it over and closes the tab. It never does this for private windows.
  3. If neither works, a notification and the download's **Get a fresh link** button open the page; click the link there and Siler takes it.

  Before continuing, Siler checks that the new link gives the same file (its size and samples of what is already on disk, or, for a stream, pieces fetched again) and starts over if it doesn't. A download gets two tries without progress. **Refresh link** in the right-click menu replaces a link by hand. There are also **Restart** and **Download again**.
- **Keep up to date:** a finished download can be checked every hour, day or week, and a newer version on the server replaces the file once it has downloaded.
- **Properties** renames or moves a download. Finished files can be dragged out of the list into a file manager or another app.
- **Verify checksum…** in the right-click menu checks any finished file and shows its MD5, SHA-1 and SHA-256.
- **When a file already exists:** ask, keep both, replace, or skip.
- **Categories** sort files into folders by type, with editable file types and folders.
- **Speed limits:** global and per download, including custom values. A **data quota** allows at most so many MB every so many hours; downloads wait for the next period, and a banner offers **Ignore for this period**.
- **A command after each download** (Settings → Transfers): any program, with `{path}`, `{dir}`, `{name}`, `{url}` and `{size}` filled in. Its exit code and output show with the download. **Try it on a file…** tests it first.

## The list

- **Cards or Details:** the toolbar's view button switches between cards and a **Details** table. In Details, click a column's title to sort by it (again to reverse), drag its edge to resize it, drag the title to move it, and right-click the titles to choose the columns: name, size, progress, speed, time left, status, queue, server, type, added, finished, connections.
- **Sorting** (the toolbar's sort menu) offers the same orders in both views, with **Reverse order**.
- **The toolbar** is yours: More → **Customize toolbar…** chooses its buttons and their order.
- These choices are kept with the settings.

## Appearance

- **Theme:** Settings → Appearance → System, Light or Dark.
- **Skins:** Siler, Graphite, Breeze, Ocean, Forest, Sunset and High contrast, each with light and dark colours. **Toolbar buttons** shows them as icons, icons and words, or as the skin says.
- **Your own skin:** **Export…** saves a skin as a JSON file to start from; **Import…** adds one (kept in `~/.local/share/app.siler.Siler/skins/`), and **Remove** takes it away again. A skin has a `name`, a corner `radius` (0 to 20), a `toolbar` style (`mixed`, `icons` or `text`), and `light` and `dark` colours by name (`accent`, `bg`, `surface`, `text`, `muted`, `ok`, `warn`, `err` and the others an exported skin lists), written as hex. Files are 64 KB at most, and anything else is refused, so a skin can't change more than colours.

## Keyboard

| Keys | Action |
| --- | --- |
| Ctrl+N | New download |
| Ctrl+V | Add the links on the clipboard |
| Ctrl+B | Numbered batch download |
| ↑ ↓, Page Up/Down, Home/End | Move through the list (Shift extends the selection) |
| Ctrl+A | Select all |
| Space | Pause or resume the selection |
| Enter | Open a finished file, or resume a stopped download |
| Delete | Remove the selection |
| Menu key | Right-click menu |
| Ctrl+Q | Quit |

## Network

- **Proxy** (Settings → Network):
  - **None**, **System** (environment variables, or the KDE or GNOME settings, including their proxy script or automatic detection), **Manual**, **Script** or **Detect**.
  - **Manual:** an HTTP, HTTPS, SOCKS4, SOCKS4a or SOCKS5 proxy with an optional login, and addresses that go around it (`example.com`, `*.example.com`, `10.0.0.0/8`, `<local>` for names without a dot). An optional **FTP proxy** takes `ftp://` downloads, logging in as `user@server`.
  - **Script:** the address or file of a proxy auto-config (PAC) script. Each address, and each redirect, goes where the script says, trying its proxies in order; a proxy that doesn't answer is skipped for five minutes. If the script can't be read or run, downloads stop rather than going around the proxy. Addresses on this computer always connect directly.
  - **Detect:** Siler asks the network for a script (WPAD): first the one the DHCP server named (NetworkManager keeps it), then `http://wpad.<domain>/wpad.dat` for the network's domains. It never asks under a public suffix such as `co.uk`. Without a script it connects directly. Use it only on networks you trust.
  - Scripts run in `siler-pac`, a separate helper with limits on time and memory; the script can't reach the network or your files.
  - **Test with an address** shows which proxy an address goes through, and why.
  - A login saved for the manual proxy is never sent to a proxy that a script chose.
- **Logins:** when a server or a proxy asks for a username and password, Siler asks once and can save the login in the desktop keyring. `user:password@` in a link works too. FTP and SFTP logins are kept apart from a site's web login. A login belongs to the scheme, server and port it was given for, so an https login never goes to http.
  - **Basic** and **Digest**.
  - **NTLM** (NTLMv2, with channel binding over https): enter a Windows account as `DOMAIN\user`. A refused password is tried once, not again on every connection, so a wrong one doesn't lock the account. Over plain http, the login window warns that others on the network could misuse a Windows login.
  - **Kerberos** (Negotiate): tick **Use my Kerberos ticket for this site** to use the ticket `kinit` got, or give `user@REALM` and a password. It needs the MIT Kerberos library (`libgssapi_krb5.so.2`, package `krb5` or `libkrb5-3`; the AppImage carries it). Tickets are never delegated. Without a ticket, Negotiate falls back to NTLM with the password.
  - NTLM and Kerberos have been tested against a pyspnego test server and an MIT KDC, not yet against Windows servers or Active Directory.
- **SFTP server keys:** the first connection to an SFTP server shows its key's fingerprint and asks whether to trust it (as `ssh` does). Trusted keys are kept in Siler's own `known_hosts` (next to its database); `~/.ssh/known_hosts` is read too. A key that changed is refused.
- **Proxies for FTP and SFTP:** they go where the proxy settings (or the script) say: through a SOCKS proxy, or an HTTP or HTTPS proxy with `CONNECT`.
- **Connections:** connections per download, simultaneous downloads and a per-server connection limit are in Settings → Transfers.

## Desktop

- **Start when you log in** (Settings → General) starts Siler in the tray with your session, through an entry in `~/.config/autostart/`. Without a tray, the window opens instead. Your desktop's own autostart settings can turn it off too.
- **The tray** (KDE/SNI): a click on the icon opens a panel beside it with each running download's progress, speed and time left, buttons to pause, resume, finish or cancel, the waiting downloads, the ones that need you (a login, a fresh link), and the last three finished. Its footer has **Pause all**, a speed **Limit** and **Open Siler**. A click outside it, Esc or a second click on the icon closes it.
  - The panel works on Wayland desktops with layer-shell (KDE Plasma, and wlroots-based ones such as Sway; it needs gtk-layer-shell, which the AppImage carries) and on X11. Elsewhere, such as GNOME on Wayland, a click opens Siler instead.
  - Settings → General → **Click the tray icon for the downloads panel** turns it off; a middle click always opens Siler.
  - The icon fills a ring as downloads progress (in the skin's accent), with a red dot while recording and an amber one when a download needs you. Its tooltip and right-click menu list the running downloads, with **Pause**, **Finish** and **Open folder** for each.
- Clickable notifications. Closing the window keeps Siler running in the tray when there is one.
- Optional sounds for a finished or failed download and a finished queue, from the desktop's sound theme or chosen files.
- Siler keeps the computer awake while downloads run (logind sleep inhibitor; can be turned off).
- A confirmation before quitting while downloads run, and a remembered window size.
- **Updates:** once a day (Settings → General can turn it off, or **Check now**), Siler asks GitHub whether a newer version is out; nothing else is sent. An AppImage offers **Update and restart**: the new AppImage is downloaded through Siler's own connection settings, checked against the release's SHA-256, put in place of the old one (kept as `.old` until the next start) and started. A package (AUR) is updated by your package manager.
- **Language:** Settings → General → Language; by default Siler follows the system. Siler is in English for now; see [Translating Siler](translating.md).

## Logs and data

- **Logs** are written to `~/.local/state/siler/logs/`: `siler.<date>.log` (kept for a week), `nmhost.log`, and `siler-stderr.log` when the browser started Siler. Settings → General → **Open log folder** opens the folder.
- **Data** is stored in `~/.local/share/app.siler.Siler/siler.db`. See [Architecture → Files on disk](architecture.md#files-on-disk).

## Troubleshooting

- **Siler can't start** (e.g. the database can't be opened): the window shows the error instead of the app disappearing. The log folder has the details.
- **The extension says Siler isn't installed:** run `siler --install-integration` again, or Settings → Browser → **Install**. After moving or replacing the AppImage, start it once so the browser host points at the new file.
- **The extension or the app is out of date:** the popup, the options page and Siler's settings say which one to update.
