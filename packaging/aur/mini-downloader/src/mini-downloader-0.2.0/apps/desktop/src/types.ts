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

export interface Category {
  id: number;
  name: string;
  dir: string;
  rules: string;
  priority: number;
}

export interface ParsedLink {
  url: string;
  kind: string;
  host: string;
}

export interface Schedule {
  id: number;
  name: string | null;
  action: string;
  days_mask: number;
  at_minute: number;
  speed_limit: number | null;
  enabled: boolean;
}

export interface Format {
  format_id: string;
  ext: string;
  resolution: string;
  vcodec: string;
  acodec: string;
  filesize: number;
  protocol: string;
  note: string;
  height: number;
}

export interface MediaInfo {
  title: string;
  formats: Format[];
}

export interface Tick {
  id: number;
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
