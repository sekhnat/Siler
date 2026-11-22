//! Which way each request goes: settings and scripts say, per address and so per redirect.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use url::Url;

use super::{Proxy, ProxyCheck, ProxyRoute, ProxySettings, ProxyWhy, Script, Via, pac};
use crate::net::Login;
use crate::session::auth::HostAuth;
use crate::{Error, Result};

/// A proxy that couldn't be reached is tried last for this long.
const SKIP_FAILED: Duration = Duration::from_secs(5 * 60);

/// The routes for one address, in the order to try them.
#[derive(Debug, Clone)]
pub(crate) struct Choice {
    pub via: Vec<Via>,
    pub why: ProxyWhy,
}

struct Setup {
    route: ProxyRoute,
    script: Option<pac::Runner>,
    /// Logins from Siler's settings, or written into the desktop's proxy addresses. They
    /// answer those proxies and no other.
    own_logins: Vec<(Proxy, Arc<HostAuth>)>,
}

pub(crate) struct Router {
    setup: RwLock<Arc<Setup>>,
    failed: Mutex<HashMap<Proxy, Instant>>,
    helper: Arc<RwLock<Option<PathBuf>>>,
}

fn on_this_computer(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']).trim_end_matches('.');
    host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

impl Default for Router {
    fn default() -> Self {
        Router {
            setup: RwLock::new(Arc::new(Setup {
                route: ProxyRoute::default(),
                script: None,
                own_logins: Vec::new(),
            })),
            failed: Mutex::default(),
            helper: Arc::default(),
        }
    }
}

impl Router {
    /// Applies settings. An address that can't be read means direct, and the route says so.
    pub fn configure(&self, settings: &ProxySettings, password: Option<&str>) {
        let mut route = super::route(settings);
        let mut proxies = Vec::new();
        for raw in [route.http.clone(), route.https.clone()]
            .into_iter()
            .flatten()
        {
            match Proxy::parse(&raw) {
                Some(found) => proxies.push(found),
                None => {
                    let shown = super::without_login(&raw);
                    route = ProxyRoute {
                        source: route.source,
                        note: Some(format!(
                            "Invalid proxy address {shown}; connecting directly"
                        )),
                        ..Default::default()
                    };
                    proxies.clear();
                    break;
                }
            }
        }
        let manual = settings.needs_password().then(|| Login {
            username: settings.username.trim().to_string(),
            password: password.unwrap_or_default().to_string(),
        });
        let mut own_logins: Vec<(Proxy, Arc<HostAuth>)> = Vec::new();
        for (proxy, in_address) in proxies {
            let Some(login) = in_address.or_else(|| manual.clone()) else {
                continue;
            };
            if !own_logins.iter().any(|(p, _)| *p == proxy) {
                let auth = Arc::new(HostAuth::default());
                auth.set(Some(login));
                own_logins.push((proxy, auth));
            }
        }
        let script = route
            .script
            .clone()
            .map(|script| pac::Runner::new(script, self.helper.clone()));
        tracing::info!("proxy: {}", route.summary());
        self.failed.lock().unwrap().clear();
        *self.setup.write().unwrap() = Arc::new(Setup {
            route,
            script,
            own_logins,
        });
    }

    fn setup(&self) -> Arc<Setup> {
        self.setup.read().unwrap().clone()
    }

    pub fn route(&self) -> ProxyRoute {
        self.setup().route.clone()
    }

    pub fn set_helper(&self, path: Option<PathBuf>) {
        *self.helper.write().unwrap() = path;
    }

    /// The login Siler was configured with for this proxy.
    pub fn own_login(&self, proxy: &Proxy) -> Option<Arc<HostAuth>> {
        self.setup()
            .own_logins
            .iter()
            .find(|(p, _)| p == proxy)
            .map(|(_, a)| a.clone())
    }

    pub async fn choose(&self, url: &Url) -> Result<Choice> {
        let setup = self.setup();
        let mut choice = match &setup.script {
            Some(runner) => Self::scripted(&setup.route, runner, url).await?,
            None => Self::fixed(&setup.route, url),
        };
        if choice.via.len() > 1 {
            let mut failed = self.failed.lock().unwrap();
            failed.retain(|_, at| at.elapsed() < SKIP_FAILED);
            // Stable: the script's order holds among the others, and the failed are still
            // tried when nothing else works.
            choice.via.sort_by_key(|via| match via {
                Via::Proxy(p) => failed.contains_key(p),
                Via::Direct => false,
            });
        }
        Ok(choice)
    }

    fn fixed(route: &ProxyRoute, url: &Url) -> Choice {
        let direct = |why| Choice {
            via: vec![Via::Direct],
            why,
        };
        if route.is_direct() {
            return direct(ProxyWhy::NoProxy);
        }
        let host = url.host_str().unwrap_or("");
        let port = url.port_or_known_default();
        if super::bypassed(&route.no_proxy, host, port) != route.only_listed {
            return direct(ProxyWhy::Excepted);
        }
        let raw = match url.scheme() {
            "http" => route.http.as_deref(),
            "https" => route.https.as_deref(),
            // Connections that only need a tunnel.
            _ => route.https.as_deref().or(route.http.as_deref()),
        };
        match raw.and_then(Proxy::parse) {
            Some((proxy, _)) => Choice {
                via: vec![Via::Proxy(proxy)],
                why: ProxyWhy::Configured {
                    source: route.source.clone(),
                },
            },
            None => direct(ProxyWhy::NoProxy),
        }
    }

    async fn scripted(route: &ProxyRoute, runner: &pac::Runner, url: &Url) -> Result<Choice> {
        if on_this_computer(url.host_str().unwrap_or("")) {
            return Ok(Choice {
                via: vec![Via::Direct],
                why: ProxyWhy::ThisComputer,
            });
        }
        let given = matches!(route.script, Some(Script::Given(_)));
        match runner.ask(url).await {
            Ok(answer) => Ok(Choice {
                via: answer.via,
                why: if given {
                    ProxyWhy::Script { from: answer.from }
                } else {
                    ProxyWhy::Detected { from: answer.from }
                },
            }),
            // A script someone chose is obeyed, so its failure stops the request.
            Err(e) if given => Err(Error::Proxy(e)),
            // One that was only looked for is optional.
            Err(reason) => Ok(Choice {
                via: vec![Via::Direct],
                why: ProxyWhy::NothingDetected { reason },
            }),
        }
    }

    /// The FTP proxy for a plain ftp:// address.
    pub fn ftp_proxy(&self, url: &Url) -> Option<(String, u16)> {
        let setup = self.setup();
        let route = &setup.route;
        let raw = route.ftp.as_deref().filter(|_| url.scheme() == "ftp")?;
        let host = url.host_str()?;
        if super::bypassed(&route.no_proxy, host, Some(url.port().unwrap_or(21)))
            != route.only_listed
        {
            return None;
        }
        let proxy = Url::parse(raw).ok()?;
        let host = proxy.host_str()?.trim_matches(['[', ']']).to_string();
        Some((host, proxy.port().unwrap_or(21)))
    }

    /// Where `url` would go and why; nothing is sent to it.
    pub async fn check(&self, url: &Url) -> Result<ProxyCheck> {
        if let Some((host, port)) = self.ftp_proxy(url) {
            let host = if host.contains(':') {
                format!("[{host}]")
            } else {
                host
            };
            return Ok(ProxyCheck {
                via: vec![format!("ftp://{host}:{port}")],
                why: ProxyWhy::Configured {
                    source: self.route().source,
                },
            });
        }
        let choice = self.choose(url).await?;
        Ok(ProxyCheck {
            via: choice.via.iter().map(ToString::to_string).collect(),
            why: choice.why,
        })
    }

    /// A router with other settings and the same helper, to try those settings out.
    pub fn trying(&self, settings: &ProxySettings) -> Router {
        let other = Router {
            helper: self.helper.clone(),
            ..Router::default()
        };
        other.configure(settings, None);
        other
    }

    pub fn mark_failed(&self, proxy: &Proxy) {
        tracing::info!("proxy {proxy} can't be reached; trying the others first for a while");
        self.failed
            .lock()
            .unwrap()
            .insert(proxy.clone(), Instant::now());
    }
}
