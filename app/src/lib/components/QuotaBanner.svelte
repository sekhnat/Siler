<script lang="ts">
  import { t } from "../i18n";
  import { Gauge } from "@lucide/svelte";
  import { api } from "../api";
  import { bytes } from "../format";
  import { attempt, store } from "../store.svelte";

  const q = $derived(store.quota);
  const at = $derived(
    q?.resumes_at ? new Date(q.resumes_at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" }) : null,
  );
</script>

{#if q?.enabled && q.reached}
  <div class="banner">
    <Gauge size={16} />
    <span class="grow">
      <b>{t("quota-banner-data-quota-reached")}</b> {at ? t("quota-banner-used-wait-until", { used: bytes(q.used), limit: bytes(q.limit), at }) : t("quota-banner-used-wait", { used: bytes(q.used), limit: bytes(q.limit) })}
    </span>
    <button class="btn btn-sm" onclick={() => attempt(api.ignoreQuota())}>{t("quota-banner-ignore-for-this-period")}</button>
  </div>
{/if}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 0 14px 10px;
    padding: 10px 14px;
    border-radius: var(--r-lg);
    background: color-mix(in srgb, var(--warn) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 30%, transparent);
    font-size: 12.5px;
  }
  .banner > :global(svg) {
    color: var(--warn);
    flex: none;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
</style>
