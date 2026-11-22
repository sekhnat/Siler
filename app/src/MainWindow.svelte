<script lang="ts">
  import { t } from "./lib/i18n";
  import { ArrowDownToLine, ArrowUpToLine, ChevronDown, ChevronUp, CircleStop, Copy, ExternalLink, FileOutput, FolderOpen, KeyRound, Link2, ListOrdered, Pause, Play, RefreshCcwDot, RefreshCw, RotateCcw, ShieldCheck, SlidersHorizontal, Trash2, Zap } from "@lucide/svelte";
  import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import { api, MAIN_QUEUE, type AfterDone, type Download, type ListFormat, type Queue, type QueueMove, type Settings } from "./lib/api";
  import { draggable, startFileDrag } from "./lib/drag";
  import { CATEGORIES, canPause, canResume, isActive } from "./lib/categories";
  import { describeSchedule, nameFromUrl, parseUrls, relativeTime, speed, syncLabel } from "./lib/format";
  import { attempt, store, toasts } from "./lib/store.svelte";
  import AddDialog from "./lib/components/AddDialog.svelte";
  import BatchDialog, { type BatchItem } from "./lib/components/BatchDialog.svelte";
  import BatchPatternDialog from "./lib/components/BatchPatternDialog.svelte";
  import GrabberDialog from "./lib/components/GrabberDialog.svelte";
  import QuotaBanner from "./lib/components/QuotaBanner.svelte";
  import UpdateBanner from "./lib/components/UpdateBanner.svelte";
  import VerifyDialog from "./lib/components/VerifyDialog.svelte";
  import ConfirmDialog from "./lib/components/ConfirmDialog.svelte";
  import ConfirmRemove from "./lib/components/ConfirmRemove.svelte";
  import ContextMenu, { type MenuEntry } from "./lib/components/ContextMenu.svelte";
  import DetailPanel from "./lib/components/DetailPanel.svelte";
  import DownloadList from "./lib/components/DownloadList.svelte";
  import EmptyState from "./lib/components/EmptyState.svelte";
  import Modal from "./lib/components/Modal.svelte";
  import LoginDialog from "./lib/components/LoginDialog.svelte";
  import PropertiesDialog from "./lib/components/PropertiesDialog.svelte";
  import QueueDialog from "./lib/components/QueueDialog.svelte";
  import RefreshLinkDialog from "./lib/components/RefreshLinkDialog.svelte";
  import SettingsDialog from "./lib/components/SettingsDialog.svelte";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import Toasts from "./lib/components/Toasts.svelte";
  import Toolbar from "./lib/components/Toolbar.svelte";
  import DetailsList from "./lib/components/DetailsList.svelte";
  import ToolbarDialog from "./lib/components/ToolbarDialog.svelte";
  import { DEFAULT_TOOLBAR, isSortKey, shownColumns, sorter, type SortKey } from "./lib/columns";

  /** The sort kept by Siler before 0.8 (in the window's storage), moved to the settings once. */
  function oldSort(): SortKey | null {
    try {
      const v = localStorage.getItem("siler.sort");
      localStorage.removeItem("siler.sort");
      return isSortKey(v) ? v : null;
    } catch {
      return null;
    }
  }

  let filter = $state("all");
  let search = $state("");
  let sort = $state<SortKey>("added");
  let reverse = $state(false);
  let toolbarOpen = $state(false);
  const selected = new SvelteSet<number>();
  let anchor: number | null = null;
  let cursor: number | null = null;
  let list: { pageSize(): number; scrollToIndex(i: number): void } | undefined = $state();

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
  /** The queue being edited; null = a new one, undefined = closed. */
  let queueEdit = $state<Queue | null | undefined>(undefined);
  let moveItems = $state<Download[] | null>(null);
  let syncItems = $state<Download[] | null>(null);
  let whenDone = $state<AfterDone>("nothing");
  let patternOpen = $state(false);
  let grabberOpen = $state(false);
  /** Downloads to export; empty = all of them. */
  let exportItems = $state<Download[] | null>(null);
  let exportFormat = $state<ListFormat>("text");
  let exportCookies = $state(false);
  let verifyFor = $state<number | null>(null);

  // How the list looks lives in the settings (ui); changes are saved shortly after.
  const ui = $derived(settings?.ui ?? null);
  const view = $derived(ui?.view ?? "cards");
  const columns = $derived(shownColumns(ui?.columns));
  let uiLoaded = false;
  $effect(() => {
    if (!settings || uiLoaded) return;
    uiLoaded = true;
    const legacy = oldSort();
    sort = legacy ?? (isSortKey(settings.ui.sort) ? settings.ui.sort : "added");
    reverse = legacy ? false : settings.ui.sort_reverse;
  });
  let uiTimer: ReturnType<typeof setTimeout> | undefined;
  function saveUi(patch: Partial<NonNullable<typeof ui>>) {
    if (!settings) return;
    settings = { ...settings, ui: { ...settings.ui, ...patch } };
    clearTimeout(uiTimer);
    uiTimer = setTimeout(() => {
      if (settings) api.setSettings(settings).catch(toasts.error.bind(toasts));
    }, 400);
  }
  $effect(() => {
    const s = sort;
    const r = reverse;
    if (uiLoaded && settings && (settings.ui.sort !== s || settings.ui.sort_reverse !== r)) saveUi({ sort: s, sort_reverse: r });
  });
  function sortBy(key: SortKey) {
    if (sort === key) reverse = !reverse;
    else {
      sort = key;
      reverse = false;
    }
  }

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
    api.getWhenDone().then((a) => (whenDone = a));
    window.addEventListener("focus", reloadSoon);
    const askLogin = (e: Event) => (loginFor = (e as CustomEvent<number>).detail);
    window.addEventListener("siler-login", askLogin);
    const askLink = (e: Event) => (refreshFor = (e as CustomEvent<number>).detail);
    window.addEventListener("siler-link", askLink);
    const unlisten = [
      listen<number>("select-download", (e) => {
        filter = "all";
        search = "";
        select(e.payload);
      }),
      listen<number>("confirm-quit", (e) => (quitCount = e.payload)),
      listen<Settings>("settings-changed", (e) => (settings = e.payload)),
      listen<AfterDone>("when-done-changed", (e) => (whenDone = e.payload)),
      listen<number>("ask-login", async (e) => {
        // One prompt at a time; the other downloads keep their "Log in…" menu item.
        if (loginDownload) return;
        if (!store.items[e.payload]?.needs_login && !store.items[e.payload]?.needs_host_key) await store.reload();
        loginFor = e.payload;
      }),
      listen<number>("ask-link", (e) => (refreshFor = e.payload)),
      listen<{ id: number; error: string | null }>("refresh-done", (e) => {
        if (refreshFor === e.payload.id) return; // the dialog reports it
        if (e.payload.error) toasts.show(e.payload.error, "error");
        else toasts.show(t("main-link-updated-the-download-continues"), "success");
      }),
    ];
    return () => {
      window.removeEventListener("focus", reloadSoon);
      window.removeEventListener("siler-login", askLogin);
      window.removeEventListener("siler-link", askLink);
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
      case "sync":
        return d.sync_every > 0;
      default:
        if (f.startsWith("queue:")) return d.queue_id === Number(f.slice(6));
        return d.category === f.slice(4);
    }
  }

  const filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    return store.list
      .filter((d) => matches(d, filter) && (!q || (d.filename ?? "").toLowerCase().includes(q) || d.url.toLowerCase().includes(q)))
      .sort(sorter(sort, reverse));
  });
  const selection = $derived(filtered.filter((d) => selected.has(d.id)));
  const detail = $derived(selection.length === 1 ? selection[0] : null);
  const propsDownload = $derived(propsFor != null ? store.items[propsFor] : null);
  const refreshDownload = $derived(refreshFor != null ? store.items[refreshFor] : null);
  const loginDownload = $derived(
    loginFor != null && (store.items[loginFor]?.needs_login || store.items[loginFor]?.needs_host_key) ? store.items[loginFor] : null,
  );
  /** The queue the toolbar's Start/Stop queue acts on: the one shown, else the main one. */
  const queueCtx = $derived(filter.startsWith("queue:") ? Number(filter.slice(6)) : MAIN_QUEUE);
  const waitingCount = $derived(store.list.filter((d) => d.status === "waiting" && d.queue_id === queueCtx).length);
  const queuedCount = $derived(store.list.filter((d) => d.status === "queued" && d.queue_id === queueCtx).length);

  const title = $derived.by(() => {
    const labels: Record<string, string> = {
      all: t("main-all-downloads"),
      active: t("main-downloading"),
      queued: t("main-queued-paused"),
      completed: t("main-completed"),
      failed: t("main-failed"),
      sync: t("main-kept-up-to-date"),
    };
    if (filter.startsWith("queue:")) return store.queueName(queueCtx);
    return labels[filter] ?? CATEGORIES.find((c) => `cat:${c.key}` === filter)?.label ?? t("main-downloads");
  });
  const subtitle = $derived.by(() => {
    const active = filtered.filter((d) => isActive(d.status)).length;
    const n = filtered.length;
    const parts = [t("main-items", { count: n })];
    const q = filter.startsWith("queue:") ? store.queues.find((x) => x.id === queueCtx) : null;
    if (q?.active) parts.push(t("main-queue-running"));
    if (active) parts.push(t("main-downloading-at", { count: active, speed: speed(filtered.reduce((s, d) => s + d.speed, 0)) }));
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
      if (sort !== "queue") toasts.show(t("main-moved-in-the-queue-sort-by"));
    });

  /** Drags the file of `d`, or of every selected finished download when `d` is one of them. */
  function dragOut(d: Download) {
    const items = selected.has(d.id) ? selection.filter(draggable) : [d];
    startFileDrag(items.length ? items : [d]).catch((e) => toasts.error(e));
  }

  async function copyLinks(items: Download[]) {
    try {
      await navigator.clipboard.writeText(items.map((d) => d.url).join("\n"));
      toasts.show(t("main-links-copied", { count: items.length }), "success");
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
        label: t("main-open"),
        icon: ExternalLink,
        hint: "Enter",
        disabled: !(one && one.status === "completed" && !one.file_missing),
        run: () => one && attempt(api.openFile(one.id)),
      },
      {
        label: t("main-show-in-folder"),
        icon: FolderOpen,
        disabled: !(one && one.dir),
        run: () => one && attempt(api.showInFolder(one.id)),
      },
      "separator",
    ];
    if (sel.some((x) => canResume(x.status)))
      items.push({ label: one?.status === "waiting" ? t("main-start") : t("main-resume"), icon: Play, hint: "Space", run: () => resumeSel(sel) });
    if (sel.some((x) => canPause(x.status))) items.push({ label: t("main-pause"), icon: Pause, hint: "Space", run: () => pauseSel(sel) });
    items.push(
      {
        label: t("main-start-now"),
        icon: Zap,
        disabled: !sel.some((x) => x.status !== "completed" && !isActive(x.status)),
        run: () => sel.forEach((x) => x.status !== "completed" && attempt(api.startNow(x.id))),
      },
      { label: t("main-restart"), icon: RotateCcw, disabled: !unfinished.length, run: () => (restartItems = unfinished) },
      {
        label: t("main-download-again-2"),
        icon: RefreshCw,
        disabled: !(one && one.status === "completed"),
        run: () => one && (redownloadFor = one),
      },
      ...(one?.media?.live && !one.media.finished && one.status !== "completed" && one.status !== "failed"
        ? [{ label: t("main-finish-recording"), icon: CircleStop, run: () => attempt(api.finish(one.id)) } satisfies MenuEntry]
        : []),
      ...(one?.needs_host_key
        ? [{ label: t("main-check-server-key"), icon: KeyRound, run: () => (loginFor = one.id) } satisfies MenuEntry]
        : one?.needs_login
          ? [{ label: t("main-log-in"), icon: KeyRound, run: () => (loginFor = one.id) } satisfies MenuEntry]
          : []),
      {
        label: t("main-refresh-link"),
        icon: Link2,
        disabled: !(one && one.status !== "completed"),
        run: () => one && (refreshFor = one.id),
      },
      "separator",
      { label: sel.length > 1 ? t("main-copy-links") : t("main-copy-link"), icon: Copy, run: () => copyLinks(sel) },
      { label: t("main-properties"), icon: SlidersHorizontal, disabled: !one, run: () => one && (propsFor = one.id) },
      "separator",
      ...(store.queues.length > 1
        ? [{ label: t("main-move-to-queue"), icon: ListOrdered, disabled: !unfinished.length, run: () => (moveItems = unfinished) } satisfies MenuEntry]
        : []),
      {
        label: t("main-verify-checksum"),
        icon: ShieldCheck,
        disabled: !(one && one.status === "completed" && !one.file_missing),
        run: () => one && (verifyFor = one.id),
      },
      ...(one?.checksum_failed
        ? [{ label: t("main-keep-it-anyway"), icon: ShieldCheck, run: () => attempt(api.acceptUnverified(one.id)) } satisfies MenuEntry]
        : []),
      { label: sel.length > 1 ? t("main-export-n", { count: sel.length }) : t("main-export"), icon: FileOutput, run: () => (exportItems = sel) },
      {
        label: t("main-keep-up-to-date-2"),
        icon: RefreshCcwDot,
        disabled: !sel.some((x) => x.status === "completed"),
        run: () => (syncItems = sel.filter((x) => x.status === "completed")),
      },
      { label: t("main-move-to-top"), icon: ArrowUpToLine, disabled: !unfinished.length, run: () => move(sel, "top") },
      { label: t("main-move-up"), icon: ChevronUp, disabled: !unfinished.length, run: () => move(sel, "up") },
      { label: t("main-move-down"), icon: ChevronDown, disabled: !unfinished.length, run: () => move(sel, "down") },
      { label: t("main-move-to-bottom"), icon: ArrowDownToLine, disabled: !unfinished.length, run: () => move(sel, "bottom") },
      "separator",
      { label: t("main-remove"), icon: Trash2, hint: "Del", danger: true, run: () => (removeItems = sel) },
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
    if (!items.length) return toasts.show(what === "completed" ? t("main-no-completed-downloads") : t("main-no-failed-downloads"));
    if (what === "failed") return (removeFailed = items);
    const n = await attempt(api.removeMany(items.map((d) => d.id), false));
    if (n) toasts.show(t("main-cleared-completed", { count: n }), "success");
  }

  // ------------------------------------------------------------ tools

  function tool(which: "batch" | "grabber" | "import" | "export") {
    if (which === "batch") patternOpen = true;
    else if (which === "grabber") grabberOpen = true;
    else if (which === "export") exportItems = [];
    else importList();
  }

  async function importList() {
    const path = await openDialog({
      title: t("main-import-a-download-list"),
      filters: [
        { name: t("main-download-lists"), extensions: ["txt", "json", "meta4", "metalink", "ef2", "csv", "html", "htm"] },
        { name: t("main-all-files"), extensions: ["*"] },
      ],
    });
    if (typeof path !== "string") return;
    const n = await attempt(api.importList(path));
    if (n) toasts.show(t("main-to-review", { count: n }), "success");
  }

  async function exportList() {
    const items = exportItems ?? [];
    const json = exportFormat === "json";
    const path = await saveDialog({
      title: t("main-export-downloads"),
      defaultPath: json ? "siler-downloads.json" : "siler-downloads.txt",
      filters: [json ? { name: t("main-siler-list"), extensions: ["json"] } : { name: t("main-text"), extensions: ["txt"] }],
    });
    if (!path) return;
    const ok = await attempt(api.exportList(path, items.map((d) => d.id), exportFormat, json && exportCookies));
    if (ok !== undefined) {
      toasts.show(t("main-exported", { count: items.length || store.list.length }), "success");
      exportItems = null;
      exportCookies = false;
    }
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
      queueEdit !== undefined ||
      !!moveItems ||
      !!syncItems ||
      patternOpen ||
      grabberOpen ||
      !!exportItems ||
      verifyFor != null ||
      quitCount != null,
  );

  const inField = (target: EventTarget | null) => target instanceof Element && !!target.closest("input, textarea, select");

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
    } else if (mod && e.key.toLowerCase() === "b") {
      e.preventDefault();
      patternOpen = true;
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
    else toasts.show(t("main-drop-a-link-to-a-file"));
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
    onqueue={(q) => (queueEdit = q)}
  />

  <main>
    <Toolbar
      bind:search
      bind:sort
      bind:reverse
      items={ui?.toolbar ?? DEFAULT_TOOLBAR}
      {view}
      onview={(v) => saveUi({ view: v })}
      onsettings={() => (settingsOpen = true)}
      oncustomize={() => (toolbarOpen = true)}
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
      onstartqueue={() => attempt(api.startQueue(queueCtx))}
      onstopqueue={() => attempt(api.stopQueue(queueCtx))}
      {whenDone}
      onwhendone={(a) => attempt(api.setWhenDone(a))}
      oncleanup={cleanup}
      ontool={tool}
    />
    <UpdateBanner {settings} />
    <QuotaBanner />
    <div class="content">
      <div class="list">
        {#if !store.loaded}
          <div></div>
        {:else if filtered.length}
          {#if view === "details"}
            <DetailsList
              bind:this={list}
              items={filtered}
              {selected}
              {columns}
              {sort}
              {reverse}
              onsort={sortBy}
              oncolumns={(cols) => saveUi({ columns: cols })}
              onrowclick={rowclick}
              onrowopen={rowopen}
              onrowmenu={openMenu}
              ondragout={dragOut}
            />
          {:else}
            <DownloadList bind:this={list} items={filtered} {selected} onrowclick={rowclick} onrowopen={rowopen} onrowmenu={openMenu} ondragout={dragOut} />
          {/if}
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
      <div class="drop-card">{t("main-drop-links-to-download-them")}</div>
    </div>
  {/if}
</div>

{#if toolbarOpen && settings}
  <ToolbarDialog items={settings.ui.toolbar} onsave={(items) => saveUi({ toolbar: items })} onclose={() => (toolbarOpen = false)} />
{/if}

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

{#if patternOpen}
  <BatchPatternDialog
    onclose={() => (patternOpen = false)}
    oncontinue={(urls) => {
      patternOpen = false;
      openUrls(urls);
    }}
  />
{/if}

{#if grabberOpen}
  <GrabberDialog onclose={() => (grabberOpen = false)} />
{/if}

{#if verifyFor != null && store.items[verifyFor]}
  <VerifyDialog d={store.items[verifyFor]} onclose={() => (verifyFor = null)} />
{/if}

{#if exportItems}
  {@const n = exportItems.length || store.list.length}
  <Modal title={exportItems.length ? t("main-export-title", { count: n }) : t("main-export-all-downloads")} width={460} onclose={() => (exportItems = null)}>
    <div class="pick">
      <button class="pick-row" class:on={exportFormat === "text"} onclick={() => (exportFormat = "text")}>
        <FileOutput size={15} />
        <span class="grow"><span class="pick-name">{t("main-a-list-of-links")}</span><span class="hint">{t("main-one-address-per-line-any-download")}</span></span>
      </button>
      <button class="pick-row" class:on={exportFormat === "json"} onclick={() => (exportFormat = "json")}>
        <FileOutput size={15} />
        <span class="grow"><span class="pick-name">{t("main-siler-list")}</span><span class="hint">{t("main-also-names-folders-queues-checksums-and")}</span></span>
      </button>
    </div>
    {#if exportFormat === "json"}
      <button class="toggle export-cookies" onclick={() => (exportCookies = !exportCookies)}>
        <span class="switch" class:on={exportCookies}></span>
        {t("main-include-cookies")} <span class="hint">{t("main-they-can-log-someone-else-in")}</span>
      </button>
    {/if}
    {#snippet footer()}
      <span class="grow"></span>
      <button class="btn btn-ghost" onclick={() => (exportItems = null)}>{t("main-cancel")}</button>
      <button class="btn btn-primary" onclick={exportList}>{t("main-export")}</button>
    {/snippet}
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
    title={t("main-restart-title", { count: restartItems.length })}
    confirm={t("main-restart-2")}
    danger
    onclose={() => (restartItems = null)}
    onconfirm={async () => {
      for (const d of restartItems ?? []) await attempt(api.restart(d.id));
    }}
  >
    <p>{t("main-what-was-downloaded-so-far-is", { count: restartItems.length })}</p>
  </ConfirmDialog>
{/if}

{#if redownloadFor}
  {@const d = redownloadFor}
  <ConfirmDialog
    title={t("main-download-again", { v: d.filename ?? nameFromUrl(d.url) })}
    confirm={t("main-download-again-3")}
    onclose={() => {
      redownloadFor = null;
      replaceOld = false;
    }}
    onconfirm={() => attempt(api.redownload(d.id, replaceOld && !d.file_missing))}
  >
    {#if d.file_missing}
      <p>{(d.dir ? t("main-the-file-was-moved-or-deleted", { dir: d.dir }) : t("main-the-file-was-moved-or-deleted-its-folder"))}</p>
    {:else}
      <p>{(d.url.length > 60 ? t("main-the-file-is-downloaded-again-from-its-address") : t("main-the-file-is-downloaded-again-from", { url: d.url }))}</p>
      <button class="toggle" onclick={() => (replaceOld = !replaceOld)}>
        <span class="switch" class:on={replaceOld}></span>
        {t("main-replace-the-existing-file-when-done")}
      </button>
    {/if}
  </ConfirmDialog>
{/if}

{#if removeFailed}
  <ConfirmDialog
    title={t("main-remove-failed", { count: removeFailed.length })}
    confirm={t("main-remove-2")}
    danger
    onclose={() => (removeFailed = null)}
    onconfirm={() => attempt(api.removeMany((removeFailed ?? []).map((d) => d.id), false))}
  >
    <p>{t("main-they-are-removed-from-the-list")}</p>
  </ConfirmDialog>
{/if}

{#if quitCount != null}
  <ConfirmDialog
    title={t("main-quit-siler")}
    confirm={t("main-quit")}
    onclose={() => {
      quitCount = null;
      dontAskQuit = false;
    }}
    onconfirm={() => api.quit(dontAskQuit)}
  >
    <p>
      {(settings?.resume_on_startup === false ? t("main-still-running-resume-by-hand", { count: quitCount }) : t("main-still-running-now-and-the-next", { count: quitCount }))}
    </p>
    <button class="toggle" onclick={() => (dontAskQuit = !dontAskQuit)}>
      <span class="switch" class:on={dontAskQuit}></span>
      {t("main-don-t-ask-again")}
    </button>
  </ConfirmDialog>
{/if}

{#if queueEdit !== undefined}
  <QueueDialog queue={queueEdit} onclose={() => (queueEdit = undefined)} />
{/if}

{#if moveItems}
  {@const items = moveItems}
  <Modal title={t("main-move-to-queue-title", { count: items.length })} width={400} onclose={() => (moveItems = null)}>
    <div class="pick">
      {#each store.queues as q (q.id)}
        <button
          class="pick-row"
          class:on={items.every((d) => d.queue_id === q.id)}
          onclick={async () => {
            await attempt(api.moveToQueue(items.map((d) => d.id), q.id));
            moveItems = null;
          }}
        >
          <ListOrdered size={16} />
          <span class="grow">
            <span class="pick-name">{q.name}</span>
            {#if describeSchedule(q.schedule)}<span class="hint">{describeSchedule(q.schedule)}</span>{/if}
          </span>
        </button>
      {/each}
    </div>
  </Modal>
{/if}

{#if syncItems}
  {@const items = syncItems}
  {@const current = items.every((d) => d.sync_every === items[0].sync_every) ? items[0].sync_every : -1}
  <Modal title={t("main-keep-up-to-date")} width={440} onclose={() => (syncItems = null)}>
    <p class="sync-intro">
      {t("main-siler-asks-the-server-whether-changed", { count: items.length })}
    </p>
    <div class="pick">
      {#each [{ secs: 0, label: t("main-don-t-check") }, { secs: 3600, label: t("main-check-every-hour") }, { secs: 86400, label: t("main-check-every-day") }, { secs: 604800, label: t("main-check-every-week") }] as o (o.secs)}
        <button
          class="pick-row"
          class:on={current === o.secs}
          onclick={async () => {
            for (const d of items) await attempt(api.setSync(d.id, o.secs));
            syncItems = null;
            if (o.secs) toasts.show(t("main-kept-up-to-date-every", { every: syncLabel(o.secs) }), "success");
          }}
        >
          <RefreshCw size={15} />
          <span class="grow pick-name">{o.label}</span>
        </button>
      {/each}
    </div>
    {#snippet footer()}
      {#if items.length === 1 && items[0].sync_checked_at && items[0].sync_every}
        <span class="hint grow">{t("main-last-checked", { v: relativeTime(items[0].sync_checked_at) })}</span>
      {:else}
        <span class="grow"></span>
      {/if}
      <button
        class="btn"
        onclick={async () => {
          for (const d of items) await attempt(api.checkForUpdate(d.id));
          syncItems = null;
          toasts.show(t("main-checking-for-newer", { count: items.length }));
        }}>{t("main-check-now")}</button
      >
    {/snippet}
  </Modal>
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
  .export-cookies {
    margin-top: 12px;
    text-align: left;
    font-size: 12.5px;
  }
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
  .pick {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .pick-row {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 40px;
    padding: 6px 12px;
    border-radius: var(--r-sm);
    text-align: left;
    color: var(--text-2);
  }
  .pick-row:hover {
    background: var(--hover);
    color: var(--text);
  }
  .pick-row.on {
    background: var(--selected);
    color: var(--accent-text);
  }
  .pick-row .grow {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .pick-name {
    font-weight: 600;
  }
  .sync-intro {
    margin: 0 0 12px;
    color: var(--text-2);
  }
  .grow {
    flex: 1;
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
