<script lang="ts">
  import { fade, pop } from "../ui/motion";
  import Icon from "../ui/Icon.svelte";
  import type { RecentBundle } from "../api";
  import { onBack } from "../back.svelte";
  import { t } from "../i18n.svelte";
  import { prefs, savePrefs } from "../prefs.svelte";
  import PasswordManager from "./PasswordManager.svelte";
  import { biometricName, refreshVault, setBiometricUnlock, vault } from "./vault.svelte";

  let { recent, onclose }: { recent: RecentBundle[]; onclose: () => void } = $props();

  const THEMES = ["dark", "light", "system"] as const;
  let managing = $state(false);

  $effect(() => {
    refreshVault();
  });
  $effect(() => {
    if (!managing) return onBack(onclose);
  });

  const bio = $derived(vault.biometric);
  const bioOn = $derived(!!prefs.app?.security?.biometric_unlock);
</script>

<div out:fade class="scrim" role="presentation">
  <div out:pop class="dialog settings" role="dialog" aria-modal="true" aria-labelledby="start-settings-title">
    <header>
      <h2 id="start-settings-title">{t("start.settings")}</h2>
      <button class="icon-btn" aria-label={t("common.close")} onclick={onclose}><Icon name="x" size={16} /></button>
    </header>
    <div class="body">
      {#if prefs.app}
        <section>
          <div class="eyebrow">{t("start.settings.appearance")}</div>
          <div class="field">
            <span class="label">{t("set.theme")}</span>
            <div class="seg sm">
              {#each THEMES as th (th)}
                <button
                  class:on={prefs.app.ui.theme === th}
                  onclick={() => {
                    prefs.app!.ui.theme = th;
                    savePrefs(0);
                  }}>{t(`set.theme.${th}`)}</button
                >
              {/each}
            </div>
          </div>
        </section>

        <section>
          <div class="eyebrow">{t("vault.section")}</div>
          {#if bio.supported}
            <label class="field">
              <span class="grow">
                <span class="label">{t("vault.use")}</span>
                <span class="help">{t("vault.use.help", { kind: biometricName() })}</span>
              </span>
              <button
                type="button"
                class="switch"
                class:on={bioOn}
                disabled={!bio.available && !bioOn}
                aria-label={t("vault.use")}
                onclick={() => setBiometricUnlock(!bioOn)}
              ></button>
            </label>
            {#if vault.loaded && !bio.available}
              <p class="note"><Icon name="alert" size={14} />{t("vault.notEnrolled")}</p>
            {/if}
          {:else if vault.loaded}
            <p class="note"><Icon name="info" size={14} />{t("vault.unsupported")}</p>
          {/if}
          <div class="field">
            <span class="grow">
              <span class="label">{t("vault.manage")}</span>
              <span class="help">{t("vault.manage.help")}{vault.entries.length ? ` (${vault.entries.length})` : ""}</span>
            </span>
            <button class="btn sm" onclick={() => (managing = true)}><Icon name="key" size={14} />{t("vault.manage.open")}</button>
          </div>
        </section>
      {/if}
    </div>
  </div>
</div>

{#if managing}
  <PasswordManager {recent} onclose={() => (managing = false)} />
{/if}

<style>
  .settings {
    width: min(460px, 100%);
  }
  .body {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
  }
  section {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 10px;
  }
  .field {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
  }
  label.field {
    cursor: pointer;
  }
  .field > .grow {
    display: grid;
    min-width: 0;
  }
  .label {
    color: var(--fg);
  }
  .field > .label {
    flex: 1;
  }
  .help {
    font-size: 12px;
    color: var(--faint);
    line-height: 1.5;
  }
  .switch {
    flex-shrink: 0;
  }
  .note {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    margin: 0;
    padding: 10px 12px;
    border-radius: var(--radius-md);
    background: var(--surface-2);
    font-size: 12px;
    line-height: 1.5;
    color: var(--fg-2);
  }
  .note :global(svg) {
    flex-shrink: 0;
    margin-top: 2px;
  }
</style>
