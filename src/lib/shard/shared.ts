import { asCmdError } from "../api";
import { tx } from "../i18n.svelte";

/** A backend error as text: known shard error codes are localized. */
export function errText(e: unknown): string {
  const err = asCmdError(e);
  return tx(`shard.err.${err.code}`, err.message);
}
