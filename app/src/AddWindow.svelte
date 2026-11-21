<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { api, type Pending } from "./lib/api";
  import AddDialog from "./lib/components/AddDialog.svelte";
  import BatchDialog, { type BatchItem } from "./lib/components/BatchDialog.svelte";
  import Toasts from "./lib/components/Toasts.svelte";

  const win = getCurrentWindow();
  let items = $state<Pending[]>([]);
  let loaded = $state(false);
  let closing = false;

  async function refresh() {
    items = await api.pending();
    loaded = true;
    if (!items.length) finish();
  }

  function finish() {
    closing = true;
    win.destroy();
  }

  async function cancel(id: number) {
    await api.dismissPending(id);
    await refresh();
  }

  async function cancelAll() {
    for (const it of items) await api.dismissPending(it.id);
    await refresh();
  }

  const batch = $derived<BatchItem[]>(
    items.map((p) => ({
      key: p.id,
      url: p.url,
      filename: p.filename ?? p.title,
      size: p.size,
      selected: p.selected,
      pendingId: p.id,
      queueId: p.queue_id,
      stream: p.stream,
      dir: p.dir,
      checksum: p.checksum,
      mirrors: p.mirrors,
      origin: p.origin,
    })),
  );

  onMount(() => {
    refresh();
    const unlisten = listen("pending-changed", refresh);
    // Closing the window with items still waiting means "cancel them".
    const unclose = win.onCloseRequested(async () => {
      if (closing) return;
      for (const it of items) await api.dismissPending(it.id);
    });
    return () => {
      unlisten.then((f) => f());
      unclose.then((f) => f());
    };
  });
</script>

<div class="window">
  {#if loaded && items.length > 1}
    <BatchDialog items={batch} ondone={refresh} oncancel={cancelAll} />
  {:else if loaded && items.length === 1}
    {#key items[0].id}
      <AddDialog pending={items[0]} ondone={refresh} oncancel={() => cancel(items[0].id)} />
    {/key}
  {/if}
</div>

<Toasts />

<style>
  .window {
    height: 100%;
    padding: 22px 24px 20px;
    overflow-y: auto;
    background: var(--surface);
  }
</style>
