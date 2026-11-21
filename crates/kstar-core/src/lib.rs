//! K-Star download engine.
//!
//! The [`Engine`] owns a persistent queue of downloads and runs each of them with
//! IDM-style dynamic segmentation: a file is split into ranges fetched over several
//! HTTP connections, and whenever a connection finishes, it takes over the tail half of
//! the largest remaining range so that no connection sits idle near the end.

pub mod batch;
mod category;
pub mod checksum;
mod error;
pub mod extras;
pub mod filename;
pub mod grabber;
pub mod host;
mod http;
pub mod lists;
mod manager;
pub mod media;
pub mod metalink;
mod net;
pub mod postrun;
mod probe;
pub mod proxy;
mod queue;
pub mod quota;
mod ratelimit;
mod resolve;
pub mod scheduler;
mod segment;
mod settings;
mod store;
mod task;
mod types;
pub mod zip;

pub use category::Category;
pub use error::{Error, MediaError};
pub use kstar_media::select::{ContainerChoice, Selection, SubtitleMode};
pub use kstar_media::{ManifestKind, TrackId};
pub use manager::Engine;
pub use media::{
    FfmpegStatus, HintSubtitle, InlineManifest, JobKind, MediaInfo, MediaKind, MediaProbe,
    MediaProgress, NewMediaDownload, SourceHint, TrackView, VariantView,
};
pub use net::{Login, host_key};
pub use probe::ProbeResult;
pub use queue::{AfterDone, MAIN_QUEUE, Queue};
pub use scheduler::Schedule;
pub use segment::Segment;
pub use settings::{
    AfterDownload, CategoryRule, ServerLimit, Settings, SoundSettings, default_categories,
};
pub use types::*;

pub type Result<T, E = Error> = std::result::Result<T, E>;
