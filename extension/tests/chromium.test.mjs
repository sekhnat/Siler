// The Chromium build: the same scripts as Firefox's, run as a service worker without the
// Firefox-only parts of webRequest. `node --test extension/tests/*.test.mjs`.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const ext = path.join(here, "..");
const read = (f) => fs.readFileSync(path.join(ext, f), "utf8");
const manifest = JSON.parse(read("manifest.json"));
const overlay = JSON.parse(read("manifest.chromium.json"));

test("the service worker loads the background scripts in the background page's order", () => {
  const listed = [...read("sw.js").matchAll(/"([^"]+\.js)"/g)].map((m) => m[1]);
  assert.deepEqual(listed, manifest.background.scripts);
});

test("the page hook reads the same site answers as Firefox's response filter", () => {
  const ctx = vm.createContext({ URL, URLSearchParams, TextDecoder, console });
  for (const f of ["media/common.js", "media/sites.js"]) vm.runInContext(read(f), ctx, { filename: f });
  const body = JSON.parse(JSON.stringify(ctx.Siler.sites.BODY_URLS));
  const hook = read("hook.js");
  const patterns = JSON.parse(hook.slice(hook.indexOf("[", hook.indexOf("const PATTERNS")), hook.indexOf("];", hook.indexOf("const PATTERNS")) + 1).replace(/,\s*]/, "]"));
  assert.deepEqual(patterns, body);
  // And the hook only runs on those sites.
  const hosts = overlay.content_scripts.find((c) => c.world === "MAIN").matches;
  // (The hook runs in the page making the request, so its site, not the API's host.)
  const site = (h) => h.split(".").slice(-2).join(".");
  for (const p of body) {
    const host = p.split("/")[2];
    assert.ok(hosts.some((h) => site(h.split("/")[2]) === site(host)), `${host} has no hook`);
  }
});

test("the Chromium manifest asks for nothing Chromium refuses", () => {
  assert.ok(!overlay.permissions.includes("webRequestBlocking"));
  assert.ok(!overlay.permissions.includes("webRequestFilterResponse"));
  assert.equal(overlay.browser_specific_settings, null);
  assert.equal(overlay.background.service_worker, "sw.js");
  assert.ok(overlay.key.length > 300, "a key pins the unpacked extension's ID");
});

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
  return make("chrome");
}

function loadBackground(gecko) {
  const listeners = [];
  const sent = [];
  const calls = [];
  const overrides = {
    "chrome.storage.local.get": (d) => Promise.resolve(typeof d === "object" ? d : {}),
    "chrome.storage.session.get": () => Promise.resolve({}),
    "chrome.runtime.getManifest": () => ({ version: "0.4.0" }),
    "chrome.i18n.getMessage": (k) => k,
    "chrome.runtime.getURL": (p) => `chrome-extension://x/${p}`,
    "chrome.runtime.sendNativeMessage": (host, msg) => {
      sent.push(msg);
      if (msg.type === "ping") return Promise.resolve({ type: "pong", protocol: 2, version: "0.8.0" });
      return Promise.resolve({ type: "accepted" });
    },
    "chrome.cookies.getAll": () => Promise.resolve([{ name: "s", value: "1" }]),
    "chrome.downloads.cancel": (id) => (calls.push(["cancel", id]), Promise.resolve()),
    "chrome.downloads.erase": (q) => (calls.push(["erase", q.id]), Promise.resolve([q.id])),
  };
  overrides["chrome.webRequest.filterResponseData"] = gecko ? () => ({}) : undefined;
  const chrome = stubApi(overrides, listeners);
  const ctx = vm.createContext({ chrome, console, URL, URLSearchParams, TextDecoder, TextEncoder, setTimeout, clearTimeout, Date, Math, navigator: { userAgent: "test" }, fetch: () => Promise.reject(new Error("no network")), btoa, atob });
  for (const f of manifest.background.scripts) vm.runInContext(read(f), ctx, { filename: f });
  return { listeners, sent, calls, ctx };
}

test("Chromium registers no blocking listeners", () => {
  const { listeners } = loadBackground(false);
  const blocking = listeners.filter((l) => (l.opts || []).includes("blocking"));
  assert.deepEqual(blocking.map((l) => l.name), []);
  // Request headers include cookies and referrer only with extraHeaders.
  const sends = listeners.find((l) => l.name === "chrome.webRequest.onSendHeaders");
  assert.ok(sends.opts.includes("extraHeaders"));
});

test("Firefox keeps catching downloads at their headers", () => {
  const { listeners } = loadBackground(true);
  const headers = listeners.filter((l) => l.name === "chrome.webRequest.onHeadersReceived");
  assert.ok(headers.some((l) => l.opts.includes("blocking")));
});

test("in Chromium a download is cancelled and handed to Siler when it starts", async () => {
  const { listeners, sent, calls } = loadBackground(false);
  const created = listeners.find((l) => l.name === "chrome.downloads.onCreated");
  await created.fn({ id: 42, url: "https://files.example/big.iso", finalUrl: "https://files.example/big.iso", filename: "", mime: "application/octet-stream", totalBytes: 5e8, state: "in_progress", referrer: "https://files.example/" });
  assert.deepEqual(calls, [["cancel", 42], ["erase", 42]]);
  const add = sent.find((m) => m.type === "add_downloads");
  assert.ok(add, "nothing was handed over");
  assert.equal(add.items[0].url, "https://files.example/big.iso");
  assert.equal(add.items[0].cookies, "s=1");
});
