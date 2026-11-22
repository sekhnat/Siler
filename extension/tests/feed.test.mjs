// The video feed at work: the background scripts (Firefox flavour unless asked) against a
// stand-in browser that records listeners, storage writes, fetches and hand-overs.
// `node --test extension/tests/*.test.mjs`.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const ext = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (f) => fs.readFileSync(path.join(ext, f), "utf8");
const manifest = JSON.parse(read("manifest.json"));
const messages = JSON.parse(read("_locales/en/messages.json"));
const settle = (ms = 30) => new Promise((ok) => setTimeout(ok, ms));
const plain = (v) => JSON.parse(JSON.stringify(v));

/** An API stub where every name exists: calls resolve, `addListener` records. */
function stubApi(overrides, listeners) {
  const make = (pathName) =>
    new Proxy(function () {}, {
      get(_, prop) {
        const name = `${pathName}.${String(prop)}`;
        if (name in overrides) return overrides[name];
        if (prop === "addListener") return (fn, filter, opts) => listeners.push({ name: pathName, fn, filter, opts });
        if (prop === "then") return undefined;
        return make(name);
      },
      apply() {
        return Promise.resolve(undefined);
      },
    });
  return make("browser");
}

const TAB = { id: 5, url: "https://news.example/story", title: "Storm hits the coast | News", cookieStoreId: "firefox-default", incognito: false, active: true };

function background({ gecko = true, fetch: fetchStub = null, scripts = null, tabs = [TAB] } = {}) {
  const listeners = [];
  const sessionWrites = [];
  const fetched = [];
  const sent = [];
  const filters = [];
  const session = {};
  const overrides = {
    "browser.storage.local.get": (d) => Promise.resolve(typeof d === "object" && d ? { ...d } : {}),
    "browser.storage.session.get": (k) => Promise.resolve(typeof k === "string" && k in session ? { [k]: session[k] } : {}),
    "browser.storage.session.set": (o) => (sessionWrites.push(Object.keys(o)), Object.assign(session, structuredClone(o)), Promise.resolve()),
    "browser.runtime.getManifest": () => ({ version: "0.6.0" }),
    "browser.runtime.getURL": (p) => `moz-extension://x/${p}`,
    "browser.i18n.getMessage": (k, subs) => (messages[k]?.message ?? "").replace(/\$(\d)/g, (_, n) => [].concat(subs ?? [])[n - 1] ?? ""),
    "browser.runtime.sendNativeMessage": (_host, msg) => {
      sent.push(msg);
      return Promise.resolve(msg.type === "ping" ? { type: "pong", protocol: 2, version: "0.9.0" } : { type: "accepted" });
    },
    "browser.cookies.getAll": () => Promise.resolve([{ name: "s", value: "1" }]),
    "browser.tabs.get": (id) => {
      const t = tabs.find((x) => x.id === id);
      return t ? Promise.resolve({ ...t }) : Promise.reject(new Error("no tab"));
    },
    "browser.tabs.query": () => Promise.resolve(tabs.map((t) => ({ ...t }))),
    "browser.scripting.executeScript": (o) => Promise.resolve(scripts ? scripts(o) : []),
  };
  overrides["browser.webRequest.filterResponseData"] = gecko
    ? (requestId) => {
        const f = { requestId, written: [], closed: false, disconnected: false, write(b) { this.written.push(b); }, close() { this.closed = true; }, disconnect() { this.disconnected = true; } };
        filters.push(f);
        return f;
      }
    : undefined;
  const browser = stubApi(overrides, listeners);
  const fetchFn = async (url, init = {}) => {
    fetched.push({ url, headers: init.headers || null });
    const r = fetchStub ? await fetchStub(url, init) : null;
    if (!r) throw new Error("offline");
    const body = typeof r.body === "string" ? new TextEncoder().encode(r.body) : r.body;
    return {
      ok: (r.status || 200) < 300,
      status: r.status || 200,
      body: { getReader: () => { let done = false; return { read: async () => (done ? { done: true } : ((done = true), { value: body, done: false })), cancel: async () => {} }; } },
    };
  };
  const ctx = vm.createContext({
    browser, console: { ...console, warn() {}, info() {} }, URL, URLSearchParams, TextDecoder, TextEncoder, setTimeout, clearTimeout, setInterval, clearInterval,
    Date, Math, navigator: { userAgent: "test" }, fetch: fetchFn, btoa, atob, structuredClone, crypto: globalThis.crypto,
  });
  for (const f of manifest.background.scripts) vm.runInContext(read(f), ctx, { filename: f });

  const toRe = (p) => (p === "<all_urls>" ? /^/ : new RegExp("^" + p.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*") + "$"));
  /** Fires a webRequest event at every listener whose filter takes it, as the browser does. */
  function fire(event, d) {
    const out = [];
    for (const l of listeners) {
      if (l.name !== `browser.webRequest.${event}`) continue;
      if (l.filter?.types && !l.filter.types.includes(d.type)) continue;
      if (l.filter?.urls && !l.filter.urls.some((u) => toRe(u).test(d.url))) continue;
      out.push(l.fn(d));
    }
    return out;
  }
  const message = (msg, sender = { tab: { ...TAB }, frameId: 0 }) => {
    const l = listeners.find((x) => x.name === "browser.runtime.onMessage");
    return new Promise((ok) => {
      const async = l.fn(msg, sender, ok);
      if (!async) ok(undefined);
    });
  };
  return { listeners, sessionWrites, fetched, sent, filters, fire, message, ctx };
}

let nextId = 1;
/** A response a page's player got. */
function response(url, { type = "xmlhttprequest", ct = null, length = null, status = 200, tabId = TAB.id, frameId = 0, range = null, doc = TAB.url, method = "GET" } = {}) {
  const responseHeaders = [];
  if (ct) responseHeaders.push({ name: "Content-Type", value: ct });
  if (length != null) responseHeaders.push({ name: "Content-Length", value: String(length) });
  if (range) responseHeaders.push({ name: "Content-Range", value: range });
  return { requestId: String(nextId++), url, type, method, statusCode: status, tabId, frameId, documentUrl: doc, originUrl: doc, cookieStoreId: "firefox-default", responseHeaders };
}

/** Feeds text through a filter the background opened, as Firefox's response filter does. */
function pass(filter, text) {
  filter.ondata({ data: new TextEncoder().encode(text).buffer });
  filter.onstop();
}

const MASTER =
  '#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS="avc1.4d401e,mp4a.40.2"\n360/index.m3u8\n' +
  '#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,CODECS="avc1.640028,mp4a.40.2"\n1080/index.m3u8\n';
const MEDIA = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\na.ts\n#EXTINF:6,\nb.ts\n#EXTINF:6,\nc.ts\n#EXT-X-ENDLIST\n";

const list = async (bg, fresh = false) => plain(await bg.message({ type: "media-list", tabId: TAB.id, fresh }));

test("Firefox holds back only manifest-shaped addresses; everything else is looked at without holding", () => {
  const bg = background();
  const feed = bg.listeners.filter((l) => l.name === "browser.webRequest.onHeadersReceived" && l.filter.types.includes("xmlhttprequest"));
  const held = feed.filter((l) => (l.opts || []).includes("blocking"));
  assert.deepEqual(plain(held.map((l) => l.filter.urls)), [["*://*/*.m3u8*", "*://*/*.mpd*"]]);
  assert.ok(feed.some((l) => !(l.opts || []).includes("blocking") && l.filter.urls.includes("<all_urls>")));
  // The headers a manifest was asked with are read for manifests only.
  const sends = bg.listeners.filter((l) => l.name === "browser.webRequest.onSendHeaders" && l.filter.types.includes("xmlhttprequest"));
  assert.deepEqual(plain(sends.map((l) => l.filter.urls)), [["*://*/*.m3u8*", "*://*/*.mpd*"]]);
});

test("a page's ordinary requests cost no filter, no fetch and no storage write", async () => {
  const bg = background();
  await settle();
  const writes = bg.sessionWrites.length;
  for (let i = 0; i < 200; i++) {
    bg.fire("onHeadersReceived", response(`https://api.news.example/v1/items?p=${i}`, { ct: "application/json" }));
    bg.fire("onHeadersReceived", response(`https://cdn.news.example/app.${i}.js`, { ct: "text/javascript" }));
  }
  await settle(400);
  assert.equal(bg.filters.length, 0);
  assert.equal(bg.fetched.length, 0);
  assert.equal(bg.sessionWrites.length, writes);
});

test("a master playlist is read as it passes: its qualities, and its playlists fold into it", async () => {
  const bg = background();
  bg.fire("onHeadersReceived", response("https://cdn.news.example/v/42/master.m3u8?tok=1", { ct: "application/vnd.apple.mpegurl" }));
  assert.equal(bg.filters.length, 1);
  pass(bg.filters[0], MASTER);
  bg.fire("onHeadersReceived", response("https://cdn.news.example/v/42/1080/index.m3u8?tok=1", { ct: "application/vnd.apple.mpegurl" }));
  pass(bg.filters[1], MEDIA);
  await settle();
  const rows = await list(bg);
  assert.equal(rows.length, 1);
  // The media playlist lent the master its length; the label says quality, length and size.
  assert.match(rows[0].label, /^HLS stream · up to 1080p · 0:18 · ~/);
});

test("past its limit a response is let go, not pumped through", async () => {
  const bg = background();
  bg.fire("onHeadersReceived", response("https://cdn.example/huge.mpd", { ct: "application/dash+xml" }));
  const f = bg.filters[0];
  f.ondata({ data: new Uint8Array(300 * 1024).buffer });
  assert.equal(f.disconnected, true);
  await settle();
  // Unread, it is still listed (as a stream whose details aren't known).
  assert.equal((await list(bg)).length, 1);
});

test("a manifest known only by its type is read again, not held", async () => {
  const bg = background({ fetch: () => ({ body: MASTER }) });
  bg.fire("onHeadersReceived", response("https://cdn.example/play?id=9", { ct: "application/x-mpegURL" }));
  assert.equal(bg.filters.length, 0);
  await settle();
  assert.deepEqual(bg.fetched.map((f) => f.url), ["https://cdn.example/play?id=9"]);
  assert.match((await list(bg))[0].label, /up to 1080p/);
});

test("an unlabelled playlist is found from its first kilobyte, once, within a budget", async () => {
  const bg = background({ fetch: () => ({ body: MASTER }) });
  const disguised = "https://cdn.example/stream/abc123";
  bg.fire("onHeadersReceived", response(disguised, { ct: "text/plain" }));
  bg.fire("onHeadersReceived", response(disguised, { ct: "text/plain" }));
  await settle();
  // One ranged read, then the whole playlist.
  assert.equal(bg.fetched.length, 2);
  assert.equal(bg.fetched[0].headers.Range, "bytes=0-1023");
  const rows = await list(bg);
  assert.equal(rows.length, 1);
  assert.match(rows[0].label, /^HLS stream/);
  // Scripts and pages are what they say: never read again.
  bg.fire("onHeadersReceived", response("https://cdn.example/x.js", { ct: "text/plain" }));
  bg.fire("onHeadersReceived", response("https://cdn.example/page.html", { ct: "text/plain" }));
  await settle();
  assert.equal(bg.fetched.length, 2);
  // At most 30 a minute per tab.
  for (let i = 0; i < 40; i++) bg.fire("onHeadersReceived", response(`https://cdn.example/blob/${i}`, { ct: "application/octet-stream" }));
  await settle();
  assert.equal(bg.fetched.filter((f) => f.headers?.Range).length, 30);
});

test("a raw-bytes video is found by its start, and its full size", async () => {
  const mp4 = new Uint8Array([0, 0, 0, 24, ...new TextEncoder().encode("ftypisom"), 0, 0, 0, 0]);
  const bg = background({ fetch: () => ({ body: mp4, status: 206 }) });
  bg.fire("onHeadersReceived", response("https://media.example/get?f=77", { type: "media", ct: "application/octet-stream", status: 206, range: "bytes 0-65535/48000000" }));
  await settle();
  const [row] = await list(bg);
  assert.match(row.label, /46 MB/);
});

test("a service worker's request belongs to the one tab of its origin", async () => {
  const bg = background();
  await settle();
  bg.fire("onHeadersReceived", response("https://news.example/sw/master.m3u8", { tabId: -1, frameId: -1, doc: "https://news.example/sw.js" }));
  pass(bg.filters[0], MASTER);
  await settle();
  assert.equal((await list(bg)).length, 1);
  // The extension's own re-reads are nobody's.
  bg.fire("onHeadersReceived", response("https://cdn.example/x.m3u8", { tabId: -1, frameId: -1, doc: "moz-extension://x/_generated_background_page.html" }));
  assert.equal(bg.filters.length, 1);
});

test("the headers the player asked a manifest with go along with its hand-over", async () => {
  const bg = background();
  const r = response("https://cdn.example/secure/master.m3u8", { ct: "application/vnd.apple.mpegurl" });
  bg.fire("onSendHeaders", {
    ...r,
    requestHeaders: [
      { name: "Referer", value: "https://player.example/embed/1" },
      { name: "Origin", value: "https://player.example" },
      { name: "X-Playback-Token", value: "abc" },
      { name: "Cookie", value: "secret=1" },
      { name: "User-Agent", value: "UA" },
      { name: "Accept-Language", value: "en" },
    ],
  });
  bg.fire("onHeadersReceived", r);
  pass(bg.filters[0], MASTER);
  await settle();
  const [row] = await list(bg);
  const res = plain(await bg.message({ type: "media-download", tabId: TAB.id, keys: [row.key] }));
  assert.deepEqual(res, { ok: true });
  const item = bg.sent.find((m) => m.type === "add_downloads").items[0];
  assert.deepEqual(plain(item.headers), [["Origin", "https://player.example"], ["X-Playback-Token", "abc"]]);
  assert.equal(item.referrer, "https://player.example/embed/1");
});

test("the button picks the stream that is playing in its frame, not an ad's", async () => {
  const bg = background();
  bg.fire("onHeadersReceived", response("https://cdn.example/main/master.m3u8", { ct: "application/vnd.apple.mpegurl", frameId: 0 }));
  pass(bg.filters[0], MASTER);
  bg.fire("onHeadersReceived", response("https://cdn.example/preview/master.m3u8", { ct: "application/vnd.apple.mpegurl", frameId: 0 }));
  pass(bg.filters[1], MASTER);
  bg.fire("onHeadersReceived", response("https://s0.2mdn.net/ad/master.m3u8", { ct: "application/vnd.apple.mpegurl", frameId: 0 }));
  pass(bg.filters[2], MASTER);
  await settle();
  // The main stream's pieces are the ones being fetched.
  bg.fire("onHeadersReceived", response("https://cdn.example/main/1080/seg-7.ts", { type: "media", ct: "video/mp2t", length: 2e6 }));
  const res = plain(await bg.message({ type: "video-button", src: "blob:https://news.example/1", pageUrl: TAB.url, links: [], width: 1920, height: 1080, duration: 0, mse: true }));
  assert.equal(res.ok, true);
  const item = bg.sent.filter((m) => m.type === "add_downloads").pop().items[0];
  assert.equal(item.url, "https://cdn.example/main/master.m3u8");
  // In the popup the ad comes last and says so.
  const rows = await list(bg);
  assert.match(rows.at(-1).label, /ad$/);
});

test("a blob-fed video is offered only where nothing better is known, and says why it can't go", async () => {
  const bg = background();
  await bg.message({ type: "mse-video", pageUrl: TAB.url, src: "blob:https://news.example/a", width: 1280, height: 720, duration: 60, mse: true, protected: false });
  await settle();
  let rows = await list(bg);
  assert.equal(rows.length, 1);
  assert.match(rows[0].label, /Assembled by the page in pieces · up to 720p/);
  assert.equal(rows[0].link, null);
  const res = plain(await bg.message({ type: "media-download", tabId: TAB.id, keys: [rows[0].key] }));
  assert.equal(res.ok, false);
  assert.match(res.message, /can't record such videos yet/);
  // The button on it: the same honest answer, not another frame's stream.
  const btn = plain(await bg.message({ type: "video-button", src: "blob:https://news.example/a", pageUrl: TAB.url, mse: true }));
  assert.match(btn.message, /can't record such videos yet/);
  // Once the frame's stream shows up, that is what's offered.
  bg.fire("onHeadersReceived", response("https://cdn.example/v/master.m3u8", { ct: "application/vnd.apple.mpegurl" }));
  pass(bg.filters[0], MASTER);
  await settle();
  rows = await list(bg);
  assert.deepEqual(rows.map((r) => r.kind), ["hls"]);
});

test("a DRM-protected video is listed locked, and every way of sending it refuses", async () => {
  const bg = background();
  await bg.message({ type: "mse-video", pageUrl: TAB.url, src: "blob:https://news.example/film", width: 1920, height: 1080, duration: 5400, mse: true, protected: true });
  const mpd = '<MPD type="static" mediaPresentationDuration="PT90M"><Period><AdaptationSet mimeType="video/mp4"><ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/><Representation height="1080" bandwidth="5000000"/></AdaptationSet></Period></MPD>';
  bg.fire("onHeadersReceived", response("https://cdn.example/film.mpd", { ct: "application/dash+xml", frameId: 3 }));
  pass(bg.filters[0], mpd);
  await settle();
  const rows = await list(bg);
  assert.ok(rows.every((r) => r.protected));
  assert.ok(rows.some((r) => /DRM-protected \(Widevine\)/.test(r.label)));
  for (const r of rows) {
    const res = plain(await bg.message({ type: "media-download", tabId: TAB.id, keys: [r.key] }));
    assert.match(res.message, /DRM-protected/);
  }
  const btn = plain(await bg.message({ type: "video-button", src: "blob:x", pageUrl: TAB.url, protected: true }));
  assert.match(btn.message, /DRM-protected/);
});

test("a page's WebVTT file goes along with a stream that has no subtitles of its own", async () => {
  const bg = background();
  bg.fire("onHeadersReceived", response("https://cdn.example/v/master.m3u8", { ct: "application/vnd.apple.mpegurl" }));
  pass(bg.filters[0], MASTER);
  bg.fire("onHeadersReceived", response("https://cdn.example/subs/story.en.vtt", { ct: "text/vtt" }));
  // A piece of an HLS subtitle playlist is not a file of its own.
  bg.fire("onHeadersReceived", response("https://cdn.example/subs/seg-12.vtt", { ct: "text/vtt" }));
  await settle();
  const [row] = await list(bg);
  await bg.message({ type: "media-download", tabId: TAB.id, keys: [row.key] });
  const item = bg.sent.find((m) => m.type === "add_downloads").items[0];
  assert.deepEqual(plain(item.media.subtitles), [{ url: "https://cdn.example/subs/story.en.vtt", lang: null, label: "story.en" }]);
});

test("a feed page listing dozens of videos writes its list a few times, not once per video", async () => {
  const bg = background();
  await settle();
  const before = bg.sessionWrites.length;
  for (let i = 0; i < 40; i++) bg.fire("onHeadersReceived", response(`https://cdn.example/clips/${i}.mp4`, { type: "media", ct: "video/mp4", length: 5e6 }));
  await settle(400);
  assert.ok(bg.sessionWrites.length - before <= 2, `${bg.sessionWrites.length - before} writes`);
  assert.equal((await list(bg)).length, 40);
});

test("a page scan finds the player's video before it plays, named by the page", async () => {
  const scripts = (o) => {
    if (o.world === "MAIN") return [{ frameId: 0, result: { url: TAB.url, players: [{ player: "jwplayer", title: "Storm report", poster: null, sources: [{ url: "https://cdn.news.example/storm/master.m3u8", type: "hls" }], tracks: [] }] } }];
    return [{ frameId: 0, result: { url: TAB.url, title: TAB.title, html: "", mse: [], meta: { url: TAB.url, title: TAB.title, og: {}, jsonld: [], videos: [], tracks: [{ url: "https://news.example/s.vtt", lang: "en", label: "English" }] } } }];
  };
  const bg = background({ scripts });
  const rows = await list(bg, true);
  assert.deepEqual(rows.map((r) => [r.kind, r.name]), [["hls", "Storm report"]]);
  await bg.message({ type: "media-download", tabId: TAB.id, keys: [rows[0].key] });
  const item = bg.sent.find((m) => m.type === "add_downloads").items[0];
  assert.equal(item.media.title, "Storm report");
  assert.equal(item.media.subtitles[0].lang, "en");
});

test("the keyboard command sends the tab's best video, skipping ads", async () => {
  const bg = background();
  bg.fire("onHeadersReceived", response("https://s0.2mdn.net/ad/master.m3u8", { ct: "application/vnd.apple.mpegurl" }));
  pass(bg.filters[0], MASTER);
  bg.fire("onHeadersReceived", response("https://cdn.example/story/master.m3u8", { ct: "application/vnd.apple.mpegurl" }));
  pass(bg.filters[1], MASTER);
  await settle();
  const command = bg.listeners.find((l) => l.name === "browser.commands.onCommand");
  await command.fn("send-video", { ...TAB });
  await settle();
  const item = bg.sent.find((m) => m.type === "add_downloads").items[0];
  assert.equal(item.url, "https://cdn.example/story/master.m3u8");
});
