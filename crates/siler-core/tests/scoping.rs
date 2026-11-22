//! What the browser gave for one site (cookies, headers, a form's body, the page's address)
//! stays with that site, wherever redirects and edited links lead.

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use siler_core::{DownloadInfo, Engine, Event, NewDownload, NewMediaDownload, RequestMeta, Status};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const COOKIE: &str = "session=secret";
const PAGE: &str = "https://example.org/account/page?token=secret";
const FORM: &[u8] = b"password=secret";
const SIZE: usize = 2 * 1024 * 1024;

// ---------------------------------------------------------------- two sites

#[derive(Debug, Clone)]
struct Seen {
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Bytes,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

#[derive(Default)]
struct Site {
    /// `(path, status, Location)`.
    redirects: Mutex<Vec<(String, StatusCode, String)>>,
    files: Mutex<Vec<(String, Arc<Vec<u8>>)>>,
    seen: Mutex<Vec<Seen>>,
}

async fn serve(
    State(site): State<Arc<Site>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    site.seen.lock().unwrap().push(Seen {
        method,
        path: path.clone(),
        headers: headers.clone(),
        body,
    });
    let redirect = site
        .redirects
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.0 == path)
        .cloned();
    if let Some((_, status, to)) = redirect {
        return (status, [(header::LOCATION, to)]).into_response();
    }
    let file = site
        .files
        .lock()
        .unwrap()
        .iter()
        .find(|f| f.0 == path)
        .map(|f| f.1.clone());
    let Some(data) = file else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok())));
    let Some((start, end)) = range else {
        return data.as_ref().clone().into_response();
    };
    let end = end.unwrap_or(data.len() - 1).min(data.len() - 1);
    let mut resp = data[start..=end].to_vec().into_response();
    *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
    let cr = format!("bytes {start}-{end}/{}", data.len());
    resp.headers_mut()
        .insert(header::CONTENT_RANGE, HeaderValue::from_str(&cr).unwrap());
    resp
}

struct Server {
    base: String,
    site: Arc<Site>,
}

impl Server {
    /// A site of its own: every loopback address is another host.
    async fn start(ip: &str) -> Server {
        let site = Arc::new(Site::default());
        let listener = tokio::net::TcpListener::bind((ip, 0)).await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(serve).with_state(site.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Server { base, site }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn file(&self, path: &str, data: Vec<u8>) {
        self.site
            .files
            .lock()
            .unwrap()
            .push((path.into(), Arc::new(data)));
    }

    fn redirect(&self, path: &str, status: StatusCode, to: String) {
        self.site
            .redirects
            .lock()
            .unwrap()
            .push((path.into(), status, to));
    }

    fn seen(&self) -> Vec<Seen> {
        self.site.seen.lock().unwrap().clone()
    }

    /// Nothing that belongs to the other site arrived here.
    #[track_caller]
    fn got_nothing_private(&self) {
        let seen = self.seen();
        assert!(!seen.is_empty(), "this site was never asked");
        let mut leaked = Vec::new();
        for s in seen {
            let at = format!("{} {}", s.method, s.path);
            let range = s.header("range").unwrap_or("no range");
            let mut leak = |what: &str| leaked.push(format!("{at} ({range}): {what}"));
            if s.header("cookie").is_some() {
                leak("cookie");
            }
            if s.header("x-token").is_some() {
                leak("header");
            }
            if !s.body.is_empty() {
                leak("form body");
            }
            if let Some(r) = s.header("referer")
                && (r.contains("secret") || r.contains("account"))
            {
                leak("page address");
            }
        }
        assert!(leaked.is_empty(), "leaked to another site: {leaked:#?}");
    }
}

async fn two_sites() -> (Server, Server) {
    (
        Server::start("127.0.0.1").await,
        Server::start("127.0.0.2").await,
    )
}

fn engine(dir: &Path) -> Engine {
    let e = Engine::open(dir.join("data")).unwrap();
    let mut s = e.settings();
    s.download_dir = dir.join("dl");
    s.use_category_folders = false;
    s.min_split_size = 64 * 1024;
    s.connections = 4;
    e.set_settings(s).unwrap();
    e
}

fn private() -> RequestMeta {
    RequestMeta {
        referrer: Some(PAGE.into()),
        cookies: Some(COOKIE.into()),
        headers: vec![("X-Token".into(), "secret".into())],
        ..Default::default()
    }
}

fn private_form() -> RequestMeta {
    let mut m = private();
    m.method = Some("POST".into());
    m.body = Some(FORM.to_vec());
    m.headers.push((
        "Content-Type".into(),
        "application/x-www-form-urlencoded".into(),
    ));
    m
}

async fn until(e: &Engine, id: i64, want: impl Fn(&DownloadInfo) -> bool) -> DownloadInfo {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let d = e.get(id).expect("download exists");
        if want(&d) {
            return d;
        }
        assert!(Instant::now() < deadline, "timed out; last state: {d:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn completed(e: &Engine, id: i64) -> DownloadInfo {
    let d = until(e, id, |d| {
        matches!(d.status, Status::Completed | Status::Failed)
    })
    .await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    d
}

fn add(e: &Engine, url: String, meta: RequestMeta) -> i64 {
    e.add(NewDownload {
        url,
        meta,
        ..Default::default()
    })
    .unwrap()
}

// ---------------------------------------------------------------- files

#[tokio::test]
async fn a_redirect_to_another_host_gets_no_browser_context() {
    let (home, away) = two_sites().await;
    away.file("/f.bin", vec![7; SIZE]);
    home.redirect("/get", StatusCode::FOUND, away.url("/f.bin"));
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path());
    completed(&e, add(&e, home.url("/get"), private())).await;

    let first = &home.seen()[0];
    assert_eq!(
        first.header("cookie"),
        Some(COOKIE),
        "home needs its cookie"
    );
    assert_eq!(first.header("referer"), Some(PAGE));
    away.got_nothing_private();
}

#[tokio::test]
async fn another_port_is_another_origin() {
    let home = Server::start("127.0.0.1").await;
    let away = Server::start("127.0.0.1").await;
    away.file("/f.bin", vec![7; SIZE]);
    home.redirect("/get", StatusCode::FOUND, away.url("/f.bin"));
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path());
    completed(&e, add(&e, home.url("/get"), private())).await;
    away.got_nothing_private();
}

#[tokio::test]
async fn a_redirect_on_the_same_host_keeps_the_context() {
    let home = Server::start("127.0.0.1").await;
    home.file("/f.bin", vec![7; SIZE]);
    home.redirect("/get", StatusCode::FOUND, "/f.bin".into());
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path());
    completed(&e, add(&e, home.url("/get"), private())).await;
    for s in home.seen() {
        assert_eq!(s.header("cookie"), Some(COOKIE), "{}", s.path);
        assert_eq!(s.header("x-token"), Some("secret"), "{}", s.path);
    }
}

#[tokio::test]
async fn a_form_is_not_posted_to_another_host() {
    for status in [
        StatusCode::SEE_OTHER,
        StatusCode::TEMPORARY_REDIRECT,
        StatusCode::PERMANENT_REDIRECT,
    ] {
        let (home, away) = two_sites().await;
        away.file("/f.bin", vec![7; SIZE]);
        home.redirect("/get", status, away.url("/f.bin"));
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        completed(&e, add(&e, home.url("/get"), private_form())).await;
        assert_eq!(home.seen()[0].body.as_ref(), FORM, "{status}");
        away.got_nothing_private();
        for s in away.seen() {
            assert_eq!(s.method, Method::GET, "{status}");
        }
    }
}

#[tokio::test]
async fn a_form_answered_with_303_is_fetched_with_get() {
    let home = Server::start("127.0.0.1").await;
    home.file("/f.bin", vec![7; SIZE]);
    home.redirect("/get", StatusCode::SEE_OTHER, "/f.bin".into());
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path());
    completed(&e, add(&e, home.url("/get"), private_form())).await;
    for s in home.seen().iter().filter(|s| s.path == "/f.bin") {
        assert_eq!(s.method, Method::GET);
        assert!(s.body.is_empty());
        assert_eq!(s.header("cookie"), Some(COOKIE));
    }
}

#[tokio::test]
async fn looking_at_a_link_follows_the_same_rule() {
    let (home, away) = two_sites().await;
    away.file("/f.bin", vec![7; SIZE]);
    home.redirect("/get", StatusCode::TEMPORARY_REDIRECT, away.url("/f.bin"));
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path());
    let p = e.probe(&home.url("/get"), &private_form()).await.unwrap();
    assert_eq!(p.final_url, away.url("/f.bin"));
    away.got_nothing_private();
}

#[tokio::test]
async fn a_pasted_link_on_another_host_gets_no_old_context() {
    let (home, away) = two_sites().await;
    home.file("/old.bin", vec![7; SIZE]);
    away.file("/new.bin", vec![7; SIZE]);
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path());
    let id = e
        .add(NewDownload {
            url: home.url("/old.bin"),
            meta: private(),
            start_paused: true,
            ..Default::default()
        })
        .unwrap();
    e.refresh_url(id, &away.url("/new.bin"), None)
        .await
        .unwrap();
    completed(&e, id).await;
    away.got_nothing_private();
}

#[tokio::test]
async fn a_restart_keeps_the_rule() {
    let (home, away) = two_sites().await;
    away.file("/f.bin", vec![7; SIZE]);
    home.redirect("/get", StatusCode::FOUND, away.url("/f.bin"));
    let tmp = tempfile::tempdir().unwrap();
    let id = {
        let e = engine(tmp.path());
        let id = e
            .add(NewDownload {
                url: home.url("/get"),
                meta: private(),
                start_paused: true,
                ..Default::default()
            })
            .unwrap();
        e.shutdown().await;
        id
    };
    let e = Engine::open(tmp.path().join("data")).unwrap();
    e.resume(id).unwrap();
    completed(&e, id).await;
    away.got_nothing_private();
}

// ---------------------------------------------------------------- archives and lists

fn archive() -> Vec<u8> {
    siler_core::zip::make_zip(
        &[
            ("a.txt", b"hello from the archive", false),
            ("pad.bin", &[3u8; 300_000], false),
        ],
        false,
    )
}

#[tokio::test]
async fn listing_an_archive_after_a_redirect() {
    let (home, away) = two_sites().await;
    away.file("/a.zip", archive());
    home.redirect("/get", StatusCode::FOUND, away.url("/a.zip"));
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path());
    let l = e.zip_list(&home.url("/get"), &private()).await.unwrap();
    assert_eq!(l.entries.len(), 2);
    away.got_nothing_private();
}

#[tokio::test]
async fn a_file_out_of_an_archive_after_a_redirect() {
    let (home, away) = two_sites().await;
    away.file("/a.zip", archive());
    home.redirect("/get", StatusCode::FOUND, away.url("/a.zip"));
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path());
    let l = e
        .zip_list(&away.url("/a.zip"), &RequestMeta::default())
        .await
        .unwrap();
    let id = e
        .add(NewDownload {
            url: home.url("/get"),
            filename: Some("a.txt".into()),
            meta: private(),
            zip_entry: Some(l.entries[0].entry(l.archive_size)),
            ..Default::default()
        })
        .unwrap();
    completed(&e, id).await;
    away.got_nothing_private();
}

#[tokio::test]
async fn a_metalink_after_a_redirect() {
    let (home, away) = two_sites().await;
    let list = format!(
        r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="f.bin"><url>{}</url></file></metalink>"#,
        away.url("/f.bin")
    );
    away.file("/list.meta4", list.into_bytes());
    home.redirect("/get", StatusCode::FOUND, away.url("/list.meta4"));
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path());
    let mut events = e.subscribe();
    add(&e, home.url("/get"), private());
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(events.recv().await.unwrap(), Event::Metalink { .. }) {}
    })
    .await
    .expect("the metalink opened");
    away.got_nothing_private();
}

// ---------------------------------------------------------------- media

const PLAYLIST: &str =
    "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nseg.ts\n#EXT-X-ENDLIST\n";

#[tokio::test]
async fn a_streams_form_is_not_posted_to_another_host() {
    let (home, away) = two_sites().await;
    away.file("/v.m3u8", PLAYLIST.as_bytes().to_vec());
    home.redirect("/get", StatusCode::TEMPORARY_REDIRECT, away.url("/v.m3u8"));
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path());
    let _ = e
        .probe_media(&home.url("/get"), &private_form(), None)
        .await;
    away.got_nothing_private();
    let _ = e
        .add_media(NewMediaDownload {
            url: home.url("/get"),
            meta: private_form(),
            start_paused: true,
            ..Default::default()
        })
        .await;
    away.got_nothing_private();
}
