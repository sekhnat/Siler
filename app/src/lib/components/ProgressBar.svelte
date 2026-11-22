<script lang="ts">
  import type { Status } from "../api";

  let { value, status, height = 6 }: { value: number; status: Status; height?: number } = $props();
</script>

<div class="track" style:height="{height}px">
  <div class="fill {status}" style:width="{Math.max(0, Math.min(100, value))}%"></div>
</div>

<style>
  .track {
    width: 100%;
    border-radius: 99px;
    background: var(--track);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    border-radius: 99px;
    transition: width 0.3s linear;
    background: var(--queued);
  }
  .fill.downloading,
  .fill.probing {
    background:
      linear-gradient(90deg, transparent, var(--shine), transparent) 0 0 / 200px 100% no-repeat,
      var(--grad);
    animation: shimmer 1.6s linear infinite;
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
</style>
