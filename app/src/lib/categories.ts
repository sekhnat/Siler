import type { Component } from "svelte";
import { FileArchive, FileText, Film, Image, Music, Package, File } from "@lucide/svelte";
import type { Category, CategoryRule, Status } from "./api";

export interface CategoryMeta {
  key: Category;
  label: string;
  icon: Component<any>;
  color: string;
}

export const CATEGORIES: CategoryMeta[] = [
  { key: "video", label: "Video", icon: Film, color: "#f472b6" },
  { key: "music", label: "Music", icon: Music, color: "#a78bfa" },
  { key: "images", label: "Images", icon: Image, color: "#34d399" },
  { key: "documents", label: "Documents", icon: FileText, color: "#60a5fa" },
  { key: "compressed", label: "Compressed", icon: FileArchive, color: "#fbbf24" },
  { key: "programs", label: "Programs", icon: Package, color: "#22d3ee" },
  { key: "other", label: "Other", icon: File, color: "#94a3b8" },
];

export const categoryMeta = (c: Category): CategoryMeta =>
  CATEGORIES.find((x) => x.key === c) ?? CATEGORIES[CATEGORIES.length - 1];

export const STATUS_LABEL: Record<Status, string> = {
  waiting: "Waiting",
  queued: "Queued",
  probing: "Connecting",
  downloading: "Downloading",
  paused: "Paused",
  completed: "Completed",
  failed: "Failed",
};

export const isActive = (s: Status) => s === "probing" || s === "downloading";

export const STATUS_COLOR: Record<Status, string> = {
  downloading: "var(--accent)",
  probing: "var(--accent)",
  completed: "var(--ok)",
  paused: "var(--warn)",
  failed: "var(--err)",
  queued: "var(--queued)",
  waiting: "var(--queued)",
};

/** Not started yet, but on the list to be downloaded. */
export const isWaiting = (s: Status) => s === "queued" || s === "waiting";
export const canResume = (s: Status) => s === "paused" || s === "failed" || s === "waiting";
export const canPause = (s: Status) => isActive(s) || isWaiting(s);

/** The category a file name falls in, given the user's extension lists. */
export function categoryOf(name: string, rules: CategoryRule[] | undefined): Category {
  if (!rules) return "other";
  const lower = name.toLowerCase().split(/[?#]/)[0];
  const m = lower.match(/\.(tar\.(?:gz|bz2|xz|zst)|[a-z0-9]{1,10})$/);
  if (!m) return "other";
  const ext = m[1];
  const last = ext.split(".").pop()!;
  for (const e of [ext, last]) {
    const hit = rules.find((r) => r.category !== "other" && r.extensions.includes(e));
    if (hit) return hit.category;
  }
  return "other";
}
