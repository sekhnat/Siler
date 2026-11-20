//! Looking at a source before anything is downloaded: what kind it is, and for streams the
//! presentation it offers. Manifests and their playlists are read here, through the URL
//! policy, so nothing a manifest names is contacted unless it passes.

use futures_util::StreamExt;
use kstar_media::classify::{self, Kind};
use kstar_media::hls::{self, Master, MasterVariant};
use kstar_media::policy;
use kstar_media::{ParseError, ParseLimits, Presentation, dash};
use reqwest::Url;
use std::collections::HashMap;

use crate::error::MediaError;
use crate::http::{self, Limiters, Overflow};
use crate::net::Net;
use crate::{Error, RequestMeta, Result};

/// What a source turned out to be.
pub(crate) struct Source {
    pub kind: Kind,
    /// After redirects.
    pub final_url: Url,
    /// Set for HLS and DASH sources that could be read.
    pub presentation: Option<Presentation>,
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

/// Checks that `url` may be contacted: the URL itself (scheme, credentials, literal
/// addresses), and, when no proxy carries the connection, the addresses its name resolves to.
pub(crate) async fn guard(net: &Net, url: &Url, allow_private: bool) -> Result<()> {
    policy::check(url, allow_private)
        .map_err(|e| Error::Media(MediaError::UrlForbidden(e.to_string())))?;
    let route = net.route();
    if allow_private || route.http.is_some() || route.https.is_some() {
        return Ok(());
    }
    if let Some(url::Host::Domain(host)) = url.host() {
        let port = url.port_or_known_default().unwrap_or(443);
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| Error::Network(format!("could not look up {host} ({e})")))?;
        for a in addrs {
            if policy::is_private(a.ip()) {
                return Err(Error::Media(MediaError::UrlForbidden(format!(
                    "{host} points to an address on this computer or the local network"
                ))));
            }
        }
    }
    Ok(())
}

/// Classifies `url` and, for streams, reads the presentation it offers.
///
/// `allow_private` is the user's setting. A source that is itself on a private address
/// may point at other private addresses (a home media server).
pub(crate) async fn source(
    net: &Net,
    url: &Url,
    meta: &RequestMeta,
    ua: &str,
    allow_private: bool,
    lim: &ParseLimits,
    io: &Limiters<'_>,
) -> Result<Source> {
    let allow = allow_private || policy::is_private_source(url);
    guard(net, url, allow).await?;
    let head = http::fetch_bounded(
        net,
        url,
        meta,
        ua,
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
    let final_url = head.final_url;
    let presentation = match kind {
        Kind::Hls => {
            let body = full_body(net, &final_url, meta, ua, lim, io).await?;
            Some(hls_presentation(net, &final_url, &body, meta, ua, allow, lim, io).await?)
        }
        Kind::Dash => {
            let body = full_body(net, &final_url, meta, ua, lim, io).await?;
            Some(dash::parse(&body, &final_url, lim).map_err(map_parse)?)
        }
        _ => None,
    };
    Ok(Source {
        kind,
        final_url,
        presentation,
    })
}

async fn full_body(
    net: &Net,
    url: &Url,
    meta: &RequestMeta,
    ua: &str,
    lim: &ParseLimits,
    io: &Limiters<'_>,
) -> Result<Vec<u8>> {
    let f = http::fetch_bounded(
        net,
        url,
        meta,
        ua,
        lim.max_manifest_bytes as u64,
        Overflow::Refuse,
        io,
    )
    .await?;
    Ok(f.body)
}

/// Builds the HLS presentation: a master with its media playlists, or a bare media playlist
/// (which becomes a presentation with one variant).
#[allow(clippy::too_many_arguments)]
async fn hls_presentation(
    net: &Net,
    url: &Url,
    body: &[u8],
    meta: &RequestMeta,
    ua: &str,
    allow: bool,
    lim: &ParseLimits,
    io: &Limiters<'_>,
) -> Result<Presentation> {
    if hls::is_media_playlist(body) {
        let media = hls::parse_media(body, url, lim).map_err(map_parse)?;
        let master = Master {
            variants: vec![MasterVariant {
                uri: url.clone(),
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
            guard(net, &u, allow).await?;
            let f = http::fetch_bounded(
                net,
                &u,
                meta,
                ua,
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
