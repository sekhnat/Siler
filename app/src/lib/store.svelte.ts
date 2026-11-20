import { listen } from "@tauri-apps/api/event";
import { api, errorText, MAIN_QUEUE, type Download, type EngineEvent, type Queue } from "./api";

const HISTORY = 90; // samples at ~4 Hz ≈ 22 s

class DownloadStore {
  items = $state<Record<number, Download>>({});
  totalSpeed = $state(0);
  history = $state<number[]>(new Array(HISTORY).fill(0));
  perHistory = $state<Record<number, number[]>>({});
  loaded = $state(false);
  queues = $state<Queue[]>([]);

  queueName(id: number): string {
    return this.queues.find((q) => q.id === id)?.name ?? "Main queue";
  }

  list = $derived(Object.values(this.items).sort((a, b) => b.id - a.id));

  async init() {
    await listen<EngineEvent>("engine", (e) => this.apply(e.payload));
    await listen("engine-resync", () => this.reload());
    await this.reload();
  }

  async reload() {
    const [list, queues] = await Promise.all([api.list(), api.queues()]);
    this.queues = queues;
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
      case "removed":
        delete this.items[ev.id];
        delete this.perHistory[ev.id];
        break;
      case "progress": {
        for (const p of ev.items) {
          const d = this.items[p.id];
          if (!d) continue;
          d.downloaded = p.downloaded;
          d.speed = p.speed;
          d.active_connections = p.active_connections;
          d.segments = p.segments;
          if (p.media !== undefined) d.media_progress = p.media;
          if (p.total_size != null) d.total_size = p.total_size;
          const h = this.perHistory[p.id] ?? new Array(HISTORY).fill(0);
          h.push(p.speed);
          if (h.length > HISTORY) h.shift();
          this.perHistory[p.id] = h;
        }
        // Downloads that stopped running no longer report a speed.
        const running = new Set(ev.items.map((p) => p.id));
        for (const d of Object.values(this.items)) {
          if (d.speed && !running.has(d.id)) d.speed = 0;
        }
        this.totalSpeed = ev.total_speed;
        this.history.push(ev.total_speed);
        if (this.history.length > HISTORY) this.history.shift();
        break;
      }
    }
  }
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
    this.items = this.items.filter((t) => t.id !== id);
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
