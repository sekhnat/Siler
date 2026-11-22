<script lang="ts">
  import { t } from "../i18n";
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

  const DAYS = [t("queue-mon"), t("queue-tue"), t("queue-wed"), t("queue-thu"), t("queue-fri"), t("queue-sat"), t("queue-sun")];
  const PRESETS: { label: string; days: boolean[] }[] = [
    { label: t("queue-every-day"), days: [true, true, true, true, true, true, true] },
    { label: t("queue-weekdays"), days: [true, true, true, true, true, false, false] },
    { label: t("queue-weekends"), days: [false, false, false, false, false, true, true] },
  ];

  const ACTIONS = $derived<{ value: AfterDone; label: string; ok: boolean }[]>([
    { value: "nothing", label: t("queue-do-nothing"), ok: true },
    { value: "quit", label: t("queue-quit-siler"), ok: true },
    { value: "sleep", label: t("queue-put-the-computer-to-sleep"), ok: caps.sleep },
    { value: "hibernate", label: t("queue-hibernate-the-computer"), ok: caps.hibernate },
    { value: "power_off", label: t("queue-shut-down-the-computer"), ok: caps.power_off },
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
      toasts.show(t("queue-deleted", { name: queue.name }), "success");
      onclose();
    } catch (e) {
      toasts.error(e);
    }
  }
</script>

<Modal title={queue ? t("queue-edit-queue") : t("queue-new-queue")} width={520} {onclose}>
  <div class="form">
    <label class="field">
      <span class="label">{t("queue-name")}</span>
      <input bind:this={nameInput} class="input" bind:value={q.name} placeholder={t("queue-night-downloads")} maxlength="60" />
    </label>

    <label class="field">
      <span class="label">{t("queue-downloads-at-the-same-time")} <b>{q.max_concurrent || t("queue-no-own-limit")}</b></span>
      <input type="range" min="0" max="10" bind:value={q.max_concurrent} />
      <span class="hint">{t("queue-the-overall-limit-in-settings-still")}</span>
    </label>

    <button class="toggle-row" onclick={() => (q.schedule.enabled = !q.schedule.enabled)}>
      <div>
        <div class="t-label">{t("queue-start-on-a-schedule")}</div>
        <div class="hint">{t("queue-siler-must-be-running-in-the")}</div>
      </div>
      <span class="switch" class:on={q.schedule.enabled}></span>
    </button>

    {#if q.schedule.enabled}
      <div class="sched">
        <div class="times">
          <label class="field">
            <span class="label">{t("queue-start-at")}</span>
            <input class="input" type="time" value={hm(q.schedule.start_at)} onchange={(e) => (q.schedule.start_at = minutes(e.currentTarget.value))} />
          </label>
          <label class="field">
            <span class="label">
              <input type="checkbox" checked={q.schedule.stop_at != null} onchange={(e) => (q.schedule.stop_at = e.currentTarget.checked ? 7 * 60 : null)} />
              {t("queue-stop-at")}
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
            {t("queue-runs-until-everything-in-the-queue")}
          {:else if overnight}
            {t("queue-stops-the-next-morning-downloads-that")}
          {:else}
            {t("queue-downloads-that-haven-t-finished-by")}
          {/if}
        </div>

        <div class="field">
          <span class="label">{t("queue-days")}</span>
          <div class="days" class:off={q.schedule.once}>
            {#each DAYS as d, i (d)}
              <button class:on={q.schedule.days[i]} disabled={q.schedule.once} onclick={() => (q.schedule.days[i] = !q.schedule.days[i])}>{d}</button>
            {/each}
          </div>
          <div class="presets">
            {#each PRESETS as p (p.label)}
              <button class="chip" class:on={!q.schedule.once && sameDays(q.schedule.days, p.days)} onclick={() => ((q.schedule.days = [...p.days]), (q.schedule.once = false))}>{p.label}</button>
            {/each}
            <button class="chip" class:on={q.schedule.once} onclick={() => (q.schedule.once = !q.schedule.once)}>{t("queue-only-once")}</button>
          </div>
        </div>
      </div>
    {/if}

    <label class="field">
      <span class="label">{t("queue-when-the-queue-is-done")}</span>
      <select class="input" bind:value={q.after_done}>
        {#each ACTIONS as a (a.value)}
          <option value={a.value} disabled={!a.ok}>{a.label}{a.ok ? "" : t("queue-not-allowed-on-this-computer")}</option>
        {/each}
      </select>
      {#if q.after_done === "sleep" || q.after_done === "hibernate" || q.after_done === "power_off"}
        <span class="hint">{t("queue-siler-counts-down-for-a-minute")}</span>
      {/if}
    </label>
  </div>

  {#snippet footer()}
    {#if queue && queue.id !== MAIN_QUEUE}
      {#if confirmDelete}
        <button class="btn btn-danger" onclick={remove}>{t("queue-delete", { name: queue.name })}</button>
      {:else}
        <button class="btn btn-ghost" onclick={() => (confirmDelete = true)}><Trash2 size={15} /> {t("queue-delete-2")}</button>
      {/if}
    {/if}
    <span class="grow"></span>
    <button class="btn btn-ghost" onclick={onclose}>{t("queue-cancel")}</button>
    <button class="btn btn-primary" disabled={!valid || busy} onclick={save}>{queue ? t("queue-save") : t("queue-create-queue")}</button>
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
    color: var(--on-accent);
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
