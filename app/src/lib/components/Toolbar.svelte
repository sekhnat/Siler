<script lang="ts" module>
  export { SORTS, type SortKey } from "../columns";
</script>

<script lang="ts">
  import { t } from "../i18n";
  import { ArrowDownUp, Ellipsis, FileDown, Globe, LayoutGrid, LayoutList, ListEnd, ListOrdered, ListStart, Pause, Play, Power, Search, Settings2, Trash2, X } from "@lucide/svelte";
  import type { AfterDone } from "../api";
  import { SORTS, type SortKey } from "../columns";
  import Menu from "./Menu.svelte";

  const WHEN_DONE: { label: string; value: AfterDone }[] = [
    { label: t("toolbar-do-nothing"), value: "nothing" },
    { label: t("toolbar-quit-siler"), value: "quit" },
    { label: t("toolbar-sleep"), value: "sleep" },
    { label: t("toolbar-hibernate"), value: "hibernate" },
    { label: t("toolbar-shut-down"), value: "power_off" },
  ];

  let {
    search = $bindable(),
    sort = $bindable(),
    reverse = $bindable(),
    items,
    view,
    onview,
    onsettings,
    oncustomize,
    waitingCount,
    queuedCount,
    onstartqueue,
    onstopqueue,
    oncleanup,
    ontool,
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
    whenDone,
    onwhendone,
  }: {
    search: string;
    sort: SortKey;
    /** The sort runs the other way from its natural order. */
    reverse: boolean;
    /** The buttons beside the search box, in order. */
    items: string[];
    view: "cards" | "details";
    onview: (v: "cards" | "details") => void;
    onsettings: () => void;
    oncustomize: () => void;
    waitingCount: number;
    queuedCount: number;
    onstartqueue: () => void;
    onstopqueue: () => void;
    oncleanup: (what: "completed" | "failed") => void;
    /** Batch pattern, site grabber, import or export. */
    ontool: (tool: "batch" | "grabber" | "import" | "export") => void;
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
    /** What happens once all downloads have finished (a one-off choice). */
    whenDone: AfterDone;
    onwhendone: (a: AfterDone) => void;
  } = $props();
  let whenMenu = $state(false);
  // "More" holds "Customize toolbar…", so it is always there.
  const shown = $derived(items.includes("more") ? items : [...items, "more"]);

  let sortMenu = $state(false);
  let moreMenu = $state(false);
</script>

<div class="bar">
  <div class="heading">
    <h1>{title}</h1>
    <div class="sub" title={subtitle}>{subtitle}</div>
  </div>

  <div class="actions">
    {#if selectedCount > 0}
      <div class="group">
        <span class="sel">{t("toolbar-selected", { selectedCount })}</span>
        <button class="icon-btn" title={t("toolbar-resume-space")} disabled={!canResume} onclick={onresume}><Play size={17} /></button>
        <button class="icon-btn" title={t("toolbar-pause-space")} disabled={!canPause} onclick={onpause}><Pause size={17} /></button>
        <button class="icon-btn" title={t("toolbar-remove-del")} onclick={onremove}><Trash2 size={17} /></button>
      </div>
    {/if}

    <div class="group">
      {#each shown as item (item)}
        {#if item === "queue" && selectedCount === 0}
          {#if waitingCount}
            <button class="btn btn-ghost btn-sm" onclick={onstartqueue} title={t("toolbar-start-the-downloads-added-with-download")}><ListStart size={14} /> {t("toolbar-start-queue")} <span class="n">{waitingCount}</span></button>
          {:else if queuedCount}
            <button class="btn btn-ghost btn-sm" onclick={onstopqueue} title={t("toolbar-hold-back-queued-downloads-that-haven")}><ListEnd size={14} /> {t("toolbar-stop-queue")}</button>
          {/if}
        {:else if item === "resume_all" && selectedCount === 0}
          <button class="btn btn-ghost btn-sm" onclick={onresumeall}><Play size={14} /> {t("toolbar-resume-all")}</button>
        {:else if item === "pause_all" && selectedCount === 0}
          <button class="btn btn-ghost btn-sm" onclick={onpauseall}><Pause size={14} /> {t("toolbar-pause-all")}</button>
        {:else if item === "when_done"}
          <div class="anchor">
            <button
              class="icon-btn"
              class:armed={whenDone !== "nothing"}
              title={whenDone === "nothing" ? t("toolbar-when-all-downloads-finish") : t("toolbar-when-all-downloads-finish-action", { action: WHEN_DONE.find((w) => w.value === whenDone)?.label })}
              onclick={() => (whenMenu = !whenMenu)}><Power size={16} /></button
            >
            {#if whenMenu}
              <Menu align="right" value={whenDone} items={WHEN_DONE} onselect={onwhendone} onclose={() => (whenMenu = false)} />
            {/if}
          </div>
        {:else if item === "sort"}
          <div class="anchor">
            <button class="icon-btn" title={(reverse ? t("toolbar-sort-reversed", { by: SORTS.find((s) => s.value === sort)?.label }) : t("toolbar-sort", { by: SORTS.find((s) => s.value === sort)?.label }))} onclick={() => (sortMenu = !sortMenu)}><ArrowDownUp size={16} /></button>
            {#if sortMenu}
              <Menu
                align="right"
                value={sort}
                items={[...SORTS, { label: reverse ? t("toolbar-reverse-order") : t("toolbar-reverse-order-2"), value: "__reverse" as SortKey }]}
                onselect={(v) => (v === ("__reverse" as SortKey) ? (reverse = !reverse) : ((sort = v), (reverse = false)))}
                onclose={() => (sortMenu = false)}
              />
            {/if}
          </div>
        {:else if item === "view"}
          <button class="icon-btn" title={view === "cards" ? t("toolbar-show-as-a-table-details") : t("toolbar-show-as-cards")} onclick={() => onview(view === "cards" ? "details" : "cards")}>
            {#if view === "cards"}<LayoutList size={16} />{:else}<LayoutGrid size={16} />{/if}
          </button>
        {:else if item === "batch"}
          <button class="icon-btn" title={t("toolbar-batch-download")} onclick={() => ontool("batch")}><ListOrdered size={16} /></button>
        {:else if item === "grabber"}
          <button class="icon-btn" title={t("toolbar-site-grabber")} onclick={() => ontool("grabber")}><Globe size={16} /></button>
        {:else if item === "import"}
          <button class="icon-btn" title={t("toolbar-import-list")} onclick={() => ontool("import")}><FileDown size={16} /></button>
        {:else if item === "settings"}
          <button class="icon-btn" title={t("toolbar-settings")} onclick={onsettings}><Settings2 size={16} /></button>
        {:else if item === "more"}
          <div class="anchor">
            <button class="icon-btn" title={t("toolbar-more")} onclick={() => (moreMenu = !moreMenu)}><Ellipsis size={17} /></button>
            {#if moreMenu}
              <Menu
                align="right"
                items={[
                  { label: t("toolbar-batch-download"), value: "batch" as const },
                  { label: t("toolbar-site-grabber"), value: "grabber" as const },
                  { label: t("toolbar-import-list"), value: "import" as const },
                  { label: t("toolbar-export-list"), value: "export" as const },
                  { label: t("toolbar-clear-completed-keeps-files"), value: "completed" as const },
                  { label: t("toolbar-remove-failed-downloads"), value: "failed" as const },
                  { label: t("toolbar-customize-toolbar"), value: "customize" as const },
                ]}
                onselect={(v) => (v === "customize" ? oncustomize() : v === "completed" || v === "failed" ? oncleanup(v) : ontool(v))}
                onclose={() => (moreMenu = false)}
              />
            {/if}
          </div>
        {/if}
      {/each}
    </div>

    <label class="search">
      <Search size={15} />
      <input placeholder={t("toolbar-search-downloads")} bind:value={search} spellcheck="false" />
      {#if search}<button class="icon-btn sm clear" onclick={() => (search = "")} aria-label={t("toolbar-clear")}><X size={14} /></button>{/if}
    </label>
  </div>
</div>

<style>
  .armed {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 14%, transparent);
  }
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
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .heading {
    min-width: 0;
    flex: 1;
  }
  .sub {
    color: var(--muted);
    font-size: 12.5px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
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
