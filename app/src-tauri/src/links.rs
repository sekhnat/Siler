//! Expired links: the browsers that can be asked for fresh ones, and the downloads that
//! wait for the user to click the link again.

use siler_core::{DownloadInfo, FoundLink, LinkAsk, LinkFinder, LinkSearch, LinkSource};
use siler_ipc::{BrowserItem, LinkInfo, LinkTask, Response};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch};

use crate::commands::{hint_of, meta_of};

/// One tab's time, and a little for the messages.
const ANSWER: Duration = Duration::from_secs(65);
/// A browser's host looks for Siler every few seconds, so just after Siler started it may
/// not have connected yet.
const CONNECTING: Duration = Duration::from_secs(6);

type Found = Result<BrowserItem, String>;

struct Port {
    serial: u64,
    browser: String,
    to: mpsc::UnboundedSender<Response>,
}

struct Question {
    port: u64,
    answer: oneshot::Sender<Found>,
}

#[derive(Default)]
struct State {
    ports: Vec<Port>,
    questions: HashMap<u64, Question>,
}

/// The browsers subscribed over the socket.
pub struct Browsers {
    state: Mutex<State>,
    next: AtomicU64,
    /// Changes whenever a browser subscribes.
    joined: watch::Sender<u64>,
    /// One tab at a time per site.
    turns: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// A browser's subscription, while its connection lasts.
pub struct Subscription {
    browsers: Arc<Browsers>,
    serial: u64,
}

impl Subscription {
    /// The browser's answer. Only to a question it was asked.
    pub fn answer(&self, request: u64, found: Found) {
        let question = {
            let mut st = self.browsers.state.lock().unwrap();
            match st.questions.get(&request) {
                Some(q) if q.port == self.serial => st.questions.remove(&request),
                _ => None,
            }
        };
        if let Some(q) = question {
            let _ = q.answer.send(found);
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut st = self.browsers.state.lock().unwrap();
        st.ports.retain(|p| p.serial != self.serial);
        // Whoever waits for this browser hears it at once.
        st.questions.retain(|_, q| q.port != self.serial);
    }
}

/// Takes a question back when nobody waits for its answer any more.
struct Open<'a> {
    browsers: &'a Browsers,
    request: u64,
    to: mpsc::UnboundedSender<Response>,
}

impl Drop for Open<'_> {
    fn drop(&mut self) {
        let open = self
            .browsers
            .state
            .lock()
            .unwrap()
            .questions
            .remove(&self.request);
        if open.is_some() {
            let _ = self.to.send(Response::CancelFind {
                request: self.request,
            });
        }
    }
}

impl Browsers {
    pub fn new() -> Arc<Browsers> {
        Arc::new(Browsers {
            state: Mutex::default(),
            next: AtomicU64::new(1),
            joined: watch::Sender::new(0),
            turns: Mutex::default(),
        })
    }

    pub fn subscribe(
        self: &Arc<Self>,
        browser: String,
    ) -> (Subscription, mpsc::UnboundedReceiver<Response>) {
        let (to, questions) = mpsc::unbounded_channel();
        let serial = self.next.fetch_add(1, Relaxed);
        self.state.lock().unwrap().ports.push(Port {
            serial,
            browser,
            to,
        });
        self.joined.send_modify(|n| *n += 1);
        let subscription = Subscription {
            browsers: self.clone(),
            serial,
        };
        (subscription, questions)
    }

    /// The newest port of that very browser: no other one is asked.
    fn port(&self, browser: &str) -> Option<(u64, mpsc::UnboundedSender<Response>)> {
        let st = self.state.lock().unwrap();
        st.ports
            .iter()
            .rev()
            .find(|p| p.browser == browser)
            .map(|p| (p.serial, p.to.clone()))
    }

    async fn port_soon(&self, browser: &str) -> Option<(u64, mpsc::UnboundedSender<Response>)> {
        let mut joined = self.joined.subscribe();
        let wait = async {
            loop {
                joined.borrow_and_update();
                if let Some(port) = self.port(browser) {
                    return Some(port);
                }
                joined.changed().await.ok()?;
            }
        };
        tokio::time::timeout(CONNECTING, wait).await.ok().flatten()
    }

    fn turn_for(&self, page: &str) -> Arc<tokio::sync::Mutex<()>> {
        let site = url::Url::parse(page)
            .ok()
            .and_then(|u| u.host_str().map(|h| siler_core::site_of(h).to_string()))
            .unwrap_or_default();
        self.turns.lock().unwrap().entry(site).or_default().clone()
    }

    async fn ask(&self, ask: LinkAsk) -> Result<FoundLink, String> {
        let browser = ask.source.browser.clone().unwrap_or_default();
        let turn = self.turn_for(&ask.page);
        let _turn = turn.lock().await;
        let Some((port, to)) = self.port_soon(&browser).await else {
            return Err("the browser it came from isn't connected".into());
        };
        let request = self.next.fetch_add(1, Relaxed);
        let (answer, answered) = oneshot::channel();
        self.state
            .lock()
            .unwrap()
            .questions
            .insert(request, Question { port, answer });
        let open = Open {
            browsers: self,
            request,
            to,
        };
        let info = info_of(&ask.source);
        open.to
            .send(Response::FindLink(Box::new(LinkTask {
                request,
                page: ask.page,
                url: ask.url,
                filename: ask.filename,
                size: ask.size,
                media: ask.media,
                container: info.container,
                locator: info.locator,
                video: info.video,
            })))
            .map_err(|_| "the browser disconnected".to_string())?;
        let item = tokio::time::timeout(ANSWER, answered)
            .await
            .map_err(|_| "the browser didn't answer in time".to_string())?
            .map_err(|_| "the browser disconnected".to_string())??;
        Ok(found_of(&item))
    }
}

impl LinkFinder for Browsers {
    fn find(&self, ask: LinkAsk) -> LinkSearch<'_> {
        Box::pin(self.ask(ask))
    }
}

/// The two have the same shape.
fn info_of(source: &LinkSource) -> LinkInfo {
    serde_json::to_value(source)
        .and_then(serde_json::from_value)
        .unwrap_or_default()
}

/// Where a hand-over's link came from, as the engine keeps it.
pub fn source_of(item: &BrowserItem) -> LinkSource {
    item.link
        .as_ref()
        .and_then(|link| serde_json::to_value(link).ok())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// A hand-over as a fresh link for a download.
pub fn found_of(item: &BrowserItem) -> FoundLink {
    FoundLink {
        url: item.url.clone(),
        meta: Some(meta_of(item)),
        hint: item.media.as_ref().and_then(hint_of),
        source: item.link.is_some().then(|| source_of(item)),
    }
}

// ---------------------------------------------------------------- waiting for a click

/// The downloads waiting for the user's click on a fresh link: one slot each.
#[derive(Default)]
pub struct Awaiting(Mutex<HashMap<i64, Instant>>);

impl Awaiting {
    pub fn wait(&self, id: i64, until: Instant) {
        self.0.lock().unwrap().insert(id, until);
    }

    pub fn stop(&self, id: i64) {
        self.0.lock().unwrap().remove(&id);
    }

    /// The one download `item` is a fresh link for, which then waits no longer.
    pub fn take(
        &self,
        item: &BrowserItem,
        get: impl Fn(i64) -> Option<DownloadInfo>,
    ) -> Option<i64> {
        let mut waiting = self.0.lock().unwrap();
        let now = Instant::now();
        waiting.retain(|_, until| *until > now);
        let downloads: Vec<DownloadInfo> = waiting.keys().filter_map(|id| get(*id)).collect();
        let candidates: Vec<Waiting<'_>> = downloads.iter().map(Waiting::of).collect();
        let id = which(&candidates, item)?;
        waiting.remove(&id);
        Some(id)
    }
}

/// What a waiting download is recognised by.
struct Waiting<'a> {
    id: i64,
    url: &'a str,
    page: Option<&'a str>,
    name: Option<&'a str>,
    size: Option<u64>,
}

impl<'a> Waiting<'a> {
    fn of(d: &'a DownloadInfo) -> Waiting<'a> {
        Waiting {
            id: d.id,
            url: &d.url,
            page: d.page_url.as_deref().or(d.referrer.as_deref()),
            name: d.filename.as_deref(),
            size: d.total_size,
        }
    }

    fn likeness(&self, item: &BrowserItem) -> u32 {
        let bare = |u: &str| u.split(['#', '?']).next().unwrap_or_default().to_string();
        let page = item.page_url.as_deref().or(item.referrer.as_deref());
        let mut n = 0;
        if bare(self.url) == bare(&item.url) {
            n += 4;
        }
        if self.name.is_some() && self.name == item.filename.as_deref() {
            n += 2;
        }
        if let (Some(a), Some(b)) = (self.page, page)
            && a.split('#').next() == b.split('#').next()
        {
            n += 1;
        }
        n
    }
}

/// The one it is: a file of another size isn't, and between several the likest is, if one
/// stands out (the engine compares again before it changes anything).
fn which(waiting: &[Waiting<'_>], item: &BrowserItem) -> Option<i64> {
    let fits: Vec<&Waiting<'_>> = waiting
        .iter()
        .filter(|w| match (w.size, item.size) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        })
        .collect();
    if let [only] = fits.as_slice() {
        return Some(only.id);
    }
    let mut ranked: Vec<(u32, i64)> = fits.iter().map(|w| (w.likeness(item), w.id)).collect();
    ranked.sort_unstable_by(|a, b| b.cmp(a));
    match ranked.as_slice() {
        [(best, id), (second, _), ..] if best > second => Some(*id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(browser: &str, page: &str) -> LinkAsk {
        LinkAsk {
            id: 1,
            page: page.into(),
            url: "https://cdn.example/f.zip?token=old".into(),
            filename: Some("f.zip".into()),
            size: Some(10),
            media: false,
            source: LinkSource {
                browser: Some(browser.into()),
                container: Some("firefox-container-2".into()),
                locator: Some(siler_core::Locator {
                    text: Some("Download".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        }
    }

    fn task(message: Option<Response>) -> LinkTask {
        match message {
            Some(Response::FindLink(task)) => *task,
            other => panic!("expected a question, got {other:?}"),
        }
    }

    fn fresh(url: &str) -> BrowserItem {
        BrowserItem {
            url: url.into(),
            cookies: Some("a=b".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn only_the_same_browser_is_asked() {
        let b = Browsers::new();
        let (_other, mut to_other) = b.subscribe("other".into());
        let (mine, mut to_mine) = b.subscribe("mine".into());
        let asking = tokio::spawn({
            let b = b.clone();
            async move { b.ask(ask("mine", "https://site.example/p")).await }
        });
        let t = task(to_mine.recv().await);
        assert_eq!(t.page, "https://site.example/p");
        assert_eq!(t.container.as_deref(), Some("firefox-container-2"));
        assert_eq!(t.locator.unwrap().text.as_deref(), Some("Download"));
        mine.answer(t.request, Ok(fresh("https://cdn.example/f.zip?token=new")));
        let found = asking.await.unwrap().unwrap();
        assert_eq!(found.url, "https://cdn.example/f.zip?token=new");
        assert_eq!(found.meta.unwrap().cookies.as_deref(), Some("a=b"));
        assert!(to_other.try_recv().is_err(), "another browser was asked");
    }

    #[tokio::test]
    async fn one_tab_at_a_time_per_site() {
        let b = Browsers::new();
        let (port, mut to) = b.subscribe("b".into());
        let start = |page: &'static str| {
            let b = b.clone();
            tokio::spawn(async move { b.ask(ask("b", page)).await })
        };
        let one = start("https://www.site.example/1");
        let first = task(to.recv().await);
        let two = start("https://dl.site.example/2");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(to.try_recv().is_err(), "two tabs on one site at once");
        let elsewhere = start("https://elsewhere.test/3");
        let third = task(to.recv().await);
        assert_eq!(third.page, "https://elsewhere.test/3");
        port.answer(first.request, Err("nothing there".into()));
        assert!(one.await.unwrap().is_err());
        let second = task(to.recv().await);
        assert_eq!(second.page, "https://dl.site.example/2");
        port.answer(second.request, Ok(fresh("https://cdn.example/2")));
        assert!(two.await.unwrap().is_ok());
        port.answer(third.request, Ok(fresh("https://cdn.example/3")));
        assert!(elsewhere.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn a_closed_browser_or_a_cancelled_question() {
        let b = Browsers::new();
        let (port, mut to) = b.subscribe("b".into());
        let asking = tokio::spawn({
            let b = b.clone();
            async move { b.ask(ask("b", "https://site.example/p")).await }
        });
        task(to.recv().await);
        drop(port);
        assert!(asking.await.unwrap().is_err());

        let (_port, mut to) = b.subscribe("b".into());
        let asking = tokio::spawn({
            let b = b.clone();
            async move { b.ask(ask("b", "https://site.example/p")).await }
        });
        let t = task(to.recv().await);
        asking.abort();
        assert!(asking.await.is_err());
        match to.recv().await {
            Some(Response::CancelFind { request }) => assert_eq!(request, t.request),
            other => panic!("the tab would stay: {other:?}"),
        }
    }

    #[test]
    fn several_wait_for_a_click() {
        let w = |id, url, name, size| Waiting {
            id,
            url,
            page: Some("https://site.example/files"),
            name: Some(name),
            size,
        };
        let a = || w(1, "https://cdn.example/a.zip?t=1", "a.zip", Some(10));
        let b = || w(2, "https://cdn.example/b.zip?t=1", "b.zip", Some(10));
        let item = |url: &str, size| BrowserItem {
            url: url.into(),
            size,
            page_url: Some("https://site.example/files".into()),
            ..Default::default()
        };
        let fresh_b = item("https://cdn.example/b.zip?t=2", Some(10));
        assert_eq!(which(&[a(), b()], &fresh_b), Some(2));
        assert_eq!(
            which(&[a()], &item("https://mirror.example/x", None)),
            Some(1)
        );
        assert_eq!(
            which(&[a()], &item("https://cdn.example/a.zip", Some(99))),
            None
        );
        let unclear = item("https://mirror.example/get?id=9", Some(10));
        assert_eq!(which(&[a(), b()], &unclear), None);
        assert_eq!(which(&[], &fresh_b), None);
    }
}
