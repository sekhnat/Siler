<script lang="ts">
  import type { Segment, Status } from "../api";

  let {
    segments,
    total,
    status,
    downloaded,
    ticks = false,
  }: {
    segments: Segment[];
    total: number | null;
    status: Status;
    downloaded: number;
    /** Marks where each segment starts, as IDM's connections strip does. */
    ticks?: boolean;
  } = $props();

  const running = $derived(status === "downloading");
  const blocks = $derived.by(() => {
    if (!total) return [];
    return [...segments]
      .sort((a, b) => a.start - b.start)
      .map((s) => ({
        left: (s.start / total) * 100,
        width: ((s.end - s.start) / total) * 100,
        fill: s.end > s.start ? ((s.pos - s.start) / (s.end - s.start)) * 100 : 100,
        active: running && s.pos < s.end,
      }));
  });
</script>

{#if blocks.length}
  <div class="map" class:done={status === "completed"} class:ticked={ticks}>
    {#each blocks as b, i (i)}
      <div class="seg" style:left="{b.left}%" style:width="{b.width}%">
        <div class="fill" class:active={b.active} style:width="{b.fill}%"></div>
      </div>
      {#if ticks && b.left > 0}<div class="tick" style:left="{b.left}%"></div>{/if}
    {/each}
  </div>
{:else}
  <div class="map">
    <div class="seg" style:left="0" style:width="100%">
      <div class="fill" style:width="{status === 'completed' ? 100 : total ? (downloaded / total) * 100 : 0}%"></div>
    </div>
  </div>
{/if}

<style>
  .map {
    position: relative;
    height: 26px;
    border-radius: 8px;
    background: var(--track);
    overflow: hidden;
  }
  .seg {
    position: absolute;
    top: 0;
    bottom: 0;
    border-right: 1px solid var(--surface);
  }
  /* No width transition: progress comes four times a second (see ProgressBar). */
  .fill {
    height: 100%;
    background: color-mix(in srgb, var(--accent) 55%, transparent);
  }
  .fill.active {
    background: var(--grad);
    background-size: 400px 100%;
    border-right: 2px solid var(--knob);
  }
  .tick {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    margin-left: -1px;
    background: var(--text-2);
    opacity: 0.55;
    pointer-events: none;
  }
  .ticked {
    border-radius: 4px;
  }
  .done .fill {
    background: color-mix(in srgb, var(--ok) 70%, transparent);
  }
</style>
