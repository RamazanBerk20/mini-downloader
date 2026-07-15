import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Category, Download, DownloadDetails, MediaInfo, MediaOpts, Package, ParsedLink, PlaylistEntry, Schedule, UpdateInfo } from "./types";

export type ConnectorStatus = {
  /** A supported browser profile root exists locally for this family. */
  firefoxProfileDetected: boolean;
  chromiumProfileDetected: boolean;
  /** A connector has reached the running app during this session. */
  firefoxDetected: boolean;
  chromiumDetected: boolean;
  /** An enabled connector entry was found in a supported local profile. */
  firefoxConnectorInstalled: boolean;
  chromiumConnectorInstalled: boolean;
  firefoxLastSeen: number | null;
  chromiumLastSeen: number | null;
};

export const api = {
  add: (url: string, checksum?: string) =>
    invoke<Download>("add_download", { url, checksum: checksum ?? null }),
  addTorrentFile: (path: string) => invoke<Download>("add_torrent_file", { path }),
  addMetalinkFile: (path: string) => invoke<Download>("add_metalink_file", { path }),
  list: (status?: string) => invoke<Download[]>("list_downloads", { status: status ?? null }),
  pause: (id: number) => invoke<void>("pause_download", { id }),
  resume: (id: number) => invoke<void>("resume_download", { id }),
  retry: (id: number) => invoke<Download>("retry_download", { id }),
  remove: (id: number, deleteFiles: boolean) =>
    invoke<void>("remove_download", { id, deleteFiles }),
  moveInQueue: (id: number, direction: "top" | "up" | "down" | "bottom") =>
    invoke<void>("move_in_queue", { id, direction }),
  setQueuePosition: (id: number, pos: number) =>
    invoke<void>("set_queue_position", { id, pos }),
  setMaxConcurrent: (n: number) => invoke<void>("set_max_concurrent", { n }),
  getMaxConcurrent: () => invoke<number>("get_max_concurrent"),
  pauseAll: () => invoke<void>("pause_all"),
  resumeAll: () => invoke<void>("resume_all"),
  removeCompleted: () => invoke<number>("remove_completed"),
  removeFailed: () => invoke<number>("remove_failed"),
  setGlobalSpeed: (down: number, up: number) =>
    invoke<void>("set_global_speed", { down, up }),
  setDownloadSpeed: (id: number, limit: number) =>
    invoke<void>("set_download_speed", { id, limit }),
  openFolder: (id: number) => invoke<void>("open_containing_folder", { id }),
  probeMedia: (url: string, playlist = false) =>
    invoke<MediaInfo>("probe_media", { url, playlist }),
  addMedia: (url: string, formatId?: string, opts?: MediaOpts) =>
    invoke<Download>("add_media_download", { url, formatId: formatId ?? null, opts: opts ?? null }),
  addPlaylistBatch: (
    entries: Pick<PlaylistEntry, "url" | "title">[],
    packageName: string,
    quality?: string,
    opts?: MediaOpts,
  ) =>
    invoke<number>("add_playlist_batch", {
      entries,
      packageName,
      quality: quality ?? null,
      opts: opts ?? null,
    }),
  getConnectorStatus: () => invoke<ConnectorStatus>("get_connector_status"),
  listCategories: () => invoke<Category[]>("list_categories"),
  saveCategory: (name: string, dir: string, rules: string, priority: number) =>
    invoke<void>("save_category", { name, dir, rules, priority }),
  deleteCategory: (id: number) => invoke<void>("delete_category", { id }),
  restoreDefaultCategories: () => invoke<Category[]>("restore_default_categories"),
  resetCategoryDir: (id: number) => invoke<void>("reset_category_dir", { id }),
  getSetting: (key: string) => invoke<string | null>("get_setting", { key }),
  getSystemLocale: () => invoke<string | null>("get_system_locale"),
  setSetting: (key: string, value: string) => invoke<void>("set_setting", { key, value }),
  setHandleMagnets: (enabled: boolean) => invoke<void>("set_handle_magnets", { enabled }),
  grabLinks: (text: string) => invoke<ParsedLink[]>("grab_links", { text }),
  addLinksBatch: (urls: string[], packageName?: string) =>
    invoke<number>("add_links_batch", { urls, packageName: packageName ?? null }),
  listPackages: () => invoke<Package[]>("list_packages"),
  getDetails: (id: number) => invoke<DownloadDetails>("get_download_details", { id }),
  setTorrentFiles: (id: number, indices: number[]) =>
    invoke<void>("set_torrent_files", { id, indices }),
  scheduleDownload: (id: number, startAt: number | null) =>
    invoke<void>("schedule_download", { id, startAt }),
  applyProxy: (value: string) => invoke<void>("apply_proxy", { value }),
  listSchedules: () => invoke<Schedule[]>("list_schedules"),
  saveSchedule: (s: Omit<Schedule, "id"> & { id?: number }) =>
    invoke<void>("save_schedule", {
      id: s.id ?? null,
      name: s.name ?? null,
      action: s.action,
      daysMask: s.days_mask,
      atMinute: s.at_minute,
      speedLimit: s.speed_limit ?? null,
      enabled: s.enabled,
    }),
  deleteSchedule: (id: number) => invoke<void>("delete_schedule", { id }),
  setClipboardWatch: (enabled: boolean) => invoke<void>("set_clipboard_watch", { enabled }),
  getEngineDefaults: () => invoke<[number, number]>("get_engine_defaults"),
  setEngineDefaults: (split: number, connections: number) =>
    invoke<void>("set_engine_defaults", { split, connections }),
  checkUpdate: () => invoke<UpdateInfo>("check_update"),
  installUpdate: (assetUrl: string | null, pageUrl: string) =>
    invoke<void>("install_update", { assetUrl, pageUrl }),
};

export function on<T>(event: string, cb: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (e) => cb(e.payload));
}

/** Extract a human string from a rejected invoke error. Commands now reject with
 *  a typed `{ kind, message }`; fall back to String() for plain errors. */
export function errText(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) {
    return String((e as { message: unknown }).message);
  }
  return String(e);
}

/** The `kind` tag of a typed command error, if present (e.g. "yt-dlp-missing"). */
export function errKind(e: unknown): string | null {
  if (e && typeof e === "object" && "kind" in e) {
    return String((e as { kind: unknown }).kind);
  }
  return null;
}
