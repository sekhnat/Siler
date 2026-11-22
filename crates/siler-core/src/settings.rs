use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::proxy::ProxySettings;
use crate::{Category, OnConflict};
use siler_media::select::{ContainerChoice, SubtitleMode};

pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0";

/// Which file types belong to a category, and where its files go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryRule {
    pub category: Category,
    /// Lower-case extensions without the leading dot, e.g. `mkv` or `tar.gz`.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Folder for this category; `<download_dir>/<Category>` when absent.
    #[serde(default)]
    pub folder: Option<PathBuf>,
}

impl CategoryRule {
    pub fn default_for(category: Category) -> Self {
        CategoryRule {
            category,
            extensions: category
                .default_extensions()
                .iter()
                .map(|s| s.to_string())
                .collect(),
            folder: None,
        }
    }
}

/// A different connection limit for one server (and its subdomains).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerLimit {
    pub host: String,
    pub connections: u8,
}

/// Sounds for download events. Each plays the desktop theme's sound unless a file is chosen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundSettings {
    pub enabled: bool,
    pub on_complete: bool,
    pub on_failed: bool,
    pub on_queue_done: bool,
    pub complete_file: Option<PathBuf>,
    pub failed_file: Option<PathBuf>,
    pub queue_done_file: Option<PathBuf>,
}

impl Default for SoundSettings {
    fn default() -> Self {
        SoundSettings {
            enabled: false,
            on_complete: true,
            on_failed: true,
            on_queue_done: true,
            complete_file: None,
            failed_file: None,
            queue_done_file: None,
        }
    }
}

pub fn default_categories() -> Vec<CategoryRule> {
    Category::ALL
        .into_iter()
        .map(CategoryRule::default_for)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub download_dir: PathBuf,
    /// Put files into `<download_dir>/<Category>/`.
    pub use_category_folders: bool,
    /// How many downloads run at once; the rest wait in the queue.
    pub max_concurrent: usize,
    /// Default number of connections per download (1–32).
    pub connections: u8,
    /// Global speed limit in bytes/s, 0 = unlimited.
    pub global_speed_limit: u64,
    pub max_retries: u32,
    /// Segments are never split below this size.
    pub min_split_size: u64,
    pub resume_on_startup: bool,
    pub user_agent: String,
    /// Start browser-captured downloads immediately instead of showing the Add dialog.
    pub auto_start_browser_downloads: bool,
    pub notify_on_complete: bool,
    pub close_to_tray: bool,
    /// A left click on the tray icon opens the panel (otherwise Siler's window).
    pub tray_opens_panel: bool,
    /// Ask before quitting while downloads are running.
    pub confirm_quit: bool,
    /// What to do when the target file already exists.
    pub on_conflict: OnConflict,
    pub categories: Vec<CategoryRule>,
    /// Connections to one server across all its downloads; 0 = no limit.
    pub max_connections_per_server: u8,
    pub server_limits: Vec<ServerLimit>,
    pub proxy: ProxySettings,
    /// Keep the computer from sleeping while downloads run.
    pub prevent_sleep: bool,
    /// Offer links to downloadable files when they are copied.
    pub clipboard_monitor: bool,
    pub sounds: SoundSettings,
    /// Segments fetched at once for one media download (1–16).
    pub media_segment_concurrency: u8,
    /// The highest video height to pick by default (0 = no limit).
    pub preferred_max_height: u32,
    /// Preferred audio and subtitle languages (for example `en`); empty for none.
    pub preferred_audio_language: String,
    pub preferred_subtitle_language: String,
    /// The container media downloads use unless the picker says otherwise.
    pub default_media_container: ContainerChoice,
    /// What happens to subtitles by default.
    pub default_subtitle_mode: SubtitleMode,
    /// Let media downloads use private and loopback addresses (for example a home media server).
    pub allow_private_media_addresses: bool,
    /// At most so many megabytes every so many hours.
    pub quota: crate::quota::QuotaSettings,
    /// A command run after each download finishes.
    pub after_download: AfterDownload,
    /// A download that turns out to be a metalink is replaced by the files it lists.
    pub open_metalinks: bool,
    /// When a link expires, get a fresh one: from the address it came from, or by the browser
    /// looking at its page in a background tab.
    pub auto_refresh_links: bool,
    /// Ask GitHub once a day whether a newer Siler is out.
    pub check_updates: bool,
    /// A version the user chose to skip (not announced again).
    pub skipped_version: String,
    /// How the download list looks: kept by the app, not used by the engine.
    pub ui: UiSettings,
    /// The interface's language (`de`, `pt-BR`, …); empty for the system's.
    pub language: String,
}

/// The list's view, columns, order and toolbar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    /// `cards` or `details`.
    pub view: String,
    /// The Details view's columns, in order.
    pub columns: Vec<ColumnSetting>,
    /// What the list is sorted by (`added`, `name`, `size`, …).
    pub sort: String,
    /// The order is reversed from the sort's natural one.
    pub sort_reverse: bool,
    /// The toolbar's buttons, in order.
    pub toolbar: Vec<String>,
    /// The skin's id.
    pub skin: String,
    /// `mixed`, `icons` or `text`; empty for what the skin says.
    pub toolbar_style: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnSetting {
    pub id: String,
    /// In pixels.
    pub width: u32,
}

/// Toolbar buttons, in their default order.
pub const DEFAULT_TOOLBAR: &[&str] = &[
    "queue",
    "resume_all",
    "pause_all",
    "when_done",
    "sort",
    "view",
    "more",
];

impl Default for UiSettings {
    fn default() -> Self {
        let col = |id: &str, width| ColumnSetting {
            id: id.into(),
            width,
        };
        UiSettings {
            view: "cards".into(),
            columns: vec![
                col("name", 220),
                col("size", 80),
                col("progress", 130),
                col("speed", 90),
                col("eta", 80),
                col("status", 110),
                col("added", 140),
            ],
            sort: "added".into(),
            sort_reverse: false,
            toolbar: DEFAULT_TOOLBAR.iter().map(|s| s.to_string()).collect(),
            skin: "siler".into(),
            toolbar_style: String::new(),
        }
    }
}

impl UiSettings {
    fn normalized(mut self) -> Self {
        if !matches!(self.view.as_str(), "cards" | "details") {
            self.view = "cards".into();
        }
        let mut seen = HashSet::new();
        self.columns
            .retain(|c| !c.id.is_empty() && c.id.len() <= 32 && seen.insert(c.id.clone()));
        self.columns.truncate(24);
        for c in &mut self.columns {
            c.width = c.width.clamp(40, 1200);
        }
        // The name is always shown, first.
        if let Some(i) = self.columns.iter().position(|c| c.id == "name") {
            let name = self.columns.remove(i);
            self.columns.insert(0, name);
        } else {
            self.columns.insert(
                0,
                ColumnSetting {
                    id: "name".into(),
                    width: 220,
                },
            );
        }
        let mut seen = HashSet::new();
        self.toolbar
            .retain(|t| !t.is_empty() && t.len() <= 32 && seen.insert(t.clone()));
        self.toolbar.truncate(24);
        if self.skin.is_empty() || self.skin.len() > 64 {
            self.skin = "siler".into();
        }
        if !matches!(self.toolbar_style.as_str(), "mixed" | "icons" | "text") {
            self.toolbar_style.clear();
        }
        if self.sort.is_empty() || self.sort.len() > 32 {
            self.sort = "added".into();
        }
        self
    }
}

/// A command run after a download finishes, for example to scan, unpack or move the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AfterDownload {
    pub enabled: bool,
    /// The program and its arguments, split like a shell would (quotes and backslashes) but
    /// never run by one. `{path}`, `{dir}`, `{name}`, `{url}` and `{size}` are filled in.
    pub command: String,
    /// Only for these categories; empty = every download.
    pub categories: Vec<Category>,
    /// Notify when the command fails (exits with something other than 0).
    pub notify_on_failure: bool,
}

impl Default for AfterDownload {
    fn default() -> Self {
        AfterDownload {
            enabled: false,
            command: String::new(),
            categories: Vec::new(),
            notify_on_failure: true,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        let download_dir = dirs::download_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
            .unwrap_or_else(|| PathBuf::from("."));
        Settings {
            download_dir,
            use_category_folders: true,
            max_concurrent: 3,
            connections: 8,
            global_speed_limit: 0,
            max_retries: 10,
            min_split_size: 1024 * 1024,
            resume_on_startup: true,
            user_agent: DEFAULT_USER_AGENT.to_string(),
            auto_start_browser_downloads: false,
            notify_on_complete: true,
            close_to_tray: true,
            tray_opens_panel: true,
            confirm_quit: true,
            on_conflict: OnConflict::Ask,
            categories: default_categories(),
            max_connections_per_server: 16,
            server_limits: Vec::new(),
            proxy: ProxySettings::default(),
            prevent_sleep: true,
            clipboard_monitor: false,
            sounds: SoundSettings::default(),
            media_segment_concurrency: 4,
            preferred_max_height: 0,
            preferred_audio_language: String::new(),
            preferred_subtitle_language: String::new(),
            default_media_container: ContainerChoice::Auto,
            default_subtitle_mode: SubtitleMode::Embed,
            allow_private_media_addresses: false,
            quota: crate::quota::QuotaSettings::default(),
            after_download: AfterDownload::default(),
            open_metalinks: true,
            auto_refresh_links: true,
            check_updates: true,
            skipped_version: String::new(),
            ui: UiSettings::default(),
            language: String::new(),
        }
    }
}

impl Settings {
    fn rule(&self, c: Category) -> Option<&CategoryRule> {
        self.categories.iter().find(|r| r.category == c)
    }

    pub fn category_dir(&self, c: Category) -> PathBuf {
        if let Some(folder) = self.rule(c).and_then(|r| r.folder.as_ref()) {
            return if folder.is_absolute() {
                folder.clone()
            } else {
                self.download_dir.join(folder)
            };
        }
        if self.use_category_folders {
            self.download_dir.join(c.folder_name())
        } else {
            self.download_dir.clone()
        }
    }

    /// The category of a file name according to the user's extension lists.
    pub fn category_for(&self, filename: &str) -> Category {
        Category::classify(filename, |c| {
            self.rule(c)
                .map(|r| r.extensions.clone())
                .unwrap_or_default()
        })
    }

    /// The connection limit for a server (host name without port); 0 = no limit.
    /// The most specific matching exception wins.
    pub fn connection_limit(&self, host: &str) -> usize {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.server_limits
            .iter()
            .filter(|l| {
                let h = l.host.as_str();
                host == h || host.ends_with(&format!(".{h}"))
            })
            .max_by_key(|l| l.host.len())
            .map(|l| l.connections)
            .unwrap_or(self.max_connections_per_server) as usize
    }

    /// A conflict policy for downloads added without a dialog, where nobody can be asked.
    pub fn unattended_conflict(&self) -> OnConflict {
        match self.on_conflict {
            OnConflict::Ask => OnConflict::Rename,
            p => p,
        }
    }

    pub(crate) fn normalized(mut self) -> Self {
        self.media_segment_concurrency = self.media_segment_concurrency.clamp(1, 16);
        self.ui = std::mem::take(&mut self.ui).normalized();
        self.preferred_audio_language = self.preferred_audio_language.trim().to_string();
        self.preferred_subtitle_language = self.preferred_subtitle_language.trim().to_string();
        self.max_concurrent = self.max_concurrent.clamp(1, 20);
        self.quota = self.quota.normalized();
        self.after_download.command = self.after_download.command.trim().to_string();
        self.connections = self.connections.clamp(1, 32);
        self.min_split_size = self.min_split_size.max(16 * 1024);
        self.max_connections_per_server = self.max_connections_per_server.min(128);
        let mut hosts = HashSet::new();
        self.server_limits = std::mem::take(&mut self.server_limits)
            .into_iter()
            .filter_map(|mut l| {
                let h = l.host.trim().to_ascii_lowercase();
                // Accept pasted URLs and `host:port`.
                let h = h.split("://").last().unwrap_or(&h);
                let h = h.split(['/', ':']).next().unwrap_or("").trim_matches('.');
                l.host = h.strip_prefix("www.").unwrap_or(h).to_string();
                l.connections = l.connections.clamp(1, 128);
                (!l.host.is_empty() && hosts.insert(l.host.clone())).then_some(l)
            })
            .collect();
        if self.user_agent.trim().is_empty() {
            self.user_agent = DEFAULT_USER_AGENT.to_string();
        }

        // One rule per category, in the canonical order; an extension belongs to the first
        // category that lists it.
        let mut seen = HashSet::new();
        self.categories = Category::ALL
            .into_iter()
            .map(|c| {
                let mut rule = self
                    .rule(c)
                    .cloned()
                    .unwrap_or_else(|| CategoryRule::default_for(c));
                rule.extensions = rule
                    .extensions
                    .iter()
                    .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                    .filter(|e| !e.is_empty() && c != Category::Other && seen.insert(e.clone()))
                    .collect();
                rule.folder = rule.folder.filter(|f| !f.as_os_str().is_empty());
                rule
            })
            .collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_category_rules() {
        let mut s = Settings {
            download_dir: PathBuf::from("/dl"),
            ..Default::default()
        };
        let video = s
            .categories
            .iter_mut()
            .find(|r| r.category == Category::Video)
            .unwrap();
        video.extensions.push(".ISO".into());
        video.folder = Some(PathBuf::from("/media/films"));
        let s = s.normalized();
        // The first category listing an extension wins.
        assert_eq!(s.category_for("disc.iso"), Category::Video);
        assert!(
            !s.rule(Category::Programs)
                .unwrap()
                .extensions
                .contains(&"iso".to_string())
        );
        assert_eq!(
            s.category_dir(Category::Video),
            PathBuf::from("/media/films")
        );
        assert_eq!(s.category_dir(Category::Music), PathBuf::from("/dl/Music"));
        assert_eq!(s.category_for("a.tar.gz"), Category::Compressed);
        assert_eq!(s.category_for("README"), Category::Other);
    }

    #[test]
    fn per_server_limits() {
        let s = Settings {
            max_connections_per_server: 12,
            server_limits: vec![
                ServerLimit {
                    host: "https://www.Example.com/x".into(),
                    connections: 4,
                },
                ServerLimit {
                    host: "cdn.example.com:8443".into(),
                    connections: 2,
                },
                ServerLimit {
                    host: "  ".into(),
                    connections: 9,
                },
            ],
            ..Default::default()
        }
        .normalized();
        assert_eq!(s.server_limits.len(), 2);
        assert_eq!(s.server_limits[0].host, "example.com");
        assert_eq!(s.connection_limit("example.com"), 4);
        assert_eq!(s.connection_limit("dl.example.com"), 4);
        assert_eq!(s.connection_limit("a.cdn.example.com"), 2);
        assert_eq!(s.connection_limit("notexample.com"), 12);
        assert_eq!(
            Settings {
                max_connections_per_server: 0,
                ..Default::default()
            }
            .connection_limit("x"),
            0
        );
    }

    #[test]
    fn old_settings_get_default_categories() {
        let s: Settings = serde_json::from_str(r#"{"connections": 4}"#).unwrap();
        let s = s.normalized();
        assert_eq!(s.categories.len(), Category::ALL.len());
        assert_eq!(s.category_for("x.mkv"), Category::Video);
        assert_eq!(s.on_conflict, OnConflict::Ask);
    }
}
