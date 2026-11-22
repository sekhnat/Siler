//! SFTP over SSH (russh). The server's key is checked against `~/.ssh/known_hosts` and
//! Siler's own list; an unknown key stops the download until the user trusts it, and a
//! changed key is refused.

use bytes::Bytes;
use russh::client::{self, AuthResult, Handle};
use russh::keys::{self, HashAlg, PrivateKeyWithHashAlg, PublicKey};
use russh_sftp::client::SftpSession;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use url::Url;

use super::{ByteStream, OpenError, decoded_path, login_key};
use crate::net::Net;
use crate::probe::ProbeResult;
use crate::types::HostKeyRequest;
use crate::{Category, Error, Result, filename};

const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// What the key check found, for the error after a refused connection.
#[derive(Clone)]
enum KeyCheck {
    Trusted,
    Unknown(PublicKey),
    Changed,
}

struct Checker {
    host: String,
    port: u16,
    lists: Vec<PathBuf>,
    found: Arc<Mutex<Option<KeyCheck>>>,
}

impl client::Handler for Checker {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let key = match key {
            keys::PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            keys::PublicKeyOrCertificate::Certificate(c) => {
                PublicKey::new(c.public_key().clone(), "")
            }
        };
        let check = check_key(&self.lists, &self.host, self.port, &key);
        let ok = matches!(check, KeyCheck::Trusted);
        *self.found.lock().unwrap() = Some(check);
        Ok(ok)
    }
}

fn check_key(lists: &[PathBuf], host: &str, port: u16, key: &PublicKey) -> KeyCheck {
    let mut trusted = false;
    for list in lists.iter().filter(|p| p.exists()) {
        match keys::check_known_hosts_path(host, port, key, list) {
            Ok(true) => trusted = true,
            Ok(false) => {}
            Err(keys::Error::KeyChanged { .. }) => return KeyCheck::Changed,
            Err(e) => tracing::debug!(list = %list.display(), "known_hosts: {e}"),
        }
    }
    if trusted {
        KeyCheck::Trusted
    } else {
        KeyCheck::Unknown(key.clone())
    }
}

/// `ssh-ed25519 AAAA…` of a key, as stored in known_hosts.
fn openssh(key: &PublicKey) -> String {
    key.to_openssh().unwrap_or_default()
}

pub(crate) fn host_key_request(
    host: &str,
    port: u16,
    key: &PublicKey,
    login: &str,
) -> HostKeyRequest {
    HostKeyRequest {
        host: login.to_string(),
        server: host.to_string(),
        port,
        algorithm: key.algorithm().to_string(),
        fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
        key: openssh(key),
    }
}

/// Adds a key the user trusted to Siler's own list.
pub(crate) fn trust(list: &std::path::Path, req: &HostKeyRequest) -> Result<()> {
    let key = PublicKey::from_openssh(&req.key)
        .map_err(|e| Error::Protocol(format!("The server key can't be read: {e}")))?;
    if let Some(dir) = list.parent() {
        std::fs::create_dir_all(dir)?;
    }
    keys::known_hosts::learn_known_hosts_path(&req.server, req.port, &key, list)
        .map_err(|e| Error::Protocol(format!("The server key can't be saved: {e}")))
}

/// The path to open: absolute, or relative to the home folder for `sftp://h/~/f`.
fn sftp_path(url: &Url) -> String {
    let p = decoded_path(url);
    match p.strip_prefix("/~/") {
        Some(rel) => rel.to_string(),
        None => p,
    }
}

struct Session {
    sftp: SftpSession,
    /// Keeps the SSH connection open while the SFTP session is used.
    _ssh: Handle<Checker>,
}

fn net_error(e: impl std::fmt::Display) -> OpenError {
    OpenError::Transient(e.to_string())
}

async fn connect(net: &Net, url: &Url) -> std::result::Result<Session, OpenError> {
    let host = url
        .host_str()
        .ok_or_else(|| OpenError::Fatal(Error::InvalidUrl("no server name".into())))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = url.port().unwrap_or(22);
    let key = login_key(url);
    let mut lists = Vec::new();
    if let Some(home) = dirs::home_dir() {
        lists.push(home.join(".ssh").join("known_hosts"));
    }
    if let Some(own) = net.known_hosts.read().unwrap().clone() {
        lists.push(own);
    }
    let found = Arc::new(Mutex::new(None));
    let checker = Checker {
        host: host.clone(),
        port,
        lists,
        found: found.clone(),
    };
    let tcp = net
        .dial(&host, port)
        .await
        .map_err(|e| OpenError::Transient(format!("can't connect to {host}: {e}")))?;
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(120)),
        ..Default::default()
    });
    let mut ssh = match client::connect_stream(config, tcp, checker).await {
        Ok(h) => h,
        Err(e) => {
            return Err(match found.lock().unwrap().clone() {
                Some(KeyCheck::Unknown(k)) => OpenError::Fatal(Error::HostKeyUnknown(Box::new(
                    host_key_request(&host, port, &k, &key),
                ))),
                Some(KeyCheck::Changed) => OpenError::Fatal(Error::HostKeyChanged(key.clone())),
                _ => net_error(e),
            });
        }
    };

    let login = net.login_for(url);
    let user = login
        .as_ref()
        .map(|l| l.username.clone())
        .filter(|u| !u.is_empty())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "root".into());
    let mut ok = false;
    if let Some(pass) = login
        .as_ref()
        .map(|l| l.password.as_str())
        .filter(|p| !p.is_empty())
    {
        ok = ssh
            .authenticate_password(&user, pass)
            .await
            .map_err(net_error)?
            .success();
    }
    if !ok {
        ok = by_agent(&mut ssh, &user).await;
    }
    if !ok {
        ok = by_key_files(&mut ssh, &user).await;
    }
    if !ok {
        return Err(OpenError::Fatal(Error::AuthRequired {
            host: key,
            realm: None,
            rejected: login.is_some_and(|l| !l.password.is_empty()),
        }));
    }
    let channel = ssh.channel_open_session().await.map_err(net_error)?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(net_error)?;
    let sftp = SftpSession::new(channel.into_stream()).await.map_err(|e| {
        OpenError::Fatal(Error::Protocol(format!(
            "The server doesn't offer SFTP: {e}"
        )))
    })?;
    Ok(Session { sftp, _ssh: ssh })
}

/// Keys held by the desktop's SSH agent (`SSH_AUTH_SOCK`).
async fn by_agent(ssh: &mut Handle<Checker>, user: &str) -> bool {
    let Ok(mut agent) = keys::agent::client::AgentClient::connect_env().await else {
        return false;
    };
    let Ok(ids) = agent.request_identities().await else {
        return false;
    };
    let hash = ssh.best_supported_rsa_hash().await.ok().flatten().flatten();
    for id in ids {
        let key = id.public_key().into_owned();
        if let Ok(AuthResult::Success) = ssh
            .authenticate_publickey_with(user, key, hash, &mut agent)
            .await
        {
            return true;
        }
    }
    false
}

/// `~/.ssh/id_*` keys without a passphrase (one with a passphrase is skipped: there is
/// nowhere to type it).
async fn by_key_files(ssh: &mut Handle<Checker>, user: &str) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let hash = ssh.best_supported_rsa_hash().await.ok().flatten().flatten();
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        let path = home.join(".ssh").join(name);
        if !path.exists() {
            continue;
        }
        let key = match keys::load_secret_key(&path, None) {
            Ok(k) => k,
            Err(e) => {
                tracing::info!("SSH key {name} skipped: {e}");
                continue;
            }
        };
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
        if let Ok(AuthResult::Success) = ssh.authenticate_publickey(user, key).await {
            return true;
        }
    }
    false
}

fn to_error(e: OpenError) -> Error {
    match e {
        OpenError::Fatal(e) => e,
        OpenError::Transient(m) => Error::Network(m),
        OpenError::Throttled(c, a) => Error::Busy(c, a),
    }
}

fn sftp_error(e: russh_sftp::client::error::Error) -> Error {
    let m = e.to_string();
    let lower = m.to_ascii_lowercase();
    if lower.contains("no such file") {
        Error::Protocol("The file isn't on the server".into())
    } else if lower.contains("permission denied") {
        Error::Protocol("The server doesn't allow reading this file".into())
    } else {
        Error::Protocol(format!("SFTP: {m}"))
    }
}

pub(crate) async fn probe(net: &Net, url: &Url) -> Result<ProbeResult> {
    let s = connect(net, url).await.map_err(to_error)?;
    let path = sftp_path(url);
    let meta = s.sftp.metadata(&path).await.map_err(sftp_error)?;
    if meta.is_dir() {
        return Err(Error::Protocol("This link is a folder, not a file.".into()));
    }
    let _ = s.sftp.close().await;
    let name = path
        .rsplit('/')
        .find(|s| !s.is_empty())
        .and_then(filename::sanitize)
        .unwrap_or_else(|| "download".to_string());
    let content_type = mime_guess::from_path(&name).first().map(|m| m.to_string());
    Ok(ProbeResult {
        final_url: url.to_string(),
        total_size: meta.size,
        resumable: meta.size.is_some(),
        etag: None,
        last_modified: meta.mtime.map(|t| t.to_string()),
        category: Category::from_filename(&name),
        filename: name,
        content_type,
        dir: PathBuf::new(),
        exists: false,
        suggested: String::new(),
        duplicates: Vec::new(),
        digest: None,
    })
}

struct Reading {
    file: russh_sftp::client::fs::File,
    remaining: Option<u64>,
    buf: Vec<u8>,
    _session: Session,
}

pub(crate) async fn open_range(
    net: &Net,
    url: &Url,
    start: u64,
    end: Option<u64>,
) -> std::result::Result<ByteStream, OpenError> {
    let session = connect(net, url).await?;
    let mut file = session
        .sftp
        .open(sftp_path(url))
        .await
        .map_err(|e| OpenError::Fatal(sftp_error(e)))?;
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(net_error)?;
    }
    let state = Reading {
        file,
        remaining: end.map(|e| e.saturating_sub(start)),
        buf: vec![0u8; 64 * 1024],
        _session: session,
    };
    Ok(Box::pin(futures_util::stream::unfold(
        Some(state),
        |s| async move {
            let mut s = s?;
            if s.remaining == Some(0) {
                return None;
            }
            let want = s
                .remaining
                .map(|r| r.min(s.buf.len() as u64) as usize)
                .unwrap_or(s.buf.len());
            match tokio::time::timeout(READ_TIMEOUT, s.file.read(&mut s.buf[..want])).await {
                Err(_) => Some((Err("the server stopped sending".to_string()), None)),
                Ok(Err(e)) => Some((Err(e.to_string()), None)),
                Ok(Ok(0)) => {
                    if s.remaining.is_some_and(|r| r > 0) {
                        Some((Err("connection closed early".to_string()), None))
                    } else {
                        None
                    }
                }
                Ok(Ok(n)) => {
                    if let Some(r) = s.remaining.as_mut() {
                        *r -= n as u64;
                    }
                    let chunk = Bytes::copy_from_slice(&s.buf[..n]);
                    Some((Ok(chunk), Some(s)))
                }
            }
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert_eq!(sftp_path(&u("sftp://h/srv/a%20b.iso")), "/srv/a b.iso");
        assert_eq!(sftp_path(&u("sftp://h/~/notes.txt")), "notes.txt");
    }
}
