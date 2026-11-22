<script lang="ts">
  import { t } from "../i18n";
  import { FolderOpen, OctagonX, Power } from "@lucide/svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { api } from "../api";
  import Logo from "./Logo.svelte";

  let { message }: { message: string } = $props();
</script>

<div class="screen">
  <div class="card">
    <div class="brand"><Logo size={28} /> {t("fatal-screen-siler")}</div>
    <div class="icon"><OctagonX size={30} /></div>
    <h1>{t("fatal-screen-siler-couldn-t-start")}</h1>
    <pre class="selectable">{message}</pre>
    <p class="hint">
      {t("fatal-screen-your-downloads-are-safe-on-disk")}
    </p>
    <div class="actions">
      <button class="btn" onclick={() => api.openLogFolder().catch(() => {})}><FolderOpen size={15} /> {t("fatal-screen-open-log-folder")}</button>
      <button class="btn btn-primary" onclick={() => getCurrentWindow().destroy()}><Power size={15} /> {t("fatal-screen-quit")}</button>
    </div>
  </div>
</div>

<style>
  .screen {
    height: 100%;
    display: grid;
    place-items: center;
    padding: 24px;
    background: var(--bg);
  }
  .card {
    width: min(560px, 100%);
    padding: 28px;
    border-radius: var(--r-xl);
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: var(--shadow-lg);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 700;
    margin-bottom: 22px;
  }
  .icon {
    color: var(--err);
  }
  h1 {
    margin: 8px 0 12px;
    font-size: 20px;
    letter-spacing: -0.02em;
  }
  pre {
    margin: 0 0 12px;
    padding: 12px;
    border-radius: var(--r);
    background: var(--surface-2);
    border: 1px solid var(--border);
    white-space: pre-wrap;
    word-break: break-word;
    font-size: 12.5px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 18px;
  }
</style>
