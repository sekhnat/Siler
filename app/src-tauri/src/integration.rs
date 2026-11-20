//! Registers the native messaging host with Firefox and the Firefox-based browsers.

use serde::Serialize;
use std::path::{Path, PathBuf};

pub const HOST_NAME: &str = "com.kstar.nmhost";
pub const EXTENSION_ID: &str = "kstar@kstar.app";

/// Browsers and the folder under `$HOME` they read native host manifests from.
/// Firefox's folder is always written: Zen and Floorp read it too. The others get a copy
/// when they are installed (their folder exists); LibreWolf needs its own.
const BROWSERS: &[(&str, &str)] = &[
    ("Firefox", ".mozilla"),
    ("LibreWolf", ".librewolf"),
    ("Waterfox", ".waterfox"),
    ("Floorp", ".floorp"),
    ("Zen", ".zen"),
];

#[derive(Debug, Serialize)]
pub struct BrowserStatus {
    pub name: &'static str,
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
    base.join("native-messaging-hosts")
        .join(format!("{HOST_NAME}.json"))
}

/// (name, manifest path) of every browser to register with.
fn targets() -> Vec<(&'static str, PathBuf)> {
    let home = home();
    BROWSERS
        .iter()
        .filter(|(name, dir)| *name == "Firefox" || home.join(dir).is_dir())
        .map(|(name, dir)| (*name, manifest_in(&home.join(dir))))
        .collect()
}

/// The `kstar-nmhost` binary shipped next to the app.
fn bundled_host() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let host = exe.parent()?.join("kstar-nmhost");
    host.exists().then_some(host)
}

/// Where `scripts/sign-extension.sh` puts the signed extension.
pub fn signed_xpi() -> Option<PathBuf> {
    let p = dirs::data_dir()?.join("kstar").join("kstar-integration.xpi");
    p.exists().then_some(p)
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
        .map(|(name, path)| BrowserStatus {
            name,
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
        protocol: kstar_ipc::PROTOCOL,
        extension_version: extension.as_ref().map(|e| e.0.clone()),
        extension_protocol: extension.map(|e| e.1),
        desktop_entry: crate::desktop::is_current(),
        tray_available,
        signed_xpi: signed_xpi().map(|p| p.to_string_lossy().into_owned()),
        log_dir: crate::logging::dir().to_string_lossy().into_owned(),
    }
}

/// Writes the host manifest for every browser found; returns the paths written.
pub fn install() -> std::io::Result<Vec<PathBuf>> {
    let host = bundled_host().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "kstar-nmhost was not found next to the K-Star executable",
        )
    })?;
    let manifest = serde_json::json!({
        "name": HOST_NAME,
        "description": "K-Star download manager",
        "path": host,
        "type": "stdio",
        "allowed_extensions": [EXTENSION_ID],
    });
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    let mut written = Vec::new();
    for (_, path) in targets() {
        std::fs::create_dir_all(path.parent().expect("has parent"))?;
        std::fs::write(&path, &bytes)?;
        written.push(path);
    }
    Ok(written)
}
