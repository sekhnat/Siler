//! Windows logins (NTLM, also inside Negotiate): they belong to a connection and to one
//! scheme, host and port, and a refused password is sent once.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use siler_core::{DownloadInfo, Engine, Error, Login, NewDownload, RequestMeta, Settings, Status};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

mod ntlm_check;
use ntlm_check::{PASS, challenge, right_answer};

const SIZE: usize = 8 * 1024 * 1024;
const FORM: &[u8] = b"a=1&b=2";

// ---------------------------------------------------------------- a server that checks NTLMv2

struct Site {
    data: Vec<u8>,
    offers: Vec<&'static str>,
    /// The word the challenge comes back under.
    carrier: &'static str,
    addr: std::net::SocketAddr,
    /// Type 3 messages seen, and those that were right.
    answers: AtomicUsize,
    accepted: AtomicUsize,
    /// Every `Authorization` header seen.
    sent: Mutex<Vec<String>>,
    /// Close the connection after a challenge, without saying so.
    hang_up_once: AtomicBool,
    /// A login lasts one request.
    forgetful: AtomicBool,
}

impl Site {
    async fn serve(self: Arc<Self>, sock: TcpStream) -> std::io::Result<()> {
        let (read, mut write) = sock.into_split();
        let mut read = BufReader::new(read);
        let mut logged_in = false;
        let mut pending: Option<(Vec<u8>, Vec<u8>, [u8; 8])> = None;
        loop {
            let mut first = String::new();
            if read.read_line(&mut first).await? == 0 {
                return Ok(());
            }
            let method = first.split(' ').next().unwrap_or("").to_string();
            let path = first.split(' ').nth(1).unwrap_or("").to_string();
            let (mut auth, mut range, mut length) = (None, None, 0usize);
            loop {
                let mut line = String::new();
                if read.read_line(&mut line).await? == 0 {
                    return Ok(());
                }
                if line.trim().is_empty() {
                    break;
                }
                let (name, value) = line.trim().split_once(':').unwrap_or_default();
                let value = value.trim().to_string();
                match name.to_ascii_lowercase().as_str() {
                    "authorization" => {
                        self.sent.lock().unwrap().push(value.clone());
                        auth = Some(value);
                    }
                    "range" => range = value.strip_prefix("bytes=").map(str::to_string),
                    "content-length" => length = value.parse().unwrap(),
                    _ => {}
                }
            }
            let mut body = vec![0; length];
            read.read_exact(&mut body).await?;

            let refuse: Vec<String> = self.offers.iter().map(|o| o.to_string()).collect();
            let mut ask = None;
            let mut hang_up = false;
            if path.starts_with("/basic") {
                let want = format!("Basic {}", B64.encode(format!("CORP\\jo:{PASS}")));
                if auth.as_deref() != Some(&want) {
                    ask = Some(vec!["Basic realm=\"old part\"".to_string()]);
                }
            } else if !path.starts_with("/open") {
                let token = auth
                    .as_deref()
                    .and_then(|a| a.split_once(' '))
                    .filter(|(word, _)| *word == self.carrier)
                    .and_then(|(_, t)| B64.decode(t).ok())
                    .filter(|t| t.len() > 12 && &t[..8] == b"NTLMSSP\0");
                match token {
                    Some(t1) if t1[8] == 1 => {
                        logged_in = false;
                        let nonce = fastrand::u64(..).to_le_bytes();
                        let t2 = challenge(&nonce);
                        ask = Some(vec![format!("{} {}", self.carrier, B64.encode(&t2))]);
                        pending = Some((t1, t2, nonce));
                        hang_up = self.hang_up_once.swap(false, SeqCst);
                    }
                    Some(t3) if t3[8] == 3 => {
                        self.answers.fetch_add(1, SeqCst);
                        logged_in = pending
                            .take()
                            .is_some_and(|(t1, t2, nonce)| right_answer(&t1, &t2, &nonce, &t3));
                        if logged_in {
                            self.accepted.fetch_add(1, SeqCst);
                        } else {
                            ask = Some(refuse);
                        }
                    }
                    _ if logged_in => {}
                    _ => ask = Some(refuse),
                }
            }

            if let Some(asks) = ask {
                let mut head = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 5\r\n".to_string();
                for a in asks {
                    head += &format!("WWW-Authenticate: {a}\r\n");
                }
                write
                    .write_all(format!("{head}\r\nlogin").as_bytes())
                    .await?;
                if hang_up {
                    return Ok(());
                }
                continue;
            }
            if method == "POST" && body != FORM {
                write
                    .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
                    .await?;
                continue;
            }
            if let Some(to) = path.split_once("?to=").map(|p| p.1) {
                let head = format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\n");
                write.write_all(format!("{head}\r\n").as_bytes()).await?;
                continue;
            }
            let range = range
                .as_deref()
                .and_then(|r| r.split_once('-'))
                .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().unwrap_or(SIZE - 1))));
            let (status, extra, from, to) = match range {
                Some((a, b)) => {
                    let b: usize = std::cmp::min(b, SIZE - 1);
                    let cr = format!("Content-Range: bytes {a}-{b}/{SIZE}\r\n");
                    ("206 Partial Content", cr, a, b + 1)
                }
                None => ("200 OK", String::new(), 0, SIZE),
            };
            let len = to - from;
            let head = format!("HTTP/1.1 {status}\r\nContent-Length: {len}\r\n{extra}\r\n");
            write.write_all(head.as_bytes()).await?;
            for chunk in self.data[from..to].chunks(256 * 1024) {
                write.write_all(chunk).await?;
                // Slow enough that several connections get to work.
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if self.forgetful.load(SeqCst) {
                logged_in = false;
            }
        }
    }

    async fn start(offers: &[&'static str], carrier: &'static str) -> Arc<Site> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let site = Arc::new(Site {
            data: (0..SIZE).map(|i| (i % 251) as u8).collect(),
            offers: offers.to_vec(),
            carrier,
            addr: listener.local_addr().unwrap(),
            answers: AtomicUsize::new(0),
            accepted: AtomicUsize::new(0),
            sent: Mutex::new(Vec::new()),
            hang_up_once: AtomicBool::new(false),
            forgetful: AtomicBool::new(false),
        });
        let served = site.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(served.clone().serve(sock));
            }
        });
        site
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// What logins for this site are kept under.
    fn key(&self) -> String {
        format!("http://{}", self.addr)
    }
}

// ---------------------------------------------------------------- helpers

async fn engine(dir: &Path, connections: u8, tweak: impl FnOnce(&mut Settings)) -> Engine {
    let e = Engine::open(dir.join("data")).unwrap();
    let mut s = e.settings();
    s.download_dir = dir.join("dl");
    s.use_category_folders = false;
    s.min_split_size = 64 * 1024;
    s.connections = connections;
    tweak(&mut s);
    e.set_settings(s).unwrap();
    e
}

fn login(user: &str, pass: &str) -> Login {
    Login {
        username: user.into(),
        password: pass.into(),
    }
}

fn corp() -> Login {
    login("CORP\\jo", PASS)
}

fn add(e: &Engine, url: String) -> i64 {
    e.add(NewDownload {
        url,
        ..Default::default()
    })
    .unwrap()
}

async fn finished(e: &Engine, id: i64) -> DownloadInfo {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let d = e.get(id).expect("the download");
        if matches!(d.status, Status::Completed | Status::Failed) {
            return d;
        }
        assert!(Instant::now() < until, "didn't finish: {d:?}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[track_caller]
fn complete(d: &DownloadInfo, site: &Site) {
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let path = Path::new(d.dir.as_ref().unwrap()).join(d.filename.as_ref().unwrap());
    assert!(
        std::fs::read(path).unwrap() == site.data,
        "the file differs"
    );
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn ntlm_with_32_connections() {
    let site = Site::start(&["NTLM", "Basic realm=\"x\""], "NTLM").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), 32, |_| {}).await;
    let id = add(&e, site.url("/file.bin"));
    let d = finished(&e, id).await;
    let asked = d.needs_login.expect("a login is asked for");
    assert_eq!((asked.host.clone(), asked.rejected), (site.key(), false));

    e.set_login(&asked.host, corp()).unwrap();
    complete(&finished(&e, id).await, &site);
    assert!(
        site.accepted.load(SeqCst) > 1,
        "several connections logged in"
    );
    assert_eq!(site.accepted.load(SeqCst), site.answers.load(SeqCst));
    // NTLM was on offer, so the password was never sent readably.
    let sent = site.sent.lock().unwrap().clone();
    assert!(sent.iter().all(|a| !a.starts_with("Basic")), "{sent:?}");
}

#[tokio::test]
async fn a_wrong_password_is_sent_once() {
    let site = Site::start(&["NTLM"], "NTLM").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), 32, |s| s.max_concurrent = 8).await;
    e.set_login(&site.key(), login("CORP\\jo", "wrong"))
        .unwrap();
    let ids: Vec<i64> = (0..8)
        .map(|i| add(&e, site.url(&format!("/{i}.bin"))))
        .collect();
    for id in ids {
        let d = finished(&e, id).await;
        assert!(d.needs_login.is_some_and(|l| l.rejected), "{:?}", d.error);
    }
    assert_eq!(site.answers.load(SeqCst), 1, "one attempt for all of them");

    // Not again by a retry or a new download either.
    let id = add(&e, site.url("/later.bin"));
    assert_eq!(finished(&e, id).await.status, Status::Failed);
    assert_eq!(site.answers.load(SeqCst), 1);

    // What needs no login still downloads.
    let id = add(&e, site.url("/open/free.bin"));
    complete(&finished(&e, id).await, &site);
    assert_eq!(site.answers.load(SeqCst), 1);
}

#[tokio::test]
async fn a_password_that_stops_working() {
    let site = Site::start(&["NTLM"], "NTLM").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), 4, |_| {}).await;
    e.set_login(&site.key(), corp()).unwrap();
    let first = add(&e, site.url("/first.bin"));
    complete(&finished(&e, first).await, &site);

    e.set_login(&site.key(), login("CORP\\jo", "wrong"))
        .unwrap();
    let second = add(&e, site.url("/second.bin"));
    assert!(
        finished(&e, second)
            .await
            .needs_login
            .is_some_and(|l| l.rejected)
    );

    e.set_login(&site.key(), corp()).unwrap();
    complete(&finished(&e, second).await, &site);
}

#[tokio::test]
async fn negotiate_falls_back_to_ntlm_with_a_password() {
    let site = Site::start(&["Negotiate"], "Negotiate").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), 8, |_| {}).await;
    e.set_login(&site.key(), corp()).unwrap();
    let id = add(&e, site.url("/file.bin"));
    complete(&finished(&e, id).await, &site);
}

#[tokio::test]
async fn negotiate_sends_nothing_unapproved() {
    let site = Site::start(&["Negotiate", "NTLM"], "NTLM").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), 4, |_| {}).await;
    match e
        .probe(&site.url("/file.bin"), &RequestMeta::default())
        .await
    {
        Err(Error::AuthRequired { host, rejected, .. }) => {
            assert_eq!((host, rejected), (site.key(), false));
        }
        other => panic!("expected a login request, got {other:?}"),
    }
    assert!(site.sent.lock().unwrap().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn a_login_stays_with_its_scheme_host_and_port() {
    let site = Site::start(&["NTLM"], "NTLM").await;
    let other = Site::start(&["NTLM"], "NTLM").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), 4, |_| {}).await;
    e.set_login(&format!("https://{}", site.addr), corp())
        .unwrap();
    e.set_login(&site.addr.to_string(), corp()).unwrap();
    let id = add(&e, site.url("/file.bin"));
    assert_eq!(finished(&e, id).await.status, Status::Failed);
    assert_eq!(
        site.answers.load(SeqCst),
        0,
        "a login for https, or for old logins"
    );

    e.set_login(&other.key(), corp()).unwrap();
    let to = site.url("/file.bin");
    let id = add(&e, other.url(&format!("/away?to={to}")));
    assert_eq!(finished(&e, id).await.status, Status::Failed);
    assert_eq!(other.accepted.load(SeqCst), 1, "its own site took it");
    assert_eq!(site.answers.load(SeqCst), 0, "a redirect carried it along");
}

#[tokio::test]
async fn posts_keep_their_body() {
    let site = Site::start(&["NTLM"], "NTLM").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), 4, |_| {}).await;
    e.set_login(&site.key(), corp()).unwrap();
    let id = e
        .add(NewDownload {
            url: site.url("/form"),
            meta: RequestMeta {
                method: Some("POST".into()),
                body: Some(FORM.to_vec()),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
    complete(&finished(&e, id).await, &site);
}

#[tokio::test]
async fn a_lost_connection_is_not_a_wrong_password() {
    let site = Site::start(&["NTLM"], "NTLM").await;
    site.hang_up_once.store(true, SeqCst);
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), 1, |_| {}).await;
    e.set_login(&site.key(), corp()).unwrap();
    let id = add(&e, site.url("/file.bin"));
    let d = finished(&e, id).await;
    assert!(d.needs_login.is_none(), "asked again: {:?}", d.error);
}

#[tokio::test]
async fn a_login_that_lasts_one_request() {
    let site = Site::start(&["NTLM"], "NTLM").await;
    site.forgetful.store(true, SeqCst);
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), 4, |_| {}).await;
    e.set_login(&site.key(), corp()).unwrap();
    let id = add(&e, site.url("/file.bin"));
    complete(&finished(&e, id).await, &site);
}

#[tokio::test]
async fn other_schemes_on_the_same_site() {
    let site = Site::start(&["NTLM"], "NTLM").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), 4, |_| {}).await;
    e.set_login(&site.key(), corp()).unwrap();
    for path in ["/one.bin", "/basic/two.bin", "/open/three.bin", "/four.bin"] {
        let id = add(&e, site.url(path));
        complete(&finished(&e, id).await, &site);
    }
}

#[tokio::test]
async fn a_login_in_the_address() {
    let site = Site::start(&["NTLM"], "NTLM").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), 4, |_| {}).await;
    let url = format!("http://CORP%5Cjo:P%C3%A4ssword!@{}/file.bin", site.addr);
    let id = add(&e, url);
    complete(&finished(&e, id).await, &site);
}

#[tokio::test]
async fn a_ticket_is_no_answer_to_ntlm() {
    let site = Site::start(&["NTLM", "Basic realm=\"x\""], "NTLM").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), 4, |_| {}).await;
    e.set_login(&site.key(), Login::kerberos_ticket()).unwrap();
    let id = add(&e, site.url("/file.bin"));
    let d = finished(&e, id).await;
    // Refused, so that a saved approval isn't tried again and again.
    assert!(d.needs_login.is_some_and(|l| l.rejected), "{:?}", d.error);
    assert!(site.sent.lock().unwrap().is_empty(), "nothing was sent");
}
