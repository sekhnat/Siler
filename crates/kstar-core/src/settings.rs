use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::{Category, OnConflict};

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
    /// Ask before quitting while downloads are running.
    pub confirm_quit: bool,
    /// What to do when the target file already exists.
    pub on_conflict: OnConflict,
    pub categories: Vec<CategoryRule>,
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
            confirm_quit: true,
            on_conflict: OnConflict::Ask,
            categories: default_categories(),
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
            self.rule(c).map(|r| r.extensions.clone()).unwrap_or_default()
        })
    }

    /// A conflict policy for downloads added without a dialog, where nobody can be asked.
    pub fn unattended_conflict(&self) -> OnConflict {
        match self.on_conflict {
            OnConflict::Ask => OnConflict::Rename,
            p => p,
        }
    }

    pub(crate) fn normalized(mut self) -> Self {
        self.max_concurrent = self.max_concurrent.clamp(1, 20);
        self.connections = self.connections.clamp(1, 32);
        self.min_split_size = self.min_split_size.max(16 * 1024);
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
        assert_eq!(s.category_dir(Category::Video), PathBuf::from("/media/films"));
        assert_eq!(s.category_dir(Category::Music), PathBuf::from("/dl/Music"));
        assert_eq!(s.category_for("a.tar.gz"), Category::Compressed);
        assert_eq!(s.category_for("README"), Category::Other);
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
