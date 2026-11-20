import { invoke } from "@tauri-apps/api/core";

export type Status = "waiting" | "queued" | "probing" | "downloading" | "paused" | "completed" | "failed";
export type Category = "video" | "music" | "images" | "documents" | "compressed" | "programs" | "other";
export type OnConflict = "ask" | "rename" | "overwrite" | "skip";
export type QueueMove = "up" | "down" | "top" | "bottom";

export type AfterDone = "nothing" | "quit" | "sleep" | "hibernate" | "power_off";

export interface Schedule {
  enabled: boolean;
  /** Minutes after midnight. */
  start_at: number | null;
  stop_at: number | null;
  /** Monday first. */
  days: boolean[];
  once: boolean;
}

export interface Queue {
  id: number;
  name: string;
  /** 0 = only the global limit. */
  max_concurrent: number;
  schedule: Schedule;
  after_done: AfterDone;
  active: boolean;
}

export const MAIN_QUEUE = 1;

export interface PowerCaps {
  sleep: boolean;
  hibernate: boolean;
  power_off: boolean;
}

export interface PendingPower {
  action: AfterDone;
  reason: string;
  seconds_left: number;
}

export type SoundEvent = "complete" | "failed" | "queue_done";

export interface SoundSettings {
  enabled: boolean;
  on_complete: boolean;
  on_failed: boolean;
  on_queue_done: boolean;
  complete_file: string | null;
  failed_file: string | null;
  queue_done_file: string | null;
}

export interface LoginRequest {
  host: string;
  realm: string | null;
  rejected: boolean;
}

/** What `probe` rejects with. */
export interface ProbeError {
  message: string;
  login: LoginRequest | null;
}

export type ProxyMode = "none" | "system" | "manual";
export type ProxyScheme = "http" | "socks5";

export interface ProxySettings {
  mode: ProxyMode;
  scheme: ProxyScheme;
  host: string;
  port: number;
  username: string;
  no_proxy: string;
}

/** Where connections go under the saved proxy settings. */
export interface ProxyRoute {
  summary: string;
  note: string | null;
}

export interface ServerLimit {
  host: string;
  connections: number;
}

export interface SavedLogin {
  host: string;
  username: string;
}

export interface Segment {
  start: number;
  end: number;
  pos: number;
}

export interface Download {
  id: number;
  url: string;
  final_url: string | null;
  referrer: string | null;
  page_url: string | null;
  method: string | null;
  filename: string | null;
  dir: string | null;
  category: Category;
  total_size: number | null;
  downloaded: number;
  status: Status;
  resumable: boolean | null;
  error: string | null;
  notice: string | null;
  connections: number;
  speed_limit: number;
  created_at: number;
  completed_at: number | null;
  position: number;
  on_conflict: OnConflict;
  queue_id: number;
  /** Checked for a newer version this often (seconds); 0 = not kept up to date. */
  sync_every: number;
  sync_checked_at: number | null;
  file_missing: boolean;
  needs_login: LoginRequest | null;
  speed: number;
  active_connections: number;
  segments: Segment[];
}

export interface ProgressItem {
  id: number;
  downloaded: number;
  total_size: number | null;
  speed: number;
  active_connections: number;
  segments: Segment[];
}

export type EngineEvent =
  | { type: "upsert"; download: Download }
  | { type: "removed"; id: number }
  | { type: "progress"; items: ProgressItem[]; total_speed: number }
  | { type: "completed"; id: number; filename: string; path: string }
  | { type: "failed"; id: number; filename: string | null; error: string }
  | { type: "login_required"; id: number; filename: string | null; request: LoginRequest }
  | { type: "queues"; queues: Queue[] }
  | {
      type: "queue_done";
      queue_id: number;
      name: string;
      after_done: AfterDone;
      completed: number;
      failed: number;
    };

export interface ProbeResult {
  final_url: string;
  total_size: number | null;
  resumable: boolean;
  etag: string | null;
  last_modified: string | null;
  filename: string;
  content_type: string | null;
  category: Category;
  dir: string;
  exists: boolean;
  suggested: string;
}

export interface CategoryRule {
  category: Category;
  extensions: string[];
  folder: string | null;
}

export interface Settings {
  download_dir: string;
  use_category_folders: boolean;
  max_concurrent: number;
  connections: number;
  global_speed_limit: number;
  max_retries: number;
  min_split_size: number;
  resume_on_startup: boolean;
  user_agent: string;
  auto_start_browser_downloads: boolean;
  notify_on_complete: boolean;
  close_to_tray: boolean;
  confirm_quit: boolean;
  on_conflict: OnConflict;
  categories: CategoryRule[];
  max_connections_per_server: number;
  server_limits: ServerLimit[];
  proxy: ProxySettings;
  prevent_sleep: boolean;
  clipboard_monitor: boolean;
  sounds: SoundSettings;
}

export interface Pending {
  id: number;
  url: string;
  filename: string | null;
  referrer: string | null;
  page_url: string | null;
  size: number | null;
  mime: string | null;
  method: string | null;
  selected: boolean;
  dir: string | null;
  start_paused: boolean;
  queue_id: number | null;
  /** The name was given on the command line: don't replace it with the server's. */
  keep_name: boolean;
  resumable: Download | null;
}

export interface BrowserStatus {
  name: string;
  manifest_path: string;
  installed: boolean;
}

export interface IntegrationStatus {
  browsers: BrowserStatus[];
  manifest_installed: boolean;
  manifest_path: string;
  host_path: string | null;
  host_exists: boolean;
  last_contact: number | null;
  app_version: string;
  protocol: number;
  extension_version: string | null;
  extension_protocol: number | null;
  desktop_entry: boolean;
  tray_available: boolean;
  signed_xpi: string | null;
  log_dir: string;
}

export interface AddArgs {
  url: string;
  filename?: string | null;
  dir?: string | null;
  category?: Category | null;
  startPaused?: boolean;
  connections?: number | null;
  onConflict?: OnConflict | null;
  pendingId?: number | null;
  queueId?: number | null;
}

export const api = {
  startupError: () => invoke<string | null>("startup_error"),
  list: () => invoke<Download[]>("list_downloads"),
  probe: (url: string, pendingId?: number | null) => invoke<ProbeResult>("probe_url", { url, pendingId }),
  nameStatus: (dir: string, name: string) => invoke<{ exists: boolean; suggested: string }>("name_status", { dir, name }),
  add: (args: AddArgs) => invoke<number>("add_download", { args }),
  pending: () => invoke<Pending[]>("pending_adds"),
  dismissPending: (id: number) => invoke<void>("dismiss_pending", { id }),
  pause: (id: number) => invoke<void>("pause", { id }),
  resume: (id: number) => invoke<void>("resume", { id }),
  remove: (id: number, deleteFile: boolean) => invoke<void>("remove", { id, deleteFile }),
  removeMany: (ids: number[], deleteFiles: boolean) => invoke<number>("remove_many", { ids, deleteFiles }),
  pauseAll: () => invoke<void>("pause_all"),
  resumeAll: () => invoke<void>("resume_all"),
  startNow: (id: number) => invoke<void>("start_now", { id }),
  startQueue: (queueId: number = MAIN_QUEUE) => invoke<void>("start_queue", { queueId }),
  stopQueue: (queueId: number = MAIN_QUEUE) => invoke<void>("stop_queue", { queueId }),
  queues: () => invoke<Queue[]>("list_queues"),
  createQueue: (queue: Queue) => invoke<number>("create_queue", { queue }),
  updateQueue: (queue: Queue) => invoke<void>("update_queue", { queue }),
  deleteQueue: (queueId: number) => invoke<void>("delete_queue", { queueId }),
  moveToQueue: (ids: number[], queueId: number) => invoke<void>("move_to_queue", { ids, queueId }),
  setSync: (id: number, everySecs: number) => invoke<void>("set_sync", { id, everySecs }),
  checkForUpdate: (id: number) => invoke<void>("check_for_update", { id }),
  powerCaps: () => invoke<PowerCaps>("power_caps"),
  powerPending: () => invoke<PendingPower | null>("power_pending"),
  powerCancel: () => invoke<void>("power_cancel"),
  powerNow: () => invoke<void>("power_now"),
  getWhenDone: () => invoke<AfterDone>("get_when_done"),
  setWhenDone: (action: AfterDone) => invoke<void>("set_when_done", { action }),
  testSound: (sound: SoundEvent, settings?: SoundSettings) => invoke<void>("test_sound", { sound, settings }),
  moveInQueue: (ids: number[], to: QueueMove) => invoke<void>("move_in_queue", { ids, to }),
  restart: (id: number) => invoke<void>("restart", { id }),
  redownload: (id: number, replace: boolean) => invoke<void>("redownload", { id, replace }),
  refreshUrl: (id: number, url: string, pendingId?: number | null) => invoke<void>("refresh_url", { id, url, pendingId }),
  awaitRefresh: (id: number) => invoke<boolean>("await_refresh", { id }),
  cancelAwaitRefresh: () => invoke<void>("cancel_await_refresh"),
  setConnections: (id: number, connections: number) => invoke<void>("set_connections", { id, connections }),
  edit: (id: number, filename: string | null, dir: string | null) => invoke<void>("edit_download", { id, filename, dir }),
  setSpeedLimit: (id: number, bytesPerSec: number) => invoke<void>("set_speed_limit", { id, bytesPerSec }),
  defaultCategories: () => invoke<CategoryRule[]>("default_categories"),
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (settings: Settings) => invoke<Settings>("set_settings", { settings }),
  openFile: (id: number) => invoke<void>("open_file", { id }),
  showInFolder: (id: number) => invoke<void>("show_in_folder", { id }),
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  openLogFolder: () => invoke<void>("open_log_folder"),
  integrationStatus: () => invoke<IntegrationStatus>("integration_status"),
  installIntegration: () => invoke<IntegrationStatus>("install_integration"),
  installExtension: () => invoke<void>("install_extension"),
  showMain: () => invoke<void>("show_main_window"),
  requestQuit: () => invoke<void>("request_quit"),
  quit: (dontAsk: boolean) => invoke<void>("quit_app", { dontAsk }),
  submitLogin: (host: string, username: string, password: string, remember: boolean) =>
    invoke<void>("submit_login", { host, username, password, remember }),
  savedLogins: () => invoke<SavedLogin[]>("saved_logins"),
  forgetSavedLogin: (host: string) => invoke<void>("forget_saved_login", { host }),
  keyringAvailable: () => invoke<boolean>("keyring_available"),
  proxyStatus: () => invoke<ProxyRoute>("proxy_status"),
  setProxyPassword: (password: string | null) => invoke<void>("set_proxy_password", { password }),
  proxyPasswordSaved: () => invoke<boolean>("proxy_password_saved"),
};

/** A readable message from a rejected command (a string, or a `ProbeError`). */
export function errorText(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}
