//! Finding a proxy script on the network (WPAD): the DHCP option 252 that NetworkManager
//! got, then `wpad.<domain>` for the search domains. Only those domains themselves are
//! asked: never their parents, and never a name under a public suffix.

use std::collections::HashMap;
use std::time::Duration;
use url::Url;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const BUS_TIMEOUT: Duration = Duration::from_secs(3);
/// No more search domains than this are tried.
const MAX_DOMAINS: usize = 4;
const NM: &str = "org.freedesktop.NetworkManager";

/// What NetworkManager knows about the main connection.
#[derive(Debug, Default)]
struct Network {
    option_252: Option<String>,
    domains: Vec<String>,
}

async fn property<T: TryFrom<OwnedValue>>(
    bus: &zbus::Connection,
    path: &str,
    interface: &str,
    name: &str,
) -> Option<T> {
    let reply = bus
        .call_method(
            Some(NM),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(interface, name),
        )
        .await
        .ok()?;
    T::try_from(reply.body().deserialize::<OwnedValue>().ok()?).ok()
}

async fn network_manager() -> Option<Network> {
    let bus = zbus::Connection::system().await.ok()?;
    let active: OwnedObjectPath = property(
        &bus,
        "/org/freedesktop/NetworkManager",
        NM,
        "PrimaryConnection",
    )
    .await?;
    if active.as_str() == "/" {
        return None;
    }
    let interface = format!("{NM}.Connection.Active");
    let mut found = Network::default();

    let dhcp: Option<OwnedObjectPath> =
        property(&bus, active.as_str(), &interface, "Dhcp4Config").await;
    if let Some(path) = dhcp.filter(|p| p.as_str() != "/") {
        let interface = format!("{NM}.DHCP4Config");
        let options: Option<HashMap<String, OwnedValue>> =
            property(&bus, path.as_str(), &interface, "Options").await;
        let mut options = options.unwrap_or_default();
        let mut take = |key: &str| {
            options
                .remove(key)
                .and_then(|v| String::try_from(v).ok())
                .unwrap_or_default()
        };
        found.option_252 = Some(take("wpad")).filter(|v| !v.is_empty());
        for key in ["domain_name", "domain_search"] {
            found
                .domains
                .extend(take(key).split_whitespace().map(str::to_string));
        }
    }
    let ip: Option<OwnedObjectPath> =
        property(&bus, active.as_str(), &interface, "Ip4Config").await;
    if let Some(path) = ip.filter(|p| p.as_str() != "/") {
        let interface = format!("{NM}.IP4Config");
        for key in ["Domains", "Searches"] {
            let list: Option<Vec<String>> = property(&bus, path.as_str(), &interface, key).await;
            found.domains.extend(list.unwrap_or_default());
        }
    }
    Some(found)
}

fn search_domains(resolv_conf: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in resolv_conf.lines() {
        let mut words = line.split_whitespace();
        if matches!(words.next(), Some("search" | "domain")) {
            out.extend(words.map(str::to_string));
        }
    }
    out
}

/// The option's value as an address. Only a web one: the network mustn't name a file here.
fn option_url(value: &str) -> Option<String> {
    let value = value.trim_matches(|c: char| c.is_whitespace() || c.is_control());
    let url = Url::parse(value).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

/// `wpad.<domain>` may be asked: the domain isn't one others can register names in.
fn may_ask(domain: &str) -> bool {
    let tidy = !domain.is_empty()
        && domain.len() <= 240
        && domain
            .split('.')
            .all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        && domain.parse::<std::net::IpAddr>().is_err();
    let public_suffix =
        psl::suffix(domain.as_bytes()).is_some_and(|s| s.is_known() && s == domain.as_bytes());
    tidy && !public_suffix
}

/// The addresses to try, in order.
fn candidates(option_252: Option<&str>, domains: &[String]) -> Vec<String> {
    let mut out: Vec<String> = option_252.and_then(option_url).into_iter().collect();
    let mut asked: Vec<String> = Vec::new();
    for domain in domains {
        // "~domain" only routes lookups and is no search domain.
        if domain.starts_with('~') {
            continue;
        }
        let domain = domain.trim_end_matches('.').to_ascii_lowercase();
        if asked.len() < MAX_DOMAINS && may_ask(&domain) && !asked.contains(&domain) {
            out.push(format!("http://wpad.{domain}/wpad.dat"));
            asked.push(domain);
        }
    }
    out
}

/// The script and where it came from.
pub(super) async fn find() -> Result<(String, String), String> {
    let mut network = tokio::time::timeout(BUS_TIMEOUT, network_manager())
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    if let Ok(text) = tokio::fs::read_to_string("/etc/resolv.conf").await {
        network.domains.extend(search_domains(&text));
    }
    for url in candidates(network.option_252.as_deref(), &network.domains) {
        match super::pac::fetch(&url).await {
            Ok(text) if text.contains("FindProxyForURL") => return Ok((url, text)),
            Ok(_) => tracing::debug!("{url} is no proxy script"),
            Err(e) => tracing::debug!("WPAD: {e}"),
        }
    }
    Err("the network names no proxy script".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_asked() {
        let domains = [
            "corp.example.com",
            "com",
            "co.uk",
            "github.io",
            "~vpn.example",
            "lan",
            "Corp.Example.com.",
            "bad host",
            "example.co.uk",
        ]
        .map(String::from);
        assert_eq!(
            candidates(Some("http://proxy.corp/wpad.dat\0"), &domains),
            [
                "http://proxy.corp/wpad.dat",
                "http://wpad.corp.example.com/wpad.dat",
                "http://wpad.lan/wpad.dat",
                "http://wpad.example.co.uk/wpad.dat"
            ]
        );
        assert!(candidates(Some("file:///etc/passwd"), &[]).is_empty());
    }

    #[test]
    fn resolv_conf() {
        assert_eq!(
            search_domains("# x\nnameserver 1.1.1.1\nsearch a.example b.example\n"),
            ["a.example", "b.example"]
        );
    }
}
