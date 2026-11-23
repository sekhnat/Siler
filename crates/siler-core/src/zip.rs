//! Looking inside a ZIP archive on a server, and fetching single files out of it.
//!
//! The list comes from the archive's central directory at its end: the last 64 KiB hold the
//! end record (and usually the whole directory), so listing costs one or two range requests.
//! A picked file is an ordinary download of its compressed bytes (with resume and several
//! connections), unpacked and checked against its CRC-32 when complete.

use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use crate::extras::ZipEntry;
use crate::filename;

/// The end record (22 bytes) plus the longest comment.
pub const TAIL: u64 = 22 + 65_535;
/// The largest central directory Siler reads.
pub const MAX_DIRECTORY: u64 = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 200_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZipItem {
    pub path: String,
    pub size: u64,
    pub compressed: u64,
    pub method: u16,
    pub crc: u32,
    pub header_offset: u64,
    pub encrypted: bool,
    pub is_dir: bool,
    /// Why it can't be fetched alone (encrypted, an unsupported method); None when it can.
    pub unsupported: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZipListing {
    pub url: String,
    pub archive_size: u64,
    pub entries: Vec<ZipItem>,
}

impl ZipItem {
    pub fn entry(&self, archive_size: u64) -> ZipEntry {
        ZipEntry {
            path: self.path.clone(),
            archive_size,
            header_offset: self.header_offset,
            compressed: self.compressed,
            size: self.size,
            method: self.method,
            crc: self.crc,
            data_offset: None,
        }
    }
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

const EOCD: u32 = 0x0605_4b50;
const EOCD64: u32 = 0x0606_4b50;
const LOCATOR64: u32 = 0x0706_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const LOCAL: u32 = 0x0403_4b50;

/// Where the central directory is, read from the end of the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The directory's offset, size and entry count.
    Directory { offset: u64, size: u64, count: u64 },
    /// A ZIP64 archive: the ZIP64 end record is at this offset (56 bytes to read).
    Zip64At(u64),
}

/// Finds the end record in the last bytes of an archive (`tail` starts at `tail_start`).
pub fn find_end(tail: &[u8], tail_start: u64) -> Result<End, String> {
    let not_zip = || "This isn't a ZIP archive (its end record wasn't found).".to_string();
    if tail.len() < 22 {
        return Err(not_zip());
    }
    let pos = (0..=tail.len() - 22)
        .rev()
        .find(|&i| u32_at(tail, i) == Some(EOCD))
        .ok_or_else(not_zip)?;
    let disk = u16_at(tail, pos + 4).unwrap_or(0);
    let cd_disk = u16_at(tail, pos + 6).unwrap_or(0);
    let count = u16_at(tail, pos + 10).unwrap_or(0);
    let size = u32_at(tail, pos + 12).unwrap_or(0);
    let offset = u32_at(tail, pos + 16).unwrap_or(0);
    let zip64 = count == 0xFFFF || size == 0xFFFF_FFFF || offset == 0xFFFF_FFFF;
    if zip64 {
        if pos >= 20 && u32_at(tail, pos - 20) == Some(LOCATOR64) {
            let at = u64_at(tail, pos - 20 + 8).ok_or_else(not_zip)?;
            return Ok(End::Zip64At(at));
        }
        if tail_start == 0 && pos < 20 {
            return Err(not_zip());
        }
        return Err("The archive's ZIP64 end record wasn't found.".into());
    }
    if disk != 0 || cd_disk != 0 {
        return Err(
            "This archive is split over several files, which Siler can't look into.".into(),
        );
    }
    Ok(End::Directory {
        offset: u64::from(offset),
        size: u64::from(size),
        count: u64::from(count),
    })
}

/// Reads the ZIP64 end record.
pub fn read_end64(rec: &[u8]) -> Result<End, String> {
    if u32_at(rec, 0) != Some(EOCD64) {
        return Err("The archive's ZIP64 end record is damaged.".into());
    }
    let bad = || "The archive's ZIP64 end record is damaged.".to_string();
    Ok(End::Directory {
        count: u64_at(rec, 32).ok_or_else(bad)?,
        size: u64_at(rec, 40).ok_or_else(bad)?,
        offset: u64_at(rec, 48).ok_or_else(bad)?,
    })
}

/// Decodes a name that isn't marked as UTF-8: UTF-8 if it is valid, else code page 437.
fn decode_name(raw: &[u8], utf8: bool) -> String {
    if utf8 || std::str::from_utf8(raw).is_ok() {
        return String::from_utf8_lossy(raw).into_owned();
    }
    const CP437: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
    let high: Vec<char> = CP437.chars().collect();
    raw.iter()
        .map(|&b| {
            if b < 0x80 {
                b as char
            } else {
                high.get(usize::from(b - 0x80)).copied().unwrap_or('?')
            }
        })
        .collect()
}

/// Reads the central directory.
pub fn parse_directory(cd: &[u8], count: u64) -> Result<Vec<ZipItem>, String> {
    let damaged = || "The archive's list of files is damaged.".to_string();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 46 <= cd.len() && u32_at(cd, i) == Some(CENTRAL) {
        if out.len() >= MAX_ENTRIES {
            return Err(format!("The archive holds more than {MAX_ENTRIES} files."));
        }
        let flags = u16_at(cd, i + 8).ok_or_else(damaged)?;
        let method = u16_at(cd, i + 10).ok_or_else(damaged)?;
        let crc = u32_at(cd, i + 16).ok_or_else(damaged)?;
        let mut compressed = u64::from(u32_at(cd, i + 20).ok_or_else(damaged)?);
        let mut size = u64::from(u32_at(cd, i + 24).ok_or_else(damaged)?);
        let nlen = usize::from(u16_at(cd, i + 28).ok_or_else(damaged)?);
        let xlen = usize::from(u16_at(cd, i + 30).ok_or_else(damaged)?);
        let clen = usize::from(u16_at(cd, i + 32).ok_or_else(damaged)?);
        let mut header_offset = u64::from(u32_at(cd, i + 42).ok_or_else(damaged)?);
        let name = cd.get(i + 46..i + 46 + nlen).ok_or_else(damaged)?;
        let extra = cd
            .get(i + 46 + nlen..i + 46 + nlen + xlen)
            .ok_or_else(damaged)?;
        // ZIP64 extra field: the 64-bit values of the fields that are 0xFFFFFFFF, in order.
        let mut j = 0;
        while j + 4 <= extra.len() {
            let id = u16_at(extra, j).unwrap_or(0);
            let len = usize::from(u16_at(extra, j + 2).unwrap_or(0));
            let data = extra.get(j + 4..j + 4 + len).unwrap_or(&[]);
            if id == 0x0001 {
                let mut k = 0;
                for field in [&mut size, &mut compressed, &mut header_offset] {
                    if *field == 0xFFFF_FFFF {
                        *field = u64_at(data, k).ok_or_else(damaged)?;
                        k += 8;
                    }
                }
            }
            j += 4 + len;
        }
        let path = decode_name(name, flags & 0x0800 != 0);
        let encrypted = flags & 0x0001 != 0;
        let is_dir = path.ends_with('/') || path.ends_with('\\');
        let unsupported = if is_dir {
            None
        } else if encrypted {
            Some("It is encrypted.".to_string())
        } else if !matches!(method, 0 | 8) {
            Some(format!(
                "It is compressed in a way Siler can't unpack (method {method})."
            ))
        } else {
            None
        };
        out.push(ZipItem {
            path,
            size,
            compressed,
            method,
            crc,
            header_offset,
            encrypted,
            is_dir,
            unsupported,
        });
        i += 46 + nlen + xlen + clen;
    }
    if (out.len() as u64) < count.min(MAX_ENTRIES as u64) {
        return Err(damaged());
    }
    Ok(out)
}

/// Where an entry's data starts, from the first 30 bytes of its local header.
pub fn data_offset(local: &[u8], header_offset: u64) -> Result<u64, String> {
    if u32_at(local, 0) != Some(LOCAL) {
        return Err(
            "The file's header in the archive is damaged (the archive may have changed).".into(),
        );
    }
    let n = u64::from(u16_at(local, 26).unwrap_or(0));
    let m = u64::from(u16_at(local, 28).unwrap_or(0));
    Ok(header_offset + 30 + n + m)
}

/// Where an entry goes below the chosen folder: its folders and its file name, made safe.
/// None for names that climb out (`..`) or are empty.
pub fn target(path: &str) -> Option<(Vec<String>, String)> {
    if path.ends_with(['/', '\\']) {
        return None;
    }
    let mut parts = Vec::new();
    for c in path.split(['/', '\\']) {
        match c {
            "" | "." => {}
            ".." => return None,
            c => parts.push(filename::sanitize(c)?),
        }
    }
    let name = parts.pop()?;
    Some((parts, name))
}

/// Unpacks the compressed bytes in `part` into `out`, a new empty file (a stored entry is
/// only checked, and needs none), and checks the size and CRC-32. Returns false when
/// cancelled.
pub fn unpack(
    part: &Path,
    out: Option<std::fs::File>,
    e: &ZipEntry,
    progress: &AtomicU64,
    cancel: &AtomicBool,
) -> Result<bool, String> {
    let io = |err: std::io::Error| format!("Unpacking failed: {err}");
    let input = std::fs::File::open(part).map_err(io)?;
    let input = BufReader::new(input).take(e.compressed);
    let mut reader: Box<dyn Read> = match e.method {
        0 => Box::new(input),
        8 => Box::new(flate2::read::DeflateDecoder::new(input)),
        m => return Err(format!("Siler can't unpack compression method {m}.")),
    };
    let mut writer = if e.method == 0 {
        None
    } else {
        let out = out.ok_or("Unpacking failed: nowhere to unpack to")?;
        Some(std::io::BufWriter::new(out))
    };
    let mut crc = crc32fast::Hasher::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        if cancel.load(Relaxed) {
            return Ok(false);
        }
        let n = reader
            .read(&mut buf)
            .map_err(|err| format!("The file in the archive is damaged: {err}"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > e.size {
            return Err("The file in the archive is larger than the archive says.".into());
        }
        crc.update(&buf[..n]);
        if let Some(w) = writer.as_mut() {
            w.write_all(&buf[..n]).map_err(io)?;
        }
        progress.store(total, Relaxed);
    }
    if let Some(mut w) = writer {
        w.flush().map_err(io)?;
        w.get_ref().sync_all().map_err(io)?;
    }
    if total != e.size {
        return Err(format!(
            "The file in the archive is damaged: {total} bytes instead of {}.",
            e.size
        ));
    }
    if crc.finalize() != e.crc {
        return Err("The file in the archive is damaged (its CRC-32 doesn't match).".into());
    }
    Ok(true)
}

/// A tiny ZIP writer (stored and deflated entries, optionally ZIP64 records), for tests.
/// A tiny ZIP writer for tests: stored and deflated entries, optionally ZIP64 records.
#[doc(hidden)]
pub fn make_zip(files: &[(&str, &[u8], bool)], zip64: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data, deflate) in files {
        let crc = crc32fast::hash(data);
        let body = if *deflate {
            let mut enc =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(data).unwrap();
            enc.finish().unwrap()
        } else {
            data.to_vec()
        };
        let method: u16 = if *deflate { 8 } else { 0 };
        let offset = out.len() as u32;
        out.extend_from_slice(&LOCAL.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0x0800u16.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&3u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b"xyz");
        out.extend_from_slice(&body);

        central.extend_from_slice(&CENTRAL.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0x0800u16.to_le_bytes());
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&[0; 4]);
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(body.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len() as u64;
    out.extend_from_slice(&central);
    let n = files.len() as u64;
    if zip64 {
        let rec = out.len() as u64;
        out.extend_from_slice(&EOCD64.to_le_bytes());
        out.extend_from_slice(&44u64.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&(central.len() as u64).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&LOCATOR64.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&rec.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
    }
    out.extend_from_slice(&EOCD.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    let (count, size, off) = if zip64 {
        (0xFFFFu16, 0xFFFF_FFFFu32, 0xFFFF_FFFFu32)
    } else {
        (n as u16, central.len() as u32, cd_offset as u32)
    };
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&off.to_le_bytes());
    out.extend_from_slice(&5u16.to_le_bytes());
    out.extend_from_slice(b"hello");
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn list(zip: &[u8]) -> Vec<ZipItem> {
        let start = zip.len().saturating_sub(TAIL as usize);
        let end = match find_end(&zip[start..], start as u64).unwrap() {
            End::Zip64At(at) => read_end64(&zip[at as usize..at as usize + 56]).unwrap(),
            e => e,
        };
        let End::Directory {
            offset,
            size,
            count,
        } = end
        else {
            unreachable!()
        };
        parse_directory(&zip[offset as usize..(offset + size) as usize], count).unwrap()
    }

    #[test]
    fn lists_and_unpacks() {
        let big: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        for zip64 in [false, true] {
            let zip = make_zip(
                &[
                    ("docs/", b"", false),
                    ("docs/readme.txt", b"hello zip", false),
                    ("data/big.bin", &big, true),
                ],
                zip64,
            );
            let items = list(&zip);
            assert_eq!(items.len(), 3);
            assert!(items[0].is_dir);
            assert_eq!(items[1].path, "docs/readme.txt");
            assert_eq!(items[2].method, 8);
            assert_eq!(items[2].size, big.len() as u64);
            assert!(items[2].compressed < items[2].size);

            let dir = tempfile::tempdir().unwrap();
            for item in &items[1..] {
                let mut e = item.entry(zip.len() as u64);
                let off = item.header_offset as usize;
                let data = data_offset(&zip[off..off + 30], item.header_offset).unwrap();
                e.data_offset = Some(data);
                let part = dir.path().join("part");
                std::fs::write(&part, &zip[data as usize..(data + e.compressed) as usize]).unwrap();
                let out = dir.path().join("out");
                let p = AtomicU64::new(0);
                let file = || Some(crate::partfile::replace(&out).unwrap());
                assert!(unpack(&part, file(), &e, &p, &AtomicBool::new(false)).unwrap());
                let got = if e.method == 0 {
                    std::fs::read(&part).unwrap()
                } else {
                    std::fs::read(&out).unwrap()
                };
                let want: &[u8] = if e.method == 0 { b"hello zip" } else { &big };
                assert_eq!(got, want);
                // A damaged copy is caught.
                let mut bad = std::fs::read(&part).unwrap();
                let last = bad.len() - 1;
                bad[last] ^= 0xff;
                std::fs::write(&part, bad).unwrap();
                assert!(unpack(&part, file(), &e, &p, &AtomicBool::new(false)).is_err());
            }
        }
    }

    #[test]
    fn safe_targets_and_names() {
        assert_eq!(
            target("a/b/c.txt"),
            Some((vec!["a".into(), "b".into()], "c.txt".into()))
        );
        assert_eq!(target("../../etc/passwd"), None);
        assert_eq!(target("dir/"), None);
        assert_eq!(target("/abs/x"), Some((vec!["abs".into()], "x".into())));
        assert_eq!(decode_name(&[0x82, b'a'], false), "éa");
        assert!(find_end(b"not a zip at all, sorry", 0).is_err());
    }
}
