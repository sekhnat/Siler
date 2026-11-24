// One download kept up to date from the engine's events, for a window about just that
// download (its progress window, its "Download complete" window).
import { listen } from "@tauri-apps/api/event";
import { api, type Download, type EngineEvent } from "./api";

export class LiveDownload {
  d = $state<Download | null>(null);
  gone = $state(false);

  constructor(readonly id: number) {}

  async reload() {
    try {
      this.d = await api.get(this.id);
    } catch {
      this.gone = true;
    }
  }

  /** Follows the download until the returned function is called (`reload` reads it first). */
  start(): () => void {
    const stops = [
      listen<EngineEvent>("engine", (e) => this.apply(e.payload)),
      listen("engine-resync", () => this.reload()),
      // Back from the tray: progress was skipped meanwhile.
      listen("progress-shown", () => this.reload()),
    ];
    return () => stops.forEach((s) => s.then((f) => f()));
  }

  private apply(ev: EngineEvent) {
    const d = this.d;
    switch (ev.type) {
      case "upsert":
        if (ev.download.id === this.id) this.d = ev.download;
        break;
      case "removed":
        if (ev.id === this.id) this.gone = true;
        break;
      case "progress": {
        if (!d) return;
        const p = ev.items.find((i) => i.id === this.id);
        if (!p) {
          if (d.speed) d.speed = 0;
          return;
        }
        d.downloaded = p.downloaded;
        d.speed = p.speed;
        d.active_connections = p.active_connections;
        d.segments = p.segments;
        d.conns = p.conns;
        if (p.media !== undefined) d.media_progress = p.media;
        d.phase = p.phase ?? null;
        if (p.total_size != null) d.total_size = p.total_size;
        break;
      }
    }
  }
}
