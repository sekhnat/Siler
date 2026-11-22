//! The `tls-server-end-point` channel binding (RFC 5929) of a server's certificate.

use sha2::{Digest, Sha256, Sha384, Sha512};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hash {
    Sha256,
    Sha384,
    Sha512,
}

/// One DER value: its tag, its content, and what follows it.
fn read(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, b) = b.split_first()?;
    let (&first, b) = b.split_first()?;
    let (len, b) = if first < 0x80 {
        (usize::from(first), b)
    } else {
        let n = usize::from(first & 0x7f);
        if !(1..=4).contains(&n) || b.len() < n {
            return None;
        }
        let len = b[..n].iter().fold(0, |a, x| a << 8 | usize::from(*x));
        (len, &b[n..])
    };
    Some((tag, b.get(..len)?, b.get(len..)?))
}

/// The hash RFC 5929 takes for a certificate signed with this algorithm: the signature's
/// own, or SHA-256 in place of MD5 and SHA-1. `None` for algorithms without one.
fn hash_of(algorithm: &[u8]) -> Option<Hash> {
    let (_, oid, params) = read(algorithm)?;
    match oid {
        // RSA PKCS #1: MD5, SHA-1, SHA-256, SHA-384, SHA-512
        [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, n] => match n {
            0x04 | 0x05 | 0x0b => Some(Hash::Sha256),
            0x0c => Some(Hash::Sha384),
            0x0d => Some(Hash::Sha512),
            // RSA-PSS names its hash first in the parameters; without one it is SHA-1.
            0x0a => Some(pss_hash(params)),
            _ => None,
        },
        [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x01] => Some(Hash::Sha256),
        [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, n] => match n {
            0x02 => Some(Hash::Sha256),
            0x03 => Some(Hash::Sha384),
            0x04 => Some(Hash::Sha512),
            _ => None,
        },
        _ => None,
    }
}

fn pss_hash(params: &[u8]) -> Hash {
    let named = || {
        let (_, fields, _) = read(params)?;
        let (tag, algorithm, _) = read(fields)?;
        if tag != 0xa0 {
            return None;
        }
        let (_, id, _) = read(algorithm)?;
        read(id).map(|(_, oid, _)| oid.last().copied())
    };
    match named() {
        Some(Some(0x02)) => Hash::Sha384,
        Some(Some(0x03)) => Hash::Sha512,
        _ => Hash::Sha256,
    }
}

/// The binding's application data for a DER certificate.
pub(crate) fn end_point(cert: &[u8]) -> Option<Vec<u8>> {
    let (_, body, _) = read(cert)?;
    let (_, _, after_tbs) = read(body)?;
    let (_, algorithm, _) = read(after_tbs)?;
    let hash = match hash_of(algorithm)? {
        Hash::Sha256 => Sha256::digest(cert).to_vec(),
        Hash::Sha384 => Sha384::digest(cert).to_vec(),
        Hash::Sha512 => Sha512::digest(cert).to_vec(),
    };
    Some([&b"tls-server-end-point:"[..], &hash].concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_follows_the_signature() {
        let rsa = |n| {
            [
                6, 9, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, n, 5, 0,
            ]
        };
        assert_eq!(hash_of(&rsa(0x05)), Some(Hash::Sha256));
        assert_eq!(hash_of(&rsa(0x0c)), Some(Hash::Sha384));
        let ecdsa = [6, 8, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x04];
        assert_eq!(hash_of(&ecdsa), Some(Hash::Sha512));
        let ed25519 = [6, 3, 0x2b, 0x65, 0x70];
        assert_eq!(hash_of(&ed25519), None);
        let mut pss = vec![6, 9, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0a];
        pss.extend([0x30, 17, 0xa0, 15, 0x30, 13, 6, 9]);
        pss.extend([0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02, 5, 0]);
        assert_eq!(hash_of(&pss), Some(Hash::Sha384));
        assert_eq!(end_point(&[0x30, 0x05, 1, 2]), None);
    }
}
