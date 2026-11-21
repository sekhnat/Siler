<script lang="ts">
  import { Trash2 } from "@lucide/svelte";
  import { onMount } from "svelte";
  import { api, MAIN_QUEUE, type AfterDone, type PowerCaps, type Queue } from "../api";
  import { toasts } from "../store.svelte";
  import Modal from "./Modal.svelte";

  /** Edits `queue`, or makes a new one when it is null. */
  let { queue, onclose }: { queue: Queue | null; onclose: () => void } = $props();

  const blank: Queue = {
    id: 0,
    name: "",
    max_concurrent: 0,
    schedule: { enabled: false, start_at: 2 * 60, stop_at: 7 * 60, days: [true, true, true, true, true, true, true], once: false },
    after_done: "nothing",
    active: false,
  };
  // svelte-ignore state_referenced_locally
  let q = $state<Queue>(structuredClone($state.snapshot(queue ?? blank)) as Queue);
  let caps = $state<PowerCaps>({ sleep: true, hibernate: true, power_off: true });
  let busy = $state(false);
  let confirmDelete = $state(false);
  let nameInput: HTMLInputElement | undefined = $state();

  onMount(async () => {
    if (!queue) nameInput?.focus();
    caps = await api.powerCaps().catch(() => caps);
  });

  const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
  const PRESETS: { label: string; days: boolean[] }[] = [
    { label: "Every day", days: [true, true, true, true, true, true, true] },
    { label: "Weekdays", days: [true, true, true, true, true, false, false] },
    { label: "Weekends", days: [false, false, false, false, false, true, true] },
  ];

  const ACTIONS = $derived<{ value: AfterDone; label: string; ok: boolean }[]>([
    { value: "nothing", label: "Do nothing", ok: true },
    { value: "quit", label: "Quit Siler", ok: true },
    { value: "sleep", label: "Put the computer to sleep", ok: caps.sleep },
    { value: "hibernate", label: "Hibernate the computer", ok: caps.hibernate },
    { value: "power_off", label: "Shut down the computer", ok: caps.power_off },
  ]);

  const hm = (m: number | null) => (m == null ? "" : `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`);
  const minutes = (v: string): number | null => {
    const [h, m] = v.split(":").map(Number);
    return Number.isFinite(h) && Number.isFinite(m) ? h * 60 + m : null;
  };

  const sameDays = (a: boolean[], b: boolean[]) => a.every((v, i) => v === b[i]);
  const overnight = $derived(q.schedule.stop_at != null && q.schedule.start_at != null && q.schedule.stop_at <= q.schedule.start_at);
  const valid = $derived(
    q.name.trim().length > 0 &&
      (!q.schedule.enabled || (q.schedule.start_at != null && (q.schedule.once || q.schedule.days.some(Boolean)))),
  );

  async function save() {
    if (!valid || busy) return;
    busy = true;
    try {
      if (queue) await api.updateQueue(q);
      else await api.createQueue(q);
      onclose();
    } catch (e) {
      toasts.error(e);
    } finally {
      busy = false;
    }
  }

  async function remove() {
    if (!queue) return;
    try {
      await api.deleteQueue(queue.id);
      toasts.show(`Deleted “${queue.name}”; its downloads moved to the main queue`, "success");
      onclose();
    } catch (e) {
      toasts.error(e);
    }
  }
</script>

<Modal title={queue ? "Edit queue" : "New queue"} width={520} {onclose}>
  <div class="form">
    <label class="field">
      <span class="label">Name</span>
      <input bind:this={nameInput} class="input" bind:value={q.name} placeholder="Night downloads" maxlength="60" />
    </label>

    <label class="field">
      <span class="label">Downloads at the same time <b>{q.max_concurrent || "No own limit"}</b></span>
      <input type="range" min="0" max="10" bind:value={q.max_concurrent} />
      <span class="hint">The overall limit in Settings still applies.</span>
    </label>

    <button class="toggle-row" onclick={() => (q.schedule.enabled = !q.schedule.enabled)}>
      <div>
        <div class="t-label">Start on a schedule</div>
        <div class="hint">Siler must be running (in the tray is fine); it can't wake a sleeping computer.</div>
      </div>
      <span class="switch" class:on={q.schedule.enabled}></span>
    </button>

    {#if q.schedule.enabled}
      <div class="sched">
        <div class="times">
          <label class="field">
            <span class="label">Start at</span>
            <input class="input" type="time" value={hm(q.schedule.start_at)} onchange={(e) => (q.schedule.start_at = minutes(e.currentTarget.value))} />
          </label>
          <label class="field">
            <span class="label">
              <input type="checkbox" checked={q.schedule.stop_at != null} onchange={(e) => (q.schedule.stop_at = e.currentTarget.checked ? 7 * 60 : null)} />
              Stop at
            </span>
            <input
              class="input"
              type="time"
              disabled={q.schedule.stop_at == null}
              value={hm(q.schedule.stop_at)}
              onchange={(e) => (q.schedule.stop_at = minutes(e.currentTarget.value))}
            />
          </label>
        </div>
        <div class="hint">
          {#if q.schedule.stop_at == null}
            Runs until everything in the queue has finished.
          {:else if overnight}
            Stops the next morning; downloads that haven't finished wait for the next start.
          {:else}
            Downloads that haven't finished by then wait for the next start.
          {/if}
        </div>

        <div class="field">
          <span class="label">Days</span>
          <div class="days" class:off={q.schedule.once}>
            {#each DAYS as d, i (d)}
              <button class:on={q.schedule.days[i]} disabled={q.schedule.once} onclick={() => (q.schedule.days[i] = !q.schedule.days[i])}>{d}</button>
            {/each}
          </div>
          <div class="presets">
            {#each PRESETS as p (p.label)}
              <button class="chip" class:on={!q.schedule.once && sameDays(q.schedule.days, p.days)} onclick={() => ((q.schedule.days = [...p.days]), (q.schedule.once = false))}>{p.label}</button>
            {/each}
            <button class="chip" class:on={q.schedule.once} onclick={() => (q.schedule.once = !q.schedule.once)}>Only once</button>
          </div>
        </div>
      </div>
    {/if}

    <label class="field">
      <span class="label">When the queue is done</span>
      <select class="input" bind:value={q.after_done}>
        {#each ACTIONS as a (a.value)}
          <option value={a.value} disabled={!a.ok}>{a.label}{a.ok ? "" : " (not allowed on this computer)"}</option>
        {/each}
      </select>
      {#if q.after_done === "sleep" || q.after_done === "hibernate" || q.after_done === "power_off"}
        <span class="hint">Siler counts down for a minute first, so you can cancel.</span>
      {/if}
    </label>
  </div>

  {#snippet footer()}
    {#if queue && queue.id !== MAIN_QUEUE}
      {#if confirmDelete}
        <button class="btn btn-danger" onclick={remove}>Delete “{queue.name}”</button>
      {:else}
        <button class="btn btn-ghost" onclick={() => (confirmDelete = true)}><Trash2 size={15} /> Delete</button>
      {/if}
    {/if}
    <span class="grow"></span>
    <button class="btn btn-ghost" onclick={onclose}>Cancel</button>
    <button class="btn btn-primary" disabled={!valid || busy} onclick={save}>{queue ? "Save" : "Create queue"}</button>
  {/snippet}
</Modal>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .label b {
    float: right;
    color: var(--accent-text);
  }
  .label input {
    accent-color: var(--accent);
    margin: 0 4px 0 0;
    vertical-align: -2px;
  }
  input[type="range"] {
    width: 100%;
    accent-color: var(--accent);
  }
  .toggle-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 2px 0;
    text-align: left;
  }
  .t-label {
    font-weight: 600;
  }
  .sched {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 14px;
    margin-top: -4px;
    border-radius: var(--r-lg);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .sched .hint {
    margin-top: -6px;
  }
  .times {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .days {
    display: flex;
    gap: 4px;
  }
  .days button {
    flex: 1;
    height: 32px;
    border-radius: var(--r-sm);
    font-weight: 600;
    font-size: 12.5px;
    color: var(--text-2);
    background: var(--surface-3);
  }
  .days button.on {
    color: var(--on-accent, #fff);
    background: var(--accent);
  }
  .days.off button {
    opacity: 0.45;
  }
  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    height: 26px;
    padding: 0 10px;
    border-radius: 99px;
    font-size: 12px;
    font-weight: 600;
    color: var(--text-2);
    background: var(--surface-3);
  }
  .chip.on {
    color: var(--accent-text);
    background: var(--selected);
  }
  .grow {
    flex: 1;
  }
</style>
