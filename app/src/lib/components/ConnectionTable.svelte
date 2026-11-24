<script lang="ts">
  // IDM's table of connections: each segment, what its connection has fetched, and what it
  // is doing now.
  import { t } from "../i18n";
  import type { ConnState, Download } from "../api";
  import { bytes } from "../format";

  let { d }: { d: Download } = $props();

  function info(state: ConnState | undefined): string {
    switch (state) {
      case "waiting":
        return t("progress-conn-waiting");
      case "connecting":
        return t("progress-conn-connecting");
      case "receiving":
        return t("progress-conn-receiving");
      case "retrying":
        return t("progress-conn-retrying");
      case "finished":
        return t("progress-conn-finished");
      default:
        if (d.status === "paused") return t("progress-conn-paused");
        if (d.status === "failed") return t("progress-conn-stopped");
        return "";
    }
  }

  /** A download whose size isn't known has one stream and no segments. */
  const rows = $derived(
    d.segments.length
      ? [...d.segments.keys()]
          .sort((a, b) => d.segments[a].start - d.segments[b].start)
          .map((i) => ({ got: d.segments[i].pos - d.segments[i].start, info: info(d.conns?.[i]), live: d.conns?.[i] === "receiving" }))
      : [
          {
            got: d.downloaded,
            info:
              d.status === "downloading"
                ? t("progress-conn-receiving")
                : d.status === "probing"
                  ? t("progress-conn-connecting")
                  : d.status === "completed"
                    ? t("progress-conn-finished")
                    : info(undefined),
            live: d.status === "downloading",
          },
        ],
  );
</script>

<div class="table" role="table" aria-label={t("progress-connections")}>
  <div class="head" role="row">
    <span role="columnheader">{t("progress-col-n")}</span>
    <span role="columnheader">{t("progress-col-downloaded")}</span>
    <span role="columnheader">{t("progress-col-info")}</span>
  </div>
  <div class="body">
    {#each rows as r, i (i)}
      <div class="row" role="row" class:live={r.live}>
        <span class="num" role="cell">{i + 1}</span>
        <span class="num" role="cell">{bytes(r.got)}</span>
        <span class="truncate" role="cell">{r.info}</span>
      </div>
    {/each}
  </div>
</div>

<style>
  .table {
    display: flex;
    flex-direction: column;
    min-height: 0;
    flex: 1;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-sm);
    overflow: hidden;
    font-size: 12px;
  }
  .head,
  .row {
    display: grid;
    grid-template-columns: 44px 130px 1fr;
    gap: 8px;
    padding: 4px 10px;
  }
  .head {
    background: var(--surface-2);
    border-bottom: 1px solid var(--border);
    color: var(--muted);
    font-weight: 600;
  }
  .body {
    overflow-y: auto;
    min-height: 0;
    flex: 1;
  }
  .row:nth-child(even) {
    background: var(--surface-2);
  }
  .row {
    color: var(--text-2);
  }
  .row.live {
    color: var(--text);
  }
  .num {
    font-variant-numeric: tabular-nums;
  }
</style>
