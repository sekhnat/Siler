<script lang="ts">
  import { FolderOpen, RotateCcw, X } from "@lucide/svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, type CategoryRule, type Settings } from "../api";
  import { categoryMeta } from "../categories";
  import { toasts } from "../store.svelte";

  let { s = $bindable() }: { s: Settings } = $props();

  let drafts = $state<Record<string, string>>({});

  function defaultFolder(rule: CategoryRule): string {
    return s.use_category_folders ? `${s.download_dir}/${categoryMeta(rule.category).label}` : s.download_dir;
  }

  async function browse(rule: CategoryRule) {
    const picked = await open({ directory: true, defaultPath: rule.folder ?? defaultFolder(rule), title: `${categoryMeta(rule.category).label} folder` });
    if (typeof picked === "string") rule.folder = picked;
  }

  /** Adds extensions to a category, taking them away from any other category. */
  function addExtensions(rule: CategoryRule, text: string) {
    const exts = text
      .split(/[\s,;]+/)
      .map((e) => e.trim().replace(/^\.+/, "").toLowerCase())
      .filter((e) => /^[a-z0-9][a-z0-9.+_-]{0,15}$/.test(e));
    for (const e of exts) {
      for (const other of s.categories) {
        if (other !== rule && other.extensions.includes(e)) {
          other.extensions = other.extensions.filter((x) => x !== e);
          toasts.show(`.${e} moved from ${categoryMeta(other.category).label} to ${categoryMeta(rule.category).label}`);
        }
      }
      if (!rule.extensions.includes(e)) rule.extensions.push(e);
    }
    drafts[rule.category] = "";
  }

  function onkeydown(rule: CategoryRule, e: KeyboardEvent) {
    const value = drafts[rule.category] ?? "";
    if ((e.key === "Enter" || e.key === "," || e.key === " ") && value.trim()) {
      e.preventDefault();
      addExtensions(rule, value);
    } else if (e.key === "Backspace" && !value && rule.extensions.length) {
      rule.extensions.pop();
    }
  }

  async function reset() {
    const defaults = await api.defaultCategories().catch(() => null);
    if (defaults) s.categories = defaults;
  }
</script>

<div class="cats">
  <div class="intro">
    <span class="hint">Files are sorted by extension. Each category can have its own folder.</span>
    <button class="btn btn-ghost btn-sm" onclick={reset}><RotateCcw size={13} /> Reset all</button>
  </div>
  {#each s.categories as rule (rule.category)}
    {@const meta = categoryMeta(rule.category)}
    <div class="cat">
      <div class="cat-head">
        <span class="dot" style:--c={meta.color}><meta.icon size={13} strokeWidth={2.2} /></span>
        <span class="cat-name">{meta.label}</span>
        <div class="folder">
          <input
            class="input"
            value={rule.folder ?? ""}
            placeholder={defaultFolder(rule)}
            spellcheck="false"
            oninput={(e) => (rule.folder = e.currentTarget.value.trim() || null)}
          />
          <button class="icon-btn sm" title="Choose folder" onclick={() => browse(rule)}><FolderOpen size={14} /></button>
          {#if rule.folder}
            <button class="icon-btn sm" title="Use the default folder" onclick={() => (rule.folder = null)}><X size={14} /></button>
          {/if}
        </div>
      </div>
      {#if rule.category === "other"}
        <div class="hint exts">Everything that doesn't match another category.</div>
      {:else}
        <div class="exts chips">
          {#each rule.extensions as ext (ext)}
            <span class="chip">
              .{ext}
              <button aria-label="Remove .{ext}" onclick={() => (rule.extensions = rule.extensions.filter((x) => x !== ext))}><X size={11} /></button>
            </span>
          {/each}
          <input
            class="add"
            placeholder="add…"
            spellcheck="false"
            bind:value={drafts[rule.category]}
            onkeydown={(e) => onkeydown(rule, e)}
            onblur={() => drafts[rule.category]?.trim() && addExtensions(rule, drafts[rule.category])}
          />
        </div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .cats {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .intro {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .cat {
    padding: 10px 12px;
    border-radius: var(--r-lg);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .cat-head {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .dot {
    display: grid;
    place-items: center;
    flex: none;
    width: 22px;
    height: 22px;
    border-radius: 6px;
    color: var(--c);
    background: color-mix(in srgb, var(--c) 16%, transparent);
  }
  .cat-name {
    width: 96px;
    flex: none;
    font-weight: 650;
  }
  .folder {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .folder .input {
    height: 30px;
    font-size: 12.5px;
  }
  .exts {
    margin: 8px 0 0 32px;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    height: 22px;
    padding: 0 4px 0 8px;
    border-radius: 99px;
    font-size: 11.5px;
    font-weight: 600;
    background: var(--surface-3);
    color: var(--text-2);
  }
  .chip button {
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    border-radius: 99px;
    color: var(--muted);
  }
  .chip button:hover {
    background: var(--hover);
    color: var(--err);
  }
  .add {
    width: 70px;
    height: 22px;
    border: none;
    outline: none;
    background: none;
    color: var(--text);
    font: inherit;
    font-size: 12px;
  }
</style>
