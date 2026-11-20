<script lang="ts">
  import { Film, FolderOpen, KeyRound, ListOrdered, LoaderCircle, Pause, Play, RefreshCw, RotateCw } from "@lucide/svelte";
  import { api, MAIN_QUEUE, type Download } from "../api";
  import { STATUS_COLOR, STATUS_LABEL } from "../categories";
  import { bytes, eta, host, nameFromUrl, percent, relativeTime, speed, syncLabel } from "../format";
  import { attempt, store } from "../store.svelte";
  import FileIcon from "./FileIcon.svelte";
  import ProgressBar from "./ProgressBar.svelte";

  let { d, selected }: { d: Download; selected: boolean } = $props();

  const done = $derived(d.status === "completed");
  const mp = $derived(d.media_progress ?? null);
  /** A media download has no known size: its progress is its phase. */
  function mediaPct(m: NonNullable<typeof mp>): number {
    switch (m.phase) {
      case "downloading":
        return m.segments_total ? (88 * m.segments_done) / m.segments_total : 0;
      case "rebuilding":
        return 88 + (m.permille / 1000) * 10;
      case "publishing":
        return 99;
      default:
        return 0;
    }
  }
  const pct = $derived(done ? 100 : mp ? mediaPct(mp) : percent(d.total_size, d.downloaded));
  const name = $derived(d.filename ?? nameFromUrl(d.url));
  const phaseText = $derived.by(() => {
    if (!mp) return null;
    switch (mp.phase) {
      case "resolving":
        return "Reading the stream…";
      case "downloading":
        return `Pieces ${mp.segments_done} of ${mp.segments_total}`;
      case "rebuilding":
        return `Rebuilding ${(mp.permille / 10).toFixed(0)}%`;
      default:
        return "Saving…";
    }
  });
  const sizeText = $derived.by(() => {
    if (done) return bytes(d.total_size);
    if (phaseText) return phaseText;
    if (d.total_size) return `${bytes(d.downloaded)} of ${bytes(d.total_size)}`;
    return d.downloaded ? bytes(d.downloaded) : "Unknown size";
  });
  const statusColor = $derived(d.needs_login ? "var(--accent)" : STATUS_COLOR[d.status]);

  function askLogin(e: MouseEvent) {
    e.stopPropagation();
    window.dispatchEvent(new CustomEvent("kstar-login", { detail: d.id }));
  }
</script>

<div class="row" class:selected class:done>
  <FileIcon category={d.category} size={40} />

  <div class="main">
    <div class="top">
      <span class="name truncate" title={name}>{name}</span>
      {#if d.file_missing}
        <span class="pill" style:--c="var(--warn)" title="The file was moved or deleted">File missing</span>
      {/if}
      {#if d.media}
        <span class="pill quiet" title={d.media.reason}><Film size={11} />{d.media.manifest === "hls" ? "HLS" : "DASH"}</span>
      {/if}
      {#if !done && d.queue_id !== MAIN_QUEUE}
        <span class="pill quiet" title="In the queue “{store.queueName(d.queue_id)}”"><ListOrdered size={11} />{store.queueName(d.queue_id)}</span>
      {/if}
      {#if d.sync_every}
        <span
          class="pill quiet"
          title="Checked for a newer version {syncLabel(d.sync_every)}{d.sync_checked_at ? ` · last checked ${relativeTime(d.sync_checked_at)}` : ''}"
          ><RefreshCw size={11} />{syncLabel(d.sync_every)}</span
        >
      {/if}
      {#if !done}
        <span class="pill" style:--c={statusColor}>
          {#if d.status === "probing"}<LoaderCircle size={11} class="spin" />{/if}
          {d.needs_login ? "Needs login" : STATUS_LABEL[d.status]}
        </span>
      {/if}
    </div>
    {#if !done}<ProgressBar value={pct} status={d.status} height={5} />{/if}
    <div class="meta truncate">
      {#if d.status === "failed" && d.error}
        <span class="err">{d.error}</span>
      {:else}
        <span class="num">{sizeText}</span>
        {#if !done && d.total_size}<span class="sep">·</span><span class="num">{pct.toFixed(pct < 10 ? 1 : 0)}%</span>{/if}
        <span class="sep">·</span><span>{host(d.final_url ?? d.url)}</span>
        {#if done}<span class="sep">·</span><span>{relativeTime(d.completed_at)}</span>{/if}
      {/if}
    </div>
  </div>

  <div class="rate">
    {#if d.status === "downloading"}
      <div class="speed num">{speed(d.speed)}</div>
      {#if !d.media}<div class="eta num">{eta(d.total_size, d.downloaded, d.speed)} left</div>{/if}
    {/if}
  </div>

  <div class="quick">
    {#if d.status === "downloading" || d.status === "probing" || d.status === "queued"}
      <button class="icon-btn sm" title="Pause" onclick={(e) => (e.stopPropagation(), attempt(api.pause(d.id)))}><Pause size={15} /></button>
    {:else if d.status === "paused" || d.status === "waiting"}
      <button class="icon-btn sm" title="Resume" onclick={(e) => (e.stopPropagation(), attempt(api.resume(d.id)))}><Play size={15} /></button>
    {:else if d.needs_login}
      <button class="icon-btn sm" title="Log in" onclick={askLogin}><KeyRound size={15} /></button>
    {:else if d.status === "failed"}
      <button class="icon-btn sm" title="Retry" onclick={(e) => (e.stopPropagation(), attempt(api.resume(d.id)))}><RotateCw size={15} /></button>
    {/if}
    {#if d.dir && !d.file_missing}
      <button class="icon-btn sm" title="Show in folder" onclick={(e) => (e.stopPropagation(), attempt(api.showInFolder(d.id)))}><FolderOpen size={15} /></button>
    {/if}
  </div>
</div>

<style>
  .row {
    display: grid;
    grid-template-columns: 40px minmax(0, 1fr) auto 64px;
    align-items: center;
    gap: 14px;
    height: 100%;
    padding: 0 12px 0 14px;
    border-radius: var(--r-lg);
    border: 1px solid transparent;
    transition: background 0.12s, border-color 0.12s;
  }
  .row:hover {
    background: var(--hover);
  }
  .row.selected {
    background: var(--selected);
    border-color: var(--selected-border);
  }
  .main {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .done .main {
    gap: 2px;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  .name {
    flex: 1;
    min-width: 0;
    font-weight: 590;
    font-size: 13.5px;
  }
  .meta {
    font-size: 12px;
    color: var(--muted);
  }
  .num {
    font-variant-numeric: tabular-nums;
  }
  .sep {
    margin: 0 6px;
    opacity: 0.6;
  }
  .err {
    color: var(--err);
  }
  .rate {
    min-width: 92px;
    text-align: right;
    white-space: nowrap;
  }
  .speed {
    font-size: 13px;
    font-weight: 650;
  }
  .eta {
    font-size: 12px;
    color: var(--muted);
  }
  .quick {
    display: flex;
    justify-content: flex-end;
    gap: 2px;
    opacity: 0;
    transition: opacity 0.12s;
  }
  .row:hover .quick,
  .row.selected .quick {
    opacity: 1;
  }
  .pill :global(.spin) {
    animation: spin 0.9s linear infinite;
  }
</style>
