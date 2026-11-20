<script lang="ts">
  import type { Snippet } from "svelte";
  import Modal from "./Modal.svelte";

  let {
    title,
    confirm,
    danger = false,
    onconfirm,
    onclose,
    children,
  }: {
    title: string;
    confirm: string;
    danger?: boolean;
    onconfirm: () => unknown;
    onclose: () => void;
    children: Snippet;
  } = $props();

  let busy = $state(false);

  async function run() {
    busy = true;
    try {
      await onconfirm();
    } finally {
      busy = false;
      onclose();
    }
  }
</script>

<Modal {title} width={460} {onclose}>
  <div class="body">{@render children()}</div>
  {#snippet footer()}
    <button class="btn btn-ghost" onclick={onclose}>Cancel</button>
    <button class="btn {danger ? 'btn-danger' : 'btn-primary'}" onclick={run} disabled={busy}>{confirm}</button>
  {/snippet}
</Modal>

<style>
  .body {
    display: flex;
    flex-direction: column;
    gap: 14px;
    color: var(--text-2);
    line-height: 1.5;
  }
  .body :global(p) {
    margin: 0;
  }
</style>
