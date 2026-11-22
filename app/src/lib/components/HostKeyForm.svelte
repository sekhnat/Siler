<script lang="ts">
  import { t } from "../i18n";
  import { ShieldQuestion } from "@lucide/svelte";
  import { api, type HostKeyRequest } from "../api";
  import { toasts } from "../store.svelte";

  let {
    request,
    ontrusted,
    oncancel,
  }: {
    request: HostKeyRequest;
    /** Called after the key was saved (downloads waiting for it resume). */
    ontrusted: () => void;
    oncancel?: () => void;
  } = $props();

  let busy = $state(false);
  const where = $derived(request.port === 22 ? request.server : `${request.server}:${request.port}`);

  async function trust() {
    busy = true;
    try {
      await api.trustHostKey(request);
      ontrusted();
    } catch (err) {
      toasts.error(err);
    } finally {
      busy = false;
    }
  }
</script>

<div class="hostkey">
  <div class="who">
    <ShieldQuestion size={18} />
    <div class="min">
      <div class="t">{t("host-key-form-siler-hasn-t-connected-to")} <b>{where}</b> {t("host-key-form-before")}</div>
      <div class="hint">{t("host-key-form-make-sure-this-is-the-server")} <code>ssh-keygen -lf</code> {t("host-key-form-on-the-server")}</div>
    </div>
  </div>
  <dl>
    <dt>{t("host-key-form-key-type")}</dt>
    <dd>{request.algorithm}</dd>
    <dt>{t("host-key-form-fingerprint")}</dt>
    <dd class="mono">{request.fingerprint}</dd>
  </dl>
  <div class="actions">
    {#if oncancel}<button type="button" class="btn btn-ghost" onclick={oncancel}>{t("host-key-form-cancel")}</button>{/if}
    <span class="grow"></span>
    <button type="button" class="btn btn-primary" disabled={busy} onclick={trust}>{t("host-key-form-trust-and-connect")}</button>
  </div>
</div>

<style>
  .hostkey {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .who {
    display: flex;
    align-items: flex-start;
    gap: 12px;
  }
  .who > :global(svg) {
    flex: none;
    color: var(--accent-text);
    margin-top: 2px;
  }
  .min {
    min-width: 0;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .t {
    font-weight: 550;
    overflow-wrap: anywhere;
  }
  dl {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: 6px 14px;
    margin: 0;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--surface-2);
    font-size: 12.5px;
  }
  dt {
    color: var(--text-2);
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .mono {
    font-family: var(--mono);
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  .grow {
    flex: 1;
  }
</style>
