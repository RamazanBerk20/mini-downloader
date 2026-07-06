import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Download } from "./types";

export const api = {
  add: (url: string) => invoke<Download>("add_download", { url }),
  list: (status?: string) => invoke<Download[]>("list_downloads", { status: status ?? null }),
  pause: (id: number) => invoke<void>("pause_download", { id }),
  resume: (id: number) => invoke<void>("resume_download", { id }),
  remove: (id: number, deleteFiles: boolean) =>
    invoke<void>("remove_download", { id, deleteFiles }),
  pauseAll: () => invoke<void>("pause_all"),
  resumeAll: () => invoke<void>("resume_all"),
  setGlobalSpeed: (down: number, up: number) =>
    invoke<void>("set_global_speed", { down, up }),
  openFolder: (id: number) => invoke<void>("open_containing_folder", { id }),
};

export function on<T>(event: string, cb: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (e) => cb(e.payload));
}
