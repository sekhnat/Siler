import { t } from "./i18n";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { api, errorText, MAIN_QUEUE, type Download, type EngineEvent, type GrabProgress, type Queue, type QuotaStatus } from "./api";

const HISTORY = 90; // samples at ~4 Hz ≈ 22 s

class DownloadStore {
  items = $state<Record<number, Download>>({});
  totalSpeed = $state(0);
  // Replaced whole on each sample: one change for the graphs instead of one per point.
  history = $state.raw<number[]>(new Array(HISTORY).fill(0));
  perHistory = $state.raw<Record<number, number[]>>({});
  loaded = $state(false);
  queues = $state<Queue[]>([]);
  quota = $state<QuotaStatus | null>(null);
  /** The latest progress of each site grab, by run. */
  grabs = $state<Record<number, GrabProgress>>({});

  queueName(id: number): string {
    return this.queues.find((q) => q.id === id)?.name ?? t("store-main-queue");
  }

  list = $derived(Object.values(this.items).sort((a, b) => b.id - a.id));

  /** The newest progress not shown yet, and the frame that will show it. */
  private pending: ProgressEvent | null = null;
  private frame = 0;

  async init() {
    // This window's own events: Siler sends progress only to the windows on screen.
    const win = getCurrentWebviewWindow();
    await win.listen<EngineEvent>("engine", (e) => this.receive(e.payload));
    await win.listen("engine-resync", () => this.reload());
    document.addEventListener("visibilitychange", () => {
      if (!document.hidden) this.flush();
    });
    await this.reload();
  }

  private receive(ev: EngineEvent) {
    if (ev.type === "progress") {
      // Only the newest progress counts: shown once per frame, and not at all while the
      // window can't be seen.
      this.pending = ev;
      if (!document.hidden && !this.frame) {
        this.frame = requestAnimationFrame(() => {
          this.frame = 0;
          this.flush();
        });
      }
      return;
    }
    // Everything else lands after the progress that came before it.
    this.flush();
    this.apply(ev);
  }

  private flush() {
    const ev = this.pending;
    this.pending = null;
    if (ev) this.apply(ev);
  }

  async reload() {
    const [list, queues, quota] = await Promise.all([api.list(), api.queues(), api.quotaStatus().catch(() => null)]);
    this.queues = queues;
    this.quota = quota;
    const next: Record<number, Download> = {};
    for (const d of list) next[d.id] = d;
    this.items = next;
    this.loaded = true;
  }

  private apply(ev: EngineEvent) {
    switch (ev.type) {
      case "upsert":
        this.items[ev.download.id] = ev.download;
        break;
      case "queues":
        this.queues = ev.queues;
        break;
      case "quota": {
        const { type: _t, ...q } = ev;
        this.quota = q;
        break;
      }
      case "grab": {
        const { type: _t, ...g } = ev;
        this.grabs[g.run_id] = g;
        break;
      }
      case "removed": {
        delete this.items[ev.id];
        const { [ev.id]: _gone, ...rest } = this.perHistory;
        this.perHistory = rest;
        break;
      }
      case "progress": {
        const per = { ...this.perHistory };
        for (const p of ev.items) {
          const d = this.items[p.id];
          if (!d) continue;
          // Unchanged values are left alone, so what shows them isn't redrawn.
          if (d.downloaded !== p.downloaded) d.downloaded = p.downloaded;
          if (d.speed !== p.speed) d.speed = p.speed;
          if (d.active_connections !== p.active_connections) d.active_connections = p.active_connections;
          if (!sameSegments(d.segments, p.segments)) d.segments = p.segments;
          if (!sameList(d.conns, p.conns)) d.conns = p.conns;
          if (p.media !== undefined) d.media_progress = p.media;
          if ((d.phase ?? null) !== null || p.phase) d.phase = p.phase ?? null;
          if (p.source_bytes)
            p.source_bytes.forEach((b, i) => d.sources[i] && d.sources[i].bytes !== b && (d.sources[i].bytes = b));
          if (p.total_size != null && d.total_size !== p.total_size) d.total_size = p.total_size;
          if (p.traffic && !sameTraffic(d.traffic, p.traffic)) d.traffic = p.traffic;
          if (p.protocol && d.protocol !== p.protocol) d.protocol = p.protocol;
          per[p.id] = sample(per[p.id], p.speed);
        }
        this.perHistory = per;
        // Downloads that stopped running no longer report a speed.
        const running = new Set(ev.items.map((p) => p.id));
        for (const d of Object.values(this.items)) {
          if (d.speed && !running.has(d.id)) d.speed = 0;
        }
        this.totalSpeed = ev.total_speed;
        this.history = sample(this.history, ev.total_speed);
        break;
      }
    }
  }
}

type ProgressEvent = Extract<EngineEvent, { type: "progress" }>;

/** A speed graph with `value` added and its oldest point dropped. */
function sample(points: number[] | undefined, value: number): number[] {
  const old = points ?? new Array(HISTORY).fill(0);
  return [...old.slice(old.length >= HISTORY ? 1 : 0), value];
}

function sameSegments(a: Download["segments"] | undefined, b: Download["segments"]): boolean {
  if (!a || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i].start !== b[i].start || a[i].end !== b[i].end || a[i].pos !== b[i].pos) return false;
  }
  return true;
}

function sameList<T>(a: T[] | undefined, b: T[]): boolean {
  return !!a && a.length === b.length && a.every((x, i) => x === b[i]);
}

function sameTraffic(a: Download["traffic"] | undefined, b: NonNullable<Download["traffic"]>): boolean {
  return !!a && a.wire === b.wire && a.wasted === b.wasted && a.retries === b.retries;
}

export const store = new DownloadStore();
export { MAIN_QUEUE };

// ---------------------------------------------------------------- toasts

export interface Toast {
  id: number;
  kind: "info" | "error" | "success";
  text: string;
}

class Toasts {
  items = $state<Toast[]>([]);
  private next = 1;

  show(text: string, kind: Toast["kind"] = "info") {
    const id = this.next++;
    this.items.push({ id, kind, text });
    setTimeout(() => this.dismiss(id), kind === "error" ? 6000 : 3500);
  }

  error(e: unknown) {
    this.show(e instanceof Error ? e.message : errorText(e), "error");
  }

  dismiss(id: number) {
    this.items = this.items.filter((x) => x.id !== id);
  }
}

export const toasts = new Toasts();

/** Runs an API call, reporting failures as a toast. */
export async function attempt<T>(p: Promise<T>): Promise<T | undefined> {
  try {
    return await p;
  } catch (e) {
    toasts.error(e);
    return undefined;
  }
}
