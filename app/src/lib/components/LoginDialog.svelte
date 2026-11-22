<script lang="ts">
  import { t } from "../i18n";
  import type { Download } from "../api";
  import { nameFromUrl } from "../format";
  import HostKeyForm from "./HostKeyForm.svelte";
  import LoginForm from "./LoginForm.svelte";
  import Modal from "./Modal.svelte";

  let { d, onclose }: { d: Download; onclose: () => void } = $props();
</script>

{#if d.needs_host_key}
  <Modal title={t("login-trust-this-server")} width={520} {onclose}>
    <div class="file hint truncate">{d.filename ?? nameFromUrl(d.url)}</div>
    <HostKeyForm request={d.needs_host_key} ontrusted={onclose} oncancel={onclose} />
  </Modal>
{:else if d.needs_login}
  <Modal title={t("login-log-in-to-continue")} width={480} {onclose}>
    <div class="file hint truncate">{d.filename ?? nameFromUrl(d.url)}</div>
    <LoginForm request={d.needs_login} onloggedin={onclose} oncancel={onclose} />
  </Modal>
{/if}

<style>
  .file {
    margin: -6px 0 14px;
  }
</style>
