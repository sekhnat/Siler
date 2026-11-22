// Expired links: what a hand-over records about where its link came from, and finding a
// fresh one when Siler asks. Siler asks over a native port that stays open; the page is
// opened in an inactive, muted tab of the same container, looked at, and closed again.
// Nothing is clicked or submitted, and private windows are neither recorded nor used.
// Loaded before background.js, whose helpers it uses.
"use strict";

const Links = (() => {
  const { siteOf, idFromUrl } = Siler.sites;
  /** A little less than Siler waits for one tab. */
  const TAB_LIMIT = 55_000;
  const LOOK_EVERY = 1500;
  /** A page still loading after this is looked at as far as it got. */
  const LOAD_PATIENCE = 12_000;
  /** Only then is an unchanged address taken (it can work again with fresh cookies). */
  const SAME_AFTER = 6000;
  const BEAT_EVERY = 20_000;
  const RECONNECT = 60_000;
  /** An older host or app: asked again later. */
  const RETRY_OLDER = 5 * 60_000;
  const CLICK_KEPT = 120_000;
  const REMEMBERED = 200;
  /** Endings that name a script, not the file it serves. */
  const SCRIPT_EXTS = new Set(["php", "asp", "aspx", "ashx", "cgi", "jsp", "do", "action", "dll", "pl", "py"]);

  const isWeb = (url) => /^https?:\/\//i.test(url || "");
  const tidy = (text) => (text || "").replace(/\s+/g, " ").trim().slice(0, 200);
  const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

  /** An address without its query and fragment, where tokens usually are. */
  function bare(url) {
    try {
      const u = new URL(url);
      return `${u.origin}${u.pathname}`;
    } catch {
      return null;
    }
  }

  function lastPart(url) {
    try {
      const part = new URL(url).pathname.split("/").pop() || "";
      try {
        return decodeURIComponent(part);
      } catch {
        return part;
      }
    } catch {
      return "";
    }
  }

  const endingOf = (name) => (/\.([a-z0-9]{1,8})$/i.exec(name || "")?.[1] ?? "").toLowerCase();

  function remember(map, key, value) {
    map.delete(key);
    map.set(key, value);
    if (map.size > REMEMBERED) map.delete(map.keys().next().value);
  }

  // ------------------------------------------------------------ where a link came from

  let idPromise = null;
  function browserId() {
    idPromise ??= browser.storage.local.get("browserId").then(async ({ browserId: known }) => {
      if (known) return known;
      const id = crypto.randomUUID();
      await browser.storage.local.set({ browserId: id });
      return id;
    });
    return idPromise.catch((e) => {
      idPromise = null;
      throw e;
    });
  }

  const clicks = new Map();

  /** A link the user clicked (content.js). */
  function clicked(msg, sender) {
    if (!sender.tab || sender.tab.incognito || !isWeb(msg.url)) return;
    remember(clicks, msg.url, { text: tidy(msg.text), page: sender.tab.url || msg.page || null, at: Date.now() });
  }

  /** The click that led to one of these addresses. */
  function clickOn(...urls) {
    for (const url of urls) {
      const c = clicks.get(url);
      if (c && Date.now() - c.at < CLICK_KEPT) return c;
    }
    return null;
  }

  // The start of each request's redirects. A form's address can't be asked again as a link.
  const chains = new Map();
  browser.webRequest.onBeforeRedirect.addListener(
    (d) => {
      if (chains.has(d.requestId)) return;
      remember(chains, d.requestId, { url: !d.method || d.method === "GET" ? d.url : null });
    },
    { urls: ["<all_urls>"], types: ["main_frame", "sub_frame"] },
  );
  const firstOf = (requestId) => chains.get(requestId)?.url ?? null;

  /**
   * What goes along with a hand-over to find its link again; null for private windows.
   * `first` is where the redirects to `url` started.
   */
  async function source({ url, first = null, storeId = null, incognito = false, text = null, name = null, video = null }) {
    if (incognito || storeId === "firefox-private" || !isWeb(url)) return null;
    try {
      const link = { browser: await browserId() };
      if (SILER_GECKO && storeId) link.container = storeId;
      if (isWeb(first) && first !== url) link.first = { url: first, cookies: await cookieHeader(first, storeId) };
      const words = tidy(text ?? clickOn(first, url)?.text);
      link.locator = { path: bare(isWeb(first) ? first : url), text: words || null, name: name || null };
      if (video?.site && video.id) link.video = video;
      return link;
    } catch (e) {
      // The hand-over matters more than finding it again.
      console.warn("Siler: where a link came from couldn't be recorded", e);
      return null;
    }
  }

  // ------------------------------------------------------------ which link it is

  /** The one link on a page that is the expired one, renewed; null unless just one is. */
  function pick(task, found) {
    const loc = task.locator || {};
    const name = loc.name || task.filename || null;
    const words = tidy(loc.text);
    const byUrl = new Map();
    for (const l of found) {
      if (!isWeb(l?.url)) continue;
      const entry = byUrl.get(l.url) || { url: l.url, texts: new Set(), names: new Set([lastPart(l.url)]) };
      if (l.text) entry.texts.add(tidy(l.text));
      if (l.download) entry.names.add(l.download);
      byUrl.set(l.url, entry);
    }
    const all = [...byUrl.values()];
    const named = (l) => !!name && (l.names.has(name) || l.texts.has(name));
    const worded = (l) => !!words && l.texts.has(words);
    const only = (list) => {
      let left = list;
      for (const test of [named, worded]) {
        if (left.length < 2) break;
        const fewer = left.filter(test);
        if (fewer.length) left = fewer;
      }
      return left.length === 1 ? { url: left[0].url, text: worded(left[0]) ? words : ([...left[0].texts][0] ?? null) } : null;
    };

    const paths = new Set([loc.path, bare(task.url)].filter(Boolean));
    const samePath = all.filter((l) => paths.has(bare(l.url)));
    if (samePath.length) return only(samePath);
    const sameName = all.filter(named);
    if (sameName.length) return only(sameName);
    // By its words alone, a link that names another kind of file isn't it.
    const wanted = endingOf(name);
    const sameWords = all.filter((l) => {
      if (!worded(l)) return false;
      const ending = endingOf(lastPart(l.url));
      return !ending || !wanted || ending === wanted || SCRIPT_EXTS.has(ending);
    });
    return only(sameWords);
  }

  // Runs in each frame of the tab Siler opened (isolated world). It only reads.
  function silerLinksInPage() {
    const out = [];
    const tidy = (text) => (text || "").replace(/\s+/g, " ").trim().slice(0, 200);
    for (const el of document.querySelectorAll("a[href], area[href], video[src], audio[src], video source[src], audio source[src]")) {
      const url = el.href || el.src;
      if (typeof url !== "string" || !/^https?:/i.test(url)) continue;
      const text = tidy(el.textContent) || tidy(el.getAttribute("aria-label") || el.getAttribute("title"));
      out.push({ url, text, download: el.getAttribute("download") || "" });
      if (out.length >= 5000) break;
    }
    return out;
  }

  async function linksIn(tabId) {
    const read = browser.scripting.executeScript({ target: { tabId, allFrames: true }, func: silerLinksInPage }).catch(() => []);
    const frames = await Promise.race([read, sleep(5000).then(() => [])]);
    return (frames || []).flatMap((f) => (Array.isArray(f?.result) ? f.result : []));
  }

  /** The page to open: for a recognised video, that video's own where that can be told. */
  function pageFor(task) {
    const { site, id } = task.video || {};
    if (!site || !id) return task.page;
    if (site === "youtube" && /^[\w-]{11}$/.test(id)) return Siler.youtube.watchUrl(id);
    if (idFromUrl(task.page) === id || siteOf(task.page)?.id !== site) return task.page;
    if (site === "instagram" && /^(?!\d+$)[\w-]+$/.test(id)) return `https://www.instagram.com/p/${id}/`;
    if (site === "facebook" && /^\d+$/.test(id)) return `https://www.facebook.com/watch/?v=${id}`;
    if (site === "vimeo" && /^\d+$/.test(id)) return `https://vimeo.com/${id}`;
    return task.page;
  }

  // ------------------------------------------------------------ the tabs

  const jobs = new Map();
  const jobOf = (tabId) => [...jobs.values()].find((j) => j.tabId != null && j.tabId === tabId) || null;

  function saveTabs() {
    const open = [...jobs.values()].map((j) => j.tabId).filter((id) => id != null);
    return browser.storage.session.set({ linkTabs: open }).catch(() => {});
  }

  /** Tabs that a background which was stopped left open. */
  async function closeLeftovers() {
    const { linkTabs = [] } = await browser.storage.session.get("linkTabs").catch(() => ({}));
    for (const id of linkTabs) await browser.tabs.remove(id).catch(() => {});
    if (linkTabs.length) await browser.storage.session.remove("linkTabs").catch(() => {});
  }

  async function closeTab(job) {
    const id = job.tabId;
    if (id == null) return;
    job.tabId = null;
    await browser.tabs.remove(id).catch(() => {});
    await saveTabs();
  }

  /** A download that began in a tab of ours: the answer, if it is the file asked for. */
  function offer(job, { url, first = null, filename = null, size = null }) {
    const named = [url, first].filter(Boolean).map((u) => ({ url: u, text: "", download: filename }));
    const sameSize = !!size && size === job.task.size;
    if (!sameSize && !named.some((l) => pick(job.task, [l]))) return false;
    job.offered = { url, first, size, filename };
    return true;
  }

  /** Chromium doesn't say which tab a download came from: its page and file do. */
  function claim(item) {
    for (const job of jobs.values()) {
      if (job.tabId == null || !item.referrer || silerHostOf(item.referrer) !== silerHostOf(job.page)) continue;
      const found = { url: item.finalUrl || item.url, first: item.url, filename: basename(item.filename), size: item.totalBytes > 0 ? item.totalBytes : null };
      if (offer(job, found)) return true;
    }
    return false;
  }

  /** The hand-over for a link found in the job's tab. */
  function itemFor(job, tab, { url, text = null, first = null, filename = null, size = null }) {
    const { task } = job;
    const storeId = tab.cookieStoreId || task.container || null;
    return describe(url, {
      referrer: tab.url || job.page,
      pageUrl: task.page,
      filename,
      size,
      storeId,
      from: { first, text, name: task.locator?.name || task.filename || filename, incognito: false, video: task.video },
    });
  }

  async function lookIn(job, tab) {
    const { task } = job;
    if (task.video?.site || task.media) {
      const item = await Feed.refind(tab, task).catch((e) => {
        job.why = reasonOf(e);
        return null;
      });
      // A recognised video is only ever that video.
      if (item || task.video?.site) return item;
    }
    const links = await linksIn(tab.id);
    if (!task.media) {
      const played = await Feed.list(tab.id);
      for (const c of played) if (c.kind === "file" && !c.site) links.push({ url: c.url, text: "" });
    }
    const found = pick(task, links);
    return found ? itemFor(job, tab, found) : null;
  }

  async function windowToUse() {
    const normal = (await browser.windows.getAll({ windowTypes: ["normal"] })).filter((w) => !w.incognito);
    return normal.find((w) => w.focused) || normal[0] || null;
  }

  async function find(job) {
    const { task } = job;
    if (!isWeb(job.page)) throw new Error("the page isn't a web page");
    if (task.container === "firefox-private") throw new Error("private windows aren't used");
    const win = await windowToUse();
    if (!win) throw new Error("no normal browser window is open");
    if (job.cancelled) throw new Error("cancelled");
    const props = { url: job.page, active: false, windowId: win.id };
    if (SILER_GECKO && task.container) props.cookieStoreId = task.container;
    const opened = await browser.tabs.create(props);
    job.tabId = opened.id;
    await saveTabs();
    await browser.tabs.update(opened.id, { muted: true }).catch(() => {});
    if (job.cancelled) throw new Error("cancelled");

    const started = Date.now();
    await sleep(500);
    while (!job.cancelled && Date.now() - started < TAB_LIMIT) {
      const tab = await browser.tabs.get(opened.id).catch(() => null);
      if (!tab || tab.incognito) throw new Error("the tab was closed");
      if (job.offered) return itemFor(job, tab, job.offered);
      const waited = Date.now() - started;
      if (tab.status === "complete" || waited > LOAD_PATIENCE) {
        const item = await lookIn(job, tab);
        if (job.offered) return itemFor(job, tab, job.offered);
        // A video's address is its page's: what is fresh is what comes with it.
        if (item && (item.url !== task.url || item.media?.manifest || waited > SAME_AFTER)) return item;
      }
      await sleep(LOOK_EVERY);
    }
    throw new Error(job.cancelled ? "cancelled" : job.why || "the page has no one link that matches");
  }

  async function answer(to, task) {
    const job = { task, page: pageFor(task), tabId: null, cancelled: false, offered: null, why: null };
    jobs.set(task.request, job);
    const reply = { type: "link_found", request: task.request };
    try {
      reply.item = await find(job);
    } catch (e) {
      reply.error = reasonOf(e);
    }
    await closeTab(job);
    jobs.delete(task.request);
    if (port === to && !job.cancelled) to.postMessage(reply);
  }

  function cancel(request) {
    const job = jobs.get(request);
    if (!job) return;
    job.cancelled = true;
    closeTab(job);
  }

  // ------------------------------------------------------------ the port

  let port = null;
  let beating = null;
  let again = null;
  let connecting = false;
  let wanted = true;

  function drop() {
    const old = port;
    port = null;
    if (beating != null) clearInterval(beating);
    beating = null;
    for (const request of jobs.keys()) cancel(request);
    return old;
  }

  function later(ms) {
    if (again != null) clearTimeout(again);
    again = setTimeout(() => {
      again = null;
      connect();
    }, ms);
  }

  function received(from, msg) {
    if (from !== port) return;
    switch (msg?.type) {
      case "subscribed":
      case "waiting":
        // Only now: to an older host a beat is a request to start Siler for.
        beating ??= setInterval(() => port?.postMessage({ type: "beat" }), BEAT_EVERY);
        return;
      case "beat":
        return;
      case "find_link":
        answer(from, msg);
        return;
      case "cancel_find":
        cancel(msg.request);
        return;
      default:
        // An older host or app: it answers the ping like any other.
        drop()?.disconnect();
        later(RETRY_OLDER);
    }
  }

  async function connect() {
    if (port || connecting || typeof browser.runtime.connectNative !== "function") return;
    connecting = true;
    try {
      wanted = (await silerConfig()).refreshLinks;
      if (!wanted) return;
      const id = await browserId();
      if (!wanted) return;
      const opened = browser.runtime.connectNative(SILER_HOST);
      port = opened;
      opened.onMessage.addListener((msg) => received(opened, msg));
      opened.onDisconnect.addListener(() => {
        void browser.runtime.lastError; // no host installed, say: nothing to report
        if (port !== opened) return;
        drop();
        if (wanted) later(RECONNECT);
      });
      opened.postMessage({
        type: "ping",
        extension_version: browser.runtime.getManifest().version,
        protocol: SILER_PROTOCOL,
        subscribe: { browser: id },
      });
    } catch (e) {
      console.warn("Siler: no connection for refreshing links", e);
      drop();
    } finally {
      connecting = false;
    }
  }

  browser.storage.onChanged.addListener((changes, where) => {
    if (where !== "local" || !("refreshLinks" in changes)) return;
    wanted = changes.refreshLinks.newValue ?? SILER_DEFAULTS.refreshLinks;
    if (wanted) connect();
    else drop()?.disconnect();
  });

  // After the scripts that follow have loaded: it uses their helpers.
  setTimeout(() => closeLeftovers().then(connect), 0);

  return { source, clicked, clickOn, firstOf, jobOf, offer, claim, connect, pick };
})();
