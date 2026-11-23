//! Engine tests for 0.7's power features: checksums, mirrors, metalinks, ZIP entries, the
//! data quota, the command after a download, and the site grabber.

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use siler_core::checksum::{Algo, Checksum, ChecksumSource, Pieces};
use siler_core::extras::{Mirror, MirrorOrigin};
use siler_core::grabber::{GrabConfig, GrabResult, Scope};
use siler_core::{DownloadInfo, Engine, Event, NewDownload, Settings, Status};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- a small file server

#[derive(Clone, Default)]
struct Item {
    body: Arc<Vec<u8>>,
    content_type: String,
    headers: Vec<(String, String)>,
    /// Body requests answer 500 (probes still work).
    fail_body: bool,
    /// The first body request covering this byte range gets it damaged.
    corrupt: Option<(u64, u64)>,
    /// Per-request throttle in bytes per second.
    rate: Option<u64>,
}

#[derive(Default)]
struct Shared {
    items: Mutex<HashMap<String, Item>>,
    served: AtomicU64,
    corrupted: AtomicBool,
    requests: AtomicUsize,
}

struct Server {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

fn range(h: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let v = h
        .get(header::RANGE)?
        .to_str()
        .ok()?
        .strip_prefix("bytes=")?;
    let (a, b) = v.split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()))
}

async fn serve(State(srv): State<Arc<Shared>>, uri: Uri, headers: HeaderMap) -> Response {
    srv.requests.fetch_add(1, SeqCst);
    let Some(item) = srv.items.lock().unwrap().get(uri.path()).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let len = item.body.len() as u64;
    let r = range(&headers);
    let probe = r == Some((0, Some(0)));
    if item.fail_body && !probe {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let (status, start, end) = match r {
        Some((a, b)) if len > 0 => (
            StatusCode::PARTIAL_CONTENT,
            a,
            b.unwrap_or(len - 1).min(len - 1),
        ),
        _ => (StatusCode::OK, 0, len.saturating_sub(1)),
    };
    let mut body = if len == 0 {
        Vec::new()
    } else {
        item.body[start as usize..=end as usize].to_vec()
    };
    if !probe
        && let Some((a, b)) = item.corrupt
        && start <= a
        && end >= b
        && !srv.corrupted.swap(true, SeqCst)
    {
        for x in &mut body[(a - start) as usize..(b - start) as usize] {
            *x ^= 0x5a;
        }
    }
    if !probe {
        srv.served.fetch_add(body.len() as u64, SeqCst);
    }
    let n = body.len();
    let rate = item.rate;
    let stream = futures_util::stream::unfold((body, 0usize), move |(body, pos)| async move {
        if pos >= body.len() {
            return None;
        }
        let end = (pos + 16_384).min(body.len());
        if let Some(r) = rate {
            tokio::time::sleep(Duration::from_secs_f64((end - pos) as f64 / r as f64)).await;
        }
        let chunk = axum::body::Bytes::copy_from_slice(&body[pos..end]);
        Some((Ok::<_, std::io::Error>(chunk), (body, end)))
    });
    let mut resp = Response::new(Body::from_stream(stream));
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(header::CONTENT_LENGTH, (n as u64).into());
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&item.content_type).unwrap(),
    );
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if status == StatusCode::PARTIAL_CONTENT {
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")).unwrap(),
        );
    }
    for (k, v) in &item.headers {
        h.append(
            header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    resp
}

impl Server {
    async fn start() -> Server {
        let shared = Arc::new(Shared::default());
        let app = Router::new().fallback(serve).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Server { addr, shared }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    fn put(&self, path: &str, item: Item) {
        self.shared.items.lock().unwrap().insert(path.into(), item);
    }

    fn file(&self, path: &str, body: Vec<u8>) {
        self.put(
            path,
            Item {
                body: Arc::new(body),
                content_type: "application/octet-stream".into(),
                ..Default::default()
            },
        );
    }

    fn page(&self, path: &str, ct: &str, body: &str) {
        self.put(
            path,
            Item {
                body: Arc::new(body.as_bytes().to_vec()),
                content_type: ct.into(),
                ..Default::default()
            },
        );
    }
}

fn data(n: usize, seed: u64) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ seed;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn digest(algo: Algo, bytes: &[u8]) -> String {
    let mut h = algo.hasher();
    h.update(bytes);
    h.finish()
}

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

async fn wait_for(
    e: &Engine,
    id: i64,
    secs: u64,
    pred: impl Fn(&DownloadInfo) -> bool,
) -> DownloadInfo {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let d = e.get(id).expect("download exists");
        if pred(&d) {
            return d;
        }
        assert!(Instant::now() < deadline, "timed out; last state: {d:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn finished(e: &Engine, id: i64) -> DownloadInfo {
    wait_for(e, id, 30, |d| {
        matches!(d.status, Status::Completed | Status::Failed)
    })
    .await
}

fn path_of(d: &DownloadInfo) -> std::path::PathBuf {
    d.dir.as_ref().unwrap().join(d.filename.as_ref().unwrap())
}

fn completed_with(d: &DownloadInfo, want: &[u8]) {
    assert_eq!(d.status, Status::Completed, "failed: {:?}", d.error);
    assert!(
        std::fs::read(path_of(d)).unwrap() == want,
        "content differs"
    );
}

const SIZE: usize = 3 * 1024 * 1024;

// ---------------------------------------------------------------- checksums

#[tokio::test]
async fn a_matching_checksum_completes() {
    let srv = Server::start().await;
    let body = data(SIZE, 1);
    srv.file("/f.bin", body.clone());
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let c = Checksum::parse(&digest(Algo::Sha256, &body)).unwrap();
    let id = e
        .add(NewDownload {
            url: srv.url("/f.bin"),
            checksum: Some(c.clone()),
            ..Default::default()
        })
        .unwrap();
    let d = finished(&e, id).await;
    completed_with(&d, &body);
    let v = d.verify.unwrap();
    assert_eq!(v.matches(), Some(true));
    assert_eq!(v.actual, c.hex);
}

#[tokio::test]
async fn a_wrong_checksum_fails_and_can_be_kept() {
    let srv = Server::start().await;
    let body = data(SIZE, 2);
    srv.file("/f.bin", body.clone());
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = e
        .add(NewDownload {
            url: srv.url("/f.bin"),
            checksum: Some(Checksum::parse(&digest(Algo::Md5, b"something else")).unwrap()),
            ..Default::default()
        })
        .unwrap();
    let d = finished(&e, id).await;
    assert_eq!(d.status, Status::Failed);
    assert!(d.checksum_failed, "{d:?}");
    assert!(d.error.as_deref().unwrap().contains("MD5"));
    let part = siler_core::filename::part_path(d.dir.as_ref().unwrap(), "f.bin");
    assert!(part.exists(), "the data is kept");
    assert!(!d.dir.as_ref().unwrap().join("f.bin").exists());

    // Keep it anyway: published without fetching anything again.
    let before = srv.shared.served.load(SeqCst);
    e.accept_unverified(id).unwrap();
    let d = finished(&e, id).await;
    completed_with(&d, &body);
    assert_eq!(srv.shared.served.load(SeqCst), before);
    assert!(d.notice.unwrap().contains("Kept"));
}

#[tokio::test]
async fn the_servers_digest_header_is_checked() {
    use base64::Engine as _;
    let srv = Server::start().await;
    let body = data(SIZE, 3);
    let hex = digest(Algo::Sha256, &body);
    let raw: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    srv.put(
        "/f.bin",
        Item {
            body: Arc::new(body.clone()),
            content_type: "application/octet-stream".into(),
            headers: vec![("Repr-Digest".into(), format!("sha-256=:{b64}:"))],
            ..Default::default()
        },
    );
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = e
        .add(NewDownload {
            url: srv.url("/f.bin"),
            ..Default::default()
        })
        .unwrap();
    let d = finished(&e, id).await;
    completed_with(&d, &body);
    assert_eq!(d.checksum.unwrap().source, ChecksumSource::Server);
    assert_eq!(d.verify.unwrap().matches(), Some(true));
}

#[tokio::test]
async fn damaged_pieces_are_fetched_again() {
    let srv = Server::start().await;
    let body = data(SIZE, 4);
    srv.put(
        "/f.bin",
        Item {
            body: Arc::new(body.clone()),
            content_type: "application/octet-stream".into(),
            corrupt: Some((1_500_000, 1_500_100)),
            ..Default::default()
        },
    );
    let piece = 256 * 1024u64;
    let pieces = Pieces {
        algo: Algo::Sha1,
        length: piece,
        hashes: body
            .chunks(piece as usize)
            .map(|c| digest(Algo::Sha1, c))
            .collect(),
    };
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 1).await;
    let id = e
        .add(NewDownload {
            url: srv.url("/f.bin"),
            checksum: Some(Checksum::parse(&digest(Algo::Sha256, &body)).unwrap()),
            pieces: Some(pieces),
            ..Default::default()
        })
        .unwrap();
    let d = finished(&e, id).await;
    completed_with(&d, &body);
    assert!(srv.shared.corrupted.load(SeqCst), "the damage happened");
    let served = srv.shared.served.load(SeqCst);
    assert!(
        served < SIZE as u64 + 2 * piece,
        "only the damaged piece came again ({served} bytes)"
    );
}

// ---------------------------------------------------------------- mirrors

fn with_mirror(url: String, mirror: String) -> NewDownload {
    NewDownload {
        url,
        mirrors: vec![Mirror::new(mirror, MirrorOrigin::User)],
        ..Default::default()
    }
}

#[tokio::test]
async fn mirrors_share_the_work() {
    let (a, b) = (Server::start().await, Server::start().await);
    let body = data(SIZE, 5);
    for s in [&a, &b] {
        s.put(
            "/f.bin",
            Item {
                body: Arc::new(body.clone()),
                content_type: "application/octet-stream".into(),
                rate: Some(4 * 1024 * 1024),
                ..Default::default()
            },
        );
    }
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 4).await;
    let id = e
        .add(with_mirror(a.url("/f.bin"), b.url("/f.bin")))
        .unwrap();
    let d = finished(&e, id).await;
    completed_with(&d, &body);
    assert!(a.shared.served.load(SeqCst) > 0);
    assert!(b.shared.served.load(SeqCst) > 0, "the mirror was used");
    assert_eq!(d.sources.len(), 2);
    assert_eq!(
        d.sources.iter().map(|s| s.bytes).sum::<u64>(),
        SIZE as u64,
        "{:?}",
        d.sources
    );
}

#[tokio::test]
async fn a_mirror_with_another_file_is_not_used() {
    let (a, b) = (Server::start().await, Server::start().await);
    let body = data(SIZE, 6);
    a.file("/f.bin", body.clone());
    b.file("/f.bin", data(SIZE - 10, 7));
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = e
        .add(with_mirror(a.url("/f.bin"), b.url("/f.bin")))
        .unwrap();
    let d = finished(&e, id).await;
    completed_with(&d, &body);
    assert!(
        d.sources[1]
            .disabled
            .as_deref()
            .unwrap()
            .contains("different")
    );
    assert_eq!(b.shared.served.load(SeqCst), 0);
}

#[tokio::test]
async fn a_failing_mirror_is_dropped() {
    let (a, b) = (Server::start().await, Server::start().await);
    let body = data(SIZE, 8);
    a.file("/f.bin", body.clone());
    b.put(
        "/f.bin",
        Item {
            body: Arc::new(body.clone()),
            content_type: "application/octet-stream".into(),
            fail_body: true,
            ..Default::default()
        },
    );
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.max_retries = 2).await;
    let id = e
        .add(with_mirror(a.url("/f.bin"), b.url("/f.bin")))
        .unwrap();
    let d = finished(&e, id).await;
    completed_with(&d, &body);
    assert!(d.sources[1].disabled.is_some(), "{:?}", d.sources);
}

#[tokio::test]
async fn link_headers_add_mirrors() {
    let (a, b) = (Server::start().await, Server::start().await);
    let body = data(SIZE, 9);
    a.put(
        "/f.bin",
        Item {
            body: Arc::new(body.clone()),
            content_type: "application/octet-stream".into(),
            headers: vec![(
                "Link".into(),
                format!("<{}>; rel=duplicate; pri=1", b.url("/f.bin")),
            )],
            rate: Some(4 * 1024 * 1024),
            ..Default::default()
        },
    );
    b.file("/f.bin", body.clone());
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = e
        .add(NewDownload {
            url: a.url("/f.bin"),
            ..Default::default()
        })
        .unwrap();
    let d = finished(&e, id).await;
    completed_with(&d, &body);
    // The server named a mirror on a private address (this test's 127.0.0.1); the download's
    // own address is private too, so it may be used.
    assert_eq!(d.sources.len(), 2);
    assert_eq!(d.sources[1].origin, MirrorOrigin::Server);
}

// ---------------------------------------------------------------- metalink

#[tokio::test]
async fn a_metalink_download_becomes_its_files() {
    let srv = Server::start().await;
    let body = data(1000, 10);
    srv.file("/real.bin", body.clone());
    let ml = format!(
        r#"<?xml version="1.0"?><metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="real.bin"><size>1000</size><hash type="sha-256">{}</hash><url priority="1">{}</url></file></metalink>"#,
        digest(Algo::Sha256, &body),
        srv.url("/real.bin")
    );
    srv.page("/list.meta4", "application/metalink4+xml", &ml);
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let mut events = e.subscribe();
    let id = e
        .add(NewDownload {
            url: srv.url("/list.meta4"),
            ..Default::default()
        })
        .unwrap();
    let files = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::Metalink { id: got, files }) = events.recv().await
                && got == id
            {
                return files;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].url, srv.url("/real.bin"));
    assert!(files[0].checksum.is_some());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(e.get(id).is_none(), "the metalink itself was removed");
    let id = e.add(files[0].clone()).unwrap();
    let d = finished(&e, id).await;
    completed_with(&d, &body);
    assert_eq!(d.verify.unwrap().matches(), Some(true));
}

// ---------------------------------------------------------------- ZIP

#[tokio::test]
async fn files_are_picked_out_of_a_zip() {
    let srv = Server::start().await;
    let big = data(600_000, 11);
    let zip = siler_core::zip::make_zip(
        &[
            ("docs/", b"", false),
            ("docs/readme.txt", b"hello from the archive", false),
            ("data/big.bin", &big, true),
            ("padding.bin", &data(2_000_000, 12), false),
        ],
        false,
    );
    srv.file("/a.zip", zip.clone());
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let listing = e
        .zip_list(&srv.url("/a.zip"), &Default::default())
        .await
        .unwrap();
    assert_eq!(listing.archive_size, zip.len() as u64);
    assert_eq!(listing.entries.len(), 4);
    let served_listing = srv.shared.served.load(SeqCst);
    assert!(
        served_listing < 100_000,
        "listing read only the end ({served_listing})"
    );

    let dir = tmp.path().join("out");
    let mut ids = Vec::new();
    for item in listing
        .entries
        .iter()
        .filter(|i| i.path == "docs/readme.txt" || i.path == "data/big.bin")
    {
        let (folders, name) = siler_core::zip::target(&item.path).unwrap();
        let id = e
            .add(NewDownload {
                url: srv.url("/a.zip"),
                filename: Some(name),
                dir: Some(folders.iter().fold(dir.clone(), |d, f| d.join(f))),
                zip_entry: Some(item.entry(listing.archive_size)),
                ..Default::default()
            })
            .unwrap();
        ids.push(id);
    }
    let readme = finished(&e, ids[0]).await;
    completed_with(&readme, b"hello from the archive");
    assert_eq!(path_of(&readme), dir.join("docs/readme.txt"));
    let bigd = finished(&e, ids[1]).await;
    completed_with(&bigd, &big);
    assert_eq!(bigd.zip_path.as_deref(), Some("data/big.bin"));
    assert_eq!(
        bigd.total_size,
        Some(big.len() as u64),
        "its own size, not the compressed one"
    );
    assert!(
        srv.shared.served.load(SeqCst) < zip.len() as u64 / 2,
        "only the picked files were fetched"
    );
}

// ---------------------------------------------------------------- quota

#[tokio::test]
async fn the_quota_holds_downloads_until_the_next_period() {
    let srv = Server::start().await;
    let body = data(SIZE, 13);
    srv.put(
        "/f.bin",
        Item {
            body: Arc::new(body.clone()),
            content_type: "application/octet-stream".into(),
            ..Default::default()
        },
    );
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| {
        s.connections = 8;
        s.quota.enabled = true;
        s.quota.megabytes = 2;
        s.quota.period_secs = Some(2);
    })
    .await;
    let mut events = e.subscribe();
    let id = e
        .add(NewDownload {
            url: srv.url("/f.bin"),
            ..Default::default()
        })
        .unwrap();
    let held = wait_for(&e, id, 20, |d| {
        d.status == Status::Queued && d.notice.is_some()
    })
    .await;
    assert!(held.notice.as_deref().unwrap().contains("quota"));
    assert!(
        held.downloaded <= 2 * 1024 * 1024 + 64 * 1024,
        "stopped at the limit, not later ({} bytes)",
        held.downloaded
    );
    // The engine notices within a tick.
    let until = Instant::now() + Duration::from_secs(2);
    while !e.quota_status().reached {
        assert!(
            Instant::now() < until,
            "the quota was never reported as reached"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut saw = false;
    while let Ok(ev) = events.try_recv() {
        saw |= matches!(ev, Event::Quota(ref q) if q.reached);
    }
    assert!(saw, "a quota event was sent");
    let d = finished(&e, id).await;
    completed_with(&d, &body);
}

// ---------------------------------------------------------------- command after a download

#[tokio::test]
async fn a_command_runs_after_each_download() {
    let srv = Server::start().await;
    srv.file("/a.bin", data(1000, 14));
    srv.file("/b.bin", data(1000, 15));
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| {
        s.after_download.enabled = true;
        s.after_download.command =
            "sh -c 'test \"$1\" != {name}.no && cp \"$1\" \"$1.copy\"; exit $2' _ {path} {size}"
                .into();
    })
    .await;
    let id = e
        .add(NewDownload {
            url: srv.url("/a.bin"),
            ..Default::default()
        })
        .unwrap();
    let d = wait_for(&e, id, 20, |d| {
        d.command.as_ref().is_some_and(|c| !c.running)
    })
    .await;
    let c = d.command.clone().unwrap();
    // `exit 1000` is 1000 % 256 = 232.
    assert_eq!(c.exit_code, Some(232));
    assert!(path_of(&d).with_extension("bin.copy").exists());
    assert!(d.notice.unwrap().contains("232"));

    let mut s = e.settings();
    s.after_download.command = "true".into();
    e.set_settings(s).unwrap();
    let id = e
        .add(NewDownload {
            url: srv.url("/b.bin"),
            ..Default::default()
        })
        .unwrap();
    let d = wait_for(&e, id, 20, |d| {
        d.command.as_ref().is_some_and(|c| !c.running)
    })
    .await;
    assert!(d.command.unwrap().ok());
}

// ---------------------------------------------------------------- site grabber

async fn grab(e: &Engine, cfg: GrabConfig, project: Option<i64>) -> GrabResult {
    let mut events = e.subscribe();
    let run = e.grab_start(cfg, project).unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(Event::Grab(p)) = events.recv().await
                && p.run_id == run
                && p.finished
            {
                assert!(p.error.is_none(), "{:?}", p.error);
                return p.result.unwrap();
            }
        }
    })
    .await
    .unwrap()
}

fn site(srv: &Server) {
    let html = "text/html; charset=utf-8";
    srv.page(
        "/",
        html,
        r#"<html><head><link rel="stylesheet" href="/css/site.css"></head><body>
<a href="/docs/a.html">A</a> <a href="/private/p.html">P</a>
<a href="/files/tool.zip">zip</a> <a href="https://elsewhere.invalid/x.html">out</a>
<img src="/img/logo.png"></body></html>"#,
    );
    srv.page(
        "/docs/a.html",
        html,
        r#"<html><body><a href="b.html#top">B</a><a href="../files/manual.pdf">pdf</a><a href="/">home</a></body></html>"#,
    );
    srv.page(
        "/docs/b.html",
        html,
        r#"<html><body><a href="/files/deep.iso">deep</a></body></html>"#,
    );
    srv.page(
        "/private/p.html",
        html,
        r#"<html><body><a href="/files/secret.zip">s</a></body></html>"#,
    );
    srv.page(
        "/css/site.css",
        "text/css",
        "body{background:url(../img/bg.png)}",
    );
    srv.page(
        "/robots.txt",
        "text/plain",
        "User-agent: *\nDisallow: /private/\n",
    );
    srv.file("/img/logo.png", b"PNG1".to_vec());
    srv.file("/img/bg.png", b"PNG2".to_vec());
    srv.file("/files/tool.zip", data(5000, 16));
    srv.file("/files/manual.pdf", data(100, 17));
    srv.file("/files/deep.iso", data(100, 18));
    srv.file("/files/secret.zip", data(100, 19));
}

#[tokio::test]
async fn the_grabber_finds_files_and_saves_pages() {
    let srv = Server::start().await;
    site(&srv);
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let out = tmp.path().join("site");
    let cfg = GrabConfig {
        start_url: srv.url("/"),
        depth: 1,
        scope: Scope::SameHost,
        save_pages: true,
        dir: Some(out.clone()),
        ..Default::default()
    };
    let r = grab(&e, cfg.clone(), None).await;
    let mut names: Vec<String> = r
        .files
        .iter()
        .map(|f| f.url.rsplit('/').next().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["manual.pdf", "tool.zip"],
        "depth 1, robots respected"
    );
    assert_eq!(r.blocked, 1);
    let host = format!("127.0.0.1_{}", srv.addr.port());
    let index = out.join(&host).join("index.html");
    assert_eq!(r.site_index.as_deref(), Some(index.as_path()));
    let saved = std::fs::read_to_string(&index).unwrap();
    assert!(saved.contains(r#"href="docs/a.html""#), "{saved}");
    assert!(saved.contains(r#"href="files/tool.zip""#), "{saved}");
    assert!(saved.contains(r#"src="img/logo.png""#), "{saved}");
    assert!(saved.contains(r#"href="css/site.css""#), "{saved}");
    assert!(
        saved.contains(&srv.url("/private/p.html")),
        "unsaved pages link to the site"
    );
    let a = std::fs::read_to_string(out.join(&host).join("docs/a.html")).unwrap();
    assert!(a.contains(r#"href="../index.html""#), "{a}");
    assert!(a.contains(&srv.url("/docs/b.html#top")), "{a}");
    let css = std::fs::read_to_string(out.join(&host).join("css/site.css")).unwrap();
    assert!(css.contains(r#"url("../img/bg.png")"#), "{css}");
    assert_eq!(
        std::fs::read(out.join(&host).join("img/bg.png")).unwrap(),
        b"PNG2"
    );
    // Files go where the saved pages expect them.
    let zip = r
        .files
        .iter()
        .find(|f| f.url.ends_with("tool.zip"))
        .unwrap();
    assert_eq!(
        zip.dir.as_deref(),
        Some(out.join(&host).join("files").as_path())
    );
    assert_eq!(zip.filename.as_deref(), Some("tool.zip"));
}

#[tokio::test]
async fn grab_projects_find_only_new_files() {
    let srv = Server::start().await;
    site(&srv);
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let cfg = GrabConfig {
        start_url: srv.url("/"),
        depth: 2,
        robots: false,
        only_new: true,
        ..Default::default()
    };
    let id = e
        .save_grab_project(siler_core::grabber::GrabProject {
            id: 0,
            name: "Test site".into(),
            config: cfg.clone(),
            last_run: None,
        })
        .unwrap();
    let first = grab(&e, cfg.clone(), Some(id)).await;
    assert_eq!(first.files.len(), 4, "{:?}", first.files);
    assert_eq!(first.blocked, 0);
    srv.page(
        "/docs/b.html",
        "text/html",
        r#"<a href="/files/deep.iso">d</a><a href="/files/new.zip">n</a>"#,
    );
    srv.file("/files/new.zip", data(10, 20));
    let second = grab(&e, cfg, Some(id)).await;
    assert_eq!(second.files.len(), 1);
    assert!(second.files[0].url.ends_with("new.zip"));
    let projects = e.grab_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert!(projects[0].last_run.is_some());
}

#[tokio::test]
async fn unpacking_never_writes_through_a_planted_link() {
    let srv = Server::start().await;
    let big = data(600_000, 21);
    let zip = siler_core::zip::make_zip(&[("big.bin", &big, true)], false);
    srv.file("/b.zip", zip.clone());
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let listing = e
        .zip_list(&srv.url("/b.zip"), &Default::default())
        .await
        .unwrap();
    let dir = tmp.path().join("out");
    std::fs::create_dir_all(&dir).unwrap();
    let precious = tmp.path().join("precious");
    std::fs::write(&precious, b"not yours").unwrap();
    std::os::unix::fs::symlink(&precious, dir.join(".big.bin.siler-unpack")).unwrap();
    let id = e
        .add(NewDownload {
            url: srv.url("/b.zip"),
            filename: Some("big.bin".into()),
            dir: Some(dir.clone()),
            zip_entry: Some(listing.entries[0].entry(listing.archive_size)),
            ..Default::default()
        })
        .unwrap();
    let d = finished(&e, id).await;
    assert_eq!(d.status, Status::Failed, "{d:?}");
    assert_eq!(
        d.error_code.as_ref().map(|c| c.code.as_str()),
        Some("unsafe-part")
    );
    assert_eq!(std::fs::read(&precious).unwrap(), b"not yours");
    assert!(!dir.join("big.bin").exists());
}
