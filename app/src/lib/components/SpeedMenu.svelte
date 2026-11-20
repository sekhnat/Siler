<script lang="ts">
  import { Check } from "@lucide/svelte";
  import { scale } from "svelte/transition";
  import { SPEED_PRESETS, SPEED_UNITS, limitLabel } from "../format";

  let {
    value,
    onselect,
    onclose,
    align = "left",
    direction = "down",
  }: {
    value: number;
    onselect: (bps: number) => void;
    onclose: () => void;
    align?: "left" | "right";
    direction?: "up" | "down";
  } = $props();

  let el: HTMLDivElement;
  let custom = $state(false);
  // Seed the custom field with the current limit.
  // svelte-ignore state_referenced_locally
  let unit = $state(value && value < 1024 ** 2 ? 0 : 1);
  // svelte-ignore state_referenced_locally
  let amount = $state(value ? +(value / SPEED_UNITS[value && value < 1024 ** 2 ? 0 : 1].factor).toFixed(1) : 1);
  const isPreset = $derived(SPEED_PRESETS.includes(value));

  function pick(v: number) {
    onselect(v);
    onclose();
  }

  function applyCustom(e: SubmitEvent) {
    e.preventDefault();
    if (amount > 0) pick(Math.round(amount * SPEED_UNITS[unit].factor));
  }

  function onpointerdown(e: PointerEvent) {
    if (el && !el.contains(e.target as Node)) onclose();
  }
</script>

<svelte:window {onpointerdown} onkeydown={(e) => e.key === "Escape" && onclose()} />

<div bind:this={el} class="menu {align} {direction}" role="menu" transition:scale={{ duration: 120, start: 0.95 }}>
  {#each SPEED_PRESETS as p (p)}
    <button role="menuitem" class:active={p === value} onclick={() => pick(p)}>
      <span>{limitLabel(p)}</span>
      {#if p === value}<Check size={14} />{/if}
    </button>
  {/each}
  {#if !isPreset}
    <button role="menuitem" class="active" onclick={onclose}>
      <span>{limitLabel(value)}</span>
      <Check size={14} />
    </button>
  {/if}
  <div class="sep"></div>
  {#if custom}
    <form class="custom" onsubmit={applyCustom}>
      <!-- svelte-ignore a11y_autofocus -->
      <input class="input" type="number" min="1" step="any" bind:value={amount} autofocus />
      <select class="input" bind:value={unit}>
        {#each SPEED_UNITS as u, i (u.label)}<option value={i}>{u.label}</option>{/each}
      </select>
      <button type="submit" class="btn btn-primary btn-sm">Set</button>
    </form>
  {:else}
    <button role="menuitem" onclick={() => (custom = true)}><span>Custom…</span></button>
  {/if}
</div>

<style>
  .menu {
    position: absolute;
    z-index: 40;
    min-width: 184px;
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
  button[role="menuitem"] {
    display: flex;
    width: 100%;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 7px 10px;
    border-radius: var(--r-sm);
    text-align: left;
  }
  button[role="menuitem"]:hover {
    background: var(--hover);
  }
  .active {
    color: var(--accent-text);
    font-weight: 600;
  }
  .sep {
    height: 1px;
    margin: 4px 6px;
    background: var(--border);
  }
  .custom {
    display: flex;
    gap: 5px;
    padding: 4px;
  }
  .custom .input {
    height: 30px;
    padding: 0 8px;
  }
  .custom input {
    width: 54px;
    min-width: 0;
  }
  .custom select {
    width: 68px;
    padding: 0 4px;
  }
</style>
