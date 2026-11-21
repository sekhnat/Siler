<script lang="ts" module>
  /** One link offered for download. `pendingId` links it to a browser hand-over. */
  export interface BatchItem {
    key: number;
    url: string;
    filename: string | null;
    size: number | null;
    selected: boolean;
    pendingId: number | null;
    queueId?: number | null;
    /** The browser extension handed over the stream itself. */
    stream?: boolean;
    /** Where it goes (a saved site's folder, a metalink's folder). */
    dir?: string | null;
    /** The checksum it comes with, as `SHA-256 ab12…`. */
    checksum?: string | null;
    mirrors?: number;
    origin?: string;
  }
</script>

<script lang="ts">
  import { Search, ShieldCheck, Waypoints, X } from "@lucide/svelte";
  import { onMount } from "svelte";
  import { api, MAIN_QUEUE, type Category, type OnConflict, type Settings } from "../api";
  import { CATEGORIES, categoryOf } from "../categories";
  import { bytes, host, nameFromUrl } from "../format";
  import { toasts } from "../store.svelte";
  import QueuePicker from "./QueuePicker.svelte";

  let { items, ondone, oncancel }: { items: BatchItem[]; ondone: () => void; oncancel: () => void } = $props();

  // svelte-ignore state_referenced_locally
  let checked = $state<Record<number, boolean>>(Object.fromEntries(items.map((i) => [i.key, i.selected])));
  let settings = $state<Settings | null>(null);
  let query = $state("");
  let type = $state<Category | "all">("all");
  let conflict = $state<OnConflict>("rename");
  // svelte-ignore state_referenced_locally
  let queueId = $state(items.find((i) => i.queueId)?.queueId ?? MAIN_QUEUE);
  let busy = $state(false);

  onMount(async () => {
    settings = await api.getSettings().catch(() => null);
    if (settings) conflict = settings.on_conflict === "ask" ? "rename" : settings.on_conflict;
  });

  const nameOf = (i: BatchItem) => i.filename ?? nameFromUrl(i.url);
  // A manifest is a stream: it becomes a video, whatever its own name says.
  const isStream = (i: BatchItem) => !!i.stream || /\.(m3u8|mpd)(\?|#|$)/i.test(i.url);
  const typed = $derived(
    items.map((i) => ({ ...i, name: nameOf(i), category: isStream(i) ? "video" : categoryOf(nameOf(i), settings?.categories) })),
  );
  const types = $derived(CATEGORIES.map((c) => ({ ...c, n: typed.filter((i) => i.category === c.key).length })).filter((c) => c.n > 0));
  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return typed.filter(
      (i) => (type === "all" || i.category === type) && (!q || i.name.toLowerCase().includes(q) || i.url.toLowerCase().includes(q)),
    );
  });
  const count = $derived(items.filter((i) => checked[i.key]).length);
  const shownOn = $derived(shown.length > 0 && shown.every((i) => checked[i.key]));
  const totalSize = $derived(items.filter((i) => checked[i.key] && i.size).reduce((s, i) => s + (i.size ?? 0), 0));

  function setShown(on: boolean) {
    for (const i of shown) checked[i.key] = on;
  }

  async function submit(startPaused: boolean) {
    busy = true;
    let failed = 0;
    for (const it of items) {
      try {
        if (checked[it.key] && isStream(it)) {
          // A manifest is a stream: its default tracks, as a media download.
          await api.addMedia({ url: it.url, startPaused, pendingId: it.pendingId, onConflict: conflict, queueId });
        } else if (checked[it.key]) {
          await api.add({ url: it.url, filename: it.filename, dir: it.dir ?? null, startPaused, pendingId: it.pendingId, onConflict: conflict, queueId });
        } else if (it.pendingId != null) {
          await api.dismissPending(it.pendingId);
        }
      } catch (e) {
        failed++;
        toasts.error(e);
      }
    }
    busy = false;
    if (!failed) ondone();
  }
</script>

<div class="batch">
  <div>
    <h2>Download {items.length} files</h2>
    <div class="hint">
      {#if items[0]?.origin === "grabber"}Found by the site grabber.{" "}{:else if items[0]?.origin === "list"}From a list or metalink.{" "}{/if}Choose which files K-Star should download.
    </div>
  </div>

  <div class="filters">
    <label class="search">
      <Search size={14} />
      <input placeholder="Filter by name or address" bind:value={query} spellcheck="false" />
      {#if query}<button class="icon-btn sm" onclick={() => (query = "")} aria-label="Clear"><X size={13} /></button>{/if}
    </label>
    <div class="chips">
      <button class="chip" class:on={type === "all"} onclick={() => (type = "all")}>All <span>{items.length}</span></button>
      {#each types as t (t.key)}
        <button class="chip" class:on={type === t.key} style:--c={t.color} onclick={() => (type = t.key)}>
          <t.icon size={12} /> {t.label} <span>{t.n}</span>
        </button>
      {/each}
    </div>
  </div>

  <div class="list">
    <div class="row head">
      <input type="checkbox" checked={shownOn} onchange={() => setShown(!shownOn)} aria-label="Select all shown" />
      <span>{count} of {items.length} selected{totalSize ? ` · ${bytes(totalSize)}` : ""}</span>
      <span class="links">
        <button onclick={() => setShown(true)}>Select shown</button>
        <button onclick={() => setShown(false)}>Clear shown</button>
      </span>
    </div>
    {#each shown as it (it.key)}
      <label class="row">
        <input type="checkbox" bind:checked={checked[it.key]} />
        <span class="name truncate" title={it.url}>{it.name}</span>
        {#if it.checksum}<span class="badge" title="Checked against {it.checksum}"><ShieldCheck size={12} /></span>{/if}
        {#if it.mirrors}<span class="badge" title="{it.mirrors} {it.mirrors === 1 ? 'mirror' : 'mirrors'}"><Waypoints size={12} /> {it.mirrors}</span>{/if}
        <span class="host truncate">{host(it.url)}</span>
        <span class="size">{it.size ? bytes(it.size) : ""}</span>
      </label>
    {:else}
      <div class="none hint">Nothing matches the filter.</div>
    {/each}
  </div>

  <div class="actions">
    <button class="btn btn-ghost" onclick={oncancel} disabled={busy}>Cancel</button>
    <label class="conflict">
      <span class="hint">If a file exists</span>
      <select class="input" bind:value={conflict}>
        <option value="rename">Keep both</option>
        <option value="overwrite">Replace it</option>
        <option value="skip">Skip it</option>
      </select>
    </label>
    <span class="grow"></span>
    <QueuePicker bind:value={queueId} />
    <button class="btn" disabled={!count || busy} onclick={() => submit(true)}>Download later</button>
    <button class="btn btn-primary" disabled={!count || busy} onclick={() => submit(false)}>Download {count}</button>
  </div>
</div>

<style>
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    color: var(--ok);
    font-size: 11px;
    font-weight: 600;
    flex: none;
  }
  .batch {
    display: flex;
    flex-direction: column;
    gap: 12px;
    height: 100%;
    min-height: 0;
  }
  h2 {
    margin: 0;
    font-size: 17px;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  .filters {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 34px;
    padding: 0 6px 0 11px;
    border-radius: var(--r);
    background: var(--surface);
    border: 1px solid var(--border-strong);
    color: var(--muted);
  }
  .search:focus-within {
    border-color: var(--accent);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: none;
    outline: none;
    background: none;
    color: var(--text);
    font: inherit;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    --c: var(--accent);
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 26px;
    padding: 0 10px;
    border-radius: 99px;
    font-size: 12px;
    font-weight: 600;
    color: var(--text-2);
    background: var(--surface-3);
  }
  .chip span {
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .chip.on {
    color: var(--c);
    background: color-mix(in srgb, var(--c) 15%, transparent);
  }
  .list {
    flex: 1;
    min-height: 120px;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    background: var(--surface-2);
  }
  .row {
    display: grid;
    grid-template-columns: 18px minmax(0, 1fr) 140px 70px;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
  }
  .row:last-child {
    border-bottom: none;
  }
  .row.head {
    grid-template-columns: 18px 1fr auto;
    font-size: 12px;
    font-weight: 600;
    color: var(--text-2);
    position: sticky;
    top: 0;
    z-index: 1;
    background: var(--surface-2);
  }
  .links {
    display: flex;
    gap: 12px;
  }
  .links button {
    color: var(--accent-text);
    font-size: 12px;
    font-weight: 600;
  }
  .none {
    padding: 20px;
    text-align: center;
  }
  input[type="checkbox"] {
    accent-color: var(--accent);
    width: 15px;
    height: 15px;
  }
  .name {
    font-weight: 550;
  }
  .host,
  .size {
    font-size: 12px;
    color: var(--muted);
    text-align: right;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .conflict {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .conflict select {
    height: 32px;
    width: auto;
    padding: 0 8px;
  }
  .grow {
    flex: 1;
  }
</style>
