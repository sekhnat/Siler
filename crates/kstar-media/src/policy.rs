//! The derived-URL policy (design §16.2): what a manifest may make K-Star fetch. Pure
//! checks on the URL itself; the engine also resolves names and checks the addresses they
//! point at before connecting.

use std::net::{IpAddr, Ipv6Addr};
use url::{Host, Url};

/// Why a URL is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the scheme “{0}” isn't allowed")]
    Scheme(String),
    #[error("a URL with a user name or password isn't allowed")]
    Credentials,
    #[error("the address has no host name")]
    NoHost,
    #[error("the address {0} is on this computer or the local network")]
    PrivateAddress(String),
}

/// Checks a URL a manifest (or the user) gave. `allow_private` is set when the source is
/// itself on a private address, or the user allowed private addresses.
pub fn check(url: &Url, allow_private: bool) -> Result<(), Refusal> {
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(Refusal::Scheme(other.to_string())),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Refusal::Credentials);
    }
    match url.host() {
        None => Err(Refusal::NoHost),
        Some(Host::Ipv4(ip)) => private_check(IpAddr::V4(ip), allow_private),
        Some(Host::Ipv6(ip)) => private_check(IpAddr::V6(ip), allow_private),
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if (d == "localhost" || d.ends_with(".localhost")) && !allow_private {
                return Err(Refusal::PrivateAddress(d));
            }
            Ok(())
        }
    }
}

/// Whether a URL names a private address or `localhost` (so a source on one is trusted to
/// point at others).
pub fn is_private_source(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => is_private(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => is_private(IpAddr::V6(ip)),
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        None => false,
    }
}

fn private_check(ip: IpAddr, allow_private: bool) -> Result<(), Refusal> {
    if !allow_private && is_private(ip) {
        return Err(Refusal::PrivateAddress(ip.to_string()));
    }
    Ok(())
}

/// Loopback, private, link-local, unique-local, shared (CGNAT) and unspecified addresses.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || (o[0] == 100 && (64..=127).contains(&o[1]))
        }
        IpAddr::V6(v6) => {
            let s0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (s0 & 0xfe00) == 0xfc00
                || (s0 & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_private(IpAddr::V4(v4)))
                || is_nat64_private(v6)
        }
    }
}

/// `64:ff9b::/96` embeds an IPv4 address in its low 32 bits.
fn is_nat64_private(v6: Ipv6Addr) -> bool {
    let s = v6.segments();
    s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|&x| x == 0) && {
        let [a, b] = s[6].to_be_bytes();
        let [c, d] = s[7].to_be_bytes();
        is_private(IpAddr::V4(std::net::Ipv4Addr::new(a, b, c, d)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).expect("test URL")
    }

    #[test]
    fn schemes_and_credentials() {
        assert!(check(&u("https://cdn.example/a.ts"), false).is_ok());
        assert_eq!(
            check(&u("file:///etc/passwd"), false),
            Err(Refusal::Scheme("file".into()))
        );
        assert_eq!(
            check(&u("ftp://x/a"), false),
            Err(Refusal::Scheme("ftp".into()))
        );
        assert_eq!(
            check(&u("https://user:pw@cdn.example/a"), false),
            Err(Refusal::Credentials)
        );
    }

    #[test]
    fn private_addresses_need_permission() {
        for s in [
            "http://127.0.0.1/a",
            "http://10.1.2.3/a",
            "http://192.168.0.9/a",
            "http://172.16.5.5/a",
            "http://169.254.169.254/latest",
            "http://0.0.0.0/a",
            "http://100.64.1.1/a",
            "http://[::1]/a",
            "http://[fd00::1]/a",
            "http://[fe80::1]/a",
            "http://[::ffff:10.0.0.1]/a",
            "http://[64:ff9b::a00:1]/a",
            "http://localhost/a",
            "http://api.localhost/a",
        ] {
            assert!(
                matches!(check(&u(s), false), Err(Refusal::PrivateAddress(_))),
                "{s}"
            );
            assert!(check(&u(s), true).is_ok(), "{s}");
            assert!(is_private_source(&u(s)), "{s}");
        }
        for s in [
            "http://93.184.216.34/a",
            "http://[2606:4700::1111]/a",
            "https://example.org/a",
        ] {
            assert!(check(&u(s), false).is_ok(), "{s}");
            assert!(!is_private_source(&u(s)), "{s}");
        }
    }
}
