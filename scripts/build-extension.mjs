#!/usr/bin/env node
// Builds the browser extension for one browser family into dist/extension-<target>/ and
// zips it: `node scripts/build-extension.mjs firefox|chromium`.
//   firefox:  manifest.json as it is, without the Chromium-only files.
//   chromium: manifest.json with manifest.chromium.json merged over it (a key set to null
//             is removed), for the Chrome Web Store or "Load unpacked".
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const src = path.join(root, "extension");
const target = process.argv[2];
if (!["firefox", "chromium"].includes(target)) {
  console.error("usage: build-extension.mjs firefox|chromium");
  process.exit(2);
}

const base = JSON.parse(fs.readFileSync(path.join(src, "manifest.json"), "utf8"));
let manifest = base;
if (target === "chromium") {
  const overlay = JSON.parse(fs.readFileSync(path.join(src, "manifest.chromium.json"), "utf8"));
  delete overlay._comment;
  manifest = { ...base };
  for (const [k, v] of Object.entries(overlay)) {
    if (v === null) delete manifest[k];
    else manifest[k] = v;
  }
}

// Files that aren't part of either build, or only of the other one.
const skip = new Set(["tests", "manifest.chromium.json", "manifest.json", ...(target === "firefox" ? ["sw.js", "hook.js"] : [])]);
const out = path.join(root, "dist", `extension-${target}`);
fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(out, { recursive: true });
for (const entry of fs.readdirSync(src)) {
  if (skip.has(entry) || entry.startsWith(".")) continue;
  fs.cpSync(path.join(src, entry), path.join(out, entry), { recursive: true });
}
fs.writeFileSync(path.join(out, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");

const zip = path.join(root, "dist", `siler-${target}-${manifest.version}.zip`);
fs.rmSync(zip, { force: true });
execFileSync("zip", ["-qr", "-X", zip, "."], { cwd: out });
console.log(`${out}\n${zip}`);
