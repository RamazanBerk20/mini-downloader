import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Category, Download, MediaInfo, ParsedLink, Schedule } from "./types";

export const api = {
  add: (url: string) => invoke<Download>("add_download", { url }),
  addTorrentFile: (path: string) => invoke<Download>("add_torrent_file", { path }),
  addMetalinkFile: (path: string) => invoke<Download>("add_metalink_file", { path }),
  list: (status?: string) => invoke<Download[]>("list_downloads", { status: status ?? null }),
  pause: (id: number) => invoke<void>("pause_download", { id }),
  resume: (id: number) => invoke<void>("resume_download", { id }),
  remove: (id: number, deleteFiles: boolean) =>
    invoke<void>("remove_download", { id, deleteFiles }),
  pauseAll: () => invoke<void>("pause_all"),
  resumeAll: () => invoke<void>("resume_all"),
  setGlobalSpeed: (down: number, up: number) =>
    invoke<void>("set_global_speed", { down, up }),
  setDownloadSpeed: (id: number, limit: number) =>
    invoke<void>("set_download_speed", { id, limit }),
  openFolder: (id: number) => invoke<void>("open_containing_folder", { id }),
  probeMedia: (url: string) => invoke<MediaInfo>("probe_media", { url }),
  addMedia: (url: string, formatId?: string) =>
    invoke<Download>("add_media_download", { url, formatId: formatId ?? null }),
  installBrowser: () => invoke<string>("install_browser_integration"),
  listCategories: () => invoke<Category[]>("list_categories"),
  saveCategory: (name: string, dir: string, rules: string, priority: number) =>
    invoke<void>("save_category", { name, dir, rules, priority }),
  deleteCategory: (id: number) => invoke<void>("delete_category", { id }),
  getSetting: (key: string) => invoke<string | null>("get_setting", { key }),
  setSetting: (key: string, value: string) => invoke<void>("set_setting", { key, value }),
  grabLinks: (text: string) => invoke<ParsedLink[]>("grab_links", { text }),
  addLinksBatch: (urls: string[]) => invoke<number>("add_links_batch", { urls }),
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
};

export function on<T>(event: string, cb: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (e) => cb(e.payload));
}
