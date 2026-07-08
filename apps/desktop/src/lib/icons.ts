// Cohesive stroke icon set (24x24, stroke 1.75, round caps) — no emoji, no
// external library. Inner SVG markup per name; rendered by Icon.svelte.

export const ICONS = {
  add: `<path d="M12 5v14M5 12h14"/>`,
  close: `<path d="M6 6l12 12M18 6L6 18"/>`,
  file: `<path d="M6 3h8l4 4v14H6z"/><path d="M14 3v4h4"/>`,
  video: `<rect x="3" y="6" width="13" height="12" rx="2"/><path d="M16 10l5-3v10l-5-3z"/>`,
  link: `<path d="M9.5 14.5l5-5"/><path d="M11 7l1-1a4 4 0 0 1 6 6l-1 1"/><path d="M13 17l-1 1a4 4 0 0 1-6-6l1-1"/>`,
  gear: `<circle cx="12" cy="12" r="3.2"/><path d="M12 3v2.2M12 18.8V21M3 12h2.2M18.8 12H21M5.6 5.6l1.6 1.6M16.8 16.8l1.6 1.6M18.4 5.6l-1.6 1.6M7.2 16.8l-1.6 1.6"/>`,
  gauge: `<path d="M4 15a8 8 0 0 1 16 0"/><path d="M12 15l4.5-4"/><circle cx="12" cy="15" r="1.1"/>`,
  play: `<path d="M8 5.5v13l10.5-6.5z"/>`,
  pause: `<path d="M9 5v14M15 5v14"/>`,
  folder: `<path d="M3 7a2 2 0 0 1 2-2h3.5l2 2H19a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>`,
  retry: `<path d="M20 12a8 8 0 1 1-2.3-5.6"/><path d="M20 4v4h-4"/>`,
  trash: `<path d="M4 7h16"/><path d="M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/><path d="M6.5 7l1 12.5a1 1 0 0 0 1 .9h7a1 1 0 0 0 1-.9L18 7"/>`,
  search: `<circle cx="11" cy="11" r="6"/><path d="M20 20l-4.6-4.6"/>`,
  download: `<path d="M12 4v11"/><path d="M7 11l5 5 5-5"/><path d="M5 20h14"/>`,
  magnet: `<path d="M6 4v7a6 6 0 0 0 12 0V4"/><path d="M6 8h4M14 8h4"/>`,
  check: `<path d="M5 13l4 4L19 7"/>`,
  warning: `<path d="M12 4l9 16H3z"/><path d="M12 10v4.5M12 17.6h.01"/>`,
  inbox: `<path d="M4 13l2.2-8.2A2 2 0 0 1 8.1 3.3h7.8a2 2 0 0 1 1.9 1.5L20 13v5a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z"/><path d="M4 13h4.5l1.2 2.2h4.6L15.5 13H20"/>`,
  list: `<path d="M8 6h13M8 12h13M8 18h13M3.5 6h.01M3.5 12h.01M3.5 18h.01"/>`,
  clock: `<circle cx="12" cy="12" r="8"/><path d="M12 8v4.2l2.8 1.8"/>`,
  dot: `<circle cx="12" cy="12" r="4"/>`,
  help: `<circle cx="12" cy="12" r="8.5"/><path d="M9.6 9.5a2.4 2.4 0 0 1 4.6.9c0 1.6-2.2 2-2.2 3.4M12 17.2h.01"/>`,
  heart: `<path d="M12 20s-7-4.6-9.2-9A4.6 4.6 0 0 1 12 6.5 4.6 4.6 0 0 1 21.2 11C19 15.4 12 20 12 20z"/>`,
  "chevron-up": `<path d="M6 15l6-6 6 6"/>`,
  "chevron-down": `<path d="M6 9l6 6 6-6"/>`,
  "chevron-right": `<path d="M9 6l6 6-6 6"/>`,
} as const;

export type IconName = keyof typeof ICONS;
