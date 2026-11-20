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
    let p = dirs::data_dir()?
        .join("kstar")
        .join("kstar-integration.xpi");
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

/// The AppImage file K-Star runs from, if it runs from one (`$APPIMAGE`, set by the
/// AppImage runtime).
pub fn appimage() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_file())
}

/// Where the host is copied when K-Star runs as an AppImage: its own copy lives in the
/// AppImage's mount, which is gone when K-Star quits.
fn stable_host_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("kstar").join("bin"))
}

/// Copies the host out of the AppImage, with a note of the AppImage's path beside it
/// (`kstar-app`), which the host starts when K-Star isn't running.
fn stable_host(app: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let from = bundled_host().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "kstar-nmhost was not found inside the AppImage",
        )
    })?;
    let dir = stable_host_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no data folder"))?;
    std::fs::create_dir_all(&dir)?;
    let host = dir.join("kstar-nmhost");
    // Through a temporary name: the browser may be running the old copy.
    let tmp = dir.join(".kstar-nmhost.new");
    std::fs::copy(&from, &tmp)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, &host)?;
    std::fs::write(dir.join("kstar-app"), app.as_os_str().as_encoded_bytes())?;
    Ok(host)
}

/// On start-up from an AppImage: if the browsers use the copied host, update it and the
/// AppImage path it starts (the AppImage may have been moved or replaced by a newer one).
pub fn refresh_appimage() {
    let (Some(app), Some(dir)) = (appimage(), stable_host_dir()) else {
        return;
    };
    let firefox = manifest_in(&home().join(".mozilla"));
    if host_of(&firefox).as_deref() != Some(dir.join("kstar-nmhost").to_string_lossy().as_ref()) {
        return;
    }
    let current = std::fs::read(dir.join("kstar-app")).ok();
    let same_app = current.as_deref() == Some(app.as_os_str().as_encoded_bytes());
    let same_host = bundled_host()
        .and_then(|b| std::fs::read(b).ok())
        .zip(std::fs::read(dir.join("kstar-nmhost")).ok())
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
                "kstar-nmhost was not found next to the K-Star executable",
            )
        })?,
    };
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
