const UNITS = ["B", "KB", "MB", "GB", "TB"];

export function bytes(n: number | null | undefined): string {
  if (n == null) return "—";
  if (n < 1024) return `${n} B`;
  let i = 0;
  let v = n;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2)} ${UNITS[i]}`;
}

export function speed(n: number | null | undefined): string {
  if (!n) return "0 B/s";
  return `${bytes(n)}/s`;
}

export function duration(secs: number): string {
  if (!isFinite(secs) || secs < 0) return "—";
  secs = Math.round(secs);
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m ${secs % 60}s`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ${m % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
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
  if (diff < 45_000) return "just now";
  if (diff < 3_600_000) return `${Math.round(diff / 60_000)} min ago`;
  if (diff < 86_400_000) return `${Math.round(diff / 3_600_000)} h ago`;
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

export function looksLikeUrl(text: string): boolean {
  return /^https?:\/\/\S+$/i.test(text.trim());
}

/** Speed-limit presets in bytes/s; 0 = unlimited. */
export const SPEED_PRESETS = [0, 256 * 1024, 512 * 1024, 1024 ** 2, 2 * 1024 ** 2, 5 * 1024 ** 2, 10 * 1024 ** 2, 25 * 1024 ** 2];

export function limitLabel(bps: number): string {
  return bps ? speed(bps) : "Unlimited";
}

/** Every http(s) link in a piece of text (one per line, or separated by spaces). */
export function parseUrls(text: string): string[] {
  const found = text.match(/https?:\/\/[^\s"'<>]+/gi) ?? [];
  return [...new Set(found.map((u) => u.replace(/[),.;]+$/, "")))];
}

export const SPEED_UNITS = [
  { label: "KB/s", factor: 1024 },
  { label: "MB/s", factor: 1024 ** 2 },
];
