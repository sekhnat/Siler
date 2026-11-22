//! TCP connections for FTP and SFTP, through the proxy when there is one: SOCKS5 directly,
//! an HTTP proxy with `CONNECT`.

use std::io;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use url::Url;

use crate::proxy::ProxyRoute;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// A proxy login from the settings (the manual proxy's username and keyring password).
pub(crate) type ProxyLogin = Option<(String, String)>;

/// Connects to `host:port`, through the route's proxy unless `no_proxy` exempts the host.
pub(crate) async fn dial(
    route: &ProxyRoute,
    login: &ProxyLogin,
    host: &str,
    port: u16,
) -> io::Result<TcpStream> {
    let proxy = route
        .https
        .as_deref()
        .or(route.http.as_deref())
        .filter(|_| !bypassed(&route.no_proxy, host))
        .and_then(|p| Url::parse(p).ok());
    let fut = async {
        let Some(proxy) = proxy else {
            let s = TcpStream::connect((host, port)).await?;
            let _ = s.set_nodelay(true);
            return Ok(s);
        };
        let phost = proxy
            .host_str()
            .ok_or_else(|| io::Error::other("the proxy address has no host"))?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
        let pport = proxy
            .port_or_known_default()
            .unwrap_or(match proxy.scheme() {
                "http" => 8080,
                _ => 1080,
            });
        // A login written into the proxy address (environment variables) wins.
        let from_url = (!proxy.username().is_empty()).then(|| {
            let d = |s: &str| {
                percent_encoding::percent_decode_str(s)
                    .decode_utf8_lossy()
                    .into_owned()
            };
            (d(proxy.username()), d(proxy.password().unwrap_or("")))
        });
        let login = from_url.or_else(|| login.clone());
        let mut s = TcpStream::connect((phost.as_str(), pport)).await?;
        let _ = s.set_nodelay(true);
        match proxy.scheme() {
            "socks5" | "socks5h" => socks5(&mut s, &login, host, port).await?,
            "http" => http_connect(&mut s, &login, host, port).await?,
            other => {
                return Err(io::Error::other(format!(
                    "a {other} proxy can't carry FTP or SFTP connections"
                )));
            }
        }
        Ok(s)
    };
    tokio::time::timeout(CONNECT_TIMEOUT, fut)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connection timed out"))?
}

/// Whether a `no_proxy` list (comma or space separated) exempts `host`: `*`, exact names
/// and addresses, and domains (`example.com` or `.example.com` cover their subdomains).
pub(crate) fn bypassed(no_proxy: &str, host: &str) -> bool {
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    no_proxy
        .split([',', ' ', ';'])
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .any(|e| {
            if e == "*" {
                return true;
            }
            // Entries may carry a port; FTP and SFTP match on the host only.
            let e = match e.rsplit_once(':') {
                Some((h, p)) if !h.contains(':') && p.parse::<u16>().is_ok() => h.to_string(),
                _ => e,
            };
            let e = e.trim_start_matches("*.").trim_start_matches('.');
            host == e || host.ends_with(&format!(".{e}"))
        })
}

async fn socks5(s: &mut TcpStream, login: &ProxyLogin, host: &str, port: u16) -> io::Result<()> {
    let fail = |m: &str| io::Error::other(format!("SOCKS proxy: {m}"));
    let methods: &[u8] = if login.is_some() {
        &[0x00, 0x02]
    } else {
        &[0x00]
    };
    let mut hello = vec![0x05, methods.len() as u8];
    hello.extend_from_slice(methods);
    s.write_all(&hello).await?;
    let mut reply = [0u8; 2];
    s.read_exact(&mut reply).await?;
    if reply[0] != 0x05 {
        return Err(fail("not a SOCKS5 proxy"));
    }
    match reply[1] {
        0x00 => {}
        0x02 => {
            let (user, pass) = login
                .as_ref()
                .ok_or_else(|| fail("it needs a username and password"))?;
            if user.len() > 255 || pass.len() > 255 {
                return Err(fail("the username or password is too long"));
            }
            let mut auth = vec![0x01, user.len() as u8];
            auth.extend_from_slice(user.as_bytes());
            auth.push(pass.len() as u8);
            auth.extend_from_slice(pass.as_bytes());
            s.write_all(&auth).await?;
            s.read_exact(&mut reply).await?;
            if reply[1] != 0x00 {
                return Err(fail("it didn't accept the username and password"));
            }
        }
        _ => return Err(fail("no login method in common")),
    }
    // Let the proxy resolve the name (socks5h behaviour), unless it is an address.
    let mut req = vec![0x05, 0x01, 0x00];
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(a)) => {
            req.push(0x01);
            req.extend_from_slice(&a.octets());
        }
        Ok(std::net::IpAddr::V6(a)) => {
            req.push(0x04);
            req.extend_from_slice(&a.octets());
        }
        Err(_) => {
            if bare.len() > 255 {
                return Err(fail("the server name is too long"));
            }
            req.push(0x03);
            req.push(bare.len() as u8);
            req.extend_from_slice(bare.as_bytes());
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        let why = match head[1] {
            0x02 => "the connection isn't allowed",
            0x03 => "network unreachable",
            0x04 => "host unreachable",
            0x05 => "connection refused",
            0x06 => "timed out",
            _ => "the connection failed",
        };
        return Err(fail(why));
    }
    let skip = match head[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut n = [0u8; 1];
            s.read_exact(&mut n).await?;
            n[0] as usize
        }
        _ => return Err(fail("unexpected answer")),
    };
    let mut rest = vec![0u8; skip + 2];
    s.read_exact(&mut rest).await?;
    Ok(())
}

async fn http_connect(
    s: &mut TcpStream,
    login: &ProxyLogin,
    host: &str,
    port: u16,
) -> io::Result<()> {
    use base64::Engine as _;
    let target = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some((u, p)) = login {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"));
        req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).await?;
    // Read the answer byte by byte so nothing after the blank line is consumed.
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err(io::Error::other("HTTP proxy: answer too long"));
        }
        if s.read(&mut b).await? == 0 {
            return Err(io::Error::other("HTTP proxy closed the connection"));
        }
        head.push(b[0]);
    }
    let status = String::from_utf8_lossy(&head);
    let code = status
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    match code {
        200..=299 => Ok(()),
        407 => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the proxy needs a username and password",
        )),
        c => Err(io::Error::other(format!(
            "the HTTP proxy refused the connection (HTTP {c})"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_proxy_lists() {
        assert!(bypassed("localhost, 127.0.0.1, ::1", "localhost"));
        assert!(bypassed("localhost, 127.0.0.1, ::1", "[::1]"));
        assert!(bypassed(".lan", "nas.lan"));
        assert!(bypassed("example.com", "ftp.example.com"));
        assert!(bypassed("*.example.com:21", "ftp.example.com"));
        assert!(bypassed("*", "anything"));
        assert!(!bypassed("example.com", "badexample.com"));
        assert!(!bypassed("", "example.com"));
    }
}
