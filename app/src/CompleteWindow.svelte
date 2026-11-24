<script lang="ts">
  // IDM's "Download complete" window: where the file went, and what to do with it.
  import { onMount } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { AppWindow, ExternalLink, FolderOpen } from "@lucide/svelte";
  import { t } from "./lib/i18n";
  import { api } from "./lib/api";
  import { bytes, nameFromUrl } from "./lib/format";
  import { LiveDownload } from "./lib/live.svelte";
  import { attempt, toasts } from "./lib/store.svelte";
  import TitleBar from "./lib/components/TitleBar.svelte";
  import FileIcon from "./lib/components/FileIcon.svelte";
  import Toasts from "./lib/components/Toasts.svelte";

  let { id }: { id: number } = $props();

  const win = getCurrentWindow();
  // svelte-ignore state_referenced_locally
  const live = new LiveDownload(id);
  const d = $derived(live.d);
  let dontShow = $state(false);

  const name = $derived(d ? (d.filename ?? nameFromUrl(d.url)) : "");
  const path = $derived(d?.dir && d.filename ? `${d.dir}/${d.filename}` : (d?.dir ?? ""));

  $effect(() => {
    if (live.gone) win.destroy();
  });

  async function close() {
    if (dontShow) await attempt(api.setCompleteDialog(false));
    win.destroy();
  }

  /** Runs an action on the file, then closes like IDM does. */
  async function then(p: Promise<void>) {
    try {
      await p;
      await close();
    } catch (e) {
      toasts.error(e);
    }
  }

  onMount(() => {
    const stop = live.start();
    live.reload();
    return stop;
  });
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && close()} />

<div class="window">
  <TitleBar title={t("complete-title")} onclose={close} closeLabel={t("progress-close")} />
  {#if d}
    <div class="content">
      <div class="head">
        <FileIcon category={d.category} size={44} />
        <div class="min">
          <div class="big">{t("complete-heading")}</div>
          <div class="name selectable truncate" title={name}>{name}</div>
        </div>
      </div>
      <dl>
        <dt>{t("complete-downloaded")}</dt>
        <dd class="num">{bytes(d.total_size ?? d.downloaded)}</dd>
        <dt>{t("complete-address")}</dt>
        <dd class="selectable truncate" title={d.url}>{d.url}</dd>
        <dt>{t("complete-saved-as")}</dt>
        <dd class="selectable truncate" title={path}>{path}</dd>
      </dl>
      <span class="grow"></span>
      <label class="check">
        <input type="checkbox" bind:checked={dontShow} />
        {t("complete-dont-show")}
      </label>
      <div class="buttons">
        <!-- svelte-ignore a11y_autofocus -->
        <button class="btn btn-sm btn-primary" autofocus disabled={d.file_missing} onclick={() => then(api.openFile(id))}><ExternalLink size={14} />{t("complete-open")}</button>
        <button class="btn btn-sm" disabled={d.file_missing} onclick={() => then(api.openWith(id))}><AppWindow size={14} />{t("complete-open-with")}</button>
        <button class="btn btn-sm" onclick={() => then(api.showInFolder(id))}><FolderOpen size={14} />{t("complete-open-folder")}</button>
        <span class="grow"></span>
        <button class="btn btn-sm" onclick={close}>{t("progress-close")}</button>
      </div>
    </div>
  {/if}
</div>

<Toasts />

<style>
  .window {
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    overflow: hidden;
  }
  .content {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 14px 16px;
  }
  .head {
    display: flex;
    gap: 12px;
    align-items: center;
  }
  .min {
    min-width: 0;
  }
  .big {
    font-size: 15px;
    font-weight: 700;
    color: var(--ok);
  }
  .name {
    font-size: 13px;
    color: var(--text);
  }
  dl {
    display: grid;
    grid-template-columns: 110px 1fr;
    gap: 4px 10px;
    margin: 0;
    font-size: 12.5px;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    min-width: 0;
  }
  .num {
    font-variant-numeric: tabular-nums;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12.5px;
    color: var(--text-2);
  }
  .buttons {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .grow {
    flex: 1;
  }
</style>
