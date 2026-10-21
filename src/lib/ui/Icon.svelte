<script lang="ts" module>
  /** 24×24 stroke icons. Add new ones here; components refer to them by name. */
  export const ICONS = {
    grid: "M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z",
    list: "M8 6h12M8 12h12M8 18h12M4 6h.01M4 12h.01M4 18h.01",
    tree: "M4 5h6M7 5v12a2 2 0 0 0 2 2h3M7 11h5M15 11h5M15 19h5",
    folder: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
    folderPlus: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2zM12 10v6M9 13h6",
    server: "M4 5h16v5H4zM4 14h16v5H4zM8 7.5h.01M8 16.5h.01",
    image: "M4 5h16v14H4zM4 16l5-5 4 4 2-2 5 5M15 9.5a1.5 1.5 0 1 0 0-.01",
    video: "M4 6h12v12H4zM16 10l4-2.5v9L16 14",
    search: "M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14zM20 20l-4-4",
    sliders: "M4 7h10M18 7h2M4 17h4M12 17h8M14 4v6M8 14v6",
    code: "M9 8l-4 4 4 4M15 8l4 4-4 4",
    star: "M12 3.5l2.6 5.4 5.9.8-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8-4.3-4.1 5.9-.8z",
    heart: "M12 20s-7.5-4.6-9.2-9.4A4.9 4.9 0 0 1 12 6.4a4.9 4.9 0 0 1 9.2 4.2C19.5 15.4 12 20 12 20z",
    panel: "M4 5h16v14H4zM14 5v14",
    settings:
      "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z",
    x: "M6 6l12 12M18 6L6 18",
    users: "M15 19v-1a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v1M8.5 11a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7zM21 19v-1a4 4 0 0 0-3-3.9M15 4.1a3.5 3.5 0 0 1 0 6.8",
    copy: "M9 9h11v11H9zM5 15H4V4h11v1",
    plus: "M12 5v14M5 12h14",
    minus: "M5 12h14",
    check: "M5 12.5l4.5 4.5L19 7.5",
    refresh: "M20 11a8 8 0 0 0-14.3-4.9L4 8M4 4v4h4M4 13a8 8 0 0 0 14.3 4.9L20 16M20 20v-4h-4",
    pencil: "M4 20h4L19 9l-4-4L4 16zM13.5 6.5l4 4",
    trash: "M4 7h16M10 11v6M14 11v6M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12M9 7V4h6v3",
    chevronRight: "M9 6l6 6-6 6",
    chevronDown: "M6 9l6 6 6-6",
    chevronLeft: "M15 6l-6 6 6 6",
    arrowUp: "M12 19V5M6 11l6-6 6 6",
    arrowDown: "M12 5v14M6 13l6 6 6-6",
    lock: "M6 11h12v9H6zM8 11V8a4 4 0 0 1 8 0v3",
    logout: "M15 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3M10 16l4-4-4-4M14 12H4",
    save: "M5 4h11l3 3v13H5zM8 4v5h7V4M8 20v-6h8v6",
    play: "M7 5l12 7-12 7z",
    pause: "M7 5h3v14H7zM14 5h3v14h-3z",
    stop: "M6 6h12v12H6z",
    loop: "M17 3l3 3-3 3M4 11V9a3 3 0 0 1 3-3h13M7 21l-3-3 3-3M20 13v2a3 3 0 0 1-3 3H4",
    info: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zM12 11v5M12 8h.01",
    maximize: "M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5",
    eye: "M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12zM12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z",
    activity: "M3 12h4l3-8 4 16 3-8h4",
    alert: "M12 3l10 18H2zM12 10v5M12 18h.01",
    file: "M6 3h8l4 4v14H6zM14 3v4h4",
    filter: "M4 5h16l-6 8v6l-4-2v-4z",
    bookmark: "M6 4h12v17l-6-4-6 4z",
    home: "M4 11l8-7 8 7v9H4zM10 20v-6h4v6",
    archive: "M4 5h16v4H4zM5 9v10h14V9M10 13h4",
    key: "M14 7a4 4 0 1 1-3.9 5H7l-1 2H4v-3l6.1-.1A4 4 0 0 1 14 7zM15 10h.01",
    monitor: "M3 5h18v11H3zM8 20h8M12 16v4",
    cpu: "M7 7h10v10H7zM10 10h4v4h-4zM9 3v4M15 3v4M9 17v4M15 17v4M3 9h4M3 15h4M17 9h4M17 15h4",
    keyboard: "M3 7h18v10H3zM7 10h.01M11 10h.01M15 10h.01M7 14h10",
    type: "M5 6V4h14v2M12 4v16M9 20h6",
    palette:
      "M12 21a9 9 0 1 1 9-9c0 2-1.5 3-3 3h-2a2 2 0 0 0-1.5 3.3A1.6 1.6 0 0 1 12 21zM7.5 11h.01M10 7.5h.01M14 7.5h.01M16.5 11h.01",
    globe: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zM3 12h18M12 3c2.5 2.5 3.5 5.5 3.5 9s-1 6.5-3.5 9c-2.5-2.5-3.5-5.5-3.5-9s1-6.5 3.5-9z",
    database: "M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3zM4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3",
    film: "M4 4h16v16H4zM8 4v16M16 4v16M4 8h4M4 12h4M4 16h4M16 8h4M16 12h4M16 16h4",
    layers: "M12 3l9 5-9 5-9-5zM3 13l9 5 9-5",
    scan: "M4 8V5a1 1 0 0 1 1-1h3M16 4h3a1 1 0 0 1 1 1v3M20 16v3a1 1 0 0 1-1 1h-3M8 20H5a1 1 0 0 1-1-1v-3M4 12h16",
    zap: "M13 3L4 14h7l-1 7 9-11h-7z",
    sortAsc: "M4 6h7M4 12h5M4 18h3M17 5v14M14 16l3 3 3-3",
    sortDesc: "M4 6h3M4 12h5M4 18h7M17 5v14M14 8l3-3 3 3",
    dots: "M5 12h.01M12 12h.01M19 12h.01",
    shield: "M12 3l8 3v6c0 4.5-3.4 8.3-8 9-4.6-.7-8-4.5-8-9V6z",
    columns: "M4 5h16v14H4zM12 5v14",
    rows: "M4 5h16v14H4zM4 12h16",
    external: "M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5",
    windowPlus: "M4 5h16v14H4zM4 9h16M12 12v5M9.5 14.5h5",
    volume: "M4 9h4l5-4v14l-5-4H4zM16.5 8.5a5 5 0 0 1 0 7M19 6a8.5 8.5 0 0 1 0 12",
    volumeLow: "M4 9h4l5-4v14l-5-4H4zM16.5 8.5a5 5 0 0 1 0 7",
    volumeMute: "M4 9h4l5-4v14l-5-4H4zM17 9l5 6M22 9l-5 6",
    menu: "M4 7h16M4 12h16M4 17h16",
    hardDrive: "M4 14h16v5H4zM4 14l2.5-8h11l2.5 8M8 16.5h.01",
    checkSquare: "M4 4h16v16H4zM8 12l3 3 5-6",
    eyeOff: "M3 3l18 18M10.6 6.1A9.8 9.8 0 0 1 12 6c6.5 0 10 6 10 6a17 17 0 0 1-3.2 3.9M6.6 7.7C3.8 9.5 2 12 2 12s3.5 7 10 7a9.6 9.6 0 0 0 4.4-1.1M9.9 10a3 3 0 0 0 4.2 4.2",
    unlock: "M6 11h12v9H6zM8 11V8a4 4 0 0 1 7.7-1.5",
    upload: "M12 16V4M7 9l5-5 5 5M4 20h16",
    download: "M12 4v12M7 11l5 5 5-5M4 20h16",
  } as const;
  export type IconName = keyof typeof ICONS;
</script>

<script lang="ts">
  let { name, size = 16, stroke = 1.75 }: { name: IconName; size?: number; stroke?: number } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width={stroke}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  style="flex-shrink:0"><path d={ICONS[name]} /></svg
>
