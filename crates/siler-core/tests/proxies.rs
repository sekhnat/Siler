//! Proxies of every type, proxy scripts, and proxy logins.

mod ntlm_check;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use md5::{Digest, Md5};
use ntlm_check::{challenge, right_answer};
use siler_core::proxy::{ProxyMode, ProxyScheme, ProxyWhy};
use siler_core::{DownloadInfo, Engine, Error, Login, NewDownload, RequestMeta, Settings, Status};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const SIZE: usize = 2 * 1024 * 1024;

fn data() -> Vec<u8> {
    (0..SIZE).map(|i| (i % 251) as u8).collect()
}

struct Head {
    method: String,
    target: String,
    fields: Vec<(String, String)>,
}

impl Head {
    fn get(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

async fn read_head(r: &mut BufReader<TcpStream>) -> Option<Head> {
    let mut line = String::new();
    if r.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let mut first = line.split_whitespace();
    let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
    let mut fields = Vec::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let Some((n, v)) = line.trim().split_once(':') else {
            break;
        };
        fields.push((n.to_string(), v.trim().to_string()));
    }
    Some(Head {
        method,
        target,
        fields,
    })
}

/// Answers like a file server; `?to=` redirects.
async fn serve_file(w: &mut BufReader<TcpStream>, head: &Head) -> std::io::Result<()> {
    if let Some((_, to)) = head.target.split_once("?to=") {
        let out = format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\n\r\n");
        return w.write_all(out.as_bytes()).await;
    }
    let data = data();
    let range = head
        .get("range")
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse().unwrap_or(SIZE - 1))));
    let (top, from, to) = match range {
        Some((a, b)) => {
            let b = std::cmp::min(b, SIZE - 1);
            let top = format!("206 Partial Content\r\nContent-Range: bytes {a}-{b}/{SIZE}");
            (top, a, b + 1)
        }
        None => ("200 OK".to_string(), 0, SIZE),
    };
    let out = format!("HTTP/1.1 {top}\r\nContent-Length: {}\r\n\r\n", to - from);
    w.write_all(out.as_bytes()).await?;
    w.write_all(&data[from..to]).await?;
    w.flush().await
}

// ---------------------------------------------------------------- the servers

#[derive(Clone, Copy)]
enum Auth {
    None,
    Basic,
    Digest,
    Ntlm,
}

/// A file server, or a proxy that is the web: it serves what it is asked for, and tunnels
/// to the port named on this computer.
struct Server {
    port: u16,
    auth: Auth,
    /// "CONNECT files.invalid:80" or "GET http://files.invalid/a".
    seen: Mutex<Vec<String>>,
    logins: Mutex<Vec<String>>,
    answers: AtomicUsize,
    tunnels: AtomicUsize,
}

fn md5_hex(s: &str) -> String {
    Md5::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn digest_right(method: &str, header: &str) -> bool {
    let Some(rest) = header.strip_prefix("Digest ") else {
        return false;
    };
    let get = |key: &str| {
        rest.split(", ")
            .find_map(|p| p.trim().strip_prefix(&format!("{key}=")))
            .unwrap_or("")
            .trim_matches('"')
            .to_string()
    };
    let ha1 = md5_hex("puser:px:secret");
    let ha2 = md5_hex(&format!("{method}:{}", get("uri")));
    let mid = format!("n1:{}:{}:auth", get("nc"), get("cnonce"));
    get("response") == md5_hex(&format!("{ha1}:{mid}:{ha2}"))
}

impl Server {
    async fn start(auth: Auth) -> Arc<Server> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Arc::new(Server {
            port: listener.local_addr().unwrap().port(),
            auth,
            seen: Mutex::new(Vec::new()),
            logins: Mutex::new(Vec::new()),
            answers: AtomicUsize::new(0),
            tunnels: AtomicUsize::new(0),
        });
        let s = server.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(s.clone().session(sock));
            }
        });
        server
    }

    async fn session(self: Arc<Self>, sock: TcpStream) -> std::io::Result<()> {
        let mut io = BufReader::new(sock);
        let mut logged_in = false;
        let mut pending: Option<(Vec<u8>, Vec<u8>, [u8; 8])> = None;
        while let Some(head) = read_head(&mut io).await {
            self.seen
                .lock()
                .unwrap()
                .push(format!("{} {}", head.method, head.target));
            let given = head.get("proxy-authorization").unwrap_or("").to_string();
            if !given.is_empty() {
                self.logins.lock().unwrap().push(given.clone());
            }
            let mut ask = None;
            match self.auth {
                Auth::None => {}
                Auth::Basic => {
                    let right = ["puser:secret", "puser:100%41"]
                        .map(|login| format!("Basic {}", B64.encode(login)));
                    if !right.contains(&given) {
                        ask = Some("Basic realm=\"px\"".to_string());
                    }
                }
                Auth::Digest => {
                    if !digest_right(&head.method, &given) {
                        ask = Some(r#"Digest realm="px", nonce="n1", qop="auth""#.to_string());
                    }
                }
                Auth::Ntlm => {
                    let token = given
                        .strip_prefix("NTLM ")
                        .and_then(|t| B64.decode(t).ok())
                        .filter(|t| t.len() > 12);
                    match token {
                        Some(t1) if t1[8] == 1 => {
                            logged_in = false;
                            let nonce = fastrand::u64(..).to_le_bytes();
                            let t2 = challenge(&nonce);
                            ask = Some(format!("NTLM {}", B64.encode(&t2)));
                            pending = Some((t1, t2, nonce));
                        }
                        Some(t3) if t3[8] == 3 => {
                            self.answers.fetch_add(1, SeqCst);
                            logged_in = pending
                                .take()
                                .is_some_and(|(t1, t2, nonce)| right_answer(&t1, &t2, &nonce, &t3));
                            if !logged_in {
                                ask = Some("NTLM".to_string());
                            }
                        }
                        _ if logged_in => {}
                        _ => ask = Some("NTLM".to_string()),
                    }
                }
            }
            if let Some(ask) = ask {
                let out = format!(
                    "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: {ask}\r\nContent-Length: 5\r\n\r\nlogin"
                );
                io.write_all(out.as_bytes()).await?;
                continue;
            }
            if head.method == "CONNECT" {
                self.tunnels.fetch_add(1, SeqCst);
                let port: u16 = head.target.rsplit(':').next().unwrap().parse().unwrap();
                let mut to = TcpStream::connect(("127.0.0.1", port)).await?;
                io.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await?;
                tokio::io::copy_bidirectional(&mut io, &mut to).await?;
                return Ok(());
            }
            serve_file(&mut io, &head).await?;
        }
        Ok(())
    }

    fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// An address of this computer that a script is asked about (loopback ones aren't).
    fn unlisted(&self) -> String {
        format!("0.0.0.0:{}", self.port)
    }

    fn times(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

/// A SOCKS proxy of every version, whose names all mean this computer.
struct Socks {
    port: u16,
    used: Mutex<Vec<String>>,
}

impl Socks {
    async fn start(login: Option<(&'static str, &'static str)>) -> Arc<Socks> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socks = Arc::new(Socks {
            port: listener.local_addr().unwrap().port(),
            used: Mutex::new(Vec::new()),
        });
        let s = socks.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(s.clone().session(sock, login));
            }
        });
        socks
    }

    async fn session(
        self: Arc<Self>,
        sock: TcpStream,
        login: Option<(&'static str, &'static str)>,
    ) -> std::io::Result<()> {
        let mut s = BufReader::new(sock);
        let (what, port) = if s.read_u8().await? == 4 {
            s.read_u8().await?;
            let port = s.read_u16().await?;
            let ip = s.read_u32().await?;
            let mut user = Vec::new();
            s.read_until(0, &mut user).await?;
            let what = if ip >> 8 == 0 && ip != 0 {
                let mut name = Vec::new();
                s.read_until(0, &mut name).await?;
                name.pop();
                format!("4a {}", String::from_utf8_lossy(&name))
            } else {
                format!("4 {}", std::net::Ipv4Addr::from(ip))
            };
            s.write_all(&[0, 90, 0, 0, 0, 0, 0, 0]).await?;
            (what, port)
        } else {
            let n = s.read_u8().await?;
            let mut methods = vec![0; usize::from(n)];
            s.read_exact(&mut methods).await?;
            match login {
                None => s.write_all(&[5, 0]).await?,
                Some(_) if !methods.contains(&2) => return s.write_all(&[5, 0xff]).await,
                Some((user, pass)) => {
                    s.write_all(&[5, 2]).await?;
                    s.read_u8().await?;
                    let mut u = vec![0; usize::from(s.read_u8().await?)];
                    s.read_exact(&mut u).await?;
                    let mut p = vec![0; usize::from(s.read_u8().await?)];
                    s.read_exact(&mut p).await?;
                    if u != user.as_bytes() || p != pass.as_bytes() {
                        return s.write_all(&[1, 1]).await;
                    }
                    s.write_all(&[1, 0]).await?;
                }
            }
            let mut head = [0; 4];
            s.read_exact(&mut head).await?;
            let what = if head[3] == 3 {
                let mut name = vec![0; usize::from(s.read_u8().await?)];
                s.read_exact(&mut name).await?;
                format!("5 {}", String::from_utf8_lossy(&name))
            } else {
                format!("5 {}", std::net::Ipv4Addr::from(s.read_u32().await?))
            };
            let port = s.read_u16().await?;
            s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            (what, port)
        };
        self.used.lock().unwrap().push(what);
        let mut to = TcpStream::connect(("127.0.0.1", port)).await?;
        tokio::io::copy_bidirectional(&mut s, &mut to).await?;
        Ok(())
    }
}

// ---------------------------------------------------------------- helpers

fn helper() -> PathBuf {
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap();
    let path = target.join("debug/siler-pac");
    assert!(path.is_file(), "build it first: cargo build -p siler-pac");
    path
}

async fn engine(dir: &Path, tweak: impl FnOnce(&mut Settings)) -> Engine {
    let e = Engine::open(dir.join("data")).unwrap();
    e.set_pac_helper(Some(helper()));
    let mut s = e.settings();
    s.download_dir = dir.join("dl");
    s.use_category_folders = false;
    s.min_split_size = 64 * 1024;
    s.connections = 8;
    s.max_retries = 2;
    s.proxy.mode = ProxyMode::None;
    s.proxy.no_proxy = String::new();
    tweak(&mut s);
    e.set_settings(s).unwrap();
    e
}

fn manual(s: &mut Settings, scheme: ProxyScheme, port: u16, user: &str) {
    s.proxy.mode = ProxyMode::Manual;
    s.proxy.scheme = scheme;
    s.proxy.host = "127.0.0.1".into();
    s.proxy.port = port;
    s.proxy.username = user.into();
}

/// An engine that follows a script whose function has this body.
async fn scripted(dir: &Path, body: &str) -> Engine {
    let path = dir.join("proxy.pac");
    std::fs::write(
        &path,
        format!("function FindProxyForURL(url, host) {{ {body} }}"),
    )
    .unwrap();
    engine(dir, |s| {
        s.proxy.mode = ProxyMode::Script;
        s.proxy.script = path.display().to_string();
    })
    .await
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
fn complete(d: &DownloadInfo) {
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let path = Path::new(d.dir.as_ref().unwrap()).join(d.filename.as_ref().unwrap());
    assert!(std::fs::read(path).unwrap() == data(), "the file differs");
}

fn login(user: &str, pass: &str) -> Login {
    Login {
        username: user.into(),
        password: pass.into(),
    }
}

async fn dead_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
}

// ---------------------------------------------------------------- types

#[tokio::test]
async fn socks_of_every_version() {
    let web = Server::start(Auth::None).await;
    for (scheme, host, used) in [
        (ProxyScheme::Socks4, "127.0.0.1", "4 127.0.0.1"),
        (ProxyScheme::Socks4a, "files.invalid", "4a files.invalid"),
        (ProxyScheme::Socks5, "files.invalid", "5 files.invalid"),
    ] {
        let socks = Socks::start(None).await;
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path(), |s| manual(s, scheme, socks.port, "")).await;
        let id = add(&e, format!("http://{host}:{}/a.bin", web.port));
        complete(&finished(&e, id).await);
        assert!(socks.used.lock().unwrap().iter().all(|u| u == used));
        assert!(!socks.used.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn socks5_with_a_login() {
    let web = Server::start(Auth::None).await;
    let socks = Socks::start(Some(("puser", "p@ss wörd:%41"))).await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| {
        manual(s, ProxyScheme::Socks5, socks.port, "puser")
    })
    .await;
    e.set_proxy_password(Some("p@ss wörd:%41".into()));
    let id = add(&e, format!("http://files.invalid:{}/a.bin", web.port));
    complete(&finished(&e, id).await);
}

// ---------------------------------------------------------------- scripts

#[tokio::test]
async fn a_script_chooses_per_address_and_per_hop() {
    let proxy = Server::start(Auth::None).await;
    let direct = Server::start(Auth::None).await;
    let tmp = tempfile::tempdir().unwrap();
    let body = format!(
        "return shExpMatch(host, '*.invalid') ? 'PROXY {}' : 'DIRECT';",
        proxy.addr()
    );
    let e = scripted(tmp.path(), &body).await;
    let id = add(&e, "http://files.invalid/a.bin".into());
    complete(&finished(&e, id).await);
    assert!(proxy.times() > 0 && direct.times() == 0);

    let to = format!("http://{}/b.bin", direct.unlisted());
    let id = add(&e, format!("http://away.invalid/go?to={to}"));
    complete(&finished(&e, id).await);
    assert!(direct.times() > 0, "the redirect's hop went direct");
    let seen = proxy.seen.lock().unwrap().clone();
    assert!(seen.iter().all(|s| !s.ends_with(" /b.bin")), "{seen:?}");
    let direct_seen = format!("http://{}/b.bin", direct.unlisted());
    assert!(!seen.contains(&format!("GET {direct_seen}")), "{seen:?}");
}

#[tokio::test]
async fn the_next_in_the_list_takes_over() {
    let live = Server::start(Auth::None).await;
    let tmp = tempfile::tempdir().unwrap();
    let body = format!(
        "return 'PROXY 127.0.0.1:{}; PROXY {}';",
        dead_port().await,
        live.addr()
    );
    let e = scripted(tmp.path(), &body).await;
    let id = add(&e, "http://files.invalid/a.bin".into());
    complete(&finished(&e, id).await);

    let web = Server::start(Auth::None).await;
    let tmp = tempfile::tempdir().unwrap();
    let body = format!("return 'PROXY 127.0.0.1:{}; DIRECT';", dead_port().await);
    let e = scripted(tmp.path(), &body).await;
    let id = add(&e, format!("http://{}/a.bin", web.unlisted()));
    complete(&finished(&e, id).await);
}

#[tokio::test]
async fn a_failing_script_never_means_direct() {
    for body in ["throw 'no';", "for (;;) {}", "return 'NONSENSE here';"] {
        let web = Server::start(Auth::None).await;
        let tmp = tempfile::tempdir().unwrap();
        let e = scripted(tmp.path(), body).await;
        let id = add(&e, format!("http://{}/a.bin", web.unlisted()));
        let d = finished(&e, id).await;
        assert_eq!(d.status, Status::Failed, "{body}");
        assert!(d.error.unwrap().contains("proxy script"), "{body}");
        assert_eq!(web.times(), 0, "{body}");
    }
    let web = Server::start(Auth::None).await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| {
        s.proxy.mode = ProxyMode::Script;
        s.proxy.script = "/nowhere/proxy.pac".into();
    })
    .await;
    let id = add(&e, format!("http://{}/a.bin", web.unlisted()));
    assert_eq!(finished(&e, id).await.status, Status::Failed);
    assert_eq!(web.times(), 0);
}

#[tokio::test]
async fn a_script_from_the_web() {
    let proxy = Server::start(Auth::None).await;
    let body = format!(
        "function FindProxyForURL() {{ return 'PROXY {}'; }}",
        proxy.addr()
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = listener.local_addr().unwrap();
    let fetched = Arc::new(AtomicUsize::new(0));
    let count = fetched.clone();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let mut io = BufReader::new(sock);
            if read_head(&mut io).await.is_some() {
                count.fetch_add(1, SeqCst);
                let out = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = io.write_all(out.as_bytes()).await;
            }
        }
    });
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| {
        s.proxy.mode = ProxyMode::Script;
        s.proxy.script = format!("http://{at}/proxy.pac");
    })
    .await;
    let id = add(&e, "http://files.invalid/a.bin".into());
    complete(&finished(&e, id).await);
    assert_eq!(fetched.load(SeqCst), 1, "fetched once for all connections");
}

#[tokio::test]
async fn a_script_doesnt_see_what_is_private_in_https() {
    let tmp = tempfile::tempdir().unwrap();
    let body = "return url === 'https://bank.example/' ? 'DIRECT' : 'PROXY saw.it:1';";
    let e = scripted(tmp.path(), body).await;
    let check = e
        .check_proxy("https://bank.example/account?token=secret")
        .await
        .unwrap();
    assert_eq!(check.via, ["DIRECT"]);
    assert!(
        matches!(&check.why, ProxyWhy::Script { from } if from.ends_with("proxy.pac")),
        "{:?}",
        check.why
    );
    let check = e.check_proxy("http://plain.example/page").await.unwrap();
    assert_eq!(check.via, ["http://saw.it:1"]);
}

#[tokio::test]
async fn media_still_refuses_private_addresses_for_direct() {
    let web = Server::start(Auth::None).await;
    let proxy = Server::start(Auth::None).await;
    let tmp = tempfile::tempdir().unwrap();
    let body = format!(
        "return host === 'public.invalid' ? 'PROXY {}' : 'DIRECT';",
        proxy.addr()
    );
    let e = scripted(tmp.path(), &body).await;
    let url = format!(
        "http://public.invalid/v.m3u8?to=http://localhost:{}/private.m3u8",
        web.port
    );
    let result = e.probe_media(&url, &RequestMeta::default(), None).await;
    assert!(result.is_err());
    assert_eq!(web.times(), 0, "a private address was asked");
}

// ---------------------------------------------------------------- logins

#[tokio::test]
async fn the_manual_password_stays_with_the_manual_proxy() {
    let proxy = Server::start(Auth::Basic).await;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("proxy.pac");
    std::fs::write(
        &path,
        format!(
            "function FindProxyForURL() {{ return 'PROXY {}'; }}",
            proxy.addr()
        ),
    )
    .unwrap();
    let e = engine(tmp.path(), |s| {
        manual(s, ProxyScheme::Http, proxy.port, "puser");
        s.proxy.mode = ProxyMode::Script;
        s.proxy.script = path.display().to_string();
    })
    .await;
    e.set_proxy_password(Some("secret".into()));
    let id = add(&e, "http://files.invalid/a.bin".into());
    let d = finished(&e, id).await;
    let key = format!("proxy+http://{}", proxy.addr());
    let asked = d.needs_login.expect("the found proxy's login is asked for");
    assert_eq!((asked.host.as_str(), asked.rejected), (key.as_str(), false));
    assert!(proxy.logins.lock().unwrap().is_empty(), "a login was sent");

    e.set_login(&key, login("puser", "secret")).unwrap();
    complete(&finished(&e, id).await);
}

#[tokio::test]
async fn digest_at_the_proxy() {
    let proxy = Server::start(Auth::Digest).await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| {
        manual(s, ProxyScheme::Http, proxy.port, "puser")
    })
    .await;
    e.set_proxy_password(Some("secret".into()));
    let id = add(&e, "http://files.invalid/a.bin".into());
    complete(&finished(&e, id).await);
    let sent = proxy.logins.lock().unwrap().clone();
    assert!(sent.iter().all(|l| l.starts_with("Digest")), "{sent:?}");
}

#[tokio::test]
async fn ntlm_at_the_proxy() {
    let proxy = Server::start(Auth::Ntlm).await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| {
        manual(s, ProxyScheme::Http, proxy.port, "CORP\\jo");
        s.connections = 16;
    })
    .await;
    e.set_proxy_password(Some(ntlm_check::PASS.into()));
    let id = add(&e, "http://127.0.0.1:9/a.bin".into());
    complete(&finished(&e, id).await);
    let sent = proxy.logins.lock().unwrap().clone();
    assert!(sent.iter().all(|l| !l.starts_with("Basic")), "{sent:?}");
}

#[tokio::test]
async fn a_wrong_ntlm_proxy_password_is_sent_once() {
    let proxy = Server::start(Auth::Ntlm).await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| {
        manual(s, ProxyScheme::Http, proxy.port, "CORP\\jo");
        s.max_concurrent = 8;
    })
    .await;
    e.set_proxy_password(Some("wrong".into()));
    let mut ids: Vec<i64> = (0..4)
        .map(|i| add(&e, format!("http://files.invalid/{i}.bin")))
        .collect();
    // Through a tunnel as well.
    ids.push(add(&e, "https://files.invalid/secure.bin".into()));
    for id in ids {
        let d = finished(&e, id).await;
        assert_eq!(
            d.error.as_deref(),
            Some("The proxy needs a username and password")
        );
    }
    assert_eq!(proxy.answers.load(SeqCst), 1);
}

#[tokio::test]
async fn tunnels_log_in() {
    for (auth, user, pass) in [
        (Auth::None, "", ""),
        (Auth::Basic, "puser", "secret"),
        (Auth::Basic, "puser", "100%41"),
        (Auth::Digest, "puser", "secret"),
        (Auth::Ntlm, "CORP\\jo", ntlm_check::PASS),
    ] {
        let proxy = Server::start(auth).await;
        let not_tls = Server::start(Auth::None).await;
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path(), |s| {
            manual(s, ProxyScheme::Http, proxy.port, user);
            s.max_retries = 1;
        })
        .await;
        e.set_proxy_password(Some(pass.into()).filter(|p: &String| !p.is_empty()));
        let url = format!("https://files.invalid:{}/a.bin", not_tls.port);
        let result = e.probe(&url, &RequestMeta::default()).await;
        assert!(proxy.tunnels.load(SeqCst) > 0, "{user}: no tunnel");
        assert!(
            matches!(result, Err(Error::Network(_))),
            "{user}: TLS to a plain server: {result:?}"
        );
    }
}
