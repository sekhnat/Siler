<script lang="ts">
  import { t } from "../i18n";
  import { CircleStop, FileArchive, Film, FolderOpen, KeyRound, ListOrdered, LoaderCircle, Pause, Play, Radio, RefreshCw, RotateCw, ShieldCheck, ShieldX, SquareTerminal, Waypoints } from "@lucide/svelte";
  import { ALGO_LABEL, api, MAIN_QUEUE, type Download } from "../api";
    import { bytes, clock, eta, host, knownSize, nameFromUrl, percent, relativeTime, speed, STREAM_KIND, syncLabel } from "../format";
  import { errorOf, isRecording, phaseText, progressOf, sizeText, statusColor, statusText } from "../rowview";
  import { attempt, store } from "../store.svelte";
  import FileIcon from "./FileIcon.svelte";
  import ProgressBar from "./ProgressBar.svelte";

  let { d, selected }: { d: Download; selected: boolean } = $props();

  const done = $derived(d.status === "completed");
  const recording = $derived(isRecording(d));
  const mp = $derived(d.media_progress ?? null);
  const size = $derived(knownSize(d));
  const pct = $derived(progressOf(d));
  const name = $derived(d.filename ?? nameFromUrl(d.url));
  const phase = $derived(phaseText(d));
  const verified = $derived(d.verify && d.verify.expected ? d.verify.expected.toLowerCase() === d.verify.actual.toLowerCase() : null);
  const usedMirrors = $derived(d.sources.filter((s, i) => i > 0 && !s.disabled).length);
  const commandFailed = $derived(!!d.command && !d.command.running && (d.command.error != null || d.command.exit_code !== 0));
  const sizeLine = $derived(sizeText(d));
  const statusColorV = $derived(statusColor(d));

  function askLogin(e: MouseEvent) {
    e.stopPropagation();
    window.dispatchEvent(new CustomEvent("siler-login", { detail: d.id }));
  }
</script>

<div class="row" class:selected class:done>
  <FileIcon category={d.category} size={40} />

  <div class="main">
    <div class="top">
      <span class="name truncate" title={name}>{name}</span>
      {#if d.file_missing}
        <span class="pill" style:--c="var(--warn)" title={t("download-row-the-file-was-moved-or-deleted")}>{t("download-row-file-missing")}</span>
      {/if}
      {#if verified === true}
        <span class="pill" style:--c="var(--ok)" title={t("download-row-its-matches-the-expected-checksum", { v: ALGO_LABEL[d.verify!.algo] })}><ShieldCheck size={11} />{t("download-row-verified")}</span>
      {:else if d.checksum_failed}
        <span class="pill" style:--c="var(--err)" title={(d.verify ? t("download-row-its-doesn-t-match", { algo: ALGO_LABEL[d.verify.algo] }) : t("download-row-its-checksum-doesn-t-match"))}><ShieldX size={11} />{t("download-row-damaged")}</span>
      {/if}
      {#if d.sources.length > 1 && !done}
        <span class="pill quiet" title={t("download-row-of-mirrors-in-use", { usedMirrors, v: d.sources.length - 1 })}><Waypoints size={11} />{t("download-row-sources", { v: usedMirrors + 1 })}</span>
      {/if}
      {#if d.zip_path}
        <span class="pill quiet" title={t("download-row-from-inside-the-archive", { zip_path: d.zip_path })}><FileArchive size={11} />{t("download-row-zip")}</span>
      {/if}
      {#if d.command?.running}
        <span class="pill" style:--c="var(--accent)" title={t("download-row-running-the-command-after-the-download")}><SquareTerminal size={11} />{t("download-row-running-command")}</span>
      {:else if commandFailed}
        <span class="pill" style:--c="var(--warn)" title={d.command?.error ?? t("download-row-command-exited", { code: d.command?.exit_code ?? "" })}><SquareTerminal size={11} />{t("download-row-command-failed")}</span>
      {/if}
      {#if d.media}
        <span class="pill quiet" title={d.media.reason}>{#if d.media.live && !done}<Radio size={11} />{t("download-row-live")}{:else}<Film size={11} />{STREAM_KIND[d.media.manifest] ?? t("download-row-stream")}{/if}</span>
      {/if}
      {#if !done && d.queue_id !== MAIN_QUEUE}
        <span class="pill quiet" title={t("download-row-in-the-queue", { v: store.queueName(d.queue_id) })}><ListOrdered size={11} />{store.queueName(d.queue_id)}</span>
      {/if}
      {#if d.sync_every}
        <span
          class="pill quiet"
          title={(d.sync_checked_at ? t("download-row-checked-for-a-newer-version-last", { every: syncLabel(d.sync_every), when: relativeTime(d.sync_checked_at) }) : t("download-row-checked-for-a-newer-version", { every: syncLabel(d.sync_every) }))}
          ><RefreshCw size={11} />{syncLabel(d.sync_every)}</span
        >
      {/if}
      {#if !done}
        <span class="pill" style:--c={statusColorV}>
          {#if d.status === "probing"}<LoaderCircle size={11} class="spin" />{/if}
          {statusText(d)}
        </span>
      {/if}
    </div>
    {#if !done}<ProgressBar value={pct} status={d.status} height={5} />{/if}
    <div class="meta truncate">
      {#if d.status === "failed" && d.error}
        <span class="err">{errorOf(d)}</span>
      {:else}
        <span class="num">{sizeLine}</span>
        {#if !done && d.total_size}<span class="sep">·</span><span class="num">{pct.toFixed(pct < 10 ? 1 : 0)}%</span>{/if}
        <span class="sep">·</span><span>{host(d.final_url ?? d.url)}</span>
        {#if done}<span class="sep">·</span><span>{relativeTime(d.completed_at)}</span>{/if}
      {/if}
    </div>
  </div>

  <div class="rate">
    {#if d.status === "downloading"}
      <div class="speed num">{speed(d.speed)}</div>
      {#if (!d.media || mp?.bytes_total) && mp?.phase !== "recording"}<div class="eta num">{t("download-row-left", { v: eta(size, d.downloaded, d.speed) })}</div>{/if}
    {/if}
  </div>

  <div class="quick">
    {#if recording}
      <button class="icon-btn sm finish" title={t("download-row-finish-the-recording-and-keep-it")} onclick={(e) => (e.stopPropagation(), attempt(api.finish(d.id)))}><CircleStop size={15} /></button>
    {/if}
    {#if d.status === "downloading" || d.status === "probing" || d.status === "queued"}
      <button class="icon-btn sm" title={t("download-row-pause")} onclick={(e) => (e.stopPropagation(), attempt(api.pause(d.id)))}><Pause size={15} /></button>
    {:else if d.status === "paused" || d.status === "waiting"}
      <button class="icon-btn sm" title={t("download-row-resume")} onclick={(e) => (e.stopPropagation(), attempt(api.resume(d.id)))}><Play size={15} /></button>
    {:else if d.needs_host_key}
      <button class="icon-btn sm" title={t("download-row-check-the-server-s-key")} onclick={askLogin}><KeyRound size={15} /></button>
    {:else if d.needs_login}
      <button class="icon-btn sm" title={t("download-row-log-in")} onclick={askLogin}><KeyRound size={15} /></button>
    {:else if d.status === "failed"}
      <button class="icon-btn sm" title={t("download-row-retry")} onclick={(e) => (e.stopPropagation(), attempt(api.resume(d.id)))}><RotateCw size={15} /></button>
    {/if}
    {#if d.dir && !d.file_missing}
      <button class="icon-btn sm" title={t("download-row-show-in-folder")} onclick={(e) => (e.stopPropagation(), attempt(api.showInFolder(d.id)))}><FolderOpen size={15} /></button>
    {/if}
  </div>
</div>

<style>
  .finish {
    color: var(--err);
  }
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
