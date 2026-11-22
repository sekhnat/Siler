<script lang="ts">
  import { t } from "../i18n";
  import { ArrowLeft, FileArchive, Folder, FolderOpen, LoaderCircle, Lock, Search, TriangleAlert, X } from "@lucide/svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";
  import { api, errorText, type ZipItem, type ZipListing } from "../api";
  import { bytes } from "../format";
  import { toasts } from "../store.svelte";
  import QueuePicker from "./QueuePicker.svelte";

  let {
    url,
    pendingId = null,
    archiveName,
    baseDir,
    queueId = $bindable(),
    onback,
    ondone,
  }: {
    url: string;
    pendingId?: number | null;
    /** The archive's file name: picked files go into a folder named after it. */
    archiveName: string;
    /** The folder the archive itself would have gone to. */
    baseDir: string;
    queueId: number;
    onback: () => void;
    ondone: () => void;
  } = $props();

  let listing = $state<ZipListing | null>(null);
  let loadError = $state("");
  let picked = $state<Record<string, boolean>>({});
  let query = $state("");
  let busy = $state(false);
  // svelte-ignore state_referenced_locally
  let dir = $state(`${baseDir.replace(/\/+$/, "")}/${archiveName.replace(/\.zip$/i, "")}`);

  onMount(async () => {
    try {
      listing = await api.zipList(url, pendingId);
    } catch (e) {
      loadError = errorText(e);
    }
  });

  const files = $derived((listing?.entries ?? []).filter((e) => !e.is_dir));
  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return q ? files.filter((f) => f.path.toLowerCase().includes(q)) : files;
  });
  const chosen = $derived(files.filter((f) => picked[f.path] && !f.unsupported));
  const chosenSize = $derived(chosen.reduce((s, f) => s + f.size, 0));
  const fetchSize = $derived(chosen.reduce((s, f) => s + f.compressed, 0));
  const total = $derived(files.reduce((s, f) => s + f.size, 0));
  const shownOn = $derived(shown.length > 0 && shown.filter((f) => !f.unsupported).every((f) => picked[f.path]));

  function setShown(on: boolean) {
    for (const f of shown) if (!f.unsupported) picked[f.path] = on;
  }

  function folderOf(p: string): string {
    const i = p.lastIndexOf("/");
    return i > 0 ? p.slice(0, i) : "";
  }
  function nameOf(p: string): string {
    return p.slice(p.lastIndexOf("/") + 1);
  }

  async function browse() {
    const got = await open({ directory: true, defaultPath: dir || undefined, title: t("zip-preview-save-the-files-to") });
    if (typeof got === "string") dir = got;
  }

  async function submit(startPaused: boolean) {
    if (!listing || !chosen.length || busy) return;
    busy = true;
    try {
      const n = await api.addZipEntries({
        url: listing.url,
        archiveSize: listing.archive_size,
        dir: dir.trim(),
        entries: chosen as ZipItem[],
        pendingId,
        queueId,
        startPaused,
      });
      toasts.show(t("zip-preview-added", { count: n }), "success");
      ondone();
    } catch (e) {
      toasts.error(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="zip">
  <div class="head">
    <button class="icon-btn" onclick={onback} aria-label={t("zip-preview-back")}><ArrowLeft size={16} /></button>
    <div class="min">
      <h2 class="truncate"><FileArchive size={16} /> {archiveName}</h2>
      <div class="hint">
        {#if listing}{t("zip-preview-files-unpacked-pick-the-ones-you", { length: files.length, v: bytes(total) })}{:else}{t("zip-preview-reading-the-archive-s-list-of")}{/if}
      </div>
    </div>
  </div>

  {#if loadError}
    <div class="err"><TriangleAlert size={15} /> {loadError}</div>
  {:else if !listing}
    <div class="loading"><LoaderCircle size={18} class="spin" /></div>
  {:else}
    <label class="search">
      <Search size={14} />
      <input placeholder={t("zip-preview-filter-by-name")} bind:value={query} spellcheck="false" />
      {#if query}<button class="icon-btn sm" onclick={() => (query = "")} aria-label={t("zip-preview-clear")}><X size={13} /></button>{/if}
    </label>
    <div class="list">
      <div class="row head-row">
        <input type="checkbox" checked={shownOn} onchange={() => setShown(!shownOn)} aria-label={t("zip-preview-select-all-shown")} />
        <span>{t("zip-preview-of-selected", { length: chosen.length, length2: files.length })}{chosen.length ? t("zip-preview-size-to-fetch", { size: bytes(chosenSize), fetch: bytes(fetchSize) }) : ""}</span>
      </div>
      {#each shown as f (f.path)}
        <label class="row" class:off={!!f.unsupported} title={f.unsupported ?? f.path}>
          <input type="checkbox" bind:checked={picked[f.path]} disabled={!!f.unsupported} />
          <span class="name truncate">
            {#if folderOf(f.path)}<span class="folder"><Folder size={11} /> {folderOf(f.path)}/</span>{/if}{nameOf(f.path)}
          </span>
          {#if f.unsupported}<Lock size={12} />{/if}
          <span class="size">{bytes(f.size)}</span>
        </label>
      {:else}
        <div class="hint none">{t("zip-preview-nothing-matches-the-filter")}</div>
      {/each}
    </div>
    <div class="field">
      <span class="label">{t("zip-preview-save-to")} <span class="hint">{t("zip-preview-folders-inside-the-archive-are-kept")}</span></span>
      <div class="folder-pick">
        <input class="input" bind:value={dir} spellcheck="false" />
        <button class="btn" onclick={browse} title={t("zip-preview-choose-folder")}><FolderOpen size={15} /></button>
      </div>
    </div>
  {/if}

  <div class="actions">
    <button class="btn btn-ghost" onclick={onback}>{t("zip-preview-back")}</button>
    <span class="grow"></span>
    <QueuePicker bind:value={queueId} />
    <button class="btn" disabled={!chosen.length || busy} onclick={() => submit(true)}>{t("zip-preview-download-later")}</button>
    <button class="btn btn-primary" disabled={!chosen.length || busy} onclick={() => submit(false)}>{t("zip-preview-download-selected")}</button>
  </div>
</div>

<style>
  .zip {
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-height: 0;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .min {
    min-width: 0;
    flex: 1;
  }
  h2 {
    margin: 0;
    font-size: 17px;
    font-weight: 700;
    letter-spacing: -0.02em;
    display: flex;
    align-items: center;
    gap: 7px;
  }
  .err {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    color: var(--err);
    font-weight: 550;
    padding: 12px;
    border-radius: var(--r-lg);
    background: color-mix(in srgb, var(--err) 8%, transparent);
  }
  .loading {
    display: flex;
    justify-content: center;
    padding: 40px;
    color: var(--muted);
  }
  .loading :global(.spin) {
    animation: spin 0.9s linear infinite;
  }
  .search {
    position: relative;
    display: flex;
    align-items: center;
  }
  .search > :global(svg) {
    position: absolute;
    left: 10px;
    color: var(--muted);
  }
  .search input {
    width: 100%;
    height: 34px;
    padding: 0 32px;
    border-radius: var(--r);
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--text);
  }
  .search .icon-btn {
    position: absolute;
    right: 4px;
  }
  .list {
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    max-height: 280px;
    overflow: auto;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 12px;
    border-bottom: 1px solid var(--border);
    font-size: 12.5px;
  }
  .row:last-child {
    border-bottom: none;
  }
  .row.off {
    opacity: 0.55;
  }
  .head-row {
    position: sticky;
    top: 0;
    background: var(--surface-2);
    font-weight: 600;
    color: var(--text-2);
    z-index: 1;
  }
  .name {
    flex: 1;
    min-width: 0;
  }
  .folder {
    color: var(--muted);
  }
  .folder :global(svg) {
    vertical-align: -1px;
  }
  .size {
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .none {
    padding: 14px;
  }
  .folder-pick {
    display: flex;
    gap: 6px;
  }
  .folder-pick .btn {
    padding: 0 10px;
    height: 36px;
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  .grow {
    flex: 1;
  }
</style>
