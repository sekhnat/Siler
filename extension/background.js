// K-Star browser integration: captures downloads and hands them to the K-Star app.
"use strict";

const HEALTH_TTL = 30_000;
let health = { installed: false, running: false, outdated: null, checkedAt: 0 };
let bypassUntil = 0;
// URLs we re-issued to Firefox ourselves after a failed hand-over; don't capture them again.
const passThrough = new Set();

async function refreshHealth(force = false) {
  if (!force && Date.now() - health.checkedAt < HEALTH_TTL) return health;
  health = { ...(await kstarHealth()), checkedAt: Date.now() };
  const problem = !health.installed || health.outdated;
  await browser.action.setBadgeText({ text: problem ? "!" : "" });
  await browser.action.setBadgeBackgroundColor({ color: health.outdated ? "#d97706" : "#dc2626" });
  return health;
}

// ---------------------------------------------------------------- telling the user

const UNREACHABLE_EVERY = 10 * 60_000;
let unreachableShownAt = 0;

async function notify(title, message) {
  const cfg = await kstarConfig();
  if (!cfg.notifyFailures) return;
  browser.notifications
    .create({ type: "basic", iconUrl: browser.runtime.getURL("icons/icon-96.png"), title, message })
    .catch(() => {});
}

/** Remembers the last problem for the popup, and shows it as a notification. */
async function reportFailure(file, reason) {
  const message = `K-Star couldn't take ${file ? `“${file}”` : "the download"}: ${reason}. Firefox is downloading it instead.`;
  await browser.storage.session.set({ lastError: { at: Date.now(), message } }).catch(() => {});
  notify("Download not handed to K-Star", message);
}

/** A download would have gone to K-Star, but K-Star can't be reached. Said at most every 10 min. */
function reportUnreachable() {
  if (Date.now() - unreachableShownAt < UNREACHABLE_EVERY) return;
  unreachableShownAt = Date.now();
  const message = "Firefox handles downloads itself until K-Star is set up: open K-Star → Settings → Browser → Install.";
  browser.storage.session.set({ lastError: { at: Date.now(), message: "K-Star isn't reachable. " + message } }).catch(() => {});
  notify("K-Star isn't reachable", message);
}

const reasonOf = (e) => String(e?.message ?? e).replace(/^Error:\s*/, "");

// ---------------------------------------------------------------- capture rules

function extensionOf(name) {
  const base = (name || "").split(/[\\/]/).pop().split(/[?#]/)[0].toLowerCase();
  const m = base.match(/\.(tar\.(?:gz|bz2|xz|zst)|[a-z0-9]{1,10})$/);
  return m ? m[1] : "";
}

function wanted(item, cfg) {
  if (!/^https?:/i.test(item.url)) return false; // blob:, data:, file: stay in Firefox
  if (kstarSiteBlocked(cfg, kstarHostOf(item.finalUrl || item.url))) return false;
  if (cfg.minSizeMB > 0 && item.totalBytes > 0 && item.totalBytes < cfg.minSizeMB * 1024 * 1024) return false;
  if (cfg.captureAll) return true;

  const exts = new Set(cfg.extensions.toLowerCase().split(/[\s,]+/).filter(Boolean));
  const ext = extensionOf(item.filename) || extensionOf(item.finalUrl || item.url);
  if (ext && (exts.has(ext) || exts.has(ext.split(".").pop()))) return true;

  if (cfg.captureByMime && item.mime) {
    const mime = item.mime.toLowerCase();
    if (mime.startsWith("video/") || mime.startsWith("audio/")) return true;
    if (/^application\/(octet-stream|zip|x-(7z|rar|tar|xz|bzip2|gzip|iso9660|msdownload|debian|rpm|apple-diskimage)|vnd\.(rar|android))/.test(mime))
      return true;
  }
  return false;
}

// ---------------------------------------------------------------- hand-over

async function cookieHeader(url, storeId) {
  const query = { url };
  if (storeId) query.storeId = storeId;
  let cookies;
  try {
    cookies = await browser.cookies.getAll(query);
  } catch {
    // First-party isolation requires the firstPartyDomain key.
    try {
      cookies = await browser.cookies.getAll({ ...query, firstPartyDomain: null });
    } catch {
      return null;
    }
  }
  if (!cookies.length) return null;
  return cookies.map((c) => `${c.name}=${c.value}`).join("; ");
}

function basename(path) {
  const b = (path || "").split(/[\\/]/).pop();
  return b || null;
}

async function describe(url, { referrer = null, pageUrl = null, filename = null, size = null, mime = null, storeId = null } = {}) {
  return {
    url,
    filename,
    referrer,
    page_url: pageUrl,
    cookies: await cookieHeader(url, storeId),
    user_agent: navigator.userAgent,
    headers: [],
    size,
    mime,
  };
}

async function handOver(items, silent = false) {
  const reply = await kstarSend({ type: "add_downloads", items, silent });
  if (reply?.type !== "accepted") throw new Error(reply?.message || "K-Star rejected the download");
  health = { ...health, installed: true, running: true, checkedAt: Date.now() };
  return reply;
}

// ---------------------------------------------------------------- header-level capture
//
// Downloads are recognised from the response headers and cancelled before Firefox turns
// them into a download, so no Firefox dialog or download panel ever appears.

const DOC_REQUESTS = { urls: ["<all_urls>"], types: ["main_frame", "sub_frame"] };
// Request headers (cookies, referer, user agent) exactly as Firefox sent them.
const sentHeaders = new Map();
// Bodies of form submissions (base64), so K-Star can send the same POST.
const postBodies = new Map();
const MAX_BODY = 1024 * 1024;
// Tabs opened recently that haven't shown a page yet: closed if they only carried a download.
const freshTabs = new Map();

browser.tabs.onCreated.addListener((tab) => freshTabs.set(tab.id, Date.now()));
browser.tabs.onRemoved.addListener((id) => freshTabs.delete(id));

function toBase64(bytes) {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

/** The request body as bytes, or null if it can't be replayed (file uploads, too big). */
function bodyBytes(rb) {
  if (!rb || rb.error) return null;
  if (rb.raw) {
    if (rb.raw.some((part) => !part.bytes)) return null; // a file upload
    const parts = rb.raw.map((p) => new Uint8Array(p.bytes));
    const size = parts.reduce((n, p) => n + p.length, 0);
    if (size > MAX_BODY) return null;
    const out = new Uint8Array(size);
    let at = 0;
    for (const p of parts) {
      out.set(p, at);
      at += p.length;
    }
    return out;
  }
  if (rb.formData) {
    const params = new URLSearchParams();
    for (const [k, values] of Object.entries(rb.formData)) for (const v of values) params.append(k, v);
    const bytes = new TextEncoder().encode(params.toString());
    return bytes.length <= MAX_BODY ? bytes : null;
  }
  return null;
}

browser.webRequest.onBeforeRequest.addListener(
  (d) => {
    if (d.method !== "POST") return;
    const bytes = bodyBytes(d.requestBody);
    if (bytes) postBodies.set(d.requestId, { body: toBase64(bytes), form: !!d.requestBody.formData });
  },
  DOC_REQUESTS,
  ["requestBody"],
);

function headerMap(headers) {
  const out = {};
  for (const h of headers || []) out[h.name.toLowerCase()] = h.value ?? "";
  return out;
}

function filenameFromDisposition(cd) {
  const ext = cd.match(/filename\*\s*=\s*([^']*)'[^']*'([^;]+)/i);
  if (ext) {
    try {
      return decodeURIComponent(ext[2].trim().replace(/^"|"$/g, ""));
    } catch {
      /* fall through */
    }
  }
  const plain = cd.match(/filename\s*=\s*("((?:[^"\\]|\\.)*)"|[^;]+)/i);
  if (!plain) return null;
  const name = (plain[2] ?? plain[1]).trim().replace(/\\(.)/g, "$1");
  return basename(name);
}

/** Types Firefox shows in the tab itself; those are only captured when sent as attachments. */
function displaysInline(mime, type) {
  if (!mime) return true; // unknown: let Firefox decide
  if (/^(text\/|image\/)/.test(mime)) return true;
  if (/^application\/(xhtml\+xml|xml|json|pdf|javascript|ecmascript|x-javascript|rss\+xml|atom\+xml)$/.test(mime)) return true;
  // Media embedded in a frame is playback, not a download.
  if (type === "sub_frame" && /^(video|audio)\//.test(mime)) return true;
  return false;
}

browser.webRequest.onSendHeaders.addListener(
  (d) => {
    const h = headerMap(d.requestHeaders);
    sentHeaders.set(d.requestId, {
      cookie: h.cookie || null,
      referer: h.referer || null,
      ua: h["user-agent"] || null,
      contentType: h["content-type"] || null,
    });
  },
  DOC_REQUESTS,
  ["requestHeaders"],
);
const forgetRequest = (d) => {
  sentHeaders.delete(d.requestId);
  postBodies.delete(d.requestId);
};
browser.webRequest.onCompleted.addListener((d) => {
  forgetRequest(d);
  if (d.type === "main_frame") freshTabs.delete(d.tabId); // it shows a page now
}, DOC_REQUESTS);
browser.webRequest.onErrorOccurred.addListener(forgetRequest, DOC_REQUESTS);

/** Closes a tab that was opened only to start a download that K-Star took over. */
function closeIfBlank(tabId, type) {
  const created = freshTabs.get(tabId);
  if (type !== "main_frame" || created == null || Date.now() - created > 30_000) return;
  freshTabs.delete(tabId);
  browser.tabs.remove(tabId).catch(() => {});
}

browser.webRequest.onHeadersReceived.addListener(
  async (d) => {
    const sent = sentHeaders.get(d.requestId);
    const post = postBodies.get(d.requestId);
    forgetRequest(d);
    // Form submissions are taken only when their body can be sent again.
    if (!(d.method === "GET" || (d.method === "POST" && post))) return {};
    if (d.statusCode < 200 || d.statusCode >= 300) return {};
    if (d.tabId < 0 || passThrough.has(d.url) || Date.now() < bypassUntil) return {};

    const h = headerMap(d.responseHeaders);
    const disposition = h["content-disposition"] || "";
    const attachment = /^\s*attachment/i.test(disposition);
    const mime = (h["content-type"] || "").split(";")[0].trim().toLowerCase();
    if (!attachment && displaysInline(mime, d.type)) return {};

    const cfg = await kstarConfig();
    if (!cfg.enabled) return {};
    const size = Number(h["content-length"]);
    const filename = filenameFromDisposition(disposition);
    const candidate = { url: d.url, filename: filename || "", mime, totalBytes: size > 0 ? size : -1 };
    if (!wanted(candidate, cfg)) return {};
    if (!(await refreshHealth()).installed) {
      reportUnreachable();
      return {};
    }

    const item = {
      url: d.url,
      filename,
      referrer: sent?.referer || d.originUrl || null,
      page_url: d.documentUrl || d.originUrl || null,
      cookies: sent?.cookie ?? (await cookieHeader(d.url, d.cookieStoreId)),
      user_agent: sent?.ua || navigator.userAgent,
      headers: [],
      size: size > 0 ? size : null,
      mime: mime || null,
    };
    if (d.method === "POST") {
      item.method = "POST";
      item.body = post.body;
      const ct = sent?.contentType || (post.form ? "application/x-www-form-urlencoded" : null);
      if (ct) item.headers = [["Content-Type", ct]];
    }
    handOver([item])
      .then(() => closeIfBlank(d.tabId, d.type))
      .catch((e) => {
        console.warn("K-Star hand-over failed; downloading in Firefox instead", e);
        health.checkedAt = 0;
        reportFailure(filename || basename(new URL(d.url).pathname), reasonOf(e));
        passThrough.add(d.url);
        const again = { url: d.url, cookieStoreId: d.cookieStoreId };
        if (d.method === "POST") {
          again.method = "POST";
          again.body = new TextDecoder().decode(Uint8Array.from(atob(post.body), (c) => c.charCodeAt(0)));
          if (item.headers.length) again.headers = item.headers.map(([name, value]) => ({ name, value }));
        }
        browser.downloads.download(again).catch(() => passThrough.delete(d.url));
      });
    return { cancel: true };
  },
  DOC_REQUESTS,
  ["blocking", "responseHeaders"],
);

// Fallback for downloads the header check can't see (e.g. <a download>, blob URLs from
// scripts are excluded by `wanted`). Firefox may already have shown its UI for these.
browser.downloads.onCreated.addListener(async (item) => {
  if (passThrough.delete(item.url)) return;
  if (Date.now() < bypassUntil) return;
  if (item.state && item.state !== "in_progress") return;

  const cfg = await kstarConfig();
  if (!cfg.enabled || !wanted(item, cfg)) return;
  // Never take a download away from Firefox unless K-Star can be reached.
  if (!(await refreshHealth()).installed) {
    reportUnreachable();
    return;
  }

  try {
    await browser.downloads.cancel(item.id);
    await browser.downloads.erase({ id: item.id });
  } catch {
    // Already finished or gone; nothing to hand over.
    return;
  }

  try {
    const desc = await describe(item.finalUrl || item.url, {
      referrer: item.referrer || null,
      filename: basename(item.filename),
      size: item.totalBytes > 0 ? item.totalBytes : null,
      mime: item.mime || null,
      storeId: item.cookieStoreId,
    });
    await handOver([desc]);
  } catch (e) {
    console.warn("K-Star hand-over failed; downloading in Firefox instead", e);
    health.checkedAt = 0;
    reportFailure(basename(item.filename), reasonOf(e));
    passThrough.add(item.url);
    browser.downloads.download({ url: item.url, cookieStoreId: item.cookieStoreId }).catch(() => passThrough.delete(item.url));
  }
});

// ---------------------------------------------------------------- context menus

function createMenus() {
  browser.contextMenus.removeAll().then(() => {
    browser.contextMenus.create({
      id: "kstar-link",
      title: "Download with K-Star",
      contexts: ["link", "image"],
    });
    browser.contextMenus.create({
      id: "kstar-video",
      title: "Download this video with K-Star",
      contexts: ["video", "audio"],
    });
    browser.contextMenus.create({
      id: "kstar-all",
      title: "Download all links with K-Star",
      contexts: ["page", "selection"],
    });
  });
}

// Runs in the page: collects links from the selection, or from the whole page.
function collectLinks() {
  const sel = window.getSelection();
  let anchors = [];
  if (sel && !sel.isCollapsed) {
    for (let i = 0; i < sel.rangeCount; i++) {
      const frag = sel.getRangeAt(i).cloneContents();
      anchors.push(...frag.querySelectorAll("a[href]"));
      const parent = sel.getRangeAt(i).commonAncestorContainer;
      if (parent instanceof Element && parent.closest("a[href]")) anchors.push(parent.closest("a[href]"));
    }
  } else {
    anchors = [...document.querySelectorAll("a[href]")];
  }
  const seen = new Set();
  const links = [];
  for (const a of anchors) {
    let url;
    try {
      url = new URL(a.getAttribute("href"), document.baseURI).href;
    } catch {
      continue;
    }
    if (!/^https?:/i.test(url) || seen.has(url)) continue;
    seen.add(url);
    links.push({ url, text: (a.getAttribute("download") || a.textContent || "").trim().slice(0, 200) });
  }
  return { links, selection: !!(sel && !sel.isCollapsed) };
}

/** Link text that reads like a file name ("setup-2.1.exe"), usable as a name hint. */
function nameFromText(text) {
  const t = (text || "").trim();
  return /^[^\\/:*?"<>|\n]{1,150}\.[a-z0-9]{1,8}$/i.test(t) ? t : null;
}

browser.contextMenus.onClicked.addListener(async (info, tab) => {
  const storeId = tab?.cookieStoreId;
  try {
    if (info.menuItemId === "kstar-link") {
      const url = info.linkUrl || info.srcUrl;
      if (!url) return;
      await handOver([await describe(url, { referrer: info.pageUrl, pageUrl: info.pageUrl, storeId })]);
    } else if (info.menuItemId === "kstar-video" && tab) {
      await handOver([await Feed.forVideo(tab, info.frameId ?? 0, info.srcUrl, info.frameUrl || info.pageUrl)]);
    } else if (info.menuItemId === "kstar-all" && tab?.id != null) {
      const [res] = await browser.scripting.executeScript({ target: { tabId: tab.id, frameIds: [info.frameId ?? 0] }, func: collectLinks });
      const { links = [], selection = false } = res?.result ?? {};
      if (!links.length) {
        notify("No links found", selection ? "The selection contains no links." : "This page has no links to download.");
        return;
      }
      const cfg = await kstarConfig();
      const exts = new Set(cfg.extensions.toLowerCase().split(/[\s,]+/).filter(Boolean));
      // Everything goes to K-Star for review; links that look like files (or were selected)
      // start out ticked.
      const items = await Promise.all(
        links.slice(0, 2000).map(async ({ url, text }) => {
          const hint = nameFromText(text);
          const item = await describe(url, { referrer: info.pageUrl, pageUrl: info.pageUrl, storeId, filename: hint });
          item.selected = selection || exts.has(extensionOf(hint || url));
          return item;
        }),
      );
      await handOver(items);
    }
  } catch (e) {
    console.error("K-Star:", e);
    health.checkedAt = 0;
    notify(info.menuItemId === "kstar-video" ? "K-Star couldn't take the video" : "K-Star couldn't take the links", reasonOf(e));
  }
});

// ---------------------------------------------------------------- messages & lifecycle

browser.runtime.onMessage.addListener((msg, sender) => {
  switch (msg?.type) {
    case "media-list":
      return Feed.forPopup(msg.tabId, !!msg.fresh).catch(() => []);
    case "media-download":
      return Feed.download(msg.tabId, msg.keys).then(
        () => ({ ok: true }),
        (e) => ({ ok: false, message: reasonOf(e) }),
      );
    case "video-button": {
      const tab = sender.tab;
      if (!tab) return;
      // The frame's address as it is now: sender.url stays the one the frame opened with,
      // so after YouTube moves to another video in place it still names the first.
      return Feed.forVideo(tab, sender.frameId ?? 0, msg.src, msg.pageUrl || sender.url)
        .then((item) => handOver([item]))
        .then(
          () => ({ ok: true }),
          (e) => {
            health.checkedAt = 0;
            notify("K-Star couldn't take the video", reasonOf(e));
            return { ok: false, message: reasonOf(e) };
          },
        );
    }
    case "bypass":
      bypassUntil = Date.now() + 4000;
      return;
    case "health":
      return refreshHealth(true);
    case "status":
      return kstarSend({ type: "get_status" }).catch((e) => ({ type: "error", message: String(e) }));
    case "show":
      return kstarSend({ type: "show" }).catch((e) => ({ type: "error", message: String(e) }));
    case "add-url":
      return describe(msg.url, { referrer: msg.referrer, pageUrl: msg.referrer }).then((d) => handOver([d]));
    case "last-error":
      return browser.storage.session.get("lastError").then((r) => r.lastError ?? null);
    case "clear-error":
      return browser.storage.session.remove("lastError");
  }
});

browser.runtime.onInstalled.addListener(({ reason }) => {
  createMenus();
  if (reason === "install") browser.runtime.openOptionsPage();
});
browser.runtime.onStartup.addListener(() => {
  createMenus();
  refreshHealth(true);
});
