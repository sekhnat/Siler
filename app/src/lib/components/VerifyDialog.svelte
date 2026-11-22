<script lang="ts">
  import { t } from "../i18n";
  import { Copy, LoaderCircle, ShieldCheck, ShieldX } from "@lucide/svelte";
  import { ALGO_LABEL, api, errorText, type Checksum, type Download, type VerifyResult } from "../api";
  import { nameFromUrl } from "../format";
  import { toasts } from "../store.svelte";
  import Modal from "./Modal.svelte";

  let { d, onclose }: { d: Download; onclose: () => void } = $props();

  // svelte-ignore state_referenced_locally
  let text = $state(d.checksum?.hex ?? "");
  let parsed = $state<Checksum | null>(null);
  let parseError = $state("");
  let busy = $state(false);
  let result = $state<VerifyResult | null>(null);
  let error = $state("");
  let digests = $state<{ algo: string; hex: string }[] | null>(null);
  let digesting = $state(false);

  $effect(() => {
    const t = text.trim();
    parsed = null;
    parseError = "";
    if (!t) return;
    let stale = false;
    api
      .parseChecksum(t)
      .then((c) => !stale && (parsed = c))
      .catch((e) => !stale && (parseError = errorText(e)));
    return () => {
      stale = true;
    };
  });

  const matches = $derived(result?.expected ? result.expected.toLowerCase() === result.actual.toLowerCase() : null);

  async function verify() {
    if (busy || parseError) return;
    busy = true;
    error = "";
    result = null;
    try {
      result = await api.verifyFile(d.id, text.trim() || null);
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  async function compute() {
    digesting = true;
    try {
      digests = await api.fileDigests(d.id);
    } catch (e) {
      toasts.error(e);
    } finally {
      digesting = false;
    }
  }

  async function copy(v: string) {
    await navigator.clipboard.writeText(v).catch(() => {});
    toasts.show(t("verify-copied"), "success");
  }
</script>

<Modal title={t("verify-verify", { v: d.filename ?? nameFromUrl(d.url) })} width={560} {onclose}>
  <div class="body">
    <label class="field">
      <span class="label">{t("verify-expected-checksum")} <span class="hint">{t("verify-paste-it-from-the-site-md5")}</span></span>
      <!-- svelte-ignore a11y_autofocus -->
      <input class="input mono" bind:value={text} placeholder={t("verify-leave-empty-to-work-out-its")} spellcheck="false" autofocus onkeydown={(e) => e.key === "Enter" && verify()} />
      {#if parseError}<span class="note err">{parseError}</span>{:else if parsed}<span class="note">{ALGO_LABEL[parsed.algo]}</span>{/if}
    </label>

    {#if busy}
      <div class="result"><LoaderCircle size={16} class="spin" /> {t("verify-reading-the-file")}</div>
    {:else if error}
      <div class="result bad"><ShieldX size={16} /> {error}</div>
    {:else if result}
      <div class="result" class:good={matches === true} class:bad={matches === false}>
        {#if matches === true}<ShieldCheck size={18} /> <b>{t("verify-the-file-is-intact")}</b> {t("verify-its-matches", { v: ALGO_LABEL[result.algo] })}
        {:else if matches === false}<ShieldX size={18} /> <b>{t("verify-the-file-doesn-t-match")}</b> {t("verify-it-is-damaged-or-a-different")}
        {:else}<ShieldCheck size={18} /> {t("verify-its-is-below", { v: ALGO_LABEL[result.algo] })}{/if}
      </div>
      <div class="mono hex selectable">{result.actual}</div>
    {/if}

    <div class="digests">
      {#if digests}
        {#each digests as g (g.algo)}
          <div class="dg"><span class="algo">{g.algo}</span><span class="mono hex selectable truncate">{g.hex}</span><button class="icon-btn xs" onclick={() => copy(g.hex)} aria-label={t("verify-copy")}><Copy size={12} /></button></div>
        {/each}
      {:else}
        <button class="link" onclick={compute} disabled={digesting}>{digesting ? t("verify-working-them-out") : t("verify-show-its-md5-sha-1-and")}</button>
      {/if}
    </div>
  </div>
  {#snippet footer()}
    <span class="grow"></span>
    <button class="btn btn-ghost" onclick={onclose}>{t("verify-close")}</button>
    <button class="btn btn-primary" onclick={verify} disabled={busy || !!parseError}>{t("verify-verify-2")}</button>
  {/snippet}
</Modal>

<style>
  .body {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .field {
    display: block;
  }
  .note {
    display: block;
    margin-top: 5px;
    font-size: 12px;
    font-weight: 600;
    color: var(--ok);
  }
  .note.err {
    color: var(--err);
  }
  .result {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 14px;
    border-radius: var(--r-lg);
    background: var(--surface-2);
  }
  .result :global(.spin) {
    animation: spin 0.9s linear infinite;
  }
  .result.good {
    background: color-mix(in srgb, var(--ok) 12%, transparent);
    color: var(--ok);
  }
  .result.bad {
    background: color-mix(in srgb, var(--err) 10%, transparent);
    color: var(--err);
  }
  .hex {
    font-size: 11.5px;
    word-break: break-all;
  }
  .digests {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .dg {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .algo {
    width: 60px;
    flex: none;
    font-weight: 600;
    font-size: 12px;
    color: var(--text-2);
  }
  .link {
    align-self: flex-start;
    color: var(--accent);
    font-weight: 600;
    font-size: 12.5px;
  }
  .grow {
    flex: 1;
  }
</style>
