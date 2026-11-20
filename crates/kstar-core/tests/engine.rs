//! End-to-end engine tests against a local HTTP server with configurable misbehaviour.

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use base64::Engine as _;
use futures_util::stream;
use kstar_core::proxy::{ProxyMode, ProxyScheme};
use kstar_core::{
    Category, DownloadInfo, EditDownload, Engine, Error, Event, Login, LoginRequest, NewDownload,
    OnConflict, QueueMove, RequestMeta, ServerLimit, Settings, Status,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const SIZE: usize = 8 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

// ---------------------------------------------------------------- test server

struct Shared {
    data: RwLock<Arc<Vec<u8>>>,
    version: AtomicU32,
    served: AtomicU64,
    active: AtomicUsize,
    requests: AtomicUsize,
    probes: AtomicUsize,
    busy_hits: AtomicUsize,
    posts: AtomicUsize,
    /// Requests that arrived through a proxy (absolute-form request target).
    proxied: AtomicUsize,
    /// `Proxy-Authorization` the "proxy" demands, if any.
    proxy_login: RwLock<Option<String>>,
    /// Digest: current nonce number, and accepted requests (the nonce rotates after 5).
    nonce: AtomicU32,
    digest_ok: AtomicUsize,
    unauthorized: AtomicUsize,
}

const USER: &str = "user";
const PASS: &str = "pa:ss wörd";

fn basic(user: &str, pass: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
    )
}

fn unauthorized(srv: &Shared, challenge: String) -> Response {
    srv.unauthorized.fetch_add(1, SeqCst);
    let mut resp = StatusCode::UNAUTHORIZED.into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_str(&challenge).unwrap(),
    );
    resp
}

/// Every request (probe and segments) must carry a valid Basic login.
async fn serve_basic(
    State(srv): State<Arc<Shared>>,
    Query(q): Query<Opts>,
    headers: HeaderMap,
) -> Response {
    let got = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if got != Some(basic(USER, PASS).as_str()) {
        return unauthorized(&srv, "Basic realm=\"test\"".into());
    }
    serve_file(State(srv), Query(q), headers).await
}

/// Like `serve_basic` with Digest; the nonce changes once mid-download (answered `stale`).
async fn serve_digest(
    State(srv): State<Arc<Shared>>,
    Query(q): Query<Opts>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response {
    let nonce = format!("n{}", srv.nonce.load(SeqCst));
    let challenge = |stale: bool| {
        format!(
            "Digest realm=\"test\", nonce=\"{nonce}\", qop=\"auth\", algorithm=MD5{}",
            if stale { ", stale=true" } else { "" }
        )
    };
    let Some(hdr) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| digest_auth::AuthorizationHeader::parse(v).ok())
    else {
        return unauthorized(&srv, challenge(false));
    };
    let mut check = hdr.clone();
    check.digest(&digest_auth::AuthContext::new(USER, PASS, hdr.uri.as_str()));
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("");
    if check.response != hdr.response || hdr.username != USER || hdr.uri != path {
        return unauthorized(&srv, challenge(false));
    }
    if hdr.nonce != nonce {
        return unauthorized(&srv, challenge(true));
    }
    if srv.digest_ok.fetch_add(1, SeqCst) == 5 {
        srv.nonce.fetch_add(1, SeqCst);
    }
    serve_file(State(srv), Query(q), headers).await
}

/// Basic login, then a redirect to the same server under another name.
async fn basic_then_away(
    State(srv): State<Arc<Shared>>,
    Query(q): Query<Opts>,
    headers: HeaderMap,
) -> Response {
    let got = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if got != Some(basic(USER, PASS).as_str()) {
        return unauthorized(&srv, "Basic realm=\"test\"".into());
    }
    Redirect::temporary(&q.to.unwrap()).into_response()
}

/// Lets the test server act as an HTTP proxy too: requests in absolute form are counted
/// and, when configured, must carry the proxy login.
async fn proxy_layer(State(srv): State<Arc<Shared>>, req: Request, next: Next) -> Response {
    if req.uri().authority().is_some() {
        srv.proxied.fetch_add(1, SeqCst);
        let want = srv.proxy_login.read().unwrap().clone();
        let got = req
            .headers()
            .get(header::PROXY_AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        if want.is_some() && got != want.as_deref() {
            let mut resp = StatusCode::PROXY_AUTHENTICATION_REQUIRED.into_response();
            resp.headers_mut().insert(
                header::PROXY_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"proxy\""),
            );
            return resp;
        }
    }
    next.run(req).await
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Opts {
    /// Per-connection throttle in bytes/s.
    rate: Option<u64>,
    /// Throttle only for requests starting at byte 0 (excluding the probe).
    slowfirst: Option<u64>,
    /// The first N body requests drop the connection after 100 KB.
    flaky: usize,
    /// Answer 503 when more than N connections are open.
    maxconn: Option<usize>,
    norange: bool,
    cd: bool,
    size: Option<usize>,
    status: Option<u16>,
    /// Answer 403 unless the browser context (cookie, referer, user agent) is present.
    needauth: bool,
    /// The first N probes (`Range: bytes=0-0`) get a 503.
    probefail: usize,
    /// The first N body requests get a 503.
    busy: usize,
    /// `Retry-After` seconds sent with those 503s.
    retryafter: Option<u64>,
    /// Only answer the POST form submission (see `serve_post`).
    needpost: bool,
    /// Refuse requests that carry an `Authorization` header.
    noauth: bool,
    /// Redirect target for `/auth/away`.
    to: Option<String>,
}

fn busy_response(retry_after: Option<u64>) -> Response {
    let mut resp = StatusCode::SERVICE_UNAVAILABLE.into_response();
    if let Some(s) = retry_after {
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(s));
    }
    resp
}

/// A form submission: the body and content type must be replayed exactly.
async fn serve_post(
    State(srv): State<Arc<Shared>>,
    Query(mut q): Query<Opts>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    srv.posts.fetch_add(1, SeqCst);
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if body.as_ref() != b"a=1&b=2" || ct != "application/x-www-form-urlencoded" {
        return StatusCode::BAD_REQUEST.into_response();
    }
    q.needpost = false;
    serve_file(State(srv), Query(q), headers).await
}

fn make_data(version: u32) -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15 ^ u64::from(version);
    (0..SIZE)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

struct ActiveGuard(Arc<Shared>);
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, SeqCst);
    }
}

fn parse_range(h: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let v = h
        .get(header::RANGE)?
        .to_str()
        .ok()?
        .strip_prefix("bytes=")?;
    let (a, b) = v.split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()))
}

async fn serve_file(
    State(srv): State<Arc<Shared>>,
    Query(q): Query<Opts>,
    headers: HeaderMap,
) -> Response {
    if let Some(code) = q.status {
        return StatusCode::from_u16(code).unwrap().into_response();
    }
    if q.needpost {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    if q.noauth && headers.contains_key(header::AUTHORIZATION) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if q.needauth {
        let h = |n: header::HeaderName| {
            headers
                .get(n)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string()
        };
        if h(header::COOKIE) != "session=abc; theme=dark"
            || h(header::REFERER) != "https://example.org/page"
            || h(header::USER_AGENT) != "TestBrowser/1.0"
            || h(header::ACCEPT_ENCODING) != "identity"
        {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    let full = srv.data.read().unwrap().clone();
    let len = q.size.unwrap_or(full.len()).min(full.len()) as u64;
    let range = if q.norange {
        None
    } else {
        parse_range(&headers)
    };
    let is_probe = range == Some((0, Some(0)));
    if is_probe && srv.probes.fetch_add(1, SeqCst) < q.probefail {
        return busy_response(q.retryafter);
    }
    if !is_probe && q.busy > 0 && srv.busy_hits.fetch_add(1, SeqCst) < q.busy {
        return busy_response(q.retryafter);
    }

    if let Some(max) = q.maxconn
        && srv.active.load(SeqCst) >= max
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    srv.active.fetch_add(1, SeqCst);
    let guard = ActiveGuard(srv.clone());

    let (status, start, end) = match range {
        Some((s, e)) => (
            StatusCode::PARTIAL_CONTENT,
            s,
            e.unwrap_or(len - 1).min(len - 1),
        ),
        None => (StatusCode::OK, 0, len - 1),
    };
    let rate = if start == 0 && !is_probe {
        q.slowfirst.or(q.rate)
    } else {
        q.rate
    };
    let drop_at = if !is_probe && srv.requests.fetch_add(1, SeqCst) < q.flaky {
        Some(start + 100_000)
    } else {
        None
    };

    let body_stream = stream::unfold((start, guard), move |(pos, guard)| {
        let full = full.clone();
        let srv = guard.0.clone();
        async move {
            if pos > end {
                return None;
            }
            if drop_at.is_some_and(|d| pos >= d) {
                return Some((
                    Err(std::io::Error::other("simulated drop")),
                    (end + 1, guard),
                ));
            }
            let n = 16_384u64.min(end + 1 - pos);
            if let Some(r) = rate {
                tokio::time::sleep(Duration::from_secs_f64(n as f64 / r as f64)).await;
            }
            srv.served.fetch_add(n, SeqCst);
            let chunk = Bytes::copy_from_slice(&full[pos as usize..(pos + n) as usize]);
            Some((Ok(chunk), (pos + n, guard)))
        }
    });

    let mut resp = Response::new(Body::from_stream(body_stream));
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(header::CONTENT_LENGTH, (end + 1 - start).into());
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    let etag = format!("\"v{}\"", srv.version.load(SeqCst));
    h.insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
    if !q.norange {
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    }
    if status == StatusCode::PARTIAL_CONTENT {
        let cr = format!("bytes {start}-{end}/{len}");
        h.insert(header::CONTENT_RANGE, HeaderValue::from_str(&cr).unwrap());
    }
    if q.cd {
        h.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment; filename=\"x.bin\"; filename*=UTF-8''na%C3%AFve%20r%C3%A9sum%C3%A9.bin"),
        );
    }
    resp
}

struct Server {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

impl Server {
    async fn start() -> Server {
        let shared = Arc::new(Shared {
            data: RwLock::new(Arc::new(make_data(1))),
            version: AtomicU32::new(1),
            served: AtomicU64::new(0),
            active: AtomicUsize::new(0),
            requests: AtomicUsize::new(0),
            probes: AtomicUsize::new(0),
            busy_hits: AtomicUsize::new(0),
            posts: AtomicUsize::new(0),
            proxied: AtomicUsize::new(0),
            proxy_login: RwLock::new(None),
            nonce: AtomicU32::new(1),
            digest_ok: AtomicUsize::new(0),
            unauthorized: AtomicUsize::new(0),
        });
        let app = Router::new()
            .route("/file/{name}", get(serve_file).post(serve_post))
            .route(
                "/redirect",
                get(|| async { Redirect::temporary("/redirect2") }),
            )
            .route(
                "/redirect2",
                get(|| async { Redirect::temporary("/file/target.bin") }),
            )
            .route("/auth/basic/{name}", get(serve_basic))
            .route("/auth/digest/{name}", get(serve_digest))
            .route("/auth/away", get(basic_then_away))
            .layer(middleware::from_fn_with_state(shared.clone(), proxy_layer))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Server { addr, shared }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    fn data(&self, len: usize) -> Vec<u8> {
        self.shared.data.read().unwrap()[..len].to_vec()
    }

    fn bump_version(&self) {
        let v = self.shared.version.fetch_add(1, SeqCst) + 1;
        *self.shared.data.write().unwrap() = Arc::new(make_data(v));
    }
}

// ---------------------------------------------------------------- helpers

async fn engine(dir: &Path, tweak: impl FnOnce(&mut Settings)) -> Engine {
    let e = Engine::open(dir.join("data")).unwrap();
    let mut s = e.settings();
    s.download_dir = dir.join("dl");
    s.use_category_folders = false;
    s.min_split_size = 64 * 1024;
    s.connections = 8;
    tweak(&mut s);
    e.set_settings(s).unwrap();
    e
}

fn add(e: &Engine, url: String) -> i64 {
    e.add(NewDownload {
        url,
        ..Default::default()
    })
    .unwrap()
}

async fn wait_for(e: &Engine, id: i64, pred: impl Fn(&DownloadInfo) -> bool) -> DownloadInfo {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let d = e.get(id).expect("download exists");
        if pred(&d) {
            return d;
        }
        assert!(Instant::now() < deadline, "timed out; last state: {d:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_finished(e: &Engine, id: i64) -> DownloadInfo {
    wait_for(e, id, |d| {
        matches!(d.status, Status::Completed | Status::Failed)
    })
    .await
}

fn path_of(d: &DownloadInfo) -> PathBuf {
    d.dir.as_ref().unwrap().join(d.filename.as_ref().unwrap())
}

fn sha(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

fn assert_complete(d: &DownloadInfo, expected: &[u8]) {
    assert_eq!(
        d.status,
        Status::Completed,
        "download failed: {:?}",
        d.error
    );
    let got = std::fs::read(path_of(d)).unwrap();
    assert_eq!(got.len(), expected.len());
    assert!(sha(&got) == sha(expected), "content mismatch");
    let part =
        kstar_core::filename::part_path(d.dir.as_ref().unwrap(), d.filename.as_ref().unwrap());
    assert!(!part.exists(), "part file left behind");
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn probe_reads_metadata() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let p = e
        .probe(&srv.url("/file/a.bin?cd=true"), &RequestMeta::default())
        .await
        .unwrap();
    assert_eq!(p.filename, "naïve résumé.bin");
    assert_eq!(p.total_size, Some(SIZE as u64));
    assert!(p.resumable);
    assert_eq!(p.etag.as_deref(), Some("\"v1\""));
    assert_eq!(p.dir, tmp.path().join("dl"));
}

#[tokio::test]
async fn segmented_download_matches_source() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(&e, srv.url("/file/a.bin"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert_eq!(d.filename.as_deref(), Some("a.bin"));
    assert_eq!(d.resumable, Some(true));
}

#[tokio::test]
async fn slow_connection_gets_its_work_split() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 4).await;
    // The connection serving byte 0 crawls at 256 KiB/s: its 2 MiB segment alone would take 8 s.
    let started = Instant::now();
    let id = add(&e, srv.url("/file/a.bin?slowfirst=262144"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn server_without_ranges() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(&e, srv.url("/file/a.bin?norange=true"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert_eq!(d.resumable, Some(false));
}

#[tokio::test]
async fn follows_redirects() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(&e, srv.url("/redirect"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert_eq!(d.filename.as_deref(), Some("target.bin"));
    assert!(d.final_url.unwrap().ends_with("/file/target.bin"));
}

#[tokio::test]
async fn pause_and_resume() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 4).await;
    let id = add(&e, srv.url("/file/a.bin?rate=1048576"));
    wait_for(&e, id, |d| d.downloaded > MIB).await;
    e.pause(id).unwrap();
    let paused = wait_for(&e, id, |d| d.status == Status::Paused).await;
    assert!(paused.downloaded > 0 && paused.downloaded < SIZE as u64);
    assert!(!paused.segments.is_empty());

    // Nothing more is written while paused.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(e.get(id).unwrap().downloaded, paused.downloaded);

    e.resume(id).unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert!(d.notice.is_none(), "unexpected restart: {:?}", d.notice);
}

#[tokio::test]
async fn resumes_after_restart() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let id = {
        let e = engine(tmp.path(), |s| s.connections = 4).await;
        let id = add(&e, srv.url("/file/a.bin?rate=1048576"));
        wait_for(&e, id, |d| d.downloaded > 2 * MIB).await;
        e.shutdown().await;
        id
    };
    let served_before = srv.shared.served.load(SeqCst);

    let e = Engine::open(tmp.path().join("data")).unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    let served_after = srv.shared.served.load(SeqCst) - served_before;
    assert!(
        served_after < SIZE as u64 - MIB,
        "re-downloaded too much: {served_after}"
    );
}

#[tokio::test]
async fn restarts_when_file_changes_on_server() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 4).await;
    let id = add(&e, srv.url("/file/a.bin?rate=2097152"));
    wait_for(&e, id, |d| d.downloaded > MIB).await;
    e.pause(id).unwrap();
    wait_for(&e, id, |d| d.status == Status::Paused).await;

    srv.bump_version();
    e.resume(id).unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert!(d.notice.is_some());
}

#[tokio::test]
async fn recovers_from_dropped_connections() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 4).await;
    let id = add(&e, srv.url("/file/a.bin?flaky=3"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
}

#[tokio::test]
async fn adapts_to_connection_limit() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(&e, srv.url("/file/a.bin?maxconn=2&rate=8388608"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
}

#[tokio::test]
async fn global_speed_limit() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.global_speed_limit = 2 * MIB).await;
    let size = 3 * MIB as usize;
    let started = Instant::now();
    let id = add(&e, srv.url(&format!("/file/a.bin?size={size}")));
    let d = wait_finished(&e, id).await;
    let elapsed = started.elapsed().as_secs_f64();
    assert_complete(&d, &srv.data(size));
    assert!(
        (1.3..2.0).contains(&elapsed),
        "3 MiB at 2 MiB/s took {elapsed:.2}s"
    );
}

#[tokio::test]
async fn queue_respects_max_concurrent() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.max_concurrent = 1).await;
    let url = srv.url("/file/q.bin?size=1048576&rate=4194304");
    let ids: Vec<i64> = (0..3).map(|_| add(&e, url.clone())).collect();
    let running = e.list().iter().filter(|d| d.status.is_running()).count();
    assert_eq!(running, 1);
    let mut names = Vec::new();
    for id in ids {
        let d = wait_finished(&e, id).await;
        assert_complete(&d, &srv.data(1048576));
        names.push(d.filename.unwrap());
    }
    names.sort();
    assert_eq!(names, ["q (1).bin", "q (2).bin", "q.bin"]);
}

#[tokio::test]
async fn concurrent_downloads_get_unique_names() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let a = add(&e, srv.url("/file/same.bin?rate=4194304&size=2097152"));
    let b = add(&e, srv.url("/file/same.bin?rate=4194304&size=2097152"));
    let (da, db) = (wait_finished(&e, a).await, wait_finished(&e, b).await);
    assert_complete(&da, &srv.data(2097152));
    assert_complete(&db, &srv.data(2097152));
    assert_ne!(da.filename, db.filename);
}

#[tokio::test]
async fn remove_deletes_partial_file() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(&e, srv.url("/file/r.bin?rate=1048576"));
    let d = wait_for(&e, id, |d| d.downloaded > 0).await;
    let part =
        kstar_core::filename::part_path(d.dir.as_ref().unwrap(), d.filename.as_ref().unwrap());
    assert!(part.exists());
    e.remove(id, false).unwrap();
    assert!(e.get(id).is_none());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!part.exists());
}

#[tokio::test]
async fn forwards_browser_context() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let meta = RequestMeta {
        referrer: Some("https://example.org/page".into()),
        cookies: Some("session=abc; theme=dark".into()),
        user_agent: Some("TestBrowser/1.0".into()),
        headers: vec![
            ("X-Extra".into(), "1".into()),
            ("Range".into(), "bytes=5-".into()),
        ],
        ..Default::default()
    };
    let id = e
        .add(NewDownload {
            url: srv.url("/file/auth.bin?needauth=true&size=2097152"),
            meta,
            ..Default::default()
        })
        .unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(2097152));

    // Without the context the same URL is refused.
    let id = add(&e, srv.url("/file/auth.bin?needauth=true&size=2097152"));
    let d = wait_finished(&e, id).await;
    assert_eq!(d.status, Status::Failed);
    assert!(d.error.unwrap().contains("403"));
}

#[tokio::test]
async fn http_errors_fail_with_reason() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(&e, srv.url("/file/missing.bin?status=404"));
    let d = wait_finished(&e, id).await;
    assert_eq!(d.status, Status::Failed);
    assert!(d.error.unwrap().contains("404"));
}

#[tokio::test]
async fn rejects_unsupported_urls() {
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    assert!(
        e.add(NewDownload {
            url: "ftp://example.com/x".into(),
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        e.add(NewDownload {
            url: "not a url".into(),
            ..Default::default()
        })
        .is_err()
    );
}

// ---------------------------------------------------------------- M1 completion

fn parts_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.to_string_lossy().ends_with(".kstar.part"))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn probe_retries_passing_failures() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let started = Instant::now();
    let id = add(
        &e,
        srv.url("/file/a.bin?probefail=2&retryafter=1&size=1048576"),
    );
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(1048576));
    // Two refusals, each asking for a second's pause.
    assert!(
        started.elapsed() >= Duration::from_millis(1900),
        "{:?}",
        started.elapsed()
    );
    assert!(
        d.notice.is_none(),
        "retry notice left behind: {:?}",
        d.notice
    );
}

#[tokio::test]
async fn gives_up_probing_after_max_retries() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.max_retries = 1).await;
    let id = add(&e, srv.url("/file/a.bin?probefail=5"));
    let d = wait_finished(&e, id).await;
    assert_eq!(d.status, Status::Failed);
    assert!(d.error.unwrap().contains("503"));
}

#[tokio::test]
async fn transfer_honours_retry_after() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 1).await;
    let started = Instant::now();
    let id = add(&e, srv.url("/file/a.bin?busy=1&retryafter=2&size=1048576"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(1048576));
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn refresh_link_keeps_progress() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 4).await;
    let id = add(&e, srv.url("/file/a.bin?rate=1048576"));
    wait_for(&e, id, |d| d.downloaded > 2 * MIB).await;
    e.pause(id).unwrap();
    let paused = wait_for(&e, id, |d| d.status == Status::Paused).await;

    // A link to a file of another size is refused and changes nothing.
    let err = e
        .refresh_url(id, &srv.url("/file/other.bin?size=1000"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::LinkMismatch(_)), "{err}");
    assert_eq!(e.get(id).unwrap().status, Status::Paused);

    let served_before = srv.shared.served.load(SeqCst);
    e.refresh_url(id, &srv.url("/file/mirror.bin"), None)
        .await
        .unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert_eq!(d.filename.as_deref(), Some("a.bin"));
    assert!(d.url.ends_with("/file/mirror.bin"));
    let served = srv.shared.served.load(SeqCst) - served_before;
    assert!(
        served <= SIZE as u64 - paused.downloaded + MIB,
        "re-downloaded too much: {served}"
    );
}

#[tokio::test]
async fn finds_resumable_download_by_name_and_size() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(&e, srv.url("/file/big.bin?rate=1048576"));
    wait_for(&e, id, |d| d.downloaded > 0).await;
    assert!(
        e.find_resumable("big.bin", Some(SIZE as u64)).is_none(),
        "running ones don't count"
    );
    e.pause(id).unwrap();
    wait_for(&e, id, |d| d.status == Status::Paused).await;
    assert_eq!(
        e.find_resumable("big.bin", Some(SIZE as u64)).unwrap().id,
        id
    );
    assert!(e.find_resumable("big.bin", Some(5)).is_none());
    assert!(e.find_resumable("other.bin", Some(SIZE as u64)).is_none());
}

#[tokio::test]
async fn restart_starts_over() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 4).await;
    let id = add(&e, srv.url("/file/a.bin?rate=4194304"));
    wait_for(&e, id, |d| d.downloaded > MIB).await;
    let served_before = srv.shared.served.load(SeqCst);
    e.restart(id).await.unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert!(srv.shared.served.load(SeqCst) - served_before >= SIZE as u64);
}

#[tokio::test]
async fn redownload_renames_or_replaces() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(&e, srv.url("/file/r.bin?size=1048576"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(1048576));
    let original = path_of(&d);

    std::fs::write(&original, b"tampered").unwrap();
    e.redownload(id, true).await.unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(1048576));
    assert_eq!(path_of(&d), original, "replaced in place");

    e.redownload(id, false).await.unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(1048576));
    assert_eq!(d.filename.as_deref(), Some("r (1).bin"));
    assert!(original.exists());
}

#[tokio::test]
async fn connection_count_changes_while_running() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 1).await;
    let id = add(&e, srv.url("/file/a.bin?rate=524288"));
    wait_for(&e, id, |d| d.downloaded > 64 * 1024).await;
    assert_eq!(srv.shared.active.load(SeqCst), 1);

    e.set_connections(id, 6).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while srv.shared.active.load(SeqCst) < 6 {
        assert!(Instant::now() < deadline, "never reached 6 connections");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(e.get(id).unwrap().connections, 6);

    e.set_connections(id, 2).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while srv.shared.active.load(SeqCst) > 2 {
        assert!(Instant::now() < deadline, "never dropped to 2 connections");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Finish quickly.
    e.set_connections(id, 16).unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
}

#[tokio::test]
async fn default_connections_follow_settings() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 2).await;
    let id = e
        .add(NewDownload {
            url: srv.url("/file/a.bin"),
            start_paused: true,
            ..Default::default()
        })
        .unwrap();
    let custom = e
        .add(NewDownload {
            url: srv.url("/file/b.bin"),
            start_paused: true,
            connections: Some(5),
            ..Default::default()
        })
        .unwrap();
    let mut s = e.settings();
    s.connections = 12;
    e.set_settings(s).unwrap();
    assert_eq!(e.get(id).unwrap().connections, 12);
    assert_eq!(e.get(custom).unwrap().connections, 5);
}

async fn add_with_conflict(e: &Engine, url: String, p: Option<OnConflict>) -> DownloadInfo {
    let id = e
        .add(NewDownload {
            url,
            on_conflict: p,
            ..Default::default()
        })
        .unwrap();
    wait_finished(e, id).await
}

#[tokio::test]
async fn existing_files_follow_the_conflict_policy() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let dl = tmp.path().join("dl");
    std::fs::create_dir_all(&dl).unwrap();
    let existing = dl.join("c.bin");
    std::fs::write(&existing, b"old").unwrap();
    let url = srv.url("/file/c.bin?size=1048576");

    let (exists, suggested) = e.name_status(&dl, "c.bin");
    assert!(exists);
    assert_eq!(suggested, "c (1).bin");

    let d = add_with_conflict(&e, url.clone(), Some(OnConflict::Skip)).await;
    assert_eq!(d.status, Status::Failed);
    assert!(d.error.unwrap().contains("already exists"));
    assert_eq!(std::fs::read(&existing).unwrap(), b"old");
    assert!(parts_in(&dl).is_empty());

    let d = add_with_conflict(&e, url.clone(), Some(OnConflict::Rename)).await;
    assert_complete(&d, &srv.data(1048576));
    assert_eq!(d.filename.as_deref(), Some("c (1).bin"));

    // "Ask" can't ask without a dialog, so it renames.
    let d = add_with_conflict(&e, url.clone(), None).await;
    assert_eq!(d.filename.as_deref(), Some("c (2).bin"));

    let d = add_with_conflict(&e, url, Some(OnConflict::Overwrite)).await;
    assert_complete(&d, &srv.data(1048576));
    assert_eq!(d.filename.as_deref(), Some("c.bin"));
}

#[tokio::test]
async fn removing_while_starting_leaves_no_part_file() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.max_concurrent = 20).await;
    for i in 0..50u64 {
        let id = add(&e, srv.url(&format!("/file/race{i}.bin?rate=1048576")));
        tokio::time::sleep(Duration::from_millis(i % 7)).await;
        e.remove(id, false).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let left = parts_in(&tmp.path().join("dl"));
    assert!(left.is_empty(), "orphaned part files: {left:?}");
    assert!(e.list().is_empty());
}

#[tokio::test]
async fn queue_order_and_start_now() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.max_concurrent = 1).await;
    let url = |n: &str| srv.url(&format!("/file/{n}.bin?size=262144&rate=1048576"));
    let a = add(&e, url("a"));
    let b = add(&e, url("b"));
    let c = add(&e, url("c"));
    e.move_in_queue(&[c], QueueMove::Top).unwrap();
    assert!(e.get(c).unwrap().position < e.get(b).unwrap().position);
    let (_, db, dc) = (
        wait_finished(&e, a).await,
        wait_finished(&e, b).await,
        wait_finished(&e, c).await,
    );
    assert!(dc.completed_at < db.completed_at, "c should run before b");

    e.move_in_queue(&[b], QueueMove::Up).unwrap(); // completed ones don't move
    let x = add(&e, url("x"));
    let y = add(&e, url("y"));
    wait_for(&e, x, |d| d.status.is_running()).await;
    e.start_now(y).unwrap();
    wait_for(&e, y, |d| d.status.is_running()).await;
    assert!(
        e.get(x).unwrap().status.is_running(),
        "both run past the limit"
    );
}

#[tokio::test]
async fn download_later_waits_for_the_queue() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = e
        .add(NewDownload {
            url: srv.url("/file/later.bin?size=1048576"),
            start_paused: true,
            ..Default::default()
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(e.get(id).unwrap().status, Status::Waiting);
    e.resume_all().unwrap();
    assert_eq!(
        e.get(id).unwrap().status,
        Status::Waiting,
        "resume all leaves the queue alone"
    );
    e.start_queue().unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(1048576));
}

#[tokio::test]
async fn replays_form_submissions() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 4).await;
    let meta = RequestMeta {
        method: Some("POST".into()),
        body: Some(b"a=1&b=2".to_vec()),
        headers: vec![(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )],
        ..Default::default()
    };
    let id = e
        .add(NewDownload {
            url: srv.url("/file/form.bin?needpost=true&size=2097152"),
            meta,
            ..Default::default()
        })
        .unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(2097152));
    assert_eq!(d.method.as_deref(), Some("POST"));
    assert!(
        srv.shared.posts.load(SeqCst) >= 2,
        "probe and segments all POST"
    );
}

#[tokio::test]
async fn detects_missing_files() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(&e, srv.url("/file/m.bin?size=65536"));
    let d = wait_finished(&e, id).await;
    assert!(!d.file_missing);
    std::fs::remove_file(path_of(&d)).unwrap();
    assert!(e.get(id).unwrap().file_missing);
}

#[tokio::test]
async fn edit_renames_and_moves_files() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 4).await;
    let elsewhere = tmp.path().join("elsewhere");

    // Finished: the file itself moves.
    let id = add(&e, srv.url("/file/done.bin?size=65536"));
    let d = wait_finished(&e, id).await;
    let edit = EditDownload {
        filename: Some("renamed.bin".into()),
        dir: Some(elsewhere.clone()),
    };
    e.edit(id, edit).await.unwrap();
    let d2 = e.get(id).unwrap();
    assert!(!path_of(&d).exists());
    assert_eq!(path_of(&d2), elsewhere.join("renamed.bin"));
    assert_eq!(std::fs::read(path_of(&d2)).unwrap(), srv.data(65536));

    // Unfinished and running: the partial file moves and the download carries on.
    let id = add(&e, srv.url("/file/live.bin?rate=2097152"));
    wait_for(&e, id, |d| d.downloaded > MIB).await;
    let edit = EditDownload {
        filename: Some("moved.bin".into()),
        dir: Some(elsewhere.clone()),
    };
    e.edit(id, edit).await.unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert_eq!(path_of(&d), elsewhere.join("moved.bin"));
    assert!(d.notice.is_none(), "should not start over: {:?}", d.notice);

    // Never onto an existing file.
    let err = e
        .edit(
            id,
            EditDownload {
                filename: Some("renamed.bin".into()),
                dir: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Exists(_)), "{err}");
}

#[tokio::test]
async fn custom_category_folders() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let films = tmp.path().join("films");
    let e = engine(tmp.path(), |s| {
        let rule = s
            .categories
            .iter_mut()
            .find(|r| r.category == Category::Video)
            .unwrap();
        rule.extensions.push("bin".into());
        rule.folder = Some(films.clone());
    })
    .await;
    let p = e
        .probe(&srv.url("/file/movie.bin"), &RequestMeta::default())
        .await
        .unwrap();
    assert_eq!(p.category, Category::Video);
    assert_eq!(p.dir, films);
    let id = add(&e, srv.url("/file/movie.bin?size=65536"));
    let d = wait_finished(&e, id).await;
    assert_eq!(d.category, Category::Video);
    assert_eq!(d.dir.as_deref(), Some(films.as_path()));
}

#[tokio::test]
async fn remove_many_at_once() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let ids: Vec<i64> = (0..3)
        .map(|i| add(&e, srv.url(&format!("/file/m{i}.bin?size=65536"))))
        .collect();
    for &id in &ids {
        wait_finished(&e, id).await;
    }
    let files: Vec<PathBuf> = ids.iter().map(|&id| path_of(&e.get(id).unwrap())).collect();
    assert_eq!(e.remove_many(&ids, false).unwrap(), 3);
    assert!(e.list().is_empty());
    assert!(files.iter().all(|f| f.exists()), "files kept");
}

// ---------------------------------------------------------------- logins, proxy, server limits

fn login(pass: &str) -> Login {
    Login {
        username: USER.into(),
        password: pass.into(),
    }
}

#[tokio::test]
async fn basic_login_flow() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let host = srv.addr.to_string();

    // The Add dialog's probe reports what's needed.
    match e
        .probe(&srv.url("/auth/basic/a.bin"), &RequestMeta::default())
        .await
    {
        Err(Error::AuthRequired {
            host: h,
            realm,
            rejected,
        }) => {
            assert_eq!(
                (h.as_str(), realm.as_deref(), rejected),
                (host.as_str(), Some("test"), false)
            );
        }
        other => panic!("expected a login request, got {other:?}"),
    }

    let mut events = e.subscribe();
    let id = add(&e, srv.url("/auth/basic/a.bin"));
    let d = wait_finished(&e, id).await;
    assert_eq!(d.status, Status::Failed);
    let want = LoginRequest {
        host: host.clone(),
        realm: Some("test".into()),
        rejected: false,
    };
    assert_eq!(d.needs_login, Some(want.clone()));
    let ev = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(Event::LoginRequired { id: i, request, .. }) = events.recv().await {
                return (i, request);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(ev, (id, want));

    // A wrong password is reported as refused.
    e.set_login(&host, login("nope")).unwrap();
    let d = wait_for(&e, id, |d| {
        d.needs_login.as_ref().is_some_and(|l| l.rejected)
    })
    .await;
    assert_eq!(d.status, Status::Failed);

    // The right one resumes it; every segment request carries it.
    e.set_login(&host, login(PASS)).unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert!(d.needs_login.is_none());
}

#[tokio::test]
async fn digest_login_with_nonce_change() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 8).await;
    e.set_login(&srv.addr.to_string(), login(PASS)).unwrap();
    let id = add(&e, srv.url("/auth/digest/a.bin?rate=4194304"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert!(srv.shared.nonce.load(SeqCst) > 1, "the nonce never rotated");
}

#[tokio::test]
async fn login_in_the_url() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let url = format!(
        "http://{USER}:pa%3Ass%20w%C3%B6rd@{}/auth/basic/a.bin",
        srv.addr
    );
    let id = add(&e, url);
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert!(
        !d.url.contains('@'),
        "credentials kept in the URL: {}",
        d.url
    );
}

#[tokio::test]
async fn login_not_sent_to_another_host() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    e.set_login(&srv.addr.to_string(), login(PASS)).unwrap();
    let away = format!(
        "http://localhost:{}/file/b.bin?noauth=true",
        srv.addr.port()
    );
    let id = add(&e, srv.url(&format!("/auth/away?to={}", urlencode(&away))));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert!(d.final_url.unwrap().starts_with("http://localhost:"));
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tokio::test]
async fn manual_proxy_with_login() {
    let srv = Server::start().await;
    *srv.shared.proxy_login.write().unwrap() = Some(basic("proxyuser", "secret"));
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| {
        s.proxy.mode = ProxyMode::Manual;
        s.proxy.scheme = ProxyScheme::Http;
        s.proxy.host = "127.0.0.1".into();
        s.proxy.port = srv.addr.port();
        s.proxy.username = "proxyuser".into();
        s.proxy.no_proxy = String::new();
    })
    .await;
    assert_eq!(
        e.proxy_route().summary(),
        format!("http://127.0.0.1:{}", srv.addr.port())
    );

    // `.invalid` never resolves: only the proxy can serve it.
    let url = "http://files.invalid/file/a.bin".to_string();
    let id = add(&e, url.clone());
    let d = wait_finished(&e, id).await;
    assert_eq!(d.status, Status::Failed);
    assert_eq!(
        d.error.as_deref(),
        Some("The proxy needs a username and password")
    );

    e.set_proxy_password(Some("secret".into()));
    e.resume(id).unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &srv.data(SIZE));
    assert!(srv.shared.proxied.load(SeqCst) >= 2);
}

#[tokio::test]
async fn connections_shared_per_server() {
    let srv = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| {
        s.connections = 8;
        s.max_concurrent = 3;
        s.max_connections_per_server = 6;
    })
    .await;
    let size = 6 * MIB as usize;
    let url = |n: &str| srv.url(&format!("/file/{n}?size={size}&rate=1048576"));
    let a = add(&e, url("a.bin"));
    // Alone, a download may use the whole limit but no more.
    wait_for(&e, a, |d| d.active_connections == 6).await;
    let b = add(&e, url("b.bin"));
    wait_for(&e, b, |d| d.status == Status::Downloading).await;

    let active = |e: &Engine| -> u32 {
        [a, b]
            .iter()
            .map(|&i| e.get(i).unwrap().active_connections)
            .sum()
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while active(&e) > 6 {
        assert!(Instant::now() < deadline, "never settled under the limit");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for _ in 0..20 {
        let (da, db) = (e.get(a).unwrap(), e.get(b).unwrap());
        if da.status != Status::Downloading || db.status != Status::Downloading {
            break;
        }
        assert!(
            active(&e) <= 6,
            "{} + {} connections",
            da.active_connections,
            db.active_connections
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // An exception for this server lifts the limit for both.
    let mut s = e.settings();
    s.server_limits = vec![ServerLimit {
        host: "127.0.0.1".into(),
        connections: 32,
    }];
    e.set_settings(s).unwrap();
    let da = wait_finished(&e, a).await;
    let db = wait_finished(&e, b).await;
    assert_complete(&da, &srv.data(size));
    assert_complete(&db, &srv.data(size));
}
