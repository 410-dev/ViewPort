import { asCmdError, vaultApi, type BiometricStatus, type VaultEntry } from "../api";
import { tx } from "../i18n.svelte";
import { prefs, savePrefs } from "../prefs.svelte";

/** Remembered bundle passwords and this device's biometrics, shared by the start screen's dialogs. */
export const vault = $state<{ biometric: BiometricStatus; entries: VaultEntry[]; loaded: boolean }>({
  biometric: { available: false, supported: false, kind: "" },
  entries: [],
  loaded: false,
});

export async function refreshVault() {
  try {
    const s = await vaultApi.status();
    vault.biometric = s.biometric;
    vault.entries = s.entries;
  } catch {
    // Keep what we had; the dialogs still work without the list.
  }
  vault.loaded = true;
}

/** "Face ID", "Touch ID", … for the prompts. */
export function biometricName(): string {
  return tx(`vault.kind.${vault.biometric.kind}`, tx("vault.kind."));
}

/** Whether a saved password opens bundles now without typing it (after biometrics where there are any). */
export function savedPasswordsUsable(): boolean {
  const b = vault.biometric;
  if (!b.supported) return true;
  return b.available && !!prefs.app?.security?.biometric_unlock;
}

export function setBiometricUnlock(on: boolean) {
  if (!prefs.app) return;
  prefs.app.security = { ...prefs.app.security, biometric_unlock: on };
  savePrefs(0);
}

/** A user-facing message for a vault error (`vault.rs` codes), or the backend's own text. */
export function vaultError(e: unknown): string {
  const err = asCmdError(e);
  return tx(`vault.err.${err.code}`, err.message);
}
