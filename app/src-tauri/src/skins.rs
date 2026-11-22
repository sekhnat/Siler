//! Skins: the interface's colours for light and dark, how round its corners are and how the
//! toolbar's buttons look. The built-in ones are in `app/skins/`; imported ones are JSON
//! files in `<data dir>/skins/`. A skin can only set colours the interface knows, written
//! as hex, so a file from someone else can't inject styles.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;
use crate::i18n::{tr, tr_args};

const FORMAT: u32 = 1;
const MAX_FILE: u64 = 64 * 1024;
const MAX_NAME: usize = 40;
const MAX_RADIUS: u8 = 20;
const DEFAULT: &str = "siler";
/// Put before an imported skin's id when it is a built-in's.
const OWN: &str = "my-";

const BUILT_IN: [&str; 7] = [
    include_str!("../../skins/siler.json"),
    include_str!("../../skins/graphite.json"),
    include_str!("../../skins/breeze.json"),
    include_str!("../../skins/ocean.json"),
    include_str!("../../skins/forest.json"),
    include_str!("../../skins/sunset.json"),
    include_str!("../../skins/high-contrast.json"),
];

/// The accent in use, for the tray's ring.
static ACCENT: AtomicU32 = AtomicU32::new(0x008b_7bff);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolbarStyle {
    /// The main actions with words, the others as icons.
    #[default]
    Mixed,
    Icons,
    /// Icons and words.
    Text,
}

fn default_radius() -> u8 {
    10
}

type Colours = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skin {
    format: u32,
    #[serde(default)]
    id: String,
    name: String,
    #[serde(default = "default_radius")]
    radius: u8,
    #[serde(default)]
    toolbar: ToolbarStyle,
    /// Colours by the CSS variable they set, without its `--`.
    #[serde(default)]
    light: Colours,
    #[serde(default)]
    dark: Colours,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkinView {
    #[serde(flatten)]
    skin: Skin,
    built_in: bool,
}

fn built_in() -> &'static [Skin] {
    static SKINS: OnceLock<Vec<Skin>> = OnceLock::new();
    SKINS.get_or_init(|| {
        BUILT_IN
            .iter()
            .map(|s| serde_json::from_str(s).expect("built-in skins are checked by a test"))
            .collect()
    })
}

fn is_built_in(id: &str) -> bool {
    built_in().iter().any(|s| s.id == id)
}

/// `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`, and nothing else.
fn is_colour(v: &str) -> bool {
    v.strip_prefix('#').is_some_and(|h| {
        matches!(h.len(), 3 | 4 | 6 | 8) && h.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= MAX_NAME - OWN.len() {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

#[derive(Debug, PartialEq, Eq)]
enum Invalid {
    TooBig,
    NotASkin(String),
    Name,
    Radius,
    Unknown(String),
    Colour(String),
}

impl Invalid {
    fn message(self) -> String {
        match self {
            Invalid::TooBig => tr("skin-error-too-big"),
            Invalid::NotASkin(why) => tr_args("skin-error-not-a-skin", vec![("why", why.into())]),
            Invalid::Name => tr("skin-error-name"),
            Invalid::Radius => tr("skin-error-radius"),
            Invalid::Unknown(key) => tr_args("skin-error-unknown", vec![("key", key.into())]),
            Invalid::Colour(key) => tr_args("skin-error-colour", vec![("key", key.into())]),
        }
    }
}

/// Reads a skin, filling the colours it leaves out from Siler's own.
fn parse(bytes: &[u8]) -> Result<Skin, Invalid> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(Invalid::TooBig);
    }
    let mut skin: Skin =
        serde_json::from_slice(bytes).map_err(|e| Invalid::NotASkin(e.to_string()))?;
    if skin.format != FORMAT {
        return Err(Invalid::NotASkin(format!("format {}", skin.format)));
    }
    skin.name = skin.name.trim().to_string();
    if skin.name.is_empty()
        || skin.name.chars().count() > MAX_NAME
        || skin.name.chars().any(char::is_control)
    {
        return Err(Invalid::Name);
    }
    if skin.radius > MAX_RADIUS {
        return Err(Invalid::Radius);
    }
    let base = &built_in()[0];
    for (colours, defaults) in [(&mut skin.light, &base.light), (&mut skin.dark, &base.dark)] {
        for (key, value) in colours.iter() {
            if !defaults.contains_key(key) {
                return Err(Invalid::Unknown(key.chars().take(MAX_NAME).collect()));
            }
            if !is_colour(value) {
                return Err(Invalid::Colour(key.clone()));
            }
        }
        for (key, value) in defaults {
            colours.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    Ok(skin)
}

fn read(path: &Path) -> Result<Skin, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err(Invalid::NotASkin(path.display().to_string()).message());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    parse(&bytes).map_err(Invalid::message)
}

fn load_all(dir: &Path) -> Vec<SkinView> {
    let mut all: Vec<SkinView> = built_in()
        .iter()
        .map(|s| SkinView {
            skin: s.clone(),
            built_in: true,
        })
        .collect();
    let mut own = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if slug(id) != id && !(id.starts_with(OWN) && slug(&id[OWN.len()..]) == id[OWN.len()..]) {
            continue;
        }
        if is_built_in(id) {
            continue;
        }
        match read(&path) {
            Ok(mut skin) => {
                skin.id = id.to_string();
                own.push(SkinView {
                    skin,
                    built_in: false,
                });
            }
            Err(e) => tracing::warn!("ignoring the skin {}: {e}", path.display()),
        }
    }
    own.sort_by_key(|s| s.skin.name.to_lowercase());
    all.extend(own);
    all
}

/// Saves a skin from a file. One with the same id replaces the earlier import; a built-in's
/// id gets a prefix.
fn import(dir: &Path, from: &Path) -> Result<SkinView, String> {
    let mut skin = read(from)?;
    let mut id = slug(if skin.id.is_empty() {
        &skin.name
    } else {
        &skin.id
    });
    if id.is_empty() {
        // A name without a Latin letter or digit: told apart by its characters.
        let sum = skin
            .name
            .chars()
            .fold(0u32, |h, c| h.wrapping_mul(31).wrapping_add(c as u32));
        id = format!("skin-{sum:x}");
    }
    if is_built_in(&id) {
        id = format!("{OWN}{id}");
    }
    skin.id = id.clone();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let to = dir.join(format!("{id}.json"));
    let part = dir.join(format!("{id}.json.part"));
    let json = serde_json::to_string_pretty(&skin).map_err(|e| e.to_string())?;
    std::fs::write(&part, json)
        .and_then(|()| std::fs::rename(&part, &to))
        .map_err(|e| e.to_string())?;
    Ok(SkinView {
        skin,
        built_in: false,
    })
}

fn export(dir: &Path, id: &str, to: &Path) -> Result<(), String> {
    let found = load_all(dir)
        .into_iter()
        .find(|s| s.skin.id == id)
        .ok_or_else(|| tr("skin-error-gone"))?;
    let json = serde_json::to_string_pretty(&found.skin).map_err(|e| e.to_string())?;
    std::fs::write(to, json + "\n").map_err(|e| e.to_string())
}

fn remove(dir: &Path, id: &str) -> Result<(), String> {
    // Only what the list shows, so an id can't be a path.
    let own = load_all(dir)
        .into_iter()
        .any(|s| !s.built_in && s.skin.id == id);
    if !own {
        return Err(tr("skin-error-gone"));
    }
    std::fs::remove_file(dir.join(format!("{id}.json"))).map_err(|e| e.to_string())
}

fn dir_of(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("skins"))
}

/// `#rgb…` or `#rrggbb…` as a colour, without its alpha.
fn rgb(v: &str) -> Option<[u8; 3]> {
    if !is_colour(v) {
        return None;
    }
    let h = &v[1..];
    let mut out = [0u8; 3];
    for (i, c) in out.iter_mut().enumerate() {
        *c = if h.len() < 6 {
            u8::from_str_radix(&h[i..=i], 16).ok()? * 17
        } else {
            u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok()?
        };
    }
    Some(out)
}

/// The accent of the skin in use, as it is in dark (tray panels mostly are).
pub fn accent() -> [u8; 3] {
    let [_, r, g, b] = ACCENT.load(Ordering::Relaxed).to_be_bytes();
    [r, g, b]
}

/// Notes the chosen skin's accent; called at start-up and when the skin changes.
pub fn remember(app: &AppHandle) {
    let (Some(state), Ok(dir)) = (app.try_state::<AppState>(), dir_of(app)) else {
        return;
    };
    let id = state.engine.settings().ui.skin;
    let all = load_all(&dir);
    let chosen = all
        .iter()
        .find(|s| s.skin.id == id)
        .or_else(|| all.iter().find(|s| s.skin.id == DEFAULT));
    if let Some([r, g, b]) = chosen
        .and_then(|s| s.skin.dark.get("accent"))
        .and_then(|v| rgb(v))
    {
        ACCENT.store(u32::from_be_bytes([0, r, g, b]), Ordering::Relaxed);
    }
}

/// The skin or the skins changed: every window and the tray follow.
pub fn changed(app: &AppHandle) {
    remember(app);
    crate::tray::refresh();
    let _ = app.emit("skin-changed", ());
}

#[tauri::command]
pub fn list_skins(app: AppHandle) -> Result<Vec<SkinView>, String> {
    Ok(load_all(&dir_of(&app)?))
}

#[tauri::command]
pub fn import_skin(app: AppHandle, path: PathBuf) -> Result<SkinView, String> {
    let skin = import(&dir_of(&app)?, &path)?;
    changed(&app);
    Ok(skin)
}

#[tauri::command]
pub fn export_skin(app: AppHandle, id: String, path: PathBuf) -> Result<(), String> {
    export(&dir_of(&app)?, &id, &path)
}

#[tauri::command]
pub fn remove_skin(app: AppHandle, id: String) -> Result<(), String> {
    remove(&dir_of(&app)?, &id)?;
    changed(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type Rgb = [f64; 3];

    fn colour(v: &str) -> (Rgb, f64) {
        let c = rgb(v).expect("a colour").map(f64::from);
        let alpha = match v.len() {
            9 => f64::from(u8::from_str_radix(&v[7..], 16).unwrap()) / 255.0,
            _ => 1.0,
        };
        (c, alpha)
    }

    fn blend(top: Rgb, under: Rgb, alpha: f64) -> Rgb {
        [0, 1, 2].map(|i| top[i] * alpha + under[i] * (1.0 - alpha))
    }

    fn contrast(a: Rgb, b: Rgb) -> f64 {
        let light = |c: Rgb| {
            let [r, g, b] = c.map(|v| {
                let v = v / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            });
            0.2126 * r + 0.7152 * g + 0.0722 * b
        };
        let (x, y) = (light(a), light(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// WCAG AA for text, in both modes; AAA for High contrast.
    #[test]
    fn text_reads_in_every_built_in_skin() {
        let mut failures = Vec::new();
        for skin in built_in() {
            let least = if skin.id == "high-contrast" { 7.0 } else { 4.5 };
            for (mode, c) in [("light", &skin.light), ("dark", &skin.dark)] {
                let get = |k: &str| colour(&c[k]);
                let mut grounds: Vec<(String, Rgb)> =
                    ["bg", "sidebar", "surface", "surface-2", "surface-3"]
                        .map(|k| (k.to_string(), get(k).0))
                        .to_vec();
                for state in ["hover", "selected"] {
                    for under in ["bg", "surface"] {
                        let (top, a) = get(state);
                        grounds.push((format!("{state} on {under}"), blend(top, get(under).0, a)));
                    }
                }
                let mut check = |what: String, fg: Rgb, bg: Rgb| {
                    let got = contrast(fg, bg);
                    if got < least {
                        failures.push(format!("{} {mode}: {what} is {got:.2}", skin.id));
                    }
                };
                for text in [
                    "text",
                    "text-2",
                    "muted",
                    "accent-text",
                    "ok",
                    "warn",
                    "err",
                ] {
                    for (name, ground) in &grounds {
                        check(format!("{text} on {name}"), get(text).0, *ground);
                    }
                }
                for button in ["button", "button-2"] {
                    check(
                        format!("on-accent on {button}"),
                        get("on-accent").0,
                        get(button).0,
                    );
                }
                // A pill (`.pill` in app.css): 70% of its colour with the text's, on 14% of it.
                for pill in ["ok", "warn", "err", "queued", "accent", "muted"] {
                    let own = get(pill).0;
                    let words = blend(own, get("text").0, 0.7);
                    let ground = blend(own, get("surface").0, 0.14);
                    check(format!("a {pill} pill"), words, ground);
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn built_in_skins_are_whole_and_valid() {
        let base = &built_in()[0];
        assert_eq!(base.id, DEFAULT);
        for (raw, skin) in BUILT_IN.iter().zip(built_in()) {
            assert_eq!(parse(raw.as_bytes()).unwrap(), *skin, "{}", skin.id);
            assert!(skin.light.keys().eq(base.light.keys()), "{}", skin.id);
            assert!(skin.dark.keys().eq(base.dark.keys()), "{}", skin.id);
            assert_eq!(slug(&skin.id), skin.id);
        }
    }

    /// app.css paints until the skin is known, so it must be the default one.
    #[test]
    fn app_css_is_the_default_skin() {
        let css = include_str!("../../src/app.css");
        let dark_at = css.find(":root[data-theme=\"dark\"]").unwrap();
        let end = dark_at + css[dark_at..].find('}').unwrap();
        for (colours, block) in [
            (&built_in()[0].light, &css[..dark_at]),
            (&built_in()[0].dark, &css[dark_at..end]),
        ] {
            for (key, value) in colours {
                let line = format!("--{key}: {value};");
                assert!(block.contains(&line), "app.css lacks `{line}`");
            }
        }
    }

    #[test]
    fn files_cant_inject_styles() {
        let skin = |body: &str| parse(format!(r#"{{"format":1,"name":"X",{body}}}"#).as_bytes());
        for bad in [
            "red",
            "url(https://x/y)",
            "#fff; background: url(x)",
            "var(--err)",
            "#12345",
            "#fff\\n",
            "",
        ] {
            let r = skin(&format!(r##""light":{{"bg":"{bad}"}}"##));
            assert!(
                matches!(r, Err(Invalid::Colour(_) | Invalid::NotASkin(_))),
                "{bad}"
            );
        }
        assert!(matches!(
            skin(r##""dark":{"font":"#fff"}"##),
            Err(Invalid::Unknown(_))
        ));
        assert!(matches!(skin(r#""css":"*{}""#), Err(Invalid::NotASkin(_))));
        assert_eq!(skin(r#""radius":99"#), Err(Invalid::Radius));
        assert_eq!(
            parse(&vec![b' '; MAX_FILE as usize + 1]),
            Err(Invalid::TooBig)
        );
        // What a file leaves out comes from Siler's.
        let ok = skin(r##""dark":{"accent":"#f0f"}"##).unwrap();
        assert_eq!(ok.dark["accent"], "#f0f");
        assert_eq!(ok.dark["bg"], built_in()[0].dark["bg"]);
    }

    #[test]
    fn import_export_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skins");
        let file = tmp.path().join("mine.json");
        std::fs::write(
            &file,
            r##"{"format":1,"id":"../../Ocean","name":"My Ocean","dark":{"accent":"#0ff"}}"##,
        )
        .unwrap();
        let view = import(&dir, &file).unwrap();
        assert_eq!(view.skin.id, "my-ocean", "no path, and not a built-in's id");
        import(&dir, &file).unwrap();
        let own: Vec<_> = load_all(&dir).into_iter().filter(|s| !s.built_in).collect();
        assert_eq!(own.len(), 1, "importing again replaces it");

        let out = tmp.path().join("out.json");
        export(&dir, "my-ocean", &out).unwrap();
        assert_eq!(parse(&std::fs::read(&out).unwrap()).unwrap(), view.skin);

        assert!(remove(&dir, "../mine").is_err() && file.exists());
        assert!(remove(&dir, "ocean").is_err());
        remove(&dir, "my-ocean").unwrap();
        assert_eq!(load_all(&dir).len(), built_in().len());
    }

    #[test]
    fn short_and_long_hex() {
        assert_eq!(rgb("#0fa"), Some([0, 255, 170]));
        assert_eq!(rgb("#8b7bffcc"), Some([0x8b, 0x7b, 0xff]));
        assert_eq!(rgb("blue"), None);
    }
}
