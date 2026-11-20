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

async function render() {
  const cfg = await kstarConfig();
  $("enabled").setAttribute("aria-checked", String(cfg.enabled));

  const [tab] = await browser.tabs.query({ active: true, currentWindow: true }).catch(() => []);
  siteHost = /^https?:/i.test(tab?.url ?? "") ? kstarHostOf(tab.url) : "";
  $("site-row").hidden = !siteHost;
  if (siteHost) {
    const blocked = kstarSiteBlocked(cfg, siteHost);
    $("site").textContent = siteHost.replace(/^www\./, "");
    $("site-on").setAttribute("aria-checked", String(!blocked));
    $("site-hint").textContent = blocked
      ? "Firefox handles this site's downloads"
      : "Turn off to let Firefox handle this site's downloads";
  }

  const last = await browser.runtime.sendMessage({ type: "last-error" }).catch(() => null);
  $("error").hidden = !last;
  if (last) $("error-text").textContent = last.message;

  const h = await browser.runtime.sendMessage({ type: "health" });
  $("setup").hidden = h.installed;
  $("outdated").hidden = !h.outdated;
  $("outdated").textContent =
    h.outdated === "app"
      ? `K-Star ${h.version ?? ""} is older than this extension. Update K-Star.`
      : h.outdated === "extension"
        ? `This extension is older than K-Star ${h.version ?? ""}. Update the extension.`
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
  const cfg = await kstarConfig();
  await browser.storage.local.set({ enabled: !cfg.enabled });
  render();
});

$("site-on").addEventListener("click", async () => {
  if (!siteHost) return;
  const cfg = await kstarConfig();
  await kstarSetSiteBlocked(siteHost, !kstarSiteBlocked(cfg, siteHost));
  render();
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
