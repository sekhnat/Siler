<script lang="ts">
  import { t } from "../i18n";
  import type { Snippet } from "svelte";
  import { fade, scale } from "svelte/transition";
  import { X } from "@lucide/svelte";

  let {
    title,
    width = 520,
    onclose,
    children,
    footer,
  }: { title?: string; width?: number; onclose: () => void; children: Snippet; footer?: Snippet } = $props();

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      onclose();
    }
  }
</script>

<svelte:window {onkeydown} />

<div class="backdrop" role="presentation" transition:fade={{ duration: 140 }} onpointerdown={(e) => e.target === e.currentTarget && onclose()}>
  <div class="dialog" style:width="{width}px" role="dialog" aria-modal="true" transition:scale={{ duration: 170, start: 0.96 }}>
    {#if title}
      <header>
        <h2>{title}</h2>
        <button class="icon-btn sm" onclick={onclose} aria-label={t("modal-close")}><X size={16} /></button>
      </header>
    {/if}
    <div class="body">{@render children()}</div>
    {#if footer}<footer>{@render footer()}</footer>{/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 50;
    display: grid;
    place-items: center;
    padding: 24px;
    background: var(--backdrop);
    backdrop-filter: blur(3px);
  }
  .dialog {
    max-width: 100%;
    max-height: calc(100vh - 48px);
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-xl);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 18px 18px 4px 22px;
  }
  h2 {
    margin: 0;
    font-size: 16px;
    font-weight: 650;
    letter-spacing: -0.01em;
  }
  .body {
    padding: 14px 22px 20px;
    overflow: auto;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 14px 22px;
    border-top: 1px solid var(--border);
    background: var(--surface-2);
  }
</style>
