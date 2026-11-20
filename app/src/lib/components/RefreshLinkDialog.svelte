<script lang="ts">
  import { ExternalLink, LoaderCircle, MousePointerClick, TriangleAlert } from "@lucide/svelte";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import { api, errorText, type Download } from "../api";
  import { bytes, host, looksLikeUrl, nameFromUrl } from "../format";
  import { toasts } from "../store.svelte";
  import Modal from "./Modal.svelte";

  let { d, onclose }: { d: Download; onclose: () => void } = $props();

  let url = $state("");
  let busy = $state(false);
  let error = $state("");
  let waiting = $state(false);
  const page = $derived(d.page_url ?? d.referrer);

  onMount(() => {
    const un = listen<{ id: number; error: string | null }>("refresh-done", (e) => {
      if (e.payload.id !== d.id) return;
      waiting = false;
      if (e.payload.error) error = e.payload.error;
      else done();
    });
    return () => {
      un.then((f) => f());
    };
  });

  function done() {
    toasts.show("Link updated — the download continues", "success");
    onclose();
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (!looksLikeUrl(url) || busy) return;
    busy = true;
    error = "";
    try {
      await api.refreshUrl(d.id, url.trim());
      done();
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  async function openPage() {
    error = "";
    try {
      await api.awaitRefresh(d.id);
      waiting = true;
    } catch (e) {
      error = errorText(e);
    }
  }

  function close() {
    if (waiting) api.cancelAwaitRefresh();
    onclose();
  }
</script>

<Modal title="Refresh download link" width={540} onclose={close}>
  <p class="intro">
    If the link to <b>{d.filename ?? nameFromUrl(d.url)}</b> expired, give K-Star a new one. The {bytes(d.downloaded)} already downloaded are kept.
  </p>

  <form onsubmit={submit} class="field">
    <span class="label">New address</span>
    <div class="row">
      <!-- svelte-ignore a11y_autofocus -->
      <input class="input mono" bind:value={url} placeholder="https://…" spellcheck="false" autofocus />
      <button class="btn btn-primary" type="submit" disabled={!looksLikeUrl(url) || busy}>
        {#if busy}<LoaderCircle size={14} class="spin" />{/if} Update
      </button>
    </div>
  </form>

  {#if page}
    <div class="or">or</div>
    <div class="catch" class:on={waiting}>
      {#if waiting}
        <MousePointerClick size={20} />
        <div class="min">
          <div class="t">Click the download link on the page again</div>
          <div class="hint">K-Star will take the new link from Firefox and continue this download. Waiting for 2 minutes.</div>
        </div>
        <LoaderCircle size={16} class="spin" />
      {:else}
        <ExternalLink size={20} />
        <div class="min">
          <div class="t">Get a fresh link from {host(page) || "the website"}</div>
          <div class="hint truncate">Opens the page the download came from.</div>
        </div>
        <button class="btn" onclick={openPage}>Open page</button>
      {/if}
    </div>
  {/if}

  {#if error}
    <div class="error"><TriangleAlert size={15} /><span class="selectable">{error}</span></div>
  {/if}

  {#snippet footer()}
    <button class="btn btn-ghost" onclick={close}>Cancel</button>
  {/snippet}
</Modal>

<style>
  .intro {
    margin: 0 0 16px;
    color: var(--text-2);
    line-height: 1.5;
  }
  .row {
    display: flex;
    gap: 6px;
  }
  .row .btn {
    flex: none;
  }
  .or {
    margin: 14px 0;
    text-align: center;
    font-size: 12px;
    color: var(--muted);
  }
  .catch {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 14px;
    border-radius: var(--r-lg);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .catch.on {
    background: color-mix(in srgb, var(--accent) 9%, transparent);
    border-color: color-mix(in srgb, var(--accent) 30%, transparent);
  }
  .catch > :global(svg:first-child) {
    flex: none;
    color: var(--accent);
  }
  .min {
    flex: 1;
    min-width: 0;
  }
  .t {
    font-weight: 600;
  }
  .error {
    display: flex;
    gap: 9px;
    margin-top: 14px;
    padding: 10px 12px;
    border-radius: var(--r);
    font-size: 12.5px;
    color: var(--text-2);
    background: color-mix(in srgb, var(--err) 10%, transparent);
  }
  .error :global(svg) {
    flex: none;
    color: var(--err);
  }
  :global(.spin) {
    animation: spin 0.9s linear infinite;
  }
</style>
