// The video feed: videos found on each tab, from the requests its players make, from what
// recognised sites' pages and APIs carry, and on demand for YouTube. They are listed in the
// popup, counted on the toolbar button, and handed to Siler from the popup, the context
// menu or the button over a video. Loaded before background.js, whose helpers it uses.
"use strict";

const Feed = (() => {
  const MAX_PER_TAB = 100;
  const MANIFEST_PEEK = 256 * 1024;
  const BODY_MAX = 8 * 1024 * 1024;
  const { classify, keyOf, inspectManifest, label } = Siler.media;
  const { siteOf, idFromUrl, pickVideo, keepsList, fromBody, fromHtml, BODY_URLS, SITES } = Siler.sites;

  // ------------------------------------------------------------ settings, kept in memory
  // (the request listeners must decide without waiting)

  let cfg = { ...SILER_DEFAULTS };
  silerConfig().then((c) => (cfg = c));
  browser.storage.onChanged.addListener((changes, area) => {
    if (area !== "local") return;
    for (const [k, v] of Object.entries(changes)) cfg[k] = v.newValue ?? SILER_DEFAULTS[k];
    if ("findVideos" in changes || "blocklist" in changes) refreshAllBadges();
  });

  /** Whether videos are looked for on a page (the global switch and the site switch). */
  function watching(pageUrl) {
    if (!cfg.findVideos) return false;
    const host = silerHostOf(pageUrl || "");
    return !host || !silerSiteBlocked(cfg, host);
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

  /** Runs `fn` on the tab's list, one change at a time, and saves it. */
  function update(tabId, fn) {
    const prev = chains.get(tabId) || Promise.resolve();
    const next = prev.then(async () => {
      const v = await load(tabId);
      fn(v);
      await browser.storage.session.set({ [`feed:${tabId}`]: v }).catch(() => {});
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
        const merged = old >= 0 ? { ...v.items[old], ...c } : c;
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
      if (c.kind === "hls" && !c.master && v.items.some((i) => i.children?.includes(c.key))) return;
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
    browser.storage.session.remove(`feed:${tabId}`).catch(() => {});
  });

  /** A YouTube video page has its video, found when it is asked for. */
  function noticeYouTube(tabId, url, title) {
    const site = siteOf(url);
    if (site?.id !== "youtube" || !recogniserOn("youtube") || !watching(url)) return;
    const id = idFromUrl(url);
    if (!id) return;
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

  /** Passes a response through untouched, keeping up to `max` bytes of it for `done`. */
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
    let over = false;
    filter.ondata = (e) => {
      filter.write(e.data);
      if (over) return;
      if (kept + e.data.byteLength > max) {
        over = true;
        return;
      }
      parts.push(new Uint8Array(e.data));
      kept += e.data.byteLength;
    };
    filter.onstop = () => {
      filter.close();
      if (over) return done(null);
      if (!kept) return done("");
      const all = new Uint8Array(kept);
      let at = 0;
      for (const p of parts) {
        all.set(p, at);
        at += p.length;
      }
      done(new TextDecoder("utf-8").decode(all));
    };
    filter.onerror = () => done(null);
  }

  /**
   * The start of a manifest's body: read as it passes in Firefox; in Chromium, which can't,
   * fetched once more with the page's cookies.
   */
  function readManifest(d, done) {
    if (SILER_GECKO) return peek(d.requestId, MANIFEST_PEEK, done);
    fetch(d.url, { credentials: "include", cache: "force-cache" })
      .then(async (r) => {
        if (!r.ok || !r.body) return done(null);
        const reader = r.body.getReader();
        const parts = [];
        let kept = 0;
        for (;;) {
          const { value, done: end } = await reader.read();
          if (end) break;
          kept += value.length;
          if (kept > MANIFEST_PEEK) {
            reader.cancel().catch(() => {});
            return done(null);
          }
          parts.push(value);
        }
        const all = new Uint8Array(kept);
        let at = 0;
        for (const p of parts) {
          all.set(p, at);
          at += p.length;
        }
        done(new TextDecoder("utf-8").decode(all));
      })
      .catch(() => done(null));
  }

  /** The page a request belongs to (Chromium only says its origin). */
  const pageOf = (d) => d.documentUrl || d.originUrl || d.initiator || "";

  const MEDIA_REQUESTS = { urls: ["<all_urls>"], types: ["xmlhttprequest", "media", "object", "other"] };

  browser.webRequest.onHeadersReceived.addListener(
    (d) => {
      if (d.tabId < 0) return {};
      const page = pageOf(d);
      if (!watching(page)) return {};
      const h = headerMap(d.responseHeaders);
      const c = classify({
        url: d.url,
        status: d.statusCode,
        mime: h["content-type"],
        length: Number(h["content-length"]),
        contentRange: h["content-range"],
      });
      if (!c) return {};
      // Recognised sites hand over their own videos; their stream pieces aren't listed.
      const site = siteOf(page);
      if (c.kind === "file" && recogniserOn(site?.id) && site.id !== "vimeo") return {};
      const base = {
        key: keyOf(d.url),
        url: d.url,
        referrer: page || null,
        frameId: d.frameId,
        storeId: d.cookieStoreId || null,
      };
      if (c.kind === "file") {
        add(d.tabId, [{ ...base, kind: "file", size: c.size, mime: c.mime }]);
        return {};
      }
      readManifest(d, (text) => {
        const info = text == null ? null : inspectManifest(c.kind, text);
        if (text != null && !info) return; // not a manifest after all
        const children = (info?.children || []).map((u) => {
          try {
            return keyOf(new URL(u, d.url).href);
          } catch {
            return null;
          }
        });
        addStream(d.tabId, { ...base, kind: c.kind, master: info?.master ?? true, height: info?.height ?? null, live: !!info?.live, children: children.filter(Boolean) });
      });
      return {};
    },
    MEDIA_REQUESTS,
    SILER_GECKO ? ["blocking", "responseHeaders"] : ["responseHeaders"],
  );

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
  function collectPageData() {
    const markers = ["__UNIVERSAL_DATA_FOR_REHYDRATION__", "playerConfig", "dash_manifest", "playable_url", "browser_native", "video_versions", "video_dash_manifest"];
    const out = [];
    let total = 0;
    for (const s of document.querySelectorAll("script:not([src])")) {
      const t = s.textContent || "";
      if (t.length < 50 || !markers.some((m) => t.includes(m))) continue;
      if (total + t.length > 16 * 1024 * 1024) break;
      total += t.length;
      const id = s.id ? ` id="${s.id}"` : "";
      const type = s.type ? ` type="${s.type}"` : "";
      out.push(`<script${id}${type}>${t}</script>`);
    }
    return { url: location.href, title: document.title, html: out.join("\n") };
  }

  /** Looks at the tab's pages for videos recognised sites embed in them. */
  async function scan(tab) {
    if (!tab?.id || !watching(tab.url)) return;
    noticeYouTube(tab.id, tab.url, tab.title);
    const sites = SITES.filter((s) => s.id !== "youtube" && recogniserOn(s.id)).map((s) => s.id);
    if (!sites.length) return;
    // The top page on its own (quick), and every frame (for embedded players); a heavy page
    // gets a few seconds.
    const within = (p) => Promise.race([p.catch(() => []), new Promise((r) => setTimeout(() => r([]), 5000))]);
    const [top, all] = await Promise.all([
      within(browser.scripting.executeScript({ target: { tabId: tab.id, frameIds: [0] }, func: collectPageData })),
      within(browser.scripting.executeScript({ target: { tabId: tab.id, allFrames: true }, func: collectPageData })),
    ]);
    const results = [...(top || []), ...(all || []).filter((r) => r.frameId !== 0)];
    const found = [];
    for (const r of results || []) {
      const page = r?.result;
      if (!page?.html) continue;
      const site = siteOf(page.url);
      if (!site || !sites.includes(site.id)) continue;
      const pageUrl = site.id === "vimeo" ? page.url : tab.url;
      for (const c of fromHtml(site.id, page.html, pageUrl)) found.push({ ...c, frameId: r.frameId ?? 0, storeId: tab.cookieStoreId || null });
    }
    await add(tab.id, found);
  }

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
  async function linkWorks(url, ua) {
    try {
      const r = await fetch(url, { headers: { Range: "bytes=0-0", "User-Agent": ua }, credentials: "omit", cache: "no-store" });
      r.body?.cancel().catch(() => {});
      return r.status === 206 || r.status === 200;
    } catch {
      return false;
    }
  }

  async function youtubeItem(tab, cand) {
    const id = cand.ids?.[0];
    if (!id) throw new Error(silerT("feedNoYouTubeVideo"));
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
    const reasons = [];
    for (const answer of r.clients) {
      const status = answer.pr?.playabilityStatus;
      if (!answer.pr || status?.status !== "OK") {
        if (status?.reason) reasons.push(status.reason);
        continue;
      }
      const fmts = Siler.youtube.usable(answer.pr.streamingData?.adaptiveFormats);
      const client = Siler.youtube.CLIENTS[answer.name];
      if (!fmts.length) continue;
      const ua = client.context.client.userAgent;
      // Behind an ad the links answer only once its time has passed (yt-dlp sleeps for it
      // too); the wait is taken here, so the app needn't take it again.
      const wait = Siler.youtube.adWait(answer.pr);
      let waited = false;
      if (!(await linkWorks(fmts[0].url, ua))) {
        if (!wait) continue;
        await new Promise((ok) => setTimeout(ok, wait * 1000));
        waited = true;
        if (!(await linkWorks(fmts[0].url, ua))) continue;
      }
      // The tab's title names the video only while its address does.
      const tabTitle = idFromUrl(tab.url || "") === id ? pageTitle(tab.title) : null;
      const out = Siler.youtube.handOver(answer, client, Siler.youtube.watchUrl(id), r.web, tabTitle);
      if (out.error) throw new Error(out.error);
      if (waited) out.item.media.not_before_ms = null;
      console.info(`Siler: YouTube ${id} via the ${answer.name} client`);
      return out.item;
    }
    if (reasons.length) throw new Error(silerT("feedYouTubeSays", [reasons[0]]));
    if (r.solverError) throw new Error(silerT("feedSolverFailed", [String(r.solverError)]));
    throw new Error(silerT("feedNoFormats"));
  }

  // ------------------------------------------------------------ hand-over

  /**
   * Whether the app understands video details (protocol 2). Asked again once when the last
   * answer is unknown; an app that isn't running yet is started by the hand-over, and an
   * older one ignores details it doesn't know.
   */
  async function appTakesHints() {
    let h = await refreshHealth();
    if (h.protocol == null) h = await refreshHealth(true);
    if (h.protocol != null) return h.protocol >= 2;
    return !!h.installed;
  }

  /** The Siler item for a candidate. */
  async function itemFor(tab, c) {
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
    const title = c.site ? (videoPage && pageTitle(tab.title)) || c.title || pageTitle(tab.title) : null;
    const filename = c.filename || (title && c.kind === "file" ? `${safeName(title)}.mp4` : null);
    const item = await describe(c.url, {
      referrer: c.referrer || tab.url,
      pageUrl: tab.url,
      filename,
      size: c.size || null,
      mime: c.mime || null,
      storeId: c.storeId || tab.cookieStoreId,
    });
    if (hints && (c.hint || c.site)) item.media = { ...(c.hint || {}), site: c.site || c.hint?.site || null, title: title || c.hint?.title || null };
    return item;
  }

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

  /** What the popup shows: no links, cookies or manifests. */
  const view = (c) => ({
    key: c.key,
    kind: c.kind,
    site: c.site || null,
    title: c.title || null,
    label: label(c),
    name: c.title || nameOfUrl(c.url),
  });

  function nameOfUrl(url) {
    try {
      const u = new URL(url);
      return decodeURIComponent(u.pathname.split("/").filter(Boolean).pop() || u.host);
    } catch {
      return url;
    }
  }

  /** Lists a tab's videos for the popup; with `fresh`, after looking at its pages again. */
  async function forPopup(tabId, fresh = false) {
    const tab = await browser.tabs.get(tabId);
    if (fresh) await scan(tab);
    else noticeYouTube(tab.id, tab.url, tab.title);
    const items = await list(tabId);
    // The video the address names, other recognised videos, then streams, then files.
    const here = idFromUrl(tab.url);
    const rank = (c) => (here && c.ids?.includes(here) ? -1 : c.site ? 0 : c.kind === "file" ? 2 : 1);
    return [...items].sort((a, b) => rank(a) - rank(b)).map(view);
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
      const item = await describe(src, { referrer: frameUrl || pageUrl, pageUrl, storeId: tab.cookieStoreId });
      // On a recognised site, the page names the video.
      const title = site && recogniserOn(site.id) ? pageTitle(tab.title) : null;
      if (title) {
        item.filename = `${safeName(title)}.mp4`;
        if (await appTakesHints()) item.media = { site: site.id, title };
      }
      return item;
    }
    const items = await list(tab.id);
    // Masters before media playlists, newest first otherwise.
    const streams = items.filter((c) => c.kind === "hls" || c.kind === "dash").sort((a, b) => (b.master ? 1 : 0) - (a.master ? 1 : 0));
    const hit =
      streams.find((c) => c.frameId === frameId) ||
      streams[0] ||
      items.find((c) => c.kind === "file" && c.frameId === frameId) ||
      items.find((c) => c.site);
    if (!hit) throw new Error(silerT("feedNoVideoYet"));
    return itemFor(tab, hit);
  }

  /** Chromium: a site API's answer copied from the page by hook.js. */
  function fromPage(tab, frameId, url, text, pageUrl) {
    if (!tab?.id || typeof url !== "string" || typeof text !== "string" || text.length > BODY_MAX) return;
    // Only the addresses Firefox would read too.
    if (!BODY_URLS.some((p) => new RegExp("^" + p.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*") + "$").test(url))) return;
    fromSiteAnswer(tab.id, frameId ?? 0, url, text, pageUrl || tab.url, tab.cookieStoreId);
  }

  return { forPopup, download, forVideo, scan, list, add, clear, fromPage };
})();
