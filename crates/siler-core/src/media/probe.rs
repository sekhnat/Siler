//! Looking at a source before anything is downloaded: what kind it is, and for streams the
//! presentation it offers. Manifests and their playlists are read here, through the URL
//! policy, so nothing a manifest names is contacted unless it passes.

use futures_util::StreamExt;
use reqwest::Url;
use siler_media::classify::{self, Kind};
use siler_media::hls::{self, Master, MasterVariant};
use siler_media::{ParseError, ParseLimits, Presentation, dash};
use std::collections::HashMap;

use super::{InlineManifest, SourceHint};
use crate::error::MediaError;
use crate::http::{self, Http, Limiters, Overflow, Req};
use crate::{Error, Result};

/// What a source turned out to be.
pub(crate) struct Source {
    pub kind: Kind,
    /// After redirects.
    pub final_url: Url,
    /// Set for HLS, DASH and HDS sources that could be read.
    pub presentation: Option<Presentation>,
    /// The links of a plain playlist (M3U, PLS, ASX).
    pub entries: Vec<siler_media::playlist::Entry>,
}

/// Turns a parse error into the message the user sees.
pub(crate) fn map_parse(e: ParseError) -> Error {
    Error::Media(match e {
        ParseError::Invalid(m) => MediaError::Invalid(m),
        ParseError::Unsupported(m) => MediaError::Unsupported(m),
        ParseError::TooLarge { limit } => MediaError::BodyTooLarge(limit as u64),
        ParseError::TooMany { what, .. } => MediaError::Invalid(format!("it has too many {what}")),
        ParseError::Doctype => MediaError::Invalid("it contains DTD declarations".into()),
    })
}

/// Classifies `url` and, for streams, reads the presentation it offers. The first bytes are
/// read once to recognise the source; a manifest that fits in them isn't fetched again.
async fn source(http: &Http, url: &Url, lim: &ParseLimits, io: &Limiters<'_>) -> Result<Source> {
    let head = http::fetch_bounded(
        http,
        url,
        Req::Source,
        lim.sniff_bytes as u64,
        Overflow::Truncate,
        io,
    )
    .await?;
    let kind = classify::classify(
        &head.body,
        head.content_type.as_deref(),
        head.final_url.path(),
    );
    let final_url = head.final_url.clone();
    let mut entries = Vec::new();
    let presentation = match kind {
        Kind::Hls | Kind::Dash | Kind::Hds | Kind::Playlist => {
            let body = if head.complete {
                head.body
            } else {
                http::fetch_bounded(
                    http,
                    url,
                    Req::Source,
                    lim.max_manifest_bytes as u64,
                    Overflow::Refuse,
                    io,
                )
                .await?
                .body
            };
            match kind {
                Kind::Hls => Some(hls_presentation(http, &final_url, &body, lim, io).await?),
                Kind::Dash => Some(dash_presentation(&body, &final_url, lim)?),
                Kind::Hds => Some(hds_presentation(http, &final_url, &body, lim, io).await?),
                _ => {
                    entries =
                        siler_media::playlist::parse(&body, &final_url, lim).map_err(map_parse)?;
                    None
                }
            }
        }
        _ => None,
    };
    Ok(Source {
        kind,
        final_url,
        presentation,
        entries,
    })
}

/// Reads a source: the manifest the page gave as text when there is one, else the one at
/// `url`. The page's title and subtitle files are added to what the manifest offers.
pub(crate) async fn resolve(
    http: &Http,
    url: &Url,
    hint: Option<&SourceHint>,
    lim: &ParseLimits,
    io: &Limiters<'_>,
) -> Result<Source> {
    let mut src = match hint.and_then(|h| h.manifest.as_ref()) {
        Some(m) => inline(http, m, lim, io).await?,
        None => source(http, url, lim, io).await?,
    };
    if let (Some(h), Some(p)) = (hint, src.presentation.as_mut()) {
        if h.title.is_some() {
            p.title = h.title.clone();
        }
        p.add_subtitle_files(&h.subtitle_files());
    }
    Ok(src)
}

/// A manifest given as text. Its playlists (HLS) and everything it names are fetched like
/// those of any manifest, through the URL policy.
async fn inline(
    http: &Http,
    m: &InlineManifest,
    lim: &ParseLimits,
    io: &Limiters<'_>,
) -> Result<Source> {
    let base = Url::parse(&m.base_url).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    if m.body.len() > lim.max_manifest_bytes {
        return Err(Error::Media(MediaError::BodyTooLarge(
            lim.max_manifest_bytes as u64,
        )));
    }
    let body = m.body.as_bytes();
    let (kind, presentation) = match m.kind.as_str() {
        "dash" => (Kind::Dash, dash_presentation(body, &base, lim)?),
        "hls" => (
            Kind::Hls,
            hls_presentation(http, &base, body, lim, io).await?,
        ),
        other => {
            return Err(Error::Media(MediaError::Unsupported(format!(
                "a manifest of kind “{other}”"
            ))));
        }
    };
    Ok(Source {
        kind,
        final_url: base,
        presentation: Some(presentation),
        entries: Vec::new(),
    })
}

/// A DASH presentation, as of now (a live MPD lists the segments available at this moment).
fn dash_presentation(body: &[u8], url: &Url, lim: &ParseLimits) -> Result<Presentation> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    dash::parse_at(body, url, lim, Some(now)).map_err(map_parse)
}

/// An HDS presentation: the manifest, and the bootstraps it names by address.
async fn hds_presentation(
    http: &Http,
    url: &Url,
    body: &[u8],
    lim: &ParseLimits,
    io: &Limiters<'_>,
) -> Result<Presentation> {
    let manifest = siler_media::hds::parse_manifest(body, url, lim).map_err(map_parse)?;
    let mut fetched = HashMap::new();
    for u in manifest.bootstrap_urls() {
        let f = http::fetch_bounded(
            http,
            &u,
            Req::Derived,
            lim.max_manifest_bytes as u64,
            Overflow::Refuse,
            io,
        )
        .await?;
        fetched.insert(u, f.body);
    }
    manifest.presentation(&fetched, lim).map_err(map_parse)
}

/// Builds the HLS presentation: a master with its media playlists, or a bare media playlist
/// (which becomes a presentation with one variant).
async fn hls_presentation(
    http: &Http,
    url: &Url,
    body: &[u8],
    lim: &ParseLimits,
    io: &Limiters<'_>,
) -> Result<Presentation> {
    if hls::is_media_playlist(body) {
        let media = hls::parse_media(body, url, lim).map_err(map_parse)?;
        let master = Master {
            variants: vec![MasterVariant {
                uri: url.clone(),
                // A bare playlist states no bandwidth; 1 marks that (and keeps track ids
                // stable), and is never shown.
                bandwidth: 1,
                codecs: None,
                resolution: None,
                audio_group: None,
                subtitle_group: None,
            }],
            renditions: Vec::new(),
            session_keys: Vec::new(),
        };
        let mut lists = HashMap::new();
        lists.insert(url.clone(), media);
        return hls::assemble(&master, &lists, lim).map_err(map_parse);
    }
    let master = hls::parse_master(body, url, lim).map_err(map_parse)?;
    let urls = hls::playlist_urls(&master, lim).map_err(map_parse)?;
    let fetched: Vec<Result<(Url, hls::Media)>> = futures_util::stream::iter(urls)
        .map(|u| async move {
            let f = http::fetch_bounded(
                http,
                &u,
                Req::Derived,
                lim.max_manifest_bytes as u64,
                Overflow::Refuse,
                io,
            )
            .await?;
            let media = hls::parse_media(&f.body, &f.final_url, lim).map_err(map_parse)?;
            Ok((u, media))
        })
        .buffer_unordered(4)
        .collect()
        .await;
    let mut lists = HashMap::new();
    for r in fetched {
        let (u, media) = r?;
        lists.insert(u, media);
    }
    hls::assemble(&master, &lists, lim).map_err(map_parse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_errors_become_user_messages() {
        assert!(matches!(
            map_parse(ParseError::Doctype),
            Error::Media(MediaError::Invalid(_))
        ));
        assert!(matches!(
            map_parse(ParseError::Unsupported("x".into())),
            Error::Media(MediaError::Unsupported(_))
        ));
        assert!(matches!(
            map_parse(ParseError::TooLarge { limit: 8 }),
            Error::Media(MediaError::BodyTooLarge(8))
        ));
    }
}
