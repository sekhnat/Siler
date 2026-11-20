<script lang="ts">
  import { api, type Download } from "../api";
  import { nameFromUrl } from "../format";
  import { toasts } from "../store.svelte";
  import Modal from "./Modal.svelte";

  let { items, onclose }: { items: Download[]; onclose: () => void } = $props();

  let deleteFiles = $state(false);
  const anyCompleted = $derived(items.some((d) => d.status === "completed"));
  const anyUnfinished = $derived(items.some((d) => d.status !== "completed"));
  const label = $derived(items.length === 1 ? `“${items[0].filename ?? nameFromUrl(items[0].url)}”` : `${items.length} downloads`);

  async function confirm() {
    for (const d of items) {
      try {
        await api.remove(d.id, deleteFiles);
      } catch (e) {
        toasts.error(e);
      }
    }
    onclose();
  }
</script>

<Modal title="Remove {label}?" width={440} {onclose}>
  <p>The download will be removed from the list.{#if anyUnfinished} Unfinished data is discarded.{/if}</p>
  {#if anyCompleted}
    <button class="toggle" onclick={() => (deleteFiles = !deleteFiles)}>
      <span class="switch" class:on={deleteFiles}></span>
      Also delete the downloaded {items.length === 1 ? "file" : "files"} from disk
    </button>
  {/if}
  {#snippet footer()}
    <button class="btn btn-ghost" onclick={onclose}>Cancel</button>
    <button class="btn btn-danger" onclick={confirm}>Remove</button>
  {/snippet}
</Modal>

<style>
  p {
    margin: 0 0 14px;
    color: var(--text-2);
  }
  .toggle {
    display: flex;
    align-items: center;
    gap: 10px;
    text-align: left;
  }
</style>
