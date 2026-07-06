import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Category, Download, MediaInfo } from "./types";

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
};

export function on<T>(event: string, cb: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (e) => cb(e.payload));
}
