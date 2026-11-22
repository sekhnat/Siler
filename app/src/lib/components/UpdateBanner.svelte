<script lang="ts">
  import { t } from "../i18n";
  import { ArrowUpCircle, X } from "@lucide/svelte";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import { api, type Settings, type UpdateInfo } from "../api";
  import { bytes } from "../format";
  import { toasts } from "../store.svelte";

  let { settings }: { settings: Settings | null } = $props();

  let info = $state<UpdateInfo | null>(null);
  let hidden = $state(false);
  let installing = $state(false);
  let progress = $state<{ done: number; total: number | null } | null>(null);

  const shown = $derived(!!info && info.newer && !hidden && info.latest !== settings?.skipped_version);

  onMount(() => {
    api.updateStatus().then((i) => (info = i)).catch(() => {});
    const un = [
      listen<UpdateInfo>("update-available", (e) => {
        info = e.payload;
        hidden = false;
      }),
      listen<{ done: number; total: number | null }>("update-progress", (e) => (progress = e.payload)),
    ];
    return () => un.forEach((p) => p.then((f) => f()));
  });

  async function update() {
    installing = true;
    progress = null;
    try {
      await api.installUpdate();
    } catch (e) {
      toasts.error(e);
      installing = false;
    }
  }

  async function skip() {
    if (!settings || !info) return;
    try {
      await api.setSettings({ ...settings, skipped_version: info.latest });
      hidden = true;
    } catch (e) {
      toasts.error(e);
    }
  }
</script>

{#if shown && info}
  <div class="banner">
    <ArrowUpCircle size={16} />
    <span class="grow">
      <b>{t("update-banner-siler-is-available", { latest: info.latest })}</b>
      {#if installing}
        {progress?.total ? t("update-banner-downloading-of", { done: bytes(progress.done), total: bytes(progress.total) }) : progress ? t("update-banner-downloading-done", { done: bytes(progress.done) }) : t("update-banner-downloading")}
      {:else if info.install === "packaged"}
        {t("update-banner-update-it-with-your-package-manager")}
      {:else if info.install === "app_image" && !info.appimage}
        {t("update-banner-this-release-has-no-appimage-yet")}
      {:else}
        {t("update-banner-you-have", { current: info.current })}
      {/if}
    </span>
    <button class="btn btn-sm btn-ghost" onclick={() => api.openUrl(info!.url).catch(toasts.error.bind(toasts))}>{t("update-banner-what-s-new")}</button>
    {#if info.install === "app_image" && info.appimage && info.sha256}
      <button class="btn btn-sm btn-primary" disabled={installing} onclick={update}>{t("update-banner-update-and-restart")}</button>
    {:else if info.install === "tarball"}
      <button class="btn btn-sm btn-primary" onclick={() => api.openUrl(info!.url).catch(toasts.error.bind(toasts))}>{t("update-banner-download")}</button>
    {/if}
    {#if !installing}
      <button class="btn btn-sm btn-ghost" onclick={skip}>{t("update-banner-skip-this-version")}</button>
      <button class="icon-btn sm" title={t("update-banner-not-now")} onclick={() => (hidden = true)}><X size={14} /></button>
    {/if}
  </div>
{/if}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 0 14px 10px;
    padding: 10px 14px;
    border-radius: var(--r-lg);
    background: var(--grad-soft);
    border: 1px solid var(--selected-border);
    font-size: 12.5px;
  }
  .banner > :global(svg) {
    color: var(--accent);
    flex: none;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
</style>
