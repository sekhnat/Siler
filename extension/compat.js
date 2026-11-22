// Runs first, in the background and every extension page, in both builds.
//
// Firefox has `browser.*` with promises; Chromium's MV3 `chrome.*` returns promises too, so
// it serves as `browser` here. What differs is feature-detected, not forked:
//  - Firefox lets the background block a response at its headers and read its body
//    (webRequest blocking + filterResponseData). Chromium doesn't (outside enterprise
//    policy), so there downloads are taken from `downloads.onCreated`, manifests are read
//    with a fetch of their own, and site API answers come from a page-world hook (hook.js).
"use strict";

if (typeof globalThis.browser === "undefined" && typeof globalThis.chrome !== "undefined") {
  globalThis.browser = globalThis.chrome;
}

/**
 * Listens for messages; `fn` may answer with a value or a promise. Chromium only waits for a
 * promised answer through `sendResponse`, which Firefox supports as well.
 */
function silerOnMessage(fn) {
  browser.runtime.onMessage.addListener((msg, sender, sendResponse) => {
    let r;
    try {
      r = fn(msg, sender);
    } catch (e) {
      console.warn("Siler: a message failed", e);
      return false;
    }
    if (r && typeof r.then === "function") {
      r.then(sendResponse, () => sendResponse(undefined));
      return true;
    }
    if (r !== undefined) sendResponse(r);
    return false;
  });
}

/** Firefox: responses can be held at their headers and their bodies read as they pass. */
const SILER_GECKO = typeof globalThis.browser?.webRequest?.filterResponseData === "function";

/**
 * The message `key` in the browser's language (_locales/<lang>/messages.json; English when
 * there is no translation), with `$1`, `$2`… filled from `subs`.
 */
function silerT(key, subs) {
  return browser.i18n.getMessage(key, subs) || key;
}

/** Fills the page's words: `data-i18n` (text), `data-i18n-placeholder`, `data-i18n-aria-label`. */
function silerLocalize(root = document) {
  for (const el of root.querySelectorAll("[data-i18n]")) el.textContent = silerT(el.dataset.i18n);
  for (const el of root.querySelectorAll("[data-i18n-placeholder]")) el.placeholder = silerT(el.dataset.i18nPlaceholder);
  for (const el of root.querySelectorAll("[data-i18n-aria-label]")) el.setAttribute("aria-label", silerT(el.dataset.i18nAriaLabel));
  if (document.documentElement) document.documentElement.lang = browser.i18n.getUILanguage();
}
