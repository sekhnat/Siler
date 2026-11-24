<script lang="ts">
  import type { Status } from "../api";

  let {
    value,
    status,
    height = 6,
    shimmer = false,
  }: {
    value: number;
    status: Status;
    height?: number;
    /** A light sweeping over the bar while it runs: for a bar on its own, not in lists. */
    shimmer?: boolean;
  } = $props();
</script>

<div class="track" style:height="{height}px">
  <!-- No width transition: progress comes four times a second, and a transition restarted
       each time never ends, so the bar would be redrawn every frame. -->
  <div class="fill {status}" class:shimmer style:width="{Math.max(0, Math.min(100, value))}%"></div>
</div>

<style>
  .track {
    width: 100%;
    border-radius: 99px;
    background: var(--track);
    overflow: hidden;
  }
  .fill {
    position: relative;
    height: 100%;
    border-radius: 99px;
    background: var(--queued);
    overflow: hidden;
  }
  .fill.downloading,
  .fill.probing {
    background: var(--grad);
  }
  /* Moved, not repainted: the stripe slides by transform, which needs no redraw. */
  .fill.shimmer.downloading::after,
  .fill.shimmer.probing::after {
    content: "";
    position: absolute;
    inset: 0 auto 0 0;
    width: 200px;
    background: linear-gradient(90deg, transparent, var(--shine), transparent);
    animation: sweep 1.6s linear infinite;
    will-change: transform;
  }
  @keyframes sweep {
    from {
      transform: translateX(-200px);
    }
    to {
      transform: translateX(100vw);
    }
  }
  .fill.completed {
    background: var(--ok);
  }
  .fill.paused {
    background: var(--warn);
  }
  .fill.failed {
    background: var(--err);
  }
  @media (prefers-reduced-motion: reduce) {
    .fill::after {
      animation: none !important;
      display: none;
    }
  }
</style>
