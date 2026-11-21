"use strict";

const $ = (id) => document.getElementById(id);

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

/** The videos Siler found on the tab, to pick from. */
async function renderVideos(fresh = false) {
  if (tabId == null) return;
  const list = await browser.runtime.sendMessage({ type: "media-list", tabId, fresh }).catch(() => []);
  // Keep what the user ticked when the list is refreshed.
  const ticked = new Set([...document.querySelectorAll("#video-list input:checked")].map((i) => i.value));
  const touched = document.querySelector("#video-list input") !== null;
  const box = $("videos");
  box.hidden = !list?.length;
  if (!list?.length) return;
  $("videos-count").textContent = list.length === 1 ? "" : `${list.length} found`;
  const ul = $("video-list");
  ul.replaceChildren(
    ...list.map((v, i) => {
      const li = document.createElement("li");
      const label = document.createElement("label");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.value = v.key;
      box.checked = touched ? ticked.has(v.key) : i === 0;
      box.addEventListener("change", syncButton);
      const text = document.createElement("span");
      text.className = "v-text";
      const name = document.createElement("span");
      name.className = "v-name";
      name.textContent = v.name;
      name.title = v.name;
      const meta = document.createElement("span");
      meta.className = "v-label";
      meta.textContent = v.label;
      text.append(name, meta);
      label.append(box, text);
      li.append(label);
      return li;
    }),
  );
  syncButton();
}

function syncButton() {
  const n = document.querySelectorAll("#video-list input:checked").length;
  $("video-download").disabled = n === 0;
  $("video-download").textContent = n > 1 ? `Download ${n} with Siler` : "Download with Siler";
}

$("video-download").addEventListener("click", async () => {
  const keys = [...document.querySelectorAll("#video-list input:checked")].map((i) => i.value);
  if (!keys.length) return;
  const btn = $("video-download");
  const msg = $("video-msg");
  btn.disabled = true;
  btn.textContent = "Sending to Siler…";
  msg.hidden = true;
  msg.classList.remove("err");
  const r = await browser.runtime.sendMessage({ type: "media-download", tabId, keys }).catch((e) => ({ ok: false, message: String(e?.message ?? e) }));
  if (r?.ok) {
    window.close();
    return;
  }
  msg.textContent = r?.message || "Siler couldn't take the video.";
  msg.classList.add("err");
  msg.hidden = false;
  syncButton();
});

async function render() {
  const cfg = await silerConfig();
  $("enabled").setAttribute("aria-checked", String(cfg.enabled));

  const [tab] = await browser.tabs.query({ active: true, currentWindow: true }).catch(() => []);
  tabId = tab?.id ?? null;
  siteHost = /^https?:/i.test(tab?.url ?? "") ? silerHostOf(tab.url) : "";
  $("site-row").hidden = !siteHost;
  if (siteHost) {
    const blocked = silerSiteBlocked(cfg, siteHost);
    $("site").textContent = siteHost.replace(/^www\./, "");
    $("site-on").setAttribute("aria-checked", String(!blocked));
    $("site-hint").textContent = blocked
      ? "Firefox handles this site's downloads and videos"
      : "Turn off to let Firefox handle this site's downloads and videos";
  }

  const last = await browser.runtime.sendMessage({ type: "last-error" }).catch(() => null);
  $("error").hidden = !last;
  if (last) $("error-text").textContent = last.message;

  const h = await browser.runtime.sendMessage({ type: "health" });
  $("setup").hidden = h.installed;
  $("outdated").hidden = !h.outdated;
  $("outdated").textContent =
    h.outdated === "app"
      ? `Siler ${h.version ?? ""} is older than this extension. Update Siler.`
      : h.outdated === "extension"
        ? `This extension is older than Siler ${h.version ?? ""}. Update the extension.`
        : "";
  $("dot").className = "dot " + (h.running ? "ok" : h.installed ? "warn" : "err");
  $("status").textContent = h.running
    ? `Connected · v${h.version}`
    : h.installed
      ? "Not running — starts when needed"
      : "Not installed";

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
