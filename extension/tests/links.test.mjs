// Expired links: what the background records about a hand-over, and how it finds a fresh
// link in a tab of its own. The real background scripts, run against a stand-in browser.
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
const PAGE = "https://site.example/files";
const OLD = "https://cdn.example/get/f.zip?token=old";
const FRESH = "https://cdn.example/get/f.zip?token=new";
// The background's seconds pass this much faster.
const FASTER = 40;

const event = () => {
  const fns = [];
  return {
    fns,
    addListener: (fn) => fns.push(fn),
    removeListener: (fn) => fns.includes(fn) && fns.splice(fns.indexOf(fn), 1),
    emit: (...args) => fns.map((fn) => fn(...args)),
  };
};

const area = (start = {}) => {
  const data = { ...start };
  return {
    data,
    async get(keys) {
      if (keys == null) return { ...data };
      if (typeof keys === "string") return keys in data ? { [keys]: data[keys] } : {};
      if (Array.isArray(keys)) return Object.fromEntries(keys.filter((k) => k in data).map((k) => [k, data[k]]));
      return Object.fromEntries(Object.entries(keys).map(([k, v]) => [k, k in data ? data[k] : v]));
    },
    async set(values) {
      Object.assign(data, structuredClone(values));
    },
    async remove(key) {
      for (const k of [].concat(key)) delete data[k];
    },
  };
};

async function until(what, ms = 5000) {
  const end = Date.now() + ms;
  for (;;) {
    const v = what();
    if (v) return v;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await new Promise((ok) => setTimeout(ok, 5));
  }
}

async function background({ settings = {}, windows = [{ id: 1, incognito: false, focused: true }], links = () => [], gecko = true, session = {} } = {}) {
  const sent = [];
  const ports = [];
  const opened = [];
  const closed = [];
  const updated = [];
  let nextTab = 40;
  const local = area({ browserId: "browser-1", ...settings });
  const changed = event();
  const webRequest = {};
  for (const e of ["onBeforeRequest", "onSendHeaders", "onBeforeRedirect", "onCompleted", "onErrorOccurred", "onHeadersReceived"]) webRequest[e] = event();
  if (gecko) webRequest.filterResponseData = () => ({ write() {}, close() {} });
  const browser = {
    i18n: { getMessage: (key) => messages[key]?.message ?? "", getUILanguage: () => "en" },
    storage: { local, session: area(session), onChanged: changed },
    action: { setBadgeText: async () => {}, setBadgeBackgroundColor: async () => {} },
    notifications: { create: async () => "n" },
    contextMenus: { onClicked: event(), removeAll: async () => {}, create() {} },
    cookies: { getAll: async (q) => [{ name: "sid", value: `${new URL(q.url).hostname}${q.storeId ? `@${q.storeId}` : ""}` }] },
    downloads: { onCreated: event(), cancel: async () => {}, erase: async () => {}, download: async () => 1 },
    windows: { getAll: async () => windows },
    tabs: {
      onCreated: event(),
      onRemoved: event(),
      onUpdated: event(),
      async create(props) {
        const tab = { id: nextTab++, windowId: props.windowId, url: props.url, status: "complete", title: "A page", incognito: false, cookieStoreId: props.cookieStoreId, props };
        opened.push(tab);
        return tab;
      },
      async update(id, props) {
        updated.push({ id, props });
        return opened.find((t) => t.id === id);
      },
      async get(id) {
        const tab = opened.find((t) => t.id === id);
        if (!tab || closed.includes(id)) throw new Error("no such tab");
        return tab;
      },
      async remove(id) {
        closed.push(...[].concat(id));
      },
      sendMessage: async () => null,
    },
    scripting: {
      executeScript: async ({ target, func }) => {
        if (func.name !== "silerLinksInPage") return [];
        const tab = opened.find((t) => t.id === target.tabId);
        return [{ frameId: 0, result: links(tab) }];
      },
    },
    webRequest,
    runtime: {
      onMessage: event(),
      onInstalled: event(),
      onStartup: event(),
      getManifest: () => ({ version: "0.5.0" }),
      getURL: (p) => `moz-extension://id/${p}`,
      openOptionsPage() {},
      async sendNativeMessage(_host, msg) {
        sent.push(msg);
        return msg.type === "ping" ? { type: "pong", version: "0.9.0", protocol: 2 } : { type: "accepted", count: 1 };
      },
      connectNative() {
        const port = { out: [], gone: false, onMessage: event(), onDisconnect: event() };
        port.postMessage = (m) => port.out.push(JSON.parse(JSON.stringify(m)));
        port.disconnect = () => (port.gone = true);
        ports.push(port);
        return port;
      },
    },
  };
  class FasterDate extends Date {
    static now() {
      return Date.now() * FASTER;
    }
  }
  const beats = [];
  const ctx = vm.createContext({
    browser,
    console: { info() {}, log() {}, warn() {}, error() {}, debug() {} },
    URL,
    URLSearchParams,
    TextDecoder,
    TextEncoder,
    AbortController,
    crypto,
    structuredClone,
    Date: FasterDate,
    navigator: { userAgent: "Stand-in/1" },
    fetch: async () => Promise.reject(new Error("no network")),
    btoa,
    atob,
    setTimeout: (fn, ms) => setTimeout(fn, Math.min((ms ?? 0) / FASTER, 2000)).unref(),
    clearTimeout,
    setInterval: (fn) => beats.push(fn),
    clearInterval: (n) => void (beats[n - 1] = null),
  });
  for (const f of manifest.background.scripts) vm.runInContext(read(f), ctx, { filename: f });
  await new Promise((ok) => setTimeout(ok, 30));

  const api = { ctx, browser, sent, ports, opened, closed, updated, local };
  api.beat = () => beats.forEach((fn) => fn?.());
  api.setting = async (key, newValue) => {
    await local.set({ [key]: newValue });
    changed.emit({ [key]: { newValue } }, "local");
  };
  api.subscribed = async () => {
    const port = await until(() => ports.at(-1));
    await until(() => port.out.length);
    port.onMessage.emit({ type: "subscribed", version: "0.9.0", protocol: 2 });
    return port;
  };
  api.handedOver = () => sent.filter((m) => m.type === "add_downloads").flatMap((m) => JSON.parse(JSON.stringify(m.items)));
  return api;
}

const ask = (port, task = {}) =>
  port.onMessage.emit({ type: "find_link", request: 7, page: PAGE, url: OLD, filename: "f.zip", size: 5_000_000, media: false, ...task });
const answerOf = (port, request = 7) => until(() => port.out.find((m) => m.type === "link_found" && m.request === request));
const anchor = (url, text = "", download = "") => ({ url, text, download });

// ---------------------------------------------------------------- the port

test("it subscribes with a ping, and beats only once that is understood", async () => {
  const b = await background();
  const port = await until(() => b.ports[0]);
  await until(() => port.out.length);
  assert.equal(port.out[0].type, "ping");
  assert.equal(port.out[0].subscribe.browser, "browser-1");
  b.beat();
  assert.equal(port.out.length, 1, "a host from before 0.9 would start Siler for a beat");
  port.onMessage.emit({ type: "waiting" });
  b.beat();
  assert.equal(port.out.at(-1).type, "beat");
});

test("an older host or app is left alone", async () => {
  for (const reply of [{ type: "pong", version: "0.8.0", protocol: 2 }, { type: "error", message: "Siler is not running" }]) {
    const b = await background();
    const port = await until(() => b.ports[0]);
    await until(() => port.out.length);
    port.onMessage.emit(reply);
    assert.ok(port.gone, reply.type);
    b.beat();
    assert.equal(port.out.length, 1);
  }
});

test("the switch", async () => {
  const off = await background({ settings: { refreshLinks: false } });
  assert.equal(off.ports.length, 0);
  await off.setting("refreshLinks", true);
  const port = await off.subscribed();
  await off.setting("refreshLinks", false);
  await until(() => port.gone);
});

test("a browser without an id gets one, and keeps it", async () => {
  const b = await background({ settings: { browserId: undefined } });
  const port = await until(() => b.ports[0]);
  await until(() => port.out.length);
  const id = port.out[0].subscribe.browser;
  assert.ok(id && id.length >= 16);
  assert.equal(b.local.data.browserId, id);
});

// ---------------------------------------------------------------- finding

test("it finds the link in a quiet tab of the same container, then closes that tab", async () => {
  const b = await background({ links: () => [anchor("https://site.example/about", "About"), anchor(FRESH, "Download")] });
  const port = await b.subscribed();
  ask(port, { container: "firefox-container-2", locator: { path: "https://cdn.example/get/f.zip", text: "Download", name: "f.zip" } });
  const { item } = await answerOf(port);
  assert.equal(item.url, FRESH);
  assert.equal(item.cookies, "sid=cdn.example@firefox-container-2");
  assert.equal(item.page_url, PAGE);
  const [tab] = b.opened;
  assert.equal(tab.props.url, PAGE);
  assert.equal(tab.props.active, false);
  assert.equal(tab.props.cookieStoreId, "firefox-container-2");
  assert.ok(b.updated.some((u) => u.id === tab.id && u.props.muted === true), "not muted");
  await until(() => b.closed.includes(tab.id));
  assert.deepEqual(b.closed, [tab.id]);
});

test("it finds a link whose path is the token, by its name or words", async () => {
  for (const links of [
    [anchor("https://cdn2.example/t-999/f.zip", "Get it"), anchor("https://cdn2.example/t-999/g.zip", "Get it")],
    [anchor("https://cdn2.example/d/83hf83", "Download"), anchor("https://site.example/help", "Help")],
  ]) {
    const b = await background({ links: () => links });
    const port = await b.subscribed();
    ask(port, { locator: { path: "https://cdn.example/t-111/f.zip", text: "Download", name: "f.zip" } });
    assert.equal((await answerOf(port)).item.url, links[0].url);
  }
});

test("between several it picks none", async () => {
  const b = await background({ links: () => [anchor("https://a.example/x/f.zip", "Download"), anchor("https://b.example/y/f.zip", "Download")] });
  const port = await b.subscribed();
  ask(port, { locator: { text: "Download", name: "f.zip" } });
  const answer = await answerOf(port);
  assert.equal(answer.item ?? null, null);
  assert.ok(answer.error);
  await until(() => b.closed.includes(b.opened[0].id));
});

test("another file with the same words isn't taken", async () => {
  const b = await background({ links: () => [anchor("https://cdn.example/other.exe", "Download")] });
  const port = await b.subscribed();
  ask(port, { locator: { text: "Download", name: "f.zip" } });
  assert.ok((await answerOf(port)).error);
});

test("it waits for a page that adds its link late", async () => {
  let looks = 0;
  const b = await background({ links: () => (++looks < 4 ? [] : [anchor(FRESH, "Download")]) });
  const port = await b.subscribed();
  ask(port);
  assert.equal((await answerOf(port)).item.url, FRESH);
});

test("the same address again is taken, later, for its fresh cookies", async () => {
  const b = await background({ links: () => [anchor(OLD, "Download")] });
  const port = await b.subscribed();
  ask(port);
  const { item } = await answerOf(port);
  assert.equal(item.url, OLD);
  assert.equal(item.cookies, "sid=cdn.example");
});

test("never in a private window, and never a page that isn't the web", async () => {
  const onlyPrivate = await background({ windows: [{ id: 3, incognito: true, focused: true }] });
  let port = await onlyPrivate.subscribed();
  ask(port);
  assert.ok((await answerOf(port)).error);
  assert.equal(onlyPrivate.opened.length, 0);

  const b = await background({ windows: [{ id: 3, incognito: true, focused: true }, { id: 4, incognito: false, focused: false }], links: () => [anchor(FRESH)] });
  port = await b.subscribed();
  ask(port, { request: 1, container: "firefox-private" });
  assert.ok((await answerOf(port, 1)).error);
  ask(port, { request: 2, page: "file:///etc/passwd" });
  assert.ok((await answerOf(port, 2)).error);
  assert.equal(b.opened.length, 0);
  ask(port, { request: 3 });
  await answerOf(port, 3);
  assert.equal(b.opened[0].props.windowId, 4);
});

test("a cancelled question closes its tab, and so does a closed port", async () => {
  const b = await background();
  const port = await b.subscribed();
  ask(port, { request: 1 });
  await until(() => b.opened.length === 1);
  port.onMessage.emit({ type: "cancel_find", request: 1 });
  await until(() => b.closed.includes(b.opened[0].id));
  ask(port, { request: 2 });
  await until(() => b.opened.length === 2);
  port.onDisconnect.emit(port);
  await until(() => b.closed.includes(b.opened[1].id));
});

test("a tab left over from a background that was stopped is closed", async () => {
  const b = await background({ session: { linkTabs: [91] } });
  await until(() => b.closed.includes(91));
});

test("a download that the page starts by itself is the answer, not a new download", async () => {
  const b = await background();
  const port = await b.subscribed();
  ask(port);
  const tab = await until(() => b.opened[0]);
  const request = { requestId: "r1", tabId: tab.id, url: FRESH, method: "GET", type: "main_frame", statusCode: 200, responseHeaders: [{ name: "content-type", value: "application/zip" }, { name: "content-disposition", value: 'attachment; filename="f.zip"' }, { name: "content-length", value: "5000000" }] };
  const results = await Promise.all(b.browser.webRequest.onHeadersReceived.emit(request));
  assert.ok(results.some((r) => r?.cancel), "the browser would download it");
  assert.equal((await answerOf(port)).item.url, FRESH);
  assert.deepEqual(b.handedOver(), []);
});

test("a recognised video is only that video", async () => {
  const b = await background();
  const port = await b.subscribed();
  ask(port, { page: "https://www.youtube.com/watch?v=AAAAAAAAAAA", media: true, url: "https://www.youtube.com/watch?v=AAAAAAAAAAA", video: { site: "youtube", id: "BBBBBBBBBBB" } });
  const tab = await until(() => b.opened[0]);
  assert.equal(tab.props.url, "https://www.youtube.com/watch?v=BBBBBBBBBBB");
});

// ---------------------------------------------------------------- recording

async function capture(b, { incognito = false, method = "GET" } = {}) {
  const first = "https://site.example/get?id=1";
  const common = { requestId: "r2", tabId: 5, type: "main_frame", method: "GET", incognito, cookieStoreId: incognito ? "firefox-private" : "firefox-default", originUrl: PAGE };
  b.browser.runtime.onMessage.emit({ type: "link-click", url: first, text: "Download f.zip", page: PAGE }, { tab: { id: 5, incognito } }, () => {});
  b.browser.webRequest.onBeforeRedirect.emit({ ...common, method, url: first, redirectUrl: "https://cdn.example/hop" });
  // A second hop doesn't replace the start of the chain.
  b.browser.webRequest.onBeforeRedirect.emit({ ...common, url: "https://cdn.example/hop", redirectUrl: FRESH });
  b.browser.webRequest.onSendHeaders.emit({ ...common, url: FRESH, requestHeaders: [{ name: "Cookie", value: "c=d" }] });
  const request = { ...common, url: FRESH, statusCode: 200, responseHeaders: [{ name: "content-type", value: "application/zip" }, { name: "content-disposition", value: 'attachment; filename="f.zip"' }, { name: "content-length", value: "5000000" }] };
  const results = await Promise.all(b.browser.webRequest.onHeadersReceived.emit(request));
  assert.ok(results.some((r) => r?.cancel), "not captured");
  return until(() => b.handedOver()[0]);
}

test("a hand-over says where its link came from", async () => {
  const item = await capture(await background());
  assert.equal(item.url, FRESH);
  assert.equal(item.link.browser, "browser-1");
  assert.equal(item.link.container, "firefox-default");
  assert.deepEqual(item.link.first, { url: "https://site.example/get?id=1", cookies: "sid=site.example@firefox-default" });
  assert.deepEqual(item.link.locator, { path: "https://site.example/get", text: "Download f.zip", name: "f.zip" });
});

test("nothing is recorded in a private window", async () => {
  const item = await capture(await background(), { incognito: true });
  assert.equal(item.link ?? null, null);
});

test("a form's address isn't asked again as a link", async () => {
  const item = await capture(await background(), { method: "POST" });
  assert.equal(item.link.first ?? null, null);
});

test("Chromium: the download's first address is recorded", async () => {
  const b = await background({ gecko: false });
  await Promise.all(b.browser.downloads.onCreated.emit({ id: 9, url: "https://site.example/get?id=2", finalUrl: FRESH, filename: "f.zip", state: "in_progress", totalBytes: 5_000_000, mime: "application/zip", referrer: PAGE, incognito: false }));
  const item = await until(() => b.handedOver()[0]);
  assert.equal(item.url, FRESH);
  assert.equal(item.link.first.url, "https://site.example/get?id=2");
  assert.equal(item.link.container ?? null, null);
});
