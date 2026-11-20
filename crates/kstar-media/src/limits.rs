//! Bounds on everything a manifest can make the parser do (design §7.7). Every limit is
//! enforced, and each has a test that exceeds it.

/// The limits used when parsing a manifest. Defaults match the design; callers may lower
/// them, never raise them past the defaults without a reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseLimits {
    /// Manifest and media playlist body.
    pub max_manifest_bytes: usize,
    /// How much of a source is read to recognise its kind.
    pub sniff_bytes: usize,
    /// An AES-128 key response (it must be exactly 16 bytes).
    pub max_key_bytes: usize,
    pub max_variants: usize,
    pub max_tracks: usize,
    pub max_segments_per_track: usize,
    pub max_periods: usize,
    pub max_xml_depth: usize,
    pub max_xml_elements: usize,
    /// Media playlists fetched for one HLS presentation.
    pub max_media_playlists: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        ParseLimits {
            max_manifest_bytes: 8 * 1024 * 1024,
            sniff_bytes: 64 * 1024,
            max_key_bytes: 64 * 1024,
            max_variants: 256,
            max_tracks: 512,
            max_segments_per_track: 200_000,
            max_periods: 256,
            max_xml_depth: 64,
            max_xml_elements: 1_000_000,
            max_media_playlists: 64,
        }
    }
}
