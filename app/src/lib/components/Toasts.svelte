<script lang="ts">
  import { fly } from "svelte/transition";
  import { CircleAlert, CircleCheck, Info } from "@lucide/svelte";
  import { toasts } from "../store.svelte";
</script>

<div class="stack">
  {#each toasts.items as toast (toast.id)}
    <button class="toast {toast.kind}" transition:fly={{ y: 16, duration: 180 }} onclick={() => toasts.dismiss(toast.id)}>
      {#if toast.kind === "error"}<CircleAlert size={16} />{:else if toast.kind === "success"}<CircleCheck size={16} />{:else}<Info size={16} />{/if}
      <span>{toast.text}</span>
    </button>
  {/each}
</div>

<style>
  .stack {
    position: fixed;
    right: 18px;
    bottom: 18px;
    z-index: 100;
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-width: 380px;
  }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 11px 14px;
    text-align: left;
    border-radius: var(--r);
    background: var(--surface);
    border: 1px solid var(--border-strong);
    box-shadow: var(--shadow);
    --c: var(--accent);
  }
  .toast :global(svg) {
    flex: none;
    margin-top: 1px;
    color: var(--c);
  }
  .toast.error {
    --c: var(--err);
  }
  .toast.success {
    --c: var(--ok);
  }
</style>
