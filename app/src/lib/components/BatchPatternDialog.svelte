<script lang="ts">
  import { ListOrdered, TriangleAlert } from "@lucide/svelte";
  import { api, errorText } from "../api";
  import Modal from "./Modal.svelte";

  let { onclose, oncontinue }: { onclose: () => void; oncontinue: (urls: string[]) => void } = $props();

  let pattern = $state("");
  let from = $state("1");
  let to = $state("20");
  let width = $state(0);
  let kind = $state<"numbers" | "letters">("numbers");
  let result = $state<{ count: number; urls: string[] } | null>(null);
  let error = $state("");

  /** The bracket the helper row builds. */
  const range = $derived.by(() => {
    if (kind === "letters") return `[${from.trim() || "a"}-${to.trim() || "z"}]`;
    const a = String(Math.max(0, parseInt(from) || 0));
    const b = String(Math.max(0, parseInt(to) || 0));
    return `[${width > 1 ? a.padStart(width, "0") : a}-${b}]`;
  });

  function insert() {
    // Replace a `*` in the address, else an existing range, else add before the extension.
    if (pattern.includes("*")) pattern = pattern.replace("*", range);
    else if (/\[[^\]]*-[^\]]*\]/.test(pattern)) pattern = pattern.replace(/\[[^\]]*-[^\]]*\]/, range);
    else {
      const m = pattern.match(/^(.*?)(\.[A-Za-z0-9]{1,5})?([?#].*)?$/);
      pattern = m ? `${m[1]}${range}${m[2] ?? ""}${m[3] ?? ""}` : pattern + range;
    }
  }

  $effect(() => {
    const p = pattern.trim();
    error = "";
    result = null;
    if (!p) return;
    let stale = false;
    const t = setTimeout(() => {
      api
        .expandBatch(p)
        .then((r) => !stale && (result = r))
        .catch((e) => !stale && (error = errorText(e)));
    }, 200);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  });

  function submit() {
    if (result?.count) oncontinue(result.urls);
  }
</script>

<Modal title="Numbered batch download" width={560} {onclose}>
  <div class="body">
    <p class="hint intro">
      Write the address with a range where the number or letter changes, like <code>https://example.com/photos/img[001-120].jpg</code>.
      <code>[a-z]</code> goes through letters, <code>[0-100:5]</code> counts in fives, and <code>{"{jpg,png}"}</code> tries each word.
    </p>
    <label class="field">
      <span class="label">Address pattern</span>
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="input mono"
        bind:value={pattern}
        placeholder="https://example.com/files/part[1-10].zip"
        spellcheck="false"
        autofocus
        onkeydown={(e) => e.key === "Enter" && submit()}
      />
    </label>

    <div class="helper">
      <span class="label">Or build a range</span>
      <div class="helper-row">
        <select class="input" bind:value={kind}>
          <option value="numbers">Numbers</option>
          <option value="letters">Letters</option>
        </select>
        <input class="input small" bind:value={from} aria-label="From" />
        <span class="hint">to</span>
        <input class="input small" bind:value={to} aria-label="To" />
        {#if kind === "numbers"}
          <label class="hint pad">Digits <input class="input tiny" type="number" min="0" max="9" bind:value={width} /></label>
        {/if}
        <button class="btn btn-sm" onclick={insert} disabled={!pattern.trim()} title="Put {range} into the address (where * is, or before the extension)">Insert {range}</button>
      </div>
    </div>

    <div class="preview">
      {#if error}
        <div class="err"><TriangleAlert size={14} /> {error}</div>
      {:else if result}
        <div class="count"><ListOrdered size={15} /> {result.count} {result.count === 1 ? "address" : "addresses"}</div>
        <div class="mono ends truncate" title={result.urls[0]}>{result.urls[0]}</div>
        {#if result.count > 2}<div class="hint dots">…</div>{/if}
        {#if result.count > 1}<div class="mono ends truncate" title={result.urls[result.count - 1]}>{result.urls[result.count - 1]}</div>{/if}
      {:else}
        <div class="hint">The first and last address appear here.</div>
      {/if}
    </div>
  </div>
  {#snippet footer()}
    <span class="grow"></span>
    <button class="btn btn-ghost" onclick={onclose}>Cancel</button>
    <button class="btn btn-primary" disabled={!result?.count} onclick={submit}>Review {result?.count ?? ""} files</button>
  {/snippet}
</Modal>

<style>
  .body {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .intro {
    margin: 0;
    line-height: 1.5;
  }
  code {
    white-space: nowrap;
    font-size: 11.5px;
    padding: 1px 4px;
    border-radius: 4px;
    background: var(--surface-3);
  }
  .field {
    display: block;
  }
  .helper-row {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .helper-row select {
    width: auto;
  }
  .small {
    width: 70px;
  }
  .tiny {
    width: 52px;
    margin-left: 4px;
  }
  .pad {
    display: inline-flex;
    align-items: center;
  }
  .preview {
    padding: 12px 14px;
    border-radius: var(--r-lg);
    background: var(--grad-soft);
    border: 1px solid var(--border);
    min-height: 74px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    justify-content: center;
  }
  .count {
    display: flex;
    align-items: center;
    gap: 7px;
    font-weight: 700;
  }
  .ends {
    font-size: 12px;
  }
  .dots {
    line-height: 1;
  }
  .err {
    display: flex;
    gap: 7px;
    align-items: center;
    color: var(--err);
    font-weight: 600;
  }
  .grow {
    flex: 1;
  }
</style>
