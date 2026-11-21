"use strict";

const $ = (id) => document.getElementById(id);
const SWITCHES = [
  "enabled",
  "captureAll",
  "captureByMime",
  "notifyFailures",
  "findVideos",
  "videoButton",
  "site_youtube",
  "site_vimeo",
  "site_facebook",
  "site_instagram",
  "site_tiktok",
];
const FIELDS = ["extensions", "minSizeMB", "blocklist"];
const ALL_SITES = { origins: ["<all_urls>"] };

async function load() {
  const cfg = await silerConfig();
  for (const k of SWITCHES) $(k).setAttribute("aria-checked", String(cfg[k]));
  for (const k of FIELDS) $(k).value = cfg[k];
}

for (const k of SWITCHES) {
  $(k).addEventListener("click", async () => {
    const on = $(k).getAttribute("aria-checked") !== "true";
    $(k).setAttribute("aria-checked", String(on));
    await browser.storage.local.set({ [k]: on });
  });
}

$("save").addEventListener("click", async () => {
  await browser.storage.local.set({
    extensions: $("extensions").value.trim(),
    minSizeMB: Math.max(0, Number($("minSizeMB").value) || 0),
    blocklist: $("blocklist").value.trim(),
  });
  $("saved").hidden = false;
  setTimeout(() => ($("saved").hidden = true), 1500);
});

$("reset").addEventListener("click", async () => {
  await browser.storage.local.clear();
  await load();
});

async function checkSetup() {
  const granted = await browser.permissions.contains(ALL_SITES);
  $("grant").hidden = granted;
  $("perm-ok").hidden = !granted;

  const h = await silerHealth();
  $("host-state").textContent = h.running ? "✓ Connected" : h.installed ? "✓ Installed (Siler starts on demand)" : "✗ Not found";
  $("host-state").className = h.installed ? "ok" : "muted";
  $("welcome").hidden = granted && h.installed;
  $("outdated").hidden = !h.outdated;
  $("outdated").textContent =
    h.outdated === "app"
      ? `Siler ${h.version ?? ""} is older than this extension (${browser.runtime.getManifest().version}). Update Siler so they work together.`
      : h.outdated === "extension"
        ? `This extension (${browser.runtime.getManifest().version}) is older than Siler ${h.version ?? ""}. Update the extension.`
        : "";
}

$("grant").addEventListener("click", async () => {
  await browser.permissions.request(ALL_SITES);
  checkSetup();
});

load();
checkSetup();
