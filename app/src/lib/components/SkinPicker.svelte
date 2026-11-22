<script lang="ts">
  // Settings → Appearance: the skins as swatches, shown on the spot and kept on Save.
  import { onDestroy } from "svelte";
  import { Check, Download, Trash2, Upload } from "@lucide/svelte";
  import { open, save } from "@tauri-apps/plugin-dialog";
  import { api, type Skin, type ToolbarStyle } from "../api";
  import { t } from "../i18n";
  import { toasts } from "../store.svelte";
  import { theme } from "../theme.svelte";

  let { skin = $bindable(), toolbar = $bindable() }: { skin: string; toolbar: ToolbarStyle | "" } = $props();

  const chosen = $derived(theme.skins.find((s) => s.id === skin) ?? theme.skins[0]);
  const nameOf = (s: Skin) => (s.built_in ? t(`skin-name-${s.id}`) : s.name);
  const colours = (s: Skin) => (theme.dark ? s.dark : s.light);

  function pick(id: string) {
    skin = id;
    theme.preview(id, toolbar);
  }

  async function importSkin() {
    const path = await open({ title: t("skin-import"), filters: [{ name: t("skin-file"), extensions: ["json"] }] });
    if (typeof path !== "string") return;
    try {
      const added = await api.importSkin(path);
      await theme.load();
      pick(added.id);
    } catch (e) {
      toasts.error(e);
    }
  }

  async function exportSkin() {
    if (!chosen) return;
    const path = await save({ title: t("skin-export"), defaultPath: `${chosen.id}.json`, filters: [{ name: t("skin-file"), extensions: ["json"] }] });
    if (!path) return;
    try {
      await api.exportSkin(chosen.id, path);
      toasts.show(t("skin-exported"), "success");
    } catch (e) {
      toasts.error(e);
    }
  }

  async function removeSkin() {
    if (!chosen) return;
    try {
      await api.removeSkin(chosen.id);
      await theme.load();
      pick("siler");
    } catch (e) {
      toasts.error(e);
    }
  }

  // Saved or not, the skin in the settings is what shows afterwards.
  onDestroy(() => theme.endPreview());
</script>

<div class="field">
  <span class="label">{t("skin-title")}</span>
  <div class="grid" role="radiogroup" aria-label={t("skin-title")}>
    {#each theme.skins as s (s.id)}
      {@const c = colours(s)}
      <button class="swatch" class:on={chosen?.id === s.id} role="radio" aria-checked={chosen?.id === s.id} onclick={() => pick(s.id)}>
        <span class="mini" aria-hidden="true" style:background={c.bg} style:border-radius="{s.radius}px" style:border-color={c["border-strong"]}>
          <span class="side" style:background={c.sidebar}><i style:background={`linear-gradient(90deg, ${c.button}, ${c["button-2"]})`}></i></span>
          <span class="rows">
            <span class="row" style:background={c.surface} style:border-radius="{s.radius * 0.5}px">
              <i style:background={c.text}></i>
              <i class="fill" style:background={`linear-gradient(90deg, ${c["grad-from"]}, ${c["grad-mid"]}, ${c["grad-to"]})`}></i>
            </span>
            <span class="row" style:background={c.surface} style:border-radius="{s.radius * 0.5}px">
              <i style:background={c["text-2"]}></i>
              <i class="short" style:background={c.ok}></i>
            </span>
          </span>
        </span>
        <span class="name"><span class="truncate">{nameOf(s)}</span>{#if chosen?.id === s.id}<Check size={13} />{/if}</span>
      </button>
    {/each}
  </div>
  <div class="actions">
    <button class="btn btn-sm" onclick={importSkin}><Upload size={13} />{t("skin-import")}</button>
    <button class="btn btn-sm" disabled={!chosen} onclick={exportSkin}><Download size={13} />{t("skin-export")}</button>
    {#if chosen && !chosen.built_in}
      <button class="btn btn-sm" onclick={removeSkin}><Trash2 size={13} />{t("skin-remove")}</button>
    {/if}
  </div>
</div>

<div class="field">
  <span class="label">{t("skin-toolbar")}</span>
  <select class="input" bind:value={toolbar} onchange={() => theme.preview(chosen?.id ?? "siler", toolbar)}>
    <option value="">{t("skin-toolbar-skins")}</option>
    <option value="mixed">{t("skin-toolbar-mixed")}</option>
    <option value="icons">{t("skin-toolbar-icons")}</option>
    <option value="text">{t("skin-toolbar-text")}</option>
  </select>
</div>

<style>
  .field {
    margin-bottom: 14px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(122px, 1fr));
    gap: 8px;
  }
  .swatch {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
    padding: 6px;
    border: 1px solid var(--border);
    border-radius: var(--r);
  }
  .swatch:hover {
    background: var(--hover);
  }
  .swatch.on {
    border-color: var(--accent);
    background: var(--selected);
  }
  .mini {
    display: flex;
    width: 100%;
    height: 58px;
    overflow: hidden;
    border: 1px solid;
  }
  .side {
    width: 30%;
    padding: 7px 5px;
  }
  .side i {
    display: block;
    height: 9px;
    border-radius: 3px;
  }
  .rows {
    flex: 1;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 5px;
    padding: 6px;
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 4px 5px;
  }
  .row i {
    width: 55%;
    height: 3px;
    border-radius: 2px;
  }
  .row .fill {
    width: 85%;
  }
  .row .short {
    width: 35%;
  }
  .name {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 4px;
    min-width: 0;
    padding: 0 2px;
    font-size: 12px;
    font-weight: 550;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
  }
</style>
