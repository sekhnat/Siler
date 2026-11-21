<script lang="ts">
  import { ChevronDown, CircleCheck, FileArchive, FolderOpen, RotateCcw, Link, LoaderCircle, ShieldCheck, TriangleAlert } from "@lucide/svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";
  import { slide } from "svelte/transition";
  import { ALGO_LABEL, api, errorText, MAIN_QUEUE, type Category, type Checksum, type ContainerChoice, type LoginRequest, type MediaProbe, type Pending, type ProbeError, type ProbeResult, type Selection, type Settings, type SubtitleMode } from "../api";
  import { CATEGORIES, categoryMeta } from "../categories";
  import { bytes, host, looksLikeUrl, nameFromUrl, parseUrls, percent } from "../format";
  import { toasts } from "../store.svelte";
  import FileIcon from "./FileIcon.svelte";
  import LoginForm from "./LoginForm.svelte";
  import MediaPicker from "./MediaPicker.svelte";
  import QueuePicker from "./QueuePicker.svelte";
  import ZipPreview from "./ZipPreview.svelte";

  let {
    initialUrl = "",
    pending = null,
    position = "",
    ondone,
    oncancel,
    onmulti,
  }: {
    initialUrl?: string;
    pending?: Pending | null;
    position?: string;
    ondone: () => void;
    oncancel: () => void;
    /** Several links were pasted: hand them to the batch dialog. */
    onmulti?: (urls: string[]) => void;
  } = $props();

  // Form fields are seeded once from the props.
  // svelte-ignore state_referenced_locally
  let url = $state(pending?.url ?? initialUrl);
  let probe = $state<ProbeResult | null>(null);
  let probing = $state(false);
  let probeError = $state("");
  /** The server wants a login before it says anything about the file. */
  let loginNeeded = $state<LoginRequest | null>(null);
  // svelte-ignore state_referenced_locally
  let filename = $state(pending?.filename ?? "");
  // A folder from the command line (`--dir`) is kept.
  // svelte-ignore state_referenced_locally
  let dir = $state(pending?.dir ?? "");
  // svelte-ignore state_referenced_locally
  let dirEdited = $state(!!pending?.dir);
  // `--queue`: "Download later" is the main action.
  // svelte-ignore state_referenced_locally
  const later = pending?.start_paused ?? false;
  // `--queue=NAME`: the queue to put it in.
  // svelte-ignore state_referenced_locally
  let queueId = $state(pending?.queue_id ?? MAIN_QUEUE);
  let category = $state<Category>("other");
  // A stream (HLS or DASH), or a stream that can't be downloaded: its tracks are the form.
  let mediaProbe = $state<MediaProbe | null>(null);
  let selection = $state<Selection>({ video: null, audio: [], subtitles: [] });
  let container = $state<ContainerChoice>("auto");
  let subtitleMode = $state<SubtitleMode>("embed");
  let settings = $state<Settings | null>(null);
  let connections = $state<number | null>(null);
  // svelte-ignore state_referenced_locally
  let advanced = $state(!!pending?.checksum || !!pending?.mirrors);
  let checksumText = $state("");
  let checksumParsed = $state<Checksum | null>(null);
  let checksumError = $state("");
  let mirrorsText = $state("");
  let zipOpen = $state(false);
  let busy = $state(false);
  let urlInput: HTMLInputElement | undefined = $state();
  // An existing file with the chosen name, and what to do about it.
  let clash = $state<{ suggested: string } | null>(null);
  let choice = $state<"rename" | "overwrite">("rename");

  const valid = $derived(looksLikeUrl(url));
  /** A stream address whose stream couldn't be read: it isn't added as a file instead. */
  const streamFailed = $derived(!mediaProbe && !!probeError && streamLike(url));
  const isStream = $derived(mediaProbe?.kind === "hls" || mediaProbe?.kind === "dash");
  const mediaReady = $derived(isStream && (selection.video !== null || selection.audio.length > 0));
  const resumable = $derived(pending?.resumable ?? null);
  const mirrorLines = $derived(
    mirrorsText
      .split(/\s+/)
      .map((l) => l.trim())
      .filter(Boolean),
  );
  const mirrorsBad = $derived(mirrorLines.filter((l) => !looksLikeUrl(l)));
  /** A ZIP archive on a server that sends parts of files: its contents can be listed. */
  const zipPreviewable = $derived(
    !!probe && probe.resumable && probe.total_size != null && (/\.zip$/i.test(probe.filename) || /zip/i.test(probe.content_type ?? "")),
  );

  // What the typed checksum is.
  $effect(() => {
    const t = checksumText.trim();
    checksumError = "";
    checksumParsed = null;
    if (!t) return;
    let stale = false;
    api
      .parseChecksum(t)
      .then((c) => !stale && (checksumParsed = c))
      .catch((e) => !stale && (checksumError = errorText(e)));
    return () => {
      stale = true;
    };
  });

  onMount(async () => {
    settings = await api.getSettings().catch(() => null);
    if (settings?.on_conflict === "overwrite") choice = "overwrite";
    if (settings) {
      container = settings.default_media_container;
      subtitleMode = settings.default_subtitle_mode;
    }
  });

  function categoryDir(c: Category): string {
    if (!settings) return dir;
    const rule = settings.categories.find((r) => r.category === c);
    if (rule?.folder) return rule.folder.startsWith("/") ? rule.folder : `${settings.download_dir}/${rule.folder}`;
    const label = CATEGORIES.find((x) => x.key === c)?.label ?? "Other";
    return settings.use_category_folders ? `${settings.download_dir}/${label}` : settings.download_dir;
  }

  function pickCategory(c: Category) {
    category = c;
    if (!dirEdited) dir = categoryDir(c);
  }

  /** An HLS or DASH address (by its path, ignoring the query). */
  function streamLike(u: string): boolean {
    return /\.(m3u8|mpd)$/i.test(u.trim().split(/[?#]/)[0]);
  }

  /** A file probe that turned out to be a manifest (by its type or name). */
  function manifestLike(p: ProbeResult): boolean {
    const type = (p.content_type ?? "").split(";")[0].trim().toLowerCase();
    return /mpegurl|dash\+xml/.test(type) || /\.(m3u8|mpd)$/i.test(p.filename);
  }

  /** The name of a site the browser extension recognises. */
  function siteName(site: string): string {
    const names: Record<string, string> = {
      youtube: "YouTube",
      vimeo: "Vimeo",
      facebook: "Facebook",
      instagram: "Instagram",
      tiktok: "TikTok",
    };
    return names[site] ?? site;
  }

  /** Shows a stream's tracks instead of the file form. */
  function showMedia(m: MediaProbe) {
    mediaProbe = m;
    selection = { ...m.default };
    probe = null;
    loginNeeded = null;
    // The manifest's own name ("master.m3u8") says nothing; the stream names the file.
    if (!pending?.keep_name) filename = "";
    category = "video";
    if (!dirEdited) dir = categoryDir("video");
  }

  let seq = 0;
  async function runProbe(target: string) {
    const mine = ++seq;
    probing = true;
    probeError = "";
    try {
      // A stream address is looked at as a stream only: a manifest is never a file to save.
      // Other links are probed as files first, so a plain download isn't fetched twice.
      // The extension may hand over the stream itself, with the page's link.
      const given = !!pending?.stream && target === pending.url;
      if (given || streamLike(target)) {
        const m = await api.probeMedia(target, pending?.id);
        if (mine !== seq) return;
        if (m.kind !== "direct" && m.kind !== "not_media") {
          showMedia(m);
          return;
        }
        if (given) {
          mediaProbe = null;
          probe = null;
          probeError = m.message ?? "The page gave no video K-Star can download.";
          return;
        }
      }
      mediaProbe = null;
      const p = await api.probe(target, pending?.id);
      if (mine !== seq) return;
      if (!streamLike(target) && manifestLike(p)) {
        const m = await api.probeMedia(target, pending?.id).catch(() => null);
        if (mine !== seq) return;
        if (m && m.kind !== "direct" && m.kind !== "not_media") {
          showMedia(m);
          return;
        }
      }
      probe = p;
      loginNeeded = null;
      // A name from the command line, or a video's title the extension found, is kept.
      if (!pending?.keep_name && !(pending?.site && pending.filename)) filename = p.filename;
      category = p.category;
      if (!dirEdited) dir = p.dir;
    } catch (e) {
      if (mine !== seq) return;
      probe = null;
      mediaProbe = null;
      probeError = errorText(e);
      loginNeeded = (e as ProbeError)?.login ?? null;
    } finally {
      if (mine === seq) probing = false;
    }
  }

  // Re-check the link shortly after the user stops typing.
  $effect(() => {
    const target = url.trim();
    if (!looksLikeUrl(target)) {
      probe = null;
      mediaProbe = null;
      probeError = "";
      loginNeeded = null;
      return;
    }
    const t = setTimeout(() => runProbe(target), pending ? 0 : 350);
    return () => clearTimeout(t);
  });

  // Is there already a file with this name in this folder?
  $effect(() => {
    const name = filename.trim();
    const folder = dir.trim();
    if (!name || !folder) {
      clash = null;
      return;
    }
    let stale = false;
    const t = setTimeout(async () => {
      const r = await api.nameStatus(folder, name).catch(() => null);
      if (!stale) clash = r?.exists ? { suggested: r.suggested } : null;
    }, 200);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  });

  $effect(() => {
    if (!pending) urlInput?.focus();
  });

  function onurlpaste(e: ClipboardEvent) {
    const urls = parseUrls(e.clipboardData?.getData("text") ?? "");
    if (urls.length > 1 && onmulti) {
      e.preventDefault();
      onmulti(urls);
    }
  }

  async function browse() {
    const picked = await open({ directory: true, defaultPath: dir || undefined, title: "Save to folder" });
    if (typeof picked === "string") {
      dir = picked;
      dirEdited = true;
    }
  }

  async function submit(startPaused: boolean) {
    if (streamFailed) return;
    if (mediaProbe) {
      if (!mediaReady || busy) return;
      busy = true;
      try {
        await api.addMedia({
          url: url.trim(),
          filename: filename.trim() || null,
          dir: dir.trim() || null,
          container,
          subtitles: subtitleMode,
          selection,
          startPaused,
          pendingId: pending?.id ?? null,
          queueId,
        });
        ondone();
      } catch (e) {
        toasts.error(e);
      } finally {
        busy = false;
      }
      return;
    }
    if (!valid || busy || checksumError || mirrorsBad.length) return;
    busy = true;
    try {
      const name = filename.trim();
      await api.add({
        url: url.trim(),
        filename: clash && choice === "rename" ? clash.suggested : name || null,
        dir: dir.trim() || null,
        category,
        startPaused,
        connections,
        onConflict: clash ? choice : null,
        pendingId: pending?.id ?? null,
        queueId,
        checksum: checksumText.trim() || null,
        mirrors: mirrorLines.length ? mirrorLines : null,
      });
      ondone();
    } catch (e) {
      toasts.error(e);
    } finally {
      busy = false;
    }
  }

  /** The browser sent a fresh link for a download that stopped: continue that one. */
  async function continueExisting() {
    if (!resumable || busy) return;
    busy = true;
    try {
      await api.refreshUrl(resumable.id, url.trim(), pending?.id);
      toasts.show("Link updated — the download continues", "success");
      ondone();
    } catch (e) {
      toasts.error(e);
    } finally {
      busy = false;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    const target = e.target as HTMLElement;
    // The login form handles its own Enter.
    if (e.key === "Enter" && target.tagName === "INPUT" && !target.closest("form")) {
      e.preventDefault();
      submit(later);
    }
  }
</script>

{#if zipOpen && probe}
  <ZipPreview
    url={probe.final_url}
    pendingId={pending?.id ?? null}
    archiveName={filename.trim() || probe.filename}
    baseDir={dir}
    bind:queueId
    onback={() => (zipOpen = false)}
    {ondone}
  />
{:else}
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="add" {onkeydown}>
  <div class="head">
    <div>
      <h2>New download</h2>
      <div class="hint">
        {#if pending && pending.origin === "command_line"}From the command line{:else if pending && pending.origin === "clipboard"}Copied to the clipboard{:else if pending && pending.origin === "list"}From a list or metalink{:else if pending && pending.origin === "grabber"}Found by the site grabber{:else if pending}From {pending.site ? siteName(pending.site) : "Firefox"}{pending.page_url ? ` · ${host(pending.page_url)}` : ""}{pending.method === "POST" ? " · form submission" : ""}{:else}Paste one or more links{/if}
        {#if position}<span class="pos">{position}</span>{/if}
      </div>
    </div>
  </div>

  {#if resumable}
    <div class="resume" transition:slide={{ duration: 160 }}>
      <RotateCcw size={18} />
      <div class="min">
        <div class="t">Looks like <b>{resumable.filename ?? nameFromUrl(resumable.url)}</b> ({percent(resumable.total_size, resumable.downloaded).toFixed(0)}% done)</div>
        <div class="hint">Use this new link to continue it instead of starting over.</div>
      </div>
      <button class="btn btn-primary btn-sm" onclick={continueExisting} disabled={busy}>Continue it</button>
    </div>
  {/if}

  <label class="field">
    <span class="label">Address</span>
    <div class="url">
      <Link size={15} />
      <input bind:this={urlInput} class="input" bind:value={url} onpaste={onurlpaste} placeholder="https://example.com/file.zip" spellcheck="false" readonly={!!pending} />
    </div>
  </label>

  {#if loginNeeded}
    <div class="login-box">
      <LoginForm request={loginNeeded} onloggedin={() => runProbe(url.trim())} />
    </div>
  {:else if mediaProbe}
    <MediaPicker probe={mediaProbe} bind:selection bind:container bind:subtitles={subtitleMode} ffmpeg={mediaProbe.ffmpeg} />
  {:else}
  <div class="card" class:loading={probing}>
    <FileIcon {category} size={44} />
    <div class="card-main">
      {#if probing && !probe}
        <div class="status"><LoaderCircle size={14} class="spin" /> Checking link…</div>
      {:else if probeError}
        <div class="status err"><TriangleAlert size={14} /> <span class="truncate" title={probeError}>{probeError}</span></div>
        {#if streamFailed}
          <div class="hint">This is a video stream, and it couldn't be read. Check the link, then try again.</div>
        {:else}
          <div class="hint">You can still add it — K-Star will retry when it starts.</div>
        {/if}
      {:else if probe}
        <div class="facts">
          <span class="size">{probe.total_size != null ? bytes(probe.total_size) : "Unknown size"}</span>
          <span class="dot">·</span>
          <span>{categoryMeta(category).label}</span>
          {#if probe.resumable}
            <span class="pill" style:--c="var(--ok)"><CircleCheck size={11} /> Resumable</span>
          {:else}
            <span class="pill" style:--c="var(--warn)">Can't resume</span>
          {/if}
        </div>
        <div class="hint truncate">{host(probe.final_url)}{probe.digest ? ` · the server gives its ${ALGO_LABEL[probe.digest.algo]}, which is checked` : ""}</div>
      {:else}
        <div class="hint">File details appear here once the link is checked.</div>
      {/if}
    </div>
    {#if zipPreviewable}
      <button class="btn btn-sm" onclick={() => (zipOpen = true)} title="See what's inside and pick files"><FileArchive size={14} /> Preview contents</button>
    {/if}
  </div>

  {/if}

  {#if mediaProbe}
    {#if isStream}
      <label class="field">
        <span class="label">File name</span>
        <input class="input" bind:value={filename} placeholder={mediaProbe.title ?? "Named after the stream"} spellcheck="false" />
      </label>
      <div class="field">
        <span class="label">Save to</span>
        <div class="folder">
          <input class="input" bind:value={dir} oninput={() => (dirEdited = true)} placeholder="Video folder" spellcheck="false" />
          <button class="btn" onclick={browse} title="Choose folder"><FolderOpen size={15} /></button>
        </div>
      </div>
    {/if}
  {:else}
  <div class="grid">
    <label class="field">
      <span class="label">File name</span>
      <input class="input" bind:value={filename} placeholder="Detected automatically" spellcheck="false" />
    </label>
    <label class="field">
      <span class="label">Category</span>
      <select class="input" value={category} onchange={(e) => pickCategory(e.currentTarget.value as Category)}>
        {#each CATEGORIES as c (c.key)}<option value={c.key}>{c.label}</option>{/each}
      </select>
    </label>
  </div>
  <div class="field">
    <span class="label">Save to</span>
    <div class="folder">
      <input class="input" bind:value={dir} oninput={() => (dirEdited = true)} placeholder="Category folder" spellcheck="false" />
      <button class="btn" onclick={browse} title="Choose folder"><FolderOpen size={15} /></button>
    </div>
  </div>

  {#if clash}
    <div class="clash" transition:slide={{ duration: 160 }}>
      <div class="clash-head"><TriangleAlert size={15} /> A file named “{filename.trim()}” is already in this folder.</div>
      <div class="seg">
        <button class:on={choice === "rename"} onclick={() => (choice = "rename")}>Keep both</button>
        <button class:on={choice === "overwrite"} onclick={() => (choice = "overwrite")}>Replace it when done</button>
      </div>
      <div class="hint truncate">
        {#if choice === "rename"}The new file is saved as “{clash.suggested}”.{:else}The old file stays until the new one is complete.{/if}
      </div>
    </div>
  {/if}

  <button class="adv-toggle" onclick={() => (advanced = !advanced)}>
    <ChevronDown size={14} style="transform: rotate({advanced ? 180 : 0}deg); transition: transform .2s" />
    Advanced
  </button>
  {#if advanced}
    <div class="adv" transition:slide={{ duration: 160 }}>
      <label class="field">
        <span class="label">Connections <span class="hint">({connections ?? "default"})</span></span>
        <input type="range" min="1" max="32" value={connections ?? 8} oninput={(e) => (connections = +e.currentTarget.value)} />
      </label>
      <label class="field">
        <span class="label">Checksum <span class="hint">(MD5, SHA-1, SHA-256 or SHA-512; checked when the file is done)</span></span>
        <input class="input mono" bind:value={checksumText} placeholder={pending?.checksum ? `${pending.checksum} (from the list)` : "Paste the checksum the site gives"} spellcheck="false" />
        {#if checksumError}
          <span class="note err"><TriangleAlert size={12} /> {checksumError}</span>
        {:else if checksumParsed}
          <span class="note ok"><ShieldCheck size={12} /> {ALGO_LABEL[checksumParsed.algo]}</span>
        {/if}
      </label>
      <label class="field">
        <span class="label">Mirrors <span class="hint">(other addresses of the same file, one per line; fetched from at the same time)</span></span>
        <textarea class="input mono" rows="2" bind:value={mirrorsText} placeholder={pending?.mirrors ? `${pending.mirrors} from the list` : "https://mirror.example/file.iso"} spellcheck="false"></textarea>
        {#if mirrorsBad.length}<span class="note err"><TriangleAlert size={12} /> “{mirrorsBad[0]}” isn't a link</span>{/if}
      </label>
    </div>
  {/if}

  {/if}

  <div class="actions">
    <button class="btn btn-ghost" onclick={oncancel}>Cancel</button>
    <span class="grow"></span>
    <QueuePicker bind:value={queueId} />
    <button class="btn" class:btn-primary={later} disabled={(mediaProbe ? !mediaReady : !valid || streamFailed) || busy} onclick={() => submit(true)}>Download later</button>
    <button class="btn" class:btn-primary={!later} disabled={(mediaProbe ? !mediaReady : !valid || streamFailed) || busy} onclick={() => submit(false)}>Start download</button>
  </div>
</div>
{/if}

<style>
  .add {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  h2 {
    margin: 0;
    font-size: 17px;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  .pos {
    margin-left: 8px;
    padding: 1px 7px;
    border-radius: 99px;
    background: var(--surface-3);
    font-weight: 600;
  }
  .field {
    display: block;
    min-width: 0;
  }
  .url {
    position: relative;
  }
  .url :global(svg) {
    position: absolute;
    left: 12px;
    top: 11px;
    color: var(--muted);
  }
  .url .input {
    padding-left: 34px;
  }
  .card {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 14px;
    border-radius: var(--r-lg);
    background: var(--grad-soft);
    border: 1px solid var(--border);
    min-height: 74px;
    transition: opacity 0.2s;
  }
  .card.loading {
    opacity: 0.75;
  }
  .card-main {
    min-width: 0;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .facts {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-weight: 550;
  }
  .size {
    font-size: 15px;
    font-weight: 700;
  }
  .dot {
    color: var(--muted);
  }
  .status {
    display: flex;
    align-items: center;
    gap: 7px;
    font-weight: 600;
    min-width: 0;
  }
  .status.err {
    color: var(--err);
  }
  .status :global(.spin) {
    animation: spin 0.9s linear infinite;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 180px;
    gap: 12px;
  }
  .resume {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 14px;
    border-radius: var(--r-lg);
    background: color-mix(in srgb, var(--accent) 9%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 28%, transparent);
  }
  .resume > :global(svg) {
    flex: none;
    color: var(--accent);
  }
  .min {
    flex: 1;
    min-width: 0;
  }
  .t {
    font-weight: 550;
  }
  .clash {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    border-radius: var(--r-lg);
    background: color-mix(in srgb, var(--warn) 10%, transparent);
  }
  .clash-head {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 600;
    font-size: 12.5px;
  }
  .clash-head :global(svg) {
    color: var(--warn);
    flex: none;
  }
  .seg {
    display: flex;
    gap: 4px;
    padding: 3px;
    border-radius: var(--r);
    background: var(--surface-3);
  }
  .seg button {
    flex: 1;
    min-width: 0;
    height: 30px;
    padding: 0 10px;
    border-radius: var(--r-sm);
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .seg button.on {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .folder {
    display: flex;
    gap: 6px;
  }
  .login-box {
    padding: 14px;
    border-radius: var(--r-lg);
    border: 1px solid color-mix(in srgb, var(--accent) 28%, transparent);
    background: color-mix(in srgb, var(--accent) 6%, transparent);
  }
  .folder .btn {
    padding: 0 10px;
    height: 36px;
  }
  .adv-toggle {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    align-self: flex-start;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-2);
    margin-top: -4px;
  }
  .adv {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .adv textarea {
    resize: vertical;
    min-height: 54px;
    padding-top: 8px;
    font-size: 12px;
  }
  .adv .mono {
    font-size: 12px;
  }
  .note {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    margin-top: 5px;
    font-size: 12px;
    font-weight: 600;
  }
  .note.err {
    color: var(--err);
  }
  .note.ok {
    color: var(--ok);
  }
  .adv input[type="range"] {
    width: 100%;
    accent-color: var(--accent);
  }
  .actions {
    display: flex;
    gap: 8px;
    padding-top: 4px;
  }
  .grow {
    flex: 1;
  }
</style>
