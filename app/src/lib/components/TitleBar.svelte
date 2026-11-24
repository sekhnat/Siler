<script lang="ts">
  // A title bar drawn by Siler, for windows without the desktop's: dragging it moves the
  // window. Minimize puts the window away in the tray (the desktop's own button couldn't be
  // told apart, and on Wayland a minimised window can't be brought back).
  import type { Snippet } from "svelte";
  import { Minus, X } from "@lucide/svelte";

  let {
    title,
    icon,
    onminimize,
    onclose,
    minimizeLabel = "",
    closeLabel,
  }: {
    title: string;
    icon?: Snippet;
    onminimize?: () => void;
    onclose: () => void;
    minimizeLabel?: string;
    closeLabel: string;
  } = $props();
</script>

<header class="titlebar" data-tauri-drag-region>
  {#if icon}<span class="icon" data-tauri-drag-region>{@render icon()}</span>{/if}
  <span class="title truncate" data-tauri-drag-region>{title}</span>
  {#if onminimize}
    <button class="tb" title={minimizeLabel} aria-label={minimizeLabel} onclick={onminimize}><Minus size={15} /></button>
  {/if}
  <button class="tb close" title={closeLabel} aria-label={closeLabel} onclick={onclose}><X size={15} /></button>
</header>

<style>
  .titlebar {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 36px;
    flex: none;
    padding: 0 4px 0 12px;
    background: var(--surface-2);
    border-bottom: 1px solid var(--border);
    user-select: none;
    -webkit-user-select: none;
  }
  .icon {
    display: flex;
  }
  .title {
    flex: 1;
    min-width: 0;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-2);
  }
  .tb {
    display: grid;
    place-items: center;
    width: 32px;
    height: 28px;
    border: 0;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--text-2);
    cursor: pointer;
  }
  .tb:hover {
    background: var(--hover);
    color: var(--text);
  }
  .tb.close:hover {
    background: var(--err);
    color: #fff;
  }
</style>
