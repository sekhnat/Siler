//! Links in HTML and CSS: found while crawling, rewritten when a site is saved for offline
//! browsing. One pass over the document does both, so the links found are exactly the links
//! later rewritten.

use lol_html::html_content::ContentType;
use lol_html::{AsciiCompatibleEncoding, HtmlRewriter, Settings, element, text};
use std::cell::RefCell;
use url::Url;

/// What a link is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Another page (a link, a frame, a refresh).
    Page,
    /// Something the page shows or uses (an image, a style sheet, a script, a video).
    Asset,
}

/// Decodes the entities that occur in URLs in attributes (`&amp;`, numeric references…).
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail[1..].find(';').map(|e| e + 1).filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let name = &tail[1..e];
            let c = match name {
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "nbsp" => Some('\u{a0}'),
                n if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16)
                    .ok()
                    .and_then(char::from_u32),
                n if n.starts_with('#') => n[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            };
            c.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Calls `f` for every `url(…)` and `@import "…"` in some CSS; where `f` returns a new value,
/// it replaces the old one. Returns the CSS (rewritten when `f` changed something).
pub fn css_urls(css: &str, f: &mut dyn FnMut(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    loop {
        let url_at = find_ci(rest, "url(");
        let import_at = find_ci(rest, "@import");
        let next = match (url_at, import_at) {
            (Some(u), Some(i)) if i < u => Some((i, true)),
            (Some(u), _) => Some((u, false)),
            (None, Some(i)) => Some((i, true)),
            (None, None) => None,
        };
        let Some((at, import)) = next else {
            out.push_str(rest);
            return out;
        };
        if import {
            // @import "x.css"; (the url( form is handled as a url() below)
            let after = &rest[at + 7..];
            let trimmed = after.trim_start();
            let skipped = after.len() - trimmed.len();
            let quote = trimmed.chars().next();
            if let Some(q @ ('"' | '\'')) = quote
                && let Some(end) = trimmed[1..].find(q)
            {
                let value = &trimmed[1..1 + end];
                out.push_str(&rest[..at + 7 + skipped + 1]);
                out.push_str(&f(value.trim()).unwrap_or_else(|| value.to_string()));
                out.push(q);
                rest = &trimmed[1 + end + 1..];
            } else {
                out.push_str(&rest[..at + 7]);
                rest = after;
            }
            continue;
        }
        let after = &rest[at + 4..];
        let Some(close) = after.find(')') else {
            out.push_str(rest);
            return out;
        };
        let raw = &after[..close];
        let inner = raw.trim();
        let (q, value) = match inner.chars().next() {
            Some(q @ ('"' | '\'')) if inner.len() >= 2 && inner.ends_with(q) => {
                (Some(q), &inner[1..inner.len() - 1])
            }
            _ => (None, inner),
        };
        out.push_str(&rest[..at + 4]);
        match (!value.starts_with("data:")).then(|| f(value)).flatten() {
            Some(new) => {
                let q = q.unwrap_or('"');
                out.push(q);
                out.push_str(&new);
                out.push(q);
            }
            None => out.push_str(raw),
        }
        out.push(')');
        rest = &after[close + 1..];
    }
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    hay.as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// The URLs of a `srcset` (`a.jpg 1x, b.jpg 2x`), rewritten by `f`.
fn srcset(value: &str, f: &mut dyn FnMut(&str) -> Option<String>) -> String {
    value
        .split(',')
        .map(|cand| {
            let cand = cand.trim();
            let (url, desc) = cand.split_once(char::is_whitespace).unwrap_or((cand, ""));
            let url = f(url).unwrap_or_else(|| url.to_string());
            if desc.is_empty() {
                url
            } else {
                format!("{url} {}", desc.trim())
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// What one pass over a page found.
#[derive(Debug, Default)]
pub struct Found {
    /// The page's `<base href>`, resolved.
    pub base: Option<Url>,
    pub links: Vec<(Kind, Url)>,
}

/// Goes over a page. `visit(kind, absolute_url, raw_value)` is called for every link; when
/// rewriting, a returned value replaces the attribute. With `rewrite`, `<base>` is removed
/// (links are made relative to the saved file) and the output is returned.
pub fn process(
    html: &[u8],
    page: &Url,
    charset: Option<&str>,
    rewrite: bool,
    visit: &mut dyn FnMut(Kind, &Url) -> Option<String>,
) -> (Found, Vec<u8>) {
    let encoding = charset
        .and_then(|c| encoding_rs::Encoding::for_label(c.trim().as_bytes()))
        .and_then(AsciiCompatibleEncoding::new)
        .unwrap_or_else(|| AsciiCompatibleEncoding::new(encoding_rs::UTF_8).expect("utf-8"));
    // The base is known only once <base> is seen; it comes before links in valid HTML.
    let base: RefCell<Url> = RefCell::new(page.clone());
    let found_base: RefCell<Option<Url>> = RefCell::new(None);
    let links: RefCell<Vec<(Kind, Url)>> = RefCell::new(Vec::new());
    let visit = RefCell::new(visit);
    let style_buf: RefCell<String> = RefCell::new(String::new());

    // Resolves a raw attribute value and hands it to `visit`.
    let handle = |kind: Kind, raw: &str| -> Option<String> {
        let raw = decode_entities(raw.trim());
        if raw.is_empty()
            || raw.starts_with('#')
            || raw.starts_with("data:")
            || raw.starts_with("javascript:")
            || raw.starts_with("mailto:")
        {
            return None;
        }
        let url = base.borrow().join(&raw).ok()?;
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        links.borrow_mut().push((kind, url.clone()));
        let mut v = visit.borrow_mut();
        (*v)(kind, &url)
    };
    let attr = |el: &mut lol_html::html_content::Element<'_, '_>, name: &str, kind: Kind| {
        if let Some(v) = el.get_attribute(name)
            && let Some(new) = handle(kind, &v)
            && rewrite
        {
            let _ = el.set_attribute(name, &new);
        }
    };
    let css_attr = |el: &mut lol_html::html_content::Element<'_, '_>| {
        if let Some(v) = el.get_attribute("style") {
            let new = css_urls(&decode_entities(&v), &mut |u| handle(Kind::Asset, u));
            if rewrite {
                let _ = el.set_attribute("style", &new);
            }
        }
    };

    let mut output = Vec::new();
    {
        let mut rw = HtmlRewriter::new(
            Settings {
                element_content_handlers: vec![
                    element!("base[href]", |el| {
                        if let Some(v) = el.get_attribute("href")
                            && let Ok(u) = page.join(&decode_entities(v.trim()))
                        {
                            *base.borrow_mut() = u.clone();
                            *found_base.borrow_mut() = Some(u);
                        }
                        if rewrite {
                            el.remove();
                        }
                        Ok(())
                    }),
                    element!("a[href], area[href]", |el| {
                        attr(el, "href", Kind::Page);
                        Ok(())
                    }),
                    element!("iframe[src], frame[src]", |el| {
                        attr(el, "src", Kind::Page);
                        Ok(())
                    }),
                    element!(
                        "img[src], script[src], source[src], video[src], audio[src], track[src], embed[src], input[src]",
                        |el| {
                            attr(el, "src", Kind::Asset);
                            Ok(())
                        }
                    ),
                    element!("img[srcset], source[srcset]", |el| {
                        if let Some(v) = el.get_attribute("srcset") {
                            let new = srcset(&decode_entities(&v), &mut |u| handle(Kind::Asset, u));
                            if rewrite {
                                let _ = el.set_attribute("srcset", &new);
                            }
                        }
                        Ok(())
                    }),
                    element!("video[poster]", |el| {
                        attr(el, "poster", Kind::Asset);
                        Ok(())
                    }),
                    element!("object[data]", |el| {
                        attr(el, "data", Kind::Asset);
                        Ok(())
                    }),
                    element!("link[href]", |el| {
                        let rel = el
                            .get_attribute("rel")
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        let asset = rel.split_whitespace().any(|r| {
                            matches!(
                                r,
                                "stylesheet" | "icon" | "apple-touch-icon" | "preload" | "manifest"
                            )
                        });
                        if asset {
                            attr(el, "href", Kind::Asset);
                        }
                        Ok(())
                    }),
                    element!("meta[http-equiv]", |el| {
                        let refresh = el
                            .get_attribute("http-equiv")
                            .is_some_and(|h| h.eq_ignore_ascii_case("refresh"));
                        if refresh
                            && let Some(content) = el.get_attribute("content")
                            && let Some(pos) = find_ci(&content, "url=")
                        {
                            let target = content[pos + 4..].trim().trim_matches(['\'', '"']);
                            if let Some(new) = handle(Kind::Page, target)
                                && rewrite
                            {
                                let _ = el.set_attribute(
                                    "content",
                                    &format!("{}url={new}", &content[..pos]),
                                );
                            }
                        }
                        Ok(())
                    }),
                    element!("[style]", |el| {
                        css_attr(el);
                        Ok(())
                    }),
                    text!("style", |t| {
                        style_buf.borrow_mut().push_str(t.as_str());
                        if t.last_in_text_node() {
                            let css = std::mem::take(&mut *style_buf.borrow_mut());
                            let new = css_urls(&css, &mut |u| handle(Kind::Asset, u));
                            if rewrite {
                                t.replace(&new, ContentType::Html);
                            }
                        } else if rewrite {
                            t.remove();
                        }
                        Ok(())
                    }),
                ],
                encoding,
                adjust_charset_on_meta_tag: true,
                ..Settings::new()
            },
            |c: &[u8]| {
                if rewrite {
                    output.extend_from_slice(c);
                }
            },
        );
        if rw.write(html).is_err() || rw.end().is_err() {
            tracing::debug!(%page, "HTML couldn't be read to the end");
        }
    }
    (
        Found {
            base: found_base.into_inner(),
            links: links.into_inner(),
        },
        output,
    )
}

/// The charset named in a `Content-Type` header.
pub fn charset_of(content_type: Option<&str>) -> Option<String> {
    content_type?
        .split(';')
        .skip(1)
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("charset"))
        .map(|(_, v)| v.trim().trim_matches('"').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r##"<!doctype html><html><head>
<base href="https://site.example/docs/">
<link rel="stylesheet" href="style.css?v=2">
<link rel="alternate" href="feed.xml">
<style>body { background: url('img/bg.png') } @import "more.css";</style>
<meta http-equiv="refresh" content="30; url=next.html">
</head><body>
<a href="page2.html#top">next</a>
<a href="files/report.pdf?a=1&amp;b=2">pdf</a>
<a href="mailto:x@y">mail</a><a href="#here">here</a><a href="javascript:void(0)">js</a>
<img src="/logo.png" srcset="/logo.png 1x, /logo@2x.png 2x">
<div style="background-image:url(&quot;tile.gif&quot;)"></div>
<iframe src="https://other.example/embed"></iframe>
<video src="clip.mp4" poster="poster.jpg"></video>
</body></html>"##;

    #[test]
    fn finds_every_link() {
        let page = Url::parse("https://site.example/index.html").unwrap();
        let (found, out) = process(PAGE.as_bytes(), &page, None, false, &mut |_, _| None);
        assert!(out.is_empty());
        assert_eq!(found.base.unwrap().as_str(), "https://site.example/docs/");
        let got: Vec<(Kind, String)> = found
            .links
            .iter()
            .map(|(k, u)| (*k, u.to_string()))
            .collect();
        let has = |k: Kind, u: &str| got.contains(&(k, u.to_string()));
        assert!(has(Kind::Asset, "https://site.example/docs/style.css?v=2"));
        assert!(has(Kind::Asset, "https://site.example/docs/img/bg.png"));
        assert!(has(Kind::Asset, "https://site.example/docs/more.css"));
        assert!(has(Kind::Page, "https://site.example/docs/next.html"));
        assert!(has(Kind::Page, "https://site.example/docs/page2.html#top"));
        assert!(has(
            Kind::Page,
            "https://site.example/docs/files/report.pdf?a=1&b=2"
        ));
        assert!(has(Kind::Asset, "https://site.example/logo.png"));
        assert!(has(Kind::Asset, "https://site.example/logo@2x.png"));
        assert!(has(Kind::Asset, "https://site.example/docs/tile.gif"));
        assert!(has(Kind::Page, "https://other.example/embed"));
        assert!(has(Kind::Asset, "https://site.example/docs/clip.mp4"));
        assert!(has(Kind::Asset, "https://site.example/docs/poster.jpg"));
        assert!(!got.iter().any(|(_, u)| u.contains("feed.xml")));
        assert!(
            !got.iter()
                .any(|(_, u)| u.contains("mailto") || u.contains("javascript"))
        );
    }

    #[test]
    fn rewrites_links() {
        let page = Url::parse("https://site.example/index.html").unwrap();
        let (_, out) = process(PAGE.as_bytes(), &page, None, true, &mut |_, u| {
            Some(format!("LOCAL{}", u.path()))
        });
        let out = String::from_utf8(out).unwrap();
        assert!(!out.contains("<base"));
        assert!(out.contains(r#"href="LOCAL/docs/style.css""#));
        assert!(out.contains(r#"url('LOCAL/docs/img/bg.png')"#));
        assert!(out.contains(r#"@import "LOCAL/docs/more.css""#));
        assert!(out.contains("url=LOCAL/docs/next.html"));
        assert!(out.contains(r#"srcset="LOCAL/logo.png 1x, LOCAL/logo@2x.png 2x""#));
        assert!(out.contains("LOCAL/docs/tile.gif"));
        assert!(out.contains(r#"href="mailto:x@y""#));
        assert!(out.contains(r##"href="#here""##));
        assert!(
            out.contains(r#"href="feed.xml""#),
            "links that aren't followed stay"
        );
    }

    #[test]
    fn css_and_entities() {
        let css = "a{b:url(x.png)} c{d:URL( \"y.woff2\" )} e{f:url(data:image/png;base64,AA)}";
        let mut seen = Vec::new();
        let out = css_urls(css, &mut |u| {
            seen.push(u.to_string());
            Some(format!("L/{u}"))
        });
        assert_eq!(seen, ["x.png", "y.woff2"]);
        assert!(out.contains(r#"url("L/x.png")"#));
        assert!(out.contains(r#"URL("L/y.woff2")"#));
        assert!(out.contains("url(data:image/png;base64,AA)"));
        assert_eq!(
            decode_entities("a&amp;b&#38;c&#x26;d&bogus;"),
            "a&b&c&d&bogus;"
        );
        assert_eq!(
            charset_of(Some("text/html; charset=\"ISO-8859-1\"")).as_deref(),
            Some("ISO-8859-1")
        );
    }
}
