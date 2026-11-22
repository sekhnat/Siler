//! Expired links: renewed from where they came from or by the browser, only ever for the
//! same file, and never for ever.

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use siler_core::{
    DownloadInfo, Engine, Event, FirstLink, FoundLink, LinkAsk, LinkFinder, LinkSearch, LinkSource,
    NewDownload, RequestMeta, Settings, Status,
};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SIZE: usize = 3 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;

// ---------------------------------------------------------------- links that expire

struct Site {
    data: Mutex<Arc<Vec<u8>>>,
    /// Only links of this generation work.
    generation: AtomicUsize,
    served: AtomicUsize,
    /// The links expire once, when this much has been served.
    expire_after: AtomicUsize,
    /// Expired links answer 401 without saying how to log in.
    plain_401: AtomicBool,
    /// The cookies `/go` was asked with.
    go_cookies: Mutex<Vec<String>>,
    addr: SocketAddr,
}

fn pattern(seed: usize) -> Vec<u8> {
    (0..SIZE).map(|i| ((i * 7 + seed) % 251) as u8).collect()
}

fn empty(status: StatusCode) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = status;
    resp
}

async fn go(
    State(site): State<Arc<Site>>,
    AxPath(name): AxPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(c) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        site.go_cookies.lock().unwrap().push(c.to_string());
    }
    Response::builder()
        .status(StatusCode::FOUND)
        .header(
            header::LOCATION,
            format!("/s/{}/{name}", site.generation.load(SeqCst)),
        )
        .body(Body::empty())
        .unwrap()
}

async fn signed(
    State(site): State<Arc<Site>>,
    AxPath((generation, _name)): AxPath<(usize, String)>,
    headers: HeaderMap,
) -> Response {
    if generation != site.generation.load(SeqCst) {
        return empty(if site.plain_401.load(SeqCst) {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::FORBIDDEN
        });
    }
    let data = site.data.lock().unwrap().clone();
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse().unwrap_or(SIZE - 1))));
    let (status, from, to) = match range {
        Some((a, b)) => (
            StatusCode::PARTIAL_CONTENT,
            a,
            std::cmp::min(b, SIZE - 1) + 1,
        ),
        None => (StatusCode::OK, 0, SIZE),
    };
    let served = site.clone();
    let body = futures_util::stream::unfold(from, move |at| {
        let (site, data) = (served.clone(), data.clone());
        async move {
            if at >= to {
                return None;
            }
            // An expired link's connections are cut.
            if generation != site.generation.load(SeqCst) {
                return Some((Err(std::io::Error::other("expired")), to));
            }
            let end = (at + CHUNK).min(to);
            let total = site.served.fetch_add(end - at, SeqCst) + (end - at);
            if total >= site.expire_after.load(SeqCst) {
                site.expire_after.store(usize::MAX, SeqCst);
                site.generation.fetch_add(1, SeqCst);
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
            Some((Ok(Bytes::copy_from_slice(&data[at..end])), end))
        }
    });
    let mut resp = Response::builder()
        .status(status)
        .header(header::CONTENT_LENGTH, to - from)
        .header(header::ETAG, format!("\"edge-{generation}\""));
    if status == StatusCode::PARTIAL_CONTENT {
        resp = resp.header(
            header::CONTENT_RANGE,
            format!("bytes {from}-{}/{SIZE}", to - 1),
        );
    }
    resp.body(Body::from_stream(body)).unwrap()
}

impl Site {
    async fn start() -> Arc<Site> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let site = Arc::new(Site {
            data: Mutex::new(Arc::new(pattern(0))),
            generation: AtomicUsize::new(0),
            served: AtomicUsize::new(0),
            expire_after: AtomicUsize::new(usize::MAX),
            plain_401: AtomicBool::new(false),
            go_cookies: Mutex::new(Vec::new()),
            addr: listener.local_addr().unwrap(),
        });
        let app = Router::new()
            .route("/go/{name}", get(go))
            .route("/s/{generation}/{name}", get(signed))
            .with_state(site.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        site
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn fresh(&self, name: &str) -> String {
        self.url(&format!("/s/{}/{name}", self.generation.load(SeqCst)))
    }
}

// ---------------------------------------------------------------- a browser

type Answer = Box<dyn Fn(&LinkAsk) -> Result<FoundLink, String> + Send + Sync>;

struct Browser {
    asked: Mutex<Vec<LinkAsk>>,
    answer: Answer,
    /// Answers wait for this.
    open: AtomicBool,
}

impl Browser {
    fn new(
        answer: impl Fn(&LinkAsk) -> Result<FoundLink, String> + Send + Sync + 'static,
    ) -> Arc<Browser> {
        Arc::new(Browser {
            asked: Mutex::new(Vec::new()),
            answer: Box::new(answer),
            open: AtomicBool::new(true),
        })
    }

    fn fresh_links(site: &Arc<Site>) -> Arc<Browser> {
        let site = site.clone();
        Browser::new(move |ask| {
            Ok(FoundLink {
                url: site.fresh(ask.filename.as_deref().unwrap_or("a.bin")),
                ..Default::default()
            })
        })
    }

    fn times(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

impl LinkFinder for Browser {
    fn find(&self, ask: LinkAsk) -> LinkSearch<'_> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(ask.clone());
            while !self.open.load(SeqCst) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            (self.answer)(&ask)
        })
    }
}

// ---------------------------------------------------------------- helpers

async fn engine(dir: &Path, tweak: impl FnOnce(&mut Settings)) -> Engine {
    let e = Engine::open(dir.join("data")).unwrap();
    let mut s = e.settings();
    s.download_dir = dir.join("dl");
    s.use_category_folders = false;
    s.min_split_size = 64 * 1024;
    s.connections = 4;
    s.max_concurrent = 6;
    s.max_retries = 3;
    s.proxy.mode = siler_core::proxy::ProxyMode::None;
    tweak(&mut s);
    e.set_settings(s).unwrap();
    e
}

fn from_browser() -> LinkSource {
    LinkSource {
        browser: Some("browser-1".into()),
        ..Default::default()
    }
}

fn add(e: &Engine, url: String, link: LinkSource) -> i64 {
    e.add(NewDownload {
        url,
        link,
        meta: RequestMeta {
            page_url: Some("http://page.invalid/downloads".into()),
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap()
}

async fn until(e: &Engine, id: i64, what: impl Fn(&DownloadInfo) -> bool) -> DownloadInfo {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let d = e.get(id).expect("the download");
        if what(&d) {
            return d;
        }
        assert!(Instant::now() < deadline, "timed out: {d:?}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn finished(e: &Engine, id: i64) -> DownloadInfo {
    until(e, id, |d| {
        matches!(d.status, Status::Completed | Status::Failed)
    })
    .await
}

#[track_caller]
fn complete(d: &DownloadInfo, data: &[u8]) {
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let path = Path::new(d.dir.as_ref().unwrap()).join(d.filename.as_ref().unwrap());
    assert!(std::fs::read(path).unwrap() == data, "the file differs");
}

/// Downloads a part, stops, and lets the link expire.
async fn part_then_expired(e: &Engine, site: &Site, id: i64) -> u64 {
    until(e, id, |d| d.downloaded > 4 * CHUNK as u64).await;
    e.pause(id).unwrap();
    let d = until(e, id, |d| d.status == Status::Paused).await;
    assert!(d.downloaded > 0 && d.downloaded < SIZE as u64);
    site.generation.fetch_add(1, SeqCst);
    d.downloaded
}

// ---------------------------------------------------------------- silent

#[tokio::test]
async fn renews_in_the_middle_from_its_own_redirect() {
    let site = Site::start().await;
    site.expire_after.store(SIZE / 3, SeqCst);
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let mut events = e.subscribe();
    let id = add(&e, site.url("/go/a.bin"), LinkSource::default());
    complete(&finished(&e, id).await, &pattern(0));
    while let Ok(ev) = events.try_recv() {
        assert!(!matches!(ev, Event::Failed { .. }), "it failed on the way");
    }
    // Another edge, another ETag: the finished part was kept all the same.
    assert!(
        site.served.load(SeqCst) < SIZE + SIZE / 3,
        "it started over"
    );
}

#[tokio::test]
async fn renews_from_the_start_of_the_browsers_redirects() {
    let site = Site::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let link = LinkSource {
        first: Some(FirstLink {
            url: site.url("/go/a.bin"),
            cookies: Some("go=secret".into()),
        }),
        ..Default::default()
    };
    let id = add(&e, site.fresh("a.bin"), link);
    part_then_expired(&e, &site, id).await;
    e.resume(id).unwrap();
    let d = finished(&e, id).await;
    complete(&d, &pattern(0));
    assert!(
        site.go_cookies
            .lock()
            .unwrap()
            .contains(&"go=secret".to_string())
    );
    assert!(d.url.contains("/s/1/"), "{}", d.url);

    // Never exported, never shown.
    for format in [
        siler_core::lists::ListFormat::Text,
        siler_core::lists::ListFormat::Json,
    ] {
        let list = e.export_list(&[id], format, true).unwrap();
        assert!(
            !list.contains("/go/") && !list.contains("go=secret"),
            "{list}"
        );
    }
    assert!(!format!("{d:?}").contains("go=secret"));
}

#[tokio::test]
async fn after_a_restart() {
    let site = Site::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let link = LinkSource {
        first: Some(FirstLink {
            url: site.url("/go/a.bin"),
            cookies: None,
        }),
        ..Default::default()
    };
    let id = add(&e, site.fresh("a.bin"), link);
    part_then_expired(&e, &site, id).await;
    e.shutdown().await;
    drop(e);

    let e = engine(tmp.path(), |_| {}).await;
    e.resume(id).unwrap();
    complete(&finished(&e, id).await, &pattern(0));
}

// ---------------------------------------------------------------- the browser

#[tokio::test]
async fn the_browser_finds_the_fresh_link() {
    let site = Site::start().await;
    site.expire_after.store(SIZE / 3, SeqCst);
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let browser = Browser::fresh_links(&site);
    e.set_link_finder(Some(browser.clone()));
    let id = add(&e, site.fresh("a.bin"), from_browser());
    complete(&finished(&e, id).await, &pattern(0));
    let asked = browser.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].page, "http://page.invalid/downloads");
    assert_eq!(asked[0].size, Some(SIZE as u64));
    assert_eq!(asked[0].source.browser.as_deref(), Some("browser-1"));
    assert!(
        site.served.load(SeqCst) < SIZE + SIZE / 3,
        "it started over"
    );
}

#[tokio::test]
async fn a_401_without_a_challenge_is_expiry_too() {
    let site = Site::start().await;
    site.plain_401.store(true, SeqCst);
    site.expire_after.store(SIZE / 3, SeqCst);
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let browser = Browser::fresh_links(&site);
    e.set_link_finder(Some(browser.clone()));
    let id = add(&e, site.fresh("a.bin"), from_browser());
    let d = finished(&e, id).await;
    complete(&d, &pattern(0));
    assert_eq!(browser.times(), 1);
}

#[tokio::test]
async fn several_at_once() {
    let site = Site::start().await;
    site.expire_after.store(SIZE, SeqCst);
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let browser = Browser::fresh_links(&site);
    e.set_link_finder(Some(browser.clone()));
    let ids: Vec<i64> = ["a.bin", "b.bin", "c.bin"]
        .iter()
        .map(|n| add(&e, site.fresh(n), from_browser()))
        .collect();
    for id in ids {
        complete(&finished(&e, id).await, &pattern(0));
    }
    assert_eq!(browser.times(), 3);
}

#[tokio::test]
async fn another_file_of_the_same_size_is_refused() {
    let site = Site::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let browser = Browser::fresh_links(&site);
    e.set_link_finder(Some(browser.clone()));
    let id = add(&e, site.fresh("a.bin"), from_browser());
    let had = part_then_expired(&e, &site, id).await;
    *site.data.lock().unwrap() = Arc::new(pattern(3));
    e.resume(id).unwrap();
    let d = finished(&e, id).await;
    assert_eq!(d.status, Status::Failed);
    assert!(d.needs_link);
    assert_eq!(d.downloaded, had, "the progress was thrown away");

    // Nor does a pasted link get past it.
    let pasted = e.refresh_url(id, &site.fresh("a.bin"), None).await;
    assert!(pasted.is_err(), "a different file was accepted");
}

#[tokio::test]
async fn two_attempts_without_progress() {
    let site = Site::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let dead = site.url("/s/999/a.bin");
    let browser = Browser::new(move |_| {
        Ok(FoundLink {
            url: dead.clone(),
            ..Default::default()
        })
    });
    e.set_link_finder(Some(browser.clone()));
    let id = add(&e, site.fresh("a.bin"), from_browser());
    part_then_expired(&e, &site, id).await;
    for _ in 0..4 {
        e.resume(id).unwrap();
        assert!(finished(&e, id).await.needs_link);
    }
    assert_eq!(browser.times(), 2);
}

#[tokio::test]
async fn no_browser_to_ask() {
    let site = Site::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let browser = Browser::new(|_| Err("the browser isn't running".into()));
    e.set_link_finder(Some(browser.clone()));
    let mut events = e.subscribe();
    let id = add(&e, site.fresh("a.bin"), from_browser());
    part_then_expired(&e, &site, id).await;
    e.resume(id).unwrap();
    assert!(finished(&e, id).await.needs_link);
    let mut told = false;
    while let Ok(ev) = events.try_recv() {
        told |= matches!(
            ev,
            Event::Failed {
                needs_link: true,
                ..
            }
        );
    }
    assert!(told, "the app wasn't told to ask the user");

    // A link of unknown origin (or from a private window) opens no tab.
    let id = add(&e, site.fresh("b.bin"), LinkSource::default());
    part_then_expired(&e, &site, id).await;
    e.resume(id).unwrap();
    assert!(finished(&e, id).await.needs_link);
    assert_eq!(browser.times(), 1);
}

#[tokio::test]
async fn switched_off() {
    let site = Site::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.auto_refresh_links = false).await;
    let browser = Browser::fresh_links(&site);
    e.set_link_finder(Some(browser.clone()));
    let link = LinkSource {
        first: Some(FirstLink {
            url: site.url("/go/a.bin"),
            cookies: None,
        }),
        ..from_browser()
    };
    let id = add(&e, site.fresh("a.bin"), link);
    part_then_expired(&e, &site, id).await;
    e.resume(id).unwrap();
    assert!(finished(&e, id).await.needs_link);
    assert_eq!(browser.times(), 0);
    assert!(site.go_cookies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_link_that_never_worked_isnt_expired() {
    let site = Site::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let browser = Browser::fresh_links(&site);
    e.set_link_finder(Some(browser.clone()));
    let id = add(&e, site.url("/s/999/a.bin"), from_browser());
    let d = finished(&e, id).await;
    assert_eq!((d.status, d.needs_link), (Status::Failed, false));
    assert_eq!(browser.times(), 0);
}

#[tokio::test]
async fn a_pending_refresh_is_busy() {
    let site = Site::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let browser = Browser::fresh_links(&site);
    browser.open.store(false, SeqCst);
    e.set_link_finder(Some(browser.clone()));
    let id = add(&e, site.fresh("a.bin"), from_browser());
    part_then_expired(&e, &site, id).await;
    let mut events = e.subscribe();
    e.resume(id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while browser.times() == 0 {
        assert!(Instant::now() < deadline, "the browser wasn't asked");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    // What "shut down when all are done" looks at.
    assert_eq!(e.get(id).unwrap().status, Status::Queued);
    while let Ok(ev) = events.try_recv() {
        assert!(
            !matches!(ev, Event::QueueDone { .. } | Event::Failed { .. }),
            "{ev:?}"
        );
    }

    // Pausing calls it off.
    e.pause(id).unwrap();
    browser.open.store(true, SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let d = e.get(id).unwrap();
    assert_eq!(d.status, Status::Paused);
    assert!(
        d.url.contains("/s/0/"),
        "a cancelled refresh changed the link"
    );
}

// ---------------------------------------------------------------- streams with tokens in their paths

const PIECES: usize = 8;

/// An HLS stream whose every address has the token in its path.
struct Stream {
    generation: AtomicUsize,
    /// The links expire once, after this many pieces.
    expire_after: AtomicUsize,
    /// What the pieces are made of.
    seed: AtomicUsize,
    served: Mutex<Vec<usize>>,
    addr: SocketAddr,
}

fn piece(seed: usize, i: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for n in 0..64usize {
        // MPEG-TS packets begin with 0x47.
        out.push(0x47);
        out.extend((1..188usize).map(|b| ((b * 3 + n + i * 11 + seed) % 251) as u8));
    }
    out
}

async fn hls_go(State(s): State<Arc<Stream>>) -> Response {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(
            header::LOCATION,
            format!("/t{}/master.m3u8", s.generation.load(SeqCst)),
        )
        .body(Body::empty())
        .unwrap()
}

async fn hls(
    State(s): State<Arc<Stream>>,
    AxPath((token, path)): AxPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let g = s.generation.load(SeqCst);
    if token != format!("t{g}") {
        return empty(StatusCode::FORBIDDEN);
    }
    let body: Vec<u8> = match path.as_str() {
        "master.m3u8" => format!(
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=640x360,CODECS=\"avc1.4d401e,mp4a.40.2\"\n/t{g}/360/list.m3u8\n"
        )
        .into(),
        "360/list.m3u8" => {
            let mut m = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n".to_string();
            for i in 0..PIECES {
                m += &format!("#EXTINF:4,\n/t{g}/360/{i}.ts\n");
            }
            (m + "#EXT-X-ENDLIST\n").into()
        }
        _ => {
            let i = path
                .strip_prefix("360/")
                .and_then(|p| p.strip_suffix(".ts"))
                .and_then(|p| p.parse::<usize>().ok());
            let Some(i) = i else {
                return empty(StatusCode::NOT_FOUND);
            };
            let count = {
                let mut served = s.served.lock().unwrap();
                served.push(i);
                served.len()
            };
            if count >= s.expire_after.load(SeqCst) {
                s.expire_after.store(usize::MAX, SeqCst);
                s.generation.fetch_add(1, SeqCst);
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
            piece(s.seed.load(SeqCst), i)
        }
    };
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse().unwrap_or(usize::MAX))));
    let mime = if path.ends_with(".m3u8") {
        "application/vnd.apple.mpegurl"
    } else {
        "video/mp2t"
    };
    let resp = Response::builder().header(header::CONTENT_TYPE, mime);
    match range.filter(|(a, _)| *a < body.len()) {
        Some((a, b)) => {
            let b = b.min(body.len() - 1);
            resp.status(StatusCode::PARTIAL_CONTENT)
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {a}-{b}/{}", body.len()),
                )
                .body(Body::from(body[a..=b].to_vec()))
        }
        None => resp.body(Body::from(body)),
    }
    .unwrap()
}

impl Stream {
    async fn start() -> Arc<Stream> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let s = Arc::new(Stream {
            generation: AtomicUsize::new(0),
            expire_after: AtomicUsize::new(usize::MAX),
            seed: AtomicUsize::new(0),
            served: Mutex::new(Vec::new()),
            addr: listener.local_addr().unwrap(),
        });
        let app = Router::new()
            .route("/go.m3u8", get(hls_go))
            .route("/{token}/{*path}", get(hls))
            .with_state(s.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        s
    }

    fn master(&self) -> String {
        format!(
            "http://{}/t{}/master.m3u8",
            self.addr,
            self.generation.load(SeqCst)
        )
    }

    fn whole(&self) -> Vec<u8> {
        let seed = self.seed.load(SeqCst);
        (0..PIECES).flat_map(|i| piece(seed, i)).collect()
    }
}

async fn stream_engine(dir: &Path) -> Engine {
    engine(dir, |s| {
        s.allow_private_media_addresses = true;
        s.media_segment_concurrency = 1;
    })
    .await
}

async fn add_stream(e: &Engine, url: String, link: LinkSource) -> i64 {
    e.add_media(siler_core::NewMediaDownload {
        url,
        link,
        filename: Some("stream".into()),
        container: Some(siler_core::ContainerChoice::Original),
        meta: RequestMeta {
            page_url: Some("http://page.invalid/watch".into()),
            ..Default::default()
        },
        ..Default::default()
    })
    .await
    .unwrap()
}

/// Downloads some pieces, stops, and lets every address expire.
async fn pieces_then_expired(e: &Engine, s: &Stream, id: i64) {
    until(e, id, |d| d.downloaded >= 3 * piece(0, 0).len() as u64).await;
    e.pause(id).unwrap();
    until(e, id, |d| d.status == Status::Paused).await;
    s.generation.fetch_add(1, SeqCst);
    s.served.lock().unwrap().clear();
}

#[tokio::test]
async fn a_stream_with_tokens_in_its_paths_goes_on() {
    let s = Stream::start().await;
    s.expire_after.store(4, SeqCst);
    let tmp = tempfile::tempdir().unwrap();
    let e = stream_engine(tmp.path()).await;
    let fresh = s.clone();
    let browser = Browser::new(move |ask| {
        assert!(ask.media);
        Ok(FoundLink {
            url: fresh.master(),
            ..Default::default()
        })
    });
    e.set_link_finder(Some(browser.clone()));
    let id = add_stream(&e, s.master(), from_browser()).await;
    complete(&finished(&e, id).await, &s.whole());
    assert_eq!(browser.times(), 1);
    // All of it, and beside that only the pieces looked at again to compare.
    let served = s.served.lock().unwrap().len();
    assert!(
        served <= PIECES + 4,
        "it started over ({served} pieces served)"
    );
}

#[tokio::test]
async fn a_stream_renews_from_its_first_address() {
    let s = Stream::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = stream_engine(tmp.path()).await;
    let link = LinkSource {
        first: Some(FirstLink {
            url: format!("http://{}/go.m3u8", s.addr),
            cookies: None,
        }),
        ..Default::default()
    };
    let id = add_stream(&e, s.master(), link).await;
    pieces_then_expired(&e, &s, id).await;
    e.resume(id).unwrap();
    complete(&finished(&e, id).await, &s.whole());
    assert!(s.served.lock().unwrap().len() < PIECES, "it started over");
}

#[tokio::test]
async fn another_stream_at_the_new_link_is_refused() {
    let s = Stream::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = stream_engine(tmp.path()).await;
    let fresh = s.clone();
    e.set_link_finder(Some(Browser::new(move |_| {
        Ok(FoundLink {
            url: fresh.master(),
            ..Default::default()
        })
    })));
    let id = add_stream(&e, s.master(), from_browser()).await;
    pieces_then_expired(&e, &s, id).await;
    s.seed.store(5, SeqCst);
    e.resume(id).unwrap();
    let d = finished(&e, id).await;
    assert_eq!(d.status, Status::Failed);
    let changed = siler_core::MediaError::ManifestChanged.to_string();
    assert_eq!(d.error.as_deref(), Some(changed.as_str()));
    assert!(d.downloaded > 0, "the finished pieces were thrown away");
}
