<script lang="ts">
  import { FolderOpen, Gauge, Minus, Plus } from "@lucide/svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, type Download } from "../api";
  import { STATUS_LABEL, categoryMeta } from "../categories";
  import { bytes, dateTime, limitLabel, nameFromUrl } from "../format";
  import { attempt, toasts } from "../store.svelte";
  import FileIcon from "./FileIcon.svelte";
  import Modal from "./Modal.svelte";
  import SpeedMenu from "./SpeedMenu.svelte";

  let { d, onclose, onrefresh }: { d: Download; onclose: () => void; onrefresh: () => void } = $props();

  // The form starts from the download as it is now.
  // svelte-ignore state_referenced_locally
  let filename = $state(d.filename ?? "");
  // svelte-ignore state_referenced_locally
  let dir = $state(d.dir ?? "");
  let limitMenu = $state(false);
  let saving = $state(false);

  const done = $derived(d.status === "completed");
  const changed = $derived((filename.trim() && filename.trim() !== d.filename) || (dir.trim() && dir.trim() !== d.dir));

  async function browse() {
    const picked = await open({ directory: true, defaultPath: dir || undefined, title: "Move to folder" });
    if (typeof picked === "string") dir = picked;
  }

  function setConnections(n: number) {
    attempt(api.setConnections(d.id, Math.max(1, Math.min(32, n))));
  }

  async function save() {
    if (!changed) return onclose();
    saving = true;
    try {
      const name = filename.trim() && filename.trim() !== d.filename ? filename.trim() : null;
      const folder = dir.trim() && dir.trim() !== d.dir ? dir.trim() : null;
      await api.edit(d.id, name, folder);
      toasts.show(done ? "File moved" : "Download updated", "success");
      onclose();
    } catch (e) {
      toasts.error(e);
    } finally {
      saving = false;
    }
  }
</script>

<Modal title="Properties" width={560} {onclose}>
  <div class="head">
    <FileIcon category={d.category} size={42} />
    <div class="min">
      <div class="name truncate">{d.filename ?? nameFromUrl(d.url)}</div>
      <div class="hint">{categoryMeta(d.category).label} · {STATUS_LABEL[d.status]} · {bytes(d.total_size)}</div>
    </div>
  </div>

  <div class="form">
    <label class="field">
      <span class="label">File name</span>
      <input class="input" bind:value={filename} spellcheck="false" placeholder="Decided when the download starts" />
    </label>
    <div class="field">
      <span class="label">Folder</span>
      <div class="row">
        <input class="input" bind:value={dir} spellcheck="false" placeholder="Category folder" />
        <button class="btn" onclick={browse} title="Choose folder"><FolderOpen size={15} /></button>
      </div>
      {#if changed && (done || d.downloaded > 0)}
        <div class="hint">{done ? "The file" : "The partial file"} will be moved{d.status === "downloading" ? "; the download pauses for a moment" : ""}.</div>
      {/if}
    </div>

    <div class="field">
      <span class="label">Address</span>
      <div class="row">
        <input class="input mono selectable" value={d.url} readonly />
        {#if !done}<button class="btn" onclick={onrefresh}>Refresh link…</button>{/if}
      </div>
    </div>

    {#if !done}
      <div class="grid">
        <div class="field">
          <span class="label">Connections</span>
          <div class="stepper">
            <button class="icon-btn sm" onclick={() => setConnections(d.connections - 1)} disabled={d.connections <= 1} aria-label="Fewer"><Minus size={14} /></button>
            <span class="num">{d.connections}</span>
            <button class="icon-btn sm" onclick={() => setConnections(d.connections + 1)} disabled={d.connections >= 32} aria-label="More"><Plus size={14} /></button>
            <span class="hint">{d.status === "downloading" ? "applies right away" : ""}</span>
          </div>
        </div>
        <div class="field">
          <span class="label">Speed limit</span>
          <div class="limit-wrap">
            <button class="btn" onclick={() => (limitMenu = !limitMenu)}><Gauge size={14} /> {limitLabel(d.speed_limit)}</button>
            {#if limitMenu}
              <SpeedMenu value={d.speed_limit} onselect={(v) => attempt(api.setSpeedLimit(d.id, v))} onclose={() => (limitMenu = false)} />
            {/if}
          </div>
        </div>
      </div>
    {/if}

    <dl>
      {#if d.referrer}<dt>Referrer</dt><dd class="mono selectable truncate" title={d.referrer}>{d.referrer}</dd>{/if}
      {#if d.method && d.method !== "GET"}<dt>Request</dt><dd>{d.method} form submission</dd>{/if}
      <dt>Added</dt><dd>{dateTime(d.created_at)}</dd>
      {#if d.completed_at}<dt>Finished</dt><dd>{dateTime(d.completed_at)}</dd>{/if}
    </dl>
  </div>

  {#snippet footer()}
    <button class="btn btn-ghost" onclick={onclose}>Close</button>
    <button class="btn btn-primary" onclick={save} disabled={!changed || saving}>Save</button>
  {/snippet}
</Modal>

<style>
  .head {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 18px;
  }
  .min {
    min-width: 0;
  }
  .name {
    font-weight: 650;
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .field {
    display: block;
    min-width: 0;
  }
  .row {
    display: flex;
    gap: 6px;
  }
  .row .btn {
    flex: none;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .stepper {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 36px;
  }
  .stepper .num {
    min-width: 22px;
    text-align: center;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }
  .limit-wrap {
    position: relative;
  }
  dl {
    display: grid;
    grid-template-columns: 86px minmax(0, 1fr);
    gap: 8px 10px;
    margin: 4px 0 0;
    font-size: 12.5px;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    min-width: 0;
  }
</style>
