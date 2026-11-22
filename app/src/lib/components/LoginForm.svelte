<script lang="ts">
  import { t } from "../i18n";
  import { KeyRound, TriangleAlert } from "@lucide/svelte";
  import { onMount } from "svelte";
  import { api, type LoginRequest } from "../api";
  import { toasts } from "../store.svelte";

  let {
    request,
    onloggedin,
    oncancel,
  }: {
    request: LoginRequest;
    /** Called after the login was handed to Siler (and saved, when asked). */
    onloggedin: () => void;
    oncancel?: () => void;
  } = $props();

  let username = $state("");
  let password = $state("");
  let remember = $state(true);
  let keyring = $state(true);
  let busy = $state(false);
  let userInput: HTMLInputElement | undefined = $state();

  /** "Use my Kerberos ticket for this site": never ticked for the user. */
  let wantsTicket = $state(false);
  const canTicket = $derived(request.asked === "ticket" || request.asked === "ticket_failed");
  const ticket = $derived(canTicket && wantsTicket);
  const windows = $derived(request.asked === "windows" || canTicket);
  const proxy = $derived(request.host.startsWith("proxy+") ? request.host.slice("proxy+".length) : null);
  const unencrypted = $derived(windows && (proxy ?? request.host).startsWith("http://"));

  onMount(async () => {
    keyring = await api.keyringAvailable().catch(() => false);
    if (!keyring) remember = false;
    userInput?.focus();
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if ((!ticket && !username.trim()) || busy) return;
    busy = true;
    try {
      await api.submitLogin(request.host, ticket ? "" : username.trim(), ticket ? "" : password, remember && keyring, ticket);
      onloggedin();
    } catch (err) {
      // The login is in use even when saving it failed.
      toasts.error(err);
      onloggedin();
    } finally {
      busy = false;
    }
  }
</script>

<form class="login" onsubmit={submit}>
  <div class="who">
    <KeyRound size={18} />
    <div class="min">
      <div class="t">
        {#if proxy}
          {t("login-form-the-proxy")} <b>{proxy}</b> {t("login-form-asks-for-a-username-and-password")}
        {:else}
          <b>{request.host}</b> {t("login-form-asks-for-a-username-and-password")}
        {/if}
      </div>
      {#if request.realm}<div class="hint truncate" title={request.realm}>“{request.realm}”</div>{/if}
    </div>
  </div>
  {#if request.rejected}
    <div class="refused">
      <TriangleAlert size={14} />
      {request.asked === "ticket_failed" ? t("login-form-ticket-failed") : t("login-form-the-server-didn-t-accept-the")}
    </div>
  {/if}
  {#if unencrypted}
    <div class="clear-text"><TriangleAlert size={14} /> {t("login-form-not-encrypted")}</div>
  {/if}
  {#if canTicket}
    <label class="check">
      <input type="checkbox" bind:checked={wantsTicket} />
      {proxy ? t("login-form-use-ticket-proxy") : t("login-form-use-ticket")}
    </label>
  {/if}
  <div class="grid">
    <label class="field">
      <span class="label">{t("login-form-username")}</span>
      <input
        bind:this={userInput}
        class="input"
        bind:value={username}
        disabled={ticket}
        placeholder={windows ? t("login-form-windows-placeholder") : undefined}
        autocomplete="username"
        spellcheck="false"
      />
    </label>
    <label class="field">
      <span class="label">{t("login-form-password")}</span>
      <input class="input" type="password" bind:value={password} disabled={ticket} autocomplete="current-password" />
    </label>
  </div>
  {#if windows}<div class="hint">{t("login-form-windows-hint")}</div>{/if}
  <label class="check" title={keyring ? "" : t("login-form-no-keyring-service")}>
    <input type="checkbox" bind:checked={remember} disabled={!keyring} />
    {t("login-form-remember-in-the-keyring")}
  </label>
  <div class="actions">
    {#if oncancel}<button type="button" class="btn btn-ghost" onclick={oncancel}>{t("login-form-cancel")}</button>{/if}
    <span class="grow"></span>
    <button type="submit" class="btn btn-primary" disabled={(!ticket && !username.trim()) || busy}>{t("login-form-log-in")}</button>
  </div>
</form>

<style>
  .login {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .who {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .who > :global(svg) {
    flex: none;
    color: var(--accent-text);
  }
  .min {
    min-width: 0;
    flex: 1;
  }
  .t {
    font-weight: 550;
    overflow-wrap: anywhere;
  }
  .refused,
  .clear-text {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--err);
  }
  .clear-text {
    color: var(--warn);
  }
  .refused > :global(svg),
  .clear-text > :global(svg) {
    flex: none;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .field {
    display: block;
    min-width: 0;
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    align-self: flex-start;
    font-size: 12.5px;
    color: var(--text-2);
  }
  .check input {
    accent-color: var(--accent);
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  .grow {
    flex: 1;
  }
</style>
