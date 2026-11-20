// Shared between the background script, popup and options page.
"use strict";

const KSTAR_HOST = "com.kstar.nmhost";
// Must match kstar_ipc::PROTOCOL in the app.
const KSTAR_PROTOCOL = 2;

const KSTAR_DEFAULTS = {
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
  // Show a notification when a download can't be handed to K-Star.
  notifyFailures: true,
  // Look for videos on pages (listed in the popup, counted on the toolbar button).
  findVideos: true,
  // Show a Download button over videos on pages.
  videoButton: true,
  // Site recognisers, each on or off.
  site_youtube: true,
  site_vimeo: true,
  site_facebook: true,
  site_instagram: true,
  site_tiktok: true,
};

async function kstarConfig() {
  const stored = await browser.storage.local.get(KSTAR_DEFAULTS);
  return { ...KSTAR_DEFAULTS, ...stored };
}

function kstarSend(message) {
  return browser.runtime.sendNativeMessage(KSTAR_HOST, message);
}

/**
 * Health of the integration:
 *  - installed: the native host answered (it can start K-Star on demand)
 *  - running:   K-Star itself answered
 */
async function kstarHealth() {
  try {
    const reply = await kstarSend({
      type: "ping",
      extension_version: browser.runtime.getManifest().version,
      protocol: KSTAR_PROTOCOL,
    });
    const running = reply?.type === "pong";
    const protocol = running ? (reply.protocol ?? 0) : null;
    // Which side is out of date, if they no longer speak the same protocol.
    const outdated = protocol == null || protocol === KSTAR_PROTOCOL ? null : protocol < KSTAR_PROTOCOL ? "app" : "extension";
    return { installed: true, running, version: reply?.version ?? null, protocol, outdated };
  } catch (e) {
    return { installed: false, running: false, outdated: null, error: String(e?.message ?? e) };
  }
}

function kstarHostOf(url) {
  try {
    return new URL(url).hostname.toLowerCase();
  } catch {
    return "";
  }
}

function kstarBlockedHosts(cfg) {
  return cfg.blocklist.split(/\s+/).filter(Boolean);
}

/** Whether a host is on the "never capture" list (subdomains included). */
function kstarSiteBlocked(cfg, host) {
  return kstarBlockedHosts(cfg).some((b) => host === b || host.endsWith("." + b));
}

/** Adds a host to the "never capture" list, or removes it (and its parent entries). */
async function kstarSetSiteBlocked(host, blocked) {
  const cfg = await kstarConfig();
  let list = kstarBlockedHosts(cfg);
  if (blocked) {
    if (!kstarSiteBlocked(cfg, host)) list.push(host);
  } else {
    list = list.filter((b) => !(host === b || host.endsWith("." + b)));
  }
  await browser.storage.local.set({ blocklist: list.join(" ") });
}
