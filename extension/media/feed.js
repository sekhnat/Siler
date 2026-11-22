// The video feed: videos found on each tab, from the requests its players make, from what
// recognised sites' pages and APIs carry, and on demand for YouTube. They are listed in the
// popup, counted on the toolbar button, and handed to Siler from the popup, the context
// menu or the button over a video. Loaded before background.js, whose helpers it uses.
"use strict";

const Feed = (() => {
  const MAX_PER_TAB = 100;
  const MANIFEST_PEEK = 256 * 1024;
  const BODY_MAX = 8 * 1024 * 1024;
  const { classify, keyOf, inspectManifest, label, bareType, interesting, sniff, rawExt, rangeTotal, VAGUE_TYPES, isAd, pieceOf, subtitleKind } = Siler.media;
  const { siteOf, idFromUrl, pickVideo, keepsList, fromBody, fromHtml, BODY_URLS, SITES } = Siler.sites;

  // ------------------------------------------------------------ settings, kept in memory
  // (the request listeners must decide without waiting)

  let cfg = { ...SILER_DEFAULTS };
  // Derived from cfg once per change: the blocked hosts, and each page host's answer.
  let blocked = [];
  const watchedHosts = new Map();
  function derive() {
    blocked = silerBlockedHosts(cfg);
    watchedHosts.clear();
  }
  derive();
  silerConfig().then((c) => {
    cfg = c;
    derive();
  });
  browser.storage.onChanged.addListener((changes, area) => {
    if (area !== "local") return;
    for (const [k, v] of Object.entries(changes)) cfg[k] = v.newValue ?? SILER_DEFAULTS[k];
    derive();
    if ("findVideos" in changes || "blocklist" in changes) refreshAllBadges();
  });

  /** The host of an address, lower case, without parsing the whole address. */
  function rawHost(url) {
    const s = url.indexOf("://");
    if (s < 0) return "";
    let e = s + 3;
    while (e < url.length) {
      const c = url.charCodeAt(e);
      // / : ? # end the host (an @ would mean a login: let URL sort that out).
      if (c === 47 || c === 58 || c === 63 || c === 35) break;
      if (c === 64) return silerHostOf(url);
      e++;
    }
    return url.slice(s + 3, e).toLowerCase();
  }

  /** Whether videos are looked for on a page (the global switch and the site switch). */
  function watching(pageUrl) {
    if (!cfg.findVideos) return false;
    const host = rawHost(pageUrl || "");
    if (!host) return true;
    let yes = watchedHosts.get(host);
    if (yes === undefined) {
      yes = !blocked.some((b) => host === b || host.endsWith("." + b));
      if (watchedHosts.size > 500) watchedHosts.clear();
      watchedHosts.set(host, yes);
    }
    return yes;
  }

  const recogniserOn = (site) => !!site && cfg.findVideos && cfg[`site_${site}`] !== false;

  // ------------------------------------------------------------ per-tab store
  // In storage.session, since the background page can be unloaded between events.

  const memory = new Map();
  const chains = new Map();

  async function load(tabId) {
    if (memory.has(tabId)) return memory.get(tabId);
    const key = `feed:${tabId}`;
    const got = (await browser.storage.session.get(key).catch(() => ({})))[key];
    const v = got || { pageUrl: null, items: [] };
    memory.set(tabId, v);
    return v;
  }

  // The list in memory is the real one; storage.session is its copy for after the background
  // page was unloaded, written at most every SAVE_EVERY per tab (a feed page can list dozens of
  // videos a second).
  const SAVE_EVERY = 250;
  const saveTimers = new Map();

  function save(tabId) {
    if (saveTimers.has(tabId)) return;
    saveTimers.set(
      tabId,
      setTimeout(() => {
        saveTimers.delete(tabId);
        const v = memory.get(tabId);
        if (v) browser.storage.session.set({ [`feed:${tabId}`]: v }).catch(() => {});
      }, SAVE_EVERY),
    );
  }

  /** Runs `fn` on the tab's list, one change at a time. */
  function update(tabId, fn) {
    const prev = chains.get(tabId) || Promise.resolve();
    const next = prev.then(async () => {
      const v = await load(tabId);
      fn(v);
      save(tabId);
      badge(tabId, v);
    });
    chains.set(tabId, next.catch(() => {}));
    return next;
  }

  function add(tabId, cands) {
    if (tabId < 0 || !cands.length) return;
    return update(tabId, (v) => {
      for (const c of cands) {
        const old = v.items.findIndex((i) => i.key === c.key);
        // What's known stays known: a later report without a title, size or height doesn't
        // erase it.
        const merged = old >= 0 ? { ...v.items[old] } : { ...c };
        if (old >= 0) for (const [k, val] of Object.entries(c)) if (val != null) merged[k] = val;
        if (old >= 0) v.items.splice(old, 1);
        merged.at = Date.now();
        v.items.unshift(merged);
      }
      v.items.length = Math.min(v.items.length, MAX_PER_TAB);
    });
  }

  /**
   * Lists a stream once: an HLS media playlist that a listed master names is part of it, and
   * a master that arrives after its playlists replaces them.
   */
  function addStream(tabId, c) {
    return update(tabId, (v) => {
      const parent = c.kind === "hls" && !c.master ? v.items.find((i) => i.children?.includes(c.key)) : null;
      if (parent) {
        // The master is what's listed; its playlists tell its length and protection.
        if (!parent.duration && c.duration) parent.duration = c.duration;
        if (c.drm?.length && !parent.drm?.length) Object.assign(parent, { drm: c.drm, protected: true });
        return;
      }
      if (c.children?.length) v.items = v.items.filter((i) => !c.children.includes(i.key));
      v.items = v.items.filter((i) => i.key !== c.key);
      v.items.unshift({ ...c, at: Date.now() });
      v.items.length = Math.min(v.items.length, MAX_PER_TAB);
    });
  }

  function clear(tabId, pageUrl) {
    return update(tabId, (v) => {
      v.pageUrl = pageUrl;
      v.items = [];
      v.tracks = [];
    });
  }

  async function list(tabId) {
    await (chains.get(tabId) || Promise.resolve());
    return (await load(tabId)).items;
  }

  // ------------------------------------------------------------ toolbar badge

  function badge(tabId, v) {
    const problem = !health.installed || health.outdated;
    const n = v.items.length;
    browser.action.setBadgeText({ tabId, text: problem ? null : n ? String(n) : "" }).catch(() => {});
    if (!problem && n) browser.action.setBadgeBackgroundColor({ tabId, color: "#2563eb" }).catch(() => {});
  }

  async function refreshAllBadges() {
    for (const [tabId, v] of memory) badge(tabId, v);
  }

  // ------------------------------------------------------------ navigation

  /** The address that names the page's video: a new one starts a new list. */
  const pageKey = (url) => {
    try {
      const u = new URL(url);
      const id = idFromUrl(url);
      return id ? `${u.host}#${id}` : `${u.host}${u.pathname}${u.search}`;
    } catch {
      return url;
    }
  };

  // Tabs that loaded a page since their address last changed. A feed changing its address
  // in place (no load) keeps its list: the videos it fetched ahead are the ones it shows next.
  const loading = new Set();
  browser.webRequest.onBeforeRequest.addListener((d) => void (d.tabId >= 0 && loading.add(d.tabId)), { urls: ["<all_urls>"], types: ["main_frame"] });

  browser.tabs.onUpdated.addListener((tabId, info, tab) => {
    if (!info.url) return;
    const loaded = loading.delete(tabId);
    load(tabId).then((v) => {
      if (v.pageUrl && pageKey(v.pageUrl) === pageKey(info.url)) return;
      if (keepsList(v.pageUrl, info.url, loaded)) {
        update(tabId, (w) => void (w.pageUrl = info.url));
        return;
      }
      clear(tabId, info.url).then(() => noticeYouTube(tabId, info.url, tab.title));
    });
  });
  browser.tabs.onRemoved.addListener((tabId) => {
    memory.delete(tabId);
    chains.delete(tabId);
    loading.delete(tabId);
    clearTimeout(saveTimers.get(tabId));
    saveTimers.delete(tabId);
    browser.storage.session.remove(`feed:${tabId}`).catch(() => {});
  });

  /** A YouTube video page has its video, found when it is asked for. */
  function noticeYouTube(tabId, url, title) {
    const site = siteOf(url);
    if (site?.id !== "youtube" || !recogniserOn("youtube") || !watching(url)) return;
    const id = idFromUrl(url);
    if (!id) return;
    youtubeLibs().catch(() => {});
    add(tabId, [
      {
        key: `youtube:${id}`,
        site: "youtube",
        kind: "site",
        siteName: "YouTube",
        title: (title || "").replace(/\s*-\s*YouTube$/, "").trim() || null,
        ids: [id],
        url: Siler.youtube.watchUrl(id),
        frameId: 0,
      },
    ]);
  }

  // ------------------------------------------------------------ reading responses

  /**
   * Passes a response through untouched, keeping up to `max` bytes of it for `done`. Past
   * `max` the filter lets go of the response (the rest flows to the page unread) and `done`
   * gets null.
   */
  function peek(requestId, max, done) {
    let filter;
    try {
      filter = browser.webRequest.filterResponseData(requestId);
    } catch {
      done(null);
      return;
    }
    const parts = [];
    let kept = 0;
    let finished = false;
    const finish = (v) => {
      if (finished) return;
      finished = true;
      done(v);
    };
    filter.ondata = (e) => {
      filter.write(e.data);
      if (finished) return;
      if (kept + e.data.byteLength > max) {
        try {
          filter.disconnect();
        } catch {
          /* already gone */
        }
        finish(null);
        return;
      }
      parts.push(new Uint8Array(e.data));
      kept += e.data.byteLength;
    };
    filter.onstop = () => {
      try {
        filter.close();
      } catch {
        /* disconnected */
      }
      if (finished) return;
      if (!kept) return finish("");
      finish(new TextDecoder("utf-8").decode(join(parts, kept)));
    };
    filter.onerror = () => finish(null);
  }

  function join(parts, size) {
    const all = new Uint8Array(size);
    let at = 0;
    for (const p of parts) {
      all.set(p, at);
      at += p.length;
    }
    return all;
  }

  /**
   * The start of a body, fetched once more with the page's cookies: for Chromium, which can't
   * read responses as they pass, and in Firefox for manifests known only by their type.
   */
  function fetchStart(url, max, done, { range = false } = {}) {
    const headers = range ? { Range: `bytes=0-${max - 1}` } : undefined;
    fetch(url, { credentials: "include", cache: "force-cache", headers })
      .then(async (r) => {
        if (!r.ok || !r.body) return done(null);
        const reader = r.body.getReader();
        const parts = [];
        let kept = 0;
        for (;;) {
          const { value, done: end } = await reader.read();
          if (end) break;
          kept += value.length;
          if (kept > max) {
            reader.cancel().catch(() => {});
            // A ranged read only wanted the start.
            if (range) {
              parts.push(value);
              return done(join(parts, kept).subarray(0, max));
            }
            return done(null);
          }
          parts.push(value);
        }
        done(range ? join(parts, kept) : new TextDecoder("utf-8").decode(join(parts, kept)));
      })
      .catch(() => done(null));
  }

  /** The page a request belongs to (Chromium only says its origin). */
  const pageOf = (d) => d.documentUrl || d.originUrl || d.initiator || "";

  const MEDIA_TYPES = ["xmlhttprequest", "media", "object", "other"];
  // Firefox holds back only the responses whose address looks like a manifest, so their bodies
  // can be read as they pass. Measured: a blocking listener on every request a page makes
  // costs ~100 µs each (2,000 requests: +32%); one filtered by address costs nothing
  // measurable. Every other response is looked at without holding it.
  const MANIFEST_URLS = ["*://*/*.m3u8*", "*://*/*.mpd*"];
  const manifestShaped = /\.(m3u8|mpd)/i;

  /** Reads one header out of a response's list without building a map of all of them. */
  function headersOf(list) {
    let type = "";
    let length = NaN;
    let range = "";
    for (const h of list || []) {
      const n = h.name.length;
      // Only the three names this needs: content-type (12), -length (14), -range (13).
      if (n < 12 || n > 14) continue;
      const name = h.name.toLowerCase();
      if (name === "content-type") type = h.value || "";
      else if (name === "content-length") length = Number(h.value);
      else if (name === "content-range") range = h.value || "";
    }
    return { type, length, range };
  }

  // ------------------------------------------------------------ what a manifest was asked with
  // Some CDNs answer a playlist only to the headers the player sent: a token in
  // Authorization or an X- header, an Origin. Those go along with the hand-over (the app sends
  // them only to the stream's own and related hosts). Cookies, the user agent and the
  // browser's own headers are not kept here: they travel their own way, or not at all.

  const asked = new Map(); // requestId → {headers, referer}
  const KEEP = /^(authorization|origin|x-[a-z0-9-]+)$/;
  const BROWSER_OWN = /^x-(requested-with|client-data|firefox-spdy)$/;
  browser.webRequest.onSendHeaders.addListener(
    (d) => {
      const headers = [];
      let referer = null;
      for (const h of d.requestHeaders || []) {
        const n = h.name.toLowerCase();
        if (n === "referer") referer = h.value || null;
        else if (KEEP.test(n) && !BROWSER_OWN.test(n) && typeof h.value === "string" && h.value.length < 4096) headers.push([h.name, h.value]);
      }
      if (!headers.length && !referer) return;
      asked.set(d.requestId, { headers: headers.slice(0, 12), referer });
      if (asked.size > 300) asked.delete(asked.keys().next().value);
    },
    { urls: MANIFEST_URLS, types: MEDIA_TYPES },
    SILER_GECKO ? ["requestHeaders"] : ["requestHeaders", "extraHeaders"],
  );

  // ------------------------------------------------------------ subtitles and activity

  const dirOf = (url) => {
    let end = url.length;
    for (const c of ["?", "#"]) {
      const i = url.indexOf(c);
      if (i >= 0 && i < end) end = i;
    }
    return url.slice(0, url.lastIndexOf("/", end - 1) + 1);
  };

  /**
   * A WebVTT file the page loaded: a subtitle track for its streams (the app takes whole
   * WebVTT files). The pieces of an HLS stream's own subtitle playlist are left out.
   */
  function subtitleSeen(tabId, url, mime) {
    if (subtitleKind(url, mime) !== "vtt") return;
    if (/[/_-](seg|segment|chunk|frag|fragment)[-_]?\d+|\/\d+\.(web)?vtt(\?|$)|[?&]sq=/i.test(url)) return;
    const v = memory.get(tabId);
    if (v?.items.some((c) => c.kind === "hls" && c.subs && url.startsWith(dirOf(c.url)))) return;
    let name = "";
    try {
      name = decodeURIComponent(new URL(url).pathname.split("/").pop() || "");
    } catch {
      /* keep it unnamed */
    }
    enrich(tabId, {}, [{ url, lang: null, label: name.replace(/\.(web)?vtt$/i, "") || null }]);
  }

  // A stream whose pieces were fetched lately is the one playing: per tab, directory → time.
  const lastPiece = new Map();
  function pieceSeen(tabId, frameId, url) {
    const v = memory.get(tabId);
    if (!v) return;
    const dir = dirOf(url);
    const seen = lastPiece.get(tabId) || new Map();
    lastPiece.set(tabId, seen);
    if (Date.now() - (seen.get(dir) || 0) < 1000) return;
    seen.set(dir, Date.now());
    for (const c of v.items) {
      if ((c.kind === "hls" || c.kind === "dash") && dir.startsWith(dirOf(c.url))) {
        c.lastSeen = Date.now();
        c.playedIn = frameId;
      }
    }
  }
  browser.tabs.onRemoved.addListener((tabId) => lastPiece.delete(tabId));

  /** Lists a stream from its manifest's text (null: unread, listed as it is). */
  function listStream(d, base, kind, text) {
    const info = text == null ? null : inspectManifest(kind, text);
    if (text != null && !info) return; // not a manifest after all
    const children = (info?.children || []).map((u) => {
      try {
        return keyOf(new URL(u, d.url).href);
      } catch {
        return null;
      }
    });
    addStream(d.tabId, {
      ...base,
      kind,
      master: info?.master ?? true,
      height: info?.height ?? null,
      live: !!info?.live,
      children: children.filter(Boolean),
      variants: info?.variants || [],
      duration: info?.duration ?? null,
      audioOnly: !!info?.audioOnly,
      subs: !!info?.subs,
      drm: info?.drm || [],
      protected: !!info?.drm?.length,
      // An ad's stream, or any stream of an ad's frame.
      ad: isAd(d.url) || isAd(d.documentUrl || ""),
    });
  }

  // ------------------------------------------------------------ service workers
  // A page's service worker fetches for it outside any tab (tabId -1). Its requests belong to
  // the tab showing that origin: the only one, or else the active one of them.

  const tabsOf = new Map(); // origin → Map(tabId → active)
  const tabOrigin = new Map(); // tabId → origin
  function originOf(url) {
    try {
      const u = new URL(url);
      return /^https?:$/.test(u.protocol) ? u.origin : "";
    } catch {
      return "";
    }
  }
  function placeTab(tabId, url, active) {
    const was = tabOrigin.get(tabId);
    if (was) tabsOf.get(was)?.delete(tabId);
    const o = originOf(url);
    if (!o) return void tabOrigin.delete(tabId);
    tabOrigin.set(tabId, o);
    if (!tabsOf.has(o)) tabsOf.set(o, new Map());
    tabsOf.get(o).set(tabId, !!active);
  }
  browser.tabs.query({}).then((tabs) => tabs.forEach((t) => placeTab(t.id, t.url, t.active))).catch(() => {});
  browser.tabs.onUpdated.addListener((tabId, info, tab) => info.url && placeTab(tabId, info.url, tab.active));
  browser.tabs.onActivated.addListener(({ tabId, previousTabId }) => {
    for (const id of [previousTabId, tabId]) {
      const o = tabOrigin.get(id);
      if (o) tabsOf.get(o)?.set(id, id === tabId);
    }
  });
  browser.tabs.onRemoved.addListener((tabId) => {
    const o = tabOrigin.get(tabId);
    if (o) tabsOf.get(o)?.delete(tabId);
    tabOrigin.delete(tabId);
  });

  /** The tab a request outside any tab (a service worker's) belongs to, or -1. */
  function workerTab(d) {
    const tabs = tabsOf.get(originOf(d.documentUrl || d.originUrl || d.initiator || ""));
    if (!tabs?.size) return -1;
    if (tabs.size === 1) return tabs.keys().next().value;
    const active = [...tabs].filter(([, a]) => a);
    return active.length === 1 ? active[0][0] : -1;
  }

  /** What a response is, as the feed sees it; `blocking` when the listener may hold it. */
  function onResponse(d, blocking) {
    if (d.tabId < 0) {
      const tabId = workerTab(d);
      if (tabId < 0) return;
      d = { ...d, tabId, frameId: 0 };
    }
    const h = headersOf(d.responseHeaders);
    const mime = bareType(h.type);
    if (!interesting(d.url, mime)) return;
    const page = pageOf(d);
    if (!watching(page)) return;
    if (subtitleKind(d.url, mime)) return void subtitleSeen(d.tabId, d.url, mime);
    if (pieceOf(d.url, mime)) return void pieceSeen(d.tabId, d.frameId, d.url);
    const c = classify({ url: d.url, status: d.statusCode, mime: h.type, length: h.length, contentRange: h.range });
    if (!c) {
      if (unlabelled(d, mime, h)) sniffAgain(d, h, page);
      return;
    }
    // Recognised sites hand over their own videos; their stream pieces aren't listed.
    const site = siteOf(page);
    if (c.kind === "file" && recogniserOn(site?.id) && site.id !== "vimeo") return;
    const sent = asked.get(d.requestId);
    asked.delete(d.requestId);
    const base = {
      key: keyOf(d.url),
      url: d.url,
      // The Referer the player sent (an embed's own page, say) over the tab's page.
      referrer: sent?.referer || page || null,
      frameId: d.frameId,
      storeId: d.cookieStoreId || null,
      ...(sent?.headers.length ? { headers: sent.headers } : {}),
    };
    if (c.kind === "file") {
      add(d.tabId, [{ ...base, kind: "file", size: c.size, mime: c.mime, ad: isAd(d.url) || isAd(d.documentUrl || "") }]);
      return;
    }
    const listed = (text) => listStream(d, base, c.kind, text);
    if (blocking) peek(d.requestId, MANIFEST_PEEK, listed);
    else fetchStart(d.url, MANIFEST_PEEK, listed);
  }

  if (SILER_GECKO) {
    browser.webRequest.onHeadersReceived.addListener(
      (d) => {
        onResponse(d, true);
        return {};
      },
      { urls: MANIFEST_URLS, types: MEDIA_TYPES },
      ["blocking", "responseHeaders"],
    );
  }
  browser.webRequest.onHeadersReceived.addListener(
    (d) => {
      // Firefox's blocking listener above has these.
      if (SILER_GECKO && manifestShaped.test(d.url) && matchesManifestUrl(d.url)) return;
      onResponse(d, false);
    },
    { urls: ["<all_urls>"], types: MEDIA_TYPES },
    ["responseHeaders"],
  );

  // ------------------------------------------------------------ unlabelled responses
  // Some players fetch their playlists (and sites their videos) under a type that says
  // nothing, "text/plain" or raw bytes, at an address without a telling ending. Those are
  // read again, their first kilobyte only, with the page's cookies: a few per page.

  const SNIFF_BUDGET = 30; // per tab and minute
  const SNIFF_SKIP = new Set(["js", "mjs", "css", "json", "html", "htm", "svg", "png", "jpg", "jpeg", "gif", "webp", "avif", "ico", "woff", "woff2", "ttf", "otf", "map", "wasm"]);
  const sniffed = new Map(); // tabId → {since, count, keys}

  /** Whether a response is worth reading again to see what it is. */
  function unlabelled(d, mime, h) {
    if (!cfg.sniffUnlabelled || d.method !== "GET") return false;
    if (d.type !== "xmlhttprequest" && d.type !== "other" && d.type !== "media") return false;
    if (!VAGUE_TYPES.test(mime) || (d.statusCode !== 200 && d.statusCode !== 206)) return false;
    // Scripts, styles, pages and images are what they say; ".txt" and ".php" addresses are
    // where disguised playlists live, so they are read.
    if (SNIFF_SKIP.has(rawExt(d.url))) return false;
    if (Number.isFinite(h.length) && h.length < 32) return false;
    if (d.statusCode === 206 && !/bytes\s+0-/.test(h.range)) return false;
    let b = sniffed.get(d.tabId);
    if (!b || Date.now() - b.since > 60_000) sniffed.set(d.tabId, (b = { since: Date.now(), count: 0, keys: new Set() }));
    const key = keyOf(d.url);
    if (b.count >= SNIFF_BUDGET || b.keys.has(key)) return false;
    b.count++;
    b.keys.add(key);
    return true;
  }

  function sniffAgain(d, h, page) {
    const size = rangeTotal(h.range) ?? (Number.isFinite(h.length) && h.length > 0 ? h.length : null);
    fetchStart(
      d.url,
      1024,
      (bytes) => {
        if (!bytes) return;
        const kind = sniff(bytes, size);
        const base = { key: keyOf(d.url), url: d.url, referrer: page || null, frameId: d.frameId, storeId: d.cookieStoreId || null, sniffed: true };
        if (kind === "file") {
          if (recogniserOn(siteOf(page)?.id)) return;
          add(d.tabId, [{ ...base, kind: "file", size, mime: null }]);
        } else if (kind === "hls" || kind === "dash") {
          // The whole playlist, for its qualities and the playlists it names.
          fetchStart(d.url, MANIFEST_PEEK, (text) => listStream(d, base, kind, text));
        }
      },
      { range: true },
    );
  }
  browser.tabs.onRemoved.addListener((tabId) => sniffed.delete(tabId));

  /** Whether MANIFEST_URLS surely match an address: `.m3u8`/`.mpd` in its path. */
  function matchesManifestUrl(url) {
    const s = url.indexOf("://");
    const slash = s < 0 ? -1 : url.indexOf("/", s + 3);
    if (slash < 0) return false;
    let end = url.indexOf("?", slash);
    if (end < 0) end = url.indexOf("#", slash);
    return manifestShaped.test(url.slice(slash, end < 0 ? url.length : end));
  }


  /** Videos in the answer of a site's API (`url`), seen in a tab's frame. */
  function fromSiteAnswer(tabId, frameId, url, text, page, storeId) {
    const site = siteOf(url);
    if (!text || !recogniserOn(site?.id) || !watching(page)) return;
    browser.tabs
      .get(tabId)
      .then((tab) => {
        const pageUrl = site.id === "vimeo" ? page : tab.url;
        const found = fromBody(url, text, pageUrl).map((c) => ({ ...c, frameId, storeId: storeId || null }));
        add(tabId, found);
      })
      .catch(() => {});
  }

  // Site APIs whose answers carry the videos (Vimeo's player config, Facebook's and
  // Instagram's GraphQL, TikTok's item lists). Firefox reads them as they pass; Chromium
  // gets them from hook.js in the page (see `fromPage`).
  if (SILER_GECKO) browser.webRequest.onBeforeRequest.addListener(
    (d) => {
      if (d.tabId < 0) return {};
      const site = siteOf(d.url);
      const page = d.documentUrl || d.originUrl || "";
      if (!recogniserOn(site?.id) || !watching(page)) return {};
      peek(d.requestId, BODY_MAX, (text) => fromSiteAnswer(d.tabId, d.frameId, d.url, text, page, d.cookieStoreId));
      return {};
    },
    { urls: BODY_URLS, types: ["xmlhttprequest", "other", "sub_frame"] },
    ["blocking"],
  );

  // ------------------------------------------------------------ reading pages

  // Runs in each frame (isolated world): the scripts that carry a site's video data.
  // `others`: only frames other than the top one (the top one is read on its own).
  function collectPageData(others = false) {
    if (others && window === window.top) return null;
    const markers = /__UNIVERSAL_DATA_FOR_REHYDRATION__|playerConfig|dash_manifest|playable_url|browser_native|video_versions/;
    const out = [];
    let total = 0;
    for (const s of document.querySelectorAll("script:not([src])")) {
      const t = s.textContent || "";
      if (t.length < 50 || !markers.test(t)) continue;
      if (total + t.length > 16 * 1024 * 1024) break;
      total += t.length;
      const id = s.id ? ` id="${s.id}"` : "";
      const type = s.type ? ` type="${s.type}"` : "";
      out.push(`<script${id}${type}>${t}</script>`);
    }
    // content.js's list of blob-fed and protected videos, in the same isolated world.
    let mse = [];
    try {
      if (typeof silerMseList === "function") mse = silerMseList();
    } catch {
      /* not loaded in this frame */
    }
    // OpenGraph and Twitter cards, JSON-LD, and the <video>/<track> elements: what the page
    // says about its videos before any player fetched them.
    const metaOf = (sel) => document.querySelector(sel)?.getAttribute("content") || null;
    const og = {
      video: metaOf('meta[property="og:video:url"]') || metaOf('meta[property="og:video"]'),
      videoSecure: metaOf('meta[property="og:video:secure_url"]'),
      videoType: metaOf('meta[property="og:video:type"]'),
      title: metaOf('meta[property="og:title"]'),
      image: metaOf('meta[property="og:image"]'),
      twitterStream: metaOf('meta[name="twitter:player:stream"]'),
    };
    const jsonld = [];
    for (const s of [...document.querySelectorAll('script[type="application/ld+json"]')].slice(0, 20)) {
      const t = s.textContent || "";
      if (t.length > 1024 * 1024 || !/VideoObject/.test(t)) continue;
      try {
        jsonld.push(JSON.parse(t));
      } catch {
        /* not JSON */
      }
    }
    const web = (u) => (typeof u === "string" && /^https?:/i.test(u) ? u : null);
    const videos = [...document.getElementsByTagName("video")].slice(0, 30).map((v) => ({
      poster: web(v.poster),
      height: v.videoHeight || null,
      sources: [{ url: web(v.currentSrc) || web(v.src) }, ...[...v.querySelectorAll("source[src]")].map((e) => ({ url: web(e.src), type: e.type || null }))].filter((x) => x.url),
    }));
    const tracks = [...document.querySelectorAll("video track[src]")]
      .filter((t) => !t.kind || t.kind === "subtitles" || t.kind === "captions")
      .slice(0, 40)
      .map((t) => ({ url: t.src, lang: t.srclang || null, label: t.label || null }));
    const meta = { url: location.href, title: document.title, og, jsonld, videos, tracks };
    return { url: location.href, title: document.title, html: out.join("\n"), mse, meta };
  }

  /** Looks at the tab's pages for videos recognised sites embed in them. */
  async function scan(tab) {
    if (!tab?.id || !watching(tab.url)) return;
    noticeYouTube(tab.id, tab.url, tab.title);
    const sites = SITES.filter((s) => s.id !== "youtube" && recogniserOn(s.id)).map((s) => s.id);
    // The top page on its own (quick), every other frame (for embedded players), and the
    // players' own registries (page world, read only); a heavy page gets a few seconds.
    const within = (p) => Promise.race([p.catch(() => []), new Promise((r) => setTimeout(() => r([]), 5000))]);
    const [top, all, probed] = await Promise.all([
      within(browser.scripting.executeScript({ target: { tabId: tab.id, frameIds: [0] }, func: collectPageData })),
      within(browser.scripting.executeScript({ target: { tabId: tab.id, allFrames: true }, func: collectPageData, args: [true] })),
      within(browser.scripting.executeScript({ target: { tabId: tab.id, allFrames: true }, world: "MAIN", func: silerPlayerProbe })),
    ]);
    const results = [...(top || []), ...(all || []).filter((r) => r.frameId !== 0)];
    const found = [];
    const titles = {};
    const tracks = [];
    // A recognised site's videos come from its recogniser; a page's metadata there would only
    // list them again under other names.
    const generic = (url) => !recogniserOn(siteOf(url)?.id);
    for (const r of results || []) {
      const page = r?.result;
      if (!page) continue;
      const frameId = r.frameId ?? 0;
      for (const v of page.mse || []) found.push(mseCandidate(frameId, v, page.url));
      if (page.meta && generic(page.url) && generic(tab.url)) {
        const m = Siler.players.fromMeta(page.meta, frameId);
        found.push(...m.found.map((c) => ({ ...c, storeId: tab.cookieStoreId || null })));
        Object.assign(titles, m.titles);
        tracks.push(...m.tracks);
      }
      if (!page.html || !sites.length) continue;
      const site = siteOf(page.url);
      if (!site || !sites.includes(site.id)) continue;
      const pageUrl = site.id === "vimeo" ? page.url : tab.url;
      for (const c of fromHtml(site.id, page.html, pageUrl)) found.push({ ...c, frameId, storeId: tab.cookieStoreId || null });
    }
    for (const r of probed || []) {
      const answer = r?.result;
      if (!answer || !generic(answer.url) || !generic(tab.url)) continue;
      found.push(...Siler.players.fromPlayers(answer, r.frameId ?? 0).map((c) => ({ ...c, storeId: tab.cookieStoreId || null })));
      tracks.push(...Siler.players.tracksOf(answer));
    }
    await add(tab.id, found);
    await enrich(tab.id, titles, tracks);
  }

  /**
   * Titles the page's metadata gives videos already listed (by key), and subtitle files its
   * players name; nothing is reordered.
   */
  function enrich(tabId, titles, tracks) {
    if (!Object.keys(titles).length && !tracks.length) return;
    return update(tabId, (v) => {
      for (const c of v.items) if (!c.title && titles[c.key]) c.title = titles[c.key];
      const known = new Set((v.tracks || []).map((t) => t.url));
      v.tracks = [...(v.tracks || []), ...tracks.filter((t) => !known.has(t.url) && known.add(t.url))].slice(-40);
    });
  }

  // ------------------------------------------------------------ videos the page assembles

  /** A short stable tag for a string (a blob: address never leaves the feed). */
  function tagOf(s) {
    let h = 0;
    for (let i = 0; i < s.length; i++) h = (Math.imul(h, 31) + s.charCodeAt(i)) | 0;
    return (h >>> 0).toString(36);
  }

  /**
   * A video element fed through Media Source Extensions (`mse`), or with DRM evidence
   * (`protected`), as reported by content.js. Its key is no address: it is never handed over.
   */
  function mseCandidate(frameId, v, page) {
    return {
      key: `mse:${frameId}:${tagOf(String(v.src || ""))}`,
      kind: "mse",
      frameId,
      width: Number(v.width) || 0,
      height: Number(v.height) || null,
      duration: Number(v.duration) || 0,
      live: !!v.live,
      mse: !!v.mse,
      protected: !!v.protected,
      referrer: page || null,
    };
  }

  function addMse(tabId, frameId, v, page) {
    if (tabId < 0 || !watching(page)) return;
    const c = mseCandidate(frameId, v, page);
    return update(tabId, (list) => {
      const old = list.items.find((i) => i.key === c.key);
      // Protection, once seen, stays: the element raised `encrypted`.
      if (old) Object.assign(old, c, { protected: old.protected || c.protected, at: Date.now() });
      else {
        list.items.unshift({ ...c, at: Date.now() });
        list.items.length = Math.min(list.items.length, MAX_PER_TAB);
      }
    });
  }

  /** Whether a frame has something better than a recording: a stream, a file, a site's video. */
  const frameHasSource = (items, frameId) =>
    items.some((o) => o.frameId === frameId && (o.kind === "hls" || o.kind === "dash" || o.kind === "file" || o.site));

  /**
   * The list as offered: a blob-fed video only where its frame has no stream or file listed
   * (the stream is the better way to get it); a protected one always (it says why not).
   */
  const offered = (items) => items.filter((c) => c.kind !== "mse" || c.protected || !frameHasSource(items, c.frameId));

  // ------------------------------------------------------------ YouTube

  let ytLibs = null;
  async function youtubeLibs() {
    if (!ytLibs) {
      const read = (p) => fetch(browser.runtime.getURL(`media/youtube/vendor/${p}`)).then((r) => r.text());
      const [meriyah, astring, core] = await Promise.all([read("meriyah.umd.min.js"), read("astring.min.js"), read("yt.solver.core.js")]);
      ytLibs = { meriyah, astring, core };
    }
    return ytLibs;
  }

  /** Whether a finished link answers (YouTube refuses links whose challenges went wrong). */
  async function linkWorks(url, ua, ranged = true) {
    try {
      const headers = ranged ? { Range: "bytes=0-0", "User-Agent": ua } : { "User-Agent": ua };
      const r = await fetch(url, { headers, credentials: "omit", cache: "no-store" });
      r.body?.cancel().catch(() => {});
      return r.status === 206 || r.status === 200;
    } catch {
      return false;
    }
  }

  // Finished YouTube items, per tab and video, for a few minutes: links last hours, and the
  // button, the popup and a refresh often ask for the same video in a row.
  const YT_KEEP = 5 * 60_000;
  const ytDone = new Map();

  function youtubeItem(tab, cand) {
    const id = cand.ids?.[0];
    if (!id) return Promise.reject(new Error(silerT("feedNoYouTubeVideo")));
    const key = `${tab.id}:${cand.frameId ?? 0}:${id}`;
    const known = ytDone.get(key);
    if (known && Date.now() - known.at < YT_KEEP) return known.item.then((it) => structuredClone(it));
    for (const [k, v] of ytDone) if (Date.now() - v.at >= YT_KEEP) ytDone.delete(k);
    const item = resolveYouTube(tab, cand, id);
    ytDone.set(key, { at: Date.now(), item });
    item.catch(() => ytDone.get(key)?.item === item && ytDone.delete(key));
    // Each hand-over gets its own copy (the caller may add to it).
    return item.then((it) => structuredClone(it));
  }
  browser.tabs.onRemoved.addListener((tabId) => {
    for (const k of ytDone.keys()) if (k.startsWith(`${tabId}:`)) ytDone.delete(k);
  });

  async function resolveYouTube(tab, cand, id) {
    const libs = await youtubeLibs();
    const { ytPlayer = null } = await browser.storage.local.get("ytPlayer").catch(() => ({}));
    const [res] = await browser.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [cand.frameId ?? 0] },
      world: "MAIN",
      func: silerYouTubeInPage,
      args: [{ videoId: id, clients: Siler.youtube.clients(), libs, cached: ytPlayer }],
    });
    const r = res?.result;
    if (!r?.ok) throw new Error(r?.error || silerT("feedYouTubeNoAnswer"));
    if (r.preprocessed) browser.storage.local.set({ ytPlayer: r.preprocessed }).catch(() => {});
    // The tab's title names the video only while its address does.
    const tabTitle = idFromUrl(tab.url || "") === id ? pageTitle(tab.title) : null;
    for (const answer of r.clients) {
      if (answer.pr?.playabilityStatus?.status !== "OK") continue;
      const client = Siler.youtube.CLIENTS[answer.name];
      const ua = client.context.client.userAgent;
      const out = Siler.youtube.handOver(answer, client, Siler.youtube.watchUrl(id), r.web, tabTitle);
      if (out.error) continue; // another client may do better; `failure` says why if none does
      if (out.live) {
        // A live stream: its playlist must answer.
        if (!(await linkWorks(out.item.url, ua, false))) continue;
      } else {
        const fmts = Siler.youtube.usable(answer.pr.streamingData?.adaptiveFormats);
        // Behind an ad the links answer only once its time has passed (yt-dlp sleeps for it
        // too); the wait is taken here, so the app needn't take it again.
        const wait = Siler.youtube.adWait(answer.pr);
        if (!(await linkWorks(fmts[0].url, ua))) {
          if (!wait) continue;
          await new Promise((ok) => setTimeout(ok, wait * 1000));
          if (!(await linkWorks(fmts[0].url, ua))) continue;
          out.item.media.not_before_ms = null;
        }
      }
      const video = { site: "youtube", id, format: out.live ? "live" : null };
      const link = await Links.source({ url: out.item.url, storeId: tab.cookieStoreId, incognito: tab.incognito, video });
      if (link) out.item.link = link;
      console.info(`Siler: YouTube ${id} via the ${answer.name} client${out.live ? " (live)" : ""}`);
      return out.item;
    }
    const why = Siler.youtube.failure(r.clients);
    if (why.kind === "no_formats" && r.solverError) throw new Error(silerT("feedSolverFailed", [String(r.solverError)]));
    throw new Error(youTubeWhyNot(why));
  }

  /** YouTube's refusal in words (`Siler.youtube.failure`'s kinds). */
  function youTubeWhyNot(why) {
    switch (why.kind) {
      case "upcoming":
        return silerT("feedYouTubeUpcoming");
      case "members":
        return silerT("feedYouTubeMembers");
      case "age":
        return silerT("feedYouTubeAge");
      case "drm":
        return silerT("feedYouTubeDRM");
      case "live_no_url":
        return silerT("feedYouTubeLiveNoUrl");
      case "bot":
        return silerT("feedYouTubeBot");
      case "says":
        return silerT("feedYouTubeSays", [why.reason]);
      default:
        return silerT("feedNoFormats");
    }
  }

  // ------------------------------------------------------------ hand-over

  /**
   * Whether the app understands video details (protocol 2). Asked again once when the last
   * answer is unknown; an app that isn't running yet is started by the hand-over, and an
   * older one ignores details it doesn't know.
   */
  async function appTakesProtocol(min) {
    let h = await refreshHealth();
    if (h.protocol == null) h = await refreshHealth(true);
    if (h.protocol != null) return h.protocol >= min;
    return !!h.installed && min <= 2;
  }
  const appTakesHints = () => appTakesProtocol(2);

  /** The Siler item for a candidate. */
  async function itemFor(tab, c) {
    if (c.kind === "mse") {
      if (c.protected) throw new Error(silerT("feedDRMRefused"));
      // Recording from the page's player needs protocol 3, which no app speaks yet.
      throw new Error(silerT("feedMseNeedsApp"));
    }
    // A stream whose manifest names a DRM system: the app would refuse it too.
    if (c.protected) throw new Error(silerT("feedDRMRefused"));
    const hints = await appTakesHints();
    if (c.site === "youtube") {
      if (!hints) throw new Error(silerT("feedUpdateForYouTube"));
      return youtubeItem(tab, c);
    }
    if (c.hint?.manifest && !hints) {
      throw new Error(silerT("feedUpdateForSite"));
    }
    // A recognised video is named by its title, or else by the page's. Facebook's own
    // titles are often internal names, so its page's title comes first for its own video.
    const videoPage = c.site === "facebook" && siteOf(tab.url)?.id === "facebook" && !!idFromUrl(tab.url);
    // Any other video: the title its page's player or metadata gave it.
    const title = c.site ? (videoPage && pageTitle(tab.title)) || c.title || pageTitle(tab.title) : c.title || null;
    const ext = /^(mp4|m4v|webm|mkv|mov|m4a|mp3|ogg|ogv|oga|opus|flac|wav|aac|flv)$/.test(rawExt(c.url)) ? rawExt(c.url) : "mp4";
    const filename = c.filename || (title && c.kind === "file" ? `${safeName(title)}.${ext}` : null);
    const item = await describe(c.url, {
      referrer: c.referrer || tab.url,
      pageUrl: tab.url,
      filename,
      size: c.size || null,
      mime: c.mime || null,
      storeId: c.storeId || tab.cookieStoreId,
      from: {
        incognito: tab.incognito,
        video: c.site && c.ids?.length ? { site: c.site, id: String(c.ids[0]), format: formatOf(c) } : null,
      },
    });
    if (hints && (c.hint || c.site)) item.media = { ...(c.hint || {}), site: c.site || c.hint?.site || null, title: title || c.hint?.title || null };
    // Headers the player sent for the stream (a token, an Origin), and subtitle files the
    // page offers for it when the stream carries none of its own.
    if (c.headers?.length) item.headers = c.headers.map(([n, v]) => [n, v]);
    if (hints && !c.site && !c.hint) {
      const tracks = (c.kind === "hls" || c.kind === "dash") && c.subs ? [] : ((await load(tab.id)).tracks || []).filter(vttLike).slice(0, 20);
      if (title || tracks.length) item.media = { site: null, title, subtitles: tracks.map((t) => ({ url: t.url, lang: t.lang || null, label: t.label || null })) };
    }
    return item;
  }

  /** Subtitle files the app can take: WebVTT (a <track>'s file always is; others by name). */
  const vttLike = (t) => !/\.(srt|ttml|dfxp|xml)(\?|#|$)/i.test(t.url);

  /** A tab's title without the site's name ("Cat video | Facebook" → "Cat video"). */
  function pageTitle(t) {
    const s = (t || "")
      .replace(/^\(\d+\+?\)\s*/, "")
      .replace(/\s*[|•·-]\s*(Facebook|Instagram|TikTok|Vimeo|YouTube)\s*$/i, "")
      .trim();
    return s && !/^(Facebook|Instagram|TikTok|Vimeo|YouTube)$/i.test(s) ? s.slice(0, 150) : null;
  }

  // Slashes become "⧸" as the app writes them; other characters file systems dislike, spaces.
  const safeName = (s) => s.replace(/[\\/]/g, "⧸").replace(/[:*?"<>|\n\r\t]+/g, " ").replace(/\s+/g, " ").trim().slice(0, 120);

  /** What the popup shows: no cookies, headers or manifests; the address only to copy. */
  const view = (c) => ({
    key: c.key,
    kind: c.kind,
    site: c.site || null,
    title: c.title || null,
    label: label(c),
    name: c.title || (c.kind === "mse" ? pageTitle(c.pageTitle) || silerT("popupPageVideo") : nameOfUrl(c.url)),
    protected: !!c.protected,
    ad: !!c.ad,
    poster: c.poster || null,
    // Only web addresses are offered for copying; a blob-fed video has none.
    link: c.kind === "mse" || c.site === "youtube" ? null : c.url || null,
  });

  function nameOfUrl(url) {
    try {
      const u = new URL(url);
      return decodeURIComponent(u.pathname.split("/").filter(Boolean).pop() || u.host);
    } catch {
      return url;
    }
  }

  /** A tab's videos in the popup's order; with `fresh`, after looking at its pages again. */
  async function ranked(tab, fresh = false) {
    if (fresh) await scan(tab);
    else noticeYouTube(tab.id, tab.url, tab.title);
    const items = offered(await list(tab.id));
    // The video the address names, other recognised videos, then streams, then files, then
    // what can't be downloaded as it is (blob-fed or protected videos).
    const here = idFromUrl(tab.url);
    const rank = (c) => (here && c.ids?.includes(here) ? -1 : c.site ? 0 : c.kind === "mse" ? 3 : c.kind === "file" ? 2 : 1);
    // Within a rank: not an ad, not a short clip beside a longer video, playing lately, then
    // the higher quality.
    const longest = Math.max(0, ...items.map((c) => c.duration || 0));
    const minor = (c) => (c.ad ? 2 : 0) + (c.duration > 0 && c.duration < 30 && longest >= 60 ? 1 : 0);
    const order = (a, b) => rank(a) - rank(b) || minor(a) - minor(b) || (b.lastSeen || 0) - (a.lastSeen || 0) || (b.height || 0) - (a.height || 0);
    return [...items].sort(order);
  }

  /** Lists a tab's videos for the popup; with `fresh`, after looking at its pages again. */
  async function forPopup(tabId, fresh = false) {
    const tab = await browser.tabs.get(tabId);
    return (await ranked(tab, fresh)).map((c) => view(c.kind === "mse" ? { ...c, pageTitle: tab.title } : c));
  }

  /**
   * The keyboard command: the first video of the popup's list that can be sent (not an ad,
   * not protected, not a recording). Says why when there is none.
   */
  async function sendBest(tab) {
    const all = await ranked(tab, true);
    const best = all.find((c) => !c.ad && !c.protected && c.kind !== "mse") || all.find((c) => !c.protected && c.kind !== "mse");
    if (!best) {
      if (all.some((c) => c.protected)) throw new Error(silerT("feedDRMRefused"));
      if (all.some((c) => c.kind === "mse")) throw new Error(silerT("feedMseNeedsApp"));
      throw new Error(silerT("feedNoVideoYet"));
    }
    return handOver([await itemFor(tab, best)]);
  }

  /** Hands the chosen videos of a tab to Siler. */
  async function download(tabId, keys) {
    const tab = await browser.tabs.get(tabId);
    const items = await list(tabId);
    const chosen = keys.map((k) => items.find((i) => i.key === k)).filter(Boolean);
    if (!chosen.length) throw new Error(silerT("feedVideosGone"));
    const out = [];
    for (const c of chosen) out.push(await itemFor(tab, c));
    return handOver(out);
  }

  /**
   * The video behind a click (the button over a video, or the context menu): the
   * recognised video of the page, the element's own file, or the stream its frame played.
   * `clues` describe the clicked element: the links in its post and the video's shape, which
   * tell a feed's videos apart.
   */
  async function forVideo(tab, frameId, src, frameUrl, clues = {}) {
    // The clicked element itself raised `encrypted` or has MediaKeys.
    if (clues.protected) throw new Error(silerT("feedDRMRefused"));
    const pageUrl = tab.url;
    const site = siteOf(frameUrl || "") || siteOf(pageUrl);
    if (site && recogniserOn(site.id) && watching(pageUrl)) {
      if (site.id === "youtube") {
        const id = idFromUrl(frameUrl || "") || idFromUrl(pageUrl);
        if (id) {
          return youtubeItem(tab, { site: "youtube", ids: [id], frameId: siteOf(frameUrl || "")?.id === "youtube" ? frameId : 0 });
        }
      } else {
        await scan(tab);
        const mine = (await list(tab.id)).filter((c) => c.site === site.id);
        const hit = pickVideo(mine, {
          pageUrl,
          frameUrl: frameUrl || "",
          frameId,
          links: Array.isArray(clues.links) ? clues.links.slice(0, 60) : [],
          width: Number(clues.width) || 0,
          height: Number(clues.height) || 0,
          duration: Number(clues.duration) || 0,
        });
        if (hit) return itemFor(tab, hit);
        // Several videos and nothing says which one was clicked: never send another one.
        if (mine.length > 1) {
          throw new Error(silerT("feedWhichVideo", [site.name]));
        }
      }
    }
    if (src && /^https?:/i.test(src)) {
      const item = await describe(src, { referrer: frameUrl || pageUrl, pageUrl, storeId: tab.cookieStoreId, from: { incognito: tab.incognito } });
      // On a recognised site, the page names the video.
      const title = site && recogniserOn(site.id) ? pageTitle(tab.title) : null;
      if (title) {
        item.filename = `${safeName(title)}.mp4`;
        if (await appTakesHints()) item.media = { site: site.id, title };
      }
      return item;
    }
    let items = await list(tab.id);
    // Nothing known for this frame yet: the page's players and metadata may name its video
    // before it was fetched.
    if (!frameHasSource(items, frameId)) {
      await scan(tab);
      items = await list(tab.id);
    }
    // A blob-fed video in a frame where no stream or file was seen: nothing to hand over that
    // would be this video (another frame's stream would be another video).
    if (clues.mse && !frameHasSource(items, frameId)) throw new Error(silerT("feedMseNeedsApp"));
    const hit =
      pickStream(items, frameId, Number(clues.duration) || 0) ||
      items.find((c) => c.kind === "file" && c.frameId === frameId && !c.ad) ||
      items.find((c) => c.site);
    if (!hit) throw new Error(silerT("feedNoVideoYet"));
    return itemFor(tab, hit);
  }

  /**
   * The stream a clicked video plays: of the frame's streams (else the page's), not an ad's,
   * the one whose pieces were fetched last (that's what is playing), with the clicked
   * video's length when both are known, a master before its playlists.
   */
  function pickStream(items, frameId, duration) {
    const streams = items.filter((c) => (c.kind === "hls" || c.kind === "dash") && !c.ad);
    const pool = streams.some((c) => c.frameId === frameId || c.playedIn === frameId)
      ? streams.filter((c) => c.frameId === frameId || c.playedIn === frameId)
      : streams;
    if (!pool.length) return null;
    const timed = duration > 0 ? pool.filter((c) => c.duration > 0 && Math.abs(c.duration - duration) <= 2) : [];
    const from = timed.length ? timed : pool;
    return [...from].sort((a, b) => (b.lastSeen || 0) - (a.lastSeen || 0) || (b.master ? 1 : 0) - (a.master ? 1 : 0))[0];
  }

  /**
   * Starts finding the video behind the button before it is clicked (the pointer is on it).
   * Only YouTube's takes long enough to matter; its answer is kept for the click.
   */
  function prepare(tab, frameId, frameUrl) {
    const site = siteOf(frameUrl || "") || siteOf(tab.url);
    if (site?.id !== "youtube" || !recogniserOn("youtube") || !watching(tab.url)) return;
    const id = idFromUrl(frameUrl || "") || idFromUrl(tab.url);
    if (!id) return;
    appTakesHints()
      .then((hints) => hints && youtubeItem(tab, { site: "youtube", ids: [id], frameId: siteOf(frameUrl || "")?.id === "youtube" ? frameId : 0 }))
      .catch(() => {});
  }

  /** Which format of a video a candidate is. */
  const formatOf = (c) => (c.height ? `${c.kind}:${c.height}` : c.kind || null);

  /**
   * An expired hand-over's video or stream, found again in a tab opened for it: the same
   * video in the same format, or the one stream that is the same playlist. Null until then.
   */
  async function refind(tab, task) {
    const video = task.video;
    if (video?.site === "youtube") {
      return idFromUrl(tab.url || "") === video.id ? youtubeItem(tab, { site: "youtube", ids: [video.id], frameId: 0 }) : null;
    }
    if (video?.site) {
      await scan(tab);
      const same = (await list(tab.id)).filter(
        (c) => c.site === video.site && c.ids?.some((id) => String(id) === video.id) && (!video.format || formatOf(c) === video.format),
      );
      return same.length === 1 ? itemFor(tab, same[0]) : null;
    }
    const streams = (await list(tab.id)).filter((c) => c.kind === "hls" || c.kind === "dash");
    const old = keyOf(task.url);
    let same = streams.filter((c) => keyOf(c.url) === old);
    if (!same.length) {
      // A token in the path: the same playlist's name, if only one has it.
      const name = old.split("/").pop();
      same = streams.filter((c) => c.master && keyOf(c.url).split("/").pop() === name);
    }
    return same.length === 1 ? itemFor(tab, same[0]) : null;
  }

  /** Chromium: a site API's answer copied from the page by hook.js. */
  function fromPage(tab, frameId, url, text, pageUrl) {
    if (!tab?.id || typeof url !== "string" || typeof text !== "string" || text.length > BODY_MAX) return;
    // Only the addresses Firefox would read too.
    if (!BODY_URLS.some((p) => new RegExp("^" + p.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*") + "$").test(url))) return;
    fromSiteAnswer(tab.id, frameId ?? 0, url, text, pageUrl || tab.url, tab.cookieStoreId);
  }

  return { forPopup, download, forVideo, refind, scan, list, add, addMse, clear, fromPage, prepare, sendBest };
})();
