<script lang="ts">
  import { Gauge } from "@lucide/svelte";
  import { api } from "../api";
  import { bytes } from "../format";
  import { attempt, store } from "../store.svelte";

  const q = $derived(store.quota);
  const at = $derived(
    q?.resumes_at ? new Date(q.resumes_at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" }) : null,
  );
</script>

{#if q?.enabled && q.reached}
  <div class="banner">
    <Gauge size={16} />
    <span class="grow">
      <b>Data quota reached</b> — {bytes(q.used)} of {bytes(q.limit)} used. Downloads wait{at ? ` until ${at}` : " for the next period"}.
    </span>
    <button class="btn btn-sm" onclick={() => attempt(api.ignoreQuota())}>Ignore for this period</button>
  </div>
{/if}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 0 14px 10px;
    padding: 10px 14px;
    border-radius: var(--r-lg);
    background: color-mix(in srgb, var(--warn) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 30%, transparent);
    font-size: 12.5px;
  }
  .banner > :global(svg) {
    color: var(--warn);
    flex: none;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
</style>
