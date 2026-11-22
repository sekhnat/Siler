import { t } from "./i18n";
import type { Download } from "./api";

const UNITS = ["B", "KB", "MB", "GB", "TB"];

export function bytes(n: number | null | undefined): string {
  if (n == null) return "—";
  if (n < 1024) return t("format-bytes", { n, unit: UNITS[0] });
  let i = 0;
  let v = n;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  return t("format-bytes", { n: v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2), unit: UNITS[i] });
}

export function speed(n: number | null | undefined): string {
  return t("format-per-second", { size: bytes(n ?? 0) });
}

export function duration(secs: number): string {
  if (!isFinite(secs) || secs < 0) return "—";
  secs = Math.round(secs);
  if (secs < 60) return t("format-duration-s", { s: secs });
  const m = Math.floor(secs / 60);
  if (m < 60) return t("format-duration-ms", { m, s: secs % 60 });
  const h = Math.floor(m / 60);
  if (h < 24) return t("format-duration-hm", { h, m: m % 60 });
  return t("format-duration-dh", { d: Math.floor(h / 24), h: h % 24 });
}

export function eta(total: number | null, done: number, bps: number): string {
  if (!total || bps <= 0) return "—";
  return duration((total - done) / bps);
}

export function percent(total: number | null, done: number): number {
  if (!total) return 0;
  return Math.min(100, (done / total) * 100);
}

export function relativeTime(ms: number | null | undefined): string {
  if (!ms) return "—";
  const diff = Date.now() - ms;
  if (diff < 45_000) return t("format-just-now");
  if (diff < 3_600_000) return t("format-minutes-ago", { n: Math.round(diff / 60_000) });
  if (diff < 86_400_000) return t("format-hours-ago", { n: Math.round(diff / 3_600_000) });
  const d = new Date(ms);
  const sameYear = d.getFullYear() === new Date().getFullYear();
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric", year: sameYear ? undefined : "numeric" });
}

export function dateTime(ms: number | null | undefined): string {
  if (!ms) return "—";
  return new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

export function host(url: string | null | undefined): string {
  if (!url) return "";
  try {
    return new URL(url).host.replace(/^www\./, "");
  } catch {
    return "";
  }
}

export function nameFromUrl(url: string): string {
  try {
    const seg = new URL(url).pathname.split("/").filter(Boolean).pop();
    return seg ? decodeURIComponent(seg) : host(url);
  } catch {
    return url;
  }
}

/** The short name of a stream's kind, for a pill. */
export const STREAM_KIND: Record<string, string> = { hls: "HLS", dash: "DASH", hds: "HDS", stream: "RTMP/MMS" };

/** `1:02:03` or `4:05`. */
export function clock(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

export function looksLikeUrl(text: string): boolean {
  return /^(https?|s?ftp|ftps|rtmpt?|mms[ht]?):\/\/\S+$/i.test(text.trim());
}

/** Speed-limit presets in bytes/s; 0 = unlimited. */
export const SPEED_PRESETS = [0, 256 * 1024, 512 * 1024, 1024 ** 2, 2 * 1024 ** 2, 5 * 1024 ** 2, 10 * 1024 ** 2, 25 * 1024 ** 2];

export function limitLabel(bps: number): string {
  return bps ? speed(bps) : t("format-unlimited");
}

/** Every web, FTP or SFTP link in a piece of text (one per line, or separated by spaces). */
export function parseUrls(text: string): string[] {
  const found = text.match(/(https?|s?ftp|ftps|rtmpt?|mms[ht]?):\/\/[^\s"'<>]+/gi) ?? [];
  return [...new Set(found.map((u) => u.replace(/[),.;]+$/, "")))];
}

export const SPEED_UNITS = [
  { label: "KB/s", factor: 1024 },
  { label: "MB/s", factor: 1024 ** 2 },
];

/** "22:00–06:00, weekdays" (empty when the queue has no schedule). */
export function describeSchedule(s: import("./api").Schedule): string {
  if (!s.enabled || s.start_at == null) return "";
  const hm = (m: number) => `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
  const time = s.stop_at == null ? t("format-schedule-from", { at: hm(s.start_at) }) : t("format-schedule-range", { from: hm(s.start_at), to: hm(s.stop_at) });
  const key = s.days.map((d) => (d ? "1" : "0")).join("");
  const names = [t("format-mon"), t("format-tue"), t("format-wed"), t("format-thu"), t("format-fri"), t("format-sat"), t("format-sun")];
  const days = s.once
    ? t("format-once")
    : key === "1111111"
      ? t("format-every-day")
      : key === "1111100"
        ? t("format-weekdays")
        : key === "0000011"
          ? t("format-weekends")
          : names.filter((_, i) => s.days[i]).join(" ");
  return t("format-schedule", { time, days });
}

/** "every hour", "every day"… for keeping a file up to date. */
export function syncLabel(secs: number): string {
  if (!secs) return "";
  if (secs % 604800 === 0) return t("format-every-n-weeks", { n: secs / 604800 });
  if (secs % 86400 === 0) return t("format-every-n-days", { n: secs / 86400 });
  if (secs % 3600 === 0) return t("format-every-n-hours", { n: secs / 3600 });
  return t("format-every-n-minutes", { n: Math.round(secs / 60) });
}

/** A download's size as far as it is known: a video's is what its pieces add up to while
 * they download (its file's size is known once it is written). */
export function knownSize(d: Download): number | null {
  const m = d.media_progress;
  if (d.status !== "completed" && m?.phase === "downloading" && m.bytes_total) return m.bytes_total;
  return d.total_size;
}
