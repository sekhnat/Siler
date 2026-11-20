<script lang="ts">
  import { ArrowDownToLine, CircleAlert, CircleCheck, Clock, Gauge, Layers, Plus, Settings } from "@lucide/svelte";
  import type { Download } from "../api";
  import { CATEGORIES, isActive } from "../categories";
  import { limitLabel, speed } from "../format";
  import { store } from "../store.svelte";
  import Logo from "./Logo.svelte";
  import SpeedMenu from "./SpeedMenu.svelte";
  import Sparkline from "./Sparkline.svelte";

  let {
    filter = $bindable(),
    globalLimit,
    onlimit,
    onadd,
    onsettings,
  }: {
    filter: string;
    globalLimit: number;
    onlimit: (bps: number) => void;
    onadd: () => void;
    onsettings: () => void;
  } = $props();

  let limitMenu = $state(false);

  const count = (pred: (d: Download) => boolean) => store.list.filter(pred).length;
  const statusNav = $derived([
    { key: "all", label: "All downloads", icon: Layers, n: store.list.length },
    { key: "active", label: "Downloading", icon: ArrowDownToLine, n: count((d) => isActive(d.status)) },
    { key: "queued", label: "Queued & paused", icon: Clock, n: count((d) => d.status === "queued" || d.status === "paused" || d.status === "waiting") },
    { key: "completed", label: "Completed", icon: CircleCheck, n: count((d) => d.status === "completed") },
    { key: "failed", label: "Failed", icon: CircleAlert, n: count((d) => d.status === "failed") },
  ]);
  const catNav = $derived(
    CATEGORIES.map((c) => ({ ...c, n: count((d) => d.category === c.key) })).filter((c) => c.n > 0 || c.key !== "other"),
  );
  const activeCount = $derived(count((d) => isActive(d.status)));
</script>

<aside>
  <div class="brand">
    <Logo size={30} />
    <div>
      <div class="name">K-Star</div>
      <div class="tag">Download manager</div>
    </div>
  </div>

  <button class="btn btn-primary add" onclick={onadd}>
    <Plus size={17} strokeWidth={2.4} /> Add URL
  </button>

  <nav>
    {#each statusNav as item (item.key)}
      <button class="nav" class:on={filter === item.key} onclick={() => (filter = item.key)}>
        <item.icon size={16} />
        <span class="truncate">{item.label}</span>
        {#if item.n}<span class="count">{item.n}</span>{/if}
      </button>
    {/each}

    <div class="section">Categories</div>
    {#each catNav as c (c.key)}
      <button class="nav" class:on={filter === `cat:${c.key}`} onclick={() => (filter = `cat:${c.key}`)}>
        <span class="dot" style:--c={c.color}><c.icon size={13} strokeWidth={2.2} /></span>
        <span class="truncate">{c.label}</span>
        {#if c.n}<span class="count">{c.n}</span>{/if}
      </button>
    {/each}
  </nav>

  <div class="speed-card">
    <div class="speed-head">
      <div>
        <div class="speed-label">{activeCount ? `${activeCount} active` : "Idle"}</div>
        <div class="speed-value">{speed(store.totalSpeed)}</div>
      </div>
      <div class="limit-wrap">
        <button class="limit" class:limited={globalLimit > 0} onclick={() => (limitMenu = !limitMenu)} title="Global speed limit">
          <Gauge size={14} />
          {globalLimit ? limitLabel(globalLimit) : "No limit"}
        </button>
        {#if limitMenu}
          <SpeedMenu align="right" direction="up" value={globalLimit} onselect={onlimit} onclose={() => (limitMenu = false)} />
        {/if}
      </div>
    </div>
    <Sparkline values={store.history} height={42} />
  </div>

  <button class="nav settings" onclick={onsettings}>
    <Settings size={16} />
    <span>Settings</span>
  </button>
</aside>

<style>
  aside {
    display: flex;
    flex-direction: column;
    width: 236px;
    flex: none;
    padding: 16px 12px 12px;
    background: var(--sidebar);
    border-right: 1px solid var(--border);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 2px 8px 16px;
  }
  .name {
    font-weight: 750;
    font-size: 16px;
    letter-spacing: -0.02em;
  }
  .tag {
    font-size: 11.5px;
    color: var(--muted);
    margin-top: -2px;
  }
  .add {
    height: 40px;
    margin: 0 4px 14px;
    font-size: 14px;
  }
  nav {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .section {
    padding: 16px 12px 6px;
    font-size: 11px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
  }
  .nav {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 34px;
    padding: 0 10px 0 12px;
    border-radius: var(--r-sm);
    color: var(--text-2);
    font-weight: 500;
    text-align: left;
    transition: background 0.12s, color 0.12s;
  }
  .nav:hover {
    background: var(--hover);
    color: var(--text);
  }
  .nav.on {
    background: var(--selected);
    color: var(--accent-text);
    font-weight: 600;
  }
  .nav span:nth-child(2) {
    flex: 1;
  }
  .count {
    font-size: 11.5px;
    font-weight: 600;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .nav.on .count {
    color: var(--accent-text);
  }
  .dot {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    margin-left: -2px;
    border-radius: 6px;
    color: var(--c);
    background: color-mix(in srgb, var(--c) 16%, transparent);
  }
  .speed-card {
    margin: 10px 4px 8px;
    padding: 12px 12px 6px;
    border-radius: var(--r-lg);
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: var(--shadow-sm);
  }
  .speed-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 8px;
  }
  .speed-label {
    font-size: 11.5px;
    color: var(--muted);
    font-weight: 550;
  }
  .speed-value {
    font-size: 18px;
    font-weight: 700;
    letter-spacing: -0.02em;
    font-variant-numeric: tabular-nums;
  }
  .limit-wrap {
    position: relative;
  }
  .limit {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    padding: 0 8px;
    border-radius: 99px;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--muted);
    background: var(--surface-3);
  }
  .limit.limited {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 14%, transparent);
  }
  .settings {
    margin: 0 0 0;
  }
</style>
