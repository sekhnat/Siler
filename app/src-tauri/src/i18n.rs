//! The words Siler shows from the Rust side (tray, notifications, errors), from the same
//! Fluent catalogs as the interface (`app/locales/<lang>/siler.ftl`, built into the binary).
//! English (en-US) is the source and the fallback.
//!
//! The language is the one in Settings (`language`), else the system's (`LC_ALL`,
//! `LC_MESSAGES`, `LANG`). It is decided once, at start-up. `en-XA` shows every message
//! accented and in brackets, to find text that isn't in the catalog yet.

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use std::sync::OnceLock;
use unic_langid::LanguageIdentifier;

/// The catalogs built into Siler: (language, text).
const CATALOGS: &[(&str, &str)] = &[("en-US", include_str!("../../locales/en-US/siler.ftl"))];

struct Catalogs {
    lang: String,
    current: Option<FluentBundle<FluentResource>>,
    english: FluentBundle<FluentResource>,
}

static CATALOG: OnceLock<Catalogs> = OnceLock::new();

fn bundle(lang: &str, text: &str) -> FluentBundle<FluentResource> {
    let id: LanguageIdentifier = lang.parse().unwrap_or_default();
    let mut b = FluentBundle::new_concurrent(vec![id]);
    b.set_use_isolating(false);
    let res = match FluentResource::try_new(text.to_string()) {
        Ok(r) => r,
        Err((r, errors)) => {
            tracing::warn!("siler.ftl ({lang}): {} lines can't be read", errors.len());
            r
        }
    };
    if let Err(e) = b.add_resource(res) {
        tracing::warn!("siler.ftl ({lang}): {e:?}");
    }
    b
}

/// The system's language: `de_DE.UTF-8` → `de-DE`.
fn system_language() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
        .map(|v| v.split(['.', '@']).next().unwrap_or("").replace('_', "-"))
        .unwrap_or_else(|| "en-US".into())
}

/// The catalog for a language tag: exact, then the same language, else English.
fn resolve(tag: &str) -> &'static str {
    if tag == "en-XA" {
        return "en-XA";
    }
    let base = tag.split('-').next().unwrap_or("").to_ascii_lowercase();
    CATALOGS
        .iter()
        .find(|(l, _)| *l == tag)
        .or_else(|| {
            CATALOGS.iter().find(|(l, _)| {
                l.split('-')
                    .next()
                    .unwrap_or("")
                    .eq_ignore_ascii_case(&base)
            })
        })
        .map(|(l, _)| *l)
        .unwrap_or("en-US")
}

/// Chooses the language: the one from Settings, or the system's when empty. Called once at
/// start-up; later calls change nothing.
pub fn init(chosen: &str) {
    let tag = if chosen.trim().is_empty() {
        system_language()
    } else {
        chosen.trim().to_string()
    };
    let lang = resolve(&tag).to_string();
    let _ = CATALOG.set(load(lang));
}

fn load(lang: String) -> Catalogs {
    let english = bundle("en-US", CATALOGS[0].1);
    let current = CATALOGS
        .iter()
        .find(|(l, _)| *l == lang && *l != "en-US")
        .map(|(l, text)| bundle(l, text));
    tracing::info!("interface language: {lang}");
    Catalogs {
        lang,
        current,
        english,
    }
}

/// The language in use (`en-US`, `en-XA`…).
pub fn language() -> &'static str {
    &catalogs().lang
}

fn catalogs() -> &'static Catalogs {
    CATALOG.get_or_init(|| load("en-US".into()))
}

fn pseudo(s: &str) -> String {
    let map = |c: char| match c {
        'a' => 'å',
        'e' => 'é',
        'i' => 'í',
        'o' => 'ö',
        'u' => 'ü',
        'c' => 'ç',
        'n' => 'ñ',
        'A' => 'Å',
        'E' => 'É',
        'I' => 'Í',
        'O' => 'Ö',
        'U' => 'Ü',
        other => other,
    };
    format!("[{}]", s.chars().map(map).collect::<String>())
}

/// A value for a message argument.
pub enum Arg {
    Text(String),
    Number(f64),
}

impl From<&str> for Arg {
    fn from(s: &str) -> Self {
        Arg::Text(s.to_string())
    }
}
impl From<String> for Arg {
    fn from(s: String) -> Self {
        Arg::Text(s)
    }
}
impl From<&String> for Arg {
    fn from(s: &String) -> Self {
        Arg::Text(s.clone())
    }
}
impl From<usize> for Arg {
    fn from(n: usize) -> Self {
        Arg::Number(n as f64)
    }
}
impl From<u64> for Arg {
    fn from(n: u64) -> Self {
        Arg::Number(n as f64)
    }
}
impl From<i32> for Arg {
    fn from(n: i32) -> Self {
        Arg::Number(n as f64)
    }
}

fn format(id: &str, args: Option<&FluentArgs>) -> Option<String> {
    let c = catalogs();
    for b in c.current.iter().chain(std::iter::once(&c.english)) {
        let Some(m) = b.get_message(id) else { continue };
        let Some(pattern) = m.value() else { continue };
        let mut errors = Vec::new();
        let text = b.format_pattern(pattern, args, &mut errors).into_owned();
        return Some(if c.lang == "en-XA" {
            pseudo(&text)
        } else {
            text
        });
    }
    None
}

/// The message `id`. An unknown id shows as itself.
pub fn tr(id: &str) -> String {
    format(id, None).unwrap_or_else(|| {
        tracing::warn!("siler.ftl: no message “{id}”");
        id.to_string()
    })
}

/// The message `id` with its arguments.
pub fn tr_args(id: &str, args: Vec<(&str, Arg)>) -> String {
    let mut fa = FluentArgs::new();
    for (k, v) in args {
        match v {
            Arg::Text(s) => fa.set(k, FluentValue::from(s)),
            Arg::Number(n) => fa.set(k, FluentValue::from(n)),
        }
    }
    format(id, Some(&fa)).unwrap_or_else(|| {
        tracing::warn!("siler.ftl: no message “{id}”");
        id.to_string()
    })
}

/// An error detail: a whole number stays a number (an HTTP status selects its words), the
/// rest is text.
fn value(v: String) -> FluentValue<'static> {
    match v.parse::<i64>() {
        Ok(n) if !v.starts_with('+') && (v == "0" || !v.starts_with('0')) => FluentValue::from(n),
        _ => FluentValue::from(v),
    }
}

/// An engine error in the interface's language (`error-<code>` in the catalog), or its
/// English text when the catalog has nothing for it.
pub fn error(e: &siler_core::Error) -> String {
    let (code, args) = e.code_args();
    let id = format!("error-{code}");
    let mut fa = FluentArgs::new();
    for (k, v) in args {
        fa.set(k, value(v));
    }
    format(&id, Some(&fa)).unwrap_or_else(|| e.to_string())
}

/// An error stored with a download (its code and details), or `fallback` (its English
/// text) when the catalog has nothing for it.
pub fn error_code(c: &siler_core::extras::ErrorCode, fallback: &str) -> String {
    let id = format!("error-{}", c.code);
    let mut fa = FluentArgs::new();
    for (k, v) in &c.args {
        fa.set(k.as_str(), value(v.clone()));
    }
    format(&id, Some(&fa)).unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_and_errors() {
        assert_eq!(tr("tray-pause-all"), "Pause all");
        assert_eq!(
            tr_args(
                "tray-downloading",
                vec![("count", 2usize.into()), ("speed", "1 MB".into())]
            ),
            "2 downloading · 1 MB/s"
        );
        let e = siler_core::Error::AuthRequired {
            host: "ftp://h".into(),
            realm: None,
            rejected: true,
        };
        assert_eq!(error(&e), e.to_string());
        let e = siler_core::Error::Media(siler_core::MediaError::Drm);
        assert_eq!(error(&e), e.to_string());
        assert_eq!(
            error(&siler_core::Error::Http(404)),
            siler_core::Error::Http(404).to_string()
        );
        assert_eq!(resolve("de-AT"), "en-US");
        assert_eq!(resolve("en-GB"), "en-US");
    }

    #[test]
    fn every_error_has_a_message() {
        use siler_core::{Error, MediaError};
        let samples = vec![
            Error::InvalidUrl("x".into()),
            Error::Network("x".into()),
            Error::Http(500),
            Error::Protocol("x".into()),
            Error::DiskFull,
            Error::NotFound,
            Error::Exists("f".into()),
            Error::FileMissing,
            Error::ProxyAuth,
            Error::Ftp(550, "x".into()),
            Error::HostKeyChanged("h".into()),
            Error::Media(MediaError::NotMedia),
            Error::Media(MediaError::LinksExpired),
        ];
        for e in samples {
            let (code, _) = e.code_args();
            assert!(
                format(&format!("error-{code}"), None).is_some(),
                "error-{code}"
            );
        }
    }
}
