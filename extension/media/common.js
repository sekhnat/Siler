// Helpers shared by the video feed and the site recognisers. Plain functions with no browser
// API use, so the tests can load them in Node.
"use strict";

var Siler = globalThis.Siler || (globalThis.Siler = {});

Siler.media = (() => {
  const HLS_TYPES = /^(application\/(vnd\.apple\.mpegurl|x-mpegurl)|audio\/(x-)?mpegurl)$/;
  const DASH_TYPES = /^application\/dash\+xml$/;
  // Files that are whole videos or songs (not pieces of a stream).
  const FILE_EXTS = new Set(["mp4", "m4v", "webm", "mkv", "mov", "m4a", "mp3", "ogg", "ogv", "oga", "opus", "flac", "wav", "aac", "flv", "avi", "wmv"]);
  // Pieces of a stream.
  const PIECE_EXTS = new Set(["ts", "m4s", "m4f", "cmfv", "cmfa", "vtt", "webvtt", "key", "init", "mp4a"]);
  const MIN_FILE = 512 * 1024;

  function extOf(url) {
    try {
      const last = new URL(url).pathname.split("/").pop() || "";
      const m = last.match(/\.([a-z0-9]{1,5})$/i);
      return m ? m[1].toLowerCase() : "";
    } catch {
      return "";
    }
  }

  /** The total size from a `Content-Range` header ("bytes 0-99/1234"). */
  function rangeTotal(cr) {
    const m = /\/(\d+)\s*$/.exec(cr || "");
    return m ? Number(m[1]) : null;
  }

  function rangeStart(cr) {
    const m = /bytes\s+(\d+)-/.exec(cr || "");
    return m ? Number(m[1]) : null;
  }

  /**
   * What a response the page fetched is, as a video candidate: an HLS or DASH manifest, a
   * whole media file, or nothing. `r` = {url, status, mime, length, contentRange, type}.
   */
  function classify(r) {
    let u;
    try {
      u = new URL(r.url);
    } catch {
      return null;
    }
    if (!/^https?:$/.test(u.protocol)) return null;
    if (r.status < 200 || r.status >= 300) return null;
    const host = u.hostname.toLowerCase();
    // YouTube's stream pieces: its recogniser finds YouTube videos.
    if (host.endsWith("googlevideo.com") || u.pathname.endsWith("/videoplayback")) return null;
    const mime = (r.mime || "").split(";")[0].trim().toLowerCase();
    const ext = extOf(r.url);
    if (ext === "m3u8" || HLS_TYPES.test(mime)) return { kind: "hls" };
    if (ext === "mpd" || DASH_TYPES.test(mime)) return { kind: "dash" };
    if (PIECE_EXTS.has(ext) || mime === "video/mp2t" || mime === "video/iso.segment") return null;
    const media = /^(video|audio)\//.test(mime) || FILE_EXTS.has(ext);
    if (!media) return null;
    // Only the first part of a file read in ranges, and not pieces named by query.
    if (r.status === 206) {
      const start = rangeStart(r.contentRange);
      if (start !== null && start > 0) return null;
    }
    if (/[?&](bytestart|byteend|range|sq)=/.test(u.search)) return null;
    const size = rangeTotal(r.contentRange) ?? (r.length > 0 ? r.length : null);
    if (size !== null && size < MIN_FILE) return null;
    return { kind: "file", size, mime: mime || null };
  }

  /** A key that is the same for the same video whatever its signed query says. */
  function keyOf(url) {
    try {
      const u = new URL(url);
      return `${u.host}${u.pathname}`;
    } catch {
      return url;
    }
  }

  /** What an HLS or DASH manifest's text says: master or media playlist, and the best height. */
  function inspectManifest(kind, text) {
    if (kind === "hls") {
      if (!/^﻿?\s*#EXTM3U/.test(text)) return null;
      const master = /#EXT-X-STREAM-INF/.test(text);
      let height = 0;
      for (const m of text.matchAll(/RESOLUTION=\d+x(\d+)/g)) height = Math.max(height, Number(m[1]));
      // The playlists a master lists (variants, audio and subtitle renditions).
      const children = [];
      if (master) {
        for (const line of text.split(/\r?\n/)) {
          const l = line.trim();
          if (l && !l.startsWith("#")) children.push(l);
          for (const m of l.matchAll(/URI="([^"]+)"/g)) children.push(m[1]);
        }
      }
      return { master, height: height || null, live: !master && !/#EXT-X-ENDLIST/.test(text), children };
    }
    if (!/<MPD[\s>]/.test(text)) return null;
    let height = 0;
    for (const m of text.matchAll(/\bheight="(\d+)"/g)) height = Math.max(height, Number(m[1]));
    return { master: true, height: height || null, live: /type="dynamic"/.test(text), children: [] };
  }

  /**
   * The JSON value that starts at the first `{` or `[` after `marker` in `text` (a script
   * assigning a config, like `window.playerConfig = {...}`), or null.
   */
  function jsonAfter(text, marker, max = 8 * 1024 * 1024) {
    const at = text.indexOf(marker);
    if (at < 0) return null;
    let i = at + marker.length;
    while (i < text.length && text[i] !== "{" && text[i] !== "[") {
      if (!/[\s=:(]/.test(text[i])) return null;
      i++;
    }
    const start = i;
    let depth = 0;
    let str = null;
    for (; i < text.length && i - start < max; i++) {
      const c = text[i];
      if (str) {
        if (c === "\\") i++;
        else if (c === str) str = null;
        continue;
      }
      if (c === '"' || c === "'") str = c;
      else if (c === "{" || c === "[") depth++;
      else if (c === "}" || c === "]") {
        depth--;
        if (depth === 0) {
          try {
            return JSON.parse(text.slice(start, i + 1));
          } catch {
            return null;
          }
        }
      }
    }
    return null;
  }

  /** Every JSON document in a response body: one, or one per line (Facebook's GraphQL). */
  function parseJsonish(text) {
    const t = text.replace(/^\)\]\}'?\s*/, "").replace(/^for \(;;\);/, "");
    try {
      return [JSON.parse(t)];
    } catch {
      const out = [];
      for (const line of t.split("\n")) {
        const l = line.trim();
        if (!l || (l[0] !== "{" && l[0] !== "[")) continue;
        try {
          out.push(JSON.parse(l));
        } catch {
          /* not JSON */
        }
      }
      return out;
    }
  }

  /** Calls `visit(obj)` for every object inside `root`, depth-first, within limits. */
  function walk(root, visit, limits = { nodes: 200000, depth: 64 }) {
    let nodes = 0;
    const stack = [[root, 0]];
    while (stack.length) {
      const [v, d] = stack.pop();
      if (!v || typeof v !== "object" || d > limits.depth) continue;
      if (++nodes > limits.nodes) return;
      if (!Array.isArray(v)) visit(v);
      const values = Array.isArray(v) ? v : Object.values(v);
      for (let i = values.length - 1; i >= 0; i--) {
        if (values[i] && typeof values[i] === "object") stack.push([values[i], d + 1]);
      }
    }
  }

  /** A short label for a candidate: "HLS · up to 1080p", "MP4 · 24 MB". */
  function label(c) {
    const parts = [];
    if (c.live) parts.push("Live: recorded until you press Finish");
    else if (c.kind === "hls") parts.push("HLS stream");
    else if (c.kind === "dash") parts.push("DASH stream");
    else if (c.kind === "site") parts.push(c.siteName || "Video");
    else parts.push((c.ext || extOf(c.url) || "file").toUpperCase());
    if (c.height) parts.push(`up to ${c.height}p`);
    if (c.size) parts.push(`${(c.size / 1048576).toFixed(c.size < 10485760 ? 1 : 0)} MB`);
    return parts.join(" · ");
  }

  return { classify, keyOf, extOf, inspectManifest, jsonAfter, parseJsonish, walk, label, rangeTotal };
})();
