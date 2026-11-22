<script lang="ts">
  import { t } from "../i18n";
  import { ArrowDown, ArrowUp } from "@lucide/svelte";
  import { DEFAULT_TOOLBAR, TOOLBAR_ITEMS, type ToolbarItem } from "../columns";
  import Modal from "./Modal.svelte";

  let { items, onsave, onclose }: { items: string[]; onsave: (items: ToolbarItem[]) => void; onclose: () => void } = $props();

  // Every item, the shown ones first in their order.
  // svelte-ignore state_referenced_locally
  let order = $state<{ id: ToolbarItem; label: string; on: boolean }[]>([
    ...items.map((id) => TOOLBAR_ITEMS.find((t) => t.id === id)).filter((t) => !!t).map((t) => ({ ...t, on: true })),
    ...TOOLBAR_ITEMS.filter((t) => !items.includes(t.id)).map((t) => ({ ...t, on: false })),
  ]);

  function move(i: number, by: number) {
    const j = i + by;
    if (j < 0 || j >= order.length) return;
    const next = [...order];
    [next[i], next[j]] = [next[j], next[i]];
    order = next;
  }

  function reset() {
    order = [
      ...DEFAULT_TOOLBAR.map((id) => ({ ...TOOLBAR_ITEMS.find((t) => t.id === id)!, on: true })),
      ...TOOLBAR_ITEMS.filter((t) => !DEFAULT_TOOLBAR.includes(t.id)).map((t) => ({ ...t, on: false })),
    ];
  }
</script>

<Modal title={t("toolbar-customize-the-toolbar")} width={420} {onclose}>
  <p class="hint">{t("toolbar-choose-the-buttons-beside-the-search")}</p>
  <ul class="list">
    {#each order as item, i (item.id)}
      <li class:off={!item.on}>
        <label>
          <input type="checkbox" bind:checked={item.on} />
          {item.label}
        </label>
        <span class="grow"></span>
        <button class="icon-btn sm" title={t("toolbar-move-up")} disabled={i === 0} onclick={() => move(i, -1)}><ArrowUp size={14} /></button>
        <button class="icon-btn sm" title={t("toolbar-move-down")} disabled={i === order.length - 1} onclick={() => move(i, 1)}><ArrowDown size={14} /></button>
      </li>
    {/each}
  </ul>
  <div class="actions">
    <button class="btn btn-ghost" onclick={reset}>{t("toolbar-reset")}</button>
    <span class="grow"></span>
    <button class="btn" onclick={onclose}>{t("toolbar-cancel")}</button>
    <button
      class="btn btn-primary"
      onclick={() => {
        onsave(order.filter((o) => o.on).map((o) => o.id));
        onclose();
      }}>{t("toolbar-save")}</button
    >
  </div>
</Modal>

<style>
  .list {
    list-style: none;
    margin: 12px 0;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  li {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 3px 6px;
    border-radius: var(--r-sm);
  }
  li:hover {
    background: var(--hover);
  }
  li.off label {
    color: var(--muted);
  }
  label {
    display: flex;
    align-items: center;
    gap: 9px;
    font-size: 13px;
  }
  input {
    accent-color: var(--accent);
  }
  .grow {
    flex: 1;
  }
  .actions {
    display: flex;
    gap: 8px;
  }
</style>
