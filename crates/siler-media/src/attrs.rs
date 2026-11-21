//! Attribute lists of HLS tags: `NAME=value,NAME="quoted, value"`.

/// Splits an attribute list into `(NAME, value)` pairs. Quoted values are unquoted and may
/// contain commas. Malformed pieces are skipped rather than failing the whole tag.
pub(crate) fn parse(list: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = list.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        while i < b.len() && b[i] != b'=' && b[i] != b',' {
            i += 1;
        }
        let name = list[start..i].trim().to_ascii_uppercase();
        if i >= b.len() {
            break;
        }
        if b[i] == b',' {
            i += 1;
            continue;
        }
        // Skip '='.
        i += 1;
        let value = if i < b.len() && b[i] == b'"' {
            i += 1;
            let vs = i;
            while i < b.len() && b[i] != b'"' {
                i += 1;
            }
            let v = list[vs..i].to_string();
            // Past the closing quote, up to the next comma.
            i += 1;
            while i < b.len() && b[i] != b',' {
                i += 1;
            }
            v
        } else {
            let vs = i;
            while i < b.len() && b[i] != b',' {
                i += 1;
            }
            list[vs..i].trim().to_string()
        };
        if !name.is_empty() {
            out.push((name, value));
        }
        // Skip the comma.
        i += 1;
    }
    out
}

/// The value of attribute `name` (upper case), if present.
pub(crate) fn get<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_values_and_commas() {
        let a = parse(
            r#"BANDWIDTH=2000000,CODECS="avc1.4d401f,mp4a.40.2",RESOLUTION=1280x720,NAME="a, b""#,
        );
        assert_eq!(get(&a, "BANDWIDTH"), Some("2000000"));
        assert_eq!(get(&a, "CODECS"), Some("avc1.4d401f,mp4a.40.2"));
        assert_eq!(get(&a, "RESOLUTION"), Some("1280x720"));
        assert_eq!(get(&a, "NAME"), Some("a, b"));
    }

    #[test]
    fn malformed_pieces_are_skipped() {
        let a = parse(",=x,LONE,KEY=,A=\"unterminated");
        assert_eq!(get(&a, "KEY"), Some(""));
        assert_eq!(get(&a, "A"), Some("unterminated"));
        assert_eq!(get(&a, "LONE"), None);
        assert!(parse("").is_empty());
    }
}
