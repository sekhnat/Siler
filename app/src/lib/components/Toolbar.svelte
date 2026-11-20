<script lang="ts" module>
  export type SortKey = "added" | "name" | "size" | "progress" | "status" | "queue";
  export const SORTS: { label: string; value: SortKey }[] = [
    { label: "Date added", value: "added" },
    { label: "Name", value: "name" },
    { label: "Size", value: "size" },
    { label: "Progress", value: "progress" },
    { label: "Status", value: "status" },
    { label: "Queue order", value: "queue" },
  ];
</script>

<script lang="ts">
  import { ArrowDownUp, Ellipsis, ListEnd, ListStart, Pause, Play, Search, Trash2, X } from "@lucide/svelte";
  import Menu from "./Menu.svelte";

  let {
    search = $bindable(),
    sort = $bindable(),
    waitingCount,
    queuedCount,
    onstartqueue,
    onstopqueue,
    oncleanup,
    title,
    subtitle,
    selectedCount,
    canResume,
    canPause,
    onresume,
    onpause,
    onremove,
    onresumeall,
    onpauseall,
  }: {
    search: string;
    sort: SortKey;
    waitingCount: number;
    queuedCount: number;
    onstartqueue: () => void;
    onstopqueue: () => void;
    oncleanup: (what: "completed" | "failed") => void;
    title: string;
    subtitle: string;
    selectedCount: number;
    canResume: boolean;
    canPause: boolean;
    onresume: () => void;
    onpause: () => void;
    onremove: () => void;
    onresumeall: () => void;
    onpauseall: () => void;
  } = $props();

  let sortMenu = $state(false);
  let moreMenu = $state(false);
</script>

<div class="bar">
  <div class="heading">
    <h1>{title}</h1>
    <div class="sub">{subtitle}</div>
  </div>

  <div class="actions">
    {#if selectedCount > 0}
      <div class="group">
        <span class="sel">{selectedCount} selected</span>
        <button class="icon-btn" title="Resume (Space)" disabled={!canResume} onclick={onresume}><Play size={17} /></button>
        <button class="icon-btn" title="Pause (Space)" disabled={!canPause} onclick={onpause}><Pause size={17} /></button>
        <button class="icon-btn" title="Remove (Del)" onclick={onremove}><Trash2 size={17} /></button>
      </div>
    {:else}
      <div class="group">
        {#if waitingCount}
          <button class="btn btn-ghost btn-sm" onclick={onstartqueue} title="Start the downloads added with “Download later”"><ListStart size={14} /> Start queue <span class="n">{waitingCount}</span></button>
        {:else if queuedCount}
          <button class="btn btn-ghost btn-sm" onclick={onstopqueue} title="Hold back queued downloads that haven't started"><ListEnd size={14} /> Stop queue</button>
        {/if}
        <button class="btn btn-ghost btn-sm" onclick={onresumeall}><Play size={14} /> Resume all</button>
        <button class="btn btn-ghost btn-sm" onclick={onpauseall}><Pause size={14} /> Pause all</button>
      </div>
    {/if}

    <div class="group">
      <div class="anchor">
        <button class="icon-btn" title="Sort: {SORTS.find((s) => s.value === sort)?.label}" onclick={() => (sortMenu = !sortMenu)}><ArrowDownUp size={16} /></button>
        {#if sortMenu}
          <Menu align="right" value={sort} items={SORTS} onselect={(v) => (sort = v)} onclose={() => (sortMenu = false)} />
        {/if}
      </div>
      <div class="anchor">
        <button class="icon-btn" title="More" onclick={() => (moreMenu = !moreMenu)}><Ellipsis size={17} /></button>
        {#if moreMenu}
          <Menu
            align="right"
            items={[
              { label: "Clear completed (keeps files)", value: "completed" as const },
              { label: "Remove failed downloads", value: "failed" as const },
            ]}
            onselect={oncleanup}
            onclose={() => (moreMenu = false)}
          />
        {/if}
      </div>
    </div>

    <label class="search">
      <Search size={15} />
      <input placeholder="Search downloads" bind:value={search} spellcheck="false" />
      {#if search}<button class="icon-btn sm clear" onclick={() => (search = "")} aria-label="Clear"><X size={14} /></button>{/if}
    </label>
  </div>
</div>

<style>
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 18px 22px 14px;
  }
  h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 720;
    letter-spacing: -0.025em;
  }
  .sub {
    color: var(--muted);
    font-size: 12.5px;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .group {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .anchor {
    position: relative;
  }
  .n {
    padding: 0 6px;
    border-radius: 99px;
    font-size: 11px;
    background: var(--surface-3);
  }
  .sel {
    margin-right: 8px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--accent-text);
  }
  .search {
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    width: 240px;
    height: 34px;
    padding: 0 6px 0 11px;
    border-radius: var(--r);
    background: var(--surface);
    border: 1px solid var(--border-strong);
    color: var(--muted);
    transition: border-color 0.15s, box-shadow 0.15s;
  }
  .search:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 20%, transparent);
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
  .clear {
    width: 22px;
    height: 22px;
  }
</style>
