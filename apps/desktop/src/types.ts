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
  user_agent?: string | null;
  cookie?: string | null;
  extra_headers?: string | null;
  page_url?: string | null;
  format_id?: string | null;
  speed_limit?: number | null;
  package_id?: number | null;
  mime?: string | null;
  checksum?: string | null;
  start_at?: number | null;
  media_opts?: string | null;
}

export interface Package {
  id: number;
  name: string;
  category_id: number | null;
  dir: string | null;
  status: string;
  created_at: number;
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

export interface PlaylistEntry {
  id: string;
  title: string;
  url: string;
  duration: number;
}

export interface MediaInfo {
  title: string;
  formats: Format[];
  kind: "video" | "playlist";
  entries: PlaylistEntry[];
}

export interface MediaOpts {
  write_subs: boolean;
  sub_langs: string;
  embed_subs: boolean;
  audio_only: boolean;
  audio_format: string;
  embed_thumbnail: boolean;
}

export interface UpdateInfo {
  current: string;
  latest: string;
  newer: boolean;
  url: string;
  asset_url: string | null;
  can_install: boolean;
  notes: string;
}

export interface DetailFile {
  index: number;
  path: string;
  length: number;
  completed_length: number;
  selected: boolean;
}

export interface DetailPeer {
  ip: string;
  down_speed: number;
  up_speed: number;
  seeder: boolean;
}

export interface DownloadDetails {
  id: number;
  url: string;
  dir: string;
  kind: string;
  error_message: string | null;
  num_pieces: number;
  piece_length: number;
  verified_length: number;
  files: DetailFile[];
  peers: DetailPeer[];
  live: boolean;
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
