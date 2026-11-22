<script lang="ts">
  // The panel a click on the tray icon opens: what runs, what waits, what just finished.
  // The window is see-through and as tall as this content.
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { CircleAlert, CircleStop, ExternalLink, FolderOpen, Gauge, Pause, Play, X } from "@lucide/svelte";
  import { t } from "./lib/i18n";
  import { api, type Download, type Settings } from "./lib/api";
  import { bytes, duration, limitLabel, nameFromUrl, relativeTime, speed } from "./lib/format";
  import { isRecording, progressOf, secondsLeft, sizeText, statusText } from "./lib/rowview";
  import { attempt, store } from "./lib/store.svelte";
  import Logo from "./lib/components/Logo.svelte";
  import ProgressBar from "./lib/components/ProgressBar.svelte";
  import Sparkline from "./lib/components/Sparkline.svelte";
  import SpeedMenu from "./lib/components/SpeedMenu.svelte";
  import Toasts from "./lib/components/Toasts.svelte";

  const WAITING_SHOWN = 8;
  const FINISHED_SHOWN = 3;

  let settings = $state<Settings | null>(null);
  let confirming = $state<number | null>(null);
  let limitOpen = $state(false);
  let panel: HTMLElement;
  let list: HTMLElement;

  const needsYou = (d: Download) => !!(d.needs_login || d.needs_host_key || d.needs_link) && d.status !== "completed";
  const byQueue = (a: Download, b: Download) => a.position - b.position || a.id - b.id;

  const all = $derived(Object.values(store.items));
  const running = $derived(all.filter((d) => d.status === "downloading" || d.status === "probing").sort(byQueue));
  const attention = $derived(all.filter(needsYou).sort(byQueue));
  const waiting = $derived(all.filter((d) => (d.status === "queued" || d.status === "waiting" || d.status === "paused") && !needsYou(d)).sort(byQueue));
  const finished = $derived(
    all
      .filter((d) => d.status === "completed")
      .sort((a, b) => (b.completed_at ?? 0) - (a.completed_at ?? 0))
      .slice(0, FINISHED_SHOWN),
  );
  const empty = $derived(!running.length && !attention.length && !waiting.length && !finished.length);
  const limit = $derived(settings?.global_speed_limit ?? 0);

  const nameOf = (d: Download) => d.filename ?? nameFromUrl(d.url);

  function runningLine(d: Download): string {
    const parts = [sizeText(d)];
    const rate = speed(d.speed);
    if (d.status === "downloading") {
      if (isRecording(d)) parts.push(rate);
      else parts.unshift(rate);
    }
    const left = secondsLeft(d);
    if (left != null) parts.push(t("tray-left", { time: duration(left) }));
    return parts.join(" · ");
  }

  async function setLimit(v: number) {
    const s = await attempt(api.setGlobalSpeedLimit(v));
    if (s) settings = s;
  }

  /** Opens the page for a click on its link; without a page, the dialog takes a pasted one. */
  async function freshLink(d: Download) {
    if (d.page_url || d.referrer) {
      await attempt(api.awaitRefresh(d.id));
      api.closeTrayPanel();
    } else {
      await attempt(api.showDownload(d.id));
    }
  }

  function openSiler() {
    api.showMain();
  }

  /**
   * WebKit's shared-memory renderer (no DMA-BUF: common on NVIDIA, or without a GPU) sends
   * just the rectangle that changed, and the rest of a see-through window turns blank. So
   * whatever changes the panel also moves the page's invisible background in the same
   * frame, which makes that frame a whole one.
   */
  function repaintWhole(): () => void {
    const root = document.documentElement;
    let moved = false;
    const move = () => {
      moved = !moved;
      root.style.backgroundPosition = moved ? "1px 0" : "0 0";
    };
    const dom = new MutationObserver(move);
    dom.observe(document.body, { subtree: true, childList: true, characterData: true, attributes: true });
    const themed = new MutationObserver(move);
    themed.observe(root, { attributes: true, attributeFilter: ["data-paint"] });
    // What CSS repaints without a change to the page: hover, pressed, focus, scrolling.
    const kinds = ["pointerover", "pointerout", "pointerdown", "pointerup", "focusin", "focusout", "input", "scroll"];
    for (const k of kinds) document.addEventListener(k, move, true);
    return () => {
      dom.disconnect();
      themed.disconnect();
      for (const k of kinds) document.removeEventListener(k, move, true);
    };
  }

  onMount(() => {
    document.documentElement.classList.add("tray-panel");
    const stopRepainting = repaintWhole();
    store.init();
    api.getSettings().then((s) => (settings = s));
    const unlisten = [
      listen<Settings>("settings-changed", (e) => (settings = e.payload)),
      listen("tray-opened", () => {
        confirming = null;
        limitOpen = false;
        list?.scrollTo(0, 0);
        api.getSettings().then((s) => (settings = s));
      }),
    ];
    const size = new ResizeObserver(() => void api.resizeTrayPanel(Math.ceil(panel.getBoundingClientRect().height)));
    size.observe(panel);
    return () => {
      stopRepainting();
      size.disconnect();
      for (const u of unlisten) u.then((f) => f());
    };
  });
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && api.closeTrayPanel()} />

{#snippet cancel(d: Download)}
  <button class="icon-btn sm" title={t("tray-panel-cancel")} aria-label={t("tray-panel-cancel")} onclick={() => (confirming = d.id)}><X size={15} /></button>
{/snippet}

{#snippet confirm(d: Download)}
  <div class="confirm">
    <div class="name truncate" title={nameOf(d)}>{nameOf(d)}</div>
    <div class="ask">
      <span class="line">{t("tray-panel-cancel-question")}</span>
      <button class="btn btn-sm" onclick={() => (confirming = null)}>{t("tray-panel-keep")}</button>
      <button class="btn btn-sm btn-danger" onclick={() => (attempt(api.remove(d.id, false)), (confirming = null))}>{t("tray-panel-discard")}</button>
    </div>
  </div>
{/snippet}

<main class="panel" class:menu-open={limitOpen} bind:this={panel} aria-label={t("window-tray-panel")}>
  <header>
    <Logo size={24} />
    <b class="title">Siler</b>
    <div class="spark"><Sparkline values={store.history} height={28} /></div>
    <span class="total num">↓ {speed(running.length ? store.totalSpeed : 0)}</span>
  </header>

  <div class="list" bind:this={list}>
    {#each running as d (d.id)}
      <div class="item running">
        {#if confirming === d.id}
          {@render confirm(d)}
        {:else}
          {@const recording = isRecording(d)}
          <div class="top">
            {#if recording}<span class="rec"></span>{/if}
            <span class="name truncate" title={nameOf(d)}>{nameOf(d)}</span>
            {#if recording}
              <button class="btn btn-sm finish" onclick={() => attempt(api.finish(d.id))}><CircleStop size={13} />{t("tray-finish")}</button>
            {/if}
            <button class="icon-btn sm" title={t("tray-pause")} aria-label={t("tray-pause")} onclick={() => attempt(api.pause(d.id))}><Pause size={15} /></button>
            {@render cancel(d)}
          </div>
          {#if !recording}
            <div class="bar">
              <ProgressBar value={progressOf(d)} status={d.status} height={6} />
              <span class="pct num">{d.total_size || d.media_progress ? `${Math.floor(progressOf(d))}%` : ""}</span>
            </div>
          {/if}
          <div class="line num truncate">{runningLine(d)}</div>
        {/if}
      </div>
    {/each}

    {#if attention.length}
      <h3>{t("tray-panel-needs-you")}</h3>
      {#each attention as d (d.id)}
        <div class="item">
          {#if confirming === d.id}
            {@render confirm(d)}
          {:else}
            <div class="top">
              <span class="warn"><CircleAlert size={15} /></span>
              <span class="two">
                <span class="name truncate" title={nameOf(d)}>{nameOf(d)}</span>
                <span class="line truncate">{statusText(d)}</span>
              </span>
              {#if d.needs_link}
                <button class="btn btn-sm" onclick={() => freshLink(d)}>{t("tray-panel-fresh-link")}</button>
              {:else}
                <button class="btn btn-sm" onclick={() => attempt(api.showDownload(d.id))}>{t("tray-panel-fix")}</button>
              {/if}
              {@render cancel(d)}
            </div>
          {/if}
        </div>
      {/each}
    {/if}

    {#if waiting.length}
      <h3>{t("tray-panel-waiting")}</h3>
      {#each waiting.slice(0, WAITING_SHOWN) as d (d.id)}
        <div class="item">
          {#if confirming === d.id}
            {@render confirm(d)}
          {:else}
            <div class="top">
              <span class="two">
                <span class="name truncate" title={nameOf(d)}>{nameOf(d)}</span>
                <span class="line num truncate">{statusText(d)} · {sizeText(d)}</span>
              </span>
              {#if d.status === "queued"}
                <button class="icon-btn sm" title={t("tray-pause")} aria-label={t("tray-pause")} onclick={() => attempt(api.pause(d.id))}><Pause size={15} /></button>
              {:else}
                <button class="icon-btn sm" title={t("tray-panel-resume")} aria-label={t("tray-panel-resume")} onclick={() => attempt(api.resume(d.id))}><Play size={15} /></button>
              {/if}
              {@render cancel(d)}
            </div>
          {/if}
        </div>
      {/each}
      {#if waiting.length > WAITING_SHOWN}
        <div class="more line">{t("tray-and-more", { count: waiting.length - WAITING_SHOWN })}</div>
      {/if}
    {/if}

    {#if finished.length}
      <h3>{t("tray-panel-finished")}</h3>
      {#each finished as d (d.id)}
        <div class="item">
          <div class="top">
            <span class="two">
              <span class="name truncate" title={nameOf(d)}>{nameOf(d)}</span>
              <span class="line num truncate">{d.file_missing ? t("tray-panel-file-missing") : `${bytes(d.total_size)} · ${relativeTime(d.completed_at)}`}</span>
            </span>
            {#if !d.file_missing}
              <button class="icon-btn sm" title={t("tray-panel-open")} aria-label={t("tray-panel-open")} onclick={() => attempt(api.openFile(d.id).then(api.closeTrayPanel))}><ExternalLink size={14} /></button>
              <button class="icon-btn sm" title={t("tray-open-folder")} aria-label={t("tray-open-folder")} onclick={() => attempt(api.showInFolder(d.id).then(api.closeTrayPanel))}><FolderOpen size={15} /></button>
            {/if}
          </div>
        </div>
      {/each}
    {/if}

    {#if store.loaded && empty}
      <div class="empty">
        <b>{t("tray-no-active-downloads")}</b>
        <span class="line">{t("tray-panel-empty")}</span>
      </div>
    {/if}
  </div>

  <footer>
    {#if running.length}
      <button class="btn btn-ghost btn-sm" onclick={() => attempt(api.pauseAll())}><Pause size={13} />{t("tray-pause-all")}</button>
    {:else}
      <button class="btn btn-ghost btn-sm" disabled={!waiting.length} onclick={() => attempt(api.resumeAll())}><Play size={13} />{t("tray-resume-all")}</button>
    {/if}
    <div class="limit">
      <button class="btn btn-ghost btn-sm" class:set={limit > 0} aria-expanded={limitOpen} onclick={() => (limitOpen = !limitOpen)}>
        <Gauge size={13} />{limit ? limitLabel(limit) : t("tray-panel-limit")}
      </button>
      {#if limitOpen}
        <SpeedMenu value={limit} direction="up" onselect={setLimit} onclose={() => (limitOpen = false)} />
      {/if}
    </div>
    <button class="btn btn-sm open" onclick={openSiler}>{t("tray-panel-open-siler")}</button>
  </footer>
</main>

<Toasts />

<style>
  :global(html.tray-panel body) {
    background: transparent;
  }
  /* See `repaintWhole`: the invisible background it moves, and nothing that repaints by
     itself between two of its moves. */
  :global(html.tray-panel) {
    background: linear-gradient(90deg, rgba(0, 0, 0, 0.004), rgba(0, 0, 0, 0.008)) 0 0 / 100% 100% repeat;
  }
  :global(html.tray-panel *),
  :global(html.tray-panel *::before),
  :global(html.tray-panel *::after) {
    animation: none !important;
    transition: none !important;
  }
  /* A text field's blinking caret does repaint by itself. */
  :global(html.tray-panel input) {
    caret-color: var(--text);
  }
  :global(html.tray-panel:has(input:focus)) {
    animation: tray-repaint 1s linear infinite;
  }
  @keyframes -global-tray-repaint {
    to {
      background-position: 100px 0;
    }
  }
  .panel :global(.fill.downloading),
  .panel :global(.fill.probing) {
    background: var(--grad);
  }
  .panel {
    display: flex;
    flex-direction: column;
    max-height: 600px;
    overflow: hidden;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-xl);
    background: var(--surface);
  }
  /* The limit menu opens upwards, inside the panel's own window. */
  .menu-open {
    min-height: 440px;
  }
  header {
    flex: none;
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 11px 14px 9px 12px;
    background: var(--surface-2);
    border-bottom: 1px solid var(--border);
  }
  .title {
    font-size: 14px;
  }
  .spark {
    flex: 1;
    min-width: 0;
  }
  .total {
    min-width: 94px;
    text-align: end;
    font-size: 14px;
    font-weight: 650;
  }
  .list {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    padding: 5px 6px 7px;
  }
  .item {
    padding: 5px 4px 5px 8px;
    border-radius: var(--r);
  }
  .item:hover {
    background: var(--hover);
  }
  .running {
    padding-block: 7px 8px;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .name {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    font-weight: 590;
  }
  .two {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .two .name {
    flex: none;
  }
  .line {
    font-size: 11.5px;
    color: var(--text-2);
  }
  .num {
    font-variant-numeric: tabular-nums;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding-inline-end: 6px;
    margin-block: 2px 4px;
  }
  .pct {
    flex: none;
    width: 34px;
    text-align: end;
    font-size: 11.5px;
    font-weight: 600;
  }
  .rec {
    flex: none;
    width: 8px;
    height: 8px;
    margin-inline-end: 3px;
    border-radius: 50%;
    background: var(--err);
  }
  .finish {
    color: var(--err);
  }
  .warn {
    display: grid;
    margin-inline-end: 3px;
    color: var(--warn);
  }
  h3 {
    margin: 9px 8px 2px;
    font-size: 11px;
    font-weight: 650;
    color: var(--muted);
  }
  .more {
    padding: 3px 8px;
  }
  .confirm {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .confirm .name {
    flex: none;
  }
  .ask {
    display: flex;
    align-items: center;
    gap: 5px;
  }
  .ask .line {
    flex: 1;
    min-width: 0;
  }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    padding: 24px 18px;
    text-align: center;
  }
  footer {
    flex: none;
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 7px 8px;
    border-top: 1px solid var(--border);
  }
  .limit {
    position: relative;
  }
  .set {
    color: var(--accent-text);
  }
  .open {
    margin-inline-start: auto;
  }
</style>
