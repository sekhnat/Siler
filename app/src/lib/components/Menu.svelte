<script lang="ts" generics="T">
  import { scale } from "svelte/transition";
  import { Check } from "@lucide/svelte";

  let {
    items,
    value,
    onselect,
    onclose,
    align = "left",
    direction = "down",
  }: {
    items: { label: string; value: T }[];
    value?: T;
    onselect: (v: T) => void;
    onclose: () => void;
    align?: "left" | "right";
    direction?: "up" | "down";
  } = $props();

  let el: HTMLDivElement;
  function onpointerdown(e: PointerEvent) {
    if (el && !el.contains(e.target as Node)) onclose();
  }
</script>

<svelte:window {onpointerdown} onkeydown={(e) => e.key === "Escape" && onclose()} />

<div
  bind:this={el}
  class="menu {align} {direction}"
  role="menu"
  transition:scale={{ duration: 120, start: 0.95 }}
>
  {#each items as item}
    <button
      role="menuitem"
      class:active={item.value === value}
      onclick={() => {
        onselect(item.value);
        onclose();
      }}
    >
      <span>{item.label}</span>
      {#if item.value === value}<Check size={14} />{/if}
    </button>
  {/each}
</div>

<style>
  .menu {
    position: absolute;
    z-index: 40;
    min-width: 170px;
    padding: 5px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--r);
    box-shadow: var(--shadow-lg);
  }
  .left {
    left: 0;
  }
  .right {
    right: 0;
  }
  .down {
    top: calc(100% + 6px);
    transform-origin: top;
  }
  .up {
    bottom: calc(100% + 6px);
    transform-origin: bottom;
  }
  button {
    display: flex;
    width: 100%;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 7px 10px;
    border-radius: var(--r-sm);
    text-align: left;
  }
  button:hover {
    background: var(--hover);
  }
  button.active {
    color: var(--accent-text);
    font-weight: 600;
  }
</style>
