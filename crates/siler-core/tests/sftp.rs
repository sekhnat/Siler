//! SFTP downloads against an in-process SSH server: host keys (unknown, trusted, changed),
//! password and key-file logins, and ranged downloads over several connections.
//!
//! The tests run with a fake HOME (holding only the test client key) and without an SSH
//! agent, so nothing of the real user's `~/.ssh` is read.

use russh::server::{Auth, Msg, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};
use siler_core::{
    DownloadInfo, Engine, HostKeyRequest, Login, NewDownload, Settings, Status as St,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

const SIZE: usize = 2 * 1024 * 1024 + 77;
const USER: &str = "me";
const PASS: &str = "secret";
/// The user whose public key (tests/sftp/client) the server accepts.
const KEY_USER: &str = "keyuser";

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 13 % 241) as u8).collect()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/sftp")
        .join(name)
}

/// A fake HOME with only the test client key, and no SSH agent. Set once, before any test
/// connects.
fn fake_home() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = tempfile::tempdir().unwrap().keep();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::copy(fixture("client"), home.join(".ssh/id_ed25519")).unwrap();
        // SAFETY: done once, before the tests in this binary start any engine.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::remove_var("SSH_AUTH_SOCK");
        }
    });
}

// ---------------------------------------------------------------- test server

#[derive(Clone)]
struct Server {
    files: Arc<HashMap<String, Vec<u8>>>,
    opens: Arc<AtomicUsize>,
}

impl russh::server::Server for Server {
    type Handler = Ssh;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Ssh {
        Ssh {
            srv: self.clone(),
            channels: HashMap::new(),
            user: String::new(),
        }
    }
}

struct Ssh {
    srv: Server,
    channels: HashMap<ChannelId, Channel<Msg>>,
    user: String,
}

impl russh::server::Handler for Ssh {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == USER && password == PASS {
            self.user = user.into();
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        key: russh::server::PublicKeyOffer<'_>,
    ) -> Result<Auth, Self::Error> {
        Ok(
            if user == KEY_USER && key.public_key.key_data() == client_key().key_data() {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        Ok(
            if user == KEY_USER && key.public_key.key_data() == client_key().key_data() {
                self.user = user.into();
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.close(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        match (name, self.channels.remove(&id)) {
            ("sftp", Some(channel)) => {
                session.channel_success(id)?;
                let sftp = Sftp {
                    srv: self.srv.clone(),
                };
                tokio::spawn(russh_sftp::server::run(channel.into_stream(), sftp));
            }
            _ => session.channel_failure(id)?,
        }
        Ok(())
    }
}

struct Sftp {
    srv: Server,
}

fn attrs(len: usize) -> FileAttributes {
    FileAttributes {
        size: Some(len as u64),
        mtime: Some(1_790_000_000),
        permissions: Some(0o100644),
        ..Default::default()
    }
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
    }
}

impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }

    async fn open(
        &mut self,
        id: u32,
        path: String,
        _: OpenFlags,
        _: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        if !self.srv.files.contains_key(&path) {
            return Err(StatusCode::NoSuchFile);
        }
        self.srv.opens.fetch_add(1, SeqCst);
        Ok(Handle { id, handle: path })
    }

    async fn close(&mut self, id: u32, _: String) -> Result<Status, StatusCode> {
        Ok(ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusCode> {
        let f = self.srv.files.get(&handle).ok_or(StatusCode::NoSuchFile)?;
        let start = offset as usize;
        if start >= f.len() {
            return Err(StatusCode::Eof);
        }
        let end = (start + len as usize).min(f.len());
        Ok(Data {
            id,
            data: f[start..end].to_vec(),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        if path == "/srv" {
            let attrs = FileAttributes {
                permissions: Some(0o040755),
                ..Default::default()
            };
            return Ok(Attrs { id, attrs });
        }
        let f = self.srv.files.get(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(Attrs {
            id,
            attrs: attrs(f.len()),
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.stat(id, path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
        self.stat(id, handle).await
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        Ok(Name {
            id,
            files: vec![File::dummy(path)],
        })
    }
}

fn client_key() -> russh::keys::PublicKey {
    let text = std::fs::read_to_string(fixture("client.pub")).unwrap();
    russh::keys::PublicKey::from_openssh(text.trim()).unwrap()
}

struct Running {
    port: u16,
    opens: Arc<AtomicUsize>,
}

async fn start(host_key: &str) -> Running {
    let mut files = HashMap::new();
    files.insert("/srv/data.bin".to_string(), data(SIZE));
    let opens = Arc::new(AtomicUsize::new(0));
    let mut srv = Server {
        files: Arc::new(files),
        opens: opens.clone(),
    };
    let key = russh::keys::load_secret_key(fixture(host_key), None).unwrap();
    let config = Arc::new(russh::server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::from_millis(10),
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = srv.run_on_socket(config, &listener).await;
    });
    Running { port, opens }
}

// ---------------------------------------------------------------- helpers

async fn engine(dir: &Path, tweak: impl FnOnce(&mut Settings)) -> Engine {
    fake_home();
    let e = Engine::open(dir.join("data")).unwrap();
    let mut s = e.settings();
    s.download_dir = dir.join("dl");
    s.use_category_folders = false;
    s.min_split_size = 64 * 1024;
    s.connections = 3;
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
    wait_for(e, id, |d| matches!(d.status, St::Completed | St::Failed)).await
}

fn assert_complete(d: &DownloadInfo, expected: &[u8]) {
    assert_eq!(d.status, St::Completed, "download failed: {:?}", d.error);
    let got = std::fs::read(d.dir.as_ref().unwrap().join(d.filename.as_ref().unwrap())).unwrap();
    assert!(got == expected, "content mismatch");
}

/// Adds a download that stops at the unknown host key, trusts it, and returns the request.
async fn trust_first(e: &Engine, id: i64) -> HostKeyRequest {
    let d = wait_finished(e, id).await;
    assert_eq!(d.status, St::Failed);
    let req = d
        .needs_host_key
        .clone()
        .expect("asks to trust the server key");
    assert!(req.fingerprint.starts_with("SHA256:"), "{req:?}");
    assert_eq!(req.algorithm, "ssh-ed25519");
    e.trust_host_key(&req).unwrap();
    req
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn unknown_key_is_trusted_then_password_login_downloads() {
    let srv = start("host_a").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(
        &e,
        format!("sftp://{USER}:{PASS}@127.0.0.1:{}/srv/data.bin", srv.port),
    );
    let req = trust_first(&e, id).await;
    assert_eq!(req.host, format!("sftp://127.0.0.1:{}", srv.port));
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &data(SIZE));
    assert!(srv.opens.load(SeqCst) >= 2, "several ranges were fetched");
    let list = std::fs::read_to_string(tmp.path().join("data/known_hosts")).unwrap();
    assert!(list.contains("ssh-ed25519"));
}

#[tokio::test]
async fn key_file_login() {
    let srv = start("host_a").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(
        &e,
        format!("sftp://{KEY_USER}@127.0.0.1:{}/srv/data.bin", srv.port),
    );
    trust_first(&e, id).await;
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &data(SIZE));
}

#[tokio::test]
async fn wrong_password_asks_for_a_login() {
    let srv = start("host_a").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(
        &e,
        format!("sftp://{USER}:nope@127.0.0.1:{}/srv/data.bin", srv.port),
    );
    trust_first(&e, id).await;
    let d = wait_for(&e, id, |d| {
        d.status == St::Failed && d.needs_login.is_some()
    })
    .await;
    let req = d.needs_login.unwrap();
    assert!(req.rejected);
    e.set_login(
        &req.host,
        Login {
            username: USER.into(),
            password: PASS.into(),
        },
    )
    .unwrap();
    let d = wait_finished(&e, id).await;
    assert_complete(&d, &data(SIZE));
}

#[tokio::test]
async fn changed_key_is_refused() {
    let srv = start("host_a").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    // Pretend another key was trusted for this server before.
    let other = std::fs::read_to_string(fixture("client.pub")).unwrap();
    e.trust_host_key(&HostKeyRequest {
        host: format!("sftp://127.0.0.1:{}", srv.port),
        server: "127.0.0.1".into(),
        port: srv.port,
        algorithm: "ssh-ed25519".into(),
        fingerprint: String::new(),
        key: other.trim().to_string(),
    })
    .unwrap();
    let id = add(
        &e,
        format!("sftp://{USER}:{PASS}@127.0.0.1:{}/srv/data.bin", srv.port),
    );
    let d = wait_finished(&e, id).await;
    assert_eq!(d.status, St::Failed);
    assert!(d.needs_host_key.is_none());
    assert!(d.error.unwrap().contains("changed"));
}

#[tokio::test]
async fn missing_file_and_folder() {
    let srv = start("host_b").await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), |_| {}).await;
    let id = add(
        &e,
        format!("sftp://{USER}:{PASS}@127.0.0.1:{}/srv/nope.bin", srv.port),
    );
    trust_first(&e, id).await;
    let d = wait_finished(&e, id).await;
    assert_eq!(d.status, St::Failed);
    assert!(d.error.unwrap().contains("isn't on the server"));
    let err = e
        .probe(
            &format!("sftp://{USER}:{PASS}@127.0.0.1:{}/srv", srv.port),
            &Default::default(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("folder"), "{err}");
}

/// An HTTP proxy that only tunnels.
async fn connect_proxy(count: Arc<AtomicUsize>) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut from, _)) = listener.accept().await {
            let count = count.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(from.read_u8().await.ok()?);
                }
                let head = String::from_utf8(head).ok()?;
                let to = head.strip_prefix("CONNECT ")?.split(' ').next()?;
                let mut up = tokio::net::TcpStream::connect(to).await.ok()?;
                count.fetch_add(1, SeqCst);
                from.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.ok()?;
                tokio::io::copy_bidirectional(&mut from, &mut up).await.ok()
            });
        }
    });
    port
}

#[tokio::test]
async fn goes_where_a_script_says() {
    let srv = start("host_a").await;
    let count = Arc::new(AtomicUsize::new(0));
    let proxy = connect_proxy(count.clone()).await;
    let tmp = tempfile::tempdir().unwrap();
    let pac = tmp.path().join("proxy.pac");
    let body = format!(
        "function FindProxyForURL(url) {{ return /^sftp:/.test(url) ? 'PROXY 127.0.0.1:{proxy}' : 'DIRECT'; }}"
    );
    std::fs::write(&pac, body).unwrap();
    let e = engine(tmp.path(), |s| {
        s.proxy.mode = siler_core::proxy::ProxyMode::Script;
        s.proxy.script = pac.display().to_string();
    })
    .await;
    let helper = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap();
    e.set_pac_helper(Some(helper.join("debug/siler-pac")));
    let id = add(
        &e,
        format!("sftp://{USER}:{PASS}@0.0.0.0:{}/srv/data.bin", srv.port),
    );
    trust_first(&e, id).await;
    assert_complete(&wait_finished(&e, id).await, &data(SIZE));
    assert!(count.load(SeqCst) > 0, "the script's proxy wasn't used");
}
