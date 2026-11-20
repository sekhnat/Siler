<script lang="ts">
  import {
    ArrowDownToLine,
    ArrowUpToLine,
    ChevronDown,
    ChevronUp,
    Copy,
    ExternalLink,
    FolderOpen,
    KeyRound,
    Link2,
    Pause,
    Play,
    RefreshCw,
    RotateCcw,
    SlidersHorizontal,
    Trash2,
    Zap,
  } from "@lucide/svelte";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import { api, type Download, type QueueMove, type Settings } from "./lib/api";
  import { draggable, startFileDrag } from "./lib/drag";
  import { CATEGORIES, canPause, canResume, isActive } from "./lib/categories";
  import { nameFromUrl, parseUrls, percent, speed } from "./lib/format";
  import { attempt, store, toasts } from "./lib/store.svelte";
  import AddDialog from "./lib/components/AddDialog.svelte";
  import BatchDialog, { type BatchItem } from "./lib/components/BatchDialog.svelte";
  import ConfirmDialog from "./lib/components/ConfirmDialog.svelte";
  import ConfirmRemove from "./lib/components/ConfirmRemove.svelte";
  import ContextMenu, { type MenuEntry } from "./lib/components/ContextMenu.svelte";
  import DetailPanel from "./lib/components/DetailPanel.svelte";
  import DownloadList from "./lib/components/DownloadList.svelte";
  import EmptyState from "./lib/components/EmptyState.svelte";
  import Modal from "./lib/components/Modal.svelte";
  import LoginDialog from "./lib/components/LoginDialog.svelte";
  import PropertiesDialog from "./lib/components/PropertiesDialog.svelte";
  import RefreshLinkDialog from "./lib/components/RefreshLinkDialog.svelte";
  import SettingsDialog from "./lib/components/SettingsDialog.svelte";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import Toasts from "./lib/components/Toasts.svelte";
  import Toolbar, { SORTS, type SortKey } from "./lib/components/Toolbar.svelte";

  const SORT_KEY = "kstar.sort";
  function loadSort(): SortKey {
    try {
      const v = localStorage.getItem(SORT_KEY);
      if (SORTS.some((s) => s.value === v)) return v as SortKey;
    } catch {
      /* storage unavailable */
    }
    return "added";
  }

  let filter = $state("all");
  let search = $state("");
  let sort = $state<SortKey>(loadSort());
  const selected = new SvelteSet<number>();
  let anchor: number | null = null;
  let cursor: number | null = null;
  let list: DownloadList | undefined = $state();

  let addOpen = $state<{ url: string } | null>(null);
  let batchOpen = $state<BatchItem[] | null>(null);
  let settingsOpen = $state(false);
  let removeItems = $state<Download[] | null>(null);
  let propsFor = $state<number | null>(null);
  let refreshFor = $state<number | null>(null);
  let loginFor = $state<number | null>(null);
  let restartItems = $state<Download[] | null>(null);
  let redownloadFor = $state<Download | null>(null);
  let replaceOld = $state(false);
  let removeFailed = $state<Download[] | null>(null);
  let quitCount = $state<number | null>(null);
  let dontAskQuit = $state(false);
  let menu = $state<{ x: number; y: number; items: MenuEntry[] } | null>(null);
  let dragging = $state(false);
  let settings = $state<Settings | null>(null);

  $effect(() => {
    try {
      localStorage.setItem(SORT_KEY, sort);
    } catch {
      /* storage unavailable */
    }
  });

  let lastReload = 0;
  function reloadSoon() {
    // The list knows about moved or deleted files only when it is reloaded.
    if (Date.now() - lastReload < 2000) return;
    lastReload = Date.now();
    store.reload();
  }

  onMount(() => {
    store.init();
    api.getSettings().then((s) => (settings = s));
    window.addEventListener("focus", reloadSoon);
    const askLogin = (e: Event) => (loginFor = (e as CustomEvent<number>).detail);
    window.addEventListener("kstar-login", askLogin);
    const unlisten = [
      listen<number>("select-download", (e) => {
        filter = "all";
        search = "";
        select(e.payload);
      }),
      listen<number>("confirm-quit", (e) => (quitCount = e.payload)),
      listen<number>("ask-login", async (e) => {
        // One prompt at a time; the other downloads keep their "Log in…" menu item.
        if (loginDownload) return;
        if (!store.items[e.payload]?.needs_login) await store.reload();
        loginFor = e.payload;
      }),
      listen<{ id: number; error: string | null }>("refresh-done", (e) => {
        if (refreshFor === e.payload.id) return; // the dialog reports it
        if (e.payload.error) toasts.show(e.payload.error, "error");
        else toasts.show("Link updated — the download continues", "success");
      }),
    ];
    return () => {
      window.removeEventListener("focus", reloadSoon);
      window.removeEventListener("kstar-login", askLogin);
      unlisten.forEach((u) => u.then((f) => f()));
    };
  });

  function matches(d: Download, f: string): boolean {
    switch (f) {
      case "all":
        return true;
      case "active":
        return isActive(d.status);
      case "queued":
        return d.status === "queued" || d.status === "paused" || d.status === "waiting";
      case "completed":
        return d.status === "completed";
      case "failed":
        return d.status === "failed";
      default:
        return d.category === f.slice(4);
    }
  }

  const nameOf = (d: Download) => d.filename ?? nameFromUrl(d.url);
  const STATUS_ORDER: Record<Download["status"], number> = {
    downloading: 0,
    probing: 1,
    queued: 2,
    waiting: 3,
    paused: 4,
    failed: 5,
    completed: 6,
  };
  const compare: Record<SortKey, (a: Download, b: Download) => number> = {
    added: (a, b) => b.id - a.id,
    name: (a, b) => nameOf(a).localeCompare(nameOf(b), undefined, { numeric: true, sensitivity: "base" }),
    size: (a, b) => (b.total_size ?? -1) - (a.total_size ?? -1),
    progress: (a, b) => progressOf(b) - progressOf(a),
    status: (a, b) => STATUS_ORDER[a.status] - STATUS_ORDER[b.status] || b.id - a.id,
    queue: (a, b) => +(a.status === "completed") - +(b.status === "completed") || a.position - b.position,
  };
  const progressOf = (d: Download) => (d.status === "completed" ? 100 : percent(d.total_size, d.downloaded));

  const filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    return store.list
      .filter((d) => matches(d, filter) && (!q || (d.filename ?? "").toLowerCase().includes(q) || d.url.toLowerCase().includes(q)))
      .sort(compare[sort]);
  });
  const selection = $derived(filtered.filter((d) => selected.has(d.id)));
  const detail = $derived(selection.length === 1 ? selection[0] : null);
  const propsDownload = $derived(propsFor != null ? store.items[propsFor] : null);
  const refreshDownload = $derived(refreshFor != null ? store.items[refreshFor] : null);
  const loginDownload = $derived(loginFor != null && store.items[loginFor]?.needs_login ? store.items[loginFor] : null);
  const waitingCount = $derived(store.list.filter((d) => d.status === "waiting").length);
  const queuedCount = $derived(store.list.filter((d) => d.status === "queued").length);

  const title = $derived.by(() => {
    const labels: Record<string, string> = {
      all: "All downloads",
      active: "Downloading",
      queued: "Queued & paused",
      completed: "Completed",
      failed: "Failed",
    };
    return labels[filter] ?? CATEGORIES.find((c) => `cat:${c.key}` === filter)?.label ?? "Downloads";
  });
  const subtitle = $derived.by(() => {
    const active = filtered.filter((d) => isActive(d.status)).length;
    const n = filtered.length;
    const parts = [`${n} ${n === 1 ? "item" : "items"}`];
    if (active) parts.push(`${active} downloading at ${speed(filtered.reduce((s, d) => s + d.speed, 0))}`);
    return parts.join(" · ");
  });

  const anyResumable = $derived(selection.some((d) => canResume(d.status)));
  const anyPausable = $derived(selection.some((d) => canPause(d.status)));

  // ------------------------------------------------------------ selection

  function select(id: number) {
    selected.clear();
    selected.add(id);
    anchor = cursor = id;
    const i = filtered.findIndex((d) => d.id === id);
    if (i >= 0) list?.scrollToIndex(i);
  }

  function rowclick(d: Download, e: MouseEvent) {
    if (e.ctrlKey || e.metaKey) {
      if (selected.has(d.id)) selected.delete(d.id);
      else selected.add(d.id);
      anchor = cursor = d.id;
    } else if (e.shiftKey && anchor != null) {
      extendTo(filtered.findIndex((x) => x.id === d.id));
    } else {
      select(d.id);
    }
  }

  function extendTo(i: number) {
    const a = filtered.findIndex((x) => x.id === anchor);
    if (a < 0) return select(filtered[i].id);
    selected.clear();
    for (const x of filtered.slice(Math.min(a, i), Math.max(a, i) + 1)) selected.add(x.id);
    cursor = filtered[i].id;
    list?.scrollToIndex(i);
  }

  function moveCursor(to: number, extend: boolean) {
    if (!filtered.length) return;
    const i = Math.max(0, Math.min(filtered.length - 1, to));
    if (extend && anchor != null) extendTo(i);
    else select(filtered[i].id);
  }

  function rowopen(d: Download) {
    if (d.status === "completed") {
      if (d.file_missing) redownloadFor = d;
      else attempt(api.openFile(d.id));
    } else if (canResume(d.status)) attempt(api.resume(d.id));
  }

  // ------------------------------------------------------------ actions

  const resumeSel = (items = selection) => items.forEach((d) => canResume(d.status) && attempt(api.resume(d.id)));
  const pauseSel = (items = selection) => items.forEach((d) => canPause(d.status) && attempt(api.pause(d.id)));
  const removeSel = () => selection.length && (removeItems = selection);
  const move = (items: Download[], to: QueueMove) =>
    attempt(api.moveInQueue(items.filter((d) => d.status !== "completed").map((d) => d.id), to)).then(() => {
      if (sort !== "queue") toasts.show("Moved in the queue — sort by queue order to see it");
    });

  /** Drags the file of `d`, or of every selected finished download when `d` is one of them. */
  function dragOut(d: Download) {
    const items = selected.has(d.id) ? selection.filter(draggable) : [d];
    startFileDrag(items.length ? items : [d]).catch((e) => toasts.error(e));
  }

  async function copyLinks(items: Download[]) {
    try {
      await navigator.clipboard.writeText(items.map((d) => d.url).join("\n"));
      toasts.show(items.length > 1 ? `${items.length} links copied` : "Link copied", "success");
    } catch (e) {
      toasts.error(e);
    }
  }

  function openMenu(d: Download, e: MouseEvent) {
    if (!selected.has(d.id)) select(d.id);
    const sel = selection.length ? selection : [d];
    const one = sel.length === 1 ? sel[0] : null;
    const unfinished = sel.filter((x) => x.status !== "completed");
    const items: MenuEntry[] = [
      {
        label: "Open",
        icon: ExternalLink,
        hint: "Enter",
        disabled: !(one && one.status === "completed" && !one.file_missing),
        run: () => one && attempt(api.openFile(one.id)),
      },
      {
        label: "Show in folder",
        icon: FolderOpen,
        disabled: !(one && one.dir),
        run: () => one && attempt(api.showInFolder(one.id)),
      },
      "separator",
    ];
    if (sel.some((x) => canResume(x.status)))
      items.push({ label: one?.status === "waiting" ? "Start" : "Resume", icon: Play, hint: "Space", run: () => resumeSel(sel) });
    if (sel.some((x) => canPause(x.status))) items.push({ label: "Pause", icon: Pause, hint: "Space", run: () => pauseSel(sel) });
    items.push(
      {
        label: "Start now",
        icon: Zap,
        disabled: !sel.some((x) => x.status !== "completed" && !isActive(x.status)),
        run: () => sel.forEach((x) => x.status !== "completed" && attempt(api.startNow(x.id))),
      },
      { label: "Restart…", icon: RotateCcw, disabled: !unfinished.length, run: () => (restartItems = unfinished) },
      {
        label: "Download again…",
        icon: RefreshCw,
        disabled: !(one && one.status === "completed"),
        run: () => one && (redownloadFor = one),
      },
      ...(one?.needs_login
        ? [{ label: "Log in…", icon: KeyRound, run: () => (loginFor = one.id) } satisfies MenuEntry]
        : []),
      {
        label: "Refresh link…",
        icon: Link2,
        disabled: !(one && one.status !== "completed"),
        run: () => one && (refreshFor = one.id),
      },
      "separator",
      { label: sel.length > 1 ? "Copy links" : "Copy link", icon: Copy, run: () => copyLinks(sel) },
      { label: "Properties…", icon: SlidersHorizontal, disabled: !one, run: () => one && (propsFor = one.id) },
      "separator",
      { label: "Move to top", icon: ArrowUpToLine, disabled: !unfinished.length, run: () => move(sel, "top") },
      { label: "Move up", icon: ChevronUp, disabled: !unfinished.length, run: () => move(sel, "up") },
      { label: "Move down", icon: ChevronDown, disabled: !unfinished.length, run: () => move(sel, "down") },
      { label: "Move to bottom", icon: ArrowDownToLine, disabled: !unfinished.length, run: () => move(sel, "bottom") },
      "separator",
      { label: "Remove…", icon: Trash2, hint: "Del", danger: true, run: () => (removeItems = sel) },
    );
    menu = { x: e.clientX, y: e.clientY, items };
  }

  async function setGlobalLimit(v: number) {
    if (!settings) return;
    const s = await attempt(api.setSettings({ ...settings, global_speed_limit: v }));
    if (s) settings = s;
  }

  async function cleanup(what: "completed" | "failed") {
    const items = store.list.filter((d) => d.status === what);
    if (!items.length) return toasts.show(what === "completed" ? "No completed downloads" : "No failed downloads");
    if (what === "failed") return (removeFailed = items);
    const n = await attempt(api.removeMany(items.map((d) => d.id), false));
    if (n) toasts.show(`Cleared ${n} completed ${n === 1 ? "download" : "downloads"}; the files are kept`, "success");
  }

  // ------------------------------------------------------------ adding

  function openUrls(urls: string[]) {
    if (!urls.length) return;
    if (urls.length === 1) addOpen = { url: urls[0] };
    else {
      addOpen = null;
      batchOpen = urls.map((url, i) => ({ key: i + 1, url, filename: null, size: null, selected: true, pendingId: null }));
    }
  }

  const modalOpen = $derived(
    !!addOpen ||
      !!batchOpen ||
      settingsOpen ||
      !!removeItems ||
      propsFor != null ||
      refreshFor != null ||
      !!restartItems ||
      !!redownloadFor ||
      !!removeFailed ||
      quitCount != null,
  );

  const inField = (t: EventTarget | null) => t instanceof Element && !!t.closest("input, textarea, select");

  function onkeydown(e: KeyboardEvent) {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && e.key.toLowerCase() === "q") {
      e.preventDefault();
      api.requestQuit();
      return;
    }
    if (modalOpen || menu) return;
    const typing = inField(e.target);
    const ci = filtered.findIndex((d) => d.id === (cursor ?? anchor));
    if (mod && e.key.toLowerCase() === "n") {
      e.preventDefault();
      addOpen = { url: "" };
    } else if (typing) {
      return;
    } else if (mod && e.key.toLowerCase() === "a") {
      e.preventDefault();
      filtered.forEach((d) => selected.add(d.id));
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const step = e.key === "ArrowDown" ? 1 : -1;
      moveCursor(ci < 0 ? 0 : ci + step, e.shiftKey);
    } else if (e.key === "PageDown" || e.key === "PageUp") {
      e.preventDefault();
      const page = list?.pageSize() ?? 5;
      moveCursor((ci < 0 ? 0 : ci) + (e.key === "PageDown" ? page : -page), e.shiftKey);
    } else if (e.key === "Home" || e.key === "End") {
      e.preventDefault();
      moveCursor(e.key === "Home" ? 0 : filtered.length - 1, e.shiftKey);
    } else if (e.key === "Delete") {
      removeSel();
    } else if (e.key === " " && selection.length) {
      e.preventDefault();
      if (anyPausable) pauseSel();
      else resumeSel();
    } else if (e.key === "Enter" && detail) {
      rowopen(detail);
    } else if (e.key === "ContextMenu" && detail) {
      e.preventDefault();
      const i = filtered.indexOf(detail);
      openMenu(detail, new MouseEvent("contextmenu", { clientX: 320, clientY: 120 + Math.min(i, 6) * 72 }));
    } else if (e.key === "Escape") {
      selected.clear();
    }
  }

  function onpaste(e: ClipboardEvent) {
    if (modalOpen || inField(e.target)) return;
    const urls = parseUrls(e.clipboardData?.getData("text") ?? "");
    if (urls.length) {
      e.preventDefault();
      openUrls(urls);
    }
  }

  const droppable = (e: DragEvent) => {
    const types = e.dataTransfer?.types ?? [];
    return !modalOpen && (types.includes("text/uri-list") || types.includes("text/plain"));
  };

  function ondragover(e: DragEvent) {
    if (!droppable(e)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    dragging = true;
  }

  function ondrop(e: DragEvent) {
    dragging = false;
    if (!droppable(e)) return;
    e.preventDefault();
    const dt = e.dataTransfer!;
    const urls = parseUrls(dt.getData("text/uri-list") || dt.getData("text/plain"));
    if (urls.length) openUrls(urls);
    else toasts.show("Drop a link to a file (http or https)");
  }
</script>

<svelte:window {onkeydown} {onpaste} />

<div
  class="shell"
  role="application"
  {ondragover}
  {ondrop}
  ondragleave={(e) => {
    if (!e.relatedTarget) dragging = false;
  }}
>
  <Sidebar
    bind:filter
    globalLimit={settings?.global_speed_limit ?? 0}
    onlimit={setGlobalLimit}
    onadd={() => (addOpen = { url: "" })}
    onsettings={() => (settingsOpen = true)}
  />

  <main>
    <Toolbar
      bind:search
      bind:sort
      {title}
      {subtitle}
      {waitingCount}
      {queuedCount}
      selectedCount={selection.length}
      canResume={anyResumable}
      canPause={anyPausable}
      onresume={() => resumeSel()}
      onpause={() => pauseSel()}
      onremove={removeSel}
      onresumeall={() => attempt(api.resumeAll())}
      onpauseall={() => attempt(api.pauseAll())}
      onstartqueue={() => attempt(api.startQueue())}
      onstopqueue={() => attempt(api.stopQueue())}
      oncleanup={cleanup}
    />
    <div class="content">
      <div class="list">
        {#if !store.loaded}
          <div></div>
        {:else if filtered.length}
          <DownloadList bind:this={list} items={filtered} {selected} onrowclick={rowclick} onrowopen={rowopen} onrowmenu={openMenu} ondragout={dragOut} />
        {:else}
          <EmptyState filtered={store.list.length > 0} onadd={() => (addOpen = { url: "" })} />
        {/if}
      </div>
      {#if detail}
        <DetailPanel
          d={detail}
          onclose={() => selected.clear()}
          onremove={() => (removeItems = [detail])}
          onproperties={() => (propsFor = detail.id)}
          onredownload={() => (redownloadFor = detail)}
        />
      {/if}
    </div>
  </main>

  {#if dragging}
    <div class="drop">
      <div class="drop-card">Drop links to download them</div>
    </div>
  {/if}
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

{#if addOpen}
  <Modal width={620} onclose={() => (addOpen = null)}>
    <AddDialog initialUrl={addOpen.url} ondone={() => (addOpen = null)} oncancel={() => (addOpen = null)} onmulti={openUrls} />
  </Modal>
{/if}

{#if batchOpen}
  <Modal width={700} onclose={() => (batchOpen = null)}>
    <div class="batch-wrap">
      <BatchDialog items={batchOpen} ondone={() => (batchOpen = null)} oncancel={() => (batchOpen = null)} />
    </div>
  </Modal>
{/if}

{#if settingsOpen && settings}
  <SettingsDialog {settings} onclose={() => (settingsOpen = false)} onsaved={(s) => (settings = s)} />
{/if}

{#if propsDownload}
  <PropertiesDialog
    d={propsDownload}
    onclose={() => (propsFor = null)}
    onrefresh={() => {
      refreshFor = propsFor;
      propsFor = null;
    }}
  />
{/if}

{#if loginDownload}
  <LoginDialog d={loginDownload} onclose={() => (loginFor = null)} />
{/if}

{#if refreshDownload}
  <RefreshLinkDialog d={refreshDownload} onclose={() => (refreshFor = null)} />
{/if}

{#if restartItems}
  <ConfirmDialog
    title={restartItems.length === 1 ? "Restart this download?" : `Restart ${restartItems.length} downloads?`}
    confirm="Restart"
    danger
    onclose={() => (restartItems = null)}
    onconfirm={async () => {
      for (const d of restartItems ?? []) await attempt(api.restart(d.id));
    }}
  >
    <p>What was downloaded so far is discarded and the {restartItems.length === 1 ? "file is" : "files are"} downloaded again from the start.</p>
  </ConfirmDialog>
{/if}

{#if redownloadFor}
  {@const d = redownloadFor}
  <ConfirmDialog
    title="Download “{d.filename ?? nameFromUrl(d.url)}” again?"
    confirm="Download again"
    onclose={() => {
      redownloadFor = null;
      replaceOld = false;
    }}
    onconfirm={() => attempt(api.redownload(d.id, replaceOld && !d.file_missing))}
  >
    {#if d.file_missing}
      <p>The file was moved or deleted. K-Star downloads it again into {d.dir ?? "its folder"}.</p>
    {:else}
      <p>The file is downloaded again from {d.url.length > 60 ? "its address" : d.url}.</p>
      <button class="toggle" onclick={() => (replaceOld = !replaceOld)}>
        <span class="switch" class:on={replaceOld}></span>
        Replace the existing file when done (otherwise both are kept)
      </button>
    {/if}
  </ConfirmDialog>
{/if}

{#if removeFailed}
  <ConfirmDialog
    title="Remove {removeFailed.length} failed {removeFailed.length === 1 ? 'download' : 'downloads'}?"
    confirm="Remove"
    danger
    onclose={() => (removeFailed = null)}
    onconfirm={() => attempt(api.removeMany((removeFailed ?? []).map((d) => d.id), false))}
  >
    <p>They are removed from the list and their unfinished data is discarded.</p>
  </ConfirmDialog>
{/if}

{#if quitCount != null}
  <ConfirmDialog
    title="Quit K-Star?"
    confirm="Quit"
    onclose={() => {
      quitCount = null;
      dontAskQuit = false;
    }}
    onconfirm={() => api.quit(dontAskQuit)}
  >
    <p>
      {quitCount === 1 ? "A download is" : `${quitCount} downloads are`} still running. {quitCount === 1 ? "It pauses" : "They pause"} now and
      {settings?.resume_on_startup === false ? "can be resumed" : "continue"} the next time K-Star starts.
    </p>
    <button class="toggle" onclick={() => (dontAskQuit = !dontAskQuit)}>
      <span class="switch" class:on={dontAskQuit}></span>
      Don't ask again
    </button>
  </ConfirmDialog>
{/if}

{#if removeItems}
  <ConfirmRemove
    items={removeItems}
    onclose={() => {
      removeItems = null;
      selected.clear();
    }}
  />
{/if}

<Toasts />

<style>
  .shell {
    position: relative;
    display: flex;
    height: 100%;
  }
  main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .content {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .list {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .batch-wrap {
    height: min(560px, calc(100vh - 140px));
  }
  .toggle {
    display: flex;
    align-items: center;
    gap: 10px;
    text-align: left;
    color: var(--text);
  }
  .drop {
    position: absolute;
    inset: 0;
    z-index: 45;
    display: grid;
    place-items: center;
    padding: 24px;
    background: color-mix(in srgb, var(--accent) 10%, var(--backdrop));
    pointer-events: none;
  }
  .drop-card {
    padding: 28px 40px;
    border-radius: var(--r-xl);
    border: 2px dashed var(--accent);
    background: var(--surface);
    font-size: 16px;
    font-weight: 650;
    color: var(--accent-text);
    box-shadow: var(--shadow-lg);
  }
</style>
