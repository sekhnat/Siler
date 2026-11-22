# Publishing the Chromium extension in the Chrome Web Store

The release workflow attaches `siler-chromium-<version>.zip` to each release (or build it with
`node scripts/build-extension.mjs chromium`). Publishing needs a Chrome Web Store developer
account (a one-time US$5 registration) at <https://chrome.google.com/webstore/devconsole>.

## First upload

1. **New item** → upload the zip.
2. **Store listing**
   - Name and summary come from the extension (`extName`, `extDescription` in `_locales/en/messages.json`).
   - Description: what Siler is (a download manager for Linux that this extension hands downloads and videos to), that the Siler app is required, and where to get it (the GitHub releases page).
   - Category: *Productivity* (or *Tools*). Language: English.
   - Icon: `extension/icons/icon-128.png`. Screenshots: 1280×800 or 640×400; the popup and the Download button over a video (make them with demo data, never real browsing).
3. **Privacy**
   - Single purpose: “Hands the browser's downloads and the videos on web pages to the Siler download manager installed on this computer.”
   - Permission justifications:
     - `downloads`: to take over a download the browser starts, and to let it continue in the browser when Siler can't be reached.
     - `cookies`: a download (or video) often needs the site's login cookies; they are passed only to the local Siler app, for that download.
     - `nativeMessaging`: to talk to the Siler app on this computer, and to hear when it needs a fresh link for a download whose link expired.
     - `webRequest`: to recognise downloads and video streams from their responses, the form data a download was requested with, and the address it was first requested from (before redirects), which Siler asks again when a link expires.
     - `contextMenus`: “Download with Siler”, “Download this video with Siler”, “Download all links with Siler”.
     - `notifications`: to say when a download stays in the browser, and why.
     - `storage`: the extension's settings, the videos found on each tab, and a random ID that tells Siler which browser a download came from.
     - `scripting`: to read the video information a site's page carries, to run YouTube's own player code in the YouTube page to finish its video links, and, when a download's link has expired, to read the links on its page in a muted background tab the extension opens and then closes. Nothing on the page is clicked or submitted.
     - Host permission `<all_urls>`: downloads and videos can be on any site.
     - Remote code: none. Everything the extension runs is in the package (the YouTube challenge solver is vendored from yt-dlp, unmodified).
   - Data usage: the extension handles “Website content”, “Authentication information” (cookies) and “Web history” (the address of the page a download came from) only to hand them to the local Siler app; nothing is sent to the developer or third parties, nothing is sold, nothing is used for anything unrelated. Tick the three certifications.
   - Privacy policy URL: a page saying the above (for example a section of the README).
4. **Distribution:** public, all regions.
5. Submit for review.

## After it's published

The store gives the extension its own ID, different from the unpacked build's. Add it to
`CHROMIUM_IDS` in `app/src-tauri/src/integration.rs` (so the browser host accepts it), and
make Settings → Browser offer **Get from Chrome Web Store** with the store link; then release
Siler (a 0.9.x is enough). Until users have that Siler, the store extension can't reach it.

Each new version: raise `version` in `extension/manifest.json`, release, and upload the new zip
under **Package → Upload new package**.
