<script lang="ts">
  // One download's progress window, laid out as IDM's: three tabs, the bar, Hide details /
  // Pause / Cancel, then where each connection starts and what it is doing.
  import { onMount } from "svelte";
  import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
  import { ChevronsDown, ChevronsUp, Pause, Play } from "@lucide/svelte";
  import { t } from "./lib/i18n";
  import { api, type AfterDone, type Download, type OnDone, type PowerCaps, type Settings } from "./lib/api";
  import { bytes, duration, host, knownSize, nameFromUrl, percent, speed, SPEED_UNITS } from "./lib/format";
  import { errorOf, finishingText, isRecording, phaseText, progressOf, secondsLeft, statusText } from "./lib/rowview";
  import { canPause, canResume } from "./lib/categories";
  import { LiveDownload } from "./lib/live.svelte";
  import { attempt } from "./lib/store.svelte";
  import TitleBar from "./lib/components/TitleBar.svelte";
  import FileIcon from "./lib/components/FileIcon.svelte";
  import ProgressBar from "./lib/components/ProgressBar.svelte";
  import SegmentMap from "./lib/components/SegmentMap.svelte";
  import ConnectionTable from "./lib/components/ConnectionTable.svelte";
  import Toasts from "./lib/components/Toasts.svelte";

  let { id }: { id: number } = $props();

  // Kept in step with `progress.rs`.
  const WIDTH = 560;
  const TALL = 570;
  const SHORT = 324;

  const win = getCurrentWindow();
  // svelte-ignore state_referenced_locally
  const live = new LiveDownload(id);
  const d = $derived(live.d);

  let tab = $state<"status" | "limit" | "done">("status");
  let settings = $state<Settings | null>(null);
  let caps = $state<PowerCaps | null>(null);
  let details = $state(true);

  const name = $derived(d ? (d.filename ?? nameFromUrl(d.url)) : "");
  const pct = $derived(d ? progressOf(d) : 0);
  const recording = $derived(!!d && isRecording(d));
  const title = $derived(d ? (recording ? name : `${Math.floor(pct)}% ${name}`) : t("window-progress"));

  $effect(() => {
    win.setTitle(title).catch(() => {});
  });
  $effect(() => {
    if (live.gone) win.destroy();
  });

  /** The status line, in IDM's words while it runs. */
  function statusLine(d: Download): string {
    if (d.status === "failed") return errorOf(d) || statusText(d);
    const finishing = finishingText(d);
    if (finishing) return finishing;
    if (d.media_progress && d.status === "downloading") return phaseText(d) ?? statusText(d);
    if (d.status === "downloading") {
      if (d.conns?.includes("receiving") || (!d.segments.length && d.downloaded > 0)) return t("progress-conn-receiving");
      if (d.conns?.includes("retrying")) return t("progress-conn-retrying");
      return t("progress-conn-connecting");
    }
    return statusText(d);
  }

  function resumeText(d: Download): string {
    if (d.resumable === true) return t("progress-yes");
    if (d.resumable === false) return t("progress-no");
    return t("progress-unknown");
  }

  async function toggleDetails() {
    details = !details;
    await win.setSize(new LogicalSize(WIDTH, details ? TALL : SHORT)).catch(() => {});
    attempt(api.setProgressDetails(details));
  }

  async function cancel() {
    if (d && canPause(d.status)) await attempt(api.pause(id));
    win.destroy();
  }

  // ------------------------------------------------------------ speed limiter

  let unit = $state(0);
  let amount = $state(500);
  let remembered = $state(0);
  const limited = $derived(!!d && d.speed_limit > 0);
  const urlOf = (d: Download) => d.final_url ?? d.url;

  /** Shows the current limit in the field, in the unit that suits it. */
  function seed(bps: number) {
    if (!bps) return;
    unit = bps < 1024 ** 2 ? 0 : 1;
    amount = +(bps / SPEED_UNITS[unit].factor).toFixed(1);
  }

  const typed = () => Math.max(1, Math.round(amount * SPEED_UNITS[unit].factor));

  async function setLimit(bps: number) {
    if (!d) return;
    await attempt(api.setSpeedLimit(id, bps));
    if (remembered) await rememberLimit(true, bps);
  }

  async function rememberLimit(on: boolean, bps = d?.speed_limit ?? 0) {
    if (!d) return;
    const value = on ? bps || typed() : 0;
    await attempt(api.rememberServerSpeedLimit(urlOf(d), value));
    remembered = value;
  }

  // ------------------------------------------------------------ on completion

  const onDone = $derived<OnDone>(d?.on_done ?? { complete_dialog: null, open_file: false, after: "nothing" });
  const showsComplete = $derived(onDone.complete_dialog ?? settings?.complete_dialog ?? true);
  const AFTER: [AfterDone, string][] = [
    ["nothing", "tray-do-nothing"],
    ["quit", "tray-quit-siler"],
    ["sleep", "tray-sleep"],
    ["hibernate", "tray-hibernate"],
    ["power_off", "tray-shut-down"],
  ];
  const possible = (a: AfterDone) => a === "nothing" || a === "quit" || (caps ? caps[a] : false);

  function changeOnDone(change: Partial<OnDone>) {
    attempt(api.setOnDone(id, { ...onDone, ...change }));
  }

  onMount(() => {
    const stop = live.start();
    api.getSettings().then((s) => {
      settings = s;
      details = s.ui.progress_details;
    });
    api.powerCaps().then((c) => (caps = c));
    live.reload().then(async () => {
      if (!live.d) return;
      seed(live.d.speed_limit);
      remembered = await api.serverSpeedLimit(urlOf(live.d)).catch(() => 0);
    });
    return stop;
  });
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && api.progressToTray(id)} />

<div class="window">
  <TitleBar {title} onminimize={() => attempt(api.progressToTray(id))} onclose={() => win.destroy()} minimizeLabel={t("progress-to-tray")} closeLabel={t("progress-close")}>
    {#snippet icon()}
      {#if d}<FileIcon category={d.category} size={20} />{/if}
    {/snippet}
  </TitleBar>

  {#if d}
    <div class="content">
      <div class="tabs" role="tablist">
        <button role="tab" class:on={tab === "status"} aria-selected={tab === "status"} onclick={() => (tab = "status")}>{t("progress-tab-status")}</button>
        <button role="tab" class:on={tab === "limit"} aria-selected={tab === "limit"} onclick={() => (tab = "limit")}>{t("progress-tab-limit")}</button>
        <button role="tab" class:on={tab === "done"} aria-selected={tab === "done"} onclick={() => (tab = "done")}>{t("progress-tab-done")}</button>
      </div>

      <div class="pane" role="tabpanel">
        {#if tab === "status"}
          <div class="url selectable truncate" title={d.url}>{d.url}</div>
          <dl>
            <dt>{t("progress-status")}</dt>
            <dd class="status" class:err={d.status === "failed"} title={statusLine(d)}>{statusLine(d)}</dd>
            <dt>{t("progress-file-size")}</dt>
            <dd class="num">{knownSize(d) ? bytes(knownSize(d)) : t("progress-unknown")}</dd>
            <dt>{t("progress-downloaded")}</dt>
            <dd class="num">
              {bytes(d.downloaded)}{#if knownSize(d)}&nbsp;({percent(knownSize(d), d.downloaded).toFixed(2)} %){/if}
            </dd>
            <dt>{t("progress-transfer-rate")}</dt>
            <dd class="num">{d.status === "downloading" ? speed(d.speed) : "—"}</dd>
            <dt>{t("progress-time-left")}</dt>
            <dd class="num">{secondsLeft(d) != null ? duration(secondsLeft(d)!) : "—"}</dd>
            <dt>{t("progress-resume-capability")}</dt>
            <dd>{resumeText(d)}</dd>
          </dl>
        {:else if tab === "limit"}
          <label class="check">
            <input type="checkbox" checked={limited} onchange={(e) => setLimit(e.currentTarget.checked ? typed() : 0)} />
            {t("progress-use-limiter")}
          </label>
          <div class="limit-row">
            <span class="muted">{t("progress-max-speed")}</span>
            <input class="input num amount" type="number" min="0.1" step="any" bind:value={amount} disabled={!limited} onchange={() => limited && setLimit(typed())} />
            <select class="input unit" bind:value={unit} disabled={!limited} onchange={() => limited && setLimit(typed())}>
              {#each SPEED_UNITS as u, i (u.label)}<option value={i}>{u.label}</option>{/each}
            </select>
          </div>
          <label class="check">
            <input type="checkbox" checked={remembered > 0} onchange={(e) => rememberLimit(e.currentTarget.checked)} />
            {t("progress-remember-limit", { host: host(urlOf(d)).replace(/:\d+$/, "") })}
          </label>
          <p class="hint">{t("progress-limit-hint")}</p>
        {:else}
          <label class="check">
            <input type="checkbox" checked={showsComplete} onchange={(e) => changeOnDone({ complete_dialog: e.currentTarget.checked })} />
            {t("progress-show-complete")}
          </label>
          <label class="check">
            <input type="checkbox" checked={onDone.open_file} onchange={(e) => changeOnDone({ open_file: e.currentTarget.checked })} />
            {t("progress-open-file")}
          </label>
          <div class="limit-row">
            <span class="muted">{t("progress-when-done")}</span>
            <select class="input when" value={onDone.after} onchange={(e) => changeOnDone({ after: e.currentTarget.value as AfterDone })}>
              {#each AFTER as [a, key] (a)}<option value={a} disabled={!possible(a)}>{t(key)}</option>{/each}
            </select>
          </div>
          <p class="hint">{t("progress-when-done-hint")}</p>
        {/if}
      </div>

      <ProgressBar value={recording ? 100 : pct} status={d.status} height={14} />

      <div class="buttons">
        <button class="btn btn-sm" onclick={toggleDetails}>
          {#if details}<ChevronsUp size={14} />{t("progress-hide-details")}{:else}<ChevronsDown size={14} />{t("progress-show-details")}{/if}
        </button>
        <span class="grow"></span>
        {#if canPause(d.status)}
          <button class="btn btn-sm" onclick={() => attempt(api.pause(id))}><Pause size={14} />{t("progress-pause")}</button>
        {:else if canResume(d.status)}
          <button class="btn btn-sm btn-primary" onclick={() => attempt(api.resume(id))}><Play size={14} />{t("progress-resume")}</button>
        {/if}
        <button class="btn btn-sm" onclick={cancel}>{t("progress-cancel")}</button>
      </div>

      {#if details}
        <div class="details">
          <div class="caption">{t("progress-by-connections")}</div>
          <SegmentMap segments={d.segments} total={knownSize(d)} status={d.status} downloaded={d.downloaded} ticks />
          <ConnectionTable {d} />
        </div>
      {/if}
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
    gap: 10px;
    padding: 10px 14px 12px;
  }
  .tabs {
    display: flex;
    gap: 2px;
    margin-bottom: -10px;
  }
  .tabs button {
    padding: 6px 12px;
    border: 1px solid transparent;
    border-bottom: 0;
    border-radius: var(--r-sm) var(--r-sm) 0 0;
    background: transparent;
    color: var(--muted);
    font: inherit;
    font-size: 12.5px;
    cursor: pointer;
  }
  .tabs button.on {
    background: var(--surface-2);
    border-color: var(--border-strong);
    color: var(--text);
    font-weight: 600;
    position: relative;
    z-index: 1;
    margin-bottom: -1px;
    padding-bottom: 7px;
  }
  .pane {
    height: 178px;
    flex: none;
    padding: 10px 14px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: 0 var(--r-sm) var(--r-sm) var(--r-sm);
    font-size: 12.5px;
    overflow: hidden;
  }
  .url {
    margin-bottom: 6px;
    color: var(--text);
  }
  dl {
    display: grid;
    grid-template-columns: 130px 1fr;
    gap: 3px 10px;
    margin: 0;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .status {
    color: var(--accent-text);
  }
  .status.err {
    color: var(--err);
  }
  .num {
    font-variant-numeric: tabular-nums;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 2px 0 8px;
  }
  .limit-row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 8px 24px;
  }
  .amount {
    width: 90px;
  }
  .unit {
    width: 84px;
  }
  .when {
    width: 180px;
  }
  .hint {
    margin: 4px 0 0;
    color: var(--muted);
    font-size: 11.5px;
  }
  .muted {
    color: var(--muted);
  }
  .buttons {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .buttons .btn {
    min-width: 92px;
    justify-content: center;
  }
  .grow {
    flex: 1;
  }
  .details {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .caption {
    text-align: center;
    font-size: 11.5px;
    color: var(--muted);
  }
</style>
