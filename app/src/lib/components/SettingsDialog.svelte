<script lang="ts">
  import { CircleCheck, CircleX, FileText, FolderOpen, KeyRound, Monitor, Moon, Play, Plus, Puzzle, Sun, TriangleAlert, X } from "@lucide/svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";
  import { api, type Category, type CommandResult, type IntegrationStatus, type ProxyMode, type ProxyRoute, type SavedLogin, type Settings, type SoundEvent, type FfmpegStatus } from "../api";
  import { SPEED_PRESETS, limitLabel, relativeTime } from "../format";
  import { CATEGORIES } from "../categories";
  import { toasts } from "../store.svelte";
  import { theme, type ThemePref } from "../theme.svelte";
  import CategoryEditor from "./CategoryEditor.svelte";
  import Modal from "./Modal.svelte";

  let { settings, onclose, onsaved }: { settings: Settings; onclose: () => void; onsaved: (s: Settings) => void } = $props();

  type Tab = "general" | "transfers" | "media" | "network" | "categories" | "browser" | "appearance";
  let tab = $state<Tab>("general");
  // Edit a copy; nothing changes until Save.
  // svelte-ignore state_referenced_locally
  let s = $state<Settings>(structuredClone($state.snapshot(settings)) as Settings);
  let integration = $state<IntegrationStatus | null>(null);
  let saving = $state(false);
  let route = $state<ProxyRoute | null>(null);
  let logins = $state<SavedLogin[]>([]);
  let keyring = $state(true);
  // Typed proxy password; saved to the keyring on Save. Empty = keep the saved one.
  let proxyPassword = $state("");
  let proxyPasswordSaved = $state(false);
  // Starting at login is the autostart file itself, not a stored setting: read it on open,
  // write it on Save.
  let autostart = $state(false);
  let autostartWas = false;

  onMount(async () => {
    integration = await api.integrationStatus().catch(() => null);
    autostart = autostartWas = await api.autostartEnabled().catch(() => false);
    route = await api.proxyStatus().catch(() => null);
  });

  // The keyring is only asked when the Network tab is opened.
  let networkLoaded = false;
  $effect(() => {
    if (tab !== "network" || networkLoaded) return;
    networkLoaded = true;
    (async () => {
      keyring = await api.keyringAvailable().catch(() => false);
      if (!keyring) return;
      logins = await api.savedLogins().catch(() => []);
      proxyPasswordSaved = await api.proxyPasswordSaved().catch(() => false);
    })();
  });

  async function forgetLogin(host: string) {
    try {
      await api.forgetSavedLogin(host);
      logins = logins.filter((l) => l.host !== host);
    } catch (e) {
      toasts.error(e);
    }
  }

  function addLimit() {
    s.server_limits = [...s.server_limits, { host: "", connections: 4 }];
  }

  // FFmpeg is looked at when the Media tab opens.
  let ffmpeg = $state<FfmpegStatus | null>(null);
  $effect(() => {
    if (tab === "media" && ffmpeg === null)
      api
        .mediaDiagnostics()
        .then((f) => (ffmpeg = f))
        .catch(() => (ffmpeg = { path: null, version: null, ok: false, missing: ["Siler couldn't check it"] }));
  });

  async function browse() {
    const picked = await open({ directory: true, defaultPath: s.download_dir, title: "Download folder" });
    if (typeof picked === "string") s.download_dir = picked;
  }

  async function install() {
    try {
      integration = await api.installIntegration();
      toasts.show("Browser integration installed", "success");
    } catch (e) {
      toasts.error(e);
    }
  }

  async function installExtension() {
    try {
      await api.installExtension();
      toasts.show("Firefox will ask to add the extension", "success");
    } catch (e) {
      toasts.error(e);
    }
  }

  const RELEASES = "https://github.com/sekhnat/Siler/releases/latest";

  // The extension and the app speak protocol versions; tell the user which one is behind.
  const mismatch = $derived.by(() => {
    const i = integration;
    if (!i || i.extension_protocol == null || i.extension_protocol === i.protocol) return null;
    return i.extension_protocol < i.protocol ? "Update the Firefox extension" : "Update Siler";
  });

  async function save() {
    saving = true;
    try {
      const saved = await api.setSettings(s);
      if (proxyPassword && saved.proxy.mode === "manual" && saved.proxy.username.trim()) {
        await api.setProxyPassword(proxyPassword);
      }
      if (autostart !== autostartWas) await api.setAutostart(autostart);
      onsaved(saved);
      onclose();
    } catch (e) {
      toasts.error(e);
    } finally {
      saving = false;
    }
  }

  const tabs: { key: Tab; label: string }[] = [
    { key: "general", label: "General" },
    { key: "transfers", label: "Transfers" },
    { key: "media", label: "Media" },
    { key: "network", label: "Network" },
    { key: "categories", label: "Categories" },
    { key: "browser", label: "Browser" },
    { key: "appearance", label: "Appearance" },
  ];
  const themes: { key: ThemePref; label: string; icon: typeof Sun }[] = [
    { key: "system", label: "System", icon: Monitor },
    { key: "light", label: "Light", icon: Sun },
    { key: "dark", label: "Dark", icon: Moon },
  ];
  const soundRows: { event: SoundEvent; label: string; on: "on_complete" | "on_failed" | "on_queue_done"; file: "complete_file" | "failed_file" | "queue_done_file" }[] = [
    { event: "complete", label: "A download finished", on: "on_complete", file: "complete_file" },
    { event: "failed", label: "A download failed", on: "on_failed", file: "failed_file" },
    { event: "queue_done", label: "A queue finished", on: "on_queue_done", file: "queue_done_file" },
  ];

  async function pickSound(file: "complete_file" | "failed_file" | "queue_done_file") {
    const picked = await open({
      title: "Choose a sound",
      filters: [{ name: "Sounds", extensions: ["oga", "ogg", "wav", "mp3", "flac", "opus"] }],
    });
    if (typeof picked === "string") s.sounds[file] = picked;
  }

  const baseName = (p: string) => p.split("/").pop() ?? p;

  let testing = $state(false);
  let testResult = $state<CommandResult | null>(null);
  async function testCommand() {
    const path = await open({ title: "Try the command on a file" });
    if (typeof path !== "string") return;
    testing = true;
    testResult = null;
    try {
      testResult = await api.testCommand(s.after_download.command, path);
    } catch (e) {
      toasts.error(e);
    } finally {
      testing = false;
    }
  }
  function toggleCommandCategory(c: Category) {
    const cats = s.after_download.categories;
    s.after_download.categories = cats.includes(c) ? cats.filter((x) => x !== c) : [...cats, c];
  }
  const customLimit = $derived(!SPEED_PRESETS.includes(s.global_speed_limit));
  const proxyModes: { key: ProxyMode; label: string }[] = [
    { key: "none", label: "No proxy" },
    { key: "system", label: "System" },
    { key: "manual", label: "Manual" },
  ];
  // The status line describes the saved settings; say so while they're being edited.
  const proxyEdited = $derived(JSON.stringify(s.proxy) !== JSON.stringify(settings.proxy));
</script>

{#snippet toggle(label: string, hint: string, value: boolean, set: (v: boolean) => void)}
  <button class="toggle-row" role="switch" aria-checked={value} onclick={() => set(!value)}>
    <div>
      <div class="t-label">{label}</div>
      <div class="hint">{hint}</div>
    </div>
    <span class="switch" class:on={value}></span>
  </button>
{/snippet}

<Modal title="Settings" width={660} {onclose}>
  <div class="tabs">
    {#each tabs as t (t.key)}
      <button class:on={tab === t.key} onclick={() => (tab = t.key)}>{t.label}</button>
    {/each}
  </div>

  <div class="pane">
    {#if tab === "general"}
      <div class="field">
        <span class="label">Download folder</span>
        <div class="folder">
          <input class="input" bind:value={s.download_dir} spellcheck="false" />
          <button class="btn" onclick={browse}><FolderOpen size={15} /> Browse</button>
        </div>
      </div>
      {@render toggle("Sort into category folders", "Video, Music, Documents… inside the download folder", s.use_category_folders, (v) => (s.use_category_folders = v))}
      <div class="field">
        <span class="label">When a file with the same name exists</span>
        <select class="input" bind:value={s.on_conflict}>
          <option value="ask">Ask me</option>
          <option value="rename">Keep both (add a number)</option>
          <option value="overwrite">Replace the old file</option>
          <option value="skip">Skip the download</option>
        </select>
      </div>
      {@render toggle(
        "Keep running in the tray",
        integration && !integration.tray_available ? "No system tray was found, so closing the window quits Siler" : "Closing the window keeps downloads going",
        s.close_to_tray,
        (v) => (s.close_to_tray = v),
      )}
      {@render toggle(
        "Start when you log in",
        integration && !integration.tray_available ? "Opens the window when you log in" : "Opens in the tray, ready for downloads",
        autostart,
        (v) => (autostart = v),
      )}
      {@render toggle("Ask before quitting", "Confirm when quitting would pause running downloads", s.confirm_quit, (v) => (s.confirm_quit = v))}
      {@render toggle("Notifications", "Notify when a download finishes or fails", s.notify_on_complete, (v) => (s.notify_on_complete = v))}
      {@render toggle("Resume on startup", "Continue unfinished downloads when Siler starts", s.resume_on_startup, (v) => (s.resume_on_startup = v))}
      {@render toggle("Keep the computer awake", "Don't let it go to sleep while downloads are running", s.prevent_sleep, (v) => (s.prevent_sleep = v))}
      {@render toggle(
        "Watch the clipboard for links",
        "Copying a link to a file (one of the types in Categories) offers to download it",
        s.clipboard_monitor,
        (v) => (s.clipboard_monitor = v),
      )}
      <div class="logs">
        <FileText size={15} />
        <span class="hint grow truncate" title={integration?.log_dir}>Logs: {integration?.log_dir ?? "~/.local/state/siler/logs"}</span>
        <button class="btn btn-sm" onclick={() => api.openLogFolder().catch(toasts.error.bind(toasts))}>Open log folder</button>
      </div>
    {:else if tab === "media"}
      <div class="field">
        <span class="label">FFmpeg</span>
        {#if ffmpeg === null}
          <div class="hint">Checking…</div>
        {:else if ffmpeg.ok}
          <div class="hint"><CircleCheck size={13} /> {ffmpeg.version ? `Version ${ffmpeg.version}` : "Found"} at {ffmpeg.path}</div>
        {:else}
          <div class="hint warn"><TriangleAlert size={13} /> FFmpeg isn't usable: {ffmpeg.missing.join(", ") || "not found"}. Single-track downloads still work; combining video and audio needs it.</div>
          <div class="hint">Build the bundled copy with <code>scripts/build-ffmpeg.sh</code>.</div>
        {/if}
      </div>
      <div class="field">
        <span class="label">Pieces fetched at once <b>{s.media_segment_concurrency}</b></span>
        <input type="range" min="1" max="16" bind:value={s.media_segment_concurrency} />
        <div class="hint">Segments of one stream downloaded in parallel. The connection limits still apply.</div>
      </div>
      <label class="field">
        <span class="label">Highest video quality by default</span>
        <select class="input" bind:value={s.preferred_max_height}>
          <option value={0}>No limit</option>
          <option value={1080}>1080p</option>
          <option value={720}>720p</option>
          <option value={480}>480p</option>
          <option value={360}>360p</option>
        </select>
      </label>
      <div class="grid2">
        <label class="field">
          <span class="label">Preferred audio language</span>
          <input class="input" bind:value={s.preferred_audio_language} placeholder="en" spellcheck="false" />
        </label>
        <label class="field">
          <span class="label">Preferred subtitle language</span>
          <input class="input" bind:value={s.preferred_subtitle_language} placeholder="none" spellcheck="false" />
        </label>
      </div>
      <label class="field">
        <span class="label">Default container</span>
        <select class="input" bind:value={s.default_media_container}>
          <option value="auto">Automatic (MP4, or MKV when needed)</option>
          <option value="mp4">MP4</option>
          <option value="mkv">MKV</option>
        </select>
      </label>
      <label class="field">
        <span class="label">Subtitles by default</span>
        <select class="input" bind:value={s.default_subtitle_mode}>
          <option value="embed">Inside the video</option>
          <option value="sidecar">As a separate file</option>
          <option value="none">Leave out</option>
        </select>
      </label>
      {@render toggle(
        "Allow streams from this computer or the local network",
        "Off by default, so a playlist can't make Siler contact other machines. Turn it on for a media server at home.",
        s.allow_private_media_addresses,
        (v) => (s.allow_private_media_addresses = v),
      )}
    {:else if tab === "transfers"}
      <div class="field">
        <span class="label">Connections per download <b>{s.connections}</b></span>
        <input type="range" min="1" max="32" bind:value={s.connections} />
        <div class="hint">More connections help on fast links and slow servers. 8 is a good default.</div>
      </div>
      <div class="field">
        <span class="label">Simultaneous downloads <b>{s.max_concurrent}</b></span>
        <input type="range" min="1" max="10" bind:value={s.max_concurrent} />
      </div>
      <div class="field">
        <span class="label">Connections per server <b>{s.max_connections_per_server || "No limit"}</b></span>
        <input type="range" min="0" max="64" bind:value={s.max_connections_per_server} />
        <div class="hint">Shared by all downloads from one server, so busy servers don't refuse or ban you.</div>
        {#each s.server_limits as limit, i (i)}
          <div class="limit">
            <input class="input" bind:value={limit.host} placeholder="example.com" spellcheck="false" />
            <input class="input num" type="number" min="1" max="128" bind:value={limit.connections} title="Connections" />
            <button class="icon-btn sm" title="Remove" onclick={() => (s.server_limits = s.server_limits.filter((_, j) => j !== i))}><X size={15} /></button>
          </div>
        {/each}
        <button class="btn btn-sm add-limit" onclick={addLimit}><Plus size={14} /> Different limit for a server</button>
      </div>
      <div class="row2">
        <div class="field">
          <span class="label">Global speed limit</span>
          <select class="input" value={customLimit ? -1 : s.global_speed_limit} onchange={(e) => {
            const v = +e.currentTarget.value;
            if (v >= 0) s.global_speed_limit = v;
          }}>
            {#each SPEED_PRESETS as p}<option value={p}>{limitLabel(p)}</option>{/each}
            {#if customLimit}<option value={-1}>{limitLabel(s.global_speed_limit)}</option>{/if}
          </select>
        </div>
        <div class="field">
          <span class="label">Retries per connection</span>
          <input class="input" type="number" min="0" max="100" bind:value={s.max_retries} />
        </div>
      </div>
      <div class="field">
        <span class="label">User agent</span>
        <input class="input mono" bind:value={s.user_agent} spellcheck="false" />
        <div class="hint">Used for links added by hand. Browser downloads use the browser's own user agent.</div>
      </div>
      {@render toggle("Data quota", "Download at most so much in each period; downloads wait for the next one. Useful on metered connections.", s.quota.enabled, (v) => (s.quota.enabled = v))}
      {#if s.quota.enabled}
        <div class="quota">
          <span class="hint">At most</span>
          <input class="input num" type="number" min="1" bind:value={s.quota.megabytes} />
          <span class="hint">MB every</span>
          <input class="input num" type="number" min="1" max="744" bind:value={s.quota.hours} />
          <span class="hint">{s.quota.hours === 1 ? "hour" : "hours"}</span>
        </div>
      {/if}
      {@render toggle("Run a command after each download", "For example to unpack, scan or move files. It runs without a shell, one download at a time.", s.after_download.enabled, (v) => (s.after_download.enabled = v))}
      {#if s.after_download.enabled}
        <div class="command">
          <input class="input mono" bind:value={s.after_download.command} placeholder={"my-tool --file {path}"} spellcheck="false" />
          <div class="hint">
            <code>{"{path}"}</code> the file · <code>{"{dir}"}</code> its folder · <code>{"{name}"}</code> its name · <code>{"{url}"}</code> its address ·
            <code>{"{size}"}</code> bytes. Quote words with spaces; use <code>sh -c '…' _ {"{path}"}</code> for a shell.
          </div>
          <div class="chips">
            <span class="hint">Only for</span>
            {#each CATEGORIES as c (c.key)}
              <button class="chip" class:on={s.after_download.categories.includes(c.key)} style:--c={c.color} onclick={() => toggleCommandCategory(c.key)}>{c.label}</button>
            {/each}
            {#if !s.after_download.categories.length}<span class="hint">(every download)</span>{/if}
          </div>
          <div class="cmd-row">
            <button class="toggle small-toggle" onclick={() => (s.after_download.notify_on_failure = !s.after_download.notify_on_failure)}>
              <span class="switch" class:on={s.after_download.notify_on_failure}></span> Notify when it fails
            </button>
            <span class="grow"></span>
            <button class="btn btn-sm" onclick={testCommand} disabled={!s.after_download.command.trim() || testing}><Play size={13} /> {testing ? "Running…" : "Try it on a file…"}</button>
          </div>
          {#if testResult}
            <div class="test" class:bad={testResult.error != null || testResult.exit_code !== 0}>
              {testResult.error ?? `Exited with ${testResult.exit_code}`}
              {#if testResult.output}<pre>{testResult.output}</pre>{/if}
            </div>
          {/if}
        </div>
      {/if}
      {@render toggle("Open metalinks", "A downloaded .meta4 or .metalink file is replaced by the files it lists, with their mirrors and checksums.", s.open_metalinks, (v) => (s.open_metalinks = v))}
    {:else if tab === "network"}
      <div class="field">
        <span class="label">Proxy</span>
        <div class="seg">
          {#each proxyModes as m (m.key)}
            <button class:on={s.proxy.mode === m.key} onclick={() => (s.proxy.mode = m.key)}>{m.label}</button>
          {/each}
        </div>
      </div>
      {#if s.proxy.mode === "manual"}
        <div class="proxy">
          <label class="field">
            <span class="label">Type</span>
            <select class="input" bind:value={s.proxy.scheme}>
              <option value="http">HTTP</option>
              <option value="socks5">SOCKS5</option>
            </select>
          </label>
          <label class="field">
            <span class="label">Address</span>
            <input class="input" bind:value={s.proxy.host} placeholder="proxy.example.com" spellcheck="false" />
          </label>
          <label class="field">
            <span class="label">Port</span>
            <input class="input" type="number" min="1" max="65535" bind:value={s.proxy.port} />
          </label>
        </div>
        <div class="row2">
          <label class="field">
            <span class="label">Username <span class="hint">(optional)</span></span>
            <input class="input" bind:value={s.proxy.username} autocomplete="off" spellcheck="false" />
          </label>
          <label class="field">
            <span class="label">Password</span>
            <input
              class="input"
              type="password"
              bind:value={proxyPassword}
              disabled={!s.proxy.username.trim() || !keyring}
              placeholder={!keyring ? "No keyring available" : proxyPasswordSaved ? "Saved in the keyring" : ""}
              autocomplete="new-password"
            />
          </label>
        </div>
        <label class="field">
          <span class="label">Connect directly to</span>
          <input class="input" bind:value={s.proxy.no_proxy} placeholder="localhost, 127.0.0.1, .lan" spellcheck="false" />
        </label>
      {/if}
      {#if route && !proxyEdited}
        <div class="route hint">
          <span>Now: {route.summary}</span>
          {#if route.note}<span class="warn"><TriangleAlert size={13} /> {route.note}</span>{/if}
        </div>
      {:else if proxyEdited}
        <div class="route hint">Applies when you save.</div>
      {/if}

      <div class="field">
        <span class="label">Saved logins</span>
        {#if !keyring}
          <div class="hint">No keyring service (KWallet, GNOME Keyring…) is running, so logins last until Siler quits.</div>
        {:else if logins.length === 0}
          <div class="hint">When a server asks for a username and password, you can save it here.</div>
        {:else}
          <div class="logins">
            {#each logins as l (l.host)}
              <div class="login-row">
                <KeyRound size={14} />
                <span class="grow truncate"><b>{l.host}</b> <span class="hint">{l.username}</span></span>
                <button class="btn btn-sm btn-ghost" onclick={() => forgetLogin(l.host)}>Forget</button>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {:else if tab === "categories"}
      <CategoryEditor bind:s />
    {:else if tab === "browser"}
      <div class="integration" class:ok={integration?.manifest_installed && integration?.host_exists && !mismatch}>
        {#if mismatch}
          <TriangleAlert size={20} />
          <div>
            <div class="t-label">{mismatch}</div>
            <div class="hint">
              The extension ({integration?.extension_version}) and Siler ({integration?.app_version}) are from different versions and may not work together.
            </div>
          </div>
        {:else if integration?.manifest_installed && integration.host_exists}
          <CircleCheck size={20} />
          <div>
            <div class="t-label">Firefox integration is installed</div>
            <div class="hint">
              {#if integration.last_contact}
                Extension {integration.extension_version ?? ""} last seen {relativeTime(integration.last_contact)}
              {:else}
                Waiting for the extension to connect
              {/if}
            </div>
          </div>
        {:else}
          <TriangleAlert size={20} />
          <div>
            <div class="t-label">Firefox integration is not set up</div>
            <div class="hint">Install the native messaging host so the extension can talk to Siler.</div>
          </div>
        {/if}
        <button class="btn btn-sm" onclick={install}>{integration?.manifest_installed ? "Repair" : "Install"}</button>
      </div>
      {#if integration}
        <div class="checks">
          {#each integration.browsers as b (b.name)}
            <span class="check" class:ok={b.installed} title={b.manifest_path}>
              {#if b.installed}<CircleCheck size={13} />{:else}<CircleX size={13} />{/if}
              {b.name}
            </span>
          {/each}
          <span class="check" class:ok={integration.desktop_entry} title="Application menu entry and icon">
            {#if integration.desktop_entry}<CircleCheck size={13} />{:else}<CircleX size={13} />{/if}
            App menu &amp; icon
          </span>
        </div>
      {/if}
      {@render toggle("Start browser downloads immediately", "Skip the confirmation window for files captured from Firefox", s.auto_start_browser_downloads, (v) => (s.auto_start_browser_downloads = v))}
      <div class="steps">
        <div class="t-label"><Puzzle size={15} /> Installing the extension</div>
        {#if integration?.signed_xpi}
          <div class="xpi">
            <span class="hint grow">The signed Siler extension is ready to add. Then allow it to access all websites when Firefox asks.</span>
            <button class="btn btn-primary btn-sm" onclick={installExtension}>Add to Firefox</button>
          </div>
        {:else}
          <ol>
            <li>Click <b>Install</b> above to register Siler with Firefox.</li>
            <li>
              Download <span class="mono">siler-integration.xpi</span> from the latest release
              <button class="btn btn-sm" onclick={() => api.openUrl(RELEASES).catch((e) => toasts.error(e))}>Open releases</button>
            </li>
            <li>Open the file with Firefox, and allow the extension to access all websites when Firefox asks.</li>
          </ol>
          <div class="hint">Building from source? See <span class="mono">docs/browser-extension.md</span>.</div>
        {/if}
      </div>
    {:else}
      <div class="field">
        <span class="label">Theme</span>
        <div class="seg">
          {#each themes as t (t.key)}
            <button class:on={theme.pref === t.key} onclick={() => theme.set(t.key)}><t.icon size={15} /> {t.label}</button>
          {/each}
        </div>
      </div>
      {@render toggle("Sounds", "Play the desktop's sounds for download events", s.sounds.enabled, (v) => (s.sounds.enabled = v))}
      {#if s.sounds.enabled}
        <div class="sounds">
          {#each soundRows as r (r.event)}
            <div class="sound-row">
              <label class="sound-check">
                <input type="checkbox" bind:checked={s.sounds[r.on]} />
                {r.label}
              </label>
              <span class="hint grow truncate" title={s.sounds[r.file] ?? ""}>{s.sounds[r.file] ? baseName(s.sounds[r.file] ?? "") : "Theme sound"}</span>
              {#if s.sounds[r.file]}
                <button class="icon-btn sm" title="Use the theme sound" onclick={() => (s.sounds[r.file] = null)}><X size={14} /></button>
              {/if}
              <button class="btn btn-sm" onclick={() => pickSound(r.file)}>Choose…</button>
              <button class="icon-btn sm" title="Play" onclick={() => api.testSound(r.event, $state.snapshot(s.sounds))}><Play size={14} /></button>
            </div>
          {/each}
        </div>
      {/if}
    {/if}
  </div>

  {#snippet footer()}
    <button class="btn btn-ghost" onclick={onclose}>Cancel</button>
    <button class="btn btn-primary" onclick={save} disabled={saving}>Save</button>
  {/snippet}
</Modal>

<style>
  .tabs {
    display: flex;
    gap: 4px;
    padding: 4px;
    margin-bottom: 18px;
    border-radius: var(--r);
    background: var(--surface-3);
  }
  .tabs button {
    flex: 1;
    height: 30px;
    border-radius: var(--r-sm);
    font-weight: 600;
    color: var(--text-2);
  }
  .tabs button.on {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .pane {
    display: flex;
    flex-direction: column;
    gap: 16px;
    min-height: 300px;
  }
  .folder {
    display: flex;
    gap: 6px;
  }
  .quota {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: -6px 0 6px;
  }
  .quota .num {
    width: 90px;
  }
  .command {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: -6px 0 6px;
  }
  .command code {
    font-size: 11px;
    padding: 0 3px;
    border-radius: 4px;
    background: var(--surface-3);
  }
  .command .chips {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 5px;
  }
  .command .chip {
    height: 24px;
    padding: 0 9px;
    border-radius: 99px;
    border: 1px solid var(--border);
    font-size: 11.5px;
    font-weight: 600;
    color: var(--text-2);
  }
  .command .chip.on {
    border-color: color-mix(in srgb, var(--c) 50%, transparent);
    background: color-mix(in srgb, var(--c) 14%, transparent);
    color: var(--text);
  }
  .cmd-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .small-toggle {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12.5px;
  }
  .grow {
    flex: 1;
  }
  .test {
    padding: 10px 12px;
    border-radius: var(--r);
    background: color-mix(in srgb, var(--ok) 10%, transparent);
    font-size: 12.5px;
    font-weight: 600;
  }
  .test.bad {
    background: color-mix(in srgb, var(--err) 10%, transparent);
    color: var(--err);
  }
  .test pre {
    margin: 6px 0 0;
    max-height: 120px;
    overflow: auto;
    font-size: 11px;
    white-space: pre-wrap;
    color: var(--text);
    font-weight: 400;
  }
  .row2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .label b {
    float: right;
    color: var(--accent-text);
  }
  .limit {
    display: grid;
    grid-template-columns: 1fr 90px auto;
    gap: 6px;
    align-items: center;
    margin-top: 8px;
  }
  .add-limit {
    margin-top: 8px;
  }
  .proxy {
    display: grid;
    grid-template-columns: 110px 1fr 100px;
    gap: 12px;
  }
  .route {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: -6px;
  }
  .route .warn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--warn);
  }
  .logins {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: var(--r);
  }
  .login-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 8px 6px 12px;
    color: var(--muted);
  }
  .login-row + .login-row {
    border-top: 1px solid var(--border);
  }
  .login-row b {
    color: var(--text);
    font-weight: 600;
  }
  input[type="range"] {
    width: 100%;
    accent-color: var(--accent);
  }
  .toggle-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 2px 0;
    text-align: left;
  }
  .t-label {
    display: flex;
    align-items: center;
    gap: 7px;
    font-weight: 600;
  }
  .integration {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 14px;
    border-radius: var(--r-lg);
    background: color-mix(in srgb, var(--warn) 10%, transparent);
    --c: var(--warn);
  }
  .integration.ok {
    background: color-mix(in srgb, var(--ok) 10%, transparent);
    --c: var(--ok);
  }
  .integration > :global(svg) {
    flex: none;
    color: var(--c);
  }
  .integration > div {
    flex: 1;
  }
  .checks {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: -6px;
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    padding: 0 9px;
    border-radius: 99px;
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
    background: var(--surface-3);
  }
  .check.ok {
    color: var(--ok);
    background: color-mix(in srgb, var(--ok) 12%, transparent);
  }
  .xpi,
  .logs {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 8px;
  }
  .logs {
    margin-top: 0;
    color: var(--muted);
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .steps {
    padding: 14px;
    border-radius: var(--r-lg);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  ol {
    margin: 8px 0 0;
    padding-left: 20px;
    color: var(--text-2);
    line-height: 1.7;
  }
  .sounds {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: var(--r);
    margin-top: -6px;
  }
  .sound-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px 6px 12px;
  }
  .sound-row + .sound-row {
    border-top: 1px solid var(--border);
  }
  .sound-check {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    width: 170px;
    flex: none;
    font-weight: 550;
  }
  .sound-check input {
    accent-color: var(--accent);
  }
  .seg {
    display: inline-flex;
    gap: 4px;
    padding: 4px;
    border-radius: var(--r);
    background: var(--surface-3);
  }
  .seg button {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 32px;
    padding: 0 16px;
    border-radius: var(--r-sm);
    font-weight: 600;
    color: var(--text-2);
  }
  .seg button.on {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .grid2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .hint.warn {
    color: var(--warn);
    display: flex;
    gap: 6px;
    align-items: center;
  }
</style>
