<script lang="ts">
  import { Film, Info, TriangleAlert } from "@lucide/svelte";
  import type { ContainerChoice, MediaProbe, Selection, SubtitleMode, TrackView } from "../api";
  import { bytes, duration as formatDuration } from "../format";

  let {
    probe,
    selection = $bindable(),
    container = $bindable(),
    subtitles = $bindable(),
    ffmpeg,
  }: {
    probe: MediaProbe;
    selection: Selection;
    container: ContainerChoice;
    subtitles: SubtitleMode;
    ffmpeg: boolean;
  } = $props();

  const stream = $derived(probe.kind === "hls" || probe.kind === "dash");
  const KIND_LABEL: Record<string, string> = { hls: "HLS stream", dash: "DASH stream", direct: "File", not_media: "Not media", live: "Live stream", drm: "Protected", unsupported: "Unsupported" };

  // Higher resolution first, then the higher bitrate.
  const choices = $derived(
    [...probe.variants].sort(
      (a, b) => (b.video.resolution?.[1] ?? 0) - (a.video.resolution?.[1] ?? 0) || (b.video.bandwidth ?? 0) - (a.video.bandwidth ?? 0),
    ),
  );
  const chosen = $derived(probe.variants.find((v) => v.video.id === selection.video) ?? null);
  const audioChoices = $derived(chosen ? probe.audio.filter((t) => chosen.audio.includes(t.id)) : probe.audio);
  const subtitleChoices = $derived(chosen ? probe.subtitles.filter((t) => chosen.subtitles.includes(t.id)) : probe.subtitles);
  const trackCount = $derived((selection.video ? 1 : 0) + selection.audio.length + selection.subtitles.length);
  const estimate = $derived.by(() => {
    const all = [...probe.variants.map((v) => v.video), ...probe.audio, ...probe.subtitles];
    const picked = all.filter((t) => t.id === selection.video || selection.audio.includes(t.id) || selection.subtitles.includes(t.id));
    const sizes = picked.map((t) => t.estimated_bytes).filter((n): n is number => n != null);
    return sizes.length ? sizes.reduce((a, b) => a + b, 0) : null;
  });

  function pickVideo(v: TrackView["id"]) {
    const variant = probe.variants.find((x) => x.video.id === v);
    selection = {
      video: v,
      audio: selection.audio.filter((a) => variant?.audio.includes(a)),
      subtitles: selection.subtitles.filter((s) => variant?.subtitles.includes(s)),
    };
  }

  function toggle(kind: "audio" | "subtitles", id: string) {
    const list = selection[kind];
    selection = { ...selection, [kind]: list.includes(id) ? list.filter((x) => x !== id) : [...list, id] };
  }

  // The original format is the track's own container: only possible for a single track.
  const originalAllowed = $derived(trackCount === 1);
</script>

{#if !stream}
  <div class="explain">
    {#if probe.kind === "drm"}<Film size={18} />{:else}<Info size={18} />{/if}
    <div class="min">
      <div class="t">{KIND_LABEL[probe.kind] ?? "Not downloadable"}</div>
      <div class="hint">{probe.message ?? "Siler can't download this."}</div>
      {#if probe.dropped_drm > 0}<div class="hint">{probe.dropped_drm} track{probe.dropped_drm === 1 ? "" : "s"} refused.</div>{/if}
    </div>
  </div>
{:else}
  <div class="picker">
    <div class="head">
      <span class="pill" style:--c="var(--accent)">{KIND_LABEL[probe.kind]}</span>
      {#if probe.title}<span class="title truncate" title={probe.title}>{probe.title}</span>{/if}
      {#if probe.duration}<span class="hint num">{formatDuration(probe.duration)}</span>{/if}
      <span class="grow"></span>
      {#if estimate != null}<span class="hint num">About {bytes(estimate)}</span>{/if}
    </div>

    {#if choices.length}
      <div class="group">
        <div class="label">Video</div>
        <div class="list" role="radiogroup" aria-label="Video quality">
          {#each choices as v (v.video.id)}
            <label class="opt" class:on={selection.video === v.video.id}>
              <input type="radio" name="video" checked={selection.video === v.video.id} onchange={() => pickVideo(v.video.id)} />
              <span class="truncate">{v.video.label}</span>
              {#if v.video.estimated_bytes}<span class="hint num">{bytes(v.video.estimated_bytes)}</span>{/if}
            </label>
          {/each}
        </div>
      </div>
    {/if}

    {#if audioChoices.length}
      <div class="group">
        <div class="label">Audio</div>
        <div class="list">
          {#each audioChoices as a (a.id)}
            <label class="opt" class:on={selection.audio.includes(a.id)}>
              <input type="checkbox" checked={selection.audio.includes(a.id)} onchange={() => toggle("audio", a.id)} />
              <span class="truncate">{a.label}</span>
              {#if a.is_default}<span class="hint">default</span>{/if}
            </label>
          {/each}
        </div>
      </div>
    {/if}

    {#if subtitleChoices.length}
      <div class="group">
        <div class="label">Subtitles</div>
        <div class="list">
          {#each subtitleChoices as s (s.id)}
            <label class="opt" class:on={selection.subtitles.includes(s.id)}>
              <input type="checkbox" checked={selection.subtitles.includes(s.id)} onchange={() => toggle("subtitles", s.id)} />
              <span class="truncate">{s.label}</span>
            </label>
          {/each}
        </div>
      </div>
    {/if}

    <div class="row">
      <label class="field">
        <span class="label">Container</span>
        <select class="input" bind:value={container}>
          <option value="auto">Automatic</option>
          <option value="mp4">MP4</option>
          <option value="mkv">MKV</option>
          <option value="original" disabled={!originalAllowed}>Original format{originalAllowed ? "" : " (one track only)"}</option>
        </select>
      </label>
      {#if selection.subtitles.length}
        <label class="field">
          <span class="label">Subtitles in the file</span>
          <select class="input" bind:value={subtitles}>
            <option value="embed">Inside the video</option>
            <option value="sidecar">As a separate file</option>
            <option value="none">Leave out</option>
          </select>
        </label>
      {/if}
    </div>

    {#if !ffmpeg && trackCount > 1}
      <div class="warn"><TriangleAlert size={14} /> Combining video and audio needs FFmpeg, which Siler couldn't find. See Settings → Media.</div>
    {/if}
    {#each probe.warnings as w, i (i)}
      <div class="hint">{w}</div>
    {/each}
  </div>
{/if}

<style>
  .explain {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    padding: 14px 16px;
    border-radius: var(--r-lg);
    background: var(--surface-2);
    border: 1px solid var(--border);
    color: var(--text-2);
  }
  .picker {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  .title {
    font-weight: 600;
    min-width: 0;
  }
  .grow {
    flex: 1;
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 160px;
    overflow-y: auto;
  }
  .opt {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 10px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    min-width: 0;
  }
  .opt.on {
    background: var(--selected);
  }
  .opt > span:first-of-type {
    flex: 1;
    min-width: 0;
  }
  .opt input {
    accent-color: var(--accent);
    margin: 0;
  }
  .row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .warn {
    display: flex;
    gap: 8px;
    align-items: center;
    color: var(--warn);
    font-size: 12.5px;
  }
  .min {
    min-width: 0;
  }
  .t {
    font-weight: 600;
    color: var(--text);
  }
</style>
