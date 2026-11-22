# Browser extension

**Siler Integration** sends downloads and the videos web pages play from your browser to Siler. One extension, built for two families: Firefox and the browsers built on it (LibreWolf, Waterfox, Floorp, Zen), and the Chromium ones (Chrome, Chromium, Brave, Vivaldi, Edge, Opera).

<p align="center">
  <picture><source media="(prefers-color-scheme: dark)" srcset="images/extension-popup-dark.png"><source media="(prefers-color-scheme: light)" srcset="images/extension-popup-light.png"><img src="images/extension-popup-light.png" alt="The extension's toolbar popup: connected to Siler, with active downloads, speed and the capture switch" width="320"></picture>
</p>

## Installing

1. **Register the browser host.** Run `siler --install-integration` once, or open Siler → Settings → Browser → **Install**. The AppImage and the release tarball include the host; from a source checkout, `./scripts/install-dev.sh` builds and registers it (it needs `siler-nmhost` next to the `siler` binary).
   - The host manifest goes to `~/.mozilla/native-messaging-hosts/`, which Firefox, Zen and Floorp read. A copy goes to `~/.librewolf/`, `~/.waterfox/`, `~/.floorp/` and `~/.zen/` when those browsers are installed.
   - For the Chromium browsers it goes to `~/.config/<browser>/NativeMessagingHosts/` (google-chrome, google-chrome-beta, google-chrome-unstable, chromium, BraveSoftware/Brave-Browser, vivaldi, microsoft-edge, opera), when the browser is installed. Install the browser first, or click **Install** again afterwards.
   - The AUR package installs the manifests system-wide instead (`/usr/lib/mozilla/native-messaging-hosts/`, `/etc/chromium/…`, `/etc/opt/chrome/…`, `/etc/opt/edge/…`).
   - When Siler runs as an AppImage, the host is copied to `~/.local/share/siler/bin/`, because the AppImage's own files disappear when it closes. Siler updates that copy and the AppImage path on every start.
2. **Install the extension.** Open Siler → Settings → Browser and click **Add to Firefox**. After an update, reload pages that were already open (or restart Firefox): they keep the previous version's Download button until then. The AppImage and the release tarball include the extension, signed by Mozilla; Siler hands Firefox a copy and Firefox asks to add it. Each [release](https://github.com/sekhnat/Siler/releases/latest) also has `siler-integration-<version>.xpi` to open with Firefox directly. A build you signed yourself (below) takes precedence.
3. **Grant access to all sites** on the welcome page. Firefox treats MV3 host permissions as optional.

### Chrome, Chromium, Brave, Vivaldi, Edge, Opera

1. Register the browser host as above.
2. In Siler, open Settings → Browser and click **Set up** under the Chromium browsers. Siler copies the extension to `~/.local/share/siler/chromium-extension/` (it must stay put: the browser loads it from there every time).
3. Open the browser's extensions page (Siler offers **Open in Chrome** and the like), turn on **Developer mode**, click **Load unpacked** and choose that folder.

The extension isn't in the Chrome Web Store yet ([docs/chrome-web-store.md](chrome-web-store.md) has the submission checklist). An unpacked extension keeps the same ID (`legjbaodpdlommbgpdfcmogjedfgpjae`, pinned by the `key` in `manifest.chromium.json`), which is the one the host manifest allows.

How it differs from Firefox: Chromium extensions can't hold a response at its headers, so a download is taken when the browser starts it (`downloads.onCreated`): it is cancelled and handed to Siler, and the browser may show it for a moment. Form downloads (POST) are sent again with their form data. Stream manifests are read with a request of their own (with the page's cookies), and site answers (Facebook, Instagram, TikTok, Vimeo) are copied from the page by a small script that runs only on those sites.

### Building

`node scripts/build-extension.mjs firefox` and `… chromium` write `dist/extension-<browser>/` and a zip. The source is one folder (`extension/`); `manifest.chromium.json` is merged over `manifest.json` for Chromium, whose background is a service worker (`sw.js`) loading the same scripts.

### Signing your own build

Release builds of Firefox only keep signed extensions installed. To sign a changed extension:

1. Create an API key at <https://addons.mozilla.org/developers/addon/api/key/>.
2. Put it in `~/.config/siler/amo.env` (`AMO_JWT_ISSUER=…`, `AMO_JWT_SECRET=…`), then `chmod 600` it.
3. Raise `version` in `extension/manifest.json`: every signing needs a new version.
4. Run `./scripts/sign-extension.sh`. It builds the Firefox extension and uploads it for an **unlisted** signature: Mozilla signs it but doesn't publish it.
5. Click Siler → Settings → Browser → **Add to Firefox**.

For a quick test instead, open `about:debugging#/runtime/this-firefox`, choose **Load Temporary Add-on** and pick `extension/manifest.json`. It is removed when Firefox restarts. `npx web-ext run -s extension` also works, except on distributions that lock `devtools.debugger.remote-enabled`, such as CachyOS.

## Capturing downloads

- **Before Firefox sees it:** downloads are recognised from the response headers and cancelled before Firefox creates a download, so its dialog and download panel never appear.
- **Which downloads:** a download is handed over when its extension or MIME type matches the rules on the options page. "Capture everything" ignores the rules.
- **Bypassing:** hold **Alt** while clicking a link to let Firefox download it instead.
- **Cookies and context:** each download goes over with its cookies, referrer and user agent, so links that need a login keep working, including on resume.
- **Forms:** POST downloads are taken too. The form body (up to 1 MiB, file uploads excluded) is replayed for every connection.
- **New tabs:** a tab that was opened only for the download is closed once Siler has it.
- **Per site:** the toolbar popup has a "Capture on this site" switch.
- **Fallback:** if Siler can't be reached, the download stays in Firefox. If the hand-over fails, Firefox downloads it again itself. Either way a notification says why; this can be turned off in the options.
- **Download all links:** sends every link on the page or in the selection to Siler. Links that look like files are pre-selected, and the batch window can filter by type or name.
- **"Always ask where to save files":** if this Firefox setting is on, the hand-over happens after the save dialog.
- **Versions:** the extension and the app exchange a protocol version. If either is out of date, the popup, the options page and Siler's settings say which one to update.

## Videos on web pages

- **Finding them:** the extension watches what a page's player loads. HLS and DASH manifests (by address or content type) and whole video or audio files (512 KB and up; pieces of streams are skipped) are listed per tab, with their number on the toolbar button. An HLS master playlist is listed once, without the playlists it names.
- **Download button:** pointing at a video (240×135 or larger) shows a **Download** button at its top right. It runs in the extension's own world, in a closed shadow root, so pages can't style or script it. The popup's "Videos on this page" list and the right-click menu's **Download this video with Siler** do the same.
- **Feeds:** on pages with many videos, such as Instagram's home feed, the button and the right-click menu find the clicked video by the post it sits in: the post's permalink, its poster, and the video's shape (and its length, when the site's data gives one). If Siler can't tell which video was clicked, it says so instead of sending another one.
- **Scrolling viewers:** Instagram's Reels, Facebook and TikTok change the page's address in place as you scroll from video to video. The videos the page fetched ahead stay listed until a real page load, so the button sends the video the address names. YouTube's Shorts feed also switches videos in place; the extension reads the title from the Shorts player, so files are named after the Short.
- **Hand-over:** the video goes to Siler's Add window with the page's cookies, referrer and user agent. A stream opens the quality, audio and subtitle picker; a file opens the usual form. The window says which site it came from.
- **Names:** videos are named by their title (slashes become "⧸", as yt-dlp writes them).

### Site recognisers

Each can be switched off in the options. They are ported from yt-dlp's extractors, which read the same data.

| Site | What Siler gets |
| --- | --- |
| **YouTube** | The extension asks YouTube's player API from the page, as the clients yt-dlp uses by default without a PO token (`tv_samsung`, then `visionos`). It finishes the signed links with YouTube's own player code, run by yt-dlp's [ejs](https://github.com/yt-dlp/ejs) solver inside a blank frame of the page. Before handing over, it checks that one link answers and falls back to the next client. Siler receives a DASH manifest of every format (up to 4K, H.264/VP9/AV1, AAC/Opus) with captions as WebVTT. It fetches the files in 10 MiB ranges, as yt-dlp does, and waits out a pre-roll ad when YouTube asks for that. |
| **Vimeo** | The player config's HLS stream, with its subtitle files. Videos whose owners turned on DRM are refused, like any DRM stream. |
| **Facebook**, **Instagram** | The DASH manifest the page's data carries (read from its embedded JSON and its GraphQL answers), or the plain video file when there is none. |
| **TikTok** | The best H.264 file of the video, with the page's cookies and referrer, which TikTok's servers require. |

The YouTube solver and its parser libraries are vendored unmodified in `extension/media/youtube/vendor/` by `scripts/vendor-extension-libs.sh`; `SOURCES.md` there lists versions, checksums and licenses.

### Limits

- DRM-protected videos are refused. Live streams aren't recorded.
- Links from a page expire (YouTube's after a few hours). Siler asks the browser for fresh ones by itself (see [Fresh links](#fresh-links)); when that isn't possible, the download says so and the video has to be sent from the page again.
- In an Instagram carousel with several videos of the same shape, the button takes the first of them; the popup's "Videos on this page" list has all of them, under the post's title.
- Content that needs a login works only when you are logged in in Firefox.
- Sites change their players; recogniser fixes come with extension updates.

## Fresh links

When a download's link expires, Siler first asks the original address again by itself. If that doesn't help, it asks the browser the download came from:

- **What is remembered:** with each download, the extension sends the first address of its redirect chain (with its cookies), the browser it came from (a random ID kept in the extension's storage), the Firefox container, the page, and how to find the link again (its address without the query, its text and the file name), or the video's ID and format on a recognised site. Siler keeps these with the download and never exports them.
- **The channel:** the extension keeps a connection open to the browser host (`connectNative`), which waits for Siler but never starts it. Older apps and hosts simply don't answer, and the extension asks again every five minutes.
- **Finding the link:** the browser opens the page in a background tab, inactive and muted, looks for exactly one matching link (or the same video, through the site recognisers), and hands Siler the fresh link and cookies. Then it closes the tab it opened. It never clicks, never submits a form, opens one tab per site at a time, gives up after a minute, and does nothing for private windows.
- **Siler checks the result:** the new link must give the same file before the download continues (see [Using Siler → Managing downloads](usage.md#managing-downloads)).
- **Switching it off:** the options page (**Help Siler refresh expired links**) or Siler's Settings → Transfers → **Refresh expired links automatically**.

## Notes for reviewers and developers

- **Mozilla review:** the YouTube solver compiles YouTube's player script with `Function`. `web-ext lint` reports this as `DANGEROUS_EVAL`, a warning that doesn't stop unlisted signing.
- **Tests:** `node --test extension/tests/*.test.mjs` checks the request rules and each recogniser against recorded, scrubbed answers in `extension/tests/fixtures/`. The app's DASH parser reads the same YouTube manifest snapshot in its own tests.
- **Protocol 2:** the extension 0.3.0 and the app 0.6.0 (and later) speak protocol 2, which adds video details to a hand-over. An older app still takes plain downloads. The extension 0.5.0 and the app 0.9.0 add the messages for fresh links to it; they are additive, so either side still works with an older other side, without fresh links.
