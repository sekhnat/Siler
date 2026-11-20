<script lang="ts">
  import type { Download } from "../api";
  import { nameFromUrl } from "../format";
  import LoginForm from "./LoginForm.svelte";
  import Modal from "./Modal.svelte";

  let { d, onclose }: { d: Download; onclose: () => void } = $props();
</script>

{#if d.needs_login}
  <Modal title="Log in to continue" width={480} {onclose}>
    <div class="file hint truncate">{d.filename ?? nameFromUrl(d.url)}</div>
    <LoginForm request={d.needs_login} onloggedin={onclose} oncancel={onclose} />
  </Modal>
{/if}

<style>
  .file {
    margin: -6px 0 14px;
  }
</style>
