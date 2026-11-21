//! Name lookups for media requests. The addresses a name resolves to are checked in the
//! same lookup the connection uses, so a name can't answer with a public address for a check
//! and a private one for the connection (DNS rebinding).

use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use siler_media::policy;

type Lookup = Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>>;

/// Where names are looked up: the system resolver, or a fixed answer in tests.
pub(crate) trait Lookuper: Send + Sync + 'static {
    fn lookup(&self, host: String) -> Lookup;
}

/// The system resolver (`getaddrinfo`, as reqwest itself uses).
pub(crate) struct System;

impl Lookuper for System {
    fn lookup(&self, host: String) -> Lookup {
        Box::pin(async move { Ok(tokio::net::lookup_host((host.as_str(), 0)).await?.collect()) })
    }
}

/// A lookup refused because the name points at this computer or the local network.
#[derive(Debug)]
pub(crate) struct PrivateAddress {
    pub host: String,
}

impl std::fmt::Display for PrivateAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} points to an address on this computer or the local network",
            self.host
        )
    }
}

impl std::error::Error for PrivateAddress {}

/// Resolves names and refuses any answer that holds a private address (media-design §16.2).
/// The proxy's own host is exempt, so a proxy on this computer still works.
pub(crate) struct PublicOnly {
    lookup: Arc<dyn Lookuper>,
    exempt: HashSet<String>,
}

impl PublicOnly {
    pub fn new(lookup: Arc<dyn Lookuper>, exempt: HashSet<String>) -> PublicOnly {
        PublicOnly { lookup, exempt }
    }
}

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().trim_end_matches('.').to_ascii_lowercase();
        let exempt = self.exempt.contains(&host);
        let fut = self.lookup.lookup(host.clone());
        Box::pin(async move {
            let addrs = fut.await?;
            if !exempt && addrs.iter().any(|a| policy::is_private(a.ip())) {
                return Err(Box::new(PrivateAddress { host }) as _);
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// Resolves names with no check (a run that may use private addresses).
pub(crate) struct Any(pub Arc<dyn Lookuper>);

impl Resolve for Any {
    fn resolve(&self, name: Name) -> Resolving {
        let fut = self.0.lookup(name.as_str().to_string());
        Box::pin(async move { Ok(Box::new(fut.await?.into_iter()) as Addrs) })
    }
}

/// The host names of the proxies a route uses.
pub(crate) fn proxy_hosts(urls: &[Option<&str>]) -> HashSet<String> {
    urls.iter()
        .flatten()
        .filter_map(|u| {
            reqwest::Url::parse(u)
                .ok()
                .filter(|p| p.host_str().is_some())
                .or_else(|| reqwest::Url::parse(&format!("http://{u}")).ok())
        })
        .filter_map(|u| {
            u.host_str()
                .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
        })
        .collect()
}

/// Whether an error came from a refused lookup; gives its message.
pub(crate) fn refused(e: &(dyn std::error::Error + 'static)) -> Option<String> {
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(s) = src {
        if let Some(p) = s.downcast_ref::<PrivateAddress>() {
            return Some(p.to_string());
        }
        src = s.source();
    }
    None
}

/// A fixed table of answers, for tests. A name may answer differently each time.
#[cfg(test)]
pub(crate) struct Table(
    pub std::sync::Mutex<std::collections::HashMap<String, Vec<Vec<SocketAddr>>>>,
);

#[cfg(test)]
impl Lookuper for Table {
    fn lookup(&self, host: String) -> Lookup {
        let mut t = self.0.lock().expect("table");
        let answer = match t.get_mut(&host) {
            Some(v) if v.len() > 1 => Ok(v.remove(0)),
            Some(v) => Ok(v.first().cloned().unwrap_or_default()),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "no such name")),
        };
        Box::pin(async move { answer })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn table(entries: &[(&str, Vec<Vec<&str>>)]) -> Arc<dyn Lookuper> {
        let map = entries
            .iter()
            .map(|(h, answers)| {
                let answers = answers
                    .iter()
                    .map(|a| {
                        a.iter()
                            .map(|ip| SocketAddr::new(ip.parse().expect("ip"), 0))
                            .collect()
                    })
                    .collect();
                (h.to_string(), answers)
            })
            .collect();
        Arc::new(Table(std::sync::Mutex::new(map)))
    }

    async fn resolve(r: &PublicOnly, host: &str) -> Result<Vec<SocketAddr>, String> {
        r.resolve(Name::from_str(host).expect("name"))
            .await
            .map(|a| a.collect())
            .map_err(|e| refused(e.as_ref()).unwrap_or_else(|| e.to_string()))
    }

    #[tokio::test]
    async fn every_lookup_is_checked_so_a_name_cant_turn_private() {
        let r = PublicOnly::new(
            table(&[(
                "rebind.test",
                vec![vec!["93.184.216.34"], vec!["127.0.0.1"]],
            )]),
            HashSet::new(),
        );
        assert!(resolve(&r, "rebind.test").await.is_ok());
        let second = resolve(&r, "rebind.test").await;
        assert_eq!(
            second,
            Err("rebind.test points to an address on this computer or the local network".into())
        );
    }

    #[tokio::test]
    async fn one_private_address_in_the_answer_refuses_it() {
        let r = PublicOnly::new(
            table(&[
                ("mixed.test", vec![vec!["93.184.216.34", "10.0.0.5"]]),
                ("v6.test", vec![vec!["::1"]]),
                ("meta.test", vec![vec!["169.254.169.254"]]),
            ]),
            HashSet::new(),
        );
        for h in ["mixed.test", "v6.test", "meta.test"] {
            assert!(resolve(&r, h).await.is_err(), "{h}");
        }
    }

    #[tokio::test]
    async fn the_proxy_host_may_be_local() {
        let exempt = proxy_hosts(&[
            Some("http://Proxy.Home:3128"),
            Some("socks5h://127.0.0.1:1080"),
        ]);
        assert!(exempt.contains("proxy.home"));
        let r = PublicOnly::new(table(&[("proxy.home", vec![vec!["192.168.1.2"]])]), exempt);
        assert!(resolve(&r, "proxy.home").await.is_ok());
        assert_eq!(
            proxy_hosts(&[Some("proxy.lan:8080")]),
            HashSet::from(["proxy.lan".to_string()])
        );
    }
}
