<script lang="ts">
  import type { Download } from "../api";
  import DownloadRow from "./DownloadRow.svelte";

  let {
    items,
    selected,
    onrowclick,
    onrowopen,
    onrowmenu,
  }: {
    items: Download[];
    selected: Set<number>;
    onrowclick: (d: Download, e: MouseEvent) => void;
    onrowopen: (d: Download) => void;
    onrowmenu: (d: Download, e: MouseEvent) => void;
  } = $props();

  const ROW = 72;
  const OVERSCAN = 6;
  let scrollTop = $state(0);
  let viewport = $state(600);
  let scroller: HTMLDivElement | undefined = $state();

  /** Rows that fit in the view, for Page Up/Down. */
  export function pageSize(): number {
    return Math.max(1, Math.floor(viewport / ROW) - 1);
  }

  /** Scrolls just enough to show row `i`. */
  export function scrollToIndex(i: number) {
    if (!scroller) return;
    const top = i * ROW;
    if (top < scroller.scrollTop) scroller.scrollTop = top;
    else if (top + ROW > scroller.scrollTop + viewport) scroller.scrollTop = top + ROW - viewport + 8;
  }

  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN));
  const last = $derived(Math.min(items.length, Math.ceil((scrollTop + viewport) / ROW) + OVERSCAN));
  const visible = $derived(items.slice(first, last));
</script>

<div class="scroller" bind:this={scroller} bind:clientHeight={viewport} onscroll={(e) => (scrollTop = e.currentTarget.scrollTop)}>
  <div class="spacer" style:height="{items.length * ROW + 8}px">
    {#each visible as d, i (d.id)}
      <div
        class="slot"
        style:transform="translateY({(first + i) * ROW}px)"
        role="row"
        tabindex="-1"
        onclick={(e) => onrowclick(d, e)}
        ondblclick={() => onrowopen(d)}
        oncontextmenu={(e) => {
          e.preventDefault();
          onrowmenu(d, e);
        }}
        onkeydown={() => {}}
      >
        <DownloadRow {d} selected={selected.has(d.id)} />
      </div>
    {/each}
  </div>
</div>

<style>
  .scroller {
    flex: 1;
    overflow-y: auto;
    padding: 0 22px 0;
  }
  .spacer {
    position: relative;
  }
  .slot {
    position: absolute;
    left: 0;
    right: 0;
    height: 72px;
    padding: 3px 0;
    outline: none;
  }
</style>
