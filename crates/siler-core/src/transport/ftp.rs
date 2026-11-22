//! A small FTP client: just what downloading needs (log in, size, date, resume, passive
//! retrieve), with explicit TLS for `ftps://`. Every connection, data ones included, goes
//! through [`Net::dial`], so a SOCKS or HTTP proxy carries the whole session.

use bytes::Bytes;
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf,
};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use url::Url;

use super::dial::Stream;
use super::{ByteStream, OpenError, decoded_path, login_key};
use crate::net::Net;
use crate::probe::ProbeResult;
use crate::{Category, Error, Result, filename};

/// A control or data connection: plain TCP or TLS over it. `Closed` stands in for a
/// connection that was dropped (reads end, writes fail).
pub(crate) enum Conn {
    Plain(Stream),
    Tls(Box<tokio_rustls::client::TlsStream<Stream>>),
    Closed,
}

impl AsyncRead for Conn {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
            Conn::Closed => Poll::Ready(Ok(())),
        }
    }
}

impl AsyncWrite for Conn {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, b: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_write(cx, b),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, b),
            Conn::Closed => Poll::Ready(Err(io::ErrorKind::NotConnected.into())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_flush(cx),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
            Conn::Closed => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
            Conn::Closed => Poll::Ready(Ok(())),
        }
    }
}

const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// How long an idle control connection is kept for the next range.
const IDLE: Duration = Duration::from_secs(30);
const ANONYMOUS: (&str, &str) = ("anonymous", "siler@");

/// Idle logged-in control connections, by server and user.
#[derive(Clone, Default)]
pub(crate) struct Pool(Arc<Mutex<HashMap<String, Vec<Idle>>>>);

/// A control connection waiting in the pool, and since when.
type Idle = (Instant, Control);

impl Pool {
    fn take(&self, key: &str) -> Option<Control> {
        let mut map = self.0.lock().unwrap();
        let list = map.get_mut(key)?;
        while let Some((at, c)) = list.pop() {
            if at.elapsed() < IDLE {
                return Some(c);
            }
        }
        None
    }

    /// Forgets them all: they were made the way the old proxy settings said.
    pub(crate) fn clear(&self) {
        self.0.lock().unwrap().clear();
    }

    fn put(&self, c: Control) {
        let mut map = self.0.lock().unwrap();
        let list = map.entry(c.key.clone()).or_default();
        list.retain(|(at, _)| at.elapsed() < IDLE);
        if list.len() < 8 {
            list.push((Instant::now(), c));
        }
    }
}

#[derive(Debug, Default, Clone)]
struct Features {
    rest_stream: bool,
    size: bool,
    mdtm: bool,
    utf8: bool,
    /// EPSV failed once on this server: use PASV.
    no_epsv: bool,
}

/// A logged-in control connection.
pub(crate) struct Control {
    io: BufReader<Conn>,
    key: String,
    /// Where data connections go too: the server, or the FTP proxy.
    host: String,
    /// What the route is chosen for; `None` through an FTP proxy, which is reached directly.
    route: Option<Url>,
    tls: bool,
    features: Features,
}

/// One server reply: its code and text (all lines of a multi-line reply).
#[derive(Debug)]
struct Reply {
    code: u16,
    text: String,
}

impl Reply {
    fn class(&self) -> u16 {
        self.code / 100
    }

    /// The last line's message, for the user.
    fn message(&self) -> String {
        let last = self.text.lines().last().unwrap_or("").trim();
        last.get(4..).unwrap_or(last).trim().to_string()
    }
}

fn net_err(e: io::Error) -> Error {
    Error::Network(e.to_string())
}

impl Control {
    async fn read_reply(&mut self) -> io::Result<Reply> {
        let mut text = String::new();
        let mut first: Option<String> = None;
        loop {
            let mut line = String::new();
            let n = tokio::time::timeout(READ_TIMEOUT, self.io.read_line(&mut line))
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "the server stopped answering")
                })??;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the server closed the connection",
                ));
            }
            if line.len() > 8192 || text.len() > 64 * 1024 {
                return Err(io::Error::other("the server's answer is too long"));
            }
            text.push_str(&line);
            let code = line
                .get(..3)
                .filter(|c| c.bytes().all(|b| b.is_ascii_digit()));
            match (&first, code) {
                (None, Some(c)) => {
                    if line.as_bytes().get(3) == Some(&b'-') {
                        first = Some(c.to_string());
                    } else {
                        return Ok(Reply {
                            code: c.parse().unwrap_or(0),
                            text,
                        });
                    }
                }
                (Some(f), Some(c)) if c == f && line.as_bytes().get(3) == Some(&b' ') => {
                    return Ok(Reply {
                        code: c.parse().unwrap_or(0),
                        text,
                    });
                }
                (None, None) => {
                    return Err(io::Error::other("this doesn't look like an FTP server"));
                }
                _ => {}
            }
        }
    }

    async fn send(&mut self, line: &str) -> io::Result<()> {
        if line.contains(['\r', '\n']) {
            return Err(io::Error::other("a line break in an FTP command"));
        }
        let w = self.io.get_mut();
        w.write_all(format!("{line}\r\n").as_bytes()).await?;
        w.flush().await
    }

    async fn cmd(&mut self, line: &str) -> io::Result<Reply> {
        self.send(line).await?;
        self.read_reply().await
    }

    /// Opens a passive data connection.
    async fn data(&mut self, net: &Net) -> std::result::Result<Stream, OpenError> {
        let port = loop {
            if !self.features.no_epsv {
                let r = self.cmd("EPSV").await.map_err(transient)?;
                if r.code == 229
                    && let Some(p) = parse_epsv(&r.text)
                {
                    break p;
                }
                self.features.no_epsv = true;
                continue;
            }
            let r = self.cmd("PASV").await.map_err(transient)?;
            if r.code != 227 {
                return Err(refused(&r));
            }
            // The address in the answer is ignored: behind NAT it is often a private one.
            // The data connection goes to the same host as the control connection.
            break parse_pasv(&r.text).ok_or_else(|| {
                OpenError::Fatal(Error::Protocol(
                    "The FTP server's PASV answer can't be read".into(),
                ))
            })?;
        };
        let data = match &self.route {
            Some(url) => net.dial(url, &self.host, port).await,
            None => net.dial_direct(&self.host, port).await,
        };
        data.map_err(OpenError::from)
    }
}

fn transient(e: io::Error) -> OpenError {
    OpenError::Transient(e.to_string())
}

/// An error for a reply the server shouldn't have sent at this point.
fn refused(r: &Reply) -> OpenError {
    match r.code {
        421 => OpenError::Throttled(421, None),
        425 | 426 | 450 | 451 => OpenError::Transient(format!("FTP {}: {}", r.code, r.message())),
        _ if too_many(r) => OpenError::Throttled(r.code, None),
        _ => OpenError::Fatal(Error::Ftp(r.code, r.message())),
    }
}

/// Some servers refuse extra connections with a 530 and words instead of 421.
fn too_many(r: &Reply) -> bool {
    let t = r.text.to_ascii_lowercase();
    r.code == 421
        || [
            "too many",
            "maximum number",
            "connection limit",
            "clients from your",
        ]
        .iter()
        .any(|w| t.contains(w))
}

/// `229 Entering Extended Passive Mode (|||6446|)`.
fn parse_epsv(text: &str) -> Option<u16> {
    let inner = text.split('(').nth(1)?.split(')').next()?;
    let delim = inner.chars().next()?;
    inner.split(delim).nth(3)?.parse().ok()
}

/// `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)`; some servers leave out the parentheses.
fn parse_pasv(text: &str) -> Option<u16> {
    let nums: Vec<u16> = text
        .get(4..)?
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() < 6 {
        return None;
    }
    let p = &nums[nums.len() - 6..];
    if p.iter().any(|&n| n > 255) {
        return None;
    }
    Some(p[4] * 256 + p[5])
}

/// `213 20261122103000[.123]` as a timestamp string used to notice a changed file.
fn parse_mdtm(r: &Reply) -> Option<String> {
    let v = r.message();
    let digits: String = v.chars().take_while(|c| c.is_ascii_digit()).collect();
    (digits.len() == 14).then_some(digits)
}

async fn tls_wrap(host: &str, s: Stream) -> std::result::Result<Conn, OpenError> {
    let config = super::dial::tls_config().map_err(OpenError::Fatal)?;
    let name = ServerName::try_from(host.to_string()).map_err(|e| {
        OpenError::Fatal(Error::Protocol(format!("Invalid server name for TLS: {e}")))
    })?;
    // One shared config, so data connections resume the control connection's TLS session
    // (vsftpd and others require that).
    let tls = TlsConnector::from(config)
        .connect(name, s)
        .await
        .map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidData {
                OpenError::Fatal(Error::Protocol(format!(
                    "The server's TLS certificate isn't trusted: {e}"
                )))
            } else {
                transient(e)
            }
        })?;
    Ok(Conn::Tls(Box::new(tls)))
}

fn port_of(url: &Url) -> u16 {
    url.port().unwrap_or(21)
}

/// The path to send: relative to the login folder (RFC 1738), so `ftp://h/pub/f` is
/// `pub/f`, and `ftp://h/%2Fetc/f` is the absolute `/etc/f`.
fn ftp_path(url: &Url) -> String {
    let p = decoded_path(url);
    p.strip_prefix('/').unwrap_or(&p).to_string()
}

/// Connects and logs in.
async fn connect(net: &Net, url: &Url) -> std::result::Result<Control, OpenError> {
    let host = url
        .host_str()
        .ok_or_else(|| OpenError::Fatal(Error::InvalidUrl("no server name".into())))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let tls = url.scheme() == "ftps";
    let login = net.login_for(url);
    let key = format!(
        "{}\n{}",
        login_key(url),
        login.as_ref().map(|l| l.username.as_str()).unwrap_or("")
    );
    let proxy = net.router.ftp_proxy(url);
    let tcp = match &proxy {
        Some((at, port)) => net.dial_direct(at, *port).await,
        None => net.dial(url, &host, port_of(url)).await,
    }
    .map_err(|e| match e {
        Error::Network(why) => OpenError::Transient(format!("can't connect to {host}: {why}")),
        e => OpenError::from(e),
    })?;
    let mut c = Control {
        io: BufReader::new(Conn::Plain(tcp)),
        key,
        host: proxy.as_ref().map_or(&host, |(at, _)| at).clone(),
        route: proxy.is_none().then(|| url.clone()),
        tls,
        features: Features::default(),
    };
    let mut greet = c.read_reply().await.map_err(transient)?;
    while greet.code == 120 {
        greet = c.read_reply().await.map_err(transient)?;
    }
    if greet.class() != 2 {
        return Err(refused(&greet));
    }
    if tls {
        let r = c.cmd("AUTH TLS").await.map_err(transient)?;
        if r.code != 234 {
            return Err(OpenError::Fatal(Error::Protocol(format!(
                "The FTP server doesn't offer TLS ({}). Use ftp:// to connect without encryption.",
                r.message()
            ))));
        }
        // Nothing follows 234 until the handshake, so the buffer is empty.
        let Conn::Plain(tcp) = std::mem::replace(c.io.get_mut(), Conn::Closed) else {
            return Err(OpenError::Fatal(Error::Protocol(
                "TLS upgrade failed".into(),
            )));
        };
        c.io = BufReader::new(tls_wrap(&host, tcp).await?);
    }

    let (user, pass) = match &login {
        Some(l) => (l.username.as_str(), l.password.as_str()),
        None => ANONYMOUS,
    };
    let auth_error = |r: &Reply| {
        if too_many(r) {
            return OpenError::Throttled(r.code, None);
        }
        OpenError::Fatal(Error::AuthRequired {
            host: login_key(url),
            realm: Some(r.message()).filter(|m| !m.is_empty()),
            rejected: login.is_some(),
            asked: crate::Asked::Password,
        })
    };
    let user = match &proxy {
        // An FTP proxy takes the server's name with the user's.
        Some(_) => {
            let at = url.host_str().unwrap_or_default();
            match port_of(url) {
                21 => format!("{user}@{at}"),
                port => format!("{user}@{at}:{port}"),
            }
        }
        None => user.to_string(),
    };
    let mut r = c.cmd(&format!("USER {user}")).await.map_err(transient)?;
    if r.code == 331 {
        r = c.cmd(&format!("PASS {pass}")).await.map_err(transient)?;
    }
    if r.code == 332 {
        return Err(OpenError::Fatal(Error::Protocol(
            "The FTP server asks for an account, which Siler doesn't support".into(),
        )));
    }
    match r.code {
        230 | 202 => {}
        421 => return Err(OpenError::Throttled(421, None)),
        530 | 331 | 430 => return Err(auth_error(&r)),
        _ => return Err(refused(&r)),
    }
    if tls {
        c.cmd("PBSZ 0").await.map_err(transient)?;
        let r = c.cmd("PROT P").await.map_err(transient)?;
        if r.class() != 2 {
            return Err(refused(&r));
        }
    }
    let r = c.cmd("FEAT").await.map_err(transient)?;
    if r.code == 211 {
        for line in r.text.lines() {
            let f = line.trim().to_ascii_uppercase();
            c.features.rest_stream |= f.starts_with("REST STREAM");
            c.features.size |= f == "SIZE";
            c.features.mdtm |= f == "MDTM";
            c.features.utf8 |= f == "UTF8";
        }
    }
    if c.features.utf8 {
        let _ = c.cmd("OPTS UTF8 ON").await;
    }
    let r = c.cmd("TYPE I").await.map_err(transient)?;
    if r.class() != 2 {
        return Err(refused(&r));
    }
    Ok(c)
}

fn to_error(e: OpenError) -> Error {
    match e {
        OpenError::Fatal(e) => e,
        OpenError::Transient(m) => Error::Network(m),
        OpenError::Throttled(c, after) => Error::Busy(c, after),
    }
}

/// A pooled connection when there is one (checked with a NOOP), otherwise a new one.
async fn control(net: &Net, url: &Url) -> std::result::Result<Control, OpenError> {
    let login = net.login_for(url);
    let key = format!(
        "{}\n{}",
        login_key(url),
        login.as_ref().map(|l| l.username.as_str()).unwrap_or("")
    );
    while let Some(mut c) = net.ftp_pool.take(&key) {
        if matches!(c.cmd("NOOP").await, Ok(r) if r.class() == 2) {
            return Ok(c);
        }
    }
    connect(net, url).await
}

pub(crate) async fn probe(net: &Net, url: &Url) -> Result<ProbeResult> {
    let mut c = control(net, url).await.map_err(to_error)?;
    let path = ftp_path(url);
    if path.is_empty() || path.ends_with('/') {
        return Err(Error::Protocol("This link is a folder, not a file.".into()));
    }
    let r = c.cmd(&format!("SIZE {path}")).await.map_err(net_err)?;
    let total_size = match r.code {
        213 => r.message().trim().parse::<u64>().ok(),
        550 => {
            let cwd = c.cmd(&format!("CWD {path}")).await.map_err(net_err)?;
            if cwd.class() == 2 {
                return Err(Error::Protocol("This link is a folder, not a file.".into()));
            }
            return Err(Error::Ftp(550, r.message()));
        }
        421 => return Err(Error::Busy(421, None)),
        // SIZE isn't supported (or refused in ASCII mode on old servers).
        _ => None,
    };
    let last_modified = match c.cmd(&format!("MDTM {path}")).await.map_err(net_err)? {
        r if r.code == 213 => parse_mdtm(&r),
        _ => None,
    };
    let resumable =
        c.features.rest_stream || matches!(c.cmd("REST 0").await, Ok(r) if r.code == 350);
    net.ftp_pool.put(c);

    let name = path
        .rsplit('/')
        .find(|s| !s.is_empty())
        .and_then(filename::sanitize)
        .unwrap_or_else(|| "download".to_string());
    let content_type = mime_guess::from_path(&name).first().map(|m| m.to_string());
    Ok(ProbeResult {
        final_meta: Default::default(),
        final_url: url.to_string(),
        total_size,
        resumable: resumable && total_size.is_some(),
        etag: None,
        last_modified,
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
    data: Conn,
    control: Option<Control>,
    remaining: Option<u64>,
    pool: Pool,
    buf: Vec<u8>,
}

pub(crate) async fn open_range(
    net: &Net,
    url: &Url,
    start: u64,
    end: Option<u64>,
) -> std::result::Result<ByteStream, OpenError> {
    let mut c = control(net, url).await?;
    let path = ftp_path(url);
    let data = c.data(net).await?;
    if start > 0 {
        let r = c.cmd(&format!("REST {start}")).await.map_err(transient)?;
        if r.code != 350 {
            return Err(OpenError::Fatal(Error::Protocol(
                "The FTP server stopped accepting resumed transfers".into(),
            )));
        }
    }
    let r = c.cmd(&format!("RETR {path}")).await.map_err(transient)?;
    if r.code != 150 && r.code != 125 {
        return Err(refused(&r));
    }
    let data = if c.tls {
        tls_wrap(&c.host, data).await?
    } else {
        Conn::Plain(data)
    };
    let state = Reading {
        data,
        control: Some(c),
        remaining: end.map(|e| e.saturating_sub(start)),
        pool: net.ftp_pool.clone(),
        buf: vec![0u8; 64 * 1024],
    };
    Ok(Box::pin(futures_util::stream::unfold(
        state,
        |mut s| async move {
            if s.remaining == Some(0) {
                s.finish_early();
                return None;
            }
            let want = s
                .remaining
                .map(|r| (r.min(s.buf.len() as u64)) as usize)
                .unwrap_or(s.buf.len());
            let read = tokio::time::timeout(READ_TIMEOUT, s.data.read(&mut s.buf[..want])).await;
            match read {
                Err(_) => Some((Err("the server stopped sending".to_string()), s.dead())),
                Ok(Err(e)) => Some((Err(e.to_string()), s.dead())),
                Ok(Ok(0)) => {
                    if s.remaining.is_some_and(|r| r > 0) {
                        return Some((Err("connection closed early".to_string()), s.dead()));
                    }
                    // The whole file arrived: the server confirms with 226.
                    let mut c = s.control.take()?;
                    s.data = Conn::Closed;
                    match c.read_reply().await {
                        Ok(r) if r.class() == 2 => {
                            s.pool.put(c);
                            None
                        }
                        Ok(r) => Some((
                            Err(format!("FTP {}: {}", r.code, r.message())),
                            Reading {
                                data: Conn::Closed,
                                ..s
                            }
                            .dead(),
                        )),
                        Err(e) => Some((
                            Err(e.to_string()),
                            Reading {
                                data: Conn::Closed,
                                ..s
                            }
                            .dead(),
                        )),
                    }
                }
                Ok(Ok(n)) => {
                    if let Some(r) = s.remaining.as_mut() {
                        *r -= n as u64;
                    }
                    let chunk = Bytes::copy_from_slice(&s.buf[..n]);
                    Some((Ok(chunk), s))
                }
            }
        },
    )))
}

impl Reading {
    /// After an error the connection can't be trusted: drop it, and end the stream next.
    fn dead(mut self) -> Self {
        self.control = None;
        self.remaining = Some(0);
        self.data = Conn::Closed;
        self
    }

    /// The range ended before the file did. Aborting cleanly is murky (a server may have
    /// finished sending already, leaving an extra reply behind), so both connections are
    /// closed instead of being reused.
    fn finish_early(&mut self) {
        self.control = None;
        self.data = Conn::Closed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passive_answers() {
        assert_eq!(
            parse_epsv("229 Entering Extended Passive Mode (|||6446|)\r\n"),
            Some(6446)
        );
        assert_eq!(
            parse_pasv("227 Entering Passive Mode (192,168,1,2,19,137).\r\n"),
            Some(19 * 256 + 137)
        );
        assert_eq!(parse_pasv("227 =10,0,0,1,4,1\r\n"), Some(1025));
        assert_eq!(parse_pasv("227 nonsense\r\n"), None);
    }

    #[test]
    fn paths() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert_eq!(
            ftp_path(&u("ftp://h/pub/gnu/a%20b.tar.gz")),
            "pub/gnu/a b.tar.gz"
        );
        assert_eq!(ftp_path(&u("ftp://h/%2Fetc/motd")), "/etc/motd");
        assert_eq!(ftp_path(&u("ftp://h/f.iso;type=i")), "f.iso");
    }
}
