// YouTube, as yt-dlp does it (yt_dlp/extractor/youtube/_base.py and _video.py): the player
// API is asked as clients whose formats come as plain or signed links (not SABR), the
// site's own challenge code is run to finish the links (page.js), and the formats are
// handed to Siler as a DASH manifest. This part has no browser use, for the tests.
"use strict";

var Siler = globalThis.Siler || (globalThis.Siler = {});

Siler.youtube = (() => {
  // Copied from yt-dlp's INNERTUBE_CLIENTS (yt_dlp/extractor/youtube/_base.py, checked against
  // revision 7940e14 of 2026-11-19). `cookies`: SUPPORTS_COOKIES; `js`: REQUIRE_JS_PLAYER.
  const CLIENTS = {
    tv_samsung: {
      number: 7,
      cookies: true,
      js: true,
      context: {
        client: {
          clientName: "TVHTML5",
          clientVersion: "5.20260707",
          deviceMake: "Samsung",
          deviceModel: "UKS9800",
          userAgent: "Mozilla/5.0 (SMART-TV; Linux; Tizen 2.4.0) AppleWebKit/538.1 (KHTML, like Gecko) Version/2.4.0 TV Safari/538.1",
          osName: "Tizen",
          osVersion: "2.4.0",
          hl: "en",
        },
      },
    },
    visionos: {
      number: 101,
      cookies: false,
      js: false,
      context: {
        client: {
          clientName: "VISIONOS",
          clientVersion: "1.02",
          deviceMake: "Apple",
          deviceModel: "RealityDevice17,1",
          userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
          osName: "visionOS",
          osVersion: "26.5.23O471",
          hl: "en",
        },
      },
    },
    // Not one of yt-dlp's defaults, but one of the few clients whose links need neither a PO
    // token nor the player's code: H.264 up to 1080p. Tried last, when the TV and visionOS
    // clients were refused (seen: "confirm you're not a bot" for both, while this one
    // answered).
    ios_producer: {
      number: 92,
      cookies: false,
      js: false,
      context: {
        client: {
          clientName: "IOS_PRODUCER",
          clientVersion: "0.147.945",
          deviceMake: "Apple",
          deviceModel: "iPhone16,2",
          userAgent: "com.google.ios.youtubeproducer/0.147.945 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)",
          osName: "iPhone",
          osVersion: "18.3.2.22D82",
          hl: "en",
        },
      },
    },
  };
  // yt-dlp's _DEFAULT_CLIENTS without `web`, whose formats need a PO token (SABR only), then
  // ios_producer. (The spec's TVHTML5_SIMPLY_EMBEDDED_PLAYER is gone from yt-dlp; its
  // successor tv_simply needs a PO token.)
  const ORDER = ["tv_samsung", "visionos", "ios_producer"];
  // yt-dlp's CHUNK_SIZE for https formats.
  const CHUNK = 10 << 20;

  /** The clients to ask, in yt-dlp's order of preference. */
  function clients() {
    return ORDER.map((name) => ({ name, ...CLIENTS[name] }));
  }

  const esc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

  function parseMime(m) {
    const r = /^\s*([^;\s]+)\s*(?:;\s*codecs="([^"]*)")?/.exec(m || "");
    return r ? { type: r[1].toLowerCase(), codecs: r[2] || "" } : null;
  }

  /** The audio language of a format: `audioTrack.id` is like "en.4" or "de-DE.3". */
  function langOf(f) {
    const id = f.audioTrack?.id;
    return id ? id.split(".")[0] : null;
  }

  /** Formats Siler can fetch whole: with a link, an index, not a DRC or OTF stream. */
  function usable(formats) {
    return (formats || []).filter(
      (f) =>
        f &&
        typeof f.url === "string" &&
        /^https:\/\//.test(f.url) &&
        f.initRange &&
        f.indexRange &&
        !f.isDrc &&
        f.type !== "FORMAT_STREAM_TYPE_OTF" &&
        !f.drmFamilies &&
        /^(video|audio)\//.test(parseMime(f.mimeType)?.type || ""),
    );
  }

  const iso = (secs) => `PT${Math.max(0, secs).toFixed(3)}S`;

  /** A static DASH manifest with one whole-file Representation per format. */
  function buildMpd(formats, duration) {
    const groups = new Map();
    for (const f of usable(formats)) {
      const mime = parseMime(f.mimeType);
      const video = mime.type.startsWith("video/");
      const family = mime.codecs.split(".")[0];
      const lang = video ? null : langOf(f);
      const key = [mime.type, family, lang ?? ""].join("|");
      if (!groups.has(key)) groups.set(key, { mime, video, lang, formats: [], main: false, label: null });
      const g = groups.get(key);
      g.formats.push(f);
      if (f.audioTrack?.audioIsDefault) g.main = true;
      if (f.audioTrack?.displayName) g.label = f.audioTrack.displayName;
    }
    const sets = [];
    for (const g of groups.values()) {
      const reps = g.formats
        .sort((a, b) => (b.bitrate || 0) - (a.bitrate || 0))
        .map((f) => {
          const mime = parseMime(f.mimeType);
          const attrs = [`id="${esc(f.itag)}"`, `codecs="${esc(mime.codecs)}"`, `bandwidth="${Number(f.averageBitrate || f.bitrate || 1)}"`];
          if (g.video) {
            if (f.width) attrs.push(`width="${Number(f.width)}"`);
            if (f.height) attrs.push(`height="${Number(f.height)}"`);
            if (f.fps) attrs.push(`frameRate="${Number(f.fps)}"`);
          }
          const channels = !g.video && f.audioChannels
            ? `<AudioChannelConfiguration schemeIdUri="urn:mpeg:dash:23003:3:audio_channel_configuration:2011" value="${Number(f.audioChannels)}"/>`
            : "";
          const range = (r) => `${Number(r.start)}-${Number(r.end)}`;
          return (
            `<Representation ${attrs.join(" ")}>${channels}<BaseURL>${esc(f.url)}</BaseURL>` +
            `<SegmentBase indexRange="${range(f.indexRange)}"><Initialization range="${range(f.initRange)}"/></SegmentBase></Representation>`
          );
        });
      const a = [`mimeType="${esc(g.mime.type)}"`];
      if (g.lang) a.push(`lang="${esc(g.lang)}"`);
      const role = g.main ? `<Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>` : "";
      const label = g.label ? `<Label>${esc(g.label)}</Label>` : "";
      sets.push(`<AdaptationSet ${a.join(" ")}>${role}${label}${reps.join("")}</AdaptationSet>`);
    }
    if (!sets.length) return null;
    return (
      `<?xml version="1.0" encoding="UTF-8"?>` +
      `<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011" mediaPresentationDuration="${iso(duration)}">` +
      `<Period>${sets.join("")}</Period></MPD>`
    );
  }

  const textOf = (t) => t?.simpleText ?? (t?.runs || []).map((r) => r.text).join("") ?? null;

  /**
   * The caption tracks as WebVTT files. Tracks whose links carry the PO-token experiment
   * (`exp=xpe`/`xpv`) are left out, as yt-dlp does without a token. No translations.
   */
  function captions(tracks) {
    const out = [];
    for (const t of tracks || []) {
      if (!t?.baseUrl) continue;
      let u;
      try {
        u = new URL(t.baseUrl, "https://www.youtube.com/");
      } catch {
        continue;
      }
      if (u.searchParams.getAll("exp").some((e) => e === "xpe" || e === "xpv")) continue;
      u.searchParams.set("fmt", "vtt");
      u.searchParams.delete("xosf");
      const name = textOf(t.name) || t.languageCode || "Captions";
      out.push({
        url: u.href,
        lang: t.languageCode || null,
        label: t.kind === "asr" ? `${name} (auto-generated)`.replace(/ \(auto-generated\) \(auto-generated\)$/, " (auto-generated)") : name,
      });
    }
    return out;
  }

  /**
   * Seconds to wait before YouTube serves the video: the pre-roll ads it would play first
   * (yt-dlp's _get_available_at_timestamp).
   */
  function adWait(pr) {
    let wait = 0;
    const renderers = [];
    for (const p of pr?.adPlacements || []) {
      const r = p?.adPlacementRenderer;
      if (r?.config?.adPlacementConfig?.kind === "AD_PLACEMENT_KIND_START" && r.renderer?.instreamVideoAdRenderer) {
        renderers.push(r.renderer.instreamVideoAdRenderer);
      }
    }
    for (const s of pr?.adSlots || []) {
      const r = s?.adSlotRenderer;
      if (r?.adSlotMetadata?.triggerEvent !== "SLOT_TRIGGER_EVENT_BEFORE_CONTENT") continue;
      const layout = r.fulfillmentContent?.fulfilledLayout?.playerBytesAdLayoutRenderer?.renderingContent;
      if (!layout) continue;
      if (layout.instreamVideoAdRenderer) renderers.push(layout.instreamVideoAdRenderer);
      for (const l of layout.playerBytesSequentialLayoutRenderer?.sequentialLayouts || []) {
        const v = l?.playerBytesAdLayoutRenderer?.renderingContent?.instreamVideoAdRenderer;
        if (v) renderers.push(v);
      }
    }
    for (const r of renderers) {
      let duration = null;
      const vars = new URLSearchParams(r.playerVars || "");
      if (vars.get("length_seconds")) duration = Number(vars.get("length_seconds"));
      if (r.skipOffsetMilliseconds != null) duration = Number(r.skipOffsetMilliseconds) / 1000;
      if (Number.isFinite(duration)) wait += duration;
    }
    return Math.ceil(wait);
  }

  /**
   * The hand-over for a client's answer: `{item, title}`, or `{error: kind}` with a kind the
   * extension turns into words (see `failure`). The title is the answer's, else the page's own
   * player answer's, else `fallbackTitle` (the tab's): TV clients answer without one.
   */
  function handOver(answer, client, pageUrl, web, fallbackTitle = null) {
    const pr = answer.pr;
    const details = pr.videoDetails || web?.videoDetails || {};
    const title = details.title || web?.videoDetails?.title || fallbackTitle || null;
    if (details.isUpcoming) return { error: "upcoming" };
    if (details.isLive) {
      // A live stream goes over as its HLS playlist; the app records it until Finish.
      const hls = pr.streamingData?.hlsManifestUrl;
      if (!hls) return { error: "live_no_url" };
      return {
        item: {
          url: hls,
          referrer: "https://www.youtube.com/",
          page_url: pageUrl,
          user_agent: client.context.client.userAgent,
          headers: [],
          media: { site: "youtube", title },
        },
        title,
        live: true,
      };
    }
    const duration =
      Number(details.lengthSeconds) ||
      Math.max(0, ...usable(pr.streamingData?.adaptiveFormats).map((f) => Number(f.approxDurationMs || 0) / 1000));
    const mpd = buildMpd(pr.streamingData?.adaptiveFormats, duration);
    if (!mpd) return { error: onlyDrm(pr) ? "drm" : "no_formats" };
    const tracks = pr.captions?.playerCaptionsTracklistRenderer?.captionTracks || web?.captions?.playerCaptionsTracklistRenderer?.captionTracks;
    const wait = adWait(pr);
    return {
      item: {
        url: pageUrl,
        referrer: "https://www.youtube.com/",
        page_url: pageUrl,
        user_agent: client.context.client.userAgent,
        headers: [],
        media: {
          site: "youtube",
          title,
          manifest: { kind: "dash", body: mpd, base_url: pageUrl },
          subtitles: captions(tracks),
          chunk_bytes: CHUNK,
          not_before_ms: wait > 0 ? Date.now() + wait * 1000 : null,
        },
      },
      title,
    };
  }

  /** Whether every format of an answer is DRM-protected (and there are some). */
  function onlyDrm(pr) {
    const all = pr?.streamingData?.adaptiveFormats || [];
    return all.length > 0 && all.every((f) => f?.drmFamilies);
  }

  /** What one client's refusal means, or null when it says nothing specific. */
  function refusal(pr) {
    if (!pr) return null;
    const ps = pr.playabilityStatus || {};
    const words = `${ps.reason || ""} ${ps.subreason || ""}`;
    if (pr.videoDetails?.isUpcoming || ps.status === "LIVE_STREAM_OFFLINE") return "upcoming";
    if (/members[- ]only|join this channel|channel's members|channel members/i.test(words)) return "members";
    if (/confirm your age|age[- ]restricted|inappropriate for some users/i.test(words)) return "age";
    if (/not a bot/i.test(words)) return "bot";
    if (ps.status === "OK" && onlyDrm(pr)) return "drm";
    if (ps.status === "OK" && pr.videoDetails?.isLive && !pr.streamingData?.hlsManifestUrl) return "live_no_url";
    return null;
  }

  // Facts about the video outrank one client's trouble: "members only" from one client says
  // more than "prove you're not a bot" from another.
  const RANK = ["upcoming", "members", "age", "drm", "live_no_url", "bot"];

  /**
   * Why no client gave a downloadable video: `{kind}` from RANK, `{kind: "says", reason}` with
   * YouTube's own words, or `{kind: "no_formats"}`. `answers` are page.js's client answers.
   */
  function failure(answers) {
    const kinds = new Set();
    let said = null;
    for (const a of answers || []) {
      const k = refusal(a?.pr);
      if (k) kinds.add(k);
      else if (!said && a?.pr?.playabilityStatus?.reason && a.pr.playabilityStatus.status !== "OK") said = a.pr.playabilityStatus.reason;
    }
    for (const k of RANK) if (kinds.has(k)) return { kind: k };
    if (said) return { kind: "says", reason: said };
    return { kind: "no_formats" };
  }

  /** The canonical address of a video. */
  const watchUrl = (id) => `https://www.youtube.com/watch?v=${encodeURIComponent(id)}`;

  return { CLIENTS, clients, buildMpd, captions, adWait, usable, handOver, failure, refusal, watchUrl, CHUNK };
})();
