export type DownloadStatus =
  | "queued"
  | "active"
  | "waiting"
  | "paused"
  | "complete"
  | "error"
  | "removed"
  | "scheduled";

export interface Download {
  id: number;
  gid: string | null;
  url: string;
  filename: string | null;
  dir: string;
  status: DownloadStatus;
  kind: string;
  total_bytes: number;
  completed_bytes: number;
  download_speed: number;
  upload_speed: number;
  connections: number;
  num_seeders: number;
  referrer: string | null;
  info_hash: string | null;
  error_code: string | null;
  error_message: string | null;
  category_id: number | null;
  created_at: number;
  finished_at: number | null;
}

export interface Tick {
  gid: string;
  name: string;
  completed: number;
  total: number;
  dl_speed: number;
  ul_speed: number;
  connections: number;
  num_seeders: number;
  status: string;
}
