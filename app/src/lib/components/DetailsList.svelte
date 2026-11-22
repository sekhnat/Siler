<script lang="ts">
  import { t } from "../i18n";
  import { ArrowDown, ArrowUp, Check, Radio } from "@lucide/svelte";
  import type { ColumnSetting, Download } from "../api";
  import { categoryMeta } from "../categories";
  import { columnDef, COLUMNS, nameOf, type SortKey } from "../columns";
  import { draggable } from "../drag";
  import { bytes, dateTime, duration, host, knownSize, speed } from "../format";
  import { errorOf, isRecording, progressOf, secondsLeft, statusColor, statusText } from "../rowview";
  import { store } from "../store.svelte";
  import FileIcon from "./FileIcon.svelte";
  import ProgressBar from "./ProgressBar.svelte";

  let {
    items,
    selected,
    columns,
    sort,
    reverse,
    onsort,
    oncolumns,
    onrowclick,
    onrowopen,
    onrowmenu,
    ondragout,
  }: {
    items: Download[];
    selected: Set<number>;
    columns: ColumnSetting[];
    sort: SortKey;
    reverse: boolean;
    /** A header was clicked: sort by it (or reverse, when it already sorts). */
    onsort: (key: SortKey) => void;
    /** Columns were resized, moved, shown or hidden. */
    oncolumns: (cols: ColumnSetting[]) => void;
    onrowclick: (d: Download, e: MouseEvent) => void;
    onrowopen: (d: Download) => void;
    onrowmenu: (d: Download, e: MouseEvent) => void;
    ondragout?: (d: Download) => void;
  } = $props();

  const ROW = 34;
  const HEAD = 34;
  const OVERSCAN = 10;
  let scrollTop = $state(0);
  let viewport = $state(600);
  let scroller: HTMLDivElement | undefined = $state();

  /** Rows that fit in the view, for Page Up/Down. */
  export function pageSize(): number {
    return Math.max(1, Math.floor((viewport - HEAD) / ROW) - 1);
  }

  /** Scrolls just enough to show row `i`. */
  export function scrollToIndex(i: number) {
    if (!scroller) return;
    const top = i * ROW;
    const view = viewport - HEAD;
    if (top < scroller.scrollTop) scroller.scrollTop = top;
    else if (top + ROW > scroller.scrollTop + view) scroller.scrollTop = top + ROW - view + 4;
  }

  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN));
  const last = $derived(Math.min(items.length, Math.ceil((scrollTop + viewport) / ROW) + OVERSCAN));
  const visible = $derived(items.slice(first, last));

  // Widths while a column is being resized (saved when the pointer is let go).
  let live = $state<ColumnSetting[] | null>(null);
  const cols = $derived(live ?? columns);
  // The name (always first) takes the room the other columns leave; its width is its least.
  const template = $derived(cols.map((c, i) => (i === 0 ? `minmax(${c.width}px, 1fr)` : `${c.width}px`)).join(" "));
  const totalWidth = $derived(cols.reduce((n, c) => n + c.width, 0));

  function startResize(i: number, e: PointerEvent) {
    e.preventDefault();
    e.stopPropagation();
    const x0 = e.clientX;
    // The name column may be wider than its setting: resizing starts from what is shown.
    const cell = (e.currentTarget as HTMLElement).closest(".th");
    const w0 = i === 0 && cell ? Math.round(cell.getBoundingClientRect().width) : cols[i].width;
    const base = cols.map((c) => ({ ...c }));
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => {
      const w = Math.max(48, Math.min(1200, Math.round(w0 + ev.clientX - x0)));
      live = base.map((c, k) => (k === i ? { ...c, width: w } : c));
    };
    const up = () => {
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
      if (live) oncolumns(live);
      live = null;
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
  }

  // Moving a column by dragging its header (the name stays first).
  let dragId = $state<string | null>(null);
  let overId = $state<string | null>(null);
  function drop(onto: string) {
    if (!dragId || dragId === onto || onto === "name") return;
    const list = cols.filter((c) => c.id !== dragId);
    const moved = cols.find((c) => c.id === dragId);
    const at = list.findIndex((c) => c.id === onto);
    if (!moved || at < 0) return;
    list.splice(at, 0, moved);
    oncolumns(list);
  }

  // The header's menu: which columns are shown.
  let chooser = $state<{ x: number; y: number } | null>(null);
  function toggleColumn(id: string) {
    if (id === "name") return;
    const shown = cols.some((c) => c.id === id);
    if (shown) oncolumns(cols.filter((c) => c.id !== id));
    else oncolumns([...cols, { id, width: columnDef(id)?.width ?? 120 }]);
  }

  // A press on a finished row becomes a file drag once the pointer moves a little.
  let press: { id: number; x: number; y: number } | null = null;
  function pointerdown(d: Download, e: PointerEvent) {
    press = e.button === 0 && ondragout && draggable(d) ? { id: d.id, x: e.clientX, y: e.clientY } : null;
  }
  function pointermove(d: Download, e: PointerEvent) {
    if (!press || press.id !== d.id || !(e.buttons & 1)) return;
    if (Math.hypot(e.clientX - press.x, e.clientY - press.y) < 6) return;
    press = null;
    ondragout?.(d);
  }

  function sizeOf(d: Download): string {
    if (d.status === "completed") return bytes(d.total_size);
    const known = knownSize(d);
    return known ? bytes(known) : d.downloaded ? bytes(d.downloaded) : "";
  }
</script>

<svelte:window
  onpointerdown={(e) => {
    if (chooser && !(e.target as HTMLElement).closest?.(".chooser")) chooser = null;
  }}
  onkeydown={(e) => e.key === "Escape" && (chooser = null)}
/>

<div class="scroller" bind:this={scroller} bind:clientHeight={viewport} onscroll={(e) => (scrollTop = e.currentTarget.scrollTop)}>
  <div class="table" style:min-width="{totalWidth + 40}px">
    <div
      class="head"
      role="row"
      tabindex="-1"
      style:grid-template-columns={template}
      oncontextmenu={(e) => {
        e.preventDefault();
        chooser = { x: e.clientX, y: e.clientY };
      }}
    >
      {#each cols as c, i (c.id)}
        {@const def = columnDef(c.id)}
        <div
          class="th"
          class:right={def?.align === "right"}
          class:over={overId === c.id && dragId !== c.id}
          role="columnheader"
          tabindex="-1"
          aria-sort={sort === def?.sort ? (reverse ? "ascending" : "descending") : "none"}
          draggable={c.id !== "name"}
          ondragstart={(e) => {
            dragId = c.id;
            e.dataTransfer?.setData("text/plain", c.id);
          }}
          ondragover={(e) => {
            if (dragId && c.id !== "name") {
              e.preventDefault();
              overId = c.id;
            }
          }}
          ondragleave={() => overId === c.id && (overId = null)}
          ondrop={(e) => {
            e.preventDefault();
            drop(c.id);
            dragId = overId = null;
          }}
          ondragend={() => (dragId = overId = null)}
          onclick={() => def && onsort(def.sort)}
          onkeydown={() => {}}
        >
          <span class="th-label">{def?.label ?? c.id}</span>
          {#if def && sort === def.sort}
            {#if reverse}<ArrowUp size={12} />{:else}<ArrowDown size={12} />{/if}
          {/if}
          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <span
            class="grip"
            role="separator"
            aria-orientation="vertical"
            tabindex="-1"
            onpointerdown={(e) => startResize(i, e)}
            onclick={(e) => e.stopPropagation()}
            onkeydown={() => {}}
          ></span>
        </div>
      {/each}
    </div>

    <div class="body" style:height="{items.length * ROW}px">
      {#each visible as d, k (d.id)}
        <div
          class="tr"
          class:selected={selected.has(d.id)}
          class:done={d.status === "completed"}
          style:transform="translateY({(first + k) * ROW}px)"
          style:grid-template-columns={template}
          role="row"
          tabindex="-1"
          onclick={(e) => onrowclick(d, e)}
          ondblclick={() => onrowopen(d)}
          onpointerdown={(e) => pointerdown(d, e)}
          onpointermove={(e) => pointermove(d, e)}
          onpointerup={() => (press = null)}
          ondragstart={(e) => e.preventDefault()}
          oncontextmenu={(e) => {
            e.preventDefault();
            onrowmenu(d, e);
          }}
          onkeydown={() => {}}
        >
          {#each cols as c (c.id)}
            {@const right = columnDef(c.id)?.align === "right"}
            <div class="td" class:right>
              {#if c.id === "name"}
                <FileIcon category={d.category} size={20} />
                <span class="truncate name" title={nameOf(d)}>{nameOf(d)}</span>
                {#if isRecording(d)}<span class="live"><Radio size={10} />{t("details-list-live")}</span>{/if}
              {:else if c.id === "size"}
                <span class="num">{sizeOf(d)}</span>
              {:else if c.id === "progress"}
                {#if d.status === "completed"}
                  <span class="muted">{t("details-list-done")}</span>
                {:else if d.media_progress?.phase === "recording"}
                  <span class="num rec">{duration(d.media_progress.recorded_secs ?? 0)}</span>
                {:else}
                  {@const p = progressOf(d)}
                  <div class="bar"><ProgressBar value={p} status={d.status} height={4} /></div>
                  <span class="num pct">{p.toFixed(p < 10 ? 1 : 0)}%</span>
                {/if}
              {:else if c.id === "speed"}
                <span class="num">{d.status === "downloading" ? speed(d.speed) : ""}</span>
              {:else if c.id === "eta"}
                {@const s = secondsLeft(d)}
                <span class="num">{s == null ? "" : duration(s)}</span>
              {:else if c.id === "status"}
                <span class="dot" style:--c={statusColor(d)}></span>
                <span class="truncate" title={d.status === "failed" ? errorOf(d) : ""}>{statusText(d)}</span>
              {:else if c.id === "queue"}
                <span class="truncate">{store.queueName(d.queue_id)}</span>
              {:else if c.id === "host"}
                <span class="truncate">{host(d.final_url ?? d.url)}</span>
              {:else if c.id === "type"}
                <span class="truncate">{categoryMeta(d.category).label}</span>
              {:else if c.id === "added"}
                <span class="truncate num">{dateTime(d.created_at)}</span>
              {:else if c.id === "finished"}
                <span class="truncate num">{d.completed_at ? dateTime(d.completed_at) : ""}</span>
              {:else if c.id === "connections"}
                <span class="num">{d.status === "downloading" ? d.active_connections : ""}</span>
              {/if}
            </div>
          {/each}
        </div>
      {/each}
    </div>
  </div>
</div>

{#if chooser}
  <div class="chooser" role="menu" style:left="{chooser.x}px" style:top="{chooser.y}px">
    <div class="ch-title">{t("details-list-columns")}</div>
    {#each COLUMNS as def (def.id)}
      {@const on = cols.some((c) => c.id === def.id)}
      <button role="menuitemcheckbox" aria-checked={on} disabled={def.id === "name"} onclick={() => toggleColumn(def.id)}>
        <span class="ck">{#if on}<Check size={13} />{/if}</span>
        {def.label}
      </button>
    {/each}
  </div>
{/if}

<style>
  .scroller {
    flex: 1;
    overflow: auto;
    padding: 0 22px 8px;
  }
  .table {
    position: relative;
  }
  .head {
    position: sticky;
    top: 0;
    z-index: 2;
    display: grid;
    height: 34px;
    background: var(--bg);
    border-bottom: 1px solid var(--border-strong);
  }
  .th {
    position: relative;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 10px;
    font-size: 11.5px;
    font-weight: 650;
    letter-spacing: 0.01em;
    color: var(--text-2);
    cursor: pointer;
    user-select: none;
    min-width: 0;
  }
  .th:hover {
    color: var(--text);
  }
  .th.right {
    justify-content: flex-end;
  }
  .th.over {
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .th-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .grip {
    position: absolute;
    top: 6px;
    bottom: 6px;
    right: -3px;
    width: 7px;
    cursor: col-resize;
    z-index: 1;
  }
  .grip::after {
    content: "";
    position: absolute;
    left: 3px;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--border);
  }
  .th:hover .grip::after {
    background: var(--border-strong);
  }
  .body {
    position: relative;
  }
  .tr {
    position: absolute;
    left: 0;
    right: 0;
    height: 34px;
    display: grid;
    align-items: center;
    border-radius: var(--r-sm);
    font-size: 12.5px;
    outline: none;
  }
  .tr:hover {
    background: var(--hover);
  }
  .tr.selected {
    background: var(--selected);
    box-shadow: inset 0 0 0 1px var(--selected-border);
  }
  .tr.done .name {
    color: var(--text-2);
  }
  .td {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 0 10px;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
  }
  .td.right {
    justify-content: flex-end;
  }
  .truncate {
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .name {
    font-weight: 550;
  }
  .num {
    font-variant-numeric: tabular-nums;
  }
  .muted {
    color: var(--muted);
  }
  .bar {
    flex: 1;
    min-width: 30px;
  }
  .pct {
    width: 38px;
    text-align: right;
    color: var(--text-2);
  }
  .rec {
    color: var(--err);
    font-weight: 600;
  }
  .live {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 1px 6px;
    border-radius: 99px;
    font-size: 10px;
    font-weight: 700;
    color: var(--err);
    background: color-mix(in srgb, var(--err) 12%, transparent);
  }
  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--c);
  }
  .chooser {
    position: fixed;
    z-index: 50;
    min-width: 180px;
    padding: 5px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--r);
    box-shadow: var(--shadow-lg);
  }
  .ch-title {
    padding: 4px 9px 6px;
    font-size: 11px;
    font-weight: 650;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .chooser button {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 6px 9px;
    border: none;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text);
    font: inherit;
    font-size: 13px;
    text-align: left;
    cursor: pointer;
  }
  .chooser button:hover:not(:disabled) {
    background: var(--hover);
  }
  .chooser button:disabled {
    color: var(--muted);
    cursor: default;
  }
  .ck {
    display: inline-flex;
    width: 14px;
    color: var(--accent-text);
  }
</style>
