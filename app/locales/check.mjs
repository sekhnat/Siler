// Checks the catalogs against the code: `node locales/check.mjs` (run by CI).
//  - every catalog parses;
//  - every t("…") key in the interface and every i18n key the Rust side uses is in en-US;
//  - no en-US message is unused;
//  - a translation has no messages en-US doesn't (stale ones), and no variables it can't get.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { FluentBundle, FluentResource } from "@fluent/bundle";

const app = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const walk = (dir, exts) =>
  fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) return e.name === "node_modules" ? [] : walk(p, exts);
    return exts.some((x) => p.endsWith(x)) ? [p] : [];
  });

let failed = false;
const fail = (m) => {
  console.error("✗ " + m);
  failed = true;
};

function load(lang) {
  const text = fs.readFileSync(path.join(app, "locales", lang, "siler.ftl"), "utf8");
  const res = new FluentResource(text);
  const bundle = new FluentBundle(lang, { useIsolating: false });
  const errors = bundle.addResource(res);
  for (const e of errors) fail(`${lang}: ${e}`);
  const ids = new Map();
  for (const m of res.body) if (m.id) ids.set(m.id, m);
  // A line Fluent couldn't read becomes a "Junk" entry.
  for (const m of res.body) if (m.type === "Junk" || m.annotations?.length) fail(`${lang}: can't be read near: ${String(m.content ?? "").slice(0, 80)}`);
  return { ids, text };
}

const langs = fs.readdirSync(path.join(app, "locales")).filter((d) => fs.statSync(path.join(app, "locales", d)).isDirectory());
const en = load("en-US");

// Keys the code uses.
const used = new Set();
const dynamic = [];
for (const f of walk(path.join(app, "src"), [".svelte", ".ts"])) {
  const s = fs.readFileSync(f, "utf8");
  for (const m of s.matchAll(/\bt\("([a-z0-9-]+)"/g)) used.add(m[1]);
  // Keys made from data: t(`error-${code}`) and the like.
  for (const m of s.matchAll(/\b(?:t|has)\(`([a-z0-9-]+)-\$\{/g)) dynamic.push(m[1] + "-");
}
for (const f of walk(path.join(app, "src-tauri", "src"), [".rs"])) {
  const s = fs.readFileSync(f, "utf8");
  // Any string that names a message (tr("…"), tr_args("…", …), or a table of them).
  for (const m of s.matchAll(/"([a-z][a-z0-9]*(?:-[a-z0-9]+)+)"/g)) if (en.ids.has(m[1])) used.add(m[1]);
  // Ids made from data: format!("error-{code}").
  for (const m of s.matchAll(/format!\(\s*"([a-z0-9-]+)-\{/g)) dynamic.push(m[1] + "-");
}

for (const k of used) if (!en.ids.has(k)) fail(`en-US has no message “${k}” (used in the code)`);
for (const k of en.ids.keys()) {
  if (!used.has(k) && !dynamic.some((p) => k.startsWith(p))) fail(`en-US message “${k}” isn't used`);
}

for (const lang of langs.filter((l) => l !== "en-US")) {
  const tr = load(lang);
  for (const k of tr.ids.keys()) if (!en.ids.has(k)) fail(`${lang}: “${k}” isn't in en-US any more`);
  const vars = (m) => new Set([...JSON.stringify(m).matchAll(/"type":"VariableReference","name":"([^"]+)"/g)].map((x) => x[1]));
  for (const [k, m] of tr.ids) {
    const have = en.ids.get(k);
    if (!have) continue;
    const allowed = vars(have);
    for (const v of vars(m)) if (!allowed.has(v)) fail(`${lang}: “${k}” uses $${v}, which en-US doesn't give`);
  }
  const missing = [...en.ids.keys()].filter((k) => !tr.ids.has(k)).length;
  console.log(`${lang}: ${tr.ids.size} messages, ${missing} still in English`);
}

console.log(`en-US: ${en.ids.size} messages, ${used.size} used directly`);
if (failed) process.exit(1);
