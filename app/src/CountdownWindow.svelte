<script lang="ts">
  import { t } from "./lib/i18n";
  import { Moon, Power, LogOut, Snowflake } from "@lucide/svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount } from "svelte";
  import { api, type PendingPower } from "./lib/api";

  /** Counts down before Siler puts the computer to sleep or shuts it down. */
  let pending = $state<PendingPower | null>(null);
  let deadline = 0;
  let left = $state(0);

  const ICON = { nothing: Power, quit: LogOut, sleep: Moon, hibernate: Snowflake, power_off: Power };

  function set(p: PendingPower | null) {
    pending = p;
    if (!p) {
      getCurrentWindow().close();
      return;
    }
    deadline = Date.now() + p.seconds_left * 1000;
    left = p.seconds_left;
  }

  onMount(() => {
    api.powerPending().then(set);
    const unlisten = listen<PendingPower | null>("power-countdown", (e) => set(e.payload));
    const timer = setInterval(() => (left = Math.max(0, Math.ceil((deadline - Date.now()) / 1000))), 250);
    return () => {
      clearInterval(timer);
      unlisten.then((f) => f());
    };
  });

  const Icon = $derived(ICON[pending?.action ?? "nothing"]);
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && api.powerCancel()} />

{#if pending}
  <div class="count">
    <div class="top">
      <div class="ring"><Icon size={22} /></div>
      <div class="min">
        <div class="title">{t("countdown-title", { action: pending.action })} <b>{t("countdown-seconds", { s: left })}</b></div>
        <div class="hint">{pending.reason}.</div>
      </div>
    </div>
    <div class="bar"><span style:width="{(left / 60) * 100}%"></span></div>
    <div class="actions">
      <button class="btn btn-ghost" onclick={() => api.powerNow()}>{t("countdown-now", { action: pending.action })}</button>
      <span class="grow"></span>
      <!-- svelte-ignore a11y_autofocus -->
      <button class="btn btn-primary" autofocus onclick={() => api.powerCancel()}>{t("countdown-cancel")}</button>
    </div>
  </div>
{/if}

<style>
  .count {
    display: flex;
    flex-direction: column;
    gap: 16px;
    height: 100vh;
    padding: 22px;
    background: var(--surface);
  }
  .top {
    display: flex;
    gap: 14px;
    align-items: center;
  }
  .ring {
    display: grid;
    place-items: center;
    flex: none;
    width: 44px;
    height: 44px;
    border-radius: 99px;
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 14%, transparent);
  }
  .min {
    min-width: 0;
  }
  .title {
    font-size: 15px;
    font-weight: 650;
  }
  .title b {
    font-variant-numeric: tabular-nums;
  }
  .bar {
    height: 4px;
    border-radius: 99px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--warn);
    transition: width 0.25s linear;
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: auto;
  }
  .grow {
    flex: 1;
  }
</style>
