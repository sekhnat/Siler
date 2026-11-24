//! The HTTP clients (one per way out), and the logins known per server and per proxy.

use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering::Relaxed};
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

/// The most HTTP/2 connections per server (Settings → Transfers).
pub const MAX_H2_SLOTS: u8 = 16;
/// HTTP/2 flow control: how much one stream may have in flight. One stream then carries
/// at most this much per round trip (2 MiB at 50 ms is about 40 MB/s).
const H2_STREAM_WINDOW: u32 = 2 * 1024 * 1024;
/// And one connection, for the several streams that share it.
const H2_CONNECTION_WINDOW: u32 = 8 * H2_STREAM_WINDOW;

/// A client for one way out, and its tunnel.
pub(crate) struct Kept {
    pub client: Client,
    pub tunnel: Option<Tunnel>,
    /// For an HTTP/2 slot: the servers that have answered it.
    first: Option<FirstTurn>,
}

/// Until a server has answered a slot client, its requests go one at a time: whether the
/// connection is HTTP/2 is only known once it is made, and requests sent together before
/// then would each make a connection of their own.
#[derive(Default)]
struct FirstTurn {
    answered: Mutex<HashSet<String>>,
    turn: tokio::sync::Mutex<()>,
}

impl Kept {
    /// Waits while another request makes this client's first connection to `url`'s server.
    /// Holding the guard, this request is the one making it.
    pub async fn first_turn(&self, url: &Url) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        let first = self.first.as_ref()?;
        let site = site_key(url);
        if first.answered.lock().unwrap().contains(&site) {
            return None;
        }
        let guard = first.turn.lock().await;
        (!first.answered.lock().unwrap().contains(&site)).then_some(guard)
    }

    /// `url`'s server answered: its connection is made, and is shared from now on.
    pub fn answered(&self, url: &Url) {
        if let Some(first) = &self.first {
            first.answered.lock().unwrap().insert(site_key(url));
        }
    }
}

#[derive(PartialEq)]
struct WayKey {
    via: Via,
    how: How,
    allow_private: bool,
    /// A tunnel leads to one server.
    to: Option<(String, u16)>,
    /// An HTTP/2 slot: a client of its own, so its connection to a server is its own.
    slot: Option<u8>,
}

impl WayKey {
    fn new(exit: &Exit, how: &How, url: &Url, slot: Option<u8>) -> WayKey {
        WayKey {
            slot,
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
    /// HTTP/2 connections per server for range workers; 0 = HTTP/1.1 only.
    h2_slots: AtomicU8,
    /// Round robin over the slots.
    h2_next: AtomicUsize,
    /// Certificates the tests' own HTTPS servers are signed with.
    #[cfg(test)]
    pub(crate) test_roots: Mutex<Vec<reqwest::Certificate>>,
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
            h2_slots: AtomicU8::new(0),
            h2_next: AtomicUsize::new(0),
            #[cfg(test)]
            test_roots: Mutex::new(Vec::new()),
        };
        // A computer that can't make a client at all says so now, not at the first download.
        net.build(&net.exit(Via::Direct, true), &How::Plain, None, false, None)?;
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

    /// How many HTTP/2 connections range workers may open to a server; 0 keeps them on
    /// HTTP/1.1. Clients made for the old number are dropped.
    pub fn set_h2_slots(&self, n: u8) {
        let n = n.min(MAX_H2_SLOTS);
        if self.h2_slots.swap(n, Relaxed) != n {
            self.clients
                .lock()
                .unwrap()
                .retain(|(k, _)| k.slot.is_none());
        }
    }

    /// The slot for the next range request, round robin; None when HTTP/2 is off.
    pub(crate) fn next_slot(&self) -> Option<u8> {
        let n = self.h2_slots.load(Relaxed);
        (n > 0).then(|| (self.h2_next.fetch_add(1, Relaxed) % n as usize) as u8)
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
    ///
    /// With a `slot` it is one of the HTTP/2 clients; without, it only speaks HTTP/1.1.
    pub(crate) fn client(
        &self,
        exit: &Exit,
        how: &How,
        url: &Url,
        slot: Option<u8>,
    ) -> Result<Arc<Kept>> {
        let slot = slot.filter(|_| *how != How::Tunnel);
        let key = WayKey::new(exit, how, url, slot);
        let setup = self.setup();
        let find = |list: &[(WayKey, Arc<Kept>)]| {
            list.iter().find(|(k, _)| *k == key).map(|(_, c)| c.clone())
        };
        if let Some(found) = find(&self.clients.lock().unwrap()) {
            return Ok(found);
        }
        let made = self.build(exit, how, key.to.as_ref(), false, key.slot)?;
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
        let key = WayKey::new(exit, how, url, None);
        self.build(exit, how, key.to.as_ref(), true, None)
    }

    fn build(
        &self,
        exit: &Exit,
        how: &How,
        to: Option<&(String, u16)>,
        pinned: bool,
        slot: Option<u8>,
    ) -> Result<Arc<Kept>> {
        let mut b = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .pool_idle_timeout(Duration::from_secs(30))
            .no_proxy();
        b = match slot {
            // Offers HTTP/2 to https servers; it keeps one connection to each.
            Some(_) if !pinned => b
                .http2_initial_stream_window_size(H2_STREAM_WINDOW)
                .http2_initial_connection_window_size(H2_CONNECTION_WINDOW),
            // Each request on a connection of its own, its ranges pointing at the file.
            _ => b.http1_only(),
        };
        if pinned {
            // One connection, whose certificate an NTLM or Kerberos answer names.
            b = b.pool_max_idle_per_host(1).tls_info(true);
        }
        #[cfg(test)]
        for cert in self.test_roots.lock().unwrap().iter() {
            b = b.tls_certs_merge([cert.clone()]);
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
        Ok(Arc::new(Kept {
            client,
            tunnel,
            first: (slot.is_some() && !pinned).then(FirstTurn::default),
        }))
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

    #[test]
    fn http2_slots_take_turns_and_are_off_by_default() {
        let net = Net::new(&ProxySettings::default(), None).unwrap();
        assert_eq!(net.next_slot(), None);
        net.set_h2_slots(3);
        let turns: Vec<_> = (0..6).map(|_| net.next_slot().unwrap()).collect();
        for (i, t) in turns.iter().enumerate() {
            assert_eq!(*t as usize, (turns[0] as usize + i) % 3);
        }
        net.set_h2_slots(200);
        assert_eq!(net.h2_slots.load(Relaxed), MAX_H2_SLOTS);
        net.set_h2_slots(0);
        assert_eq!(net.next_slot(), None);
    }

    #[test]
    fn each_slot_is_a_client_of_its_own() {
        let net = Net::new(&ProxySettings::default(), None).unwrap();
        net.set_h2_slots(2);
        let exit = net.exit(Via::Direct, false);
        let url = Url::parse("https://files.example/a.iso").unwrap();
        let plain = net.client(&exit, &How::Plain, &url, None).unwrap();
        let a = net.client(&exit, &How::Plain, &url, Some(0)).unwrap();
        let b = net.client(&exit, &How::Plain, &url, Some(1)).unwrap();
        assert!(!Arc::ptr_eq(&plain, &a) && !Arc::ptr_eq(&a, &b));
        assert!(plain.first.is_none() && a.first.is_some());
        assert!(Arc::ptr_eq(
            &a,
            &net.client(&exit, &How::Plain, &url, Some(0)).unwrap()
        ));
        // A new number of slots drops the slot clients, not the others.
        net.set_h2_slots(4);
        assert!(!Arc::ptr_eq(
            &a,
            &net.client(&exit, &How::Plain, &url, Some(0)).unwrap()
        ));
        assert!(Arc::ptr_eq(
            &plain,
            &net.client(&exit, &How::Plain, &url, None).unwrap()
        ));
    }
}
