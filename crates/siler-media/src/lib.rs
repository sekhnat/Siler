//! Pure parsing and selection for adaptive media: HLS and DASH manifests, the
//! presentation model, protection assessment, AES-128 decryption and subtitle conversion.
//!
//! Nothing here touches the network, the filesystem or an async runtime. Every input is
//! untrusted bytes, bounded by [`ParseLimits`]; parsers return typed errors instead of
//! panicking, and the crate forbids `unwrap` and `expect` outside tests.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod attrs;
pub mod classify;
pub mod crypto;
pub mod dash;
pub mod error;
pub mod hds;
pub mod hls;
pub mod limits;
pub mod model;
pub mod mp4;
pub mod playlist;
pub mod policy;
pub mod protection;
pub mod select;
pub mod subtitle;
pub mod xml;

pub use error::ParseError;
pub use limits::ParseLimits;
pub use model::{
    ByteRange, Init, Key, Liveness, ManifestKind, Object, Presentation, Protection, Segment,
    SegmentContainer, SegmentPlan, SubtitleFile, Track, TrackId, TrackKind, Variant,
};
