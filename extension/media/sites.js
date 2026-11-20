// Site recognisers: where a site keeps its videos, ported from yt-dlp's extractors (which are
// public domain) as readers of what the site's own player loads. Each reader takes JSON or
// page text and returns candidates; nothing here fetches or touches the browser.
"use strict";

var KStar = globalThis.KStar || (globalThis.KStar = {});

KStar.sites = (() => {
  const { walk, jsonAfter, parseJsonish } = KStar.media;

  const SITES = [
    { id: "youtube", name: "YouTube", hosts: ["youtube.com", "youtube-nocookie.com", "youtu.be"] },
    { id: "vimeo", name: "Vimeo", hosts: ["vimeo.com"] },
    { id: "facebook", name: "Facebook", hosts: ["facebook.com", "fb.watch"] },
    { id: "instagram", name: "Instagram", hosts: ["instagram.com"] },
    { id: "tiktok", name: "TikTok", hosts: ["tiktok.com"] },
  ];

  /** Responses whose bodies a recogniser reads (Firefox's response filter). */
  const BODY_URLS = [
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

  function hostOf(url) {
    try {
      return new URL(url).hostname.toLowerCase();
    } catch {
      return "";
    }
  }

  /** The recogniser for a page or frame address, or null. */
  function siteOf(url) {
    const host = hostOf(url);
    return SITES.find((s) => s.hosts.some((h) => host === h || host.endsWith("." + h))) || null;
  }

  /** The video a page address is about (its id on the site), or null. */
  function idFromUrl(url) {
    let u;
    try {
      u = new URL(url);
    } catch {
      return null;
    }
    const site = siteOf(url);
    const p = u.pathname;
    let m;
    switch (site?.id) {
      case "youtube":
        if (u.hostname.endsWith("youtu.be")) return p.slice(1, 12) || null;
        if (u.searchParams.get("v")) return u.searchParams.get("v");
        m = p.match(/^\/(?:shorts|embed|live|v|e)\/([0-9A-Za-z_-]{11})/);
        return m ? m[1] : null;
      case "vimeo":
        m = p.match(/\/(?:video\/)?(\d{5,})/);
        return m ? m[1] : null;
      case "tiktok":
        m = p.match(/\/(?:video|photo)\/(\d+)/);
        return m ? m[1] : null;
      case "instagram":
        m = p.match(/\/(?:p|reel|reels|tv)\/([^/?#]+)/);
        return m ? m[1] : null;
      case "facebook":
        m = p.match(/\/(?:videos|reel|watch\/live)\/(?:[^/]+\/)?(\d+)/) || p.match(/\/videos\/(?:vb\.\d+\/)?(\d+)/);
        if (m) return m[1];
        return u.searchParams.get("v");
      default:
        return null;
    }
  }

  const text = (v) => (typeof v === "string" ? v : v && typeof v.text === "string" ? v.text : null);
  const firstLine = (s, n = 100) => (s ? s.split("\n")[0].trim().slice(0, n) || null : null);

  // ------------------------------------------------------------ Vimeo (yt-dlp vimeo.py)

  /** A Vimeo player config: its HLS stream with the subtitle files. */
  function vimeoConfig(config, pageUrl) {
    const files = config?.request?.files;
    const video = config?.video || {};
    if (!files) return [];
    const title = video.title || null;
    const id = video.id != null ? String(video.id) : null;
    const subtitles = (config.request.text_tracks || [])
      .filter((t) => t && t.url)
      .map((t) => ({ url: new URL(t.url, "https://vimeo.com/").href, lang: t.lang || null, label: t.label || null }));
    const hls = files.hls;
    const cdn = hls?.cdns?.[hls.default_cdn] || Object.values(hls?.cdns || {})[0];
    if (cdn?.url) {
      return [
        {
          key: `vimeo:${id || cdn.url}`,
          site: "vimeo",
          kind: "hls",
          title,
          ids: id ? [id] : [],
          url: cdn.url,
          referrer: pageUrl,
          hint: { site: "vimeo", title, subtitles },
        },
      ];
    }
    const best = (files.progressive || []).filter((p) => p.url).sort((a, b) => (b.width || 0) - (a.width || 0))[0];
    if (!best) return [];
    return [
      {
        key: `vimeo:${id || best.url}`,
        site: "vimeo",
        kind: "file",
        title,
        ids: id ? [id] : [],
        url: best.url,
        height: best.height || null,
        mime: "video/mp4",
        referrer: pageUrl,
      },
    ];
  }

  function vimeoHtml(html, pageUrl) {
    const config = jsonAfter(html, "window.playerConfig =") || jsonAfter(html, "playerConfig =");
    return config ? vimeoConfig(config, pageUrl) : [];
  }

  // ------------------------------------------------------------ TikTok (yt-dlp tiktok.py)

  function tiktokItem(item, pageUrl) {
    const v = item?.video;
    if (!v || item.id == null) return null;
    const id = String(item.id);
    const rates = (v.bitrateInfo || []).filter((b) => b?.PlayAddr?.UrlList?.length);
    const h264 = rates.filter((b) => /^h264/i.test(b.CodecType || ""));
    const pick = (h264.length ? h264 : rates).sort((a, b) => (b.Bitrate || 0) - (a.Bitrate || 0))[0];
    let url = pick?.PlayAddr?.UrlList?.[0] || null;
    let size = Number(pick?.PlayAddr?.DataSize) || null;
    let height = Number(pick?.PlayAddr?.Height) || null;
    if (!url) {
      url = (typeof v.playAddr === "string" && v.playAddr) || (typeof v.downloadAddr === "string" && v.downloadAddr) || null;
      size = null;
      height = Number(v.height) || null;
    }
    if (!url) return null;
    const author = item.author?.uniqueId;
    const title = firstLine(item.desc) || (author ? `TikTok video by ${author}` : `TikTok video ${id}`);
    return {
      key: `tiktok:${id}`,
      site: "tiktok",
      kind: "file",
      title,
      ids: [id],
      url,
      size,
      height,
      mime: "video/mp4",
      // TikTok's video servers want the page as referrer, and its cookies.
      referrer: pageUrl,
      filename: `${title.replace(/[\\/:*?"<>|]+/g, " ").slice(0, 80).trim()}.mp4`,
    };
  }

  function tiktokData(data, pageUrl) {
    const out = new Map();
    walk(data, (o) => {
      if (o.video && (o.video.bitrateInfo || o.video.playAddr) && o.id != null) {
        const c = tiktokItem(o, pageUrl);
        if (c) out.set(c.key, c);
      }
    });
    return [...out.values()];
  }

  function tiktokHtml(html, pageUrl) {
    const m = html.match(/<script[^>]+id="__UNIVERSAL_DATA_FOR_REHYDRATION__"[^>]*>([\s\S]*?)<\/script>/);
    if (!m) return [];
    try {
      return tiktokData(JSON.parse(m[1]), pageUrl);
    } catch {
      return [];
    }
  }

  // ------------------------------------------------------------ Facebook (yt-dlp facebook.py)

  function decodeMpd(s) {
    if (typeof s !== "string" || !s) return null;
    let t = s.trim();
    if (!t.startsWith("<")) {
      try {
        t = decodeURIComponent(t.replace(/\+/g, " "));
      } catch {
        return null;
      }
    }
    return /<MPD[\s>]/.test(t) ? t : null;
  }

  function facebookVideo(o, pageUrl) {
    const fmt = o.videoDeliveryLegacyFields || o;
    const mpd =
      decodeMpd(fmt.dash_manifest_xml_string) ||
      decodeMpd(fmt.dash_manifest) ||
      decodeMpd(fmt.manifest_xml) ||
      decodeMpd(o.dash_manifests?.find?.((d) => d?.manifest_xml)?.manifest_xml);
    const progressive =
      fmt.browser_native_hd_url ||
      fmt.playable_url_quality_hd ||
      fmt.browser_native_sd_url ||
      fmt.playable_url ||
      (o.progressive_urls || []).find((p) => p?.progressive_url && /hd/i.test(p.metadata?.quality || ""))?.progressive_url ||
      (o.progressive_urls || []).find((p) => p?.progressive_url)?.progressive_url ||
      null;
    if (!mpd && !progressive) return null;
    const id = String(o.videoId || o.id || "");
    if (!id) return null;
    const title =
      firstLine(text(o.title)) || firstLine(o.name) || firstLine(text(o.savable_description)) || firstLine(text(o.message)) || null;
    const base = { key: `facebook:${id}`, site: "facebook", title, ids: [id], referrer: pageUrl };
    if (mpd) {
      return {
        ...base,
        kind: "dash",
        url: pageUrl,
        hint: { site: "facebook", title, manifest: { kind: "dash", body: mpd, base_url: pageUrl } },
      };
    }
    return { ...base, kind: "file", url: progressive, mime: "video/mp4" };
  }

  function facebookData(data, pageUrl) {
    const out = new Map();
    walk(data, (o) => {
      if (
        (o.id || o.videoId) &&
        (o.videoDeliveryLegacyFields ||
          o.dash_manifest_xml_string ||
          o.dash_manifest ||
          o.dash_manifests ||
          o.playable_url ||
          o.browser_native_hd_url ||
          o.browser_native_sd_url ||
          o.progressive_urls)
      ) {
        const c = facebookVideo(o, pageUrl);
        // A DASH manifest beats a plain file for the same video.
        if (c && (!out.has(c.key) || c.kind === "dash")) out.set(c.key, c);
      }
    });
    return [...out.values()];
  }

  /** JSON in `<script type="application/json">` tags (Facebook and Instagram embed data so). */
  function scriptJson(html) {
    const out = [];
    for (const m of html.matchAll(/<script[^>]*type="application\/json"[^>]*>([\s\S]*?)<\/script>/g)) {
      if (!/dash_manifest|playable_url|browser_native|video_versions|video_dash_manifest/.test(m[1])) continue;
      try {
        out.push(JSON.parse(m[1]));
      } catch {
        /* not JSON */
      }
    }
    return out;
  }

  const facebookHtml = (html, pageUrl) => scriptJson(html).flatMap((d) => facebookData(d, pageUrl));

  // ------------------------------------------------------------ Instagram (yt-dlp instagram.py)

  function instagramMedia(o, pageUrl) {
    const mpd = decodeMpd(o.video_dash_manifest);
    const versions = (o.video_versions || []).filter((v) => v?.url);
    if (!mpd && !versions.length) return null;
    const code = o.code || o.shortcode || null;
    const id = String(o.pk || o.id || code || "");
    if (!id) return null;
    const user = o.user?.username || o.owner?.username;
    const title = firstLine(text(o.caption)) || (user ? `Video by ${user}` : `Instagram video ${code || id}`);
    const ids = [code, id, String(o.pk || "")].filter(Boolean);
    const base = { key: `instagram:${code || id}`, site: "instagram", title, ids, referrer: pageUrl };
    if (mpd) {
      return {
        ...base,
        kind: "dash",
        url: pageUrl,
        hint: { site: "instagram", title, manifest: { kind: "dash", body: mpd, base_url: pageUrl } },
      };
    }
    const best = versions.sort((a, b) => (b.width || 0) * (b.height || 0) - (a.width || 0) * (a.height || 0))[0];
    return { ...base, kind: "file", url: best.url, height: best.height || null, mime: "video/mp4" };
  }

  function instagramData(data, pageUrl) {
    const out = new Map();
    walk(data, (o) => {
      if ((o.video_dash_manifest || o.video_versions) && (o.code || o.pk || o.id)) {
        const c = instagramMedia(o, pageUrl);
        if (c && (!out.has(c.key) || c.kind === "dash")) out.set(c.key, c);
      }
    });
    return [...out.values()];
  }

  const instagramHtml = (html, pageUrl) => scriptJson(html).flatMap((d) => instagramData(d, pageUrl));

  // ------------------------------------------------------------ dispatch

  const READERS = {
    vimeo: { data: vimeoConfig, html: vimeoHtml },
    tiktok: { data: tiktokData, html: tiktokHtml },
    facebook: { data: facebookData, html: facebookHtml },
    instagram: { data: instagramData, html: instagramHtml },
  };

  /** Candidates in a response body from one of BODY_URLS. */
  function fromBody(url, body, pageUrl) {
    const site = siteOf(url);
    const reader = site && READERS[site.id];
    if (!reader) return [];
    return parseJsonish(body).flatMap((d) => reader.data(d, pageUrl));
  }

  /** Candidates in a page's HTML (its embedded data). */
  function fromHtml(site, html, pageUrl) {
    const reader = READERS[site];
    return reader ? reader.html(html, pageUrl) : [];
  }

  return { SITES, BODY_URLS, siteOf, idFromUrl, fromBody, fromHtml, vimeoConfig, tiktokData, facebookData, instagramData };
})();
