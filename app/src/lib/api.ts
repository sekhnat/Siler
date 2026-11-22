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
  host_key: HostKeyRequest | null;
}

/** How the list looks (kept with the settings). */
export interface UiSettings {
  view: "cards" | "details";
  columns: ColumnSetting[];
  sort: string;
  sort_reverse: boolean;
  toolbar: string[];
}

export interface ColumnSetting {
  id: string;
  width: number;
}

export type InstallKind = "app_image" | "packaged" | "tarball" | "dev";

export interface AppInfo {
  version: string;
  install: InstallKind;
}

/** What an update check found. */
export interface UpdateInfo {
  current: string;
  latest: string;
  newer: boolean;
  url: string;
  notes: string;
  install: InstallKind;
  appimage: string | null;
  sha256: string | null;
  checked_at: number;
}

/** An SFTP server whose key Siler hasn't seen before. */
export interface HostKeyRequest {
  host: string;
  server: string;
  port: number;
  algorithm: string;
  fingerprint: string;
  key: string;
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
  /** The error as a code and its details, for the interface's language. */
  error_code: { code: string; args: [string, string][] } | null;
  needs_login: LoginRequest | null;
  needs_host_key: HostKeyRequest | null;
  speed: number;
  active_connections: number;
  segments: Segment[];
  /** A media download (a video or audio stream) rather than a file. */
  media: MediaInfo | null;
  /** The phase and piece counts of a media download while it runs. */
  media_progress?: MediaProgress | null;
  checksum: Checksum | null;
  verify: VerifyResult | null;
  /** Failed because the checksum didn't match; the data is kept. */
  checksum_failed: boolean;
  /** Every address it is fetched from (own URL first), when it has mirrors. */
  sources: SourceView[];
  /** The path inside the archive, for a file fetched out of a ZIP. */
  zip_path: string | null;
  command: CommandResult | null;
  /** Checking or unpacking a finished transfer. */
  phase?: PhaseProgress | null;
}

export type Algo = "md5" | "sha1" | "sha256" | "sha512";
export const ALGO_LABEL: Record<Algo, string> = { md5: "MD5", sha1: "SHA-1", sha256: "SHA-256", sha512: "SHA-512" };

export interface Checksum {
  algo: Algo;
  hex: string;
  source: "user" | "server" | "metalink" | "import";
}

export interface VerifyResult {
  algo: Algo;
  expected: string | null;
  actual: string;
  at: number;
}

export interface SourceView {
  url: string;
  origin: "user" | "metalink" | "server";
  disabled: string | null;
  bytes: number;
  active: number;
}

export interface CommandResult {
  running: boolean;
  exit_code: number | null;
  output: string;
  error: string | null;
  started_at: number;
  finished_at: number | null;
}

export interface PhaseProgress {
  name: "verifying" | "unpacking";
  done: number;
  total: number;
}

export interface QuotaStatus {
  enabled: boolean;
  reached: boolean;
  ignored: boolean;
  used: number;
  limit: number;
  resumes_at: number | null;
}

export interface QuotaSettings {
  enabled: boolean;
  megabytes: number;
  hours: number;
}

export interface AfterDownload {
  enabled: boolean;
  command: string;
  categories: Category[];
  notify_on_failure: boolean;
}

export interface ZipItem {
  path: string;
  size: number;
  compressed: number;
  method: number;
  crc: number;
  header_offset: number;
  encrypted: boolean;
  is_dir: boolean;
  unsupported: string | null;
}

export interface ZipListing {
  url: string;
  archive_size: number;
  entries: ZipItem[];
}

export type ListFormat = "text" | "json";

export type GrabScope = "same_host" | "same_site" | "folder";

export interface FileFilter {
  categories: Category[];
  extensions: string[];
  min_size: number | null;
  max_size: number | null;
  any_host: boolean;
}

export interface GrabConfig {
  start_url: string;
  depth: number;
  scope: GrabScope;
  max_pages: number;
  concurrency: number;
  robots: boolean;
  save_pages: boolean;
  filter: FileFilter;
  dir: string | null;
  queue_id: number | null;
  only_new: boolean;
  schedule: Schedule;
}

export interface GrabProject {
  id: number;
  name: string;
  config: GrabConfig;
  last_run: number | null;
}

export interface GrabFile {
  url: string;
  referrer: string;
  filename: string | null;
  dir: string | null;
  size: number | null;
}

export interface GrabResult {
  files: GrabFile[];
  pages: number;
  saved: number;
  errors: number;
  blocked: number;
  site_index: string | null;
  queued: number;
}

export interface GrabProgress {
  run_id: number;
  project_id: number | null;
  pages_done: number;
  pages_queued: number;
  files_found: number;
  saved: number;
  bytes: number;
  current: string | null;
  finished: boolean;
  cancelled: boolean;
  error: string | null;
  result: GrabResult | null;
}

export interface ProgressItem {
  id: number;
  downloaded: number;
  total_size: number | null;
  speed: number;
  active_connections: number;
  segments: Segment[];
  media?: MediaProgress | null;
  phase?: PhaseProgress | null;
  source_bytes?: number[];
}

export type ContainerChoice = "auto" | "mp4" | "mkv" | "original";
export type SubtitleMode = "embed" | "sidecar" | "none";
export type ManifestKind = "hls" | "dash" | "hds" | "stream";
export type OutputKind = "mp4" | "mkv" | "original";

/** The tracks a media download takes. */
export interface Selection {
  video: string | null;
  audio: string[];
  subtitles: string[];
}

export interface MediaInfo {
  manifest: ManifestKind;
  output: OutputKind;
  reason: string;
  subtitles: SubtitleMode;
  labels: string[];
  sidecars: string[];
  /** A recording (live stream, RTMP/MMS): it can be finished. */
  live: boolean;
  /** The recording was finished; what remains is the rebuild. */
  finished: boolean;
}

export interface MediaProgress {
  phase: "waiting" | "resolving" | "downloading" | "recording" | "rebuilding" | "publishing";
  segments_done: number;
  segments_total: number;
  /** What the pieces add up to, once every piece's size is known. */
  bytes_total: number | null;
  permille: number;
  /** A recording: seconds of the stream recorded so far. */
  recorded_secs?: number | null;
  /** A recording: times pieces of the stream were missed. */
  gaps?: number;
}

/** What a probe of a link found: a plain file, a page, or a stream and its tracks. */
export type MediaKindView = "direct" | "hls" | "dash" | "hds" | "playlist" | "stream" | "not_media" | "drm" | "unsupported";

export interface TrackView {
  id: string;
  kind: "video" | "audio" | "subtitle";
  label: string;
  lang: string | null;
  codecs: string | null;
  bandwidth: number | null;
  resolution: [number, number] | null;
  channels: number | null;
  estimated_bytes: number | null;
  is_default: boolean;
  container: string;
}

export interface VariantView {
  video: TrackView;
  audio: string[];
  subtitles: string[];
  muxed_audio: boolean;
}

export interface MediaProbe {
  kind: MediaKindView;
  message: string | null;
  title: string | null;
  duration: number | null;
  variants: VariantView[];
  audio: TrackView[];
  subtitles: TrackView[];
  default: Selection;
  warnings: string[];
  dropped_drm: number;
  ffmpeg: boolean;
  /** A live stream: recorded from now until Finish. */
  live: boolean;
  /** The links of a playlist. */
  entries: { url: string; title: string | null }[];
}

export interface FfmpegStatus {
  path: string | null;
  version: string | null;
  ok: boolean;
  missing: string[];
}

export interface MediaArgs {
  url: string;
  filename?: string | null;
  dir?: string | null;
  selection?: Selection | null;
  container?: ContainerChoice | null;
  subtitles?: SubtitleMode | null;
  startPaused?: boolean;
  onConflict?: OnConflict | null;
  pendingId?: number | null;
  queueId?: number | null;
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
    }
  | { type: "command_done"; id: number; filename: string | null; result: CommandResult }
  | ({ type: "quota" } & QuotaStatus)
  | ({ type: "grab" } & GrabProgress)
  | { type: "metalink"; id: number };

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
  duplicates: string[];
  digest: Checksum | null;
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
  media_segment_concurrency: number;
  preferred_max_height: number;
  preferred_audio_language: string;
  preferred_subtitle_language: string;
  default_media_container: ContainerChoice;
  default_subtitle_mode: SubtitleMode;
  allow_private_media_addresses: boolean;
  quota: QuotaSettings;
  after_download: AfterDownload;
  open_metalinks: boolean;
  check_updates: boolean;
  skipped_version: string;
  ui: UiSettings;
  /** The interface's language; empty for the system's. */
  language: string;
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
  /** Where the link came from. */
  origin: "browser" | "command_line" | "clipboard" | "list" | "grabber";
  /** The site the browser extension recognised the video on (`youtube`, …). */
  site: string | null;
  /** The video's title, from the page. */
  title: string | null;
  /** The extension gave the stream itself: a stream whatever the link looks like. */
  stream: boolean;
  resumable: Download | null;
  /** The checksum it comes with, as `SHA-256 ab12…`. */
  checksum: string | null;
  mirrors: number;
}

export interface BrowserStatus {
  name: string;
  family: "firefox" | "chromium";
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
  /** As typed; empty clears one that came with the item. */
  checksum?: string | null;
  /** Absent keeps the mirrors that came with the item. */
  mirrors?: string[] | null;
}

export interface ZipPick {
  url: string;
  archiveSize: number;
  dir: string;
  entries: ZipItem[];
  pendingId?: number | null;
  queueId?: number | null;
  startPaused?: boolean;
}

export const api = {
  startupError: () => invoke<string | null>("startup_error"),
  list: () => invoke<Download[]>("list_downloads"),
  probe: (url: string, pendingId?: number | null) => invoke<ProbeResult>("probe_url", { url, pendingId }),
  nameStatus: (dir: string, name: string) => invoke<{ exists: boolean; suggested: string }>("name_status", { dir, name }),
  add: (args: AddArgs) => invoke<number>("add_download", { args }),
  probeMedia: (url: string, pendingId?: number | null) => invoke<MediaProbe>("probe_media", { url, pendingId }),
  addMedia: (args: MediaArgs) => invoke<number>("add_media", { args }),
  mediaDiagnostics: () => invoke<FfmpegStatus>("media_diagnostics"),
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
  autostartEnabled: () => invoke<boolean>("autostart_enabled"),
  setAutostart: (on: boolean) => invoke<void>("set_autostart", { on }),
  showMain: () => invoke<void>("show_main_window"),
  requestQuit: () => invoke<void>("request_quit"),
  quit: (dontAsk: boolean) => invoke<void>("quit_app", { dontAsk }),
  submitLogin: (host: string, username: string, password: string, remember: boolean) =>
    invoke<void>("submit_login", { host, username, password, remember }),
  chromiumExtension: () => invoke<{ path: string; browsers: string[] }>("chromium_extension"),
  openChromiumExtensions: (browser: string) => invoke<void>("open_chromium_extensions", { browser }),
  appInfo: () => invoke<AppInfo>("app_info"),
  updateStatus: () => invoke<UpdateInfo | null>("update_status"),
  checkUpdatesNow: () => invoke<UpdateInfo>("check_updates_now"),
  installUpdate: () => invoke<void>("install_update"),
  finish: (id: number) => invoke<void>("finish_download", { id }),
  trustHostKey: (request: HostKeyRequest) => invoke<void>("trust_host_key", { request }),
  savedLogins: () => invoke<SavedLogin[]>("saved_logins"),
  forgetSavedLogin: (host: string) => invoke<void>("forget_saved_login", { host }),
  keyringAvailable: () => invoke<boolean>("keyring_available"),
  proxyStatus: () => invoke<ProxyRoute>("proxy_status"),
  setProxyPassword: (password: string | null) => invoke<void>("set_proxy_password", { password }),
  proxyPasswordSaved: () => invoke<boolean>("proxy_password_saved"),
  importList: (path: string) => invoke<number>("import_list", { path }),
  exportList: (path: string, ids: number[], format: ListFormat, includeCookies: boolean) =>
    invoke<void>("export_list", { path, ids, format, includeCookies }),
  expandBatch: (pattern: string) => invoke<{ count: number; urls: string[] }>("expand_batch", { pattern }),
  parseChecksum: (text: string) => invoke<Checksum>("parse_checksum", { text }),
  acceptUnverified: (id: number) => invoke<void>("accept_unverified", { id }),
  verifyFile: (id: number, checksum: string | null) => invoke<VerifyResult>("verify_file", { id, checksum }),
  fileDigests: (id: number) => invoke<{ algo: string; hex: string }[]>("file_digests", { id }),
  setChecksum: (id: number, checksum: string | null) => invoke<void>("set_checksum", { id, checksum }),
  setMirrors: (id: number, urls: string[]) => invoke<void>("set_mirrors", { id, urls }),
  zipList: (url: string, pendingId?: number | null) => invoke<ZipListing>("zip_list", { url, pendingId }),
  addZipEntries: (pick: ZipPick) => invoke<number>("add_zip_entries", { pick }),
  quotaStatus: () => invoke<QuotaStatus>("quota_status"),
  ignoreQuota: () => invoke<QuotaStatus>("ignore_quota"),
  testCommand: (command: string, path: string) => invoke<CommandResult>("test_command", { command, path }),
  grabProjects: () => invoke<GrabProject[]>("grab_projects"),
  saveGrabProject: (project: GrabProject) => invoke<number>("save_grab_project", { project }),
  deleteGrabProject: (id: number) => invoke<void>("delete_grab_project", { id }),
  grabStart: (config: GrabConfig, projectId: number | null) => invoke<number>("grab_start", { config, projectId }),
  grabCancel: (runId: number) => invoke<void>("grab_cancel", { runId }),
  reviewGrab: (files: GrabFile[], queueId: number | null) => invoke<number>("review_grab", { files, queueId }),
  openSavedSite: (path: string) => invoke<void>("open_saved_site", { path }),
};

/** A readable message from a rejected command (a string, or a `ProbeError`). */
export function errorText(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}
