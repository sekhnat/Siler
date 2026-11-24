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
    /// The site all of this was given for (`https://host[:port]`). Cookies, headers and the
    /// body go nowhere else. When absent, the engine uses the site of the first address.
    pub origin: Option<String>,
}

/// Largest request body Siler will store and replay.
pub const MAX_BODY: usize = 1024 * 1024;

/// Decodes standard base64 (how the browser extension sends form bodies).
pub fn decode_base64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .ok()
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
    /// The queue to put it in; the main queue when absent.
    pub queue_id: Option<i64>,
    /// The file's expected checksum, checked when it finishes.
    pub checksum: Option<crate::checksum::Checksum>,
    /// Other addresses of the same file, fetched from at the same time.
    pub mirrors: Vec<crate::extras::Mirror>,
    /// Hashes of the file's pieces (from a metalink), to repair only damaged parts.
    pub pieces: Option<crate::checksum::Pieces>,
    /// Fetch only this file out of the ZIP archive at `url`.
    pub zip_entry: Option<crate::extras::ZipEntry>,
    /// Where the link came from, to find it again when it expires.
    pub link: crate::LinkSource,
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
    pub queue_id: i64,
    /// Checked for a newer version this often (seconds); 0 = not kept up to date.
    pub sync_every: u64,
    /// The last check for a newer version (ms since the epoch).
    pub sync_checked_at: Option<i64>,
    /// A completed download whose file is no longer there.
    pub file_missing: bool,
    /// The error as a code, for the app's language (with `error`).
    pub error_code: Option<crate::extras::ErrorCode>,
    /// Stopped because the server wants a login.
    pub needs_login: Option<LoginRequest>,
    /// Stopped because an SFTP server's key has to be trusted first.
    pub needs_host_key: Option<HostKeyRequest>,
    /// Stopped because its link expired and no fresh one was found: the user can fetch one.
    pub needs_link: bool,
    pub speed: u64,
    pub active_connections: u32,
    pub segments: Vec<Segment>,
    /// A media download (video or audio stream) rather than a plain file.
    pub media: Option<crate::media::MediaInfo>,
    /// The expected checksum.
    pub checksum: Option<crate::checksum::Checksum>,
    /// The last check of the file against a checksum (or its computed digest).
    pub verify: Option<crate::extras::VerifyResult>,
    /// Failed because the checksum didn't match; the data is kept until the user decides.
    pub checksum_failed: bool,
    /// Every address the file is fetched from, the download's own URL first.
    pub sources: Vec<SourceView>,
    /// The path inside the archive, for a file fetched out of a ZIP.
    pub zip_path: Option<String>,
    /// The command run after it finished.
    pub command: Option<crate::extras::CommandResult>,
    /// What it cost on the network beyond its own bytes (see [`Traffic`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traffic: Option<Traffic>,
    /// The HTTP version its own server answers over (1 or 2), while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u8>,
}

/// One address of a download, with what it delivered.
#[derive(Debug, Clone, Serialize)]
pub struct SourceView {
    pub url: String,
    pub origin: crate::extras::MirrorOrigin,
    /// Why it isn't used.
    pub disabled: Option<String>,
    pub bytes: u64,
    pub active: u32,
}

/// A server wants a login for a download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginRequest {
    /// What the login is kept under: `host[:port]`, or `scheme://host[:port]` for FTP, SFTP
    /// and Windows logins.
    pub host: String,
    pub realm: Option<String>,
    /// A login was sent and refused.
    pub rejected: bool,
    /// What kind of login answers it.
    #[serde(default)]
    pub asked: Asked,
}

/// What a login form should ask for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Asked {
    #[default]
    Password,
    /// The 401 didn't say how to log in (often a link that expired).
    Unsaid,
    /// A Windows login (`DOMAIN\user`).
    Windows,
    /// A Windows login, or the user's Kerberos ticket.
    Ticket,
    /// The ticket was refused, or there is none.
    TicketFailed,
}

/// An SFTP server whose key Siler hasn't seen: shown to the user to trust or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostKeyRequest {
    /// The login key of the server (`sftp://host[:port]`), as downloads refer to it.
    pub host: String,
    pub server: String,
    pub port: u16,
    /// e.g. `ssh-ed25519`.
    pub algorithm: String,
    /// `SHA256:…`, as `ssh` shows it.
    pub fingerprint: String,
    /// The key in OpenSSH form, saved to Siler's known_hosts when trusted.
    pub key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressItem {
    pub id: i64,
    pub downloaded: u64,
    pub total_size: Option<u64>,
    pub speed: u64,
    pub active_connections: u32,
    pub segments: Vec<Segment>,
    /// Phase and piece counts of a media download.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<crate::media::MediaProgress>,
    /// What a finished transfer is doing before the file is ready.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<PhaseProgress>,
    /// Bytes per source, in the order of `DownloadInfo::sources`, when there are mirrors.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub source_bytes: Vec<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traffic: Option<Traffic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u8>,
}

/// What a download cost on the network beyond its own bytes: everything received (`wire`),
/// the part of it thrown away (a dropped connection's unwritten bytes, a restart's discarded
/// data, damaged pieces fetched again), and the retries it took. Counted over the download's
/// whole life, across pauses; absent until something was measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Traffic {
    pub wire: u64,
    pub wasted: u64,
    pub retries: u32,
}

impl Traffic {
    /// None when nothing was measured (a download from before Siler counted, or not started).
    pub fn measured(self) -> Option<Traffic> {
        (self.wire > 0).then_some(self)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaseProgress {
    /// `verifying` or `unpacking`.
    pub name: &'static str,
    pub done: u64,
    pub total: u64,
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
        /// The error as a code, for the app's language.
        code: Option<crate::extras::ErrorCode>,
        /// Its link expired: the user can fetch a fresh one from the page.
        needs_link: bool,
    },
    /// The list of queues changed (added, edited, removed, started or stopped).
    Queues {
        queues: Vec<crate::Queue>,
    },
    /// A started queue has nothing left to run.
    QueueDone {
        queue_id: i64,
        name: String,
        after_done: crate::AfterDone,
        completed: usize,
        failed: usize,
    },
    /// A download stopped because its server wants a login (instead of `Failed`).
    LoginRequired {
        id: i64,
        filename: Option<String>,
        request: LoginRequest,
    },
    /// A download stopped because its SFTP server's key isn't known yet.
    HostKeyUnknown {
        id: i64,
        filename: Option<String>,
        request: HostKeyRequest,
    },
    /// A download turned out to be a metalink: it was removed, and these are its files, to
    /// be added instead.
    Metalink {
        id: i64,
        files: Vec<NewDownload>,
    },
    /// The command run after a download finished (or failed to start).
    CommandDone {
        id: i64,
        filename: Option<String>,
        result: crate::extras::CommandResult,
    },
    /// The data quota for the current period is used up (or, with `reached` false, the
    /// period ended or the user lifted it).
    Quota(crate::quota::QuotaStatus),
    /// Progress of a site grab.
    Grab(Box<crate::grabber::GrabProgress>),
}
