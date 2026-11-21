//! Which proxy to use: none, the desktop's (environment, KDE or GNOME), or Siler's own.
//!
//! The parsing is kept free of I/O so it can be tested with sample inputs; [`route`] does
//! the reading.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyScheme {
    #[default]
    Http,
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
        }
    }
}

impl ProxySettings {
    /// The manual proxy needs a password from the keyring.
    pub fn needs_password(&self) -> bool {
        self.mode == ProxyMode::Manual && !self.username.trim().is_empty()
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
    /// Why a configured proxy isn't used, e.g. a proxy script.
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
        self.http.is_none() && self.https.is_none()
    }

    /// A one-line description for the settings screen.
    pub fn summary(&self) -> String {
        let target = match (&self.http, &self.https) {
            (None, None) => "no proxy".to_string(),
            (Some(a), Some(b)) if a == b => a.clone(),
            (a, b) => format!(
                "http: {}, https: {}",
                a.as_deref().unwrap_or("direct"),
                b.as_deref().unwrap_or("direct")
            ),
        };
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
    if http.is_none() && https.is_none() {
        return None;
    }
    Some(ProxyRoute {
        source: "environment".into(),
        http,
        https,
        no_proxy: var("no_proxy").unwrap_or_default(),
        note: None,
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
    let unsupported = |what: &str| ProxyRoute {
        note: Some(format!(
            "KDE uses {what}, which Siler doesn't support yet; connecting directly"
        )),
        ..ProxyRoute::direct("KDE")
    };
    match get("ProxyType") {
        "1" => {
            if get("ReversedException") == "true" {
                return unsupported("a proxy only for listed sites");
            }
            let socks = kde_proxy(get("socksProxy"));
            let http = kde_proxy(get("httpProxy")).or(socks.clone());
            let https = kde_proxy(get("httpsProxy")).or(socks);
            ProxyRoute {
                source: "KDE".into(),
                http,
                https,
                no_proxy: get("NoProxyFor").to_string(),
                note: None,
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
                note: None,
            }
        }
        "2" => unsupported("a proxy script (PAC)"),
        "3" => unsupported("automatic proxy detection"),
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
                note: None,
            })
        }
        "auto" => Some(ProxyRoute {
            note: Some(
                "GNOME uses a proxy script, which Siler doesn't support yet; connecting directly"
                    .into(),
            ),
            ..ProxyRoute::direct("GNOME")
        }),
        _ => Some(ProxyRoute::direct("GNOME")),
    }
}

fn manual(s: &ProxySettings) -> ProxyRoute {
    let host = s.host.trim();
    if host.is_empty() || s.port == 0 {
        return ProxyRoute {
            note: Some("No proxy address entered; connecting directly".into()),
            ..ProxyRoute::direct("settings")
        };
    }
    let scheme = match s.scheme {
        ProxyScheme::Http => "http",
        ProxyScheme::Socks5 => "socks5h",
    };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]") // IPv6 literal
    } else {
        host.to_string()
    };
    let url = format!("{scheme}://{host}:{}", s.port);
    ProxyRoute {
        source: "settings".into(),
        http: Some(url.clone()),
        https: Some(url),
        no_proxy: s.no_proxy.clone(),
        note: None,
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
        assert!(r.is_direct() && r.note.unwrap().contains("PAC"));
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
