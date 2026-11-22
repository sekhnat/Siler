//! Checking for a newer Siler, and updating an AppImage in place.
//!
//! The check asks GitHub for the latest release (through Siler's own proxy settings) once a
//! day, a minute after start, unless turned off in Settings. An AppImage updates itself: the
//! new AppImage is fetched next to the old one, checked against the release's SHA-256, swapped
//! in (the old one is kept as `.old` until the next start), and started. Other installs are
//! told where to get the update.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::AppState;
use crate::i18n::{tr, tr_args};

const DEFAULT_API: &str = "https://api.github.com/repos/sekhnat/Siler/releases/latest";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// How Siler was installed: what an update means for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallKind {
    /// Runs from an AppImage: it can replace itself.
    AppImage,
    /// Installed by the system's package manager (under /usr): updated by it.
    Packaged,
    /// The tarball, unpacked somewhere: the user replaces it.
    Tarball,
    /// A development build.
    Dev,
}

pub fn install_kind() -> InstallKind {
    if crate::integration::appimage().is_some() {
        return InstallKind::AppImage;
    }
    if cfg!(debug_assertions) {
        return InstallKind::Dev;
    }
    match std::env::current_exe() {
        Ok(p) if p.starts_with("/usr/") => InstallKind::Packaged,
        _ => InstallKind::Tarball,
    }
}

/// What the last check found.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    /// `latest` is newer than `current`.
    pub newer: bool,
    /// The release page.
    pub url: String,
    pub notes: String,
    pub install: InstallKind,
    /// The AppImage and its checksum file, when this is an AppImage install.
    pub appimage: Option<String>,
    pub sha256: Option<String>,
    pub checked_at: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    checked_at: i64,
}

/// The last result, for the window when it opens later.
pub static LAST: Mutex<Option<UpdateInfo>> = Mutex::new(None);

fn saved_path(app: &AppHandle) -> Option<PathBuf> {
    Some(app.path().app_data_dir().ok()?.join("update-check.json"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `v0.8.1` → (0, 8, 1). Pre-release suffixes are ignored.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((
        it.next()??,
        it.next().flatten().unwrap_or(0),
        it.next().flatten().unwrap_or(0),
    ))
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

const USER_AGENT: &str = concat!("Siler/", env!("CARGO_PKG_VERSION"));

/// Asks for the latest release.
pub async fn check(app: &AppHandle) -> Result<UpdateInfo, String> {
    let state = app.state::<AppState>();
    let api = std::env::var("SILER_UPDATE_API").unwrap_or_else(|_| DEFAULT_API.into());
    let body = state
        .engine
        .fetch_bytes(
            &api,
            1 << 20,
            &[
                ("Accept", "application/vnd.github+json"),
                ("User-Agent", USER_AGENT),
            ],
        )
        .await
        .map_err(|e| crate::i18n::error(&e))?;
    let rel: Release = serde_json::from_slice(&body).map_err(|e| {
        tr_args(
            "update-release-unreadable",
            vec![("why", e.to_string().into())],
        )
    })?;
    if rel.draft || rel.prerelease {
        return Err(tr("update-latest-is-prerelease"));
    }
    let latest = rel.tag_name.trim_start_matches('v').to_string();
    let current = env!("CARGO_PKG_VERSION").to_string();
    let asset = |pred: &dyn Fn(&str) -> bool| {
        rel.assets
            .iter()
            .find(|a| pred(&a.name))
            .map(|a| a.browser_download_url.clone())
    };
    let appimage_name = format!("Siler-{latest}-x86_64.AppImage");
    let info = UpdateInfo {
        newer: is_newer(&latest, &current),
        current,
        url: rel.html_url,
        notes: rel.body.unwrap_or_default(),
        install: install_kind(),
        appimage: asset(&|n| n == appimage_name),
        sha256: asset(&|n| n == format!("{appimage_name}.sha256")),
        checked_at: now_ms(),
        latest,
    };
    if let Some(p) = saved_path(app) {
        let _ = std::fs::write(
            p,
            serde_json::to_vec(&Saved {
                checked_at: info.checked_at,
            })
            .unwrap_or_default(),
        );
    }
    *LAST.lock().unwrap() = Some(info.clone());
    Ok(info)
}

/// Checks once a day (a minute after start, then every hour whether a day has passed), when
/// turned on, and tells the window, or the desktop when the window is hidden.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        loop {
            let settings = app.state::<AppState>().engine.settings();
            let last = saved_path(&app)
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|b| serde_json::from_slice::<Saved>(&b).ok())
                .map_or(0, |s| s.checked_at);
            if settings.check_updates && now_ms() - last >= DAY_MS {
                match check(&app).await {
                    Ok(info) if info.newer && info.latest != settings.skipped_version => {
                        announce(&app, &info)
                    }
                    Ok(_) => tracing::info!("update check: Siler is up to date"),
                    Err(e) => tracing::info!("update check failed: {e}"),
                }
            }
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });
}

fn announce(app: &AppHandle, info: &UpdateInfo) {
    tracing::info!("Siler {} is available", info.latest);
    let _ = app.emit("update-available", info);
    let visible = app
        .try_state::<AppState>()
        .is_some_and(|s| s.ui_visible.load(std::sync::atomic::Ordering::Relaxed));
    if !visible {
        let _ = app
            .notification()
            .builder()
            .title(tr_args(
                "update-available",
                vec![("version", info.latest.as_str().into())],
            ))
            .body(tr("update-open-siler-to-update"))
            .show();
    }
}

/// Removes the AppImage kept by the last update.
pub fn cleanup() {
    if let Some(app) = crate::integration::appimage() {
        let old = PathBuf::from(format!("{}.old", app.display()));
        if old.is_file() {
            let _ = std::fs::remove_file(&old);
        }
    }
}

#[derive(Clone, Serialize)]
struct Progress {
    done: u64,
    total: Option<u64>,
}

/// `<hash>  <name>` (sha256sum's format) or just the hash.
fn expected_hash(text: &str) -> Option<String> {
    let h = text.split_whitespace().next()?.to_ascii_lowercase();
    (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Fetches the new AppImage, checks it, swaps it in and starts it; this Siler then quits.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let info = LAST
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| tr("update-check-first"))?;
    if !info.newer {
        return Err(tr("update-up-to-date"));
    }
    let target = crate::integration::appimage().ok_or_else(|| tr("update-only-appimage"))?;
    let (Some(url), Some(sum_url)) = (info.appimage.clone(), info.sha256.clone()) else {
        return Err(tr("update-no-appimage-in-release"));
    };
    let state = app.state::<AppState>();
    let headers = [("User-Agent", USER_AGENT)];
    let sum = state
        .engine
        .fetch_bytes(&sum_url, 4096, &headers)
        .await
        .map_err(|e| e.to_string())?;
    let expected = expected_hash(&String::from_utf8_lossy(&sum))
        .ok_or_else(|| tr("update-checksum-unreadable"))?;
    let new = PathBuf::from(format!("{}.new", target.display()));
    let handle = app.clone();
    state
        .engine
        .fetch_to_file(&url, &new, &headers, move |done, total| {
            let _ = handle.emit("update-progress", Progress { done, total });
        })
        .await
        .map_err(|e| {
            let _ = std::fs::remove_file(&new);
            tr_args(
                "update-download-failed",
                vec![("why", crate::i18n::error(&e).into())],
            )
        })?;
    let actual = sha256_of(&new).map_err(|e| e.to_string())?;
    if actual != expected {
        let _ = std::fs::remove_file(&new);
        return Err(tr("update-checksum-mismatch"));
    }
    swap(&target, &new)
        .map_err(|e| tr_args("update-swap-failed", vec![("why", e.to_string().into())]))?;
    tracing::info!("updated {} to {}", target.display(), info.latest);
    relaunch(&target).map_err(|e| e.to_string())?;
    app.exit(0);
    Ok(())
}

/// Puts `new` in place of `target`, keeping the old file as `<target>.old`.
fn swap(target: &Path, new: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(target)?.permissions().mode() | 0o755;
    std::fs::set_permissions(new, std::fs::Permissions::from_mode(mode))?;
    let old = PathBuf::from(format!("{}.old", target.display()));
    std::fs::rename(target, &old)?;
    if let Err(e) = std::fs::rename(new, target) {
        let _ = std::fs::rename(&old, target);
        return Err(e);
    }
    Ok(())
}

/// Starts the new AppImage once this one has quit (the running copy holds the single-instance
/// lock until then).
fn relaunch(target: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    siler_core::host::command("sh")
        .arg("-c")
        .arg("sleep 2; exec \"$0\"")
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("0.8.1", "0.8.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.0"));
        assert!(!is_newer("0.8.0", "0.8.0"));
        assert!(!is_newer("0.7.5", "0.8.0"));
        assert!(!is_newer("nonsense", "0.8.0"));
        assert!(!is_newer("0.8.0-beta.1", "0.8.0"));
    }

    #[test]
    fn checksum_files() {
        let h = "a".repeat(64);
        assert_eq!(
            expected_hash(&format!("{h}  Siler-0.8.1-x86_64.AppImage\n")),
            Some(h.clone())
        );
        assert_eq!(expected_hash(&h.to_uppercase()), Some(h));
        assert_eq!(expected_hash("abc  x"), None);
    }

    #[test]
    fn swapping_keeps_the_old_copy() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("Siler.AppImage");
        let new = dir.path().join("Siler.AppImage.new");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        swap(&target, &new).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(
            std::fs::read(dir.path().join("Siler.AppImage.old")).unwrap(),
            b"old"
        );
        assert!(!new.exists());
    }
}
