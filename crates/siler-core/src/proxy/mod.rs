//! Which proxy to use: none, the desktop's (environment, KDE or GNOME), Siler's own, or
//! what a proxy script says ([`pac`]), which may have to be found first ([`wpad`]).
//!
//! The parsing is kept free of I/O so it can be tested with sample inputs; [`route`] does
//! the reading, and [`router::Router`] answers for each address.

mod pac;
pub(crate) mod router;
mod wpad;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    /// Always connect directly.
    None,
    /// Use the proxy configured for the desktop (environment variables, KDE or GNOME).
    #[default]
    System,
    /// Use the proxy in these settings.
    Manual,
    /// Ask this proxy script.
    Script,
    /// Find a proxy script on the network (WPAD).
    Detect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyScheme {
    #[default]
    Http,
    Https,
    Socks4,
    Socks4a,
    Socks5,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxySettings {
    pub mode: ProxyMode,
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    /// Leave empty when the proxy needs no login. The password is kept by the app (keyring).
    pub username: String,
    /// Comma-separated hosts or domains that bypass the proxy.
    pub no_proxy: String,
    /// The proxy script's address or file.
    pub script: String,
    /// An FTP proxy for ftp:// addresses (`USER user@host`); none when empty.
    pub ftp_host: String,
    pub ftp_port: u16,
}

impl Default for ProxySettings {
    fn default() -> Self {
        ProxySettings {
            mode: ProxyMode::System,
            scheme: ProxyScheme::Http,
            host: String::new(),
            port: 8080,
            username: String::new(),
            no_proxy: "localhost, 127.0.0.1, ::1".into(),
            script: String::new(),
            ftp_host: String::new(),
            ftp_port: 21,
        }
    }
}

impl ProxySettings {
    /// The manual proxy needs a password from the keyring.
    pub fn needs_password(&self) -> bool {
        self.mode == ProxyMode::Manual && !self.username.trim().is_empty()
    }
}

/// Where a proxy script comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Script {
    /// This address or file.
    Given(String),
    /// Looked for on the network.
    Detect,
}

/// Why an address goes the way it does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxyWhy {
    NoProxy,
    Configured {
        source: String,
    },
    Excepted,
    /// A script isn't asked about this computer.
    ThisComputer,
    Script {
        from: String,
    },
    Detected {
        from: String,
    },
    NothingDetected {
        reason: String,
    },
}

/// What "Test with an address" shows.
#[derive(Debug, Clone, Serialize)]
pub struct ProxyCheck {
    /// In the order they are tried; `DIRECT` is no proxy.
    pub via: Vec<String>,
    pub why: ProxyWhy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Http,
    Https,
    Socks4,
    Socks4a,
    Socks5,
}

impl Kind {
    pub fn scheme(self) -> &'static str {
        match self {
            Kind::Http => "http",
            Kind::Https => "https",
            Kind::Socks4 => "socks4",
            Kind::Socks4a => "socks4a",
            // The proxy resolves the names.
            Kind::Socks5 => "socks5h",
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Kind::Http => 80,
            Kind::Https => 443,
            Kind::Socks4 | Kind::Socks4a | Kind::Socks5 => 1080,
        }
    }

    /// Speaks HTTP: forwards plain requests and tunnels with CONNECT.
    pub fn is_http(self) -> bool {
        matches!(self, Kind::Http | Kind::Https)
    }
}

/// One proxy server, without a login.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Proxy {
    pub kind: Kind,
    /// Lower case, IPv6 without brackets.
    pub host: String,
    pub port: u16,
}

impl Proxy {
    /// Reads an address, and the login written into it.
    pub fn parse(raw: &str) -> Option<(Proxy, Option<crate::Login>)> {
        let url = Url::parse(&proxy_url(raw)?).ok()?;
        let kind = match url.scheme() {
            "http" => Kind::Http,
            "https" => Kind::Https,
            "socks4" => Kind::Socks4,
            "socks4a" => Kind::Socks4a,
            "socks5" | "socks5h" => Kind::Socks5,
            _ => return None,
        };
        let host = url.host_str().filter(|h| !h.is_empty())?;
        let port = url.port().unwrap_or(kind.default_port());
        let decode = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        };
        let login = (!url.username().is_empty()).then(|| crate::Login {
            username: decode(url.username()),
            password: decode(url.password().unwrap_or("")),
        });
        let proxy = Proxy {
            kind,
            host: host.trim_matches(['[', ']']).to_ascii_lowercase(),
            port,
        };
        Some((proxy, login))
    }

    /// What its login is kept under, apart from any site's.
    pub fn login_key(&self) -> String {
        format!("proxy+{self}")
    }
}

impl fmt::Display for Proxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (scheme, host, port) = (self.kind.scheme(), &self.host, self.port);
        if host.contains(':') {
            write!(f, "{scheme}://[{host}]:{port}")
        } else {
            write!(f, "{scheme}://{host}:{port}")
        }
    }
}

/// One way out.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Via {
    Direct,
    Proxy(Proxy),
}

impl fmt::Display for Via {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Via::Direct => f.write_str("DIRECT"),
            Via::Proxy(proxy) => proxy.fmt(f),
        }
    }
}

/// An address for showing and logging: without the login that may be written into it.
fn without_login(raw: &str) -> String {
    let (scheme, rest) = raw.split_once("://").map_or(("", raw), |(s, r)| (s, r));
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = authority.rsplit('@').next().unwrap_or(authority);
    if scheme.is_empty() {
        format!("{host}{path}")
    } else {
        format!("{scheme}://{host}{path}")
    }
}

/// Whether an exception list (separated by commas, spaces or semicolons) names the host:
/// `*`, names and addresses, domains (`example.com` and `.example.com` cover subdomains),
/// networks (`10.0.0.0/8`), `<local>` for names without a dot, and any of them with a port.
pub(crate) fn bypassed(list: &str, host: &str, port: Option<u16>) -> bool {
    let host = host
        .trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let address: Option<std::net::IpAddr> = host.parse().ok();
    list.split(|c: char| matches!(c, ',' | ';') || c.is_whitespace())
        .map(str::to_ascii_lowercase)
        .filter(|entry| !entry.is_empty())
        .any(|entry| {
            let entry = entry.split_once("://").map_or(entry.as_str(), |(_, e)| e);
            let entry = entry.trim_end_matches('/');
            match entry {
                "*" => return true,
                "<local>" => return address.is_none() && !host.contains('.'),
                _ => {}
            }
            if let Some((network, bits)) = entry.split_once('/') {
                return address.is_some_and(|a| in_network(a, network, bits));
            }
            let (name, only_port) = match entry.rsplit_once(':') {
                Some((h, p)) if !h.contains(':') || h.ends_with(']') => match p.parse::<u16>() {
                    Ok(p) => (h, Some(p)),
                    Err(_) => (entry, None),
                },
                _ => (entry, None),
            };
            if let (Some(wanted), Some(port)) = (only_port, port)
                && wanted != port
            {
                return false;
            }
            let name = name
                .trim_matches(['[', ']'])
                .trim_start_matches("*.")
                .trim_start_matches('.');
            !name.is_empty()
                && (host == name || (address.is_none() && host.ends_with(&format!(".{name}"))))
        })
}

fn in_network(address: std::net::IpAddr, network: &str, bits: &str) -> bool {
    use std::net::IpAddr;
    let (Ok(network), Ok(bits)) = (
        network.trim_matches(['[', ']']).parse::<IpAddr>(),
        bits.parse::<u32>(),
    ) else {
        return false;
    };
    match (address, network) {
        (IpAddr::V4(a), IpAddr::V4(n)) if bits <= 32 => {
            let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
            u32::from(a) & mask == u32::from(n) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(n)) if bits <= 128 => {
            let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
            u128::from(a) & mask == u128::from(n) & mask
        }
        _ => false,
    }
}

/// Where requests go: the proxy URL for http:// and https:// targets (direct when absent).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct ProxyRoute {
    /// Where the setting came from: "environment", "KDE", "GNOME", "settings" or "none".
    pub source: String,
    pub http: Option<String>,
    pub https: Option<String>,
    pub no_proxy: String,
    /// The listed hosts are the only ones that use the proxy (KDE's reversed exceptions).
    pub only_listed: bool,
    /// A script decides instead of `http` and `https`.
    pub script: Option<Script>,
    /// An FTP proxy (`ftp://host:port`), for ftp:// addresses.
    pub ftp: Option<String>,
    /// Why a configured proxy isn't used.
    pub note: Option<String>,
}

impl ProxyRoute {
    fn direct(source: &str) -> Self {
        ProxyRoute {
            source: source.into(),
            ..Default::default()
        }
    }

    pub fn is_direct(&self) -> bool {
        self.http.is_none() && self.https.is_none() && self.script.is_none()
    }

    /// A one-line description for the settings screen.
    pub fn summary(&self) -> String {
        let shown = |raw: &Option<String>| raw.as_deref().map(without_login);
        let mut target = match (&self.script, shown(&self.http), shown(&self.https)) {
            (Some(Script::Given(at)), ..) => format!("proxy script {}", without_login(at)),
            (Some(Script::Detect), ..) => "automatic proxy detection".to_string(),
            (None, None, None) => "no proxy".to_string(),
            (None, Some(a), Some(b)) if a == b => a,
            (None, a, b) => format!(
                "http: {}, https: {}",
                a.as_deref().unwrap_or("direct"),
                b.as_deref().unwrap_or("direct")
            ),
        };
        if let Some(ftp) = shown(&self.ftp) {
            target.push_str(&format!(", ftp: {ftp}"));
        }
        match self.source.as_str() {
            "none" | "settings" => target,
            s => format!("{s}: {target}"),
        }
    }
}

/// Normalises a proxy address: adds `http://` when there is no scheme, and resolves
/// host names through SOCKS proxies (`socks://` and `socks5://` become `socks5h://`).
fn proxy_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (scheme, rest) = match raw.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => ("http".to_string(), raw),
    };
    let scheme = match scheme.as_str() {
        "socks" | "socks5" => "socks5h",
        s => s,
    };
    let rest = rest.trim_end_matches('/');
    (!rest.is_empty()).then(|| format!("{scheme}://{rest}"))
}

/// An FTP proxy's address. A web proxy set for FTP isn't one: it tunnels like the https one.
fn ftp_url(raw: &str) -> Option<String> {
    let url = proxy_url(raw)?;
    (url.starts_with("ftp://") && Url::parse(&url).is_ok_and(|u| u.host_str().is_some()))
        .then_some(url)
}

/// The proxy from `http_proxy`, `https_proxy`, `all_proxy` and `no_proxy` (either case).
pub fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Option<ProxyRoute> {
    let var = |name: &str| {
        env(name)
            .or_else(|| env(&name.to_ascii_uppercase()))
            .filter(|v| !v.trim().is_empty())
    };
    let all = var("all_proxy").and_then(|v| proxy_url(&v));
    let http = var("http_proxy")
        .and_then(|v| proxy_url(&v))
        .or(all.clone());
    let https = var("https_proxy").and_then(|v| proxy_url(&v)).or(all);
    let ftp = var("ftp_proxy").and_then(|v| ftp_url(&v));
    if http.is_none() && https.is_none() && ftp.is_none() {
        return None;
    }
    Some(ProxyRoute {
        source: "environment".into(),
        http,
        https,
        ftp,
        no_proxy: var("no_proxy").unwrap_or_default(),
        ..Default::default()
    })
}

/// The keys of one `[section]` of a KDE config file; `key[$e]` is read as `key`.
fn ini_section(text: &str, section: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') && !line.contains('=') {
            inside = &line[1..line.len() - 1] == section;
            continue;
        }
        if !inside || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.split('[').next().unwrap_or(k).trim();
            out.insert(k.to_string(), v.trim().to_string());
        }
    }
    out
}

/// KDE writes proxies as `http://host 8080`.
fn kde_proxy(v: &str) -> Option<String> {
    let v = v.trim();
    let joined = match v.rsplit_once(' ') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => {
            format!("{}:{port}", host.trim())
        }
        _ => v.to_string(),
    };
    proxy_url(&joined)
}

/// The proxy from KDE's `kioslaverc` (`[Proxy Settings]`).
pub fn from_kde(kioslaverc: &str, env: &dyn Fn(&str) -> Option<String>) -> ProxyRoute {
    let s = ini_section(kioslaverc, "Proxy Settings");
    let get = |k: &str| s.get(k).map(String::as_str).unwrap_or("");
    let scripted = |script| ProxyRoute {
        script: Some(script),
        ..ProxyRoute::direct("KDE")
    };
    match get("ProxyType") {
        "1" => {
            let socks = kde_proxy(get("socksProxy"));
            let http = kde_proxy(get("httpProxy")).or(socks.clone());
            let https = kde_proxy(get("httpsProxy")).or(socks);
            ProxyRoute {
                source: "KDE".into(),
                http,
                https,
                ftp: kde_proxy(get("ftpProxy")).and_then(|v| ftp_url(&v)),
                no_proxy: get("NoProxyFor").to_string(),
                only_listed: get("ReversedException") == "true",
                ..Default::default()
            }
        }
        // The values name environment variables.
        "4" => {
            let var = |k: &str| {
                let name = get(k);
                if name.is_empty() {
                    None
                } else {
                    env(name).and_then(|v| proxy_url(&v))
                }
            };
            let socks = var("socksProxy");
            ProxyRoute {
                source: "KDE".into(),
                http: var("httpProxy").or(socks.clone()),
                https: var("httpsProxy").or(socks),
                no_proxy: {
                    let name = get("NoProxyFor");
                    if name.is_empty() {
                        String::new()
                    } else {
                        env(name).unwrap_or_default()
                    }
                },
                ftp: var("ftpProxy").and_then(|v| ftp_url(&v)),
                ..Default::default()
            }
        }
        "2" => match get("Proxy Config Script") {
            "" => ProxyRoute {
                note: Some("KDE's proxy script has no address; connecting directly".into()),
                ..ProxyRoute::direct("KDE")
            },
            at => scripted(Script::Given(at.to_string())),
        },
        "3" => scripted(Script::Detect),
        _ => ProxyRoute::direct("KDE"),
    }
}

/// Strips the quoting `gsettings get` adds.
fn unquote(v: &str) -> String {
    v.trim().trim_matches('\'').to_string()
}

/// The proxy from GNOME's `org.gnome.system.proxy` keys; `get(schema, key)` returns the
/// raw `gsettings get` output.
pub fn from_gnome(get: &dyn Fn(&str, &str) -> Option<String>) -> Option<ProxyRoute> {
    let mode = unquote(&get("org.gnome.system.proxy", "mode")?);
    match mode.as_str() {
        "manual" => {
            let endpoint = |kind: &str, scheme: &str| {
                let schema = format!("org.gnome.system.proxy.{kind}");
                let host = unquote(&get(&schema, "host")?);
                let port = get(&schema, "port")?.trim().parse::<u16>().ok()?;
                (!host.is_empty() && port > 0).then(|| format!("{scheme}://{host}:{port}"))
            };
            let socks = endpoint("socks", "socks5h");
            let ignore = get("org.gnome.system.proxy", "ignore-hosts")
                .map(|v| {
                    v.trim()
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .split(',')
                        .map(unquote)
                        .filter(|h| !h.is_empty())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            Some(ProxyRoute {
                source: "GNOME".into(),
                http: endpoint("http", "http").or(socks.clone()),
                https: endpoint("https", "http").or(socks),
                no_proxy: ignore,
                ..Default::default()
            })
        }
        "auto" => {
            let at = get("org.gnome.system.proxy", "autoconfig-url")
                .map(|v| unquote(&v))
                .unwrap_or_default();
            Some(ProxyRoute {
                // Without an address GNOME looks for the script itself.
                script: Some(if at.is_empty() {
                    Script::Detect
                } else {
                    Script::Given(at)
                }),
                ..ProxyRoute::direct("GNOME")
            })
        }
        _ => Some(ProxyRoute::direct("GNOME")),
    }
}

fn manual(s: &ProxySettings) -> ProxyRoute {
    let bracketed = |host: &str| {
        if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]") // IPv6 literal
        } else {
            host.to_string()
        }
    };
    let ftp_host = s.ftp_host.trim();
    let ftp = (!ftp_host.is_empty() && s.ftp_port > 0)
        .then(|| format!("ftp://{}:{}", bracketed(ftp_host), s.ftp_port));
    let host = s.host.trim();
    if host.is_empty() || s.port == 0 {
        let note = ftp
            .is_none()
            .then(|| "No proxy address entered; connecting directly".to_string());
        return ProxyRoute {
            ftp,
            note,
            no_proxy: s.no_proxy.clone(),
            ..ProxyRoute::direct("settings")
        };
    }
    let scheme = match s.scheme {
        ProxyScheme::Http => "http",
        ProxyScheme::Https => "https",
        ProxyScheme::Socks4 => "socks4",
        ProxyScheme::Socks4a => "socks4a",
        ProxyScheme::Socks5 => "socks5h",
    };
    let url = format!("{scheme}://{}:{}", bracketed(host), s.port);
    ProxyRoute {
        source: "settings".into(),
        http: Some(url.clone()),
        https: Some(url),
        no_proxy: s.no_proxy.clone(),
        ftp,
        ..Default::default()
    }
}

fn scripted(s: &ProxySettings) -> ProxyRoute {
    let at = s.script.trim();
    if at.is_empty() {
        return ProxyRoute {
            note: Some("No proxy script entered; connecting directly".into()),
            ..ProxyRoute::direct("settings")
        };
    }
    ProxyRoute {
        script: Some(Script::Given(at.to_string())),
        ..ProxyRoute::direct("settings")
    }
}

/// The desktop's proxy: environment variables first, then KDE or GNOME settings.
fn system() -> ProxyRoute {
    let env = |k: &str| std::env::var(k).ok();
    if let Some(r) = from_env(&env) {
        return r;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_uppercase();
    if desktop.contains("KDE") {
        let path = dirs::config_dir().map(|d| d.join("kioslaverc"));
        let text = path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        return from_kde(&text, &env);
    }
    let gsettings = |schema: &str, key: &str| {
        let out = crate::host::command("gsettings")
            .args(["get", schema, key])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    from_gnome(&gsettings).unwrap_or_else(|| ProxyRoute::direct("none"))
}

/// Where requests should go under these settings.
pub fn route(s: &ProxySettings) -> ProxyRoute {
    match s.mode {
        ProxyMode::None => ProxyRoute::direct("none"),
        ProxyMode::Manual => manual(s),
        ProxyMode::System => system(),
        ProxyMode::Script => scripted(s),
        ProxyMode::Detect => ProxyRoute {
            script: Some(Script::Detect),
            ..ProxyRoute::direct("settings")
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn environment() {
        assert_eq!(from_env(&env_of(&[])), None);
        let r = from_env(&env_of(&[
            ("HTTP_PROXY", "proxy.lan:3128"),
            ("all_proxy", "socks://tor:9050"),
            ("no_proxy", "localhost,.lan"),
        ]))
        .unwrap();
        assert_eq!(r.http.as_deref(), Some("http://proxy.lan:3128"));
        assert_eq!(r.https.as_deref(), Some("socks5h://tor:9050"));
        assert_eq!(r.no_proxy, "localhost,.lan");
    }

    #[test]
    fn kde_manual() {
        let text = "[General]\nfoo=1\n\n[Proxy Settings]\nNoProxyFor=localhost,127.0.0.1\n\
                    ProxyType=1\nhttpProxy=http://10.0.0.1 3128\nhttpsProxy=\nsocksProxy=socks://10.0.0.2 1080\n";
        let r = from_kde(text, &env_of(&[]));
        assert_eq!(r.source, "KDE");
        assert_eq!(r.http.as_deref(), Some("http://10.0.0.1:3128"));
        assert_eq!(r.https.as_deref(), Some("socks5h://10.0.0.2:1080"));
        assert_eq!(r.no_proxy, "localhost,127.0.0.1");
        assert_eq!(
            r.summary(),
            "KDE: http: http://10.0.0.1:3128, https: socks5h://10.0.0.2:1080"
        );
    }

    #[test]
    fn kde_other_modes() {
        let r = from_kde(
            "[Proxy Settings]\nProxyType=0\nhttpProxy=http://x 1\n",
            &env_of(&[]),
        );
        assert!(r.is_direct() && r.note.is_none());
        let r = from_kde("[Proxy Settings]\nProxyType=2\n", &env_of(&[]));
        assert!(r.is_direct() && r.note.is_some());
        let r = from_kde(
            "[Proxy Settings]\nProxyType=2\nProxy Config Script=http://cfg/p.pac\n",
            &env_of(&[]),
        );
        assert_eq!(r.script, Some(Script::Given("http://cfg/p.pac".into())));
        let r = from_kde("[Proxy Settings]\nProxyType=3\n", &env_of(&[]));
        assert_eq!(r.script, Some(Script::Detect));
        let r = from_kde(
            "[Proxy Settings]\nProxyType=1\nReversedException=true\nNoProxyFor=.corp\nhttpProxy=http://p 1\nftpProxy=ftp://f 2121\n",
            &env_of(&[]),
        );
        assert!(r.only_listed && r.note.is_none());
        assert_eq!(r.ftp.as_deref(), Some("ftp://f:2121"));
        let r = from_kde(
            "[Proxy Settings]\nProxyType=4\nhttpProxy=MY_PROXY\nhttpsProxy=MY_PROXY\nNoProxyFor=MY_NO\n",
            &env_of(&[("MY_PROXY", "http://p:8080/"), ("MY_NO", "intranet")]),
        );
        assert_eq!(r.http.as_deref(), Some("http://p:8080"));
        assert_eq!(r.https.as_deref(), Some("http://p:8080"));
        assert_eq!(r.no_proxy, "intranet");
        // Missing file or section: direct.
        assert!(from_kde("", &env_of(&[])).is_direct());
    }

    #[test]
    fn gnome() {
        let get = |schema: &str, key: &str| -> Option<String> {
            Some(
                match (schema, key) {
                    ("org.gnome.system.proxy", "mode") => "'manual'",
                    ("org.gnome.system.proxy", "ignore-hosts") => "['localhost', '127.0.0.0/8']",
                    ("org.gnome.system.proxy.http", "host") => "'proxy.example'",
                    ("org.gnome.system.proxy.http", "port") => "8080",
                    ("org.gnome.system.proxy.https", "host") => "''",
                    ("org.gnome.system.proxy.https", "port") => "0",
                    ("org.gnome.system.proxy.socks", "host") => "'socks.example'",
                    ("org.gnome.system.proxy.socks", "port") => "1080",
                    _ => return None,
                }
                .to_string(),
            )
        };
        let r = from_gnome(&get).unwrap();
        assert_eq!(r.http.as_deref(), Some("http://proxy.example:8080"));
        assert_eq!(r.https.as_deref(), Some("socks5h://socks.example:1080"));
        assert_eq!(r.no_proxy, "localhost,127.0.0.0/8");
        assert!(from_gnome(&|_: &str, _: &str| None).is_none());
        let auto = |address: &'static str| {
            from_gnome(&move |_: &str, key: &str| match key {
                "mode" => Some("'auto'".to_string()),
                "autoconfig-url" => Some(address.to_string()),
                _ => None,
            })
            .unwrap()
            .script
        };
        assert_eq!(auto("''"), Some(Script::Detect));
        assert_eq!(
            auto("'http://c/p'"),
            Some(Script::Given("http://c/p".into()))
        );
    }

    #[test]
    fn exceptions() {
        let by = |list, host| bypassed(list, host, Some(443));
        assert!(by("localhost, 127.0.0.1, ::1", "localhost"));
        assert!(by("localhost, 127.0.0.1, ::1", "[::1]"));
        assert!(by(".lan", "nas.lan"));
        assert!(by("example.com", "ftp.example.com"));
        assert!(by("*", "anything"));
        assert!(!by("example.com", "badexample.com"));
        assert!(!by("", "example.com"));
        assert!(by("10.0.0.0/8", "10.200.3.4"));
        assert!(!by("10.0.0.0/8", "11.0.0.1"));
        assert!(by("fd00::/8", "[fd12::1]"));
        assert!(by("0.0.0.0/0", "8.8.8.8"));
        assert!(by("<local>", "intranet"));
        assert!(!by("<local>", "a.example"));
        assert!(by("example.com:443", "example.com"));
        assert!(!by("example.com:80", "example.com"));
        assert!(bypassed("*.example.com:21", "ftp.example.com", Some(21)));
    }

    #[test]
    fn logins_arent_shown() {
        let r = from_env(&env_of(&[(
            "http_proxy",
            "http://me:hunter2@proxy.lan:3128",
        )]))
        .unwrap();
        assert!(!r.summary().contains("hunter2"), "{}", r.summary());
        let (proxy, login) = Proxy::parse(r.http.as_deref().unwrap()).unwrap();
        assert_eq!(proxy.login_key(), "proxy+http://proxy.lan:3128");
        assert_eq!(login.unwrap().password, "hunter2");
    }

    #[test]
    fn manual_settings() {
        let mut s = ProxySettings {
            mode: ProxyMode::Manual,
            ..Default::default()
        };
        assert!(route(&s).is_direct());
        s.host = "proxy.lan".into();
        s.port = 1080;
        s.scheme = ProxyScheme::Socks5;
        let r = route(&s);
        assert_eq!(r.http.as_deref(), Some("socks5h://proxy.lan:1080"));
        assert_eq!(r.summary(), "socks5h://proxy.lan:1080");
        assert!(!s.needs_password());
        s.username = "me".into();
        assert!(s.needs_password());
        s.mode = ProxyMode::None;
        assert_eq!(route(&s).summary(), "no proxy");
    }
}
