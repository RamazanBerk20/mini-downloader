// Canonical message catalog. Every other locale mirrors these keys.
// Placeholders use {name} and are preserved verbatim in translations.
export const en = {
  // status / nav
  statusAll: "All",
  statusActive: "Active",
  statusPaused: "Paused",
  statusWaiting: "Waiting",
  statusCompleted: "Completed",
  statusFailed: "Failed",
  statusQueued: "Queued",
  navStatus: "Status",
  navCategories: "Categories",
  navSettings: "Settings",
  globalSpeed: "Global speed limit",
  speedUnlimited: "Unlimited",

  // page titles
  titleAll: "All downloads",
  titleActive: "Active",
  titlePaused: "Paused",
  titleCompleted: "Completed",
  titleFailed: "Failed",
  titleCategory: "Category",

  // header / add
  search: "Search",
  addPlaceholder: "Paste a URL or magnet link",
  add: "Add",
  clearCompleted: "Clear completed",
  tipAddFile: "Add a .torrent or .metalink file",
  tipGrabVideo: "Grab a video (yt-dlp)",
  tipGrabLinks: "Grab many links",
  tipShortcuts: "Keyboard shortcuts",
  dismiss: "Dismiss",

  // empty states
  emptyTitle: "No downloads yet",
  emptySub: "Paste a URL above, or capture one from your browser.",
  emptyToAdd: "to add",
  emptyForShortcuts: "for shortcuts",
  noMatchTitle: "Nothing here",
  noMatchSub: "No downloads match this filter.",

  // toolbar / actions
  pauseAll: "Pause all",
  resumeAll: "Resume all",
  pause: "Pause",
  resume: "Resume",
  openFolder: "Open folder",
  retry: "Retry",
  remove: "Remove",

  // clipboard toast
  download: "Download",

  // shortcuts help
  shortcutsTitle: "Keyboard shortcuts",
  scFocusAdd: "Focus the add field",
  scSearch: "Search",
  scFilter: "Switch status filter",
  scSettings: "Settings",
  scPauseAll: "Pause all",
  scResumeAll: "Resume all",
  scPauseResume: "Pause/resume focused download",
  scRemove: "Remove focused download",
  scOpen: "Open folder (completed)",
  scClose: "Close dialogs",

  // settings
  settings: "Settings",
  sectGeneral: "General",
  optAutoOrganize: "Auto-organize finished files",
  optClipboard: "Watch clipboard for links",
  optCloseTray: "Close to tray (keep running)",
  optAutostart: "Start on login (minimized)",
  sectLanguage: "Language",
  sectConnections: "Connections",
  optConnPerServer: "Connections per server",
  optSegments: "Segments per file",
  connHint: "Higher = faster on servers that allow multiple connections. Applies to new downloads.",
  sectBrowser: "Browser integration",
  installHost: "Install native-messaging host",
  browserHint: "Load the extension via about:debugging (Firefox) or chrome://extensions (Chromium).",
  sectScheduler: "Scheduler",
  schedPauseAll: "Pause all",
  schedResumeAll: "Resume all",
  schedSetSpeed: "Set speed limit",
  schedAddRule: "Add rule",
  scheduleTimeInvalid: "Enter a valid time (HH:MM).",
  sectCategories: "Categories",
  browseFolder: "Browse folder",
  sponsor: "Sponsor Mini Downloader",
  close: "Close",

  // days
  dayMon: "Mon",
  dayTue: "Tue",
  dayWed: "Wed",
  dayThu: "Thu",
  dayFri: "Fri",
  daySat: "Sat",
  daySun: "Sun",

  // media grab
  grabVideoTitle: "Grab video",
  videoUrlPlaceholder: "Video page URL (YouTube, etc.)",
  probe: "Probe",
  probing: "Probing…",
  bestQuality: "Best quality",
  colQuality: "Quality",
  colFormat: "Format",
  colCodec: "Codec",
  colSize: "Size",
  grab: "Grab",

  // link grabber
  grabLinksTitle: "Grab links",
  pasteLinksPlaceholder: "Paste text, a link list, or HTML",
  extractLinks: "Extract links",
  selectedCount: "{n} of {m} selected",
  addSelected: "Add {n} selected",
};
