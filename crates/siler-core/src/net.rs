//! The HTTP clients (one per way out), and the logins known per server and per proxy.

use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crate::proxy::router::Router;
use crate::proxy::{ProxyRoute, ProxySettings, Via};
use crate::resolve::{self, Lookuper, PublicOnly};
use crate::session::auth::HostAuth;
use crate::session::tunnel::Tunnel;
use crate::transport::dial::{Exit, Failed, How, Stream};
use crate::{Error, Result};

/// A username and password for one server.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    pub username: String,
    pub password: String,
}

impl Login {
    /// "Use my Kerberos ticket for this site": an approval, with no name or password.
    pub fn kerberos_ticket() -> Login {
        Login {
            username: String::new(),
            password: String::new(),
        }
    }

    pub fn is_ticket(&self) -> bool {
        self.username.is_empty() && self.password.is_empty()
    }
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// `host` or `host:port` (port only when it isn't the scheme's default).
pub fn host_key(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => String::new(),
    }
}

/// `scheme://host[:port]`: what Windows logins and Kerberos approvals are kept under, so
/// that one given for https never goes to http.
pub(crate) fn site_key(url: &Url) -> String {
    format!("{}://{}", url.scheme(), host_key(url))
}

/// More ways out than this and the oldest clients are dropped.
const MAX_CLIENTS: usize = 128;

/// A client for one way out, and its tunnel.
pub(crate) struct Kept {
    pub client: Client,
    pub tunnel: Option<Tunnel>,
}

#[derive(PartialEq)]
struct WayKey {
    via: Via,
    how: How,
    allow_private: bool,
    /// A tunnel leads to one server.
    to: Option<(String, u16)>,
}

impl WayKey {
    fn new(exit: &Exit, how: &How, url: &Url) -> WayKey {
        WayKey {
            via: exit.via.clone(),
            how: how.clone(),
            allow_private: exit.allow_private,
            to: (*how == How::Tunnel).then(|| {
                (
                    url.host_str().unwrap_or("").to_string(),
                    url.port_or_known_default().unwrap_or(443),
                )
            }),
        }
    }
}

pub(crate) struct Net {
    pub(crate) router: Router,
    /// One client per way out. They never follow redirects (`session` does, and checks
    /// every hop).
    clients: Mutex<Vec<(WayKey, Arc<Kept>)>>,
    lookup: Arc<dyn Lookuper>,
    /// Counts the proxy setups, so that connections kept for logins don't outlive theirs.
    setup: AtomicU64,
    hosts: Mutex<HashMap<String, Arc<HostAuth>>>,
    /// Siler's own list of trusted SSH server keys (next to the database); `None` in tests
    /// that never use SFTP.
    pub(crate) known_hosts: RwLock<Option<std::path::PathBuf>>,
    /// Idle FTP control connections, for the next range from the same server.
    pub(crate) ftp_pool: crate::transport::ftp::Pool,
}

impl Net {
    pub fn new(proxy: &ProxySettings, password: Option<&str>) -> Result<Net> {
        Net::with_lookup(proxy, password, Arc::new(resolve::System))
    }

    /// A `Net` whose media clients look names up through `lookup`.
    pub(crate) fn with_lookup(
        proxy: &ProxySettings,
        password: Option<&str>,
        lookup: Arc<dyn Lookuper>,
    ) -> Result<Net> {
        let net = Net {
            router: Router::default(),
            clients: Mutex::new(Vec::new()),
            lookup,
            setup: AtomicU64::new(0),
            hosts: Mutex::new(HashMap::new()),
            known_hosts: RwLock::new(None),
            ftp_pool: Default::default(),
        };
        // A computer that can't make a client at all says so now, not at the first download.
        net.build(&net.exit(Via::Direct, true), &How::Plain, None, false)?;
        net.configure(proxy, password);
        Ok(net)
    }

    /// Applies new proxy settings. A broken proxy address falls back to direct connections
    /// with an explanation in the route.
    pub fn configure(&self, proxy: &ProxySettings, password: Option<&str>) {
        self.router.configure(proxy, password);
        self.setup.fetch_add(1, Relaxed);
        self.clients.lock().unwrap().clear();
        self.ftp_pool.clear();
    }

    pub(crate) fn setup(&self) -> u64 {
        self.setup.load(Relaxed)
    }

    pub fn route(&self) -> ProxyRoute {
        self.router.route()
    }

    /// One way out, with the login for its proxy: the settings' own, else one given for
    /// exactly that proxy.
    pub(crate) fn exit(&self, via: Via, allow_private: bool) -> Exit {
        let (auth, own) = match &via {
            Via::Direct => (None, false),
            Via::Proxy(proxy) => match self.router.own_login(proxy) {
                Some(auth) => (Some(auth), true),
                None => (Some(self.entry(&proxy.login_key())), false),
            },
        };
        Exit::new(via, allow_private, auth, own, self.lookup.clone())
    }

    /// The client for one way out to `url`. It returns redirects instead of following
    /// them, and unless the exit allows private addresses it refuses names that resolve to
    /// this computer or the local network.
    pub(crate) fn client(&self, exit: &Exit, how: &How, url: &Url) -> Result<Arc<Kept>> {
        let key = WayKey::new(exit, how, url);
        let setup = self.setup();
        let find = |list: &[(WayKey, Arc<Kept>)]| {
            list.iter().find(|(k, _)| *k == key).map(|(_, c)| c.clone())
        };
        if let Some(found) = find(&self.clients.lock().unwrap()) {
            return Ok(found);
        }
        let made = self.build(exit, how, key.to.as_ref(), false)?;
        let mut list = self.clients.lock().unwrap();
        if let Some(found) = find(&list) {
            return Ok(found);
        }
        // Not one from before the settings changed.
        if setup == self.setup() {
            if list.len() >= MAX_CLIENTS {
                list.remove(0);
            }
            list.push((key, made.clone()));
        }
        Ok(made)
    }

    /// A new client with a single connection, for a login that belongs to its connection.
    pub(crate) fn pinned(&self, exit: &Exit, how: &How, url: &Url) -> Result<Arc<Kept>> {
        let key = WayKey::new(exit, how, url);
        self.build(exit, how, key.to.as_ref(), true)
    }

    fn build(
        &self,
        exit: &Exit,
        how: &How,
        to: Option<&(String, u16)>,
        pinned: bool,
    ) -> Result<Arc<Kept>> {
        let mut b = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .pool_idle_timeout(Duration::from_secs(30))
            .no_proxy();
        if pinned {
            // One connection, whose certificate an NTLM or Kerberos answer names.
            b = b.http1_only().pool_max_idle_per_host(1).tls_info(true);
        }
        b = if exit.allow_private {
            b.dns_resolver(resolve::Any(self.lookup.clone()))
        } else {
            // The proxy's own name may point here: a proxy on this computer still works.
            let exempt = exit
                .proxy()
                .map(|p| p.host.trim_end_matches('.').to_string())
                .into_iter()
                .collect();
            b.dns_resolver(PublicOnly::new(self.lookup.clone(), exempt))
        };
        let mut tunnel = None;
        if let (How::Tunnel, Some((host, port))) = (how, to) {
            let made = Tunnel::open(exit.clone(), host.clone(), *port)?;
            b = b.unix_socket(made.path().to_path_buf());
            tunnel = Some(made);
        } else if let Some(proxy) = exit.proxy() {
            let invalid = || Error::Protocol(format!("Invalid proxy address {proxy}"));
            let mut address = Url::parse(&proxy.to_string()).map_err(|_| invalid())?;
            if let How::Socks(login) = how {
                // Encoded here: `Url` leaves a `%` as it is, and reqwest decodes it.
                use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
                let encode = |s: &str| utf8_percent_encode(s, NON_ALPHANUMERIC).to_string();
                address
                    .set_username(&encode(&login.username))
                    .map_err(|()| invalid())?;
                address
                    .set_password(Some(&encode(&login.password)))
                    .map_err(|()| invalid())?;
            }
            let mut through = reqwest::Proxy::all(address.as_str()).map_err(|_| invalid())?;
            if let How::Basic(login) = how {
                // Not `basic_auth`, which goes through the address and the same decoding.
                through = through.custom_http_auth(crate::session::auth::basic(login)?);
            }
            b = b.proxy(through);
        }
        let client = b.build().map_err(|e| Error::Network(e.to_string()))?;
        Ok(Arc::new(Kept { client, tunnel }))
    }

    /// A connection for FTP or SFTP, routed as `url` is.
    pub(crate) async fn dial(&self, url: &Url, host: &str, port: u16) -> Result<Stream> {
        let choice = self.router.choose(url).await?;
        let mut missed = None;
        for via in choice.via {
            let exit = self.exit(via, true);
            match exit.dial(host, port).await {
                Ok(stream) => return Ok(stream),
                Err(Failed::Unreachable(why)) => {
                    if let Some(proxy) = exit.proxy() {
                        self.router.mark_failed(proxy);
                    }
                    missed = Some(why);
                }
                Err(failed) => return Err(exit.error(failed)),
            }
        }
        Err(Error::Network(
            missed.unwrap_or_else(|| "there is no route".into()),
        ))
    }

    /// Straight to `host`, whatever the settings say (an FTP proxy itself).
    pub(crate) async fn dial_direct(&self, host: &str, port: u16) -> Result<Stream> {
        let exit = self.exit(Via::Direct, true);
        exit.dial(host, port)
            .await
            .map_err(|failed| exit.error(failed))
    }

    pub fn auth_for(&self, url: &Url) -> Option<Arc<HostAuth>> {
        self.find(&crate::transport::login_key(url))
    }

    pub(crate) fn find(&self, key: &str) -> Option<Arc<HostAuth>> {
        self.hosts.lock().unwrap().get(key).cloned()
    }

    /// The login known for a server, if any (FTP and SFTP use it directly).
    pub(crate) fn login_for(&self, url: &Url) -> Option<Login> {
        self.auth_for(url)
            .and_then(|a| a.login.lock().unwrap().clone())
    }

    pub fn entry(&self, host: &str) -> Arc<HostAuth> {
        self.hosts
            .lock()
            .unwrap()
            .entry(host.to_string())
            .or_default()
            .clone()
    }

    pub fn set_login(&self, host: &str, login: Option<Login>) {
        self.entry(host).set(login);
    }

    /// Moves `user:password@` out of a URL into the login for its server.
    pub fn take_userinfo(&self, url: &mut Url) {
        if url.username().is_empty() && url.password().is_none() {
            return;
        }
        let decode = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        };
        let login = Login {
            username: decode(url.username()),
            password: decode(url.password().unwrap_or("")),
        };
        let mut keys = vec![crate::transport::login_key(url)];
        // A login written into a web address is given for exactly that site as well.
        if crate::transport::kind(url) == crate::transport::Kind::Http && !login.is_ticket() {
            keys.push(site_key(url));
        }
        for key in keys {
            let entry = self.entry(&key);
            // The same one again must not undo a refusal.
            if entry.login.lock().unwrap().as_ref() != Some(&login) {
                entry.set(Some(login.clone()));
            }
        }
        let _ = url.set_username("");
        let _ = url.set_password(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userinfo_is_moved_out() {
        let net = Net::new(&ProxySettings::default(), None).unwrap();
        let mut url = Url::parse("https://me%40x:p%3Ass@files.example:8443/a.iso").unwrap();
        net.take_userinfo(&mut url);
        assert_eq!(url.as_str(), "https://files.example:8443/a.iso");
        let a = net.auth_for(&url).unwrap();
        let login = a.login.lock().unwrap().clone().unwrap();
        assert_eq!(login.username, "me@x");
        assert_eq!(login.password, "p:ss");
        assert_eq!(host_key(&Url::parse("https://a.b:443/").unwrap()), "a.b");
    }
}
