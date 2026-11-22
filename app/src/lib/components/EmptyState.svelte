<script lang="ts">
  import { t } from "../i18n";
  import { ClipboardPaste, Plus } from "@lucide/svelte";

  let { filtered, onadd }: { filtered: boolean; onadd: () => void } = $props();
</script>

<div class="empty">
  <div class="art">
    <svg viewBox="0 0 120 120" width="120" height="120" aria-hidden="true">
      <!-- The app icon's segmented S (app/assets/logo.svg), its top part filled like a download. -->
      <defs>
        <linearGradient id="empty-g" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stop-color="#5cc8e8" />
          <stop offset="1" stop-color="#3a9fc4" />
        </linearGradient>
        <path
          id="empty-s"
          d="M 342 188 C 342 150 304 128 256 128 C 204 128 172 152 172 188 C 172 232 226 244 256 256 C 292 270 342 282 342 326 C 342 366 306 386 256 386 C 208 386 170 366 170 326"
          fill="none"
          stroke-width="46"
          stroke-dasharray="54 16.3"
        />
        <clipPath id="empty-done">
          <rect x="0" y="0" width="512" height="292" />
        </clipPath>
      </defs>
      <circle cx="60" cy="60" r="54" fill="url(#empty-g)" opacity="0.1" />
      <circle cx="60" cy="60" r="38" fill="url(#empty-g)" opacity="0.12" />
      <g transform="translate(60 61) scale(0.19) translate(-256 -257)">
        <use href="#empty-s" class="track" />
        <g clip-path="url(#empty-done)">
          <use href="#empty-s" stroke="url(#empty-g)" />
        </g>
      </g>
    </svg>
  </div>
  {#if filtered}
    <h3>{t("empty-state-nothing-here")}</h3>
    <p>{t("empty-state-no-downloads-match-this-view")}</p>
  {:else}
    <h3>{t("empty-state-ready-when-you-are")}</h3>
    <p>{t("empty-state-add-a-link-paste-one-with")} <kbd>{t("empty-state-ctrl")}</kbd>+<kbd>V</kbd>{t("empty-state-or-click-a-download-in-the-browser")}</p>
    <div class="cta">
      <button class="btn btn-primary" onclick={onadd}><Plus size={16} /> {t("empty-state-add-url")}</button>
      <span class="hint"><ClipboardPaste size={13} /> {t("empty-state-paste-anywhere")}</span>
    </div>
  {/if}
</div>

<style>
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 40px;
    text-align: center;
  }
  .art {
    margin-bottom: 14px;
  }
  .track {
    stroke: var(--muted);
    opacity: 0.3;
  }
  h3 {
    margin: 0 0 6px;
    font-size: 17px;
    font-weight: 680;
    letter-spacing: -0.015em;
  }
  p {
    margin: 0;
    max-width: 380px;
    color: var(--muted);
  }
  kbd {
    padding: 1px 5px;
    border-radius: 5px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    font: 600 11px var(--font);
  }
  .cta {
    display: flex;
    align-items: center;
    gap: 14px;
    margin-top: 20px;
  }
  .hint {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
</style>
