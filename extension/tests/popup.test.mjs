// The popup, run against a stand-in page and browser: `node --test extension/tests/*.test.mjs`.
// Firefox answers the popup's questions asynchronously (storage, the active tab, the
// background); the stand-in does too, so start-up order matters here as it does there.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const ext = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");

/** A page element with just what the popup touches. */
function element() {
  const el = {
    hidden: false,
    disabled: false,
    textContent: "",
    className: "",
    value: "",
    title: "",
    type: "",
    checked: false,
    dataset: {},
    style: {},
    children: [],
    attrs: {},
    setAttribute(k, v) {
      el.attrs[k] = v;
    },
    addEventListener() {},
    append(...c) {
      el.children.push(...c);
    },
    insertBefore(c, ref) {
      const i = el.children.indexOf(ref);
      el.children.splice(i < 0 ? el.children.length : i, 0, c);
    },
    replaceChildren(...c) {
      el.children = c;
    },
    querySelector: () => element(),
  };
  return el;
}

/** Opens the popup on a tab that has `videos`; resolves once its start-up has settled. */
async function openPopup(videos) {
  const later = (v) => new Promise((ok) => setTimeout(() => ok(v), 5));
  const els = new Map();
  // As in popup.html, the videos section starts hidden.
  els.set("videos", Object.assign(element(), { hidden: true }));
  const asked = [];
  const document = {
    getElementById: (id) => els.get(id) || (els.set(id, element()), els.get(id)),
    createElement: () => element(),
    querySelector: () => null,
    querySelectorAll: () => [],
  };
  const messages = JSON.parse(fs.readFileSync(path.join(ext, "_locales/en/messages.json"), "utf8"));
  const browser = {
    i18n: {
      getMessage: (k, subs) => (messages[k]?.message ?? "").replace(/\$(\d)/g, (_, n) => [].concat(subs ?? [])[n - 1] ?? ""),
      getUILanguage: () => "en",
    },
    storage: { local: { get: (d) => later(d), set: () => later() }, onChanged: { addListener() {} } },
    tabs: { query: () => later([{ id: 7, url: "https://www.instagram.com/" }]) },
    runtime: {
      sendMessage: (m) => {
        asked.push(m);
        if (m.type === "media-list") return later(m.tabId === 7 ? videos : []);
        if (m.type === "health") return later({ installed: true, running: true, version: "0.7.4" });
        if (m.type === "status") return later({ type: "status", active: 0, queued: 0, speed: 0 });
        return later(null);
      },
      openOptionsPage() {},
    },
  };
  const ctx = vm.createContext({ document, browser, window: { close() {} }, console, URL, setTimeout });
  for (const f of ["compat.js", "shared.js", "popup/popup.js"]) vm.runInContext(fs.readFileSync(path.join(ext, f), "utf8"), ctx, { filename: f });
  await new Promise((ok) => setTimeout(ok, 100));
  return { els, asked };
}

test("the popup lists the tab's videos as soon as it opens", async () => {
  const videos = [
    { key: "instagram:A", name: "First reel", label: "DASH stream · up to 1280p" },
    { key: "instagram:B", name: "Second reel", label: "DASH stream · up to 720p" },
  ];
  const { els, asked } = await openPopup(videos);
  assert.ok(asked.some((m) => m.type === "media-list" && m.tabId === 7), "the popup never asked for the tab's videos");
  assert.equal(els.get("videos").hidden, false, "the videos section stayed hidden");
  assert.equal(els.get("video-list").children.length, 2);
  assert.equal(els.get("videos-count").textContent, "2 found");
});

test("the popup keeps the videos section hidden when the tab has none", async () => {
  const { els } = await openPopup([]);
  assert.equal(els.get("videos").hidden, true);
});

test("a DRM-protected video is listed locked, and never ticked", async () => {
  const videos = [
    { key: "mse:0:x", name: "A film", label: "DRM-protected · can't be downloaded", protected: true },
    { key: "hls:a", name: "master.m3u8", label: "HLS stream · up to 1080p" },
  ];
  const { els } = await openPopup(videos);
  const [locked, free] = els.get("video-list").children;
  const box = (li) => li.children[0].children[0];
  assert.equal(locked.className, "v-locked");
  assert.equal(box(locked).disabled, true);
  assert.equal(box(locked).checked, false);
  assert.match(locked.children[0].title, /DRM-protected/);
  // The first video that can be had is the one ticked.
  assert.equal(box(free).disabled, false);
  assert.equal(box(free).checked, true);
});

test("ads are listed last and hidden until asked for; a row can copy its address", async () => {
  const videos = [
    { key: "a", name: "The story", label: "HLS stream", link: "https://cdn.example/a.m3u8", poster: "https://cdn.example/a.jpg" },
    { key: "b", name: "Preroll", label: "HLS stream · ad", link: "https://s0.2mdn.net/b.m3u8", ad: true },
  ];
  const { els } = await openPopup(videos);
  const [story, ad] = els.get("video-list").children;
  assert.equal(story.hidden, false);
  assert.equal(ad.hidden, true);
  assert.equal(els.get("video-ads").hidden, false);
  assert.equal(els.get("video-ads").textContent, "Show ads (1)");
  assert.equal(els.get("videos-count").textContent, "");
  // The thumbnail sits before the words; the copy button after the label.
  assert.equal(story.children[0].children[1].className, "v-thumb");
  assert.equal(story.children[1].className, "v-copy");
});
