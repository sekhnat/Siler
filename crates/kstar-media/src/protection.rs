//! Protection assessment (design §7.5): which encryption a track uses, and whether K-Star
//! may download it. DRM is refused; AES-128 is decrypted with the key the server gives.

use crate::model::Protection;

/// The known DRM system names, by the UUID in a DASH `ContentProtection` scheme.
pub fn system_name(uuid: &str) -> String {
    match uuid.to_ascii_lowercase().as_str() {
        "edef8ba9-79d6-4ace-a3c8-27dcd51d21ed" => "Widevine".into(),
        "9a04f079-9840-4286-ab92-e65be0885f95" => "PlayReady".into(),
        "94ce86fb-07ff-4f43-adb8-93d2fa968ca2" => "FairPlay".into(),
        "e2719d58-a985-b3c9-781a-b030af78d30e" | "1077efec-c0b2-4d02-ace3-323c399eaa9f" => {
            "ClearKey".into()
        }
        other => format!("unknown system {other}"),
    }
}

/// HLS `EXT-X-KEY` (or `EXT-X-SESSION-KEY`): the protection it implies.
pub fn hls_key(method: &str, format: Option<&str>, uri: Option<&str>) -> Protection {
    let method = method.trim().to_ascii_uppercase();
    match method.as_str() {
        "NONE" => Protection::Clear,
        "AES-128" => Protection::Aes128,
        "SAMPLE-AES" | "SAMPLE-AES-CTR" => {
            let format = format.unwrap_or("identity");
            let skd = uri.is_some_and(|u| u.trim_start().to_ascii_lowercase().starts_with("skd:"));
            if format.eq_ignore_ascii_case("identity") && !skd {
                Protection::Unsupported { method }
            } else {
                Protection::Drm {
                    systems: vec![key_format_name(format)],
                }
            }
        }
        _ => Protection::Unsupported { method },
    }
}

fn key_format_name(format: &str) -> String {
    match format {
        "com.apple.streamingkeydelivery" => "FairPlay".into(),
        "com.microsoft.playready" => "PlayReady".into(),
        f if f.to_ascii_lowercase().starts_with("urn:uuid:") => {
            system_name(&f["urn:uuid:".len()..])
        }
        f => format!("key format {f}"),
    }
}

/// One DASH `ContentProtection` element. `None` for schemes that don't affect playback.
pub fn dash_scheme(scheme_uri: &str) -> Option<Protection> {
    let s = scheme_uri.trim().to_ascii_lowercase();
    if s == "urn:mpeg:dash:mp4protection:2011" {
        return Some(Protection::Drm {
            systems: vec!["CENC".into()],
        });
    }
    if let Some(uuid) = s.strip_prefix("urn:uuid:") {
        return Some(Protection::Drm {
            systems: vec![system_name(uuid)],
        });
    }
    if s.is_empty() {
        return None;
    }
    // A protection scheme we don't know: treat it as not downloadable.
    if s.contains("protection") || s.contains("drm") || s.contains("encrypt") {
        return Some(Protection::Unsupported {
            method: scheme_uri.trim().to_string(),
        });
    }
    None
}

/// Finds the encryption boxes of an initialisation segment (`encv`/`enca` sample entries,
/// `tenc`, `pssh`). Walks the ISO BMFF box tree; malformed sizes stop the walk.
pub fn init_protection(init: &[u8]) -> Option<&'static str> {
    scan(init, 0)
}

fn scan(data: &[u8], depth: u8) -> Option<&'static str> {
    if depth > 8 {
        return None;
    }
    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        let size32 =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as u64;
        let kind = &data[pos + 4..pos + 8];
        let (header, size) = match size32 {
            0 => (8u64, (data.len() - pos) as u64),
            1 => {
                if pos + 16 > data.len() {
                    return None;
                }
                let mut b = [0u8; 8];
                b.copy_from_slice(&data[pos + 8..pos + 16]);
                (16, u64::from_be_bytes(b))
            }
            n => (8, n),
        };
        if size < header || pos as u64 + size > data.len() as u64 {
            return None;
        }
        let body = &data[pos + header as usize..pos + size as usize];
        match kind {
            b"encv" => return Some("encv"),
            b"enca" => return Some("enca"),
            b"pssh" => return Some("pssh"),
            b"tenc" => return Some("tenc"),
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"moof" | b"traf" => {
                if let Some(found) = scan(body, depth + 1) {
                    return Some(found);
                }
            }
            b"stsd" => {
                // version, flags and the entry count precede the sample entries.
                if let Some(found) = body.get(8..).and_then(|b| scan(b, depth + 1)) {
                    return Some(found);
                }
            }
            _ => {}
        }
        pos += size as usize;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_keys() {
        assert_eq!(hls_key("NONE", None, None), Protection::Clear);
        assert_eq!(
            hls_key("AES-128", None, Some("https://k/x")),
            Protection::Aes128
        );
        assert_eq!(
            hls_key("SAMPLE-AES", Some("identity"), None),
            Protection::Unsupported {
                method: "SAMPLE-AES".into()
            }
        );
        assert!(hls_key("SAMPLE-AES", None, None) != Protection::Aes128);
        assert!(matches!(
            hls_key("SAMPLE-AES", Some("com.apple.streamingkeydelivery"), None),
            Protection::Drm { systems } if systems == ["FairPlay"]
        ));
        assert!(hls_key("SAMPLE-AES", Some("identity"), Some("skd://x")).is_drm());
        assert!(
            hls_key(
                "SAMPLE-AES-CTR",
                Some("urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"),
                None
            )
            .is_drm()
        );
        assert!(matches!(
            hls_key("WHATEVER", None, None),
            Protection::Unsupported { .. }
        ));
    }

    #[test]
    fn dash_schemes() {
        assert!(
            dash_scheme("urn:uuid:EDEF8BA9-79D6-4ACE-A3C8-27DCD51D21ED")
                .unwrap()
                .is_drm()
        );
        assert!(
            dash_scheme("urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95")
                .unwrap()
                .is_drm()
        );
        assert!(
            dash_scheme("urn:uuid:94ce86fb-07ff-4f43-adb8-93d2fa968ca2")
                .unwrap()
                .is_drm()
        );
        assert!(
            dash_scheme("urn:uuid:1077efec-c0b2-4d02-ace3-323c399eaa9f")
                .unwrap()
                .is_drm()
        );
        assert!(
            dash_scheme("urn:uuid:12345678-0000-0000-0000-000000000000")
                .unwrap()
                .is_drm()
        );
        assert!(
            dash_scheme("urn:mpeg:dash:mp4protection:2011")
                .unwrap()
                .is_drm()
        );
        assert_eq!(dash_scheme("urn:mpeg:dash:role:2011"), None);
        assert!(matches!(
            dash_scheme("urn:example:encryption:x"),
            Some(Protection::Unsupported { .. })
        ));
    }

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn init_segment_boxes() {
        let clear = [
            bx(b"ftyp", b"isom"),
            bx(
                b"moov",
                &bx(
                    b"trak",
                    &bx(
                        b"mdia",
                        &bx(
                            b"minf",
                            &bx(
                                b"stbl",
                                &bx(
                                    b"stsd",
                                    &[&[0u8; 8][..], &bx(b"avc1", &[0; 20])[..]].concat(),
                                ),
                            ),
                        ),
                    ),
                ),
            ),
        ]
        .concat();
        assert_eq!(init_protection(&clear), None);
        let enc = [bx(
            b"moov",
            &bx(
                b"trak",
                &bx(
                    b"mdia",
                    &bx(
                        b"minf",
                        &bx(
                            b"stbl",
                            &bx(
                                b"stsd",
                                &[&[0u8; 8][..], &bx(b"encv", &[0; 20])[..]].concat(),
                            ),
                        ),
                    ),
                ),
            ),
        )]
        .concat();
        assert_eq!(init_protection(&enc), Some("encv"));
        let pssh = bx(b"moov", &bx(b"pssh", &[0; 16]));
        assert_eq!(init_protection(&pssh), Some("pssh"));
        // A size that runs past the end stops the walk instead of reading out of bounds.
        let mut broken = bx(b"moov", &[]);
        broken[3] = 0xff;
        assert_eq!(init_protection(&broken), None);
        assert_eq!(init_protection(&[]), None);
    }
}
