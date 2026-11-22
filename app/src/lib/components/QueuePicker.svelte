<script lang="ts">
  import { t } from "../i18n";
  import { onMount } from "svelte";
  import { api, type Queue } from "../api";

  /** The queue new downloads go into. Hidden while there is only the main queue. */
  let { value = $bindable() }: { value: number } = $props();

  let queues = $state<Queue[]>([]);
  onMount(async () => {
    queues = await api.queues().catch(() => []);
    if (!queues.some((q) => q.id === value)) value = queues[0]?.id ?? value;
  });
</script>

{#if queues.length > 1}
  <label class="queue" title={t("queue-picker-the-queue-these-downloads-go-into")}>
    <span class="hint">{t("queue-picker-queue")}</span>
    <select class="input" bind:value>
      {#each queues as q (q.id)}
        <option value={q.id}>{q.name}</option>
      {/each}
    </select>
  </label>
{/if}

<style>
  .queue {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .queue select {
    height: 34px;
    max-width: 160px;
  }
</style>
