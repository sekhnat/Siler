<script lang="ts">
  import { LANGUAGES, setChosenLanguage, t } from "../i18n";
  import { CircleCheck, CircleX, FileText, FolderOpen, KeyRound, Monitor, Moon, Play, Plus, Puzzle, Sun, TriangleAlert, X } from "@lucide/svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";
  import { api, type AppInfo, type Category, type CommandResult, type FfmpegStatus, type IntegrationStatus, type ProxyCheck, type ProxyMode, type ProxyRoute, type SavedLogin, type Settings, type SoundEvent, type UpdateInfo } from "../api";
  import { SPEED_PRESETS, limitLabel, relativeTime } from "../format";
  import { CATEGORIES } from "../categories";
  import { toasts } from "../store.svelte";
  import { theme, type ThemePref } from "../theme.svelte";
  import CategoryEditor from "./CategoryEditor.svelte";
  import Modal from "./Modal.svelte";
  import SkinPicker from "./SkinPicker.svelte";

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
        .catch(() => (ffmpeg = { path: null, version: null, ok: false, missing: [t("settings-siler-couldn-t-check-it")] }));
  });

  async function browse() {
    const picked = await open({ directory: true, defaultPath: s.download_dir, title: t("settings-download-folder") });
    if (typeof picked === "string") s.download_dir = picked;
  }

  async function install() {
    try {
      integration = await api.installIntegration();
      toasts.show(t("settings-browser-integration-installed"), "success");
    } catch (e) {
      toasts.error(e);
    }
  }

  async function installExtension() {
    try {
      await api.installExtension();
      toasts.show(t("settings-firefox-will-ask-to-add-the"), "success");
    } catch (e) {
      toasts.error(e);
    }
  }

  const RELEASES = "https://github.com/sekhnat/Siler/releases/latest";

  /** A language's own name ("Deutsch"), or its tag when the browser engine doesn't know it. */
  function languageName(tag: string): string {
    if (tag === "en-XA") return "Pseudo (en-XA, finds text not in the catalog)";
    try {
      return new Intl.DisplayNames([tag], { type: "language" }).of(tag) ?? tag;
    } catch {
      return tag;
    }
  }

  let chromium = $state<{ path: string; browsers: string[] } | null>(null);
  async function prepareChromium() {
    try {
      chromium = await api.chromiumExtension();
    } catch (e) {
      toasts.error(e);
    }
  }

  let appInfo = $state<AppInfo | null>(null);
  let checked = $state<UpdateInfo | null>(null);
  let checking = $state(false);
  api.appInfo().then((a) => (appInfo = a)).catch(() => {});
  api.updateStatus().then((u) => (checked = u)).catch(() => {});

  async function checkNow() {
    checking = true;
    try {
      checked = await api.checkUpdatesNow();
      if (!checked.newer) toasts.show(t("settings-latest-version", { version: checked.current }), "success");
    } catch (e) {
      toasts.error(e);
    } finally {
      checking = false;
    }
  }

  // The extension and the app speak protocol versions; tell the user which one is behind.
  const mismatch = $derived.by(() => {
    const i = integration;
    if (!i || i.extension_protocol == null || i.extension_protocol === i.protocol) return null;
    return i.extension_protocol < i.protocol ? t("settings-update-the-browser-extension") : t("settings-update-siler");
  });

  async function save() {
    saving = true;
    try {
      const saved = await api.setSettings(s);
      setChosenLanguage(saved.language);
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
    { key: "general", label: t("settings-general") },
    { key: "transfers", label: t("settings-transfers") },
    { key: "media", label: t("settings-media") },
    { key: "network", label: t("settings-network") },
    { key: "categories", label: t("settings-categories") },
    { key: "browser", label: t("settings-browser") },
    { key: "appearance", label: t("settings-appearance") },
  ];
  const themes: { key: ThemePref; label: string; icon: typeof Sun }[] = [
    { key: "system", label: t("settings-system"), icon: Monitor },
    { key: "light", label: t("settings-light"), icon: Sun },
    { key: "dark", label: t("settings-dark"), icon: Moon },
  ];
  const soundRows: { event: SoundEvent; label: string; on: "on_complete" | "on_failed" | "on_queue_done"; file: "complete_file" | "failed_file" | "queue_done_file" }[] = [
    { event: "complete", label: t("settings-a-download-finished"), on: "on_complete", file: "complete_file" },
    { event: "failed", label: t("settings-a-download-failed"), on: "on_failed", file: "failed_file" },
    { event: "queue_done", label: t("settings-a-queue-finished"), on: "on_queue_done", file: "queue_done_file" },
  ];

  async function pickSound(file: "complete_file" | "failed_file" | "queue_done_file") {
    const picked = await open({
      title: t("settings-choose-a-sound"),
      filters: [{ name: t("settings-sounds"), extensions: ["oga", "ogg", "wav", "mp3", "flac", "opus"] }],
    });
    if (typeof picked === "string") s.sounds[file] = picked;
  }

  const baseName = (p: string) => p.split("/").pop() ?? p;

  let testing = $state(false);
  let testResult = $state<CommandResult | null>(null);
  async function testCommand() {
    const path = await open({ title: t("settings-try-the-command-on-a-file") });
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
    { key: "none", label: t("settings-no-proxy") },
    { key: "system", label: t("settings-system") },
    { key: "manual", label: t("settings-manual") },
    { key: "script", label: t("settings-proxy-script") },
    { key: "detect", label: t("settings-proxy-detect") },
  ];
  let testUrl = $state("");
  let checkingProxy = $state(false);
  let tested = $state<ProxyCheck | { error: string } | null>(null);

  async function testProxy() {
    if (!testUrl.trim() || checkingProxy) return;
    checkingProxy = true;
    tested = null;
    try {
      tested = await api.checkProxy($state.snapshot(s.proxy), testUrl.trim());
    } catch (e) {
      tested = { error: String(e) };
    } finally {
      checkingProxy = false;
    }
  }

  function whyText(why: ProxyCheck["why"]): string {
    switch (why.kind) {
      case "no_proxy":
        return t("settings-proxy-why-none");
      case "configured":
        return why.source === "settings"
          ? t("settings-proxy-why-settings")
          : t("settings-proxy-why-system", { source: why.source });
      case "excepted":
        return t("settings-proxy-why-excepted");
      case "this_computer":
        return t("settings-proxy-why-this-computer");
      case "script":
        return t("settings-proxy-why-script", { from: why.from });
      case "detected":
        return t("settings-proxy-why-detected", { from: why.from });
      case "nothing_detected":
        return t("settings-proxy-why-not-detected", { reason: why.reason });
    }
  }
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

<Modal title={t("settings-settings")} width={660} {onclose}>
  <div class="tabs">
    {#each tabs as t (t.key)}
      <button class:on={tab === t.key} onclick={() => (tab = t.key)}>{t.label}</button>
    {/each}
  </div>

  <div class="pane">
    {#if tab === "general"}
      <div class="field">
        <span class="label">{t("settings-download-folder")}</span>
        <div class="folder">
          <input class="input" bind:value={s.download_dir} spellcheck="false" />
          <button class="btn" onclick={browse}><FolderOpen size={15} /> {t("settings-browse")}</button>
        </div>
      </div>
      {@render toggle(t("settings-sort-into-category-folders"), t("settings-video-music-documents-inside-the-download"), s.use_category_folders, (v) => (s.use_category_folders = v))}
      <div class="field">
        <span class="label">{t("settings-when-a-file-with-the-same")}</span>
        <select class="input" bind:value={s.on_conflict}>
          <option value="ask">{t("settings-ask-me")}</option>
          <option value="rename">{t("settings-keep-both-add-a-number")}</option>
          <option value="overwrite">{t("settings-replace-the-old-file")}</option>
          <option value="skip">{t("settings-skip-the-download")}</option>
        </select>
      </div>
      {@render toggle(
        t("settings-keep-running-in-the-tray"),
        integration && !integration.tray_available ? t("settings-no-system-tray-was-found-so") : t("settings-closing-the-window-keeps-downloads-going"),
        s.close_to_tray,
        (v) => (s.close_to_tray = v),
      )}
      {@render toggle(t("settings-tray-panel"), t("settings-tray-panel-hint"), s.tray_opens_panel, (v) => (s.tray_opens_panel = v))}
      {@render toggle(
        t("settings-start-when-you-log-in"),
        integration && !integration.tray_available ? t("settings-opens-the-window-when-you-log") : t("settings-opens-in-the-tray-ready-for"),
        autostart,
        (v) => (autostart = v),
      )}
      {@render toggle(t("settings-ask-before-quitting"), t("settings-confirm-when-quitting-would-pause-running"), s.confirm_quit, (v) => (s.confirm_quit = v))}
      {@render toggle(t("settings-notifications"), t("settings-notify-when-a-download-finishes-or"), s.notify_on_complete, (v) => (s.notify_on_complete = v))}
      {@render toggle(t("settings-resume-on-startup"), t("settings-continue-unfinished-downloads-when-siler-starts"), s.resume_on_startup, (v) => (s.resume_on_startup = v))}
      {@render toggle(t("settings-keep-the-computer-awake"), t("settings-don-t-let-it-go-to"), s.prevent_sleep, (v) => (s.prevent_sleep = v))}
      {@render toggle(
        t("settings-watch-the-clipboard-for-links"),
        t("settings-copying-a-link-to-a-file"),
        s.clipboard_monitor,
        (v) => (s.clipboard_monitor = v),
      )}
      <div class="field">
        <span class="label">{t("settings-language")}</span>
        <select class="input" bind:value={s.language}>
          <option value="">{t("settings-language-system")}</option>
          {#each LANGUAGES as lang (lang)}
            <option value={lang}>{languageName(lang)}</option>
          {/each}
        </select>
        {#if s.language !== settings.language}<div class="hint">{t("settings-language-restart")}</div>{/if}
      </div>
      <div class="field updates">
        <span class="label">{t("settings-updates")}</span>
        <div class="upd-row">
          <span class="hint grow">
            {(appInfo?.install === "packaged" ? t("settings-siler-packaged", { version: appInfo?.version ?? "" }) : t("settings-siler", { version: appInfo?.version ?? "" }))}
            {#if checked}· {checked.newer ? t("settings-is-available", { version: checked.latest }) : t("settings-up-to-date")}{/if}
          </span>
          <button class="btn btn-sm" disabled={checking} onclick={checkNow}>{checking ? t("settings-checking") : t("settings-check-now")}</button>
        </div>
      </div>
      {@render toggle(
        t("settings-check-for-updates-every-day"),
        t("settings-asks-github-whether-a-newer-siler"),
        s.check_updates,
        (v) => (s.check_updates = v),
      )}
      <div class="logs">
        <FileText size={15} />
        <span class="hint grow truncate" title={integration?.log_dir}>{t("settings-logs", { dir: integration?.log_dir ?? "~/.local/state/siler/logs" })}</span>
        <button class="btn btn-sm" onclick={() => api.openLogFolder().catch(toasts.error.bind(toasts))}>{t("settings-open-log-folder")}</button>
      </div>
    {:else if tab === "media"}
      <div class="field">
        <span class="label">{t("settings-ffmpeg")}</span>
        {#if ffmpeg === null}
          <div class="hint">{t("settings-checking")}</div>
        {:else if ffmpeg.ok}
          <div class="hint"><CircleCheck size={13} /> {(ffmpeg.version ? t("settings-version-at", { version: ffmpeg.version, path: ffmpeg.path }) : t("settings-at", { path: ffmpeg.path }))}</div>
        {:else}
          <div class="hint warn"><TriangleAlert size={13} /> {(ffmpeg.missing.length ? t("settings-ffmpeg-isn-t-usable-single-track", { missing: ffmpeg.missing.join(", ") }) : t("settings-ffmpeg-not-found"))}</div>
          <div class="hint">{t("settings-build-the-bundled-copy-with")} <code>scripts/build-ffmpeg.sh</code>.</div>
        {/if}
      </div>
      <div class="field">
        <span class="label">{t("settings-pieces-fetched-at-once")} <b>{s.media_segment_concurrency}</b></span>
        <input type="range" min="1" max="16" bind:value={s.media_segment_concurrency} />
        <div class="hint">{t("settings-segments-of-one-stream-downloaded-in")}</div>
      </div>
      <label class="field">
        <span class="label">{t("settings-highest-video-quality-by-default")}</span>
        <select class="input" bind:value={s.preferred_max_height}>
          <option value={0}>{t("settings-no-limit")}</option>
          <option value={1080}>1080p</option>
          <option value={720}>720p</option>
          <option value={480}>480p</option>
          <option value={360}>360p</option>
        </select>
      </label>
      <div class="grid2">
        <label class="field">
          <span class="label">{t("settings-preferred-audio-language")}</span>
          <input class="input" bind:value={s.preferred_audio_language} placeholder={t("settings-en")} spellcheck="false" />
        </label>
        <label class="field">
          <span class="label">{t("settings-preferred-subtitle-language")}</span>
          <input class="input" bind:value={s.preferred_subtitle_language} placeholder={t("settings-none")} spellcheck="false" />
        </label>
      </div>
      <label class="field">
        <span class="label">{t("settings-default-container")}</span>
        <select class="input" bind:value={s.default_media_container}>
          <option value="auto">{t("settings-automatic-mp4-or-mkv-when-needed")}</option>
          <option value="mp4">{t("settings-mp4")}</option>
          <option value="mkv">{t("settings-mkv")}</option>
        </select>
      </label>
      <label class="field">
        <span class="label">{t("settings-subtitles-by-default")}</span>
        <select class="input" bind:value={s.default_subtitle_mode}>
          <option value="embed">{t("settings-inside-the-video")}</option>
          <option value="sidecar">{t("settings-as-a-separate-file")}</option>
          <option value="none">{t("settings-leave-out")}</option>
        </select>
      </label>
      {@render toggle(
        t("settings-allow-streams-from-this-computer-or"),
        t("settings-off-by-default-so-a-playlist"),
        s.allow_private_media_addresses,
        (v) => (s.allow_private_media_addresses = v),
      )}
    {:else if tab === "transfers"}
      <div class="field">
        <span class="label">{t("settings-connections-per-download")} <b>{s.connections}</b></span>
        <input type="range" min="1" max="32" bind:value={s.connections} />
        <div class="hint">{t("settings-more-connections-help-on-fast-links")}</div>
      </div>
      <div class="field">
        <span class="label">{t("settings-simultaneous-downloads")} <b>{s.max_concurrent}</b></span>
        <input type="range" min="1" max="10" bind:value={s.max_concurrent} />
      </div>
      <div class="field">
        <span class="label">{t("settings-connections-per-server")} <b>{s.max_connections_per_server || t("settings-no-limit")}</b></span>
        <input type="range" min="0" max="64" bind:value={s.max_connections_per_server} />
        <div class="hint">{t("settings-shared-by-all-downloads-from-one")}</div>
        {#each s.server_limits as limit, i (i)}
          <div class="limit">
            <input class="input" bind:value={limit.host} placeholder={t("settings-example-com")} spellcheck="false" />
            <input class="input num" type="number" min="1" max="128" bind:value={limit.connections} title={t("settings-connections")} />
            <button class="icon-btn sm" title={t("settings-remove")} onclick={() => (s.server_limits = s.server_limits.filter((_, j) => j !== i))}><X size={15} /></button>
          </div>
        {/each}
        <button class="btn btn-sm add-limit" onclick={addLimit}><Plus size={14} /> {t("settings-different-limit-for-a-server")}</button>
      </div>
      <div class="field">
        <span class="label">{t("settings-http2-connections")} <b>{s.http2_connections_per_server || t("settings-http2-off")}</b></span>
        <input type="range" min="0" max="16" bind:value={s.http2_connections_per_server} />
        <div class="hint">{t("settings-http2-connections-hint")}</div>
      </div>
      <div class="row2">
        <div class="field">
          <span class="label">{t("settings-global-speed-limit")}</span>
          <select class="input" value={customLimit ? -1 : s.global_speed_limit} onchange={(e) => {
            const v = +e.currentTarget.value;
            if (v >= 0) s.global_speed_limit = v;
          }}>
            {#each SPEED_PRESETS as p}<option value={p}>{limitLabel(p)}</option>{/each}
            {#if customLimit}<option value={-1}>{limitLabel(s.global_speed_limit)}</option>{/if}
          </select>
        </div>
        <div class="field">
          <span class="label">{t("settings-retries-per-connection")}</span>
          <input class="input" type="number" min="0" max="100" bind:value={s.max_retries} />
        </div>
      </div>
      <div class="field">
        <span class="label">{t("settings-user-agent")}</span>
        <input class="input mono" bind:value={s.user_agent} spellcheck="false" />
        <div class="hint">{t("settings-used-for-links-added-by-hand")}</div>
      </div>
      {@render toggle(t("settings-extra-safe-resume"), t("settings-extra-safe-resume-hint"), s.durable_checkpoints, (v) => (s.durable_checkpoints = v))}
      {@render toggle(t("settings-auto-refresh-links"), t("settings-auto-refresh-links-hint"), s.auto_refresh_links, (v) => (s.auto_refresh_links = v))}
      {@render toggle(t("settings-data-quota"), t("settings-download-at-most-so-much-in"), s.quota.enabled, (v) => (s.quota.enabled = v))}
      {#if s.quota.enabled}
        <div class="quota">
          <span class="hint">{t("settings-at-most")}</span>
          <input class="input num" type="number" min="1" bind:value={s.quota.megabytes} />
          <span class="hint">{t("settings-mb-every")}</span>
          <input class="input num" type="number" min="1" max="744" bind:value={s.quota.hours} />
          <span class="hint">{s.quota.hours === 1 ? "hour" : "hours"}</span>
        </div>
      {/if}
      {@render toggle(t("settings-run-a-command-after-each-download"), t("settings-for-example-to-unpack-scan-or"), s.after_download.enabled, (v) => (s.after_download.enabled = v))}
      {#if s.after_download.enabled}
        <div class="command">
          <input class="input mono" bind:value={s.after_download.command} placeholder={"my-tool --file {path}"} spellcheck="false" />
          <div class="hint">
            <code>{"{path}"}</code> {t("settings-the-file")} <code>{"{dir}"}</code> {t("settings-its-folder")} <code>{"{name}"}</code> {t("settings-its-name")} <code>{"{url}"}</code> {t("settings-its-address")}
            <code>{"{size}"}</code> {t("settings-bytes-quote-words-with-spaces-use")} <code>sh -c '…' _ {"{path}"}</code> {t("settings-for-a-shell")}
          </div>
          <div class="chips">
            <span class="hint">{t("settings-only-for")}</span>
            {#each CATEGORIES as c (c.key)}
              <button class="chip" class:on={s.after_download.categories.includes(c.key)} style:--c={c.color} onclick={() => toggleCommandCategory(c.key)}>{c.label}</button>
            {/each}
            {#if !s.after_download.categories.length}<span class="hint">{t("settings-every-download")}</span>{/if}
          </div>
          <div class="cmd-row">
            <button class="toggle small-toggle" onclick={() => (s.after_download.notify_on_failure = !s.after_download.notify_on_failure)}>
              <span class="switch" class:on={s.after_download.notify_on_failure}></span> {t("settings-notify-when-it-fails")}
            </button>
            <span class="grow"></span>
            <button class="btn btn-sm" onclick={testCommand} disabled={!s.after_download.command.trim() || testing}><Play size={13} /> {testing ? t("settings-running") : t("settings-try-it-on-a-file")}</button>
          </div>
          {#if testResult}
            <div class="test" class:bad={testResult.error != null || testResult.exit_code !== 0}>
              {testResult.error ?? t("settings-exited-with", { code: testResult.exit_code ?? "" })}
              {#if testResult.output}<pre>{testResult.output}</pre>{/if}
            </div>
          {/if}
        </div>
      {/if}
      {@render toggle(t("settings-open-metalinks"), t("settings-a-downloaded-meta4-or-metalink-file"), s.open_metalinks, (v) => (s.open_metalinks = v))}
    {:else if tab === "network"}
      <div class="field">
        <span class="label">{t("settings-proxy")}</span>
        <div class="seg proxy-modes">
          {#each proxyModes as m (m.key)}
            <button class:on={s.proxy.mode === m.key} onclick={() => (s.proxy.mode = m.key)}>{m.label}</button>
          {/each}
        </div>
      </div>
      {#if s.proxy.mode === "manual"}
        <div class="proxy">
          <label class="field">
            <span class="label">{t("settings-type")}</span>
            <select class="input" bind:value={s.proxy.scheme}>
              <option value="http">{t("settings-http")}</option>
              <option value="https">{t("settings-https")}</option>
              <option value="socks4">{t("settings-socks4")}</option>
              <option value="socks4a">{t("settings-socks4a")}</option>
              <option value="socks5">{t("settings-socks5")}</option>
            </select>
          </label>
          <label class="field">
            <span class="label">{t("settings-address")}</span>
            <input class="input" bind:value={s.proxy.host} placeholder={t("settings-proxy-example-com")} spellcheck="false" />
          </label>
          <label class="field">
            <span class="label">{t("settings-port")}</span>
            <input class="input" type="number" min="1" max="65535" bind:value={s.proxy.port} />
          </label>
        </div>
        <div class="row2">
          <label class="field">
            <span class="label">{t("settings-username")} <span class="hint">{t("settings-optional")}</span></span>
            <input class="input" bind:value={s.proxy.username} autocomplete="off" spellcheck="false" />
          </label>
          <label class="field">
            <span class="label">{t("settings-password")}</span>
            <input
              class="input"
              type="password"
              bind:value={proxyPassword}
              disabled={!s.proxy.username.trim() || !keyring}
              placeholder={!keyring ? t("settings-no-keyring-available") : proxyPasswordSaved ? t("settings-saved-in-the-keyring") : ""}
              autocomplete="new-password"
            />
          </label>
        </div>
        <div class="proxy ftp">
          <label class="field">
            <span class="label">{t("settings-ftp-proxy")} <span class="hint">{t("settings-optional")}</span></span>
            <input class="input" bind:value={s.proxy.ftp_host} title={t("settings-ftp-proxy-hint")} spellcheck="false" />
          </label>
          <label class="field">
            <span class="label">{t("settings-port")}</span>
            <input class="input" type="number" min="1" max="65535" bind:value={s.proxy.ftp_port} />
          </label>
        </div>
        <label class="field">
          <span class="label">{t("settings-connect-directly-to")}</span>
          <input class="input" bind:value={s.proxy.no_proxy} placeholder={t("settings-localhost-127-0-0-1-lan")} spellcheck="false" />
        </label>
      {/if}
      {#if s.proxy.mode === "script"}
        <label class="field">
          <span class="label">{t("settings-script-address")}</span>
          <input class="input" bind:value={s.proxy.script} placeholder={t("settings-script-placeholder")} spellcheck="false" />
          <span class="hint">{t("settings-script-hint")}</span>
        </label>
      {:else if s.proxy.mode === "detect"}
        <div class="hint">{t("settings-detect-hint")}</div>
      {/if}
      {#if route && !proxyEdited}
        <div class="route hint">
          <span>{t("settings-now", { summary: route.summary })}</span>
          {#if route.note}<span class="warn"><TriangleAlert size={13} /> {route.note}</span>{/if}
        </div>
      {:else if proxyEdited}
        <div class="route hint">{t("settings-applies-when-you-save")}</div>
      {/if}
      <div class="field">
        <span class="label">{t("settings-proxy-test")}</span>
        <div class="proxy-test">
          <input
            class="input"
            bind:value={testUrl}
            placeholder={t("settings-proxy-test-placeholder")}
            spellcheck="false"
            onkeydown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                testProxy();
              }
            }}
          />
          <button type="button" class="btn" onclick={testProxy} disabled={!testUrl.trim() || checkingProxy}>{t("settings-proxy-test-run")}</button>
        </div>
        {#if tested && "error" in tested}
          <div class="tested warn" role="status"><TriangleAlert size={13} /> {tested.error}</div>
        {:else if tested}
          <div class="tested" role="status">
            <b>{tested.via.map((v) => (v === "DIRECT" ? t("settings-proxy-direct") : v)).join(" → ")}</b>
            <span class="hint">{whyText(tested.why)}</span>
          </div>
        {/if}
      </div>

      <div class="field">
        <span class="label">{t("settings-saved-logins")}</span>
        {#if !keyring}
          <div class="hint">{t("settings-no-keyring-service-kwallet-gnome-keyring")}</div>
        {:else if logins.length === 0}
          <div class="hint">{t("settings-when-a-server-asks-for-a")}</div>
        {:else}
          <div class="logins">
            {#each logins as l (l.host)}
              <div class="login-row">
                <KeyRound size={14} />
                <span class="grow truncate"><b>{l.host.startsWith("proxy+") ? t("settings-login-proxy", { host: l.host.slice(6) }) : l.host}</b> <span class="hint">{l.username || t("settings-kerberos-ticket")}</span></span>
                <button class="btn btn-sm btn-ghost" onclick={() => forgetLogin(l.host)}>{t("settings-forget")}</button>
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
              {t("settings-the-extension-and-siler-are-from", { extension_version: integration?.extension_version, app_version: integration?.app_version })}
            </div>
          </div>
        {:else if integration?.manifest_installed && integration.host_exists}
          <CircleCheck size={20} />
          <div>
            <div class="t-label">{t("settings-browser-integration-is-installed")}</div>
            <div class="hint">
              {#if integration.last_contact}
                {t("settings-extension-last-seen", { v: integration.extension_version ?? "", v2: relativeTime(integration.last_contact) })}
              {:else}
                {t("settings-waiting-for-the-extension-to-connect")}
              {/if}
            </div>
          </div>
        {:else}
          <TriangleAlert size={20} />
          <div>
            <div class="t-label">{t("settings-browser-integration-is-not-set-up")}</div>
            <div class="hint">{t("settings-install-the-native-messaging-host-so")}</div>
          </div>
        {/if}
        <button class="btn btn-sm" onclick={install}>{integration?.manifest_installed ? t("settings-repair") : t("settings-install")}</button>
      </div>
      {#if integration}
        <div class="checks">
          {#each integration.browsers as b (b.name)}
            <span class="check" class:ok={b.installed} title={b.manifest_path}>
              {#if b.installed}<CircleCheck size={13} />{:else}<CircleX size={13} />{/if}
              {b.name}
            </span>
          {/each}
          <span class="check" class:ok={integration.desktop_entry} title={t("settings-application-menu-entry-and-icon")}>
            {#if integration.desktop_entry}<CircleCheck size={13} />{:else}<CircleX size={13} />{/if}
            {t("settings-app-menu-icon")}
          </span>
        </div>
      {/if}
      {@render toggle(t("settings-start-browser-downloads-immediately"), t("settings-skip-the-confirmation-window-for-files"), s.auto_start_browser_downloads, (v) => (s.auto_start_browser_downloads = v))}
      <div class="steps">
        <div class="t-label"><Puzzle size={15} /> {t("settings-firefox-librewolf-waterfox-floorp-zen")}</div>
        {#if integration?.signed_xpi}
          <div class="xpi">
            <span class="hint grow">{t("settings-the-signed-siler-extension-is-ready")}</span>
            <button class="btn btn-primary btn-sm" onclick={installExtension}>{t("settings-add-to-firefox")}</button>
          </div>
        {:else}
          <ol>
            <li>{t("settings-click")} <b>{t("settings-install")}</b> {t("settings-above-to-register-siler-with-firefox")}</li>
            <li>
              {t("settings-download")} <span class="mono">{t("settings-siler-integration-xpi")}</span> {t("settings-from-the-latest-release")}
              <button class="btn btn-sm" onclick={() => api.openUrl(RELEASES).catch((e) => toasts.error(e))}>{t("settings-open-releases")}</button>
            </li>
            <li>{t("settings-open-the-file-with-firefox-and")}</li>
          </ol>
          <div class="hint">{t("settings-building-from-source-see")} <span class="mono">{t("settings-docs-browser-extension-md")}</span>.</div>
        {/if}
      </div>
      <div class="steps">
        <div class="t-label"><Puzzle size={15} /> {t("settings-chrome-chromium-brave-vivaldi-edge-opera")}</div>
        {#if chromium}
          <ol>
            <li>{t("settings-click")} <b>{t("settings-install")}</b> {t("settings-above-again-if-the-browser-was")}</li>
            <li>
              {t("settings-open-the-browser-s-extensions-page")} <b>{t("settings-developer-mode")}</b>.
              {#each chromium.browsers as b (b)}
                <button class="btn btn-sm" onclick={() => api.openChromiumExtensions(b).catch((e) => toasts.error(e))}>{t("settings-open-in", { b })}</button>
              {/each}
            </li>
            <li>
              {t("settings-click")} <b>{t("settings-load-unpacked")}</b> {t("settings-and-choose-this-folder")}
              <span class="mono selectable">{chromium.path}</span>
              <button class="btn btn-sm" onclick={() => navigator.clipboard.writeText(chromium!.path).then(() => toasts.show(t("settings-copied"), "success"))}>{t("settings-copy")}</button>
            </li>
          </ol>
          <div class="hint">{t("settings-siler-s-extension-isn-t-in")}</div>
        {:else}
          <div class="xpi">
            <span class="hint grow">{t("settings-siler-s-extension-for-chromium-based")}</span>
            <button class="btn btn-sm" onclick={prepareChromium}>{t("settings-set-up")}</button>
          </div>
        {/if}
      </div>
    {:else}
      <div class="field">
        <span class="label">{t("settings-theme")}</span>
        <div class="seg">
          {#each themes as t (t.key)}
            <button class:on={theme.pref === t.key} onclick={() => theme.set(t.key)}><t.icon size={15} /> {t.label}</button>
          {/each}
        </div>
      </div>
      <SkinPicker bind:skin={s.ui.skin} bind:toolbar={s.ui.toolbar_style} />
      {@render toggle(t("settings-sounds"), t("settings-play-the-desktop-s-sounds-for"), s.sounds.enabled, (v) => (s.sounds.enabled = v))}
      {#if s.sounds.enabled}
        <div class="sounds">
          {#each soundRows as r (r.event)}
            <div class="sound-row">
              <label class="sound-check">
                <input type="checkbox" bind:checked={s.sounds[r.on]} />
                {r.label}
              </label>
              <span class="hint grow truncate" title={s.sounds[r.file] ?? ""}>{s.sounds[r.file] ? baseName(s.sounds[r.file] ?? "") : t("settings-theme-sound")}</span>
              {#if s.sounds[r.file]}
                <button class="icon-btn sm" title={t("settings-use-the-theme-sound")} onclick={() => (s.sounds[r.file] = null)}><X size={14} /></button>
              {/if}
              <button class="btn btn-sm" onclick={() => pickSound(r.file)}>{t("settings-choose")}</button>
              <button class="icon-btn sm" title={t("settings-play")} onclick={() => api.testSound(r.event, $state.snapshot(s.sounds))}><Play size={14} /></button>
            </div>
          {/each}
        </div>
      {/if}
    {/if}
  </div>

  {#snippet footer()}
    <button class="btn btn-ghost" onclick={onclose}>{t("settings-cancel")}</button>
    <button class="btn btn-primary" onclick={save} disabled={saving}>{t("settings-save")}</button>
  {/snippet}
</Modal>

<style>
  .upd-row {
    display: flex;
    align-items: center;
    gap: 10px;
  }
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
  .proxy.ftp {
    grid-template-columns: 1fr 100px;
  }
  .proxy-modes {
    flex-wrap: wrap;
  }
  .proxy-test {
    display: flex;
    gap: 8px;
  }
  .proxy-test .input {
    flex: 1;
    min-width: 0;
  }
  .tested {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    column-gap: 8px;
    margin-top: 6px;
    font-size: 12.5px;
    overflow-wrap: anywhere;
  }
  .tested.warn {
    color: var(--warn);
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
