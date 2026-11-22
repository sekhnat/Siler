<script lang="ts">
  import { t } from "../i18n";
  import { CalendarClock, FolderOpen, Globe, LoaderCircle, Plus, Save, Square, Trash2, TriangleAlert } from "@lucide/svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";
  import { api, MAIN_QUEUE, type Category, type GrabConfig, type GrabProgress, type GrabProject, type GrabScope } from "../api";
  import { CATEGORIES } from "../categories";
  import { bytes, relativeTime } from "../format";
  import { attempt, store, toasts } from "../store.svelte";
  import Modal from "./Modal.svelte";
  import QueuePicker from "./QueuePicker.svelte";

  let { onclose }: { onclose: () => void } = $props();

  const DAYS = [t("grabber-mo"), t("grabber-tu"), t("grabber-we"), t("grabber-th"), t("grabber-fr"), t("grabber-sa"), t("grabber-su")];
  const FILE_CATEGORIES = CATEGORIES.filter((c) => c.key !== "other");

  function freshConfig(): GrabConfig {
    return {
      start_url: "",
      depth: 2,
      scope: "same_host",
      max_pages: 500,
      concurrency: 4,
      robots: true,
      save_pages: false,
      filter: { categories: ["compressed", "programs", "documents", "video", "music"], extensions: [], min_size: null, max_size: null, any_host: true },
      dir: null,
      queue_id: null,
      only_new: false,
      schedule: { enabled: false, start_at: 3 * 60, stop_at: null, days: [true, true, true, true, true, true, true], once: false },
    };
  }

  let projects = $state<GrabProject[]>([]);
  /** The project being edited; id 0 = not saved. */
  let current = $state<GrabProject>({ id: 0, name: "", config: freshConfig(), last_run: null });
  let extText = $state("");
  let minMb = $state("");
  let maxMb = $state("");
  let runId = $state<number | null>(null);
  let queueId = $state(MAIN_QUEUE);
  let saving = $state(false);

  const progress = $derived<GrabProgress | null>(runId != null ? (store.grabs[runId] ?? null) : null);
  const running = $derived(runId != null && !progress?.finished);
  const c = $derived(current.config);
  const validUrl = $derived(/^https?:\/\/\S+$/i.test(c.start_url.trim()));

  onMount(async () => {
    projects = (await attempt(api.grabProjects())) ?? [];
  });

  function load(p: GrabProject) {
    if (running) return;
    current = structuredClone($state.snapshot(p)) as GrabProject;
    extText = current.config.filter.extensions.join(", ");
    minMb = current.config.filter.min_size ? String(current.config.filter.min_size / 1e6) : "";
    maxMb = current.config.filter.max_size ? String(current.config.filter.max_size / 1e6) : "";
    queueId = current.config.queue_id ?? MAIN_QUEUE;
    runId = null;
  }

  function newProject() {
    load({ id: 0, name: "", config: freshConfig(), last_run: null });
  }

  /** The form's values in the project. */
  function collect(): GrabProject {
    const p = $state.snapshot(current) as GrabProject;
    p.config.start_url = p.config.start_url.trim();
    p.config.filter.extensions = extText
      .split(/[\s,]+/)
      .map((e) => e.trim().replace(/^\./, ""))
      .filter(Boolean);
    const mb = (v: string) => (v.trim() && +v > 0 ? Math.round(+v * 1e6) : null);
    p.config.filter.min_size = mb(minMb);
    p.config.filter.max_size = mb(maxMb);
    p.config.dir = p.config.dir?.trim() || null;
    p.config.queue_id = queueId;
    if (!p.name.trim()) {
      try {
        p.name = new URL(p.config.start_url).host;
      } catch {
        p.name = t("grabber-site");
      }
    }
    return p;
  }

  async function save() {
    if (!validUrl) return;
    saving = true;
    const p = collect();
    const id = await attempt(api.saveGrabProject(p));
    saving = false;
    if (id == null) return;
    current.id = id;
    current.name = p.name;
    projects = (await attempt(api.grabProjects())) ?? projects;
    toasts.show(t("grabber-project-saved"), "success");
  }

  async function remove() {
    if (!current.id) return;
    await attempt(api.deleteGrabProject(current.id));
    projects = projects.filter((p) => p.id !== current.id);
    newProject();
  }

  async function run() {
    if (!validUrl || running) return;
    const p = collect();
    runId = (await attempt(api.grabStart(p.config, p.id || null))) ?? null;
  }

  async function review() {
    const r = progress?.result;
    if (!r?.files.length) return;
    const n = await attempt(api.reviewGrab(r.files, queueId));
    if (n) onclose();
  }

  async function browse() {
    const got = await open({ directory: true, defaultPath: c.dir ?? undefined, title: c.save_pages ? t("grabber-save-the-site-to") : t("grabber-save-the-files-to") });
    if (typeof got === "string") current.config.dir = got;
  }

  function toggleCategory(k: Category) {
    const cats = current.config.filter.categories;
    current.config.filter.categories = cats.includes(k) ? cats.filter((x) => x !== k) : [...cats, k];
  }

  const hm = (m: number | null) => (m == null ? "" : `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`);
  const minutes = (v: string) => {
    const [h, m] = v.split(":").map(Number);
    return Number.isFinite(h) ? h * 60 + (m || 0) : null;
  };

  const SCOPES: { value: GrabScope; label: string }[] = [
    { value: "same_host", label: t("grabber-this-server") },
    { value: "same_site", label: t("grabber-the-whole-site-its-other-servers") },
    { value: "folder", label: t("grabber-this-folder-and-below") },
  ];
</script>

<Modal title={t("grabber-site-grabber")} width={860} onclose={() => !running && onclose()}>
  <div class="layout">
    <aside>
      <button class="proj" class:on={current.id === 0} onclick={newProject} disabled={running}><Plus size={14} /> {t("grabber-new-grab")}</button>
      {#if projects.length}<div class="label side-label">{t("grabber-saved-projects")}</div>{/if}
      {#each projects as p (p.id)}
        <button class="proj" class:on={current.id === p.id} onclick={() => load(p)} disabled={running}>
          <Globe size={14} />
          <span class="grow min">
            <span class="truncate pname">{p.name}</span>
            <span class="hint truncate">{p.config.schedule.enabled ? t("grabber-scheduled") : ""}{p.last_run ? t("grabber-ran", { when: relativeTime(p.last_run) }) : t("grabber-not-run-yet")}</span>
          </span>
        </button>
      {/each}
    </aside>

    <div class="form">
      <div class="grid2">
        <label class="field wide">
          <span class="label">{t("grabber-start-page")}</span>
          <input class="input mono" bind:value={current.config.start_url} placeholder="https://example.com/downloads/" spellcheck="false" disabled={running} />
        </label>
        <label class="field">
          <span class="label">{t("grabber-project-name")}</span>
          <input class="input" bind:value={current.name} placeholder={t("grabber-named-after-the-site")} disabled={running} />
        </label>
      </div>

      <div class="grid3">
        <label class="field">
          <span class="label">{t("grabber-follow-links")}</span>
          <select class="input" bind:value={current.config.depth} disabled={running}>
            <option value={0}>{t("grabber-start-page-only")}</option>
            {#each [1, 2, 3, 4, 5, 8] as n (n)}<option value={n}>{t("grabber-deep", { n })}</option>{/each}
          </select>
        </label>
        <label class="field">
          <span class="label">{t("grabber-stay-within")}</span>
          <select class="input" bind:value={current.config.scope} disabled={running}>
            {#each SCOPES as s (s.value)}<option value={s.value}>{s.label}</option>{/each}
          </select>
        </label>
        <label class="field">
          <span class="label">{t("grabber-at-most")}</span>
          <select class="input" bind:value={current.config.max_pages} disabled={running}>
            {#each [50, 200, 500, 2000, 10000] as n (n)}<option value={n}>{t("grabber-pages", { n })}</option>{/each}
          </select>
        </label>
      </div>

      <div class="field">
        <span class="label">{t("grabber-files-to-find")}</span>
        <div class="chips">
          {#each FILE_CATEGORIES as cat (cat.key)}
            <button class="chip" class:on={c.filter.categories.includes(cat.key)} style:--c={cat.color} onclick={() => toggleCategory(cat.key)} disabled={running}>
              <cat.icon size={12} /> {cat.label}
            </button>
          {/each}
        </div>
        <div class="grid3 tight">
          <input class="input" bind:value={extText} placeholder={t("grabber-also-these-extensions-epub-srt")} spellcheck="false" disabled={running} />
          <input class="input" bind:value={minMb} placeholder={t("grabber-at-least-mb")} inputmode="decimal" disabled={running} />
          <input class="input" bind:value={maxMb} placeholder={t("grabber-at-most-mb")} inputmode="decimal" disabled={running} />
        </div>
      </div>

      <div class="toggles">
        <button class="toggle" onclick={() => (current.config.save_pages = !c.save_pages)} disabled={running}>
          <span class="switch" class:on={c.save_pages}></span>
          <span><b>{t("grabber-save-the-pages-for-offline-browsing")}</b><span class="hint"> {t("grabber-with-their-pictures-and-styles-links")}</span></span>
        </button>
        <button class="toggle" onclick={() => (current.config.filter.any_host = !c.filter.any_host)} disabled={running}>
          <span class="switch" class:on={c.filter.any_host}></span>
          <span>{t("grabber-take-files-from-other-servers-too")} <span class="hint">{t("grabber-sites-often-keep-files-elsewhere")}</span></span>
        </button>
        <button class="toggle" onclick={() => (current.config.robots = !c.robots)} disabled={running}>
          <span class="switch" class:on={c.robots}></span>
          <span>{t("grabber-leave-out-what-the-site-s")}</span>
        </button>
        <button class="toggle" onclick={() => (current.config.only_new = !c.only_new)} disabled={running || !current.id}>
          <span class="switch" class:on={c.only_new}></span>
          <span>{t("grabber-only-files-this-project-hasn-t")} {#if !current.id}<span class="hint">{t("grabber-save-the-project-first")}</span>{/if}</span>
        </button>
      </div>

      <div class="grid2">
        <div class="field">
          <span class="label">{t("grabber-save-to")}</span>
          <div class="folder">
            <input class="input" value={c.dir ?? ""} oninput={(e) => (current.config.dir = e.currentTarget.value || null)} placeholder={c.save_pages ? "Downloads/Sites" : t("grabber-the-category-folders")} spellcheck="false" disabled={running} />
            <button class="btn" onclick={browse} title={t("grabber-choose-folder")} disabled={running}><FolderOpen size={15} /></button>
          </div>
        </div>
        {#if store.queues.length > 1}
          <div class="field">
            <span class="label">{t("grabber-queue")}</span>
            <QueuePicker bind:value={queueId} />
          </div>
        {/if}
      </div>

      {#if current.id}
        <div class="sched">
          <button class="toggle" onclick={() => (current.config.schedule.enabled = !c.schedule.enabled)} disabled={running}>
            <span class="switch" class:on={c.schedule.enabled}></span>
            <CalendarClock size={14} /> {t("grabber-run-on-a-schedule")} <span class="hint">{t("grabber-found-files-go-straight-to-the")}</span>
          </button>
          {#if c.schedule.enabled}
            <div class="sched-row">
              <span class="hint">{t("grabber-at")}</span>
              <input class="input time" type="time" value={hm(c.schedule.start_at)} onchange={(e) => (current.config.schedule.start_at = minutes(e.currentTarget.value))} />
              <div class="days">
                {#each DAYS as d, i (d)}
                  <button class:on={c.schedule.days[i]} onclick={() => (current.config.schedule.days[i] = !c.schedule.days[i])}>{d}</button>
                {/each}
              </div>
            </div>
          {/if}
        </div>
      {/if}

      {#if progress || running}
        <div class="status">
          {#if progress?.error}
            <div class="err"><TriangleAlert size={15} /> {progress.error}</div>
          {:else if progress?.finished && progress.result}
            {@const r = progress.result}
            <div class="done">
              <b>{t("grabber-found", { count: r.files.length })}</b> {t("grabber-on", { pages: r.pages })}{r.saved ? t("grabber-saved-offline", { n: r.saved }) : ""}{r.blocked ? t("grabber-left-out-robots", { n: r.blocked }) : ""}{r.errors ? t("grabber-couldnt-be-read", { n: r.errors }) : ""}
            </div>
          {:else}
            <div class="live">
              <LoaderCircle size={15} class="spin" />
              <span>{t("grabber-pages-read", { n: progress?.pages_done ?? 0 })}{progress?.pages_queued ? t("grabber-to-go", { n: progress.pages_queued }) : ""}{t("grabber-files", { n: progress?.files_found ?? 0 })}{progress?.saved ? t("grabber-saved", { n: progress.saved }) : ""}{" · "}{bytes(progress?.bytes ?? 0)}</span>
            </div>
            {#if progress?.current}<div class="hint mono truncate">{progress.current}</div>{/if}
          {/if}
        </div>
      {/if}
    </div>
  </div>

  {#snippet footer()}
    {#if current.id}
      <button class="btn btn-ghost danger-text" onclick={remove} disabled={running}><Trash2 size={14} /> {t("grabber-delete")}</button>
    {/if}
    <button class="btn btn-ghost" onclick={save} disabled={!validUrl || saving || running}><Save size={14} /> {current.id ? t("grabber-save") : t("grabber-save-as-project")}</button>
    <span class="grow"></span>
    {#if running}
      <button class="btn" onclick={() => runId != null && api.grabCancel(runId)}><Square size={13} /> {t("grabber-stop")}</button>
    {:else if progress?.finished && progress.result}
      {#if progress.result.site_index}
        <button class="btn" onclick={() => attempt(api.openSavedSite(progress.result!.site_index!))}><Globe size={14} /> {t("grabber-open-saved-site")}</button>
      {/if}
      <button class="btn" onclick={run} disabled={!validUrl}>{t("grabber-run-again")}</button>
      <button class="btn btn-primary" onclick={review} disabled={!progress.result.files.length}>{t("grabber-review-files", { length: progress.result.files.length })}</button>
    {:else}
      <button class="btn btn-ghost" onclick={onclose}>{t("grabber-close")}</button>
      <button class="btn btn-primary" onclick={run} disabled={!validUrl}>{t("grabber-start")}</button>
    {/if}
  {/snippet}
</Modal>

<style>
  .layout {
    display: grid;
    grid-template-columns: 200px 1fr;
    gap: 18px;
    min-height: 0;
  }
  aside {
    display: flex;
    flex-direction: column;
    gap: 3px;
    border-right: 1px solid var(--border);
    padding-right: 14px;
    max-height: 560px;
    overflow-y: auto;
  }
  .side-label {
    margin: 10px 0 2px 6px;
  }
  .proj {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-radius: var(--r);
    text-align: left;
    font-weight: 600;
    font-size: 12.5px;
    color: var(--text-2);
  }
  .proj:hover:not(:disabled) {
    background: var(--hover);
  }
  .proj.on {
    background: var(--selected);
    color: var(--text);
  }
  .proj :global(svg) {
    flex: none;
  }
  .min {
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .pname {
    display: block;
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-width: 0;
    max-height: 560px;
    overflow-y: auto;
    padding-right: 2px;
  }
  .field {
    display: block;
    min-width: 0;
  }
  .grid2 {
    display: grid;
    grid-template-columns: 1.6fr 1fr;
    gap: 12px;
  }
  .grid3 {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 12px;
  }
  .grid3.tight {
    margin-top: 8px;
    grid-template-columns: 1.6fr 1fr 1fr;
    gap: 8px;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 28px;
    padding: 0 10px;
    border-radius: 99px;
    border: 1px solid var(--border);
    font-size: 12px;
    font-weight: 600;
    color: var(--text-2);
  }
  .chip.on {
    border-color: color-mix(in srgb, var(--c) 50%, transparent);
    background: color-mix(in srgb, var(--c) 14%, transparent);
    color: var(--text);
  }
  .toggles {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .toggle {
    display: flex;
    align-items: center;
    gap: 10px;
    text-align: left;
    font-size: 12.5px;
  }
  .toggle:disabled {
    opacity: 0.6;
  }
  .folder {
    display: flex;
    gap: 6px;
  }
  .folder .btn {
    padding: 0 10px;
    height: 36px;
  }
  .sched {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    border-radius: var(--r-lg);
    background: var(--surface-2);
  }
  .sched-row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .time {
    width: 110px;
  }
  .days {
    display: flex;
    gap: 3px;
  }
  .days button {
    width: 32px;
    height: 28px;
    border-radius: var(--r-sm);
    font-size: 11.5px;
    font-weight: 600;
    color: var(--text-2);
    background: var(--surface-3);
  }
  .days button.on {
    background: var(--accent);
    color: var(--accent-text);
  }
  .status {
    padding: 12px 14px;
    border-radius: var(--r-lg);
    background: var(--grad-soft);
    border: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .live {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 600;
  }
  .live :global(.spin) {
    animation: spin 0.9s linear infinite;
  }
  .err {
    display: flex;
    gap: 7px;
    align-items: center;
    color: var(--err);
    font-weight: 600;
  }
  .danger-text {
    color: var(--err);
  }
  .grow {
    flex: 1;
  }
</style>
