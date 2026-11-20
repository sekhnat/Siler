use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{Category, Segment};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Added with "Download later": waits until the queue is started.
    Waiting,
    Queued,
    Probing,
    Downloading,
    Paused,
    Completed,
    Failed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Waiting => "waiting",
            Status::Queued => "queued",
            Status::Probing => "probing",
            Status::Downloading => "downloading",
            Status::Paused => "paused",
            Status::Completed => "completed",
            Status::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Status {
        match s {
            "waiting" => Status::Waiting,
            "queued" => Status::Queued,
            "probing" => Status::Probing,
            "downloading" => Status::Downloading,
            "paused" => Status::Paused,
            "completed" => Status::Completed,
            _ => Status::Failed,
        }
    }

    pub fn is_running(self) -> bool {
        matches!(self, Status::Probing | Status::Downloading)
    }
}

/// What to do when the target file already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnConflict {
    /// Let the user decide in the Add dialog. Downloads added without a dialog rename.
    #[default]
    Ask,
    /// Save as `name (1).ext`.
    Rename,
    /// Replace the existing file when the download finishes.
    Overwrite,
    /// Don't download it; the download fails with an explanation.
    Skip,
}

impl OnConflict {
    pub fn as_str(self) -> &'static str {
        match self {
            OnConflict::Ask => "ask",
            OnConflict::Rename => "rename",
            OnConflict::Overwrite => "overwrite",
            OnConflict::Skip => "skip",
        }
    }

    pub fn parse(s: &str) -> OnConflict {
        match s {
            "ask" => OnConflict::Ask,
            "overwrite" => OnConflict::Overwrite,
            "skip" => OnConflict::Skip,
            _ => OnConflict::Rename,
        }
    }
}

/// Request context captured from the browser so the server sees the same session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestMeta {
    pub referrer: Option<String>,
    /// The page the download was started from.
    pub page_url: Option<String>,
    /// Value for the `Cookie` header.
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
    pub headers: Vec<(String, String)>,
    /// HTTP method; GET when absent.
    pub method: Option<String>,
    /// Request body (form submissions), replayed on every request.
    #[serde(with = "b64")]
    pub body: Option<Vec<u8>>,
}

/// Largest request body K-Star will store and replay.
pub const MAX_BODY: usize = 1024 * 1024;

/// Decodes standard base64 (how the browser extension sends form bodies).
pub fn decode_base64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s.trim()).ok()
}

mod b64 {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => s.serialize_some(&STANDARD.encode(b)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        s.map(|s| STANDARD.decode(s).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NewDownload {
    pub url: String,
    /// Final filename; worked out from the server response when absent.
    pub filename: Option<String>,
    /// Target folder; the category folder when absent.
    pub dir: Option<PathBuf>,
    pub category: Option<Category>,
    pub meta: RequestMeta,
    /// Add to the list without starting ("Download later"): the download waits for the queue.
    pub start_paused: bool,
    pub connections: Option<u8>,
    /// What to do if the file exists; the setting when absent.
    pub on_conflict: Option<OnConflict>,
}

/// Changes to an existing download.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EditDownload {
    pub filename: Option<String>,
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueMove {
    Up,
    Down,
    Top,
    Bottom,
}

/// A download as presented to the UI. Never contains cookies or other secrets.
#[derive(Debug, Clone, Serialize)]
pub struct DownloadInfo {
    pub id: i64,
    pub url: String,
    pub final_url: Option<String>,
    pub referrer: Option<String>,
    pub page_url: Option<String>,
    pub method: Option<String>,
    pub filename: Option<String>,
    pub dir: Option<PathBuf>,
    pub category: Category,
    pub total_size: Option<u64>,
    pub downloaded: u64,
    pub status: Status,
    pub resumable: Option<bool>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub connections: u8,
    pub speed_limit: u64,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    /// Queue order: lower starts first.
    pub position: i64,
    pub on_conflict: OnConflict,
    /// A completed download whose file is no longer there.
    pub file_missing: bool,
    pub speed: u64,
    pub active_connections: u32,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressItem {
    pub id: i64,
    pub downloaded: u64,
    pub total_size: Option<u64>,
    pub speed: u64,
    pub active_connections: u32,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A download was added or its status/metadata changed.
    Upsert {
        download: Box<DownloadInfo>,
    },
    Removed {
        id: i64,
    },
    /// Periodic (~4 Hz) progress for every running download.
    Progress {
        items: Vec<ProgressItem>,
        total_speed: u64,
    },
    Completed {
        id: i64,
        filename: String,
        path: PathBuf,
    },
    Failed {
        id: i64,
        filename: Option<String>,
        error: String,
    },
}
