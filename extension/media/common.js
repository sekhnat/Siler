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

  // Every response a page fetches passes `interesting` first, so it uses string tests only.
  const MANIFEST_EXTS = new Set(["m3u8", "m3u", "mpd"]);
  const SUBTITLE_EXTS = new Set(["vtt", "webvtt", "srt", "ttml", "dfxp"]);
  const SUBTITLE_TYPES = /^(text\/vtt|application\/(ttml\+xml|x-subrip)|text\/srt)$/;
  // Content types that say nothing about what a response is.
  const VAGUE_TYPES = /^(|text\/plain|application\/octet-stream|binary\/octet-stream|application\/binary|application\/x-binary|application\/unknown)$/;

  /** The extension of a raw address's last path part, without parsing it. */
  function rawExt(url) {
    let end = url.length;
    const q = url.indexOf("?");
    if (q >= 0) end = q;
    const h = url.indexOf("#");
    if (h >= 0 && h < end) end = h;
    const slash = url.lastIndexOf("/", end - 1);
    const dot = url.lastIndexOf(".", end - 1);
    if (dot <= slash || end - dot > 6) return "";
    return url.slice(dot + 1, end).toLowerCase();
  }

  /** The bare content type: "Video/MP4; codecs=…" → "video/mp4". */
  function bareType(ct) {
    if (!ct) return "";
    const semi = ct.indexOf(";");
    return (semi >= 0 ? ct.slice(0, semi) : ct).trim().toLowerCase();
  }

  /**
   * Whether a response could matter to the feed at all: media, a manifest, a piece of a
   * stream, subtitles, or a response whose type says nothing. Most of a page's requests
   * (JSON, scripts, beacons) end here.
   */
  function interesting(url, mime) {
    const ext = rawExt(url);
    if (FILE_EXTS.has(ext) || PIECE_EXTS.has(ext) || MANIFEST_EXTS.has(ext) || SUBTITLE_EXTS.has(ext)) return true;
    const t = mime;
    if (t.startsWith("video/") || t.startsWith("audio/")) return true;
    if (HLS_TYPES.test(t) || DASH_TYPES.test(t) || SUBTITLE_TYPES.test(t)) return true;
    return VAGUE_TYPES.test(t);
  }

  /** A subtitle file (WebVTT, SRT, TTML) by its address or type. */
  function subtitleKind(url, mime) {
    const ext = rawExt(url);
    if (ext === "vtt" || ext === "webvtt" || mime === "text/vtt") return "vtt";
    if (ext === "srt" || mime === "application/x-subrip" || mime === "text/srt") return "srt";
    if (ext === "ttml" || ext === "dfxp" || mime === "application/ttml+xml") return "ttml";
    return null;
  }

  // ------------------------------------------------------------ content sniffing
  // The rules of the app's classifier (crates/siler-media/src/classify.rs): what a body is
  // from its first bytes, whatever its address and type say.

  const at = (b, i, s) => {
    for (let k = 0; k < s.length; k++) if (b[i + k] !== s.charCodeAt(k)) return false;
    return true;
  };
  const isTs = (b) => b.length >= 3 * 188 + 1 && b[0] === 0x47 && b[188] === 0x47 && b[376] === 0x47;
  const isAdts = (b) => b.length >= 2 && b[0] === 0xff && (b[1] & 0xf6) === 0xf0;
  const isMpegAudio = (b) => b.length >= 2 && b[0] === 0xff && (b[1] & 0xe0) === 0xe0 && (b[1] & 0x06) !== 0;

  /**
   * What a body's first bytes are: "hls", "dash", "vtt", "html", "file" (a whole video or
   * song), "piece" (a part of a stream), or null. `size` is the whole body's size when known:
   * an MP4, WebM or audio start is a whole file from MIN_FILE up, else a stream's first piece.
   */
  function sniff(bytes, size = null) {
    let b = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes || []);
    if (b[0] === 0xef && b[1] === 0xbb && b[2] === 0xbf) b = b.subarray(3);
    let i = 0;
    while (i < b.length && i < 64 && (b[i] === 0x20 || b[i] === 0x09 || b[i] === 0x0a || b[i] === 0x0d)) i++;
    const t = b.subarray(i);
    const head = new TextDecoder("latin1").decode(t.subarray(0, 512)).toLowerCase();
    if (at(t, 0, "#EXTM3U")) {
      const text = new TextDecoder("latin1").decode(t.subarray(0, 64 * 1024));
      return /^\s*#EXT(-X-|INF)/m.test(text.slice(7)) ? "hls" : null;
    }
    if (head.includes("<mpd") || head.includes("urn:mpeg:dash:schema:mpd")) return "dash";
    if (head.startsWith("<!doctype html") || head.includes("<html")) return "html";
    if (at(t, 0, "WEBVTT")) return "vtt";
    const whole = size == null || size >= MIN_FILE;
    if (t.length >= 8) {
      const box = String.fromCharCode(t[4], t[5], t[6], t[7]);
      if (box === "styp" || box === "moof" || box === "sidx") return "piece";
      if (box === "ftyp" || box === "moov" || box === "mdat") return whole ? "file" : "piece";
    }
    if (isTs(t)) return size != null && size >= 20 * 1024 * 1024 ? "file" : "piece";
    if (t[0] === 0x1f && t[1] === 0x43 && t[2] === 0xb6 && t[3] === 0x75) return "piece"; // a Matroska cluster
    if (t[0] === 0x1a && t[1] === 0x45 && t[2] === 0xdf && t[3] === 0xa3) return whole ? "file" : "piece";
    if (at(t, 0, "FLV\x01") || at(t, 0, "OggS") || at(t, 0, "RIFF") || (t[0] === 0x30 && t[1] === 0x26 && t[2] === 0xb2 && t[3] === 0x75)) return "file";
    if (at(t, 0, "ID3") || isAdts(t) || isMpegAudio(t)) return whole ? "file" : "piece";
    return null;
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

  // DRM systems as the app names them (crates/siler-media/src/protection.rs).
  const DRM_UUIDS = {
    "edef8ba9-79d6-4ace-a3c8-27dcd51d21ed": "Widevine",
    "9a04f079-9840-4286-ab92-e65be0885f95": "PlayReady",
    "94ce86fb-07ff-4f43-adb8-93d2fa968ca2": "FairPlay",
  };
  const keyFormatName = (f) => {
    const l = (f || "").toLowerCase();
    if (l === "com.apple.streamingkeydelivery") return "FairPlay";
    if (l === "com.microsoft.playready") return "PlayReady";
    if (l.startsWith("urn:uuid:")) return DRM_UUIDS[l.slice(9)] || "DRM";
    return "DRM";
  };
  const attr = (line, name) => new RegExp(`[:,]${name}=("([^"]*)"|[^,]*)`, "i").exec(line)?.[2] ?? new RegExp(`[:,]${name}=("([^"]*)"|[^,]*)`, "i").exec(line)?.[1] ?? null;
  const VIDEO_CODECS = /\b(avc[13]|hvc1|hev1|dvh[1e]|vp0?[89]|av01|mp4v)\b/i;

  /**
   * What an HLS or DASH manifest's text says: master or media playlist, the best height, its
   * variants, its length, whether it is audio only, has subtitles of its own, or is
   * DRM-protected (by the app's rules: SAMPLE-AES with a key format, any DASH
   * ContentProtection; AES-128 is not DRM).
   */
  function inspectManifest(kind, text) {
    if (kind === "hls") {
      if (!/^\uFEFF?\s*#EXTM3U/.test(text)) return null;
      const master = /#EXT-X-STREAM-INF/.test(text);
      let height = 0;
      let duration = 0;
      const children = [];
      const variants = [];
      const drm = new Set();
      let subs = false;
      let pending = null;
      for (const line of text.split(/\r?\n/)) {
        const l = line.trim();
        if (!l) continue;
        if (l.startsWith("#EXT-X-STREAM-INF")) {
          const res = /RESOLUTION=\d+x(\d+)/i.exec(l);
          const h = res ? Number(res[1]) : null;
          if (h) height = Math.max(height, h);
          pending = { height: h, bandwidth: Number(attr(l, "BANDWIDTH")) || null, codecs: attr(l, "CODECS") };
        } else if (l.startsWith("#EXTINF:")) {
          duration += parseFloat(l.slice(8)) || 0;
        } else if (l.startsWith("#EXT-X-KEY") || l.startsWith("#EXT-X-SESSION-KEY")) {
          const method = (attr(l, "METHOD") || "").toUpperCase();
          const format = attr(l, "KEYFORMAT") || "identity";
          const skd = /^skd:/i.test(attr(l, "URI") || "");
          if ((method === "SAMPLE-AES" || method === "SAMPLE-AES-CTR") && (format.toLowerCase() !== "identity" || skd)) drm.add(keyFormatName(skd ? "com.apple.streamingkeydelivery" : format));
        } else if (l.startsWith("#EXT-X-MEDIA")) {
          if (/TYPE=SUBTITLES/i.test(l)) subs = true;
        }
        if (master) {
          if (!l.startsWith("#")) {
            children.push(l);
            if (pending) variants.push(pending);
            pending = null;
          }
          for (const m of l.matchAll(/URI="([^"]+)"/g)) children.push(m[1]);
        }
      }
      const audioOnly = master ? variants.length > 0 && variants.every((v) => !v.height && v.codecs && !VIDEO_CODECS.test(v.codecs)) : false;
      return {
        master,
        height: height || null,
        live: !master && !/#EXT-X-ENDLIST/.test(text),
        children,
        variants: variants.slice(0, 20),
        duration: !master && /#EXT-X-ENDLIST/.test(text) ? Math.round(duration) || null : null,
        audioOnly,
        subs,
        drm: [...drm],
      };
    }
    if (!/<MPD[\s>]/.test(text)) return null;
    let height = 0;
    for (const m of text.matchAll(/\bheight="(\d+)"/g)) height = Math.max(height, Number(m[1]));
    const drm = new Set();
    for (const m of text.matchAll(/<(?:\w+:)?ContentProtection\b[^>]*schemeIdUri="([^"]+)"/gi)) {
      const s = m[1].toLowerCase();
      drm.add(s.startsWith("urn:uuid:") ? DRM_UUIDS[s.slice(9)] || "DRM" : "DRM");
    }
    // A named system says more than the generic CENC marker next to it.
    if (drm.size > 1) drm.delete("DRM");
    const variants = [];
    for (const m of text.matchAll(/<Representation\b[^>]*>/g)) {
      const r = m[0];
      const h = Number(/\bheight="(\d+)"/.exec(r)?.[1]) || null;
      const bw = Number(/\bbandwidth="(\d+)"/.exec(r)?.[1]) || null;
      if (h) variants.push({ height: h, bandwidth: bw, codecs: /\bcodecs="([^"]*)"/.exec(r)?.[1] || null });
    }
    const dur = /mediaPresentationDuration="([^"]+)"/.exec(text)?.[1];
    const m = /^P(?:(\d+)D)?(?:T(?:(\d+)H)?(?:(\d+)M)?(?:([\d.]+)S)?)?$/.exec(dur || "");
    const duration = m ? Math.round(Number(m[1] || 0) * 86400 + Number(m[2] || 0) * 3600 + Number(m[3] || 0) * 60 + Number(m[4] || 0)) || null : null;
    const video = /mimeType="video\/|contentType="video"/.test(text) || variants.length > 0;
    return {
      master: true,
      height: height || null,
      live: /type="dynamic"/.test(text),
      children: [],
      variants: variants.slice(0, 20),
      duration,
      audioOnly: !video && /mimeType="audio\/|contentType="audio"/.test(text),
      subs: /contentType="text"|mimeType="(text\/vtt|application\/ttml\+xml)"|codecs="(wvtt|stpp)/.test(text),
      drm: [...drm],
    };
  }

  /** A stream's size from its best variant's bandwidth and its length, in bytes, or null. */
  function estimateSize(c) {
    const bw = Math.max(0, ...(c.variants || []).map((v) => v.bandwidth || 0));
    return bw && c.duration ? Math.round((bw * c.duration) / 8) : null;
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

  // Ad and ad-measurement video hosts: their streams are listed last, never hidden.
  const AD_HOSTS = /(^|\.)(doubleclick\.net|2mdn\.net|googlesyndication\.com|googleadservices\.com|imasdk\.googleapis\.com|adnxs\.com|springserve\.com|serving-sys\.com|innovid\.com|spotxchange\.com|spotx\.tv|teads\.tv|adsrvr\.org|moatads\.com|fwmrm\.net|freewheel\.tv|tremorhub\.com|smartadserver\.com|aniview\.com|vidazoo\.com|connatix\.com|primis\.tech|amazon-adsystem\.com|yieldmo\.com|undertone\.com|criteo\.(com|net)|outbrain\.com|taboola\.com)$/i;

  function isAd(url) {
    try {
      return AD_HOSTS.test(new URL(url).hostname);
    } catch {
      return false;
    }
  }

  /** Pieces of a stream (they say which stream is playing, they aren't videos). */
  function pieceOf(url, mime) {
    const ext = rawExt(url);
    if (ext === "vtt" || ext === "webvtt") return false;
    return PIECE_EXTS.has(ext) || mime === "video/mp2t" || mime === "video/iso.segment";
  }

  const clock = (secs) => {
    const s = Math.round(secs);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const pad = (n) => String(n).padStart(2, "0");
    return h ? `${h}:${pad(m)}:${pad(s % 60)}` : `${m}:${pad(s % 60)}`;
  };
  const mb = (n) => `${(n / 1048576).toFixed(n < 10485760 ? 1 : 0)} MB`;

  /** A short label for a candidate: "HLS stream · up to 1080p · 12:34 · ~420 MB". */
  function label(c) {
    const parts = [];
    if (c.protected) parts.push(c.drm?.length ? `DRM-protected (${c.drm.join(", ")})` : "DRM-protected");
    if (c.kind === "mse") parts.push(c.protected ? "can't be downloaded" : "Assembled by the page in pieces");
    else if (c.live) parts.push("Live: recorded until you press Finish");
    else if (c.kind === "hls") parts.push(c.audioOnly ? "HLS audio" : "HLS stream");
    else if (c.kind === "dash") parts.push(c.audioOnly ? "DASH audio" : "DASH stream");
    else if (c.kind === "site") parts.push(c.siteName || "Video");
    else parts.push((c.ext || extOf(c.url) || "file").toUpperCase());
    if (c.height) parts.push(`up to ${c.height}p`);
    if (c.duration > 0 && !c.live) parts.push(clock(c.duration));
    if (c.size) parts.push(mb(c.size));
    else if (estimateSize(c)) parts.push(`~${mb(estimateSize(c))}`);
    if (c.ad) parts.push("ad");
    return parts.join(" · ");
  }

  return { classify, sniff, estimateSize, isAd, pieceOf, VAGUE_TYPES, keyOf, extOf, rawExt, bareType, interesting, subtitleKind, inspectManifest, jsonAfter, parseJsonish, walk, label, rangeTotal };
})();
