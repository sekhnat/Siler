//! K-Star download engine.
//!
//! The [`Engine`] owns a persistent queue of downloads and runs each of them with
//! IDM-style dynamic segmentation: a file is split into ranges fetched over several
//! HTTP connections, and whenever a connection finishes, it takes over the tail half of
//! the largest remaining range so that no connection sits idle near the end.

mod category;
mod error;
pub mod filename;
mod manager;
mod net;
mod probe;
pub mod proxy;
mod ratelimit;
mod segment;
mod settings;
mod store;
mod task;
mod types;

pub use category::Category;
pub use error::Error;
pub use manager::Engine;
pub use net::{Login, host_key};
pub use probe::ProbeResult;
pub use segment::Segment;
pub use settings::{CategoryRule, ServerLimit, Settings, default_categories};
pub use types::*;

pub type Result<T, E = Error> = std::result::Result<T, E>;
