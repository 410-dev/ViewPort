<script lang="ts">
  import { audio, setVolume, toggleMute } from "../audio.svelte";
  import { t } from "../i18n.svelte";
  import Icon from "./Icon.svelte";

  /** Master volume: mute toggle plus a slider. Shared by the status bar and the viewer. */
  let { dark = false }: { dark?: boolean } = $props();

  const icon = $derived(audio.muted || audio.volume === 0 ? "volumeMute" : audio.volume < 0.5 ? "volumeLow" : "volume");
</script>

<span class="vc" class:dark>
  <button class="b" class:muted={audio.muted} title={audio.muted ? t("audio.unmute") : t("audio.mute")} onclick={toggleMute}>
    <Icon name={icon} size={15} />
  </button>
  <input
    type="range"
    min="0"
    max="1"
    step="0.01"
    value={audio.muted ? 0 : audio.volume}
    title={t("audio.volume", { n: Math.round(audio.volume * 100) })}
    aria-label={t("audio.volumeLabel")}
    oninput={(e) => setVolume(+e.currentTarget.value)}
  />
</span>

<style>
  .vc {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .b {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border: 0;
    border-radius: 50%;
    background: transparent;
    color: var(--muted);
    transition:
      background var(--t-fast) var(--ease),
      color var(--t-fast) var(--ease);
  }
  .b:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .b.muted {
    color: var(--warn);
  }
  .dark .b {
    width: 32px;
    height: 32px;
    color: var(--fg-2);
  }
  input {
    width: 76px;
    accent-color: var(--accent);
  }
  /* Phones have volume keys; only the mute button stays. */
  @media (max-width: 760px) {
    input {
      display: none;
    }
  }
</style>
