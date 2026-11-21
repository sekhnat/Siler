// Runs in every page and frame (the extension's own world, not the page's):
//  - Alt + click on a link lets the browser handle that download itself (Siler steps aside);
//  - a Download button over videos hands the video to Siler.
"use strict";

document.addEventListener(
  "click",
  (e) => {
    if (!e.altKey) return;
    const a = e.target instanceof Element ? e.target.closest("a[href]") : null;
    if (a) browser.runtime.sendMessage({ type: "bypass" }).catch(() => {});
  },
  { capture: true, passive: true },
);

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
  return { links: [...new Set(hrefs)].slice(0, 60), width: v.videoWidth || 0, height: v.videoHeight || 0, duration };
}

// The video under the last right-click, for the context menu's "Download this video".
let silerMenuVideo = null;
document.addEventListener(
  "contextmenu",
  (e) => {
    const t = e.target instanceof Element ? e.target : null;
    silerMenuVideo =
      t?.closest("video") ||
      [...document.getElementsByTagName("video")].find((v) => {
        const r = v.getBoundingClientRect();
        return e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
      }) ||
      null;
  },
  { capture: true, passive: true },
);
browser.runtime.onMessage.addListener((msg) => {
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
      svg{width:15px;height:15px;flex:none}
    </style><button type="button" aria-label="Download this video with Siler" title="Download with Siler"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 4v11m0 0-4.5-4.5M12 15l4.5-4.5M5 20h14"/></svg><span>Download</span></button>`;
    button = root.querySelector("button");
    button.addEventListener("click", onClick);
    button.addEventListener("pointerenter", () => clearTimeout(hideTimer));
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

  function show(v) {
    if (!enabled) return;
    if (!host || !host.isConnected) make();
    target = v;
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

  /** The video under the pointer: players lay their controls over the video element. */
  function videoAt(x, y) {
    for (const v of document.getElementsByTagName("video")) {
      const r = v.getBoundingClientRect();
      if (x >= r.left && x <= r.right && y >= r.top && y <= r.bottom && big(v) && visible(v)) return v;
    }
    return null;
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
    if (busy || !target) return;
    busy = true;
    button.dataset.busy = "";
    button.querySelector("span").textContent = "Sending…";
    const src = target.currentSrc || target.src || target.querySelector("source[src]")?.src || "";
    try {
      const r = await browser.runtime.sendMessage({ type: "video-button", src, pageUrl: location.href, ...silerVideoClues(target) });
      button.querySelector("span").textContent = r?.ok ? "Sent to Siler" : "Not sent";
    } catch {
      button.querySelector("span").textContent = "Not sent";
    }
    setTimeout(() => {
      busy = false;
      delete button.dataset.busy;
      button.querySelector("span").textContent = "Download";
      scheduleHide();
    }, 1800);
  }
})();
