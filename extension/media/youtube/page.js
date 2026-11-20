// Runs inside a YouTube page (the page's own JavaScript world, so its cookies, its visitor
// data and its player script are the ones used). Asks the player API as the given clients
// and finishes the format links with YouTube's own challenge code, run by yt-dlp's ejs
// solver in a blank frame of the page, so the player script never touches the page itself.
//
// Must stay self-contained: it is serialised and injected with scripting.executeScript.
"use strict";

// eslint-disable-next-line no-unused-vars
async function kstarYouTubeInPage(args) {
  const out = { ok: false, clients: [], web: null, preprocessed: null, error: null };
  const pick = (f) => ({
    itag: f.itag,
    url: f.url,
    signatureCipher: f.signatureCipher,
    mimeType: f.mimeType,
    bitrate: f.bitrate,
    averageBitrate: f.averageBitrate,
    width: f.width,
    height: f.height,
    fps: f.fps,
    initRange: f.initRange,
    indexRange: f.indexRange,
    contentLength: f.contentLength,
    approxDurationMs: f.approxDurationMs,
    audioTrack: f.audioTrack,
    audioChannels: f.audioChannels,
    isDrc: f.isDrc,
    type: f.type,
    drmFamilies: f.drmFamilies,
  });
  const trim = (pr) => ({
    playabilityStatus: { status: pr?.playabilityStatus?.status, reason: pr?.playabilityStatus?.reason },
    videoDetails: pr?.videoDetails
      ? {
          videoId: pr.videoDetails.videoId,
          title: pr.videoDetails.title,
          lengthSeconds: pr.videoDetails.lengthSeconds,
          isLive: pr.videoDetails.isLive,
          isUpcoming: pr.videoDetails.isUpcoming,
        }
      : null,
    streamingData: { adaptiveFormats: (pr?.streamingData?.adaptiveFormats || []).map(pick) },
    captions: pr?.captions
      ? { playerCaptionsTracklistRenderer: { captionTracks: pr.captions.playerCaptionsTracklistRenderer?.captionTracks || [] } }
      : null,
    adPlacements: pr?.adPlacements || [],
    adSlots: pr?.adSlots || [],
  });

  try {
    const yc = window.ytcfg;
    const get = (k) => {
      try {
        return yc?.get ? yc.get(k) : yc?.data_?.[k];
      } catch {
        return undefined;
      }
    };
    const visitor = get("VISITOR_DATA") || get("INNERTUBE_CONTEXT")?.client?.visitorData || null;
    const jsUrl =
      get("PLAYER_JS_URL") ||
      Object.values(get("WEB_PLAYER_CONTEXT_CONFIGS") || {}).find((c) => c?.jsUrl)?.jsUrl ||
      document.querySelector('script[src*="/s/player/"]')?.src ||
      "";
    const pid = /\/s\/player\/([0-9a-zA-Z_-]{8})\//.exec(jsUrl)?.[1];
    // yt-dlp's default player variant ("main").
    const playerUrl = pid ? `${location.origin}/s/player/${pid}/player_ias.vflset/en_US/base.js` : null;
    const loggedIn = !!get("LOGGED_IN");
    let sts = Number(get("STS")) || null;

    // The page's own answer, for the title and captions (as yt-dlp's webpage client).
    try {
      const web = document.getElementById("movie_player")?.getPlayerResponse?.();
      const initial = window.ytInitialPlayerResponse;
      const pr = web?.videoDetails?.videoId === args.videoId ? web : initial?.videoDetails?.videoId === args.videoId ? initial : null;
      if (pr) out.web = trim(pr);
    } catch {
      /* not a watch page */
    }

    async function authHeaders() {
      const cookie = (name) => new RegExp(`(?:^|; )${name.replace(/[$]/g, "\\$&")}=([^;]+)`).exec(document.cookie)?.[1];
      const sid = [
        ["SAPISIDHASH", cookie("SAPISID") || cookie("__Secure-3PAPISID")],
        ["SAPISID1PHASH", cookie("__Secure-1PAPISID")],
        ["SAPISID3PHASH", cookie("__Secure-3PAPISID")],
      ];
      const ts = String(Math.round(Date.now() / 1000));
      const user = get("USER_SESSION_ID");
      const parts = [];
      for (const [scheme, value] of sid) {
        if (!value) continue;
        const input = [...(user ? [user] : []), ts, value, location.origin].join(" ");
        const digest = await crypto.subtle.digest("SHA-1", new TextEncoder().encode(input));
        const hex = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
        // yt-dlp's _make_sid_authorization: the names of the extra parts ("u") follow.
        parts.push(`${scheme} ${[ts, hex, ...(user ? ["u"] : [])].join("_")}`);
      }
      const h = {};
      if (parts.length) {
        h.Authorization = parts.join(" ");
        h["X-Origin"] = location.origin;
      }
      const delegated = get("DELEGATED_SESSION_ID");
      const index = get("SESSION_INDEX");
      if (delegated) h["X-Goog-PageId"] = delegated;
      if (delegated || index != null) h["X-Goog-AuthUser"] = String(index ?? 0);
      if (loggedIn) h["X-Youtube-Bootstrap-Logged-In"] = "true";
      return h;
    }

    // The player script, for the signature timestamp and the challenges.
    let playerText = null;
    const player = async () => {
      if (playerText === null && playerUrl) {
        const r = await fetch(playerUrl, { credentials: "omit" });
        playerText = r.ok ? await r.text() : "";
      }
      return playerText || "";
    };
    if (!sts && args.clients.some((c) => c.js)) {
      sts = Number(/(?:signatureTimestamp|sts)\s*:\s*(\d{5})/.exec(await player())?.[1]) || null;
    }

    for (const c of args.clients) {
      const headers = {
        "Content-Type": "application/json",
        "X-YouTube-Client-Name": String(c.number),
        "X-YouTube-Client-Version": c.context.client.clientVersion,
      };
      if (visitor) headers["X-Goog-Visitor-Id"] = visitor;
      if (c.cookies && loggedIn) Object.assign(headers, await authHeaders());
      const body = {
        context: { ...c.context, client: { ...c.context.client, ...(visitor ? { visitorData: visitor } : {}) } },
        videoId: args.videoId,
        playbackContext: {
          contentPlaybackContext: { html5Preference: "HTML5_PREF_WANTS", ...(sts && c.js ? { signatureTimestamp: sts } : {}) },
        },
        contentCheckOk: true,
        racyCheckOk: true,
      };
      try {
        const r = await fetch(`${location.origin}/youtubei/v1/player?prettyPrint=false`, {
          method: "POST",
          headers,
          body: JSON.stringify(body),
          credentials: c.cookies ? "include" : "omit",
        });
        const pr = r.ok ? await r.json() : null;
        out.clients.push({ name: c.name, http: r.status, pr: pr ? trim(pr) : null });
      } catch (e) {
        out.clients.push({ name: c.name, http: 0, pr: null, error: String(e?.message ?? e) });
      }
    }

    // Collect the challenges: every `n`, and one `sig` per signature length (yt-dlp solves
    // a string of that length and uses the resulting order).
    const nSet = new Set();
    const sigLens = new Set();
    for (const c of out.clients) {
      for (const f of c.pr?.streamingData?.adaptiveFormats || []) {
        let url = f.url;
        if (!url && f.signatureCipher) {
          const sc = new URLSearchParams(f.signatureCipher);
          url = sc.get("url");
          if (sc.get("s")) sigLens.add(sc.get("s").length);
        }
        if (!url) continue;
        try {
          const n = new URL(url).searchParams.get("n");
          if (n) nSet.add(n);
        } catch {
          /* skipped below */
        }
      }
    }

    const solved = { n: {}, sig: {} };
    if ((nSet.size || sigLens.size) && playerUrl) {
      const frame = document.createElement("iframe");
      frame.style.display = "none";
      frame.setAttribute("aria-hidden", "true");
      (document.body || document.documentElement).appendChild(frame);
      try {
        const w = frame.contentWindow;
        // The page requires Trusted Types for code from strings; the blank frame gets a
        // policy of its own that lets the solver compile the player script.
        if (w.trustedTypes && !w.trustedTypes.defaultPolicy) {
          w.trustedTypes.createPolicy("default", { createScript: (s) => s });
        }
        w.Function(args.libs.meriyah)();
        w.Function(args.libs.astring)();
        const jsc = w.Function(`${args.libs.core}\n;return jsc;`)();
        const sigInputs = [...sigLens].map((len) => Array.from({ length: len }, (_, i) => String.fromCharCode(i)).join(""));
        const requests = [];
        if (nSet.size) requests.push({ type: "n", challenges: [...nSet] });
        if (sigInputs.length) requests.push({ type: "sig", challenges: sigInputs });
        const cached = args.cached && args.cached.url === playerUrl ? args.cached.code : null;
        const input = cached
          ? { type: "preprocessed", preprocessed_player: cached, requests }
          : { type: "player", player: await player(), requests, output_preprocessed: true };
        const res = jsc(input);
        if (!cached && res?.preprocessed_player) out.preprocessed = { url: playerUrl, code: res.preprocessed_player };
        for (const r of res?.responses || []) {
          if (r.type !== "result") {
            out.solverError = String(r.error || "").split("\n")[0];
            continue;
          }
          for (const [k, v] of Object.entries(r.data || {})) {
            if (nSet.has(k)) solved.n[k] = v;
            else solved.sig[k.length] = [...v].map((ch) => ch.charCodeAt(0));
          }
        }
      } catch (e) {
        out.solverError = String(e?.message ?? e).split("\n")[0];
      } finally {
        frame.remove();
      }
    }

    // Finish the links; a format whose challenge wasn't solved is left out.
    for (const c of out.clients) {
      const fmts = c.pr?.streamingData?.adaptiveFormats;
      if (!fmts) continue;
      c.pr.streamingData.adaptiveFormats = fmts
        .map((f) => {
          let url = f.url;
          let sig = null;
          if (!url && f.signatureCipher) {
            const sc = new URLSearchParams(f.signatureCipher);
            url = sc.get("url");
            const s = sc.get("s");
            const spec = s && solved.sig[s.length];
            if (!url || !spec) return null;
            sig = [sc.get("sp") || "signature", spec.map((i) => s[i]).join("")];
          }
          if (!url) return null;
          let u;
          try {
            u = new URL(url);
          } catch {
            return null;
          }
          const n = u.searchParams.get("n");
          if (n) {
            if (!solved.n[n]) return null;
            u.searchParams.set("n", solved.n[n]);
          }
          let finished = u.href;
          if (sig) finished += `&${sig[0]}=${sig[1]}`;
          return { ...f, url: finished, signatureCipher: undefined };
        })
        .filter(Boolean);
    }
    out.loggedIn = loggedIn;
    out.playerUrl = playerUrl;
    out.ok = true;
  } catch (e) {
    out.error = String(e?.message ?? e);
  }
  return out;
}
