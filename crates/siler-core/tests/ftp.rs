//! FTP downloads against a small in-process FTP server (passive mode, REST, logins,
//! connection limits), directly and through a SOCKS5 proxy.

use siler_core::proxy::{ProxyMode, ProxyScheme};
use siler_core::{DownloadInfo, Engine, Login, NewDownload, RequestMeta, Settings, Status};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const SIZE: usize = 3 * 1024 * 1024 + 123;

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 % 251) as u8).collect()
}

// ---------------------------------------------------------------- test server

#[derive(Default)]
struct Config {
    /// Required login; `None` = anonymous works.
    login: Option<(String, String)>,
    /// More sessions than this get "421 Too many connections".
    max_sessions: Option<usize>,
    no_rest: bool,
    no_epsv: bool,
    /// Pause after each 64 KiB sent.
    delay: Option<Duration>,
}

struct Ftp {
    files: HashMap<String, Vec<u8>>,
    dirs: Vec<String>,
    cfg: Config,
    sessions: AtomicUsize,
    peak: AtomicUsize,
    retrs: AtomicUsize,
    rests: Mutex<Vec<u64>>,
    refused: AtomicUsize,
}

struct Server {
    port: u16,
    ftp: Arc<Ftp>,
}

impl Server {
    async fn start(cfg: Config) -> Server {
        let mut files = HashMap::new();
        files.insert("pub/big file.bin".to_string(), data(SIZE));
        files.insert("small.txt".to_string(), b"hello over ftp\n".to_vec());
        let ftp = Arc::new(Ftp {
            files,
            dirs: vec!["pub".into()],
            cfg,
            sessions: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            retrs: AtomicUsize::new(0),
            rests: Mutex::new(Vec::new()),
            refused: AtomicUsize::new(0),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let f = ftp.clone();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                tokio::spawn(session(f.clone(), s));
            }
        });
        Server { port, ftp }
    }

    fn url(&self, path: &str) -> String {
        format!("ftp://127.0.0.1:{}/{path}", self.port)
    }
}

struct SessionGuard(Arc<Ftp>);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.sessions.fetch_sub(1, SeqCst);
    }
}

async fn session(ftp: Arc<Ftp>, s: TcpStream) {
    let n = ftp.sessions.fetch_add(1, SeqCst) + 1;
    let _guard = SessionGuard(ftp.clone());
    let (r, mut w) = s.into_split();
    let mut r = BufReader::new(r);
    if ftp.cfg.max_sessions.is_some_and(|m| n > m) {
        ftp.refused.fetch_add(1, SeqCst);
        let _ = w
            .write_all(b"421 Too many connections from your IP\r\n")
            .await;
        return;
    }
    ftp.peak.fetch_max(n, SeqCst);
    macro_rules! say {
        ($($t:tt)*) => {
            if w.write_all(format!("{}\r\n", format!($($t)*)).as_bytes()).await.is_err() {
                return;
            }
        };
    }
    say!("220-Welcome\r\n220 test server ready");
    let mut user = String::new();
    let mut logged_in = false;
    let mut rest = 0u64;
    let mut passive: Option<TcpListener> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end();
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        let cmd = cmd.to_ascii_uppercase();
        if !logged_in && !matches!(cmd.as_str(), "USER" | "PASS" | "QUIT" | "FEAT") {
            say!("530 Please login with USER and PASS");
            continue;
        }
        match cmd.as_str() {
            "USER" => {
                user = arg.to_string();
                say!("331 Password required for {user}");
            }
            "PASS" => {
                let ok = match &ftp.cfg.login {
                    None => true,
                    Some((u, p)) => *u == user && *p == arg,
                };
                if ok {
                    logged_in = true;
                    say!("230 Logged in");
                } else {
                    say!("530 Login incorrect");
                }
            }
            "FEAT" => {
                let rest = if ftp.cfg.no_rest {
                    ""
                } else {
                    " REST STREAM\r\n"
                };
                say!("211-Features:\r\n SIZE\r\n MDTM\r\n{rest} UTF8\r\n211 End");
            }
            "OPTS" | "TYPE" | "PBSZ" => say!("200 OK"),
            "NOOP" => say!("200 NOOP ok"),
            "CWD" => {
                if ftp.dirs.iter().any(|d| d == arg.trim_matches('/')) {
                    say!("250 Directory changed");
                } else {
                    say!("550 No such directory");
                }
            }
            "SIZE" => match ftp.files.get(arg) {
                Some(f) => say!("213 {}", f.len()),
                None => say!("550 {arg}: No such file"),
            },
            "MDTM" => match ftp.files.get(arg) {
                Some(_) => say!("213 20261122103000"),
                None => say!("550 No such file"),
            },
            "REST" => {
                if ftp.cfg.no_rest {
                    say!("502 REST not implemented");
                } else {
                    rest = arg.parse().unwrap_or(0);
                    say!("350 Restarting at {rest}");
                }
            }
            "EPSV" if ftp.cfg.no_epsv => say!("502 EPSV not implemented"),
            "EPSV" | "PASV" => {
                let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let p = l.local_addr().unwrap().port();
                passive = Some(l);
                if cmd == "EPSV" {
                    say!("229 Entering Extended Passive Mode (|||{p}|)");
                } else {
                    // A private-looking address that must be ignored by the client.
                    say!(
                        "227 Entering Passive Mode (10,9,8,7,{},{})",
                        p / 256,
                        p % 256
                    );
                }
            }
            "RETR" => {
                let Some(file) = ftp.files.get(arg) else {
                    say!("550 {arg}: No such file");
                    continue;
                };
                let Some(l) = passive.take() else {
                    say!("425 Use PASV first");
                    continue;
                };
                ftp.retrs.fetch_add(1, SeqCst);
                ftp.rests.lock().unwrap().push(rest);
                say!("150 Opening BINARY mode data connection");
                let Ok(Ok((mut d, _))) =
                    tokio::time::timeout(Duration::from_secs(5), l.accept()).await
                else {
                    say!("425 Can't open data connection");
                    continue;
                };
                let start = (rest as usize).min(file.len());
                rest = 0;
                let mut ok = true;
                for chunk in file[start..].chunks(64 * 1024) {
                    if d.write_all(chunk).await.is_err() {
                        ok = false;
                        break;
                    }
                    if let Some(delay) = ftp.cfg.delay {
                        tokio::time::sleep(delay).await;
                    }
                }
                drop(d);
                if ok {
                    say!("226 Transfer complete");
                } else {
                    say!("426 Connection closed; transfer aborted");
                }
            }
            "ABOR" => say!("226 ABOR command successful"),
            "QUIT" => {
                say!("221 Goodbye");
                return;
            }
            _ => say!("502 Command not implemented"),
        }
    }
}

// ---------------------------------------------------------------- SOCKS5 proxy

async fn socks_proxy(count: Arc<AtomicUsize>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = l.accept().await {
            let count = count.clone();
            tokio::spawn(async move {
                let mut hello = [0u8; 2];
                c.read_exact(&mut hello).await.ok()?;
                let mut methods = vec![0u8; hello[1] as usize];
                c.read_exact(&mut methods).await.ok()?;
                c.write_all(&[5, 0]).await.ok()?;
                let mut head = [0u8; 4];
                c.read_exact(&mut head).await.ok()?;
                let host = match head[3] {
                    1 => {
                        let mut a = [0u8; 4];
                        c.read_exact(&mut a).await.ok()?;
                        std::net::Ipv4Addr::from(a).to_string()
                    }
                    3 => {
                        let mut n = [0u8; 1];
                        c.read_exact(&mut n).await.ok()?;
                        let mut name = vec![0u8; n[0] as usize];
                        c.read_exact(&mut name).await.ok()?;
                        String::from_utf8(name).ok()?
                    }
                    _ => return None,
                };
                let mut p = [0u8; 2];
                c.read_exact(&mut p).await.ok()?;
                let mut up = TcpStream::connect((host.as_str(), u16::from_be_bytes(p)))
                    .await
                    .ok()?;
                count.fetch_add(1, SeqCst);
                c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.ok()?;
                tokio::io::copy_bidirectional(&mut c, &mut up).await.ok()?;
                Some(())
            });
        }
    });
    port
}

// ---------------------------------------------------------------- helpers

async fn engine(dir: &Path, tweak: impl FnOnce(&mut Settings)) -> Engine {
    let e = Engine::open(dir.join("data")).unwrap();
    let mut s = e.settings();
    s.download_dir = dir.join("dl");
    s.use_category_folders = false;
    s.min_split_size = 64 * 1024;
    s.connections = 4;
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

fn assert_complete(d: &DownloadInfo, expected: &[u8]) {
    assert_eq!(
        d.status,
        Status::Completed,
        "download failed: {:?}",
        d.error
    );
    let got = std::fs::read(path_of(d)).unwrap();
    assert_eq!(got.len(), expected.len());
    assert!(got == expected, "content mismatch");
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn probe_reads_size_name_and_resume() {
    let srv = Server::start(Config::default()).await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let p = e
        .probe(&srv.url("pub/big%20file.bin"), &RequestMeta::default())
        .await
        .unwrap();
    assert_eq!(p.filename, "big file.bin");
    assert_eq!(p.total_size, Some(SIZE as u64));
    assert!(p.resumable);
    assert_eq!(p.last_modified.as_deref(), Some("20261122103000"));
}

#[tokio::test]
async fn segmented_download_over_several_connections() {
    let srv = Server::start(Config::default()).await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(&e, srv.url("pub/big%20file.bin"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &data(SIZE));
    assert_eq!(d.resumable, Some(true));
    assert!(srv.ftp.retrs.load(SeqCst) >= 2, "expected several ranges");
    assert!(srv.ftp.rests.lock().unwrap().iter().any(|&r| r > 0));
}

#[tokio::test]
async fn pasv_and_no_resume_still_work() {
    let srv = Server::start(Config {
        no_rest: true,
        no_epsv: true,
        ..Default::default()
    })
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(&e, srv.url("pub/big%20file.bin"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &data(SIZE));
    assert_eq!(d.resumable, Some(false));
    assert_eq!(srv.ftp.retrs.load(SeqCst), 1);
}

#[tokio::test]
async fn asks_for_a_login_then_uses_it() {
    let srv = Server::start(Config {
        login: Some(("me".into(), "secret".into())),
        ..Default::default()
    })
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(&e, srv.url("small.txt"));
    let d = wait_finished(&e, id).await;
    assert_eq!(d.status, Status::Failed);
    let req = d.needs_login.expect("a login request");
    assert_eq!(req.host, format!("ftp://127.0.0.1:{}", srv.port));
    assert!(!req.rejected);

    e.set_login(
        &req.host,
        Login {
            username: "me".into(),
            password: "wrong".into(),
        },
    )
    .unwrap();
    let d = wait_for(&e, id, |d| {
        d.status == Status::Failed && d.needs_login.as_ref().is_some_and(|l| l.rejected)
    })
    .await;
    assert!(d.error.unwrap().contains("didn't accept"));

    e.set_login(
        &req.host,
        Login {
            username: "me".into(),
            password: "secret".into(),
        },
    )
    .unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, b"hello over ftp\n");
}

#[tokio::test]
async fn login_in_the_url_is_used() {
    let srv = Server::start(Config {
        login: Some(("me".into(), "p@ss".into())),
        ..Default::default()
    })
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(
        &e,
        format!("ftp://me:p%40ss@127.0.0.1:{}/small.txt", srv.port),
    );
    let d = wait_finished(&e, id).await;
    assert_complete(&d, b"hello over ftp\n");
    assert!(
        !d.url.contains("p%40ss"),
        "the password stays out of the list"
    );
}

#[tokio::test]
async fn adapts_to_a_connection_limit() {
    let srv = Server::start(Config {
        max_sessions: Some(2),
        delay: Some(Duration::from_millis(2)),
        ..Default::default()
    })
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 6).await;
    let id = add(&e, srv.url("pub/big%20file.bin"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &data(SIZE));
    assert!(srv.ftp.peak.load(SeqCst) <= 2);
    assert!(srv.ftp.refused.load(SeqCst) >= 1);
}

#[tokio::test]
async fn pause_and_resume_continue_where_they_stopped() {
    let srv = Server::start(Config {
        delay: Some(Duration::from_millis(15)),
        ..Default::default()
    })
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| s.connections = 2).await;
    let id = add(&e, srv.url("pub/big%20file.bin"));
    wait_for(&e, id, |d| d.downloaded > 512 * 1024).await;
    e.pause(id).unwrap();
    let paused = wait_for(&e, id, |d| d.status == Status::Paused).await;
    let before = srv.ftp.retrs.load(SeqCst);
    e.resume(id).unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &data(SIZE));
    assert!(paused.downloaded > 0);
    let rests = srv.ftp.rests.lock().unwrap().clone();
    assert!(
        rests[before..].iter().all(|&r| r > 0),
        "resumed ranges start where they stopped: {rests:?}"
    );
}

#[tokio::test]
async fn missing_file_and_folder_fail_clearly() {
    let srv = Server::start(Config::default()).await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let err = e
        .probe(&srv.url("nope.bin"), &RequestMeta::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("isn't on the FTP server"), "{err}");
    assert!(err.contains("550"), "{err}");
    let err = e
        .probe(&srv.url("pub"), &RequestMeta::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("folder"), "{err}");
}

#[tokio::test]
async fn goes_through_a_socks_proxy() {
    let srv = Server::start(Config::default()).await;
    let count = Arc::new(AtomicUsize::new(0));
    let proxy = socks_proxy(count.clone()).await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |s| {
        s.proxy.mode = ProxyMode::Manual;
        s.proxy.scheme = ProxyScheme::Socks5;
        s.proxy.host = "127.0.0.1".into();
        s.proxy.port = proxy;
        s.proxy.no_proxy = String::new();
    })
    .await;
    let id = add(&e, srv.url("pub/big%20file.bin"));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &data(SIZE));
    // Control and data connections alike.
    assert!(count.load(SeqCst) >= 2 * srv.ftp.retrs.load(SeqCst));
}
