// Shared between the background script, popup and options page.
"use strict";

const SILER_HOST = "com.siler.nmhost";
// Must match siler_ipc::PROTOCOL in the app.
const SILER_PROTOCOL = 2;

const SILER_DEFAULTS = {
  enabled: true,
  // Capture every download, ignoring the type rules below.
  captureAll: false,
  extensions:
    "zip rar 7z gz tgz bz2 xz zst tar iso img dmg exe msi deb rpm appimage flatpak apk jar " +
    "mp4 mkv webm avi mov wmv flv m4v mp3 flac m4a aac ogg opus wav pdf epub djvu bin run",
  // Also capture by MIME type (video/*, audio/*, archives, octet-stream).
  captureByMime: true,
  // Skip files smaller than this (MB, 0 = no minimum). Only applied when the size is known.
  minSizeMB: 0,
  // Hosts never captured, one per line or space-separated.
  blocklist: "",
  // Show a notification when a download can't be handed to Siler.
  notifyFailures: true,
  // Look for videos on pages (listed in the popup, counted on the toolbar button).
  findVideos: true,
  // Look at a page in a background tab when Siler asks for a fresh link.
  refreshLinks: true,
  // Show a Download button over videos on pages.
  videoButton: true,
  // Site recognisers, each on or off.
  site_youtube: true,
  site_vimeo: true,
  site_facebook: true,
  site_instagram: true,
  site_tiktok: true,
};

async function silerConfig() {
  const stored = await browser.storage.local.get(SILER_DEFAULTS);
  return { ...SILER_DEFAULTS, ...stored };
}

function silerSend(message) {
  return browser.runtime.sendNativeMessage(SILER_HOST, message);
}

/**
 * Health of the integration:
 *  - installed: the native host answered (it can start Siler on demand)
 *  - running:   Siler itself answered
 */
async function silerHealth() {
  try {
    const reply = await silerSend({
      type: "ping",
      extension_version: browser.runtime.getManifest().version,
      protocol: SILER_PROTOCOL,
    });
    const running = reply?.type === "pong";
    const protocol = running ? (reply.protocol ?? 0) : null;
    // Which side is out of date, if they no longer speak the same protocol.
    const outdated = protocol == null || protocol === SILER_PROTOCOL ? null : protocol < SILER_PROTOCOL ? "app" : "extension";
    return { installed: true, running, version: reply?.version ?? null, protocol, outdated };
  } catch (e) {
    return { installed: false, running: false, outdated: null, error: String(e?.message ?? e) };
  }
}

function silerHostOf(url) {
  try {
    return new URL(url).hostname.toLowerCase();
  } catch {
    return "";
  }
}

function silerBlockedHosts(cfg) {
  return cfg.blocklist.split(/\s+/).filter(Boolean);
}

/** Whether a host is on the "never capture" list (subdomains included). */
function silerSiteBlocked(cfg, host) {
  return silerBlockedHosts(cfg).some((b) => host === b || host.endsWith("." + b));
}

/** Adds a host to the "never capture" list, or removes it (and its parent entries). */
async function silerSetSiteBlocked(host, blocked) {
  const cfg = await silerConfig();
  let list = silerBlockedHosts(cfg);
  if (blocked) {
    if (!silerSiteBlocked(cfg, host)) list.push(host);
  } else {
    list = list.filter((b) => !(host === b || host.endsWith("." + b)));
  }
  await browser.storage.local.set({ blocklist: list.join(" ") });
}
