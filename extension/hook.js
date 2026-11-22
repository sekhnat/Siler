// Chromium only: runs in the page's own world on the sites whose API answers carry their
// videos (Facebook, Instagram, TikTok, Vimeo's player), and copies those answers to the
// extension. Firefox reads them with its response filter instead. Nothing else is read, and
// the answers go only to this extension's content script in the same frame.
(() => {
  "use strict";
  // The same addresses as Siler.sites.BODY_URLS (a test keeps them equal).
  const PATTERNS = [
    "*://player.vimeo.com/video/*/config*",
    "*://www.facebook.com/api/graphql/*",
    "*://web.facebook.com/api/graphql/*",
    "*://m.facebook.com/api/graphql/*",
    "*://www.instagram.com/api/graphql*",
    "*://www.instagram.com/graphql/query*",
    "*://www.instagram.com/api/v1/media/*",
    "*://i.instagram.com/api/v1/media/*",
    "*://www.tiktok.com/api/item/detail/*",
    "*://www.tiktok.com/api/post/item_list/*",
    "*://www.tiktok.com/api/recommend/item_list/*",
    "*://www.tiktok.com/api/related/item_list/*",
  ];
  const MAX = 8 * 1024 * 1024;
  const toRegex = (p) => new RegExp("^" + p.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*") + "$");
  const matchers = PATTERNS.map(toRegex);
  const wanted = (url) => {
    try {
      const u = new URL(url, location.href).href;
      return matchers.some((m) => m.test(u)) ? u : null;
    } catch {
      return null;
    }
  };
  const send = (url, text) => {
    if (typeof text !== "string" || !text || text.length > MAX) return;
    window.postMessage({ silerBody: true, url, text }, location.origin);
  };

  const origFetch = window.fetch;
  if (typeof origFetch === "function") {
    window.fetch = function (input, init) {
      const p = origFetch.apply(this, arguments);
      const url = wanted(typeof input === "string" ? input : input?.url);
      if (url) {
        p.then((r) => r.clone().text().then((t) => send(url, t))).catch(() => {});
      }
      return p;
    };
  }

  const open = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url) {
    this.__silerUrl = wanted(url);
    return open.apply(this, arguments);
  };
  const sendXhr = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.send = function () {
    if (this.__silerUrl) {
      this.addEventListener("load", () => {
        try {
          if (this.responseType === "" || this.responseType === "text") send(this.__silerUrl, this.responseText);
        } catch {
          /* not readable */
        }
      });
    }
    return sendXhr.apply(this, arguments);
  };
})();
