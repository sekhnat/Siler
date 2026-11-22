// Runs in every page and frame (the extension's own world, not the page's):
//  - Alt + click on a link lets the browser handle that download itself (Siler steps aside);
//  - a Download button over videos hands the video to Siler.
"use strict";

// Chromium: answers of site APIs, copied by hook.js from the page's world.
window.addEventListener("message", (e) => {
  if (e.source !== window || !e.data?.silerBody || typeof e.data.url !== "string") return;
  browser.runtime.sendMessage({ type: "media-body", url: e.data.url, text: e.data.text, pageUrl: location.href }).catch(() => {});
});

document.addEventListener(
  "click",
  (e) => {
    if (!e.altKey) return;
    const a = e.target instanceof Element ? e.target.closest("a[href]") : null;
    if (a) browser.runtime.sendMessage({ type: "bypass" }).catch(() => {});
  },
  { capture: true, passive: true },
);

// Which link was clicked, and its words: how Siler finds it again when it expires. Not in
// private windows, and only real clicks.
function silerLinkClick(e) {
  if (!e.isTrusted || e.altKey || browser.extension?.inIncognitoContext) return;
  if (e.type === "auxclick" && e.button !== 1) return;
  const a = e.target instanceof Element ? e.target.closest("a[href]") : null;
  if (!a || typeof a.href !== "string" || !/^https?:/i.test(a.href)) return;
  const text = (a.textContent || "").replace(/\s+/g, " ").trim() || a.getAttribute("aria-label") || a.title || "";
  browser.runtime.sendMessage({ type: "link-click", url: a.href, text: text.slice(0, 200), page: location.href }).catch(() => {});
}
document.addEventListener("click", silerLinkClick, { capture: true, passive: true });
document.addEventListener("auxclick", silerLinkClick, { capture: true, passive: true });

/**
 * What surrounds a video, so a feed's videos can be told apart: the links in its post (a
 * permalink, the poster's profile), the video's shape and its length.
 */
function silerVideoClues(v) {
  let box = v.closest("article, [role='article']");
  if (!box) {
    // The largest ancestor holding this video and no other.
    for (let el = v.parentElement, i = 0; el && el !== document.body && i < 15; el = el.parentElement, i++) {
      if ([...el.getElementsByTagName("video")].some((o) => o !== v)) break;
      box = el;
    }
  }
  const hrefs = box ? [...box.querySelectorAll("a[href]")].map((a) => a.href).filter((h) => /^https?:/i.test(h)) : [];
  const duration = Number.isFinite(v.duration) ? v.duration : 0;
  return {
    links: [...new Set(hrefs)].slice(0, 60),
    width: v.videoWidth || 0,
    height: v.videoHeight || 0,
    duration,
    mse: silerIsMse(v),
    protected: silerProtected(v),
  };
}

// ---------------------------------------------------------------- videos the page assembles
// A video playing from a blob: (or data:) address is fed by the page through Media Source
// Extensions: its pieces are invisible as files. A video that raised `encrypted`, or has
// MediaKeys, plays DRM-protected media. Both are only observed here, never changed: the feed
// lists them so the popup can say what they are instead of nothing.

const silerEncrypted = new WeakSet();

/** Whether a video element shows EME evidence (an `encrypted` event, or MediaKeys set). */
function silerProtected(v) {
  try {
    return silerEncrypted.has(v) || !!v.mediaKeys;
  } catch {
    return silerEncrypted.has(v);
  }
}

const silerIsMse = (v) => /^(blob|data):/i.test(v.currentSrc || v.src || "");

/** What the feed needs to list a blob-fed or protected video. */
function silerMseInfo(v) {
  const live = v.duration === Infinity;
  return {
    src: (v.currentSrc || v.src || "").slice(0, 200),
    width: v.videoWidth || 0,
    height: v.videoHeight || 0,
    duration: Number.isFinite(v.duration) ? v.duration : 0,
    live,
    mse: silerIsMse(v),
    protected: silerProtected(v),
  };
}

/** The frame's blob-fed or protected videos (asked for by the feed's page scan). */
function silerMseList() {
  return [...document.getElementsByTagName("video")].filter((v) => silerIsMse(v) || silerProtected(v)).map(silerMseInfo);
}

const silerMseSent = new Map();
function silerReportMse(v) {
  if (!(v instanceof HTMLVideoElement)) return;
  const info = silerMseInfo(v);
  if (!info.mse && !info.protected) return;
  // Once per address and state, again after a while (single-page sites reuse elements).
  const key = `${info.src}|${info.protected}|${info.height}`;
  const at = silerMseSent.get(key);
  if (at && Date.now() - at < 30_000) return;
  silerMseSent.set(key, Date.now());
  if (silerMseSent.size > 50) silerMseSent.delete(silerMseSent.keys().next().value);
  browser.runtime.sendMessage({ type: "mse-video", pageUrl: location.href, ...info }).catch(() => {});
}
for (const type of ["play", "loadedmetadata"]) {
  document.addEventListener(type, (e) => silerReportMse(e.target), { capture: true, passive: true });
}
document.addEventListener(
  "encrypted",
  (e) => {
    if (!(e.target instanceof HTMLMediaElement)) return;
    silerEncrypted.add(e.target);
    silerReportMse(e.target);
  },
  { capture: true, passive: true },
);

// The video under the last right-click, for the context menu's "Download this video".
/** The video at a point, looking through overlays and into open shadow roots. */
function silerVideoAtPoint(x, y, root = document, depth = 0) {
  for (const el of root.elementsFromPoint(x, y)) {
    if (el instanceof HTMLVideoElement) return el;
    if (el.shadowRoot && depth < 4) {
      const inner = silerVideoAtPoint(x, y, el.shadowRoot, depth + 1);
      if (inner) return inner;
    }
  }
  return null;
}
let silerMenuVideo = null;
document.addEventListener(
  "contextmenu",
  (e) => {
    const t = e.target instanceof Element ? e.target : null;
    silerMenuVideo = t?.closest("video") || silerVideoAtPoint(e.clientX, e.clientY);
  },
  { capture: true, passive: true },
);
silerOnMessage((msg) => {
  if (msg?.type === "video-clues") return Promise.resolve(silerMenuVideo ? silerVideoClues(silerMenuVideo) : null);
});

(() => {
  const MIN_W = 240;
  const MIN_H = 135;
  const IDLE_MS = 3000;
  let enabled = false;
  let host = null;
  let button = null;
  let target = null;
  let hideTimer = 0;
  let busy = false;
  let prepared = null;
  let preparedAt = "";

  async function readSettings() {
    try {
      const s = await browser.storage.local.get({ findVideos: true, videoButton: true, blocklist: "" });
      const here = location.hostname.toLowerCase();
      const blocked = s.blocklist.split(/\s+/).filter(Boolean).some((b) => here === b || here.endsWith("." + b));
      enabled = s.findVideos && s.videoButton && !blocked;
    } catch {
      enabled = false;
    }
    if (!enabled) hide();
  }
  readSettings();
  browser.storage.onChanged.addListener((_, area) => area === "local" && readSettings());

  function make() {
    host = document.createElement("siler-video-button");
    host.style.cssText = "all:initial;position:fixed;z-index:2147483647;top:0;left:0;display:none;pointer-events:none;";
    const root = host.attachShadow({ mode: "closed" });
    root.innerHTML = `<style>
      button{all:initial;pointer-events:auto;display:flex;align-items:center;gap:6px;box-sizing:border-box;
        height:30px;padding:0 12px 0 10px;border-radius:15px;cursor:pointer;
        font:600 13px/1 system-ui,-apple-system,"Segoe UI",sans-serif;color:#fff;background:rgba(17,24,39,.82);
        box-shadow:0 2px 10px rgba(0,0,0,.35);border:1px solid rgba(255,255,255,.18);transition:opacity .2s,background .15s}
      button:hover{background:#2563eb}
      button:focus-visible{outline:2px solid #93c5fd;outline-offset:2px}
      button[data-busy]{opacity:.75;cursor:progress}
      button[data-locked]{cursor:not-allowed;background:rgba(55,65,81,.82)}
      button[data-locked]:hover{background:rgba(55,65,81,.82)}
      svg{width:15px;height:15px;flex:none}
    </style><button type="button" aria-label="${silerT("menuDownloadVideo")}" title="${silerT("menuDownload")}"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 4v11m0 0-4.5-4.5M12 15l4.5-4.5M5 20h14"/></svg><span>${silerT("buttonDownload")}</span></button>`;
    button = root.querySelector("button");
    button.addEventListener("click", onClick);
    button.addEventListener("pointerenter", () => {
      clearTimeout(hideTimer);
      // The background starts on the video now; a click usually follows.
      if (target && (target !== prepared || location.href !== preparedAt)) {
        prepared = target;
        preparedAt = location.href;
        browser.runtime.sendMessage({ type: "video-hover", pageUrl: location.href }).catch(() => {});
      }
    });
    button.addEventListener("pointerleave", () => scheduleHide());
    (document.body || document.documentElement).appendChild(host);
  }

  const big = (v) => {
    const r = v.getBoundingClientRect();
    return r.width >= MIN_W && r.height >= MIN_H && r.bottom > 0 && r.right > 0 && r.top < innerHeight && r.left < innerWidth;
  };
  const visible = (v) => {
    const s = getComputedStyle(v);
    return s.visibility !== "hidden" && s.display !== "none" && Number(s.opacity) > 0.05;
  };

  function place() {
    if (!host || !target) return;
    const r = target.getBoundingClientRect();
    if (!big(target)) return hide();
    host.style.transform = `translate(${Math.round(Math.max(4, r.right - 132))}px, ${Math.round(Math.max(4, r.top + 10))}px)`;
  }

  /** The button's state for a video: a DRM-protected one can't be sent, and says so. */
  function label(v) {
    if (busy) return;
    const locked = silerProtected(v);
    if (locked) {
      button.dataset.locked = "";
      button.title = silerT("feedDRMRefused");
      button.querySelector("span").textContent = silerT("buttonDRMLocked");
    } else {
      delete button.dataset.locked;
      button.title = silerT("menuDownload");
      button.querySelector("span").textContent = silerT("buttonDownload");
    }
  }

  function show(v) {
    if (!enabled) return;
    if (!host || !host.isConnected) make();
    target = v;
    label(v);
    host.style.display = "block";
    place();
    scheduleHide();
  }

  function hide() {
    if (host) host.style.display = "none";
    target = null;
  }

  function scheduleHide() {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
      if (!busy && !button?.matches(":hover")) hide();
    }, IDLE_MS);
  }

  /**
   * The video under the pointer: players lay their controls over the video element, so this
   * looks through everything at that point (one hit test), into open shadow roots too.
   */
  function videoAt(x, y) {
    const v = silerVideoAtPoint(x, y);
    return v && big(v) && visible(v) ? v : null;
  }

  let frame = 0;
  document.addEventListener(
    "pointermove",
    (e) => {
      if (!enabled || frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        const v = videoAt(e.clientX, e.clientY);
        if (v) show(v);
      });
    },
    { passive: true, capture: true },
  );
  document.addEventListener(
    "play",
    (e) => {
      if (e.target instanceof HTMLVideoElement && big(e.target) && visible(e.target)) show(e.target);
    },
    { capture: true, passive: true },
  );
  addEventListener("scroll", place, { passive: true, capture: true });
  addEventListener("resize", place, { passive: true });
  document.addEventListener("fullscreenchange", hide);

  async function onClick(e) {
    e.preventDefault();
    e.stopPropagation();
    if (busy || !target || silerProtected(target)) return;
    busy = true;
    button.dataset.busy = "";
    button.querySelector("span").textContent = silerT("buttonSending");
    const src = target.currentSrc || target.src || target.querySelector("source[src]")?.src || "";
    try {
      const r = await browser.runtime.sendMessage({ type: "video-button", src, pageUrl: location.href, ...silerVideoClues(target) });
      button.querySelector("span").textContent = r?.ok ? silerT("buttonSent") : silerT("buttonNotSent");
    } catch {
      button.querySelector("span").textContent = silerT("buttonNotSent");
    }
    setTimeout(() => {
      busy = false;
      delete button.dataset.busy;
      if (target) label(target);
      else button.querySelector("span").textContent = silerT("buttonDownload");
      scheduleHide();
    }, 1800);
  }
})();
