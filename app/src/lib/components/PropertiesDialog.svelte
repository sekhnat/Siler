<script lang="ts">
  import { t } from "../i18n";
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
  // svelte-ignore state_referenced_locally
  let checksum = $state(d.checksum?.hex ?? "");
  // svelte-ignore state_referenced_locally
  let mirrors = $state(d.sources.slice(1).map((s) => s.url).join("\n"));
  let limitMenu = $state(false);
  let saving = $state(false);

  const done = $derived(d.status === "completed");
  const fileChanged = $derived((filename.trim() && filename.trim() !== d.filename) || (dir.trim() && dir.trim() !== d.dir));
  const checksumChanged = $derived(checksum.trim().toLowerCase() !== (d.checksum?.hex ?? ""));
  const mirrorList = $derived(mirrors.split(/\s+/).filter(Boolean));
  const mirrorsChanged = $derived(mirrorList.join("\n") !== d.sources.slice(1).map((s) => s.url).join("\n"));
  /** Checksums and mirrors apply to ordinary files only. */
  const plainFile = $derived(!d.media && !d.zip_path);
  const changed = $derived(fileChanged || (plainFile && (checksumChanged || (!done && mirrorsChanged))));

  async function browse() {
    const picked = await open({ directory: true, defaultPath: dir || undefined, title: t("properties-move-to-folder") });
    if (typeof picked === "string") dir = picked;
  }

  function setConnections(n: number) {
    attempt(api.setConnections(d.id, Math.max(1, Math.min(32, n))));
  }

  async function save() {
    if (!changed) return onclose();
    saving = true;
    try {
      if (plainFile && checksumChanged) await api.setChecksum(d.id, checksum.trim() || null);
      if (plainFile && !done && mirrorsChanged) await api.setMirrors(d.id, mirrorList);
      if (fileChanged) {
        const name = filename.trim() && filename.trim() !== d.filename ? filename.trim() : null;
        const folder = dir.trim() && dir.trim() !== d.dir ? dir.trim() : null;
        await api.edit(d.id, name, folder);
      }
      toasts.show(done ? t("properties-file-moved") : t("properties-download-updated"), "success");
      onclose();
    } catch (e) {
      toasts.error(e);
    } finally {
      saving = false;
    }
  }
</script>

<Modal title={t("properties-properties")} width={560} {onclose}>
  <div class="head">
    <FileIcon category={d.category} size={42} />
    <div class="min">
      <div class="name truncate">{d.filename ?? nameFromUrl(d.url)}</div>
      <div class="hint">{categoryMeta(d.category).label} · {STATUS_LABEL[d.status]} · {bytes(d.total_size)}</div>
    </div>
  </div>

  <div class="form">
    <label class="field">
      <span class="label">{t("properties-file-name")}</span>
      <input class="input" bind:value={filename} spellcheck="false" placeholder={t("properties-decided-when-the-download-starts")} />
    </label>
    <div class="field">
      <span class="label">{t("properties-folder")}</span>
      <div class="row">
        <input class="input" bind:value={dir} spellcheck="false" placeholder={t("properties-category-folder")} />
        <button class="btn" onclick={browse} title={t("properties-choose-folder")}><FolderOpen size={15} /></button>
      </div>
      {#if changed && (done || d.downloaded > 0)}
        <div class="hint">{(done ? t("properties-the-file-will-be-moved") : d.status === "downloading" ? t("properties-the-partial-file-will-be-moved-pause") : t("properties-the-partial-file-will-be-moved"))}</div>
      {/if}
    </div>

    <div class="field">
      <span class="label">{t("properties-address")}</span>
      <div class="row">
        <input class="input mono selectable" value={d.url} readonly />
        {#if !done}<button class="btn" onclick={onrefresh}>{t("properties-refresh-link")}</button>{/if}
      </div>
    </div>

    {#if !done}
      <div class="grid">
        <div class="field">
          <span class="label">{t("properties-connections")}</span>
          <div class="stepper">
            <button class="icon-btn sm" onclick={() => setConnections(d.connections - 1)} disabled={d.connections <= 1} aria-label={t("properties-fewer")}><Minus size={14} /></button>
            <span class="num">{d.connections}</span>
            <button class="icon-btn sm" onclick={() => setConnections(d.connections + 1)} disabled={d.connections >= 32} aria-label={t("properties-more")}><Plus size={14} /></button>
            <span class="hint">{d.status === "downloading" ? t("properties-applies-right-away") : ""}</span>
          </div>
        </div>
        <div class="field">
          <span class="label">{t("properties-speed-limit")}</span>
          <div class="limit-wrap">
            <button class="btn" onclick={() => (limitMenu = !limitMenu)}><Gauge size={14} /> {limitLabel(d.speed_limit)}</button>
            {#if limitMenu}
              <SpeedMenu value={d.speed_limit} onselect={(v) => attempt(api.setSpeedLimit(d.id, v))} onclose={() => (limitMenu = false)} />
            {/if}
          </div>
        </div>
      </div>
    {/if}

    {#if plainFile}
      <label class="field">
        <span class="label">{t("properties-checksum")} <span class="hint">({done ? t("properties-use-verify-checksum-to-check-the") : t("properties-checked-when-the-download-finishes")})</span></span>
        <input class="input mono" bind:value={checksum} placeholder={t("properties-md5-sha-1-sha-256-or")} spellcheck="false" />
      </label>
      {#if !done}
        <label class="field">
          <span class="label">{t("properties-mirrors")} <span class="hint">{t("properties-other-addresses-of-the-same-file")}</span></span>
          <textarea class="input mono mirrors" rows="2" bind:value={mirrors} spellcheck="false" placeholder="https://mirror.example/file.iso"></textarea>
        </label>
      {/if}
    {/if}

    <dl>
      {#if d.referrer}<dt>{t("properties-referrer")}</dt><dd class="mono selectable truncate" title={d.referrer}>{d.referrer}</dd>{/if}
      {#if d.method && d.method !== "GET"}<dt>{t("properties-request")}</dt><dd>{t("properties-form-submission", { method: d.method })}</dd>{/if}
      <dt>{t("properties-added")}</dt><dd>{dateTime(d.created_at)}</dd>
      {#if d.completed_at}<dt>{t("properties-finished")}</dt><dd>{dateTime(d.completed_at)}</dd>{/if}
    </dl>
  </div>

  {#snippet footer()}
    <button class="btn btn-ghost" onclick={onclose}>{t("properties-close")}</button>
    <button class="btn btn-primary" onclick={save} disabled={!changed || saving}>{t("properties-save")}</button>
  {/snippet}
</Modal>

<style>
  .mirrors {
    resize: vertical;
    min-height: 54px;
    padding-top: 8px;
    font-size: 12px;
  }
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
