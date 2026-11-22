// Player libraries and page metadata: where a publisher's page keeps its video's address and
// title when the player hasn't fetched it yet, or names it better than its address does.
//  - silerPlayerProbe runs in the page's own world (scripting.executeScript, world MAIN, on
//    demand from the feed's page scan). It only reads the players' public registries; it adds
//    no listener, wraps nothing and leaves the page as it was.
//  - Siler.players turns what the probe and the isolated-world page reader found into feed
//    candidates. Plain functions, so the tests load this file in Node.
"use strict";

/**
 * What the page's players say they play: `[{player, title, poster, sources: [{url, type,
 * label}], tracks: [{url, lang, label}]}]`. `win` is the page's window (a stand-in in tests).
 * Each probe is `{id, present(win), read(win)}`; a probe that throws is skipped.
 */
function silerPlayerProbe(win = globalThis) {
  const MAX_PLAYERS = 20;
  const MAX_SOURCES = 40;
  const str = (v) => (typeof v === "string" && v.length < 4096 ? v : null);
  const source = (url, type, label) => (str(url) ? { url: str(url), type: str(type), label: str(label) || (Number.isFinite(label) ? `${label}p` : null) } : null);
  const track = (url, lang, label, kind) =>
    str(url) && (!kind || kind === "captions" || kind === "subtitles") ? { url: str(url), lang: str(lang), label: str(label) } : null;
  const list = (v) => (Array.isArray(v) ? v.slice(0, MAX_SOURCES) : []);

  const PROBES = [
    {
      // JW Player: every instance is a .jwplayer element whose id names it.
      id: "jwplayer",
      present: (w) => typeof w.jwplayer === "function",
      read: (w) => {
        const out = [];
        const ids = [...w.document.querySelectorAll(".jwplayer[id], [id^='jwplayer']")].map((e) => e.id).slice(0, MAX_PLAYERS);
        for (const id of ids.length ? ids : [undefined]) {
          const p = w.jwplayer(id);
          if (!p || typeof p.getPlaylist !== "function") continue;
          for (const item of list(p.getPlaylist())) {
            const sources = list(item.sources).map((s) => source(s.file, s.type, s.label));
            if (!sources.length && item.file) sources.push(source(item.file, item.type, null));
            out.push({
              player: "jwplayer",
              title: str(item.title),
              poster: str(item.image),
              sources: sources.filter(Boolean),
              tracks: list(item.tracks).map((t) => track(t.file, t.language || t.srclang, t.label, t.kind)).filter(Boolean),
            });
          }
        }
        return out;
      },
    },
    {
      // Video.js: its registry of players; Brightcove's adds `mediainfo`.
      id: "videojs",
      present: (w) => typeof w.videojs === "function",
      read: (w) => {
        const v = w.videojs;
        const players = typeof v.getPlayers === "function" ? Object.values(v.getPlayers() || {}) : Object.values(v.players || {});
        return players
          .filter(Boolean)
          .slice(0, MAX_PLAYERS)
          .map((p) => {
            const info = p.mediainfo || {};
            let sources = typeof p.currentSources === "function" ? list(p.currentSources()) : [];
            if (!sources.length && typeof p.currentSrc === "function") sources = [{ src: p.currentSrc(), type: p.currentType?.() }];
            const tracks = typeof p.remoteTextTracks === "function" ? list([...(p.remoteTextTracks() || [])]) : [];
            return {
              player: "videojs",
              title: str(info.name),
              poster: str(typeof p.poster === "function" ? p.poster() : null) || str(info.poster),
              sources: sources.map((s) => source(s.src, s.type, null)).filter(Boolean),
              tracks: tracks.map((t) => track(t.src, t.language, t.label, t.kind)).filter(Boolean),
            };
          });
      },
    },
    {
      // Flowplayer (7 and the native 2.x/3.x player): its instance list.
      id: "flowplayer",
      present: (w) => !!w.flowplayer && Array.isArray(w.flowplayer.instances),
      read: (w) =>
        w.flowplayer.instances.slice(0, MAX_PLAYERS).map((p) => {
          const clip = p.video || p.conf?.clip || p.opts || {};
          const raw = list(clip.sources).length ? list(clip.sources) : [{ src: clip.src || p.src, type: clip.type }];
          return {
            player: "flowplayer",
            title: str(clip.title) || str(p.opts?.title),
            poster: str(clip.poster) || str(p.opts?.poster),
            sources: raw.map((s) => source(typeof s === "string" ? s : s.src, s.type, null)).filter(Boolean),
            tracks: list(clip.subtitles).map((t) => track(t.src, t.srclang, t.label, t.kind)).filter(Boolean),
          };
        }),
    },
  ];

  const found = [];
  for (const probe of PROBES) {
    try {
      if (!probe.present(win)) continue;
      for (const p of probe.read(win)) if (p.sources.length || p.title) found.push(p);
    } catch {
      /* a player we don't understand, or a page that guards it */
    }
    if (found.length >= MAX_PLAYERS) break;
  }
  return { url: String(win.location?.href || ""), players: found.slice(0, MAX_PLAYERS) };
}

var Siler = globalThis.Siler || (globalThis.Siler = {});

Siler.players = (() => {
  const { keyOf, rawExt, bareType } = Siler.media;
  const FILE_EXTS = new Set(["mp4", "m4v", "webm", "mkv", "mov", "m4a", "mp3", "ogg", "ogv", "oga", "opus", "flac", "wav", "aac", "flv"]);

  function absolute(url, base) {
    try {
      const u = new URL(url, base);
      return /^https?:$/.test(u.protocol) ? u.href : null;
    } catch {
      return null;
    }
  }

  /** hls, dash, file, or null, from an address and a declared type. */
  function kindOf(url, type) {
    const t = bareType(type);
    const ext = rawExt(url);
    if (ext === "m3u8" || /mpegurl/.test(t)) return "hls";
    if (ext === "mpd" || t === "application/dash+xml") return "dash";
    if (FILE_EXTS.has(ext) || /^(video|audio)\//.test(t)) return "file";
    return null;
  }

  const heightOf = (label) => {
    const m = /(\d{3,4})\s*p\b/i.exec(label || "");
    return m ? Number(m[1]) : null;
  };

  const tidy = (s, n = 150) => (typeof s === "string" ? s.replace(/\s+/g, " ").trim().slice(0, n) || null : null);

  /** Candidates from one frame's probe answer (silerPlayerProbe). */
  function fromPlayers(answer, frameId = 0) {
    const out = [];
    const base = answer?.url || undefined;
    for (const p of answer?.players || []) {
      for (const s of p.sources || []) {
        const url = absolute(s.url, base);
        const kind = url && kindOf(url, s.type);
        if (!kind) continue;
        out.push({
          key: keyOf(url),
          url,
          kind,
          title: tidy(p.title),
          poster: absolute(p.poster, base),
          height: heightOf(s.label),
          referrer: base || null,
          frameId,
          from: p.player,
        });
      }
    }
    return out;
  }

  /** Subtitle files the players name, for any stream of the page. */
  function tracksOf(answer) {
    const base = answer?.url || undefined;
    const out = [];
    for (const p of answer?.players || []) {
      for (const t of p.tracks || []) {
        const url = absolute(t.url, base);
        if (url) out.push({ url, lang: tidy(t.lang, 20), label: tidy(t.label, 60) });
      }
    }
    return out;
  }

  /** The VideoObjects in a JSON-LD document (a graph, a list, or one object). */
  function videoObjects(doc, out = [], depth = 0) {
    if (!doc || typeof doc !== "object" || depth > 6 || out.length > 20) return out;
    if (Array.isArray(doc)) {
      for (const d of doc) videoObjects(d, out, depth + 1);
      return out;
    }
    const type = [].concat(doc["@type"] || []);
    if (type.includes("VideoObject")) out.push(doc);
    for (const k of ["@graph", "video", "mainEntity", "subjectOf", "hasPart"]) if (doc[k]) videoObjects(doc[k], out, depth + 1);
    return out;
  }

  const firstString = (v) => (typeof v === "string" ? v : Array.isArray(v) ? v.find((x) => typeof x === "string") || v[0]?.url || null : v?.url || null);

  /** ISO 8601 duration ("PT1H2M3S") in seconds, or 0. */
  function seconds(iso) {
    const m = /^P(?:(\d+)D)?(?:T(?:(\d+)H)?(?:(\d+)M)?(?:([\d.]+)S)?)?$/.exec(iso || "");
    if (!m) return 0;
    return (Number(m[1] || 0) * 86400 + Number(m[2] || 0) * 3600 + Number(m[3] || 0) * 60 + Number(m[4] || 0)) || 0;
  }

  /**
   * Candidates and titles from a frame's metadata (OpenGraph, Twitter cards, JSON-LD
   * VideoObjects, <video>/<track> elements), as collectPageData reads it in the isolated
   * world. Returns `{found, titles: {key → title}, tracks}`; a VideoObject's or og:video's
   * address that isn't a video file or manifest only lends its title.
   */
  function fromMeta(meta, frameId = 0) {
    const base = meta?.url || undefined;
    const found = [];
    const titles = {};
    const pageTitle = tidy(meta?.og?.title) || tidy(meta?.title);
    const add = (url, type, title, extra = {}) => {
      const abs = absolute(url, base);
      if (!abs) return;
      const kind = kindOf(abs, type);
      const key = keyOf(abs);
      if (title) titles[key] = title;
      if (!kind) return;
      found.push({ key, url: abs, kind, title: title || null, referrer: base || null, frameId, from: "meta", ...extra });
    };
    const og = meta?.og || {};
    for (const url of [og.videoSecure, og.video, og.twitterStream].filter(Boolean)) {
      add(url, og.videoType, pageTitle, { poster: absolute(og.image, base) });
    }
    for (const doc of meta?.jsonld || []) {
      for (const v of videoObjects(doc)) {
        const title = tidy(v.name);
        const extra = { poster: absolute(firstString(v.thumbnailUrl), base), duration: seconds(v.duration) || undefined };
        if (typeof v.contentUrl === "string") add(v.contentUrl, v.encodingFormat, title, extra);
        else if (typeof v.embedUrl === "string" && title) titles[keyOf(absolute(v.embedUrl, base) || "")] = title;
      }
    }
    for (const v of meta?.videos || []) {
      for (const src of v.sources || []) add(src.url, src.type, pageTitle, { poster: absolute(v.poster, base), height: v.height || null });
    }
    const tracks = [];
    for (const t of meta?.tracks || []) {
      const url = absolute(t.url, base);
      if (url) tracks.push({ url, lang: tidy(t.lang, 20), label: tidy(t.label, 60) });
    }
    return { found, titles, tracks, poster: absolute(og.image, base) };
  }

  return { fromPlayers, tracksOf, fromMeta, kindOf, seconds };
})();
