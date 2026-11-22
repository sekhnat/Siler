// The words of the interface, from Fluent catalogs (locales/<lang>/siler.ftl, shared with
// the Rust side of the app). English (en-US) is the source and the fallback: a message
// missing from a translation is shown in English.
//
// The language is the one the app resolved (chosen in Settings, else the system's), so the
// window and the tray agree; this window's storage and the browser's language are fallbacks.
// A change takes effect when Siler restarts.
// `en-XA` is a pseudo-language for finding words that aren't in the catalog yet: every
// message from the catalog is shown accented and in brackets, so anything plain stands out.
import { FluentBundle, FluentNumber, FluentResource, type FluentVariable } from "@fluent/bundle";
import { invoke } from "@tauri-apps/api/core";

const sources = import.meta.glob("../../locales/*/siler.ftl", { query: "?raw", import: "default", eager: true }) as Record<string, string>;
const catalogs: Record<string, string> = {};
for (const [path, text] of Object.entries(sources)) {
  const lang = path.split("/").at(-2);
  if (lang) catalogs[lang] = text;
}

/** Languages with a catalog, and the pseudo-language in development builds. */
export const LANGUAGES: string[] = [...Object.keys(catalogs).sort(), ...(import.meta.env.DEV ? ["en-XA"] : [])];

const LANG_KEY = "siler.lang";

/** The language chosen in Settings ("" = the system's). */
export function chosenLanguage(): string {
  try {
    return localStorage.getItem(LANG_KEY) ?? "";
  } catch {
    return "";
  }
}

export function setChosenLanguage(lang: string) {
  try {
    if (lang) localStorage.setItem(LANG_KEY, lang);
    else localStorage.removeItem(LANG_KEY);
  } catch {
    /* storage unavailable: the system's language is used */
  }
}

/** The best catalog for a language tag: exact, then the same language, then English. */
function resolve(tag: string): string {
  if (tag === "en-XA") return tag;
  if (catalogs[tag]) return tag;
  const base = tag.split(/[-_]/)[0].toLowerCase();
  return Object.keys(catalogs).find((l) => l.split("-")[0].toLowerCase() === base) ?? "en-US";
}

/** The language the app resolved; empty outside Siler or when it can't be asked. */
const fromApp: string = await invoke<string>("ui_language").catch(() => "");

export const language = resolve(fromApp || chosenLanguage() || navigator.language || "en-US");

function bundle(lang: string): FluentBundle {
  const b = new FluentBundle(lang === "en-XA" ? "en-US" : lang, { useIsolating: false });
  const errors = b.addResource(new FluentResource(catalogs[lang] ?? catalogs["en-US"] ?? ""));
  for (const e of errors) console.warn(`siler.ftl (${lang}):`, e);
  return b;
}

const english = bundle("en-US");
const current = language === "en-US" || language === "en-XA" ? english : bundle(language);

const PSEUDO: Record<string, string> = { a: "å", e: "é", i: "í", o: "ö", u: "ü", c: "ç", n: "ñ", A: "Å", E: "É", I: "Í", O: "Ö", U: "Ü", y: "ý", s: "š" };
const pseudo = (s: string) => `[${s.replace(/[aeioucnAEIOUys]/g, (c) => PSEUDO[c] ?? c)}]`;

export type Args = Record<string, string | number | null | undefined>;

function variables(args?: Args): Record<string, FluentVariable> | undefined {
  if (!args) return undefined;
  const out: Record<string, FluentVariable> = {};
  for (const [k, v] of Object.entries(args)) {
    // Numbers stay numbers (for plurals) but aren't grouped: a port is 8080, not 8,080.
    out[k] = typeof v === "number" ? new FluentNumber(v, { useGrouping: false, maximumFractionDigits: 20 }) : (v ?? "");
  }
  return out;
}

/** The message `id`, with `args` filled in. An unknown id shows as itself. */
export function t(id: string, args?: Args): string {
  for (const b of current === english ? [english] : [current, english]) {
    const m = b.getMessage(id);
    if (m?.value) {
      const text = b.formatPattern(m.value, variables(args));
      return language === "en-XA" ? pseudo(text) : text;
    }
  }
  if (import.meta.env.DEV) console.warn(`siler.ftl: no message “${id}”`);
  return id;
}

/** Whether the catalog has `id` (for messages whose id is made from data, e.g. an error code). */
export function has(id: string): boolean {
  return !!(current.getMessage(id)?.value || english.getMessage(id)?.value);
}
