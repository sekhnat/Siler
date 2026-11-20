<script lang="ts" module>
  import type { Component } from "svelte";

  export type MenuEntry =
    | "separator"
    | {
        label: string;
        icon?: Component<any>;
        hint?: string;
        disabled?: boolean;
        danger?: boolean;
        run: () => void;
      };
</script>

<script lang="ts">
  import { onMount, tick } from "svelte";
  import { scale } from "svelte/transition";

  let { x, y, items, onclose }: { x: number; y: number; items: MenuEntry[]; onclose: () => void } = $props();

  let el: HTMLDivElement | undefined = $state();
  // Where the menu ends up after being kept inside the window.
  // svelte-ignore state_referenced_locally
  let pos = $state({ left: x, top: y });

  onMount(async () => {
    await tick();
    if (!el) return;
    const r = el.getBoundingClientRect();
    pos = {
      left: Math.max(6, Math.min(x, innerWidth - r.width - 6)),
      top: Math.max(6, y + r.height > innerHeight - 6 ? y - r.height : y),
    };
    focusItem(0);
  });

  const buttons = () => [...(el?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [])];

  function focusItem(i: number) {
    const b = buttons();
    if (b.length) b[(i + b.length) % b.length].focus();
  }

  function onkeydown(e: KeyboardEvent) {
    const b = buttons();
    const i = b.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "Escape") onclose();
    else if (e.key === "ArrowDown") focusItem(i + 1);
    else if (e.key === "ArrowUp") focusItem(i - 1);
    else if (e.key === "Home") focusItem(0);
    else if (e.key === "End") focusItem(-1);
    else return;
    e.preventDefault();
    e.stopPropagation();
  }

  function onpointerdown(e: PointerEvent) {
    if (el && !el.contains(e.target as Node)) onclose();
  }
</script>

<svelte:window {onpointerdown} onblur={onclose} onresize={onclose} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  bind:this={el}
  class="menu"
  role="menu"
  tabindex="-1"
  style:left="{pos.left}px"
  style:top="{pos.top}px"
  transition:scale={{ duration: 110, start: 0.96 }}
  {onkeydown}
  oncontextmenu={(e) => e.preventDefault()}
>
  {#each items as item, i (i)}
    {#if item === "separator"}
      <div class="sep" role="separator"></div>
    {:else}
      <button
        role="menuitem"
        class:danger={item.danger}
        disabled={item.disabled}
        onclick={() => {
          onclose();
          item.run();
        }}
      >
        <span class="icon">{#if item.icon}<item.icon size={15} />{/if}</span>
        <span class="label">{item.label}</span>
        {#if item.hint}<span class="hint-key">{item.hint}</span>{/if}
      </button>
    {/if}
  {/each}
</div>

<style>
  .menu {
    position: fixed;
    z-index: 60;
    min-width: 214px;
    padding: 5px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--r);
    box-shadow: var(--shadow-lg);
    transform-origin: top left;
    outline: none;
  }
  button {
    display: flex;
    width: 100%;
    align-items: center;
    gap: 10px;
    height: 30px;
    padding: 0 10px 0 8px;
    border-radius: var(--r-sm);
    text-align: left;
    outline: none;
  }
  button:hover:not(:disabled),
  button:focus-visible {
    background: var(--hover);
  }
  button:disabled {
    opacity: 0.4;
  }
  .icon {
    display: grid;
    place-items: center;
    width: 18px;
    color: var(--muted);
  }
  .label {
    flex: 1;
  }
  .hint-key {
    font-size: 11.5px;
    color: var(--muted);
  }
  .danger,
  .danger .icon {
    color: var(--err);
  }
  .sep {
    height: 1px;
    margin: 4px 6px;
    background: var(--border);
  }
</style>
