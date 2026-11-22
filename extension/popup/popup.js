"use strict";

const $ = (id) => document.getElementById(id);
silerLocalize();

function fmtSpeed(n) {
  if (!n) return "0 B/s";
  const u = ["B", "KB", "MB", "GB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n >= 100 ? n.toFixed(0) : n.toFixed(1)} ${u[i]}/s`;
}

let siteHost = "";
let tabId = null;
// The tab the popup belongs to. Both the status panel and the video list wait for it: the list
// is asked for as soon as the popup opens, before the panel has finished.
const tabLookup = browser.tabs
  .query({ active: true, currentWindow: true })
  .then(([tab]) => {
    tabId = tab?.id ?? null;
    return tab ?? null;
  })
  .catch(() => null);

let showAds = false;
$("video-ads").addEventListener("click", () => {
  showAds = !showAds;
  renderVideos();
});

/** The videos Siler found on the tab, to pick from. */
async function renderVideos(fresh = false) {
  await tabLookup;
  if (tabId == null) return;
  const list = await browser.runtime.sendMessage({ type: "media-list", tabId, fresh }).catch(() => []);
  // Keep what the user ticked when the list is refreshed.
  const ticked = new Set([...document.querySelectorAll("#video-list input:checked")].map((i) => i.value));
  const touched = document.querySelector("#video-list input") !== null;
  const box = $("videos");
  box.hidden = !list?.length;
  if (!list?.length) return;
  const ads = list.filter((v) => v.ad).length;
  const shown = showAds ? list.length : list.length - ads;
  $("videos-count").textContent = shown <= 1 ? "" : silerT("popupFound", [String(shown)]);
  // Ads are listed last and kept out of sight until asked for.
  $("video-ads").hidden = !ads;
  $("video-ads").textContent = showAds ? silerT("popupHideAds") : silerT("popupShowAds", [String(ads)]);
  const ul = $("video-list");
  const firstFree = list.findIndex((v) => !v.protected && !v.ad);
  ul.replaceChildren(
    ...list.map((v, i) => {
      const li = document.createElement("li");
      const label = document.createElement("label");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.value = v.key;
      // A DRM-protected video is listed to say why it can't be had, never to be ticked.
      box.disabled = !!v.protected;
      box.checked = !v.protected && (touched ? ticked.has(v.key) : i === firstFree);
      box.addEventListener("change", syncButton);
      const text = document.createElement("span");
      text.className = "v-text";
      const name = document.createElement("span");
      name.className = "v-name";
      name.textContent = v.protected ? `🔒 ${v.name}` : v.name;
      name.title = v.name;
      const meta = document.createElement("span");
      meta.className = "v-label";
      meta.textContent = v.label;
      text.append(name, meta);
      label.append(box, text);
      if (v.protected) {
        li.className = "v-locked";
        label.title = silerT("popupDRMProtected");
      }
      li.hidden = !!v.ad && !showAds;
      if (v.poster) {
        const img = document.createElement("img");
        img.className = "v-thumb";
        img.alt = "";
        img.referrerPolicy = "no-referrer";
        img.loading = "lazy";
        img.src = v.poster;
        label.insertBefore(img, text);
      }
      li.append(label);
      if (v.link) {
        const copy = document.createElement("button");
        copy.type = "button";
        copy.className = "v-copy";
        copy.textContent = "⧉";
        copy.title = silerT("popupCopyLink");
        copy.setAttribute("aria-label", silerT("popupCopyLink"));
        copy.addEventListener("click", async () => {
          try {
            await navigator.clipboard.writeText(v.link);
            copy.title = silerT("popupCopied");
            copy.textContent = "✓";
          } catch {
            /* the clipboard said no */
          }
        });
        li.append(copy);
      }
      return li;
    }),
  );
  syncButton();
}

function syncButton() {
  const n = document.querySelectorAll("#video-list input:checked").length;
  $("video-download").disabled = n === 0;
  $("video-download").textContent = n > 1 ? silerT("popupDownloadN", [String(n)]) : silerT("menuDownload");
}

$("video-download").addEventListener("click", async () => {
  const keys = [...document.querySelectorAll("#video-list input:checked")].map((i) => i.value);
  if (!keys.length) return;
  const btn = $("video-download");
  const msg = $("video-msg");
  btn.disabled = true;
  btn.textContent = silerT("popupSending");
  msg.hidden = true;
  msg.classList.remove("err");
  const r = await browser.runtime.sendMessage({ type: "media-download", tabId, keys }).catch((e) => ({ ok: false, message: String(e?.message ?? e) }));
  if (r?.ok) {
    window.close();
    return;
  }
  msg.textContent = r?.message || silerT("popupCouldntTakeVideo");
  msg.classList.add("err");
  msg.hidden = false;
  syncButton();
});

async function render() {
  const cfg = await silerConfig();
  $("enabled").setAttribute("aria-checked", String(cfg.enabled));

  const tab = await tabLookup;
  siteHost = /^https?:/i.test(tab?.url ?? "") ? silerHostOf(tab.url) : "";
  $("site-row").hidden = !siteHost;
  if (siteHost) {
    const blocked = silerSiteBlocked(cfg, siteHost);
    $("site").textContent = siteHost.replace(/^www\./, "");
    $("site-on").setAttribute("aria-checked", String(!blocked));
    $("site-hint").textContent = blocked
      ? silerT("popupBrowserHandlesSite")
      : silerT("popupTurnOffToLetBrowser");
  }

  const last = await browser.runtime.sendMessage({ type: "last-error" }).catch(() => null);
  $("error").hidden = !last;
  if (last) $("error-text").textContent = last.message;

  const h = await browser.runtime.sendMessage({ type: "health" });
  $("setup").hidden = h.installed;
  $("outdated").hidden = !h.outdated;
  $("outdated").textContent =
    h.outdated === "app"
      ? silerT("popupAppOlder", [h.version ?? ""])
      : h.outdated === "extension"
        ? silerT("popupExtensionOlder", [h.version ?? ""])
        : "";
  $("dot").className = "dot " + (h.running ? "ok" : h.installed ? "warn" : "err");
  $("status").textContent = h.running
    ? silerT("popupConnected", [h.version ?? ""])
    : h.installed
      ? silerT("popupNotRunning")
      : silerT("popupNotInstalled");

  if (h.running) {
    const s = await browser.runtime.sendMessage({ type: "status" });
    if (s?.type === "status") {
      $("stats").hidden = false;
      $("active").textContent = s.active;
      $("queued").textContent = s.queued;
      $("speed").textContent = fmtSpeed(s.speed);
    }
  }
}

$("enabled").addEventListener("click", async () => {
  const cfg = await silerConfig();
  await browser.storage.local.set({ enabled: !cfg.enabled });
  render();
});

$("site-on").addEventListener("click", async () => {
  if (!siteHost) return;
  const cfg = await silerConfig();
  await silerSetSiteBlocked(siteHost, !silerSiteBlocked(cfg, siteHost));
  render();
  renderVideos(true);
});

$("error-close").addEventListener("click", async () => {
  await browser.runtime.sendMessage({ type: "clear-error" });
  $("error").hidden = true;
});

$("open").addEventListener("click", async () => {
  await browser.runtime.sendMessage({ type: "show" });
  window.close();
});

$("options").addEventListener("click", () => {
  browser.runtime.openOptionsPage();
  window.close();
});

$("add").addEventListener("submit", async (e) => {
  e.preventDefault();
  const url = $("url").value.trim();
  if (!/^https?:\/\//i.test(url)) return;
  try {
    await browser.runtime.sendMessage({ type: "add-url", url });
    window.close();
  } catch (err) {
    $("status").textContent = String(err?.message ?? err);
  }
});

render();
// What is known now, then again after looking at the page.
renderVideos().then(() => renderVideos(true));
