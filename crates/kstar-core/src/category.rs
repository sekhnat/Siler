use serde::{Deserialize, Serialize};

use crate::filename::split_ext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Video,
    Music,
    Images,
    Documents,
    Compressed,
    Programs,
    #[default]
    Other,
}

const VIDEO: &[&str] = &[
    "mp4", "mkv", "webm", "avi", "mov", "wmv", "flv", "m4v", "mpg", "mpeg", "3gp", "ts", "m2ts",
    "ogv",
];
const MUSIC: &[&str] = &[
    "mp3", "flac", "ogg", "opus", "m4a", "aac", "wav", "wma", "alac", "aiff", "mka",
];
const IMAGES: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "tif", "tiff", "svg", "heic", "psd", "raw",
];
const DOCUMENTS: &[&str] = &[
    "pdf", "epub", "mobi", "djvu", "doc", "docx", "odt", "rtf", "txt", "md", "xls", "xlsx", "ods",
    "csv", "ppt", "pptx", "odp",
];
const COMPRESSED: &[&str] = &[
    "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "lz", "lzma", "cab", "tar.gz",
    "tar.bz2", "tar.xz", "tar.zst",
];
const PROGRAMS: &[&str] = &[
    "appimage",
    "deb",
    "rpm",
    "flatpak",
    "flatpakref",
    "snap",
    "run",
    "sh",
    "bin",
    "exe",
    "msi",
    "dmg",
    "pkg",
    "apk",
    "iso",
    "img",
    "jar",
];

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Video,
        Category::Music,
        Category::Images,
        Category::Documents,
        Category::Compressed,
        Category::Programs,
        Category::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Video => "video",
            Category::Music => "music",
            Category::Images => "images",
            Category::Documents => "documents",
            Category::Compressed => "compressed",
            Category::Programs => "programs",
            Category::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Category {
        Category::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .unwrap_or_default()
    }

    /// Sub-folder name used when "category folders" are enabled.
    pub fn folder_name(self) -> &'static str {
        match self {
            Category::Video => "Video",
            Category::Music => "Music",
            Category::Images => "Images",
            Category::Documents => "Documents",
            Category::Compressed => "Compressed",
            Category::Programs => "Programs",
            Category::Other => "Other",
        }
    }

    /// The extensions that belong to this category out of the box.
    pub fn default_extensions(self) -> &'static [&'static str] {
        match self {
            Category::Video => VIDEO,
            Category::Music => MUSIC,
            Category::Images => IMAGES,
            Category::Documents => DOCUMENTS,
            Category::Compressed => COMPRESSED,
            Category::Programs => PROGRAMS,
            Category::Other => &[],
        }
    }

    /// Classifies by the default extension lists. [`crate::Settings::category_for`]
    /// uses the user's lists instead.
    pub fn from_filename(name: &str) -> Category {
        Category::classify(name, |c| {
            c.default_extensions()
                .iter()
                .map(|s| s.to_string())
                .collect()
        })
    }

    pub(crate) fn classify(name: &str, extensions: impl Fn(Category) -> Vec<String>) -> Category {
        let lower = name.to_ascii_lowercase();
        let ext = split_ext(&lower).1.trim_start_matches('.').to_string();
        if ext.is_empty() {
            return Category::Other;
        }
        let last = ext.rsplit('.').next().unwrap_or_default();
        let lists: Vec<(Category, Vec<String>)> = Category::ALL
            .into_iter()
            .filter(|&c| c != Category::Other)
            .map(|c| (c, extensions(c)))
            .collect();
        // "tar.gz" first, then just "gz".
        [ext.as_str(), last]
            .into_iter()
            .find_map(|e| lists.iter().find(|(_, l)| l.iter().any(|x| x == e)))
            .map(|(c, _)| *c)
            .unwrap_or(Category::Other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies() {
        assert_eq!(Category::from_filename("movie.MKV"), Category::Video);
        assert_eq!(Category::from_filename("linux.iso"), Category::Programs);
        assert_eq!(Category::from_filename("src.tar.gz"), Category::Compressed);
        assert_eq!(Category::from_filename("README"), Category::Other);
        assert_eq!(Category::parse("music"), Category::Music);
    }
}
