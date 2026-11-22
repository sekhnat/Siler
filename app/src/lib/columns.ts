import { t } from "./i18n";
// The list's sort orders and the Details view's columns.
import type { ColumnSetting, Download } from "./api";
import { nameFromUrl } from "./format";
import { progressOf, secondsLeft } from "./rowview";

export type SortKey = "added" | "name" | "size" | "progress" | "status" | "queue" | "speed" | "eta" | "host" | "type" | "finished" | "connections";

export const SORTS: { label: string; value: SortKey }[] = [
  { label: t("columns-date-added"), value: "added" },
  { label: t("columns-name"), value: "name" },
  { label: t("columns-size"), value: "size" },
  { label: t("columns-progress"), value: "progress" },
  { label: t("columns-status"), value: "status" },
  { label: t("columns-queue-order"), value: "queue" },
  { label: t("columns-speed"), value: "speed" },
  { label: t("columns-time-left"), value: "eta" },
  { label: t("columns-server"), value: "host" },
  { label: t("columns-type"), value: "type" },
  { label: t("columns-finished"), value: "finished" },
  { label: t("columns-connections"), value: "connections" },
];

export const isSortKey = (v: unknown): v is SortKey => SORTS.some((s) => s.value === v);

export const nameOf = (d: Download) => d.filename ?? nameFromUrl(d.url);
const hostOf = (d: Download) => {
  try {
    return new URL(d.final_url ?? d.url).hostname;
  } catch {
    return "";
  }
};
const STATUS_ORDER: Record<Download["status"], number> = {
  downloading: 0,
  probing: 1,
  queued: 2,
  waiting: 3,
  paused: 4,
  failed: 5,
  completed: 6,
};
const text = (a: string, b: string) => a.localeCompare(b, undefined, { numeric: true, sensitivity: "base" });
const unknownLast = (a: number | null, b: number | null) => (a == null ? (b == null ? 0 : 1) : b == null ? -1 : a - b);

/** Each sort in its natural order (newest, largest, fastest first; names A–Z). */
export const COMPARE: Record<SortKey, (a: Download, b: Download) => number> = {
  added: (a, b) => b.id - a.id,
  name: (a, b) => text(nameOf(a), nameOf(b)),
  size: (a, b) => (b.total_size ?? -1) - (a.total_size ?? -1),
  progress: (a, b) => progressOf(b) - progressOf(a),
  status: (a, b) => STATUS_ORDER[a.status] - STATUS_ORDER[b.status] || b.id - a.id,
  queue: (a, b) => +(a.status === "completed") - +(b.status === "completed") || a.position - b.position,
  speed: (a, b) => b.speed - a.speed || b.id - a.id,
  eta: (a, b) => unknownLast(secondsLeft(a), secondsLeft(b)) || b.id - a.id,
  host: (a, b) => text(hostOf(a), hostOf(b)) || b.id - a.id,
  type: (a, b) => text(a.category, b.category) || text(nameOf(a), nameOf(b)),
  finished: (a, b) => (b.completed_at ?? 0) - (a.completed_at ?? 0) || b.id - a.id,
  connections: (a, b) => b.active_connections - a.active_connections || b.id - a.id,
};

export function sorter(key: SortKey, reverse: boolean): (a: Download, b: Download) => number {
  const c = COMPARE[key];
  return reverse ? (a, b) => c(b, a) : c;
}

export interface ColumnDef {
  id: string;
  label: string;
  sort: SortKey;
  align?: "right";
  /** The width a new column gets. */
  width: number;
}

export const COLUMNS: ColumnDef[] = [
  { id: "name", label: t("columns-name"), sort: "name", width: 220 },
  { id: "size", label: t("columns-size"), sort: "size", align: "right", width: 90 },
  { id: "progress", label: t("columns-progress"), sort: "progress", width: 150 },
  { id: "speed", label: t("columns-speed"), sort: "speed", align: "right", width: 90 },
  { id: "eta", label: t("columns-time-left"), sort: "eta", align: "right", width: 80 },
  { id: "status", label: t("columns-status"), sort: "status", width: 110 },
  { id: "queue", label: t("columns-queue"), sort: "queue", width: 110 },
  { id: "host", label: t("columns-server"), sort: "host", width: 150 },
  { id: "type", label: t("columns-type"), sort: "type", width: 100 },
  { id: "added", label: t("columns-added"), sort: "added", width: 140 },
  { id: "finished", label: t("columns-finished"), sort: "finished", width: 140 },
  { id: "connections", label: t("columns-connections"), sort: "connections", align: "right", width: 100 },
];

export const columnDef = (id: string) => COLUMNS.find((c) => c.id === id);

/** The columns to show: known ones only, the name always first. */
export function shownColumns(cols: ColumnSetting[] | undefined): ColumnSetting[] {
  const known = (cols ?? []).filter((c) => columnDef(c.id));
  if (!known.some((c) => c.id === "name")) known.unshift({ id: "name", width: 220 });
  return known;
}

export type ToolbarItem = "queue" | "resume_all" | "pause_all" | "when_done" | "sort" | "view" | "batch" | "grabber" | "import" | "settings" | "more";

export const TOOLBAR_ITEMS: { id: ToolbarItem; label: string }[] = [
  { id: "queue", label: t("columns-start-stop-queue") },
  { id: "resume_all", label: t("columns-resume-all") },
  { id: "pause_all", label: t("columns-pause-all") },
  { id: "when_done", label: t("columns-when-downloads-finish") },
  { id: "sort", label: t("columns-sort") },
  { id: "view", label: t("columns-cards-details") },
  { id: "batch", label: t("columns-batch-download") },
  { id: "grabber", label: t("columns-site-grabber") },
  { id: "import", label: t("columns-import-list") },
  { id: "settings", label: t("columns-settings") },
  { id: "more", label: t("columns-more") },
];

export const DEFAULT_TOOLBAR: ToolbarItem[] = ["queue", "resume_all", "pause_all", "when_done", "sort", "view", "more"];
