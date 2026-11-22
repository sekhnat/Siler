// Chromium's background service worker: the same scripts Firefox loads as its background
// page (manifest.json → background.scripts), in the same order. A test keeps them equal.
importScripts(
  "compat.js",
  "shared.js",
  "media/common.js",
  "media/sites.js",
  "media/youtube/build.js",
  "media/youtube/page.js",
  "media/feed.js",
  "background.js",
);
