// Tests for the extension's video feed and site recognisers: `node --test extension/tests/`.
// The scripts are plain background scripts; they are loaded here into a context of their own.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const ext = path.join(here, "..");
const fixture = (p) => fs.readFileSync(path.join(here, "fixtures", p), "utf8");

function load() {
  const ctx = vm.createContext({ URL, URLSearchParams, TextDecoder, console, Date, Math });
  for (const f of ["media/common.js", "media/sites.js", "media/youtube/build.js"]) {
    vm.runInContext(fs.readFileSync(path.join(ext, f), "utf8"), ctx, { filename: f });
  }
  return ctx.KStar;
}
const K = load();
// Objects made in the scripts' context compare by structure only after a JSON round trip.
const plain = (v) => JSON.parse(JSON.stringify(v));

test("responses are told apart: manifests, whole files, and pieces", () => {
  const c = (r) => plain(K.media.classify({ status: 200, ...r }));
  assert.equal(c({ url: "https://cdn.example/v/master.m3u8?token=1" }).kind, "hls");
  assert.equal(c({ url: "https://cdn.example/v/index", mime: "application/vnd.apple.mpegurl; charset=utf-8" }).kind, "hls");
  assert.equal(c({ url: "https://cdn.example/v/manifest.mpd" }).kind, "dash");
  assert.equal(c({ url: "https://cdn.example/x", mime: "application/dash+xml" }).kind, "dash");
  const file = c({ url: "https://cdn.example/movie.mp4", mime: "video/mp4", length: 50_000_000 });
  assert.deepEqual(file, { kind: "file", size: 50_000_000, mime: "video/mp4" });
  // A file read in ranges is listed once, from its first range, with its full size.
  assert.equal(c({ url: "https://cdn.example/movie.mp4", status: 206, mime: "video/mp4", contentRange: "bytes 0-1023/9000000" }).size, 9_000_000);
  assert.equal(c({ url: "https://cdn.example/movie.mp4", status: 206, mime: "video/mp4", contentRange: "bytes 1024-2047/9000000" }), null);
  // Pieces of streams, small files, YouTube's own pieces, and failures aren't videos.
  assert.equal(c({ url: "https://cdn.example/seg-1.ts", mime: "video/mp2t", length: 2_000_000 }), null);
  assert.equal(c({ url: "https://cdn.example/seg-1.m4s", mime: "video/mp4", length: 2_000_000 }), null);
  assert.equal(c({ url: "https://cdn.example/clip.mp4", mime: "video/mp4", length: 100_000 }), null);
  assert.equal(c({ url: "https://rr1---sn-x.googlevideo.com/videoplayback?itag=137", mime: "video/mp4", length: 9_000_000 }), null);
  assert.equal(c({ url: "https://video.fbcdn.net/v/a.mp4?bytestart=0&byteend=999", mime: "video/mp4" }), null);
  assert.equal(c({ url: "https://cdn.example/v/master.m3u8", status: 404 }), null);
  assert.equal(c({ url: "https://cdn.example/page", mime: "text/html" }), null);
});

test("manifests say whether they are a master and how high they go", () => {
  const master =
    '#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="a",NAME="en",URI="audio/en.m3u8"\n#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=640x360,AUDIO="a"\na.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2,RESOLUTION=1920x1080\nb.m3u8\n';
  assert.deepEqual(plain(K.media.inspectManifest("hls", master)), {
    master: true,
    height: 1080,
    live: false,
    children: ["audio/en.m3u8", "a.m3u8", "b.m3u8"],
  });
  const media = "#EXTM3U\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n";
  assert.deepEqual(plain(K.media.inspectManifest("hls", media)), { master: false, height: null, live: false, children: [] });
  assert.equal(K.media.inspectManifest("hls", "<html>"), null);
  const mpd = '<MPD type="static"><Period><AdaptationSet><Representation height="720"/></AdaptationSet></Period></MPD>';
  assert.equal(K.media.inspectManifest("dash", mpd).height, 720);
});

test("JSON is found after a marker in a script", () => {
  const s = 'var x = 1; window.playerConfig = {"a":"}{ \\"q\\"","b":[1,{"c":2}]}; var y;';
  assert.deepEqual(plain(K.media.jsonAfter(s, "window.playerConfig =")), { a: '}{ "q"', b: [1, { c: 2 }] });
  assert.equal(K.media.jsonAfter(s, "nothing ="), null);
});

test("the site and video of a page address", () => {
  const id = (u) => K.sites.idFromUrl(u);
  assert.equal(id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1"), "dQw4w9WgXcQ");
  assert.equal(id("https://youtu.be/dQw4w9WgXcQ"), "dQw4w9WgXcQ");
  assert.equal(id("https://www.youtube.com/shorts/abcdefghijk"), "abcdefghijk");
  assert.equal(id("https://www.youtube-nocookie.com/embed/abcdefghijk?rel=0"), "abcdefghijk");
  assert.equal(id("https://vimeo.com/336812660"), "336812660");
  assert.equal(id("https://player.vimeo.com/video/76979871?h=abc"), "76979871");
  assert.equal(id("https://www.tiktok.com/@scout2015/video/6718335390845095173"), "6718335390845095173");
  assert.equal(id("https://www.instagram.com/reel/Chunk8-jurw/"), "Chunk8-jurw");
  assert.equal(id("https://www.facebook.com/cnn/videos/10155529876156509/"), "10155529876156509");
  assert.equal(id("https://www.facebook.com/watch/?v=10155529876156509"), "10155529876156509");
  assert.equal(K.sites.siteOf("https://m.youtube.com/watch?v=x").id, "youtube");
  assert.equal(K.sites.siteOf("https://example.com/"), null);
});

test("Vimeo: the player config's HLS stream with its subtitle files", () => {
  const [c] = plain(K.sites.vimeoConfig(JSON.parse(fixture("vimeo/config.json")), "https://example.com/blog"));
  assert.equal(c.kind, "hls");
  assert.equal(c.title, "The New Vimeo Player (You Know, For Videos)");
  assert.match(c.url, /^https:\/\/vod-adaptive-ak\.vimeocdn\.com\/.+\/playlist\.m3u8\?/);
  assert.deepEqual(c.ids, ["76979871"]);
  assert.equal(c.referrer, "https://example.com/blog");
  assert.equal(c.hint.subtitles.length, 4);
  assert.deepEqual(c.hint.subtitles.find((s) => s.lang === "en"), {
    url: c.hint.subtitles.find((s) => s.lang === "en").url,
    lang: "en",
    label: "English",
  });
  // The same from the player page's script.
  const html = `<script>window.playerConfig = ${fixture("vimeo/config.json")}; init();</script>`;
  assert.equal(K.sites.fromHtml("vimeo", html, "https://player.vimeo.com/video/76979871").length, 1);
});

test("TikTok: the best H.264 file of the page's item, with the page as referrer", () => {
  const page = "https://www.tiktok.com/@scout2015/video/6718335390845095173";
  const [c] = plain(K.sites.fromHtml("tiktok", fixture("tiktok/video.html"), page));
  assert.equal(c.kind, "file");
  assert.equal(c.key, "tiktok:6718335390845095173");
  assert.match(c.title, /^Scramble up ur name/);
  assert.equal(c.size, 2953029, "the highest H.264 bitrate");
  assert.equal(c.height, 1024);
  assert.equal(c.referrer, page);
  assert.match(c.filename, /\.mp4$/);
  // Item lists (scrolling) carry the same objects.
  const list = { itemList: [JSON.parse(fixture("tiktok/video.html").match(/>(\{.*\})</)[1]).__DEFAULT_SCOPE__["webapp.video-detail"].itemInfo.itemStruct] };
  assert.equal(K.sites.fromBody("https://www.tiktok.com/api/recommend/item_list/?x=1", JSON.stringify(list), page).length, 1);
});

test("Facebook: a DASH manifest beats the plain files; plain files when there is none", () => {
  const page = "https://www.facebook.com/cnn/videos/10155529876156509/";
  const found = plain(K.sites.fromBody("https://www.facebook.com/api/graphql/", fixture("facebook/graphql.ndjson"), page));
  assert.equal(found.length, 2);
  const a = found.find((c) => c.ids.includes("10155529876156509"));
  assert.equal(a.kind, "dash");
  assert.equal(a.title, "A news clip");
  assert.match(a.hint.manifest.body, /^<\?xml version="1.0"\?><MPD /);
  assert.match(a.hint.manifest.body, /hd_video\.mp4\?oe=X&amp;oh=X/);
  assert.equal(a.hint.manifest.base_url, page);
  assert.equal(a.url, page, "the page is the download's link");
  const b = found.find((c) => c.ids.includes("222333444555"));
  assert.equal(b.kind, "file");
  assert.equal(b.url, "https://video.example.fbcdn.net/v/t42/hd2.mp4?oe=X", "HD first");
  assert.equal(b.title, "Only a plain file");
});

test("Instagram: the reel's DASH manifest, else its largest file", () => {
  const page = "https://www.instagram.com/reel/Chunk8-jurw/";
  const found = plain(K.sites.fromBody("https://www.instagram.com/graphql/query", fixture("instagram/web_info.json"), page));
  assert.equal(found.length, 2);
  const a = found.find((c) => c.ids.includes("Chunk8-jurw"));
  assert.equal(a.kind, "dash");
  assert.equal(a.title, "Morning at the lake");
  const b = found.find((c) => c.ids.includes("CDUMkliABpa"));
  assert.equal(b.kind, "file");
  assert.equal(b.url, "https://scontent.example.cdninstagram.com/v/b1080.mp4");
  assert.equal(b.title, "Video by other");
});

test("YouTube: the formats become a DASH manifest the app reads", () => {
  const pr = JSON.parse(fixture("youtube/visionos.json"));
  const fmts = pr.streamingData.adaptiveFormats;
  const mpd = K.youtube.buildMpd(fmts, Number(pr.videoDetails.lengthSeconds));
  // The app's DASH parser reads this same file (crates/kstar-media, dash tests).
  const snapshot = path.join(here, "fixtures", "youtube", "visionos.mpd");
  if (process.env.UPDATE_SNAPSHOTS) fs.writeFileSync(snapshot, mpd);
  assert.equal(mpd, fs.readFileSync(snapshot, "utf8"));
  const reps = [...mpd.matchAll(/<Representation id="(\d+)"/g)].map((m) => m[1]);
  assert.equal(reps.length, K.youtube.usable(fmts).length);
  assert.ok(reps.includes("137") && reps.includes("140") && reps.includes("251"));
  assert.match(mpd, /mediaPresentationDuration="PT213\.000S"/);
  assert.match(mpd, /<Representation id="137" codecs="avc1\.640028" bandwidth="\d+" width="1920" height="1080" frameRate="25">/);
  assert.match(mpd, /<SegmentBase indexRange="\d+-\d+"><Initialization range="0-\d+"\/><\/SegmentBase>/);
  assert.match(mpd, /<BaseURL>https:\/\/rr1---sn-test\.googlevideo\.com\/videoplayback\?expire=1&amp;itag=137&amp;/);
});

test("YouTube: links without an index, DRC audio and OTF streams are left out", () => {
  const f = (o) => ({ itag: 1, url: "https://x/v", mimeType: 'video/mp4; codecs="avc1"', initRange: { start: "0", end: "9" }, indexRange: { start: "10", end: "19" }, ...o });
  const n = (fmts) => K.youtube.usable(fmts).length;
  assert.equal(n([f({})]), 1);
  assert.equal(n([f({ url: undefined })]), 0);
  assert.equal(n([f({ indexRange: undefined })]), 0);
  assert.equal(n([f({ isDrc: true })]), 0);
  assert.equal(n([f({ type: "FORMAT_STREAM_TYPE_OTF" })]), 0);
  assert.equal(K.youtube.buildMpd([f({ url: undefined })], 10), null);
});

test("YouTube: captions as WebVTT files, without the ones that need a token", () => {
  const pr = JSON.parse(fixture("youtube/visionos.json"));
  const tracks = pr.captions.playerCaptionsTracklistRenderer.captionTracks;
  const subs = plain(K.youtube.captions([...tracks, { baseUrl: "/api/timedtext?v=x&lang=fr&exp=xpe", languageCode: "fr", name: { simpleText: "French" } }]));
  assert.equal(subs.length, tracks.length);
  for (const s of subs) {
    const u = new URL(s.url);
    assert.equal(u.searchParams.get("fmt"), "vtt");
    assert.equal(u.searchParams.has("xosf"), false);
  }
  assert.ok(subs.some((s) => s.label.endsWith("(auto-generated)")));
  assert.ok(subs.some((s) => s.lang === "en" && !s.label.includes("auto")));
});

test("YouTube: the wait for pre-roll ads", () => {
  const ad = (secs, skip) => ({
    adPlacementRenderer: {
      config: { adPlacementConfig: { kind: "AD_PLACEMENT_KIND_START" } },
      renderer: { instreamVideoAdRenderer: { playerVars: `length_seconds=${secs}&x=1`, ...(skip != null ? { skipOffsetMilliseconds: skip } : {}) } },
    },
  });
  assert.equal(K.youtube.adWait({ adPlacements: [ad(15)] }), 15);
  assert.equal(K.youtube.adWait({ adPlacements: [ad(30, 5000), ad(6)] }), 11);
  const mid = { adPlacementRenderer: { config: { adPlacementConfig: { kind: "AD_PLACEMENT_KIND_MILLISECONDS" } }, renderer: { instreamVideoAdRenderer: { playerVars: "length_seconds=20" } } } };
  assert.equal(K.youtube.adWait({ adPlacements: [mid] }), 0);
  assert.equal(K.youtube.adWait({}), 0);
});

test("YouTube: the hand-over carries the manifest, ranges and the client's user agent", () => {
  const pr = JSON.parse(fixture("youtube/visionos.json"));
  const client = K.youtube.CLIENTS.visionos;
  const out = plain(K.youtube.handOver({ pr }, client, K.youtube.watchUrl("dQw4w9WgXcQ"), null));
  const item = out.item;
  assert.equal(item.url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
  assert.equal(item.user_agent, client.context.client.userAgent);
  assert.equal(item.media.site, "youtube");
  assert.equal(item.media.manifest.kind, "dash");
  assert.equal(item.media.chunk_bytes, 10 << 20);
  assert.equal(item.media.not_before_ms, null);
  assert.match(item.media.title, /Never Gonna Give You Up/);
  assert.ok(item.media.subtitles.length >= 1);
  const live = plain(K.youtube.handOver({ pr: { ...pr, videoDetails: { ...pr.videoDetails, isLive: true } } }, client, "u", null));
  assert.match(live.error, /live/);
});
