//! Checksums: parsing what people paste or servers send, and hashing files.

use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Algo {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl Algo {
    pub const ALL: [Algo; 4] = [Algo::Md5, Algo::Sha1, Algo::Sha256, Algo::Sha512];

    /// The name people know it by.
    pub fn label(self) -> &'static str {
        match self {
            Algo::Md5 => "MD5",
            Algo::Sha1 => "SHA-1",
            Algo::Sha256 => "SHA-256",
            Algo::Sha512 => "SHA-512",
        }
    }

    /// Length of the digest in bytes.
    pub fn digest_len(self) -> usize {
        match self {
            Algo::Md5 => 16,
            Algo::Sha1 => 20,
            Algo::Sha256 => 32,
            Algo::Sha512 => 64,
        }
    }

    /// Recognises `sha256`, `SHA-256`, `sha_256`, `md5`… (metalink and HTTP names too).
    pub fn from_name(name: &str) -> Option<Algo> {
        let n: String = name
            .chars()
            .filter(|c| !matches!(c, '-' | '_' | ' '))
            .collect::<String>()
            .to_ascii_lowercase();
        match n.as_str() {
            "md5" => Some(Algo::Md5),
            "sha1" | "sha" => Some(Algo::Sha1),
            "sha256" => Some(Algo::Sha256),
            "sha512" => Some(Algo::Sha512),
            _ => None,
        }
    }

    fn from_hex_len(len: usize) -> Option<Algo> {
        Algo::ALL.into_iter().find(|a| a.digest_len() * 2 == len)
    }

    /// Stronger algorithms rank higher.
    fn strength(self) -> u8 {
        match self {
            Algo::Md5 => 0,
            Algo::Sha1 => 1,
            Algo::Sha256 => 2,
            Algo::Sha512 => 3,
        }
    }

    pub fn hasher(self) -> Hasher {
        use sha2::Digest;
        match self {
            Algo::Md5 => Hasher::Md5(md5::Md5::new()),
            Algo::Sha1 => Hasher::Sha1(sha1::Sha1::new()),
            Algo::Sha256 => Hasher::Sha256(sha2::Sha256::new()),
            Algo::Sha512 => Hasher::Sha512(sha2::Sha512::new()),
        }
    }
}

pub enum Hasher {
    Md5(md5::Md5),
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
    Sha512(sha2::Sha512),
}

impl Hasher {
    pub fn update(&mut self, data: &[u8]) {
        use sha2::Digest;
        match self {
            Hasher::Md5(h) => h.update(data),
            Hasher::Sha1(h) => h.update(data),
            Hasher::Sha256(h) => h.update(data),
            Hasher::Sha512(h) => h.update(data),
        }
    }

    pub fn finish(self) -> String {
        use sha2::Digest;
        match self {
            Hasher::Md5(h) => hex(&h.finalize()),
            Hasher::Sha1(h) => hex(&h.finalize()),
            Hasher::Sha256(h) => hex(&h.finalize()),
            Hasher::Sha512(h) => hex(&h.finalize()),
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Where a checksum came from: one the user gave wins over one a server sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksumSource {
    #[default]
    User,
    /// A `Digest` or `Repr-Digest` header.
    Server,
    Metalink,
    Import,
}

/// An expected digest, in lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checksum {
    pub algo: Algo,
    pub hex: String,
    #[serde(default)]
    pub source: ChecksumSource,
}

impl Checksum {
    pub fn new(algo: Algo, hex: &str, source: ChecksumSource) -> Option<Checksum> {
        let hex = hex.trim().to_ascii_lowercase();
        (hex.len() == algo.digest_len() * 2 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then_some(Checksum { algo, hex, source })
    }

    /// Reads a checksum as people paste it:
    /// - a bare hex digest (the algorithm is told by its length),
    /// - `sha256:<hex>`, `SHA-256=<hex>`, `md5 <hex>`, or base64 after the name,
    /// - a line of a `SHA256SUMS` file: `<hex>  name` (or `<hex> *name`),
    /// - BSD style: `SHA256 (name) = <hex>`.
    pub fn parse(input: &str) -> Result<Checksum, String> {
        let s = input.trim();
        if s.is_empty() {
            return Err("The checksum is empty".into());
        }
        // BSD style: ALGO (name) = digest
        if let Some((head, digest)) = s.rsplit_once(") = ")
            && let Some((algo, _)) = head.split_once(" (")
            && let Some(algo) = Algo::from_name(algo)
        {
            return Checksum::new(algo, digest, ChecksumSource::User)
                .ok_or_else(|| format!("That isn't a valid {} checksum", algo.label()));
        }
        // name:value, name=value, or "name value"
        for sep in [':', '=', ' '] {
            if let Some((name, value)) = s.split_once(sep)
                && let Some(algo) = Algo::from_name(name)
            {
                let value = value.trim().trim_matches(':');
                if let Some(c) = Checksum::new(algo, value, ChecksumSource::User) {
                    return Ok(c);
                }
                if let Some(c) = from_base64(algo, value, ChecksumSource::User) {
                    return Ok(c);
                }
                return Err(format!("That isn't a valid {} checksum", algo.label()));
            }
        }
        // The first word: a bare digest, or a SHA256SUMS line.
        let first = s.split_whitespace().next().unwrap_or("");
        if first.bytes().all(|b| b.is_ascii_hexdigit())
            && let Some(algo) = Algo::from_hex_len(first.len())
        {
            return Ok(Checksum::new(algo, first, ChecksumSource::User).expect("checked"));
        }
        Err("K-Star doesn't recognise this checksum. Paste an MD5, SHA-1, SHA-256 or SHA-512 digest.".into())
    }

    /// The best checksum in `Digest` (RFC 3230) and `Repr-Digest` (RFC 9530) headers. Both
    /// describe the whole file, even on a ranged answer (unlike `Content-Digest`).
    pub fn from_headers(digest: Option<&str>, repr_digest: Option<&str>) -> Option<Checksum> {
        let mut found: Vec<Checksum> = Vec::new();
        for v in [digest, repr_digest].into_iter().flatten() {
            for item in v.split(',') {
                let Some((name, value)) = item.split_once('=') else {
                    continue;
                };
                let Some(algo) = Algo::from_name(name.trim()) else {
                    continue;
                };
                // Repr-Digest wraps the value in colons (a structured-field byte sequence).
                let value = value.trim().trim_matches(':');
                if let Some(c) = from_base64(algo, value, ChecksumSource::Server) {
                    found.push(c);
                }
            }
        }
        found.into_iter().max_by_key(|c| c.algo.strength())
    }
}

fn from_base64(algo: Algo, value: &str, source: ChecksumSource) -> Option<Checksum> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .ok()?;
    (bytes.len() == algo.digest_len()).then(|| Checksum {
        algo,
        hex: hex(&bytes),
        source,
    })
}

/// Hashes for each piece of a file (from a metalink), to find which parts are damaged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pieces {
    pub algo: Algo,
    pub length: u64,
    pub hashes: Vec<String>,
}

impl Pieces {
    /// The byte range of piece `i` in a file of `total` bytes.
    pub fn range(&self, i: usize, total: u64) -> (u64, u64) {
        let start = (i as u64).saturating_mul(self.length);
        (
            start.min(total),
            start.saturating_add(self.length).min(total),
        )
    }

    /// Whether these piece hashes can describe a file of `total` bytes.
    pub fn fits(&self, total: u64) -> bool {
        self.length > 0 && (self.hashes.len() as u64) == total.div_ceil(self.length).max(1)
    }
}

/// Reads `len` bytes of `path` from `offset` (all of it when `len` is None) into the hashers.
/// Returns None when cancelled. `progress` counts the bytes read.
pub fn hash_range(
    path: &Path,
    algos: &[Algo],
    offset: u64,
    len: Option<u64>,
    progress: Option<&AtomicU64>,
    cancel: &AtomicBool,
) -> std::io::Result<Option<Vec<String>>> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut hashers: Vec<Hasher> = algos.iter().map(|a| a.hasher()).collect();
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len.unwrap_or(u64::MAX);
    while left > 0 {
        if cancel.load(Relaxed) {
            return Ok(None);
        }
        let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let n = f.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        for h in &mut hashers {
            h.update(&buf[..n]);
        }
        if let Some(p) = progress {
            p.fetch_add(n as u64, Relaxed);
        }
        left -= n as u64;
    }
    Ok(Some(hashers.into_iter().map(Hasher::finish).collect()))
}

/// Hashes a whole file with one algorithm.
pub fn hash_file(
    path: &Path,
    algo: Algo,
    progress: Option<&AtomicU64>,
    cancel: &AtomicBool,
) -> std::io::Result<Option<String>> {
    Ok(hash_range(path, &[algo], 0, None, progress, cancel)?.map(|mut v| v.remove(0)))
}

/// The pieces of a file whose hash doesn't match. Returns None when cancelled.
pub fn bad_pieces(
    path: &Path,
    pieces: &Pieces,
    total: u64,
    progress: Option<&AtomicU64>,
    cancel: &AtomicBool,
) -> std::io::Result<Option<Vec<usize>>> {
    let mut bad = Vec::new();
    for (i, want) in pieces.hashes.iter().enumerate() {
        let (a, b) = pieces.range(i, total);
        match hash_range(path, &[pieces.algo], a, Some(b - a), progress, cancel)? {
            None => return Ok(None),
            Some(got) if !got[0].eq_ignore_ascii_case(want) => bad.push(i),
            Some(_) => {}
        }
    }
    Ok(Some(bad))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn parses_what_people_paste() {
        let c = Checksum::parse(SHA256_ABC).unwrap();
        assert_eq!(c.algo, Algo::Sha256);
        assert_eq!(c.hex, SHA256_ABC);
        let upper = SHA256_ABC.to_uppercase();
        assert_eq!(Checksum::parse(&upper).unwrap().hex, SHA256_ABC);
        assert_eq!(
            Checksum::parse(&format!("sha256:{SHA256_ABC}"))
                .unwrap()
                .algo,
            Algo::Sha256
        );
        assert_eq!(
            Checksum::parse(&format!("SHA-256={SHA256_ABC}"))
                .unwrap()
                .algo,
            Algo::Sha256
        );
        assert_eq!(
            Checksum::parse(&format!("{SHA256_ABC}  ubuntu.iso"))
                .unwrap()
                .hex,
            SHA256_ABC
        );
        assert_eq!(
            Checksum::parse(&format!("{SHA256_ABC} *ubuntu.iso"))
                .unwrap()
                .hex,
            SHA256_ABC
        );
        assert_eq!(
            Checksum::parse(&format!("SHA256 (ubuntu.iso) = {SHA256_ABC}"))
                .unwrap()
                .hex,
            SHA256_ABC
        );
        let md5 = Checksum::parse("900150983cd24fb0d6963f7d28e17f72").unwrap();
        assert_eq!(md5.algo, Algo::Md5);
        let sha1 = Checksum::parse("a9993e364706816aba3e25717850c26c9cd0d89d").unwrap();
        assert_eq!(sha1.algo, Algo::Sha1);
        // base64 after a name
        let b64 = Checksum::parse("sha-256=ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=").unwrap();
        assert_eq!(b64.hex, SHA256_ABC);
        assert!(Checksum::parse("").is_err());
        assert!(Checksum::parse("hello").is_err());
        assert!(Checksum::parse("sha256:abcd").is_err());
        assert!(Checksum::parse("0123456789").is_err());
    }

    #[test]
    fn reads_digest_headers() {
        let c = Checksum::from_headers(
            Some("md5=kAFQmDzST7DWlj99KOF/cg==, SHA-256=ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="),
            None,
        )
        .unwrap();
        assert_eq!(c.algo, Algo::Sha256);
        assert_eq!(c.hex, SHA256_ABC);
        assert_eq!(c.source, ChecksumSource::Server);
        let r = Checksum::from_headers(
            None,
            Some("sha-256=:ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=:"),
        )
        .unwrap();
        assert_eq!(r.hex, SHA256_ABC);
        assert!(Checksum::from_headers(Some("crc32c=abc"), None).is_none());
    }

    #[test]
    fn hashes_files_and_pieces() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        let stop = AtomicBool::new(false);
        assert_eq!(
            hash_file(&p, Algo::Sha256, None, &stop).unwrap().unwrap(),
            SHA256_ABC
        );
        assert_eq!(
            hash_file(&p, Algo::Md5, None, &stop).unwrap().unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        let data: Vec<u8> = (0..10u8).collect();
        std::fs::write(&p, &data).unwrap();
        let piece = |r: &[u8]| {
            let mut h = Algo::Sha1.hasher();
            h.update(r);
            h.finish()
        };
        let mut pieces = Pieces {
            algo: Algo::Sha1,
            length: 4,
            hashes: vec![piece(&data[0..4]), piece(&data[4..8]), piece(&data[8..10])],
        };
        assert!(pieces.fits(10));
        assert_eq!(pieces.range(2, 10), (8, 10));
        assert_eq!(
            bad_pieces(&p, &pieces, 10, None, &stop).unwrap().unwrap(),
            Vec::<usize>::new()
        );
        pieces.hashes[1] = piece(b"nope");
        assert_eq!(
            bad_pieces(&p, &pieces, 10, None, &stop).unwrap().unwrap(),
            vec![1]
        );
        stop.store(true, Relaxed);
        assert!(hash_file(&p, Algo::Sha256, None, &stop).unwrap().is_none());
    }
}
