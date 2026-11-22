// What a download looks like in the list, worked out once for both views (cards and
// details): its progress, its size line, its status in words.
import type { Download } from "./api";
import { STATUS_COLOR, STATUS_LABEL } from "./categories";
import { bytes, clock, knownSize, percent } from "./format";
import { has, t } from "./i18n";

type MediaProgress = NonNullable<Download["media_progress"]>;

/** A media download's progress is its phase; while it downloads, its bytes when the pieces'
 * sizes are known, else its pieces. */
function mediaPct(d: Download, m: MediaProgress): number {
  switch (m.phase) {
    case "downloading":
      if (m.bytes_total) return 88 * Math.min(1, d.downloaded / m.bytes_total);
      return m.segments_total ? (88 * m.segments_done) / m.segments_total : 0;
    case "rebuilding":
      return 88 + (m.permille / 1000) * 10;
    case "publishing":
      return 99;
    default:
      return 0;
  }
}

/** Progress in percent (0–100). */
export function progressOf(d: Download): number {
  if (d.status === "completed") return 100;
  const mp = d.media_progress;
  return mp ? mediaPct(d, mp) : percent(knownSize(d), d.downloaded);
}

/** What a media download is doing, for its size line; null for a file. */
export function phaseText(d: Download): string | null {
  const mp = d.media_progress;
  if (!mp) return null;
  switch (mp.phase) {
    case "waiting":
      return t("rowview-waiting-for-the-site");
    case "resolving":
      return t("rowview-reading-the-stream");
    case "recording":
      return mp.gaps
        ? t("rowview-recording-line-gaps", { time: clock(mp.recorded_secs ?? 0), size: bytes(d.downloaded), gaps: mp.gaps })
        : t("rowview-recording-line", { time: clock(mp.recorded_secs ?? 0), size: bytes(d.downloaded) });
    case "downloading":
      if (mp.bytes_total) return t("rowview-of", { done: bytes(d.downloaded), total: bytes(mp.bytes_total) });
      return d.downloaded
        ? t("rowview-pieces-size", { done: mp.segments_done, total: mp.segments_total, size: bytes(d.downloaded) })
        : t("rowview-pieces", { done: mp.segments_done, total: mp.segments_total });
    case "rebuilding":
      return t("rowview-rebuilding", { pct: (mp.permille / 10).toFixed(0) });
    default:
      return t("rowview-saving");
  }
}

/** Checking a checksum or unpacking, after the transfer. */
export function finishingText(d: Download): string | null {
  const p = d.phase;
  if (!p || d.status === "completed") return null;
  const pct = p.total ? Math.floor((100 * p.done) / p.total) : null;
  if (p.name === "verifying") return pct == null ? t("rowview-checking-the-checksum") : t("rowview-checking-the-checksum-pct", { pct });
  return pct == null ? t("rowview-unpacking") : t("rowview-unpacking-pct", { pct });
}

/** The size line: "12 MB of 40 MB", "Recording · 1:02 · 9 MB", … */
export function sizeText(d: Download): string {
  if (d.status === "completed") return bytes(d.total_size);
  const f = finishingText(d);
  if (f) return f;
  const p = phaseText(d);
  if (p) return p;
  if (d.media) return d.downloaded ? t("rowview-so-far", { size: bytes(d.downloaded) }) : t("rowview-not-started");
  if (d.total_size) return t("rowview-of", { done: bytes(d.downloaded), total: bytes(d.total_size) });
  return d.downloaded ? bytes(d.downloaded) : t("rowview-unknown-size");
}

/** The status in words, including what it waits for. */
export function statusText(d: Download): string {
  if (d.needs_host_key) return t("rowview-check-server-key");
  if (d.needs_login) return t("rowview-needs-login");
  if (d.media_progress?.phase === "recording" && d.status === "downloading") return t("rowview-recording");
  return STATUS_LABEL[d.status];
}

export function statusColor(d: Download): string {
  return d.needs_login || d.needs_host_key ? "var(--accent)" : STATUS_COLOR[d.status];
}

/** A recording that can still be finished. */
export function isRecording(d: Download): boolean {
  return !!d.media?.live && !d.media.finished && d.status !== "completed" && d.status !== "failed";
}

/** Seconds left at the current speed, or null when it can't be told. */
export function secondsLeft(d: Download): number | null {
  if (d.status !== "downloading" || d.speed <= 0) return null;
  if (d.media_progress?.phase === "recording") return null;
  const total = d.media ? (d.media_progress?.bytes_total ?? null) : knownSize(d);
  if (!total) return null;
  return Math.max(0, (total - d.downloaded) / d.speed);
}

/** A download's error in the interface's language (its code from the catalog), else as the
 * engine wrote it. */
export function errorOf(d: Download): string {
  const c = d.error_code;
  if (c && has(`error-${c.code}`)) {
    // A whole number stays a number (an HTTP status selects its words).
    const args = Object.fromEntries(c.args.map(([k, v]) => [k, /^(0|[1-9]\d*)$/.test(v) ? Number(v) : v]));
    return t(`error-${c.code}`, args);
  }
  return d.error ?? "";
}
