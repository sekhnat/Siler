//! Takes secrets out of text bound for a log file.
//!
//! Addresses often carry them: signed CDN links keep their signature in the query or in a
//! path segment, and `user:password@` logins sit in the authority. Header dumps carry
//! cookies and logins. [`text`] rewrites all of these and leaves everything else alone,
//! so a log can be pasted into a public bug report. It is applied where log lines are
//! written, not where they are made, so error texts from other crates are covered too.

use std::borrow::Cow;

/// What a removed value is replaced with.
const MASK: &str = "…";
/// What a removed login or header value is replaced with.
const HIDDEN: &str = "***";

const SCHEMES: &[&str] = &[
    "https", "http", "ftps", "ftp", "sftp", "wss", "ws", "rtmps", "rtmp", "mms",
];

/// Headers whose values are secrets, longest first so a shorter name never matches inside
/// a longer one.
const SECRET_HEADERS: &[&str] = &[
    "proxy-authorization",
    "authorization",
    "set-cookie",
    "cookie",
];

/// `text` with logins, query values, fragments, token-like path segments and secret header
/// values removed. Parameter names and ordinary file names stay: they are what makes a log
/// useful.
pub fn text(s: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(s);
    for pass in [headers as fn(&str) -> Option<String>, debug_urls, urls] {
        if let Some(changed) = pass(&out) {
            out = Cow::Owned(changed);
        }
    }
    out
}

/// One address, with its secrets removed.
pub fn url(u: &str) -> String {
    text(u).into_owned()
}

/// Rewrites the spans `find` reports, `(start, end, replacement)`, left to right.
fn rewrite(
    s: &str,
    mut find: impl FnMut(&str, usize) -> Option<(usize, usize, String)>,
) -> Option<String> {
    let mut out = String::new();
    let mut last = 0;
    let mut i = 0;
    while i < s.len() {
        if !s.is_char_boundary(i) {
            i += 1;
            continue;
        }
        match find(s, i) {
            Some((start, end, with)) => {
                if s[start..end] != with {
                    out.push_str(&s[last..start]);
                    out.push_str(&with);
                    last = end;
                }
                i = end.max(i + 1);
            }
            None => i += 1,
        }
    }
    (last > 0).then(|| {
        out.push_str(&s[last..]);
        out
    })
}

/// Whether something starting at `i` starts a word: `pred` says which bytes would join it
/// to the word before. A terminal colour code (`ESC[3m`) right before it doesn't.
fn word_start(b: &[u8], i: usize, joins: impl Fn(u8) -> bool) -> bool {
    if i == 0 || !joins(b[i - 1]) {
        return true;
    }
    if b[i - 1] != b'm' {
        return false;
    }
    let mut k = i - 1;
    while k > 0 && (b[k - 1].is_ascii_digit() || b[k - 1] == b';') {
        k -= 1;
    }
    k >= 2 && b[k - 1] == b'[' && b[k - 2] == 0x1b
}

fn starts_with_ignore_case(s: &[u8], prefix: &str) -> bool {
    s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

// ------------------------------------------------------------------ addresses

fn urls(s: &str) -> Option<String> {
    rewrite(s, |s, i| {
        let b = s.as_bytes();
        if !word_start(b, i, |c| {
            c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'+' | b'.')
        }) {
            return None;
        }
        let rest = &b[i..];
        let prefix = if starts_with_ignore_case(rest, "magnet:?") {
            "magnet:?".len()
        } else {
            let scheme = SCHEMES.iter().find(|sc| {
                starts_with_ignore_case(rest, sc) && rest[sc.len()..].starts_with(b"://")
            })?;
            scheme.len() + 3
        };
        let end = url_end(s, i);
        if end <= i + prefix {
            return None;
        }
        Some((i, end, mask_url(&s[i..end], prefix)))
    })
}

/// Where an address starting at `start` ends: at a space, a quote or another character
/// that can't be in one, less trailing punctuation that belongs to the sentence.
fn url_end(s: &str, start: usize) -> usize {
    let b = s.as_bytes();
    let mut end = start;
    while end < b.len() {
        let c = b[end];
        if c.is_ascii_whitespace()
            || c.is_ascii_control()
            || matches!(
                c,
                b'"' | b'\'' | b'<' | b'>' | b'`' | b'\\' | b'{' | b'}' | b'|' | b'^'
            )
        {
            break;
        }
        end += 1;
    }
    loop {
        let url = &s[start..end];
        let Some(&last) = url.as_bytes().last() else {
            return end;
        };
        let unbalanced =
            |open: char, close: char| url.matches(close).count() > url.matches(open).count();
        let trim = match last {
            b'.' | b',' | b';' | b':' | b'!' | b'?' => true,
            b')' => unbalanced('(', ')'),
            b']' => unbalanced('[', ']'),
            _ => false,
        };
        if !trim {
            return end;
        }
        end -= 1;
    }
}

fn mask_url(url: &str, prefix: usize) -> String {
    let (head, rest) = url.split_at(prefix);
    let mut out = String::from(head);
    if head.ends_with('?') {
        // A magnet link is all query.
        out.push_str(&mask_query(rest));
        return out;
    }
    let (rest, _fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (rest, query) = match rest.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (rest, None),
    };
    let (authority, path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
    match authority.rfind('@') {
        Some(at) => {
            out.push_str(HIDDEN);
            out.push_str(&authority[at..]);
        }
        None => out.push_str(authority),
    }
    let mut first = true;
    for seg in path.split('/') {
        if !first {
            out.push('/');
        }
        first = false;
        out.push_str(if token_like(seg) { MASK } else { seg });
    }
    if let Some(q) = query {
        out.push('?');
        out.push_str(&mask_query(q));
    }
    out
}

/// Every value replaced, every name kept: `Expires=…&Signature=…`.
fn mask_query(q: &str) -> String {
    q.split('&')
        .map(|p| match p.split_once('=') {
            Some((name, "")) => format!("{name}="),
            Some((name, _)) => format!("{name}={MASK}"),
            None if p.len() <= 16
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') =>
            {
                p.to_string()
            }
            None => MASK.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// A path segment that is a signature or a key rather than a name: long, made of URL-safe
/// token characters, with a long run that mixes letters and digits. File names such as
/// `ubuntu-24.04.3-desktop-amd64.iso` are broken up by separators and don't qualify.
fn token_like(seg: &str) -> bool {
    if seg.len() < 32
        || !seg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-=.~%+,*".contains(&b))
    {
        return false;
    }
    seg.split(['-', '_', '.', '~']).any(|part| {
        let letters = part.bytes().any(|b| b.is_ascii_alphabetic());
        let digits = part.bytes().any(|b| b.is_ascii_digit());
        part.len() >= 24 && ((letters && digits) || part.len() >= 40)
    })
}

// ------------------------------------------------------------ Debug of a Url

/// `Url`'s `Debug` form spells the parts out: `password: Some("…")`, `query: Some("…")`.
fn debug_urls(s: &str) -> Option<String> {
    rewrite(s, |s, i| {
        let rest = &s[i..];
        let (marker, query) = if rest.starts_with("password: Some(\"") {
            ("password: Some(\"", false)
        } else if rest.starts_with("query: Some(\"") {
            ("query: Some(\"", true)
        } else if rest.starts_with("fragment: Some(\"") {
            ("fragment: Some(\"", false)
        } else {
            return None;
        };
        let start = i + marker.len();
        let end = start + quoted_len(&s[start..]);
        let with = if query {
            mask_query(&s[start..end])
        } else {
            HIDDEN.to_string()
        };
        Some((start, end, with))
    })
}

/// The length of a quoted string's contents, up to its unescaped closing quote.
fn quoted_len(s: &str) -> usize {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i,
            _ => i += 1,
        }
    }
    b.len()
}

// ------------------------------------------------------------------ headers

/// The values of `Authorization`, `Proxy-Authorization`, `Cookie` and `Set-Cookie`, as
/// `name: value` lines, `"name": "value"` pairs (a header map's `Debug`) and `name=value`
/// log fields.
fn headers(s: &str) -> Option<String> {
    rewrite(s, |s, i| {
        let b = s.as_bytes();
        if !word_start(b, i, |c| {
            c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')
        }) {
            return None;
        }
        let name = SECRET_HEADERS
            .iter()
            .find(|n| starts_with_ignore_case(&b[i..], n))?;
        let mut j = i + name.len();
        if b.get(j)
            .is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return None;
        }
        let quoted_name = i > 0 && b[i - 1] == b'"' && b.get(j) == Some(&b'"');
        if quoted_name {
            j += 1;
        }
        // Spaces, and a terminal's colour codes around a log field's `=`.
        let skip_spaces = |mut k: usize| {
            loop {
                if b.get(k) == Some(&b' ') {
                    k += 1;
                } else if b.get(k) == Some(&0x1b) && b.get(k + 1) == Some(&b'[') {
                    let mut e = k + 2;
                    while b.get(e).is_some_and(|c| c.is_ascii_digit() || *c == b';') {
                        e += 1;
                    }
                    if b.get(e) != Some(&b'm') {
                        return k;
                    }
                    k = e + 1;
                } else {
                    return k;
                }
            }
        };
        j = skip_spaces(j);
        let sep = *b.get(j)?;
        if sep != b':' && sep != b'=' {
            return None;
        }
        j = skip_spaces(j + 1);
        if b.get(j) == Some(&b'"') {
            let start = j + 1;
            let end = start + quoted_len(&s[start..]);
            return Some((start, end, HIDDEN.to_string()));
        }
        let start = j;
        let mut end = start;
        while end < b.len() && b[end] != b'\n' && b[end] != b'\r' {
            // A log field ends at the next space; a header line runs to the end of it.
            if sep == b'=' && (b[end] == b' ' || b[end] == b',' || b[end] == b'}') {
                break;
            }
            end += 1;
        }
        (end > start).then(|| (start, end, HIDDEN.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::text;

    fn r(s: &str) -> String {
        text(s).into_owned()
    }

    #[test]
    fn plain_text_and_plain_addresses_are_unchanged() {
        for s in [
            "download 12: finished in 3.2 s",
            "https://example.com/ubuntu-24.04.iso",
            "https://releases.ubuntu.com/24.04/ubuntu-24.04.3-desktop-amd64.iso",
            "ftp://ftp.example.org/pub/linux/kernel/v6.x/linux-6.12.tar.xz",
            "https://example.com/путь/файл.zip",
            "https://[2001:db8::1]:8443/a/b.bin",
            "http-equiv, httpd, https: no address here",
        ] {
            assert_eq!(r(s), s, "{s}");
            assert!(matches!(text(s), std::borrow::Cow::Borrowed(_)));
        }
    }

    #[test]
    fn s3_presigned_keeps_names_drops_values() {
        let s = "GET https://bucket.s3.amazonaws.com/f.zip?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIAEXAMPLE%2F20261124&X-Amz-Signature=8f2c1b9e0d4a7c6b5e3f2a1d0c9b8a7f6e5d4c3b2a19087f6e5d4c3b2a190876 failed";
        assert_eq!(
            r(s),
            "GET https://bucket.s3.amazonaws.com/f.zip?X-Amz-Algorithm=…&X-Amz-Credential=…&X-Amz-Signature=… failed"
        );
    }

    #[test]
    fn cloudfront_and_googlevideo() {
        let cf = "https://d111111abcdef8.cloudfront.net/v.mp4?Expires=1700000000&Signature=abcDEF123~xyz__&Key-Pair-Id=K2JCJMDEHXQW5F";
        assert_eq!(
            r(cf),
            "https://d111111abcdef8.cloudfront.net/v.mp4?Expires=…&Signature=…&Key-Pair-Id=…"
        );
        let gv = "media https://rr3---sn-abc.googlevideo.com/videoplayback?expire=1&ei=xyz&sig=AOq0QJ8w&lsig=AG3C_x";
        assert_eq!(
            r(gv),
            "media https://rr3---sn-abc.googlevideo.com/videoplayback?expire=…&ei=…&sig=…&lsig=…"
        );
    }

    #[test]
    fn path_tokens_are_masked() {
        let akamai = "https://cdn.example.net/hdnts=exp=1700000000~acl=*~hmac=0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f6071/video/master.m3u8";
        assert_eq!(r(akamai), "https://cdn.example.net/…/video/master.m3u8");
        let signed = "https://x.example/dl/Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MGFiY2RlZg/file.bin";
        assert_eq!(r(signed), "https://x.example/dl/…/file.bin");
    }

    #[test]
    fn logins_are_hidden() {
        assert_eq!(
            r("probe https://alice:s3cret@example.com/a.iso"),
            "probe https://***@example.com/a.iso"
        );
        assert_eq!(
            r("ftp://bob:hunter2@ftp.example.org/pub/x.tar"),
            "ftp://***@ftp.example.org/pub/x.tar"
        );
        assert_eq!(r("sftp://carol@host/x"), "sftp://***@host/x");
    }

    #[test]
    fn fragment_is_dropped_and_magnet_values_masked() {
        assert_eq!(
            r("https://a.example/p#access_token=xyz"),
            "https://a.example/p"
        );
        assert_eq!(
            r("magnet:?xt=urn:btih:abcdef&dn=name&tr=udp://t.example:80"),
            "magnet:?xt=…&dn=…&tr=…"
        );
    }

    #[test]
    fn sentence_punctuation_is_not_part_of_the_address() {
        assert_eq!(
            r("see https://a.example/f.iso?token=abc."),
            "see https://a.example/f.iso?token=…."
        );
        assert_eq!(
            r("error sending request for url (https://a.example/f?sig=xyz)"),
            "error sending request for url (https://a.example/f?sig=…)"
        );
        assert_eq!(
            r("(https://en.example.org/wiki/A_(b)?k=v)"),
            "(https://en.example.org/wiki/A_(b)?k=…)"
        );
    }

    #[test]
    fn reqwest_error_texts() {
        let display = "error sending request for url (https://u:p@host.example/x?token=abc): connection refused";
        assert_eq!(
            r(display),
            "error sending request for url (https://***@host.example/x?token=…): connection refused"
        );
        let debug = r#"reqwest::Error { kind: Request, url: "https://host.example/x?token=abc", source: Os }"#;
        assert_eq!(
            r(debug),
            r#"reqwest::Error { kind: Request, url: "https://host.example/x?token=…", source: Os }"#
        );
    }

    #[test]
    fn url_debug_form() {
        let s = r#"Url { scheme: "https", username: "u", password: Some("pw"), host: Some(Domain("h")), path: "/a", query: Some("sig=abc&v=1"), fragment: Some("frag") }"#;
        assert_eq!(
            r(s),
            r#"Url { scheme: "https", username: "u", password: Some("***"), host: Some(Domain("h")), path: "/a", query: Some("sig=…&v=…"), fragment: Some("***") }"#
        );
    }

    #[test]
    fn colour_codes_before_an_address() {
        assert_eq!(
            r("\x1b[3murl\x1b[0m\x1b[2m=\x1b[0mhttps://a.example/f?sig=x"),
            "\x1b[3murl\x1b[0m\x1b[2m=\x1b[0mhttps://a.example/f?sig=…"
        );
    }

    #[test]
    fn header_lines_and_maps() {
        assert_eq!(
            r("Authorization: Basic YWxpY2U6czNjcmV0"),
            "Authorization: ***"
        );
        assert_eq!(
            r("Proxy-Authorization: Digest username=\"a\"\nnext line"),
            "Proxy-Authorization: ***\nnext line"
        );
        assert_eq!(
            r(
                r#"headers={"set-cookie": "sid=abc; Path=/", "content-type": "text/html", "cookie": "a=b"}"#
            ),
            r#"headers={"set-cookie": "***", "content-type": "text/html", "cookie": "***"}"#
        );
        assert_eq!(r("sent cookie=sid123 to host"), "sent cookie=*** to host");
        // A terminal's colour codes around a log field.
        assert_eq!(
            r("\x1b[3mcookie\x1b[0m\x1b[2m=\x1b[0msid123 x"),
            "\x1b[3mcookie\x1b[0m\x1b[2m=\x1b[0m*** x"
        );
        // Words that merely contain a header name stay.
        assert_eq!(
            r("cookies: 3, cookie-jar ready"),
            "cookies: 3, cookie-jar ready"
        );
    }
}
