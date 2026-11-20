<script lang="ts">
  import { CircleCheck, CircleX, FileText, FolderOpen, Monitor, Moon, Puzzle, Sun, TriangleAlert } from "@lucide/svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";
  import { api, type IntegrationStatus, type Settings } from "../api";
  import { SPEED_PRESETS, limitLabel, relativeTime } from "../format";
  import { toasts } from "../store.svelte";
  import { theme, type ThemePref } from "../theme.svelte";
  import CategoryEditor from "./CategoryEditor.svelte";
  import Modal from "./Modal.svelte";

  let { settings, onclose, onsaved }: { settings: Settings; onclose: () => void; onsaved: (s: Settings) => void } = $props();

  type Tab = "general" | "transfers" | "categories" | "browser" | "appearance";
  let tab = $state<Tab>("general");
  // Edit a copy; nothing changes until Save.
  // svelte-ignore state_referenced_locally
  let s = $state<Settings>(structuredClone($state.snapshot(settings)) as Settings);
  let integration = $state<IntegrationStatus | null>(null);
  let saving = $state(false);

  onMount(async () => {
    integration = await api.integrationStatus().catch(() => null);
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

  // The extension and the app speak protocol versions; tell the user which one is behind.
  const mismatch = $derived.by(() => {
    const i = integration;
    if (!i || i.extension_protocol == null || i.extension_protocol === i.protocol) return null;
    return i.extension_protocol < i.protocol ? "Update the Firefox extension" : "Update K-Star";
  });

  async function save() {
    saving = true;
    try {
      onsaved(await api.setSettings(s));
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
    { key: "categories", label: "Categories" },
    { key: "browser", label: "Browser" },
    { key: "appearance", label: "Appearance" },
  ];
  const themes: { key: ThemePref; label: string; icon: typeof Sun }[] = [
    { key: "system", label: "System", icon: Monitor },
    { key: "light", label: "Light", icon: Sun },
    { key: "dark", label: "Dark", icon: Moon },
  ];
  const customLimit = $derived(!SPEED_PRESETS.includes(s.global_speed_limit));
</script>

{#snippet toggle(label: string, hint: string, value: boolean, set: (v: boolean) => void)}
  <button class="toggle-row" onclick={() => set(!value)}>
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
        integration && !integration.tray_available ? "No system tray was found, so closing the window quits K-Star" : "Closing the window keeps downloads going",
        s.close_to_tray,
        (v) => (s.close_to_tray = v),
      )}
      {@render toggle("Ask before quitting", "Confirm when quitting would pause running downloads", s.confirm_quit, (v) => (s.confirm_quit = v))}
      {@render toggle("Notifications", "Notify when a download finishes or fails", s.notify_on_complete, (v) => (s.notify_on_complete = v))}
      {@render toggle("Resume on startup", "Continue unfinished downloads when K-Star starts", s.resume_on_startup, (v) => (s.resume_on_startup = v))}
      <div class="logs">
        <FileText size={15} />
        <span class="hint grow truncate" title={integration?.log_dir}>Logs: {integration?.log_dir ?? "~/.local/state/kstar/logs"}</span>
        <button class="btn btn-sm" onclick={() => api.openLogFolder().catch(toasts.error.bind(toasts))}>Open log folder</button>
      </div>
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
    {:else if tab === "categories"}
      <CategoryEditor bind:s />
    {:else if tab === "browser"}
      <div class="integration" class:ok={integration?.manifest_installed && integration?.host_exists && !mismatch}>
        {#if mismatch}
          <TriangleAlert size={20} />
          <div>
            <div class="t-label">{mismatch}</div>
            <div class="hint">
              The extension ({integration?.extension_version}) and K-Star ({integration?.app_version}) are from different versions and may not work together.
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
            <div class="hint">Install the native messaging host so the extension can talk to K-Star.</div>
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
            <span class="hint grow">A signed extension is ready and stays installed across Firefox restarts.</span>
            <button class="btn btn-primary btn-sm" onclick={installExtension}>Add to Firefox</button>
          </div>
        {:else}
          <ol>
            <li>Click <b>Install</b> above to register K-Star with Firefox.</li>
            <li>For a permanent install, sign the extension with <span class="mono">scripts/sign-extension.sh</span>; it then appears here.</li>
            <li>For testing, load <span class="mono">extension/manifest.json</span> in <span class="mono">about:debugging</span> (removed when Firefox restarts).</li>
            <li>Grant the extension access to all websites when Firefox asks.</li>
          </ol>
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
  .row2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .label b {
    float: right;
    color: var(--accent-text);
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
</style>
