<script lang="ts">
  import { ClipboardPaste, Plus } from "@lucide/svelte";

  let { filtered, onadd }: { filtered: boolean; onadd: () => void } = $props();
</script>

<div class="empty">
  <div class="art">
    <svg viewBox="0 0 120 120" width="120" height="120" aria-hidden="true">
      <defs>
        <linearGradient id="empty-g" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stop-color="#7c5cff" />
          <stop offset="1" stop-color="#22d3ee" />
        </linearGradient>
      </defs>
      <circle cx="60" cy="60" r="54" fill="url(#empty-g)" opacity="0.1" />
      <circle cx="60" cy="60" r="38" fill="url(#empty-g)" opacity="0.12" />
      <path d="M60 24 C 63 47, 69 52, 92 55 C 69 58, 63 63, 60 86 C 57 63, 51 58, 28 55 C 51 52, 57 47, 60 24 Z" fill="url(#empty-g)" />
      <rect x="40" y="94" width="40" height="7" rx="3.5" fill="url(#empty-g)" opacity="0.8" />
    </svg>
  </div>
  {#if filtered}
    <h3>Nothing here</h3>
    <p>No downloads match this view.</p>
  {:else}
    <h3>Ready when you are</h3>
    <p>Add a link, paste one with <kbd>Ctrl</kbd>+<kbd>V</kbd>, or click a download in Firefox with the Siler extension installed.</p>
    <div class="cta">
      <button class="btn btn-primary" onclick={onadd}><Plus size={16} /> Add URL</button>
      <span class="hint"><ClipboardPaste size={13} /> Paste anywhere</span>
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
