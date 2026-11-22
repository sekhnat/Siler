// The extension's catalog against its code: every message used exists, none is unused,
// and every translation only has messages English has.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ext = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (f) => fs.readFileSync(path.join(ext, f), "utf8");
const en = JSON.parse(read("_locales/en/messages.json"));
const files = ["background.js", "content.js", "shared.js", "media/feed.js", "popup/popup.js", "popup/popup.html", "options/options.js", "options/options.html", "manifest.json"];

const used = new Set();
for (const f of files) {
  const s = read(f);
  for (const m of s.matchAll(/silerT\("([A-Za-z]+)"/g)) used.add(m[1]);
  for (const m of s.matchAll(/data-i18n(?:-[a-z-]+)?="([A-Za-z]+)"/g)) used.add(m[1]);
  for (const m of s.matchAll(/__MSG_([A-Za-z]+)__/g)) used.add(m[1]);
}

test("every message the extension uses is in English", () => {
  const missing = [...used].filter((k) => !en[k]);
  assert.deepEqual(missing, []);
});

test("no English message is unused", () => {
  const unused = Object.keys(en).filter((k) => !used.has(k));
  assert.deepEqual(unused, []);
});

test("translations only carry messages English has", () => {
  for (const lang of fs.readdirSync(path.join(ext, "_locales"))) {
    if (lang === "en") continue;
    const tr = JSON.parse(read(`_locales/${lang}/messages.json`));
    assert.deepEqual(Object.keys(tr).filter((k) => !en[k]), [], lang);
  }
});
