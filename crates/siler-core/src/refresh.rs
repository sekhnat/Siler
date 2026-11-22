//! Fresh links for downloads whose link expired: the original address is asked again, then
//! the browser that handed the link over looks at the page, and else the user is asked.

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::os::unix::fs::FileExt;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::manager::{Inner, State, human_size};
use crate::media::{JobKind, MediaKind};
use crate::store::Record;
use crate::task::Running;
use crate::{
    Asked, Engine, Error, Event, MediaError, ProbeResult, RequestMeta, Result, Segment, SourceHint,
    Status, filename, probe,
};

/// Where a link came from, as far as it takes to find it again. Kept with the download; it
/// holds secrets, so it is never exported or shown.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LinkSource {
    /// The start of the redirects that led to the link, and the cookies for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first: Option<FirstLink>,
    /// The browser instance that handed it over. None for private windows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    /// Its container (Firefox's cookie store).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<Locator>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoRef>,
}

impl LinkSource {
    pub fn is_empty(&self) -> bool {
        *self == LinkSource::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FirstLink {
    pub url: String,
    pub cookies: Option<String>,
}

/// What tells the link from the others on its page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Locator {
    /// The link's address without its query: that is where tokens usually are.
    pub path: Option<String>,
    /// The words of the link.
    pub text: Option<String>,
    /// The file's name.
    pub name: Option<String>,
}

/// A recognised site's video, and which of its formats it was.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoRef {
    pub site: String,
    pub id: String,
    pub format: Option<String>,
}

/// What a [`LinkFinder`] is asked for.
#[derive(Debug, Clone)]
pub struct LinkAsk {
    pub id: i64,
    /// The page to look at.
    pub page: String,
    /// The link that expired.
    pub url: String,
    pub filename: Option<String>,
    pub size: Option<u64>,
    /// A stream, not a file.
    pub media: bool,
    pub source: LinkSource,
}

/// A fresh link, as the browser handed it over.
#[derive(Debug, Clone, Default)]
pub struct FoundLink {
    pub url: String,
    pub meta: Option<RequestMeta>,
    /// What came with a video (a manifest from the page).
    pub hint: Option<SourceHint>,
    pub source: Option<LinkSource>,
}

pub type LinkSearch<'a> = Pin<Box<dyn Future<Output = Result<FoundLink, String>> + Send + 'a>>;

/// Looks for a fresh link in the browser that handed the old one over.
pub trait LinkFinder: Send + Sync + 'static {
    fn find(&self, ask: LinkAsk) -> LinkSearch<'_>;
}

// ---------------------------------------------------------------- the engine's side

/// Automatic attempts without progress, after which the user is asked.
const MAX_TRIES: u32 = 2;
/// A link this old when its turn came has waited, and may have expired meanwhile.
const WAITED_MS: i64 = 30_000;
/// The finder has its own limit per tab; this one also covers waiting for its turn.
const FIND_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Compared in up to three places of what is on disk.
const SAMPLE: u64 = 64 * 1024;
const SAMPLES: u64 = 3;

/// How far automatic refreshing got, kept with the download.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Attempts {
    tries: u32,
    /// The bytes there were at the last attempt.
    at: u64,
    /// The link expired and the user is to fetch a fresh one.
    pub expired: bool,
}

impl Attempts {
    pub fn is_default(&self) -> bool {
        *self == Attempts::default()
    }

    /// Counts an attempt; `None` when there have been enough.
    fn begin(&mut self, downloaded: u64) -> Option<u32> {
        if downloaded > self.at {
            self.tries = 0;
        }
        if self.tries >= MAX_TRIES {
            return None;
        }
        self.tries += 1;
        self.at = downloaded;
        Some(self.tries)
    }
}

/// The failure a dead link gives.
fn says_expired(e: &Error) -> bool {
    matches!(
        e,
        Error::Http(401 | 403 | 404 | 410)
            | Error::Media(MediaError::LinksExpired)
            | Error::AuthRequired {
                asked: Asked::Unsaid,
                rejected: false,
                ..
            }
    )
}

/// Whether `e` means that the link expired: it worked before, or waited before its turn.
/// A link that fails at once is just a bad link.
pub(crate) fn expired(rec: &Record, downloaded: u64, waited: bool, e: &Error) -> bool {
    if !says_expired(e) || rec.kind == JobKind::Stream || rec.media.as_ref().is_some_and(|m| m.live)
    {
        return false;
    }
    matches!(e, Error::Media(MediaError::LinksExpired))
        || waited
        || downloaded > 0
        || rec.extras.refresh.tries > 0
}

pub(crate) fn waited(rec: &Record, now: i64) -> bool {
    now.saturating_sub(rec.created_at) >= WAITED_MS
}

/// What the failure is called: a 401 that names no way to log in is no login question.
pub(crate) fn failure(e: Error) -> Error {
    match e {
        Error::AuthRequired {
            asked: Asked::Unsaid,
            ..
        } => Error::Http(401),
        e => e,
    }
}

fn is_get(meta: &RequestMeta) -> bool {
    meta.method
        .as_deref()
        .is_none_or(|m| m.trim().eq_ignore_ascii_case("GET"))
}

/// Who wants the link changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum By {
    User,
    /// Nobody is watching: anything that can't be shown to be the same file is refused.
    Siler,
}

/// A link that passed the same-file check.
pub(crate) struct Fresh {
    url: String,
    meta: RequestMeta,
    probe: Option<ProbeResult>,
    hint: Option<SourceHint>,
    source: Option<LinkSource>,
}

/// The places to compare in what is on disk: up to three, spread over it.
fn samples(segments: &[Segment]) -> Vec<(u64, u64)> {
    let mut done: Vec<(u64, u64)> = segments
        .iter()
        .filter(|s| s.pos > s.start)
        .map(|s| (s.start, s.pos))
        .collect();
    done.sort_unstable();
    let total: u64 = done.iter().map(|(a, b)| b - a).sum();
    if total == 0 {
        return Vec::new();
    }
    let len = SAMPLE.min(total);
    let mut out: Vec<(u64, u64)> = Vec::new();
    for i in 0..SAMPLES {
        // Counted along what is there, then found in the file.
        let mut skip = (total - len) * i / (SAMPLES - 1);
        for &(a, b) in &done {
            if skip < b - a {
                let found = (a + skip, len.min(b - a - skip));
                if !out.contains(&found) {
                    out.push(found);
                }
                break;
            }
            skip -= b - a;
        }
    }
    out
}

fn another_file(why: &str) -> Error {
    Error::LinkMismatch(format!("The new link points to a different file ({why})."))
}

impl Inner {
    /// Checks that a link leads to the same file or stream, and changes nothing.
    pub(crate) async fn check_link(
        self: &Arc<Self>,
        id: i64,
        found: FoundLink,
        by: By,
    ) -> Result<Fresh> {
        let url = probe::parse_url(&self.strip_login(&found.url)?)?;
        let (rec, table) = {
            let st = self.state();
            let rec = st.records.get(&id).cloned().ok_or(Error::NotFound)?;
            let table = st
                .running
                .get(&id)
                .map(|run| run.table.lock().unwrap().segs.clone())
                .filter(|segs| !segs.is_empty());
            (rec, table)
        };
        if rec.status == Status::Completed {
            return Err(Error::Protocol("This download has already finished".into()));
        }
        // A link without a context: the old one stays with the old link's site.
        let meta = found
            .meta
            .map_or(rec.meta.clone(), |m| m.bound_to(url.as_str()));
        let mut fresh = Fresh {
            url: url.to_string(),
            meta,
            probe: None,
            hint: None,
            source: found.source,
        };

        if rec.kind.is_media() {
            // A stream is checked when it runs: the same tracks must be offered, and the
            // finished pieces must still belong to it.
            let limits = siler_media::ParseLimits::default();
            fresh.hint = found.hint.map(|h| h.checked(&limits)).transpose()?;
            let probe = Engine::from_inner(self.clone())
                .probe_media(url.as_str(), &fresh.meta, fresh.hint.as_ref())
                .await?;
            if !matches!(probe.kind, MediaKind::Hls | MediaKind::Dash) {
                return Err(Error::LinkMismatch(
                    probe
                        .message
                        .unwrap_or_else(|| "The new link isn't a stream.".into()),
                ));
            }
            return Ok(fresh);
        }

        let s = self.settings();
        let p = probe::probe(&self.net, url.as_str(), &fresh.meta, &s.user_agent).await?;
        let zip = rec.extras.zip_entry.as_ref();
        let old_size = zip.map(|z| z.archive_size).or(rec.total_size);
        match (old_size, p.total_size) {
            (Some(a), Some(b)) if a != b => {
                return Err(another_file(&format!(
                    "{} instead of {}",
                    human_size(b),
                    human_size(a)
                )));
            }
            (Some(_), None) => {
                return Err(Error::LinkMismatch(
                    "The new link doesn't report the file size, so Siler can't tell whether it is the same file."
                        .into(),
                ));
            }
            _ => {}
        }

        let segments = table.as_deref().unwrap_or(&rec.segments);
        let places = samples(segments);
        if by == By::Siler {
            let html = p
                .content_type
                .as_deref()
                .is_some_and(|t| t.to_ascii_lowercase().starts_with("text/html"));
            let wanted_html = rec.filename.as_deref().is_some_and(|n| {
                let n = n.to_ascii_lowercase();
                n.ends_with(".html") || n.ends_with(".htm")
            });
            if html && !wanted_html {
                return Err(another_file("a web page"));
            }
            if !places.is_empty() && (old_size.is_none() || !p.resumable) {
                return Err(Error::LinkMismatch(
                    "The new link can't continue what was downloaded.".into(),
                ));
            }
        }

        let part = match (&rec.dir, &rec.filename) {
            (Some(dir), Some(name)) => Some(filename::part_path(dir, name)),
            _ => None,
        };
        if p.resumable
            && let Some(part) = part.filter(|part| !places.is_empty() && part.exists())
        {
            let base = zip.and_then(|z| z.data_offset).unwrap_or(0);
            let file = std::fs::File::open(part)?;
            for (at, len) in places {
                let mut here = vec![0u8; len as usize];
                file.read_exact_at(&mut here, at)?;
                let there = probe::fetch_range(
                    &self.net,
                    &p.final_url,
                    &p.final_meta,
                    &s.user_agent,
                    base + at,
                    len,
                )
                .await?;
                if there != here {
                    return Err(another_file("its contents differ"));
                }
            }
        }
        fresh.probe = Some(p);
        Ok(fresh)
    }

    /// Puts a checked link in place. The status is the caller's.
    pub(crate) fn put_link(rec: &mut Record, fresh: Fresh, by: By) {
        rec.url = fresh.url;
        rec.meta = fresh.meta;
        if let Some(p) = fresh.probe {
            rec.final_url = Some(p.final_url);
            // Another edge may name the same bytes differently; they were compared.
            rec.etag = p.etag;
            rec.last_modified = p.last_modified;
        }
        if let Some(media) = rec.media.as_mut() {
            match fresh.hint {
                Some(hint) if hint.manifest.is_some() => media.hint = Some(hint),
                // The new link is read from now on, not a manifest the page gave as text.
                _ => {
                    if let Some(h) = media.hint.as_mut() {
                        h.manifest = None;
                    }
                }
            }
        }
        if let Some(new) = fresh.source {
            let old = &mut rec.extras.link;
            *old = LinkSource {
                first: new.first.or(old.first.take()),
                browser: new.browser.or(old.browser.take()),
                container: new.container.or(old.container.take()),
                locator: new.locator.or(old.locator.take()),
                video: new.video.or(old.video.take()),
            };
        }
        match by {
            By::User => {
                rec.extras.refresh = Attempts::default();
                rec.notice = Some("The download link was updated.".into());
            }
            By::Siler => {
                rec.extras.refresh.expired = false;
                rec.notice = Some("The link had expired, so Siler got a fresh one.".into());
            }
        }
        rec.error = None;
        rec.needs_login = None;
    }

    /// An expired link stopped a run: counts the attempt, if one is still allowed.
    pub(crate) fn begin_refresh(&self, id: i64, run: &Running) -> Option<u32> {
        let allowed = self.settings().auto_refresh_links;
        let mut st = self.state();
        let rec = st.records.get_mut(&id)?;
        {
            let table = run.table.lock().unwrap();
            if !table.segs.is_empty() {
                rec.segments = table.segs.clone();
                rec.downloaded = table.downloaded();
            } else if rec.kind.is_media() {
                rec.downloaded = run.downloaded.load(Relaxed);
            }
        }
        if !allowed || !is_get(&rec.meta) {
            return None;
        }
        let n = rec.extras.refresh.begin(rec.downloaded)?;
        if let Err(e) = self.persist_and_emit(rec, Some(run)) {
            tracing::warn!("failed to persist download {id}: {e}");
        }
        Some(n)
    }

    /// The quiet first step, inside the run: the original address is asked for a new
    /// redirect. True when the run can go on.
    pub(crate) async fn renew_silently(self: &Arc<Self>, id: i64, run: &Running) -> bool {
        tokio::select! {
            renewed = self.ask_again(id) => renewed,
            _ = run.cancel.cancelled() => false,
        }
    }

    async fn ask_again(self: &Arc<Self>, id: i64) -> bool {
        let Some(rec) = self.state().records.get(&id).cloned() else {
            return false;
        };
        let media = rec.kind.is_media();
        // Its own address, when that only redirects to the link that died. (A stream reads
        // its address again on every attempt anyway.)
        if !media && rec.final_url.as_deref().is_some_and(|f| f != rec.url) {
            let found = FoundLink {
                url: rec.url.clone(),
                ..Default::default()
            };
            match self.check_link(id, found, By::Siler).await {
                Ok(fresh) => return self.put_renewed(id, fresh),
                Err(e) => tracing::info!("download {id}: its address gave no fresh link: {e}"),
            }
        }
        let Some(first) = rec.extras.link.first.as_ref().filter(|f| f.url != rec.url) else {
            return false;
        };
        // With the first address's own cookies, and no others.
        let meta = RequestMeta {
            referrer: rec.meta.referrer.clone(),
            page_url: rec.meta.page_url.clone(),
            user_agent: rec.meta.user_agent.clone(),
            cookies: first.cookies.clone(),
            ..Default::default()
        }
        .bound_to(&first.url);
        let ua = self.settings().user_agent;
        let led = match probe::probe(&self.net, &first.url, &meta, &ua).await {
            Ok(led) => led,
            Err(e) => {
                tracing::info!("download {id}: the first address gave no fresh link: {e}");
                return false;
            }
        };
        let home = url::Url::parse(&led.final_url).is_ok_and(|u| rec.meta.is_home(&u));
        let found = FoundLink {
            url: led.final_url,
            meta: Some(if home {
                rec.meta.clone()
            } else {
                led.final_meta
            }),
            ..Default::default()
        };
        match self.check_link(id, found, By::Siler).await {
            Ok(fresh) => self.put_renewed(id, fresh),
            Err(e) => {
                tracing::info!("download {id}: the first address's link won't do: {e}");
                false
            }
        }
    }

    fn put_renewed(&self, id: i64, fresh: Fresh) -> bool {
        let mut st = self.state();
        let Some(rec) = st.records.get_mut(&id) else {
            return false;
        };
        Self::put_link(rec, fresh, By::Siler);
        match self.persist_and_emit(rec, None) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("failed to persist download {id}: {e}");
                false
            }
        }
    }

    /// Who could find a fresh link, and what to ask.
    pub(crate) fn browser_to_ask(&self, rec: &Record) -> Option<(Arc<dyn LinkFinder>, LinkAsk)> {
        let finder = self.finder.read().unwrap().clone()?;
        rec.extras.link.browser.as_ref()?;
        let page = rec.meta.page_url.as_ref().or(rec.meta.referrer.as_ref())?;
        if !page.starts_with("http://") && !page.starts_with("https://") {
            return None;
        }
        let ask = LinkAsk {
            id: rec.id,
            page: page.clone(),
            url: rec.url.clone(),
            filename: rec.filename.clone(),
            size: rec
                .extras
                .zip_entry
                .as_ref()
                .map(|z| z.archive_size)
                .or(rec.total_size),
            media: rec.kind.is_media(),
            source: rec.extras.link.clone(),
        };
        Some((finder, ask))
    }

    /// The second step, outside the run. The download stays queued meanwhile, so it counts
    /// as busy, and isn't started until this ends.
    pub(crate) async fn refresh_in_browser(
        self: Arc<Self>,
        finder: Arc<dyn LinkFinder>,
        ask: LinkAsk,
        failed: Error,
        token: CancellationToken,
    ) {
        let id = ask.id;
        let work = async {
            let found = tokio::time::timeout(FIND_TIMEOUT, finder.find(ask))
                .await
                .map_err(|_| "the browser took too long".to_string())??;
            self.check_link(id, found, By::Siler)
                .await
                .map_err(|e| e.to_string())
        };
        let result = tokio::select! {
            result = work => result,
            _ = token.cancelled() => {
                self.finish_queues(&mut self.state(), false);
                return;
            }
            _ = self.quit.cancelled() => return,
        };
        let mut gave_up = None;
        {
            let mut st = self.state();
            if token.is_cancelled() || st.shutting_down {
                return;
            }
            st.refreshing.remove(&id);
            let State {
                records,
                queue_results,
                ..
            } = &mut *st;
            let Some(rec) = records.get_mut(&id) else {
                return;
            };
            match result {
                Ok(fresh) => Self::put_link(rec, fresh, By::Siler),
                Err(why) => {
                    tracing::info!("download {id}: no fresh link: {why}");
                    rec.notice = None;
                    rec.extras.refresh.expired = true;
                    // The queue may have been stopped meanwhile: then it just waits.
                    if rec.status == Status::Queued {
                        rec.status = Status::Failed;
                        rec.error = Some(failed.to_string());
                        rec.extras.error_code = Some((&failed).into());
                        if let Some(counts) = queue_results.get_mut(&rec.queue_id) {
                            counts.1 += 1;
                        }
                        gave_up = Some(Event::Failed {
                            id,
                            filename: rec.filename.clone(),
                            error: failed.to_string(),
                            code: Some((&failed).into()),
                            needs_link: true,
                        });
                    }
                }
            }
            if let Err(e) = self.persist_and_emit(rec, None) {
                tracing::warn!("failed to persist download {id}: {e}");
            }
        }
        let announce = gave_up.is_some();
        if let Some(event) = gave_up {
            self.emit(event);
        }
        self.schedule();
        self.finish_queues(&mut self.state(), announce);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_to_compare() {
        let seg = |start, pos, end| Segment {
            pos,
            ..Segment::new(start, end)
        };
        assert!(samples(&[seg(0, 0, 100)]).is_empty());
        assert_eq!(samples(&[seg(0, 10, 100)]), [(0, 10)]);
        let mb = 1 << 20;
        let got = samples(&[seg(4 * mb, 5 * mb, 8 * mb), seg(0, mb, 4 * mb)]);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], (0, SAMPLE));
        assert_eq!(got[2], (5 * mb - SAMPLE, SAMPLE));
        for (at, len) in got {
            assert!(at + len <= mb || (at >= 4 * mb && at + len <= 5 * mb));
        }
    }

    #[test]
    fn attempts_start_over_with_progress() {
        let mut a = Attempts::default();
        assert_eq!(a.begin(10), Some(1));
        assert_eq!(a.begin(10), Some(2));
        assert_eq!(a.begin(10), None);
        assert_eq!(a.begin(11), Some(1));
    }
}
