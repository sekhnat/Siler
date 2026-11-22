// Player probes and page metadata: `node --test extension/tests/*.test.mjs`.
// silerPlayerProbe runs against stand-in page windows; Siler.players against its answers and
// against what collectPageData reads from a page's metadata.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const ext = path.join(here, "..");
const fixture = (p) => JSON.parse(fs.readFileSync(path.join(here, "fixtures", p), "utf8"));

const ctx = vm.createContext({ URL, URLSearchParams, console });
for (const f of ["media/common.js", "media/players.js"]) vm.runInContext(fs.readFileSync(path.join(ext, f), "utf8"), ctx, { filename: f });
const P = ctx.Siler.players;
const plain = (v) => JSON.parse(JSON.stringify(v));

/** A stand-in page window with the given globals and `.jwplayer` element ids. */
function page(globals, ids = []) {
  return {
    location: { href: "https://news.example/story/42" },
    document: { querySelectorAll: () => ids.map((id) => ({ id })) },
    ...globals,
  };
}

test("JW Player: each playlist item's sources, title, poster and captions", () => {
  const players = {
    "jw-1": {
      getPlaylist: () => [
        {
          title: "Storm hits the coast",
          image: "/thumbs/42.jpg",
          sources: [
            { file: "https://cdn.news.example/42/master.m3u8", type: "hls" },
            { file: "/media/42-720.mp4", type: "video/mp4", label: "720p" },
          ],
          tracks: [
            { file: "/subs/42-en.vtt", kind: "captions", label: "English", language: "en" },
            { file: "/thumbs/42.vtt", kind: "thumbnails" },
          ],
        },
      ],
    },
  };
  const answer = plain(ctx.silerPlayerProbe(page({ jwplayer: (id) => players[id] }, ["jw-1"])));
  assert.equal(answer.players.length, 1);
  const found = plain(P.fromPlayers(answer, 3));
  assert.deepEqual(
    found.map((c) => [c.kind, c.url, c.height, c.title, c.frameId]),
    [
      ["hls", "https://cdn.news.example/42/master.m3u8", null, "Storm hits the coast", 3],
      ["file", "https://news.example/media/42-720.mp4", 720, "Storm hits the coast", 3],
    ],
  );
  assert.equal(found[0].poster, "https://news.example/thumbs/42.jpg");
  // Captions only: a thumbnails track isn't subtitles.
  assert.deepEqual(plain(P.tracksOf(answer)), [{ url: "https://news.example/subs/42-en.vtt", lang: "en", label: "English" }]);
});

test("Video.js (Brightcove): the registry's current sources and media info", () => {
  const player = {
    mediainfo: { name: "Interview", poster: "https://cf.example/p.jpg" },
    currentSources: () => [{ src: "https://cf.example/v/manifest.mpd", type: "application/dash+xml" }],
    poster: () => "",
    remoteTextTracks: () => [{ src: "https://cf.example/v/fr.vtt", kind: "subtitles", language: "fr", label: "Français" }],
  };
  const videojs = Object.assign(() => player, { getPlayers: () => ({ main: player }) });
  const answer = plain(ctx.silerPlayerProbe(page({ videojs })));
  const [c] = plain(P.fromPlayers(answer));
  assert.equal(c.kind, "dash");
  assert.equal(c.title, "Interview");
  assert.equal(c.poster, "https://cf.example/p.jpg");
  assert.equal(plain(P.tracksOf(answer))[0].lang, "fr");
});

test("Flowplayer: its instance list", () => {
  const flowplayer = { instances: [{ video: { src: "https://v.example/a.webm", title: "A" } }] };
  const [c] = plain(P.fromPlayers(plain(ctx.silerPlayerProbe(page({ flowplayer })))));
  assert.deepEqual([c.kind, c.url, c.title], ["file", "https://v.example/a.webm", "A"]);
});

test("a page that guards its player, or has none, gives nothing and doesn't throw", () => {
  const hostile = page({});
  Object.defineProperty(hostile, "jwplayer", {
    get() {
      throw new Error("no");
    },
  });
  assert.deepEqual(plain(ctx.silerPlayerProbe(hostile).players), []);
  assert.deepEqual(plain(ctx.silerPlayerProbe(page({})).players), []);
  // Only web addresses become candidates.
  const blob = { jwplayer: () => ({ getPlaylist: () => [{ file: "blob:https://x/1", title: "T" }] }) };
  assert.deepEqual(plain(P.fromPlayers(plain(ctx.silerPlayerProbe(page(blob)))), []), []);
});

test("OpenGraph and JSON-LD: the video before it plays, named by the page", () => {
  const meta = fixture("generic/meta-news.json");
  const m = plain(P.fromMeta(meta, 0));
  const urls = m.found.map((c) => [c.kind, c.url, c.title]);
  assert.deepEqual(urls, [
    ["file", "https://cdn.news.example/videos/storm.mp4", "Storm hits the coast"],
    ["hls", "https://cdn.news.example/videos/storm/master.m3u8", "Storm hits the coast — full report"],
    ["file", "https://news.example/media/teaser.webm", "Storm hits the coast"],
  ]);
  const hls = m.found[1];
  assert.equal(hls.duration, 754);
  assert.equal(hls.poster, "https://cdn.news.example/thumbs/storm-hd.jpg");
  // An embed address isn't a video, but its name is kept for when the embed's stream shows up.
  assert.equal(m.titles["player.example/embed/9"], "The embedded clip");
  assert.deepEqual(m.tracks, [{ url: "https://news.example/subs/storm.en.vtt", lang: "en", label: "English" }]);
});

test("a page with only an og:image lists nothing", () => {
  const m = plain(P.fromMeta({ url: "https://shop.example/", title: "Shop", og: { image: "/a.jpg", title: "Shop" }, jsonld: [], videos: [], tracks: [] }));
  assert.deepEqual(m.found, []);
});

test("ISO 8601 durations", () => {
  assert.equal(P.seconds("PT12M34S"), 754);
  assert.equal(P.seconds("PT1H"), 3600);
  assert.equal(P.seconds("P1DT1S"), 86401);
  assert.equal(P.seconds("bogus"), 0);
});
