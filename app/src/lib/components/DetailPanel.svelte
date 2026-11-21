<script lang="ts">
  import { Copy, ExternalLink, FolderOpen, Gauge, Info, Minus, Pause, Play, Plus, RotateCw, ShieldCheck, ShieldX, SlidersHorizontal, SquareTerminal, Trash2, TriangleAlert, X } from "@lucide/svelte";
  import { fly } from "svelte/transition";
  import { ALGO_LABEL, api, type Download } from "../api";
  import { STATUS_LABEL, categoryMeta } from "../categories";
  import { bytes, dateTime, eta, knownSize, limitLabel, nameFromUrl, percent, relativeTime, speed, syncLabel } from "../format";
  import { attempt, store, toasts } from "../store.svelte";
  import FileIcon from "./FileIcon.svelte";
  import SpeedMenu from "./SpeedMenu.svelte";
  import SegmentMap from "./SegmentMap.svelte";
  import Sparkline from "./Sparkline.svelte";

  let {
    d,
    onclose,
    onremove,
    onproperties,
    onredownload,
  }: {
    d: Download;
    onclose: () => void;
    onremove: () => void;
    onproperties: () => void;
    onredownload: () => void;
  } = $props();

  const setConnections = (n: number) => attempt(api.setConnections(d.id, Math.max(1, Math.min(32, n))));

  let limitMenu = $state(false);
  const size = $derived(knownSize(d));
  const pct = $derived(d.status === "completed" ? 100 : percent(size, d.downloaded));
  const running = $derived(d.status === "downloading");
  const history = $derived(store.perHistory[d.id] ?? []);
  const path = $derived(d.dir && d.filename ? `${d.dir}/${d.filename}` : d.dir);

  /** What a media download is doing now, in words. */
  function mediaPhase(m: NonNullable<Download["media_progress"]>): string {
    switch (m.phase) {
      case "waiting":
        return "Waiting for the site to allow the download";
      case "resolving":
        return "Reading the stream";
      case "downloading":
        return m.bytes_total ? "Downloading" : `Pieces ${m.segments_done} of ${m.segments_total}`;
      case "rebuilding":
        return `Rebuilding ${(m.permille / 10).toFixed(0)}%`;
      default:
        return "Saving the file";
    }
  }

  const verified = $derived(d.verify?.expected ? d.verify.expected.toLowerCase() === d.verify.actual.toLowerCase() : null);
  const ORIGIN: Record<string, string> = { user: "added by you", metalink: "from a list", server: "named by the server" };

  async function keepAnyway() {
    await attempt(api.acceptUnverified(d.id));
  }

  async function copyLink() {
    try {
      await navigator.clipboard.writeText(d.url);
      toasts.show("Link copied", "success");
    } catch (e) {
      toasts.error(e);
    }
  }
</script>

<section transition:fly={{ x: 24, duration: 180 }}>
  <header>
    <FileIcon category={d.category} size={46} />
    <div class="title">
      <div class="fname selectable">{d.filename ?? nameFromUrl(d.url)}</div>
      <div class="sub">{categoryMeta(d.category).label} · {STATUS_LABEL[d.status]}</div>
    </div>
    <button class="icon-btn sm" onclick={onclose} aria-label="Close details"><X size={16} /></button>
  </header>

  <div class="big">
    <div class="big-pct">{pct.toFixed(pct < 10 && pct > 0 ? 1 : 0)}<span>%</span></div>
    <div class="big-side">
      <div>{bytes(d.downloaded)} <span class="of">of {bytes(size)}</span></div>
      {#if running}<div class="of">{speed(d.speed)} · {eta(size, d.downloaded, d.speed)} left</div>{/if}
    </div>
  </div>

  {#if d.file_missing}
    <div class="note err"><TriangleAlert size={15} /><span>The file was moved or deleted. Download it again, or remove it from the list.</span></div>
  {:else if d.checksum_failed}
    <div class="note err col">
      <div class="row-note"><ShieldX size={15} /><span>{d.error}</span></div>
      {#if d.verify}
        <div class="mono small selectable">Expected {d.verify.expected}<br />Got&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;{d.verify.actual}</div>
      {/if}
      <div class="note-actions">
        <button class="btn btn-sm" onclick={() => attempt(api.restart(d.id))}>Download again</button>
        <button class="btn btn-sm btn-ghost" onclick={keepAnyway}>Keep it anyway</button>
      </div>
    </div>
  {:else if d.status === "failed" && d.error}
    <div class="note err"><TriangleAlert size={15} /><span class="selectable">{d.error}</span></div>
  {:else if d.notice}
    <div class="note"><Info size={15} /><span>{d.notice}</span></div>
  {/if}

  <div class="block">
    <div class="block-head">
      <span>Connections</span>
      <span class="muted conn">
        {#if running}{d.active_connections} active of{:else if d.resumable === false}Single connection{:else if d.status === "completed"}{d.connections}{/if}
        {#if d.status !== "completed" && d.resumable !== false}
          <button class="icon-btn xs" onclick={() => setConnections(d.connections - 1)} disabled={d.connections <= 1} aria-label="Fewer connections"><Minus size={12} /></button>
          <b>{d.connections}</b>
          <button class="icon-btn xs" onclick={() => setConnections(d.connections + 1)} disabled={d.connections >= 32} aria-label="More connections"><Plus size={12} /></button>
        {/if}
      </span>
    </div>
    <SegmentMap segments={d.segments} total={d.total_size} status={d.status} downloaded={d.downloaded} />
  </div>

  {#if d.sources.length > 1}
    <div class="block">
      <div class="block-head"><span>Sources</span><span class="muted">{d.sources.filter((s) => !s.disabled).length} of {d.sources.length} in use</span></div>
      <div class="sources">
        {#each d.sources as s, i (s.url)}
          <div class="source" class:off={!!s.disabled} title={s.disabled ?? s.url}>
            <span class="dot-src" class:live={s.active > 0}></span>
            <span class="truncate src-host">{(() => { try { return new URL(s.url).host; } catch { return s.url; } })()}</span>
            <span class="muted src-meta">{i === 0 ? "main" : ORIGIN[s.origin]}{s.disabled ? " · not used" : ""}</span>
            <span class="num">{bytes(s.bytes)}</span>
          </div>
        {/each}
      </div>
    </div>
  {/if}

  {#if running || history.some((v) => v > 0)}
    <div class="block">
      <div class="block-head"><span>Speed</span><span class="muted">{speed(d.speed)}</span></div>
      <div class="spark"><Sparkline values={history} height={54} /></div>
    </div>
  {/if}

  <dl class="facts">
    <dt>Address</dt>
    <dd class="selectable mono truncate" title={d.url}>{d.url}</dd>
    {#if d.referrer}
      <dt>Referrer</dt>
      <dd class="selectable mono truncate" title={d.referrer}>{d.referrer}</dd>
    {/if}
    <dt>Saved to</dt>
    <dd class="selectable truncate" title={path ?? ""}>{d.dir ?? "Decided when the download starts"}</dd>
    <dt>Resume</dt>
    <dd>
      {#if d.resumable === true}<span class="pill" style:--c="var(--ok)">Supported</span>
      {:else if d.resumable === false}<span class="pill" style:--c="var(--warn)">Not supported</span>
      {:else}<span class="muted">Unknown</span>{/if}
    </dd>
    <dt>Speed limit</dt>
    <dd class="limit-wrap">
      <button class="limit" onclick={() => (limitMenu = !limitMenu)}><Gauge size={13} /> {limitLabel(d.speed_limit)}</button>
      {#if limitMenu}
        <SpeedMenu value={d.speed_limit} onselect={(v) => attempt(api.setSpeedLimit(d.id, v))} onclose={() => (limitMenu = false)} />
      {/if}
    </dd>
    {#if store.queues.length > 1}
      <dt>Queue</dt>
      <dd>{store.queueName(d.queue_id)}</dd>
    {/if}
    {#if d.sync_every}
      <dt>Kept up to date</dt>
      <dd>Checked {syncLabel(d.sync_every)}{d.sync_checked_at ? `, last ${relativeTime(d.sync_checked_at)}` : ""}</dd>
    {/if}
    {#if d.media}
      <dt>Stream</dt>
      <dd>{d.media.manifest === "hls" ? "HLS" : "DASH"} · {d.media.output === "original" ? "original format" : d.media.output.toUpperCase()} · {d.media.reason}</dd>
      <dt>Tracks</dt>
      <dd class="selectable">{d.media.labels.join(" · ")}</dd>
      {#if d.media.subtitles !== "none" && d.media.labels.length}
        <dt>Subtitles</dt>
        <dd>{d.media.subtitles === "embed" ? "Inside the video" : d.media.subtitles === "sidecar" ? "As a separate file" : "Left out"}</dd>
      {/if}
      {#if d.media.sidecars.length}
        <dt>Also saved</dt>
        <dd class="selectable">{d.media.sidecars.join(", ")}</dd>
      {/if}
      {#if d.media_progress && d.status !== "completed"}
        <dt>Progress</dt>
        <dd>{mediaPhase(d.media_progress)}</dd>
      {/if}
    {/if}
    {#if d.zip_path}
      <dt>From archive</dt>
      <dd class="selectable mono truncate" title={d.zip_path}>{d.zip_path}</dd>
    {/if}
    {#if d.phase && d.status !== "completed"}
      <dt>Progress</dt>
      <dd>{d.phase.name === "verifying" ? "Checking the checksum" : "Unpacking"} {d.phase.total ? `${Math.floor((100 * d.phase.done) / d.phase.total)}%` : ""}</dd>
    {/if}
    {#if d.checksum || d.verify}
      <dt>Checksum</dt>
      <dd>
        {#if verified === true}
          <span class="pill" style:--c="var(--ok)"><ShieldCheck size={11} /> {ALGO_LABEL[d.verify!.algo]} matches</span>
        {:else if verified === false}
          <span class="pill" style:--c="var(--err)"><ShieldX size={11} /> {ALGO_LABEL[d.verify!.algo]} doesn't match</span>
        {:else if d.checksum}
          <span class="muted">{ALGO_LABEL[d.checksum.algo]}{d.checksum.source === "server" ? " from the server" : d.checksum.source === "metalink" ? " from the list" : ""}, checked when done</span>
        {:else if d.verify}
          <span class="mono small selectable">{ALGO_LABEL[d.verify.algo]} {d.verify.actual}</span>
        {/if}
      </dd>
    {/if}
    {#if d.command}
      <dt>Command</dt>
      <dd>
        {#if d.command.running}<span class="muted"><SquareTerminal size={12} /> Running…</span>
        {:else if d.command.error}<span class="err-text">{d.command.error}</span>
        {:else}<span class:err-text={d.command.exit_code !== 0}>Exited with {d.command.exit_code}</span>{/if}
        {#if d.command.output}<pre class="output selectable">{d.command.output}</pre>{/if}
      </dd>
    {/if}
    <dt>Added</dt>
    <dd>{dateTime(d.created_at)}</dd>
    {#if d.completed_at}
      <dt>Finished</dt>
      <dd>{dateTime(d.completed_at)}</dd>
    {/if}
  </dl>

  <div class="actions">
    {#if d.file_missing}
      <button class="btn btn-primary" onclick={onredownload}><RotateCw size={15} /> Download again</button>
    {:else if d.status === "completed"}
      <button class="btn btn-primary" onclick={() => attempt(api.openFile(d.id))}><ExternalLink size={15} /> Open</button>
    {:else if d.status === "waiting"}
      <button class="btn btn-primary" onclick={() => attempt(api.resume(d.id))}><Play size={15} /> Start</button>
    {:else if d.status === "paused"}
      <button class="btn btn-primary" onclick={() => attempt(api.resume(d.id))}><Play size={15} /> Resume</button>
    {:else if d.status === "failed"}
      <button class="btn btn-primary" onclick={() => attempt(api.resume(d.id))}><RotateCw size={15} /> Retry</button>
    {:else}
      <button class="btn" onclick={() => attempt(api.pause(d.id))}><Pause size={15} /> Pause</button>
    {/if}
    <button class="icon-btn" title="Show in folder" disabled={!d.dir} onclick={() => attempt(api.showInFolder(d.id))}><FolderOpen size={17} /></button>
    <button class="icon-btn" title="Copy link" onclick={copyLink}><Copy size={17} /></button>
    <button class="icon-btn" title="Properties" onclick={onproperties}><SlidersHorizontal size={17} /></button>
    <button class="icon-btn danger" title="Remove" onclick={onremove}><Trash2 size={17} /></button>
  </div>
</section>

<style>
  .note.col {
    flex-direction: column;
    align-items: stretch;
    gap: 8px;
  }
  .row-note {
    display: flex;
    gap: 8px;
    align-items: flex-start;
  }
  .note-actions {
    display: flex;
    gap: 6px;
  }
  .small {
    font-size: 11px;
    word-break: break-all;
  }
  .sources {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .source {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 12px;
    min-width: 0;
  }
  .source.off {
    opacity: 0.55;
  }
  .src-host {
    font-weight: 600;
    min-width: 0;
  }
  .src-meta {
    flex: 1;
    white-space: nowrap;
    font-size: 11px;
  }
  .dot-src {
    width: 7px;
    height: 7px;
    border-radius: 99px;
    background: var(--border-strong);
    flex: none;
  }
  .dot-src.live {
    background: var(--ok);
  }
  .err-text {
    color: var(--err);
  }
  .output {
    margin: 6px 0 0;
    max-height: 120px;
    overflow: auto;
    padding: 8px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    font-size: 11px;
    white-space: pre-wrap;
    word-break: break-all;
  }
  section {
    width: 340px;
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding: 18px 18px 16px;
    margin: 0 14px 14px 0;
    border-radius: var(--r-xl);
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: var(--shadow);
    overflow-y: auto;
  }
  header {
    display: flex;
    align-items: flex-start;
    gap: 12px;
  }
  .title {
    flex: 1;
    min-width: 0;
  }
  .fname {
    font-weight: 650;
    font-size: 14.5px;
    line-height: 1.3;
    word-break: break-word;
  }
  .sub {
    margin-top: 2px;
    font-size: 12px;
    color: var(--muted);
  }
  .big {
    display: flex;
    align-items: flex-end;
    gap: 14px;
  }
  .big-pct {
    font-size: 40px;
    font-weight: 750;
    letter-spacing: -0.04em;
    line-height: 1;
    font-variant-numeric: tabular-nums;
    background: var(--grad);
    -webkit-background-clip: text;
    background-clip: text;
    color: transparent;
  }
  .big-pct span {
    font-size: 22px;
  }
  .big-side {
    font-size: 13px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    padding-bottom: 2px;
  }
  .of {
    color: var(--muted);
    font-weight: 500;
  }
  .note {
    display: flex;
    gap: 9px;
    padding: 10px 12px;
    border-radius: var(--r);
    font-size: 12.5px;
    background: color-mix(in srgb, var(--accent) 9%, transparent);
    color: var(--text-2);
  }
  .note :global(svg) {
    flex: none;
    margin-top: 1px;
    color: var(--accent);
  }
  .note.err {
    background: color-mix(in srgb, var(--err) 10%, transparent);
  }
  .note.err :global(svg) {
    color: var(--err);
  }
  .block-head {
    display: flex;
    justify-content: space-between;
    margin-bottom: 8px;
    font-size: 12px;
    font-weight: 650;
  }
  .muted {
    color: var(--muted);
    font-weight: 500;
  }
  .conn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .conn b {
    min-width: 16px;
    text-align: center;
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }
  .icon-btn.xs {
    width: 20px;
    height: 20px;
    border-radius: 6px;
  }
  .spark {
    border-radius: var(--r);
    background: var(--surface-2);
    padding-top: 6px;
    overflow: hidden;
  }
  .facts {
    display: grid;
    grid-template-columns: 86px minmax(0, 1fr);
    gap: 9px 10px;
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
  .limit-wrap {
    position: relative;
  }
  .limit {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-weight: 600;
    color: var(--accent-text);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 4px;
    margin-top: auto;
    padding-top: 4px;
  }
  .actions .btn {
    flex: 1;
    margin-right: 6px;
  }
  .danger:hover {
    color: var(--err);
  }
</style>
