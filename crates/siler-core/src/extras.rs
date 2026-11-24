//! The optional parts of a download, kept together in one JSON column: its checksum, mirrors,
//! piece hashes, the archive entry it is, and what happened after it finished.

use serde::{Deserialize, Serialize};

use crate::checksum::{Algo, Checksum, Pieces};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Extras {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<Checksum>,
    /// Other addresses of the same file. The download's own URL is always the first source.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<Mirror>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pieces: Option<Pieces>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zip_entry: Option<ZipEntry>,
    /// The last checksum check of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<VerifyResult>,
    /// The checksum didn't match and the user chose to keep the file anyway.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub accept_unverified: bool,
    /// The command run after the download finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandResult>,
    /// Bytes the download's own URL delivered while there were mirrors, over all runs.
    #[serde(skip_serializing_if = "is_zero")]
    pub own_bytes: u64,
    /// What the last error was, as a code the app can show in its language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
    /// Where the link came from. It holds secrets: never exported or shown.
    #[serde(skip_serializing_if = "crate::LinkSource::is_empty")]
    pub link: crate::LinkSource,
    /// How far refreshing an expired link got.
    #[serde(skip_serializing_if = "crate::refresh::Attempts::is_default")]
    pub(crate) refresh: crate::refresh::Attempts,
    /// What happens when this download finishes, beyond what the settings say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_done: Option<OnDone>,
}

/// What happens when one download finishes (its progress window's "Options on completion").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OnDone {
    /// Show the "Download complete" window; `None` = as the settings say.
    pub complete_dialog: Option<bool>,
    /// Open the file.
    pub open_file: bool,
    /// Then quit Siler, or put the computer to sleep or shut it down.
    pub after: crate::AfterDone,
}

/// An error as a code and its details (see `Error::code_args`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorCode {
    pub code: String,
    pub args: Vec<(String, String)>,
}

impl From<&crate::Error> for ErrorCode {
    fn from(e: &crate::Error) -> Self {
        let (code, args) = e.code_args();
        ErrorCode {
            code: code.to_string(),
            args: args.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        }
    }
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

impl Extras {
    pub fn is_empty(&self) -> bool {
        *self == Extras::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MirrorOrigin {
    #[default]
    User,
    Metalink,
    /// An RFC 6249 `Link: <…>; rel=duplicate` header from the server.
    Server,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mirror {
    pub url: String,
    #[serde(default)]
    pub origin: MirrorOrigin,
    /// Why it isn't used (a different size, no range support, too many failures).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<String>,
    /// Bytes this mirror delivered, over all runs.
    #[serde(default)]
    pub bytes: u64,
}

impl Mirror {
    pub fn new(url: String, origin: MirrorOrigin) -> Mirror {
        Mirror {
            url,
            origin,
            disabled: None,
            bytes: 0,
        }
    }
}

/// One file inside a remote ZIP archive (the download's URL is the archive).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZipEntry {
    /// The path inside the archive.
    pub path: String,
    /// The size of the whole archive, to tell whether it changed on the server.
    pub archive_size: u64,
    /// Where the entry's local header starts.
    pub header_offset: u64,
    pub compressed: u64,
    pub size: u64,
    /// 0 = stored, 8 = deflate.
    pub method: u16,
    pub crc: u32,
    /// Where the compressed data starts, once the local header was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_offset: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyResult {
    pub algo: Algo,
    /// What the file was expected to be; None when the user only asked for its digest.
    pub expected: Option<String>,
    pub actual: String,
    pub at: i64,
}

impl VerifyResult {
    pub fn matches(&self) -> Option<bool> {
        self.expected
            .as_ref()
            .map(|e| e.eq_ignore_ascii_case(&self.actual))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CommandResult {
    /// Still running.
    pub running: bool,
    /// None when it couldn't start or was ended by a signal.
    pub exit_code: Option<i32>,
    /// The end of what it printed (standard output and error).
    pub output: String,
    /// Why it couldn't run.
    pub error: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

impl CommandResult {
    pub fn ok(&self) -> bool {
        !self.running && self.error.is_none() && self.exit_code == Some(0)
    }
}
