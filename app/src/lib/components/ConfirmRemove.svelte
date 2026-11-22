<script lang="ts">
  import { t } from "../i18n";
  import { api, type Download } from "../api";
  import { nameFromUrl } from "../format";
  import { toasts } from "../store.svelte";
  import Modal from "./Modal.svelte";

  let { items, onclose }: { items: Download[]; onclose: () => void } = $props();

  let deleteFiles = $state(false);
  const anyCompleted = $derived(items.some((d) => d.status === "completed"));
  const anyUnfinished = $derived(items.some((d) => d.status !== "completed"));
  const label = $derived(items.length === 1 ? t("confirm-remove-quoted-name", { name: items[0].filename ?? nameFromUrl(items[0].url) }) : t("confirm-remove-n-downloads", { count: items.length }));

  async function confirm() {
    for (const d of items) {
      try {
        await api.remove(d.id, deleteFiles);
      } catch (e) {
        toasts.error(e);
      }
    }
    onclose();
  }
</script>

<Modal title={t("confirm-remove-remove", { label })} width={440} {onclose}>
  <p>{t("confirm-remove-the-download-will-be-removed-from")}{#if anyUnfinished}{" "}{t("confirm-remove-unfinished-data-is-discarded")}{/if}</p>
  {#if anyCompleted}
    <button class="toggle" onclick={() => (deleteFiles = !deleteFiles)}>
      <span class="switch" class:on={deleteFiles}></span>
      {t("confirm-remove-also-delete-the-downloaded-from-disk", { count: items.length })}
    </button>
  {/if}
  {#snippet footer()}
    <button class="btn btn-ghost" onclick={onclose}>{t("confirm-remove-cancel")}</button>
    <button class="btn btn-danger" onclick={confirm}>{t("confirm-remove-remove-2")}</button>
  {/snippet}
</Modal>

<style>
  p {
    margin: 0 0 14px;
    color: var(--text-2);
  }
  .toggle {
    display: flex;
    align-items: center;
    gap: 10px;
    text-align: left;
  }
</style>
