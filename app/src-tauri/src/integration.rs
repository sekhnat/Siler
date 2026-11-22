//! Registers the native messaging host with Firefox, the Firefox-based browsers, and the
//! Chromium-based ones (Chrome, Chromium, Brave, Vivaldi, Edge, Opera).

use crate::i18n::tr;
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const HOST_NAME: &str = "com.siler.nmhost";
pub const EXTENSION_ID: &str = "siler@siler.app";
/// The Chromium extension's IDs: the unpacked build's (pinned by the `key` in
/// extension/manifest.chromium.json). The Chrome Web Store gives its copy an ID of its own,
/// which goes here once it is published.
pub const CHROMIUM_IDS: &[&str] = &["legjbaodpdlommbgpdfcmogjedfgpjae"];

/// How a browser family names the allowed extensions in a host manifest, and the folder
/// the manifests go in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Firefox,
    Chromium,
}

/// Browsers and the folder under `$HOME` they read native host manifests from.
/// Firefox's folder is always written: Zen and Floorp read it too. The others get a copy
/// when they are installed (their folder exists); LibreWolf needs its own.
const BROWSERS: &[(&str, &str, Family)] = &[
    ("Firefox", ".mozilla", Family::Firefox),
    ("LibreWolf", ".librewolf", Family::Firefox),
    ("Waterfox", ".waterfox", Family::Firefox),
    ("Floorp", ".floorp", Family::Firefox),
    ("Zen", ".zen", Family::Firefox),
    ("Chrome", ".config/google-chrome", Family::Chromium),
    (
        "Chrome Beta",
        ".config/google-chrome-beta",
        Family::Chromium,
    ),
    (
        "Chrome Dev",
        ".config/google-chrome-unstable",
        Family::Chromium,
    ),
    ("Chromium", ".config/chromium", Family::Chromium),
    (
        "Brave",
        ".config/BraveSoftware/Brave-Browser",
        Family::Chromium,
    ),
    ("Vivaldi", ".config/vivaldi", Family::Chromium),
    ("Edge", ".config/microsoft-edge", Family::Chromium),
    ("Opera", ".config/opera", Family::Chromium),
];

#[derive(Debug, Serialize)]
pub struct BrowserStatus {
    pub name: &'static str,
    pub family: Family,
    pub manifest_path: String,
    pub installed: bool,
}

#[derive(Debug, Serialize)]
pub struct IntegrationStatus {
    pub browsers: Vec<BrowserStatus>,
    /// The Firefox manifest is installed.
    pub manifest_installed: bool,
    pub manifest_path: String,
    /// Host binary the installed manifest points to.
    pub host_path: Option<String>,
    pub host_exists: bool,
    /// Unix ms of the last message from the extension, if any this session.
    pub last_contact: Option<i64>,
    pub app_version: &'static str,
    pub protocol: u32,
    pub extension_version: Option<String>,
    pub extension_protocol: Option<u32>,
    /// The `.desktop` entry points at this executable.
    pub desktop_entry: bool,
    pub tray_available: bool,
    /// The signed extension, ready to install permanently.
    pub signed_xpi: Option<String>,
    pub log_dir: String,
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

fn manifest_in(base: &Path) -> PathBuf {
    manifest_for(base, Family::Firefox)
}

fn manifest_for(base: &Path, family: Family) -> PathBuf {
    let dir = match family {
        Family::Firefox => "native-messaging-hosts",
        Family::Chromium => "NativeMessagingHosts",
    };
    base.join(dir).join(format!("{HOST_NAME}.json"))
}

/// The manifest for `firefox` or `chromium`, as text (for packages, which install them
/// system-wide).
pub fn host_manifest(family: &str, host: &Path) -> Option<String> {
    let family = match family {
        "firefox" => Family::Firefox,
        "chromium" => Family::Chromium,
        _ => return None,
    };
    serde_json::to_string_pretty(&manifest_json(host, family)).ok()
}

/// The manifest for one family, pointing at `host`.
fn manifest_json(host: &Path, family: Family) -> serde_json::Value {
    let mut m = serde_json::json!({
        "name": HOST_NAME,
        "description": "Siler download manager",
        "path": host,
        "type": "stdio",
    });
    match family {
        Family::Firefox => m["allowed_extensions"] = serde_json::json!([EXTENSION_ID]),
        Family::Chromium => {
            m["allowed_origins"] = CHROMIUM_IDS
                .iter()
                .map(|id| format!("chrome-extension://{id}/"))
                .collect()
        }
    }
    m
}

/// (name, manifest path) of every browser to register with.
fn targets() -> Vec<(&'static str, Family, PathBuf)> {
    let home = home();
    BROWSERS
        .iter()
        .filter(|(name, dir, _)| *name == "Firefox" || home.join(dir).is_dir())
        .map(|(name, dir, family)| (*name, *family, manifest_for(&home.join(dir), *family)))
        .collect()
}

/// The `siler-nmhost` binary shipped next to the app.
fn bundled_host() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let host = exe.parent()?.join("siler-nmhost");
    host.exists().then_some(host)
}

/// The app's resource folder (inside the AppImage), set once at start-up.
static RESOURCE_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_resource_dir(dir: PathBuf) {
    let _ = RESOURCE_DIR.set(dir);
}

const XPI: &str = "siler-integration.xpi";

/// The signed extension to install: one signed by `scripts/sign-extension.sh`, else the
/// copy bundled with the AppImage (resource folder) or the tarball (next to the
/// executable).
pub fn signed_xpi() -> Option<PathBuf> {
    let own = dirs::data_dir().map(|d| d.join("siler").join(XPI));
    let bundled = RESOURCE_DIR.get().map(|d| d.join(XPI));
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| Some(e.parent()?.join(XPI)));
    let packaged = Some(PathBuf::from("/usr/share/siler").join(XPI));
    [own, bundled, beside, packaged]
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
}

const CHROMIUM_DIR: &str = "chromium-extension";

/// The unpacked Chromium extension shipped with Siler: in the AppImage's resources, beside
/// the executable (tarball), in a package's share folder, or (development) the one
/// `scripts/build-extension.mjs chromium` made.
fn bundled_chromium() -> Option<PathBuf> {
    let bundled = RESOURCE_DIR.get().map(|d| d.join(CHROMIUM_DIR));
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| Some(e.parent()?.join(CHROMIUM_DIR)));
    let packaged = Some(PathBuf::from("/usr/share/siler").join(CHROMIUM_DIR));
    let dev = cfg!(debug_assertions)
        .then(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dist/extension-chromium"));
    [bundled, beside, packaged, dev]
        .into_iter()
        .flatten()
        .find(|p| p.join("manifest.json").is_file())
}

/// Copies the Chromium extension where it stays put (Chromium loads an unpacked extension
/// from its folder every time it starts), and returns that folder.
pub fn chromium_extension() -> std::io::Result<PathBuf> {
    let from = bundled_chromium().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            tr("integration-no-chromium-extension"),
        )
    })?;
    let dest = dirs::data_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no data folder"))?
        .join("siler")
        .join(CHROMIUM_DIR);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    copy_dir(&from, &dest)?;
    Ok(dest)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Chromium-based browsers found on PATH, by the program that starts them.
pub fn chromium_browsers() -> Vec<(&'static str, &'static str)> {
    const PROGRAMS: &[(&str, &str)] = &[
        ("Chrome", "google-chrome-stable"),
        ("Chrome", "google-chrome"),
        ("Chromium", "chromium"),
        ("Chromium", "chromium-browser"),
        ("Brave", "brave"),
        ("Brave", "brave-browser"),
        ("Vivaldi", "vivaldi-stable"),
        ("Vivaldi", "vivaldi"),
        ("Edge", "microsoft-edge-stable"),
        ("Edge", "microsoft-edge"),
        ("Opera", "opera"),
    ];
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let mut found: Vec<(&str, &str)> = Vec::new();
    for (name, prog) in PROGRAMS {
        if found.iter().any(|(n, _)| n == name) {
            continue;
        }
        if dirs.iter().any(|d| d.join(prog).is_file()) {
            found.push((name, prog));
        }
    }
    found
}

/// A copy of the signed extension that stays put while Firefox installs it: the AppImage's
/// own files disappear when Siler quits.
pub fn xpi_for_firefox() -> std::io::Result<PathBuf> {
    let xpi = signed_xpi().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            tr("integration-no-signed-extension"),
        )
    })?;
    let dir = dirs::cache_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no cache folder"))?
        .join("siler");
    std::fs::create_dir_all(&dir)?;
    let copy = dir.join(XPI);
    std::fs::copy(&xpi, &copy)?;
    Ok(copy)
}

fn host_of(manifest: &Path) -> Option<String> {
    std::fs::read(manifest)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("path")?.as_str().map(String::from))
}

pub fn status(
    last_contact: i64,
    extension: Option<(String, u32)>,
    tray_available: bool,
) -> IntegrationStatus {
    let browsers: Vec<BrowserStatus> = targets()
        .into_iter()
        .map(|(name, family, path)| BrowserStatus {
            name,
            family,
            installed: host_of(&path).is_some(),
            manifest_path: path.to_string_lossy().into_owned(),
        })
        .collect();
    let firefox = manifest_in(&home().join(".mozilla"));
    let host_path = host_of(&firefox);
    IntegrationStatus {
        manifest_installed: host_path.is_some(),
        manifest_path: firefox.to_string_lossy().into_owned(),
        host_exists: host_path.as_deref().is_some_and(|p| Path::new(p).exists()),
        host_path,
        browsers,
        last_contact: (last_contact > 0).then_some(last_contact),
        app_version: env!("CARGO_PKG_VERSION"),
        protocol: siler_ipc::PROTOCOL,
        extension_version: extension.as_ref().map(|e| e.0.clone()),
        extension_protocol: extension.map(|e| e.1),
        desktop_entry: crate::desktop::is_current(),
        tray_available,
        signed_xpi: signed_xpi().map(|p| p.to_string_lossy().into_owned()),
        log_dir: crate::logging::dir().to_string_lossy().into_owned(),
    }
}

/// The AppImage file Siler runs from, if it runs from one (`$APPIMAGE`, set by the
/// AppImage runtime).
pub fn appimage() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_file())
}

/// Where the host is copied when Siler runs as an AppImage: its own copy lives in the
/// AppImage's mount, which is gone when Siler quits.
fn stable_host_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("siler").join("bin"))
}

/// Copies the host out of the AppImage, with a note of the AppImage's path beside it
/// (`siler-app`), which the host starts when Siler isn't running.
fn stable_host(app: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let from = bundled_host().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "siler-nmhost was not found inside the AppImage",
        )
    })?;
    let dir = stable_host_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no data folder"))?;
    std::fs::create_dir_all(&dir)?;
    let host = dir.join("siler-nmhost");
    // Through a temporary name: the browser may be running the old copy.
    let tmp = dir.join(".siler-nmhost.new");
    std::fs::copy(&from, &tmp)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, &host)?;
    std::fs::write(dir.join("siler-app"), app.as_os_str().as_encoded_bytes())?;
    Ok(host)
}

/// On start-up from an AppImage: if the browsers use the copied host, update it and the
/// AppImage path it starts (the AppImage may have been moved or replaced by a newer one).
pub fn refresh_appimage() {
    let (Some(app), Some(dir)) = (appimage(), stable_host_dir()) else {
        return;
    };
    let ours = dir.join("siler-nmhost").to_string_lossy().into_owned();
    let used = targets()
        .iter()
        .any(|(_, _, path)| host_of(path).as_deref() == Some(ours.as_str()));
    if !used {
        return;
    }
    let current = std::fs::read(dir.join("siler-app")).ok();
    let same_app = current.as_deref() == Some(app.as_os_str().as_encoded_bytes());
    let same_host = bundled_host()
        .and_then(|b| std::fs::read(b).ok())
        .zip(std::fs::read(dir.join("siler-nmhost")).ok())
        .is_some_and(|(a, b)| a == b);
    if same_app && same_host {
        return;
    }
    match stable_host(&app) {
        Ok(_) => tracing::info!("updated the browser host for {}", app.display()),
        Err(e) => tracing::warn!("could not update the browser host: {e}"),
    }
}

/// Writes the host manifest for every browser found; returns the paths written.
pub fn install() -> std::io::Result<Vec<PathBuf>> {
    let host = match appimage() {
        Some(app) => stable_host(&app)?,
        None => bundled_host().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "siler-nmhost was not found next to the Siler executable",
            )
        })?,
    };
    let mut written = Vec::new();
    for (_, family, path) in targets() {
        let bytes = serde_json::to_vec_pretty(&manifest_json(&host, family))?;
        std::fs::create_dir_all(path.parent().expect("has parent"))?;
        std::fs::write(&path, &bytes)?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_per_family() {
        let host = Path::new("/usr/bin/siler-nmhost");
        let f = manifest_json(host, Family::Firefox);
        assert_eq!(f["allowed_extensions"][0], EXTENSION_ID);
        assert!(f.get("allowed_origins").is_none());
        let c = manifest_json(host, Family::Chromium);
        assert_eq!(
            c["allowed_origins"][0],
            "chrome-extension://legjbaodpdlommbgpdfcmogjedfgpjae/"
        );
        assert_eq!(c["path"], "/usr/bin/siler-nmhost");
        assert!(
            manifest_for(Path::new("/h/.config/chromium"), Family::Chromium)
                .ends_with("NativeMessagingHosts/com.siler.nmhost.json")
        );
    }
}
