//! The `.desktop` entry and icons. Without them KDE and GNOME can't match Siler's windows
//! (Wayland app id `siler`) to an application, and show a generic icon.

use std::io;
use std::path::{Path, PathBuf};

pub const APP_ID: &str = "siler";
pub(crate) const MARKER: &str = "X-Siler-Managed=true";

const ICONS: &[(&str, &[u8])] = &[
    ("32x32", include_bytes!("../icons/32x32.png")),
    ("64x64", include_bytes!("../icons/64x64.png")),
    ("128x128", include_bytes!("../icons/128x128.png")),
    ("256x256", include_bytes!("../icons/128x128@2x.png")),
    ("512x512", include_bytes!("../icons/icon.png")),
];
const SVG: &[u8] = include_bytes!("../../assets/logo.svg");

fn data_home() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".local")
            .join("share")
    })
}

pub fn entry_path() -> PathBuf {
    data_home()
        .join("applications")
        .join(format!("{APP_ID}.desktop"))
}

/// The program the menu entry starts: the AppImage file when Siler runs from one (the
/// executable itself sits in a mount that changes every run), else this executable.
pub(crate) fn exe() -> io::Result<PathBuf> {
    match crate::integration::appimage() {
        Some(file) => Ok(file),
        None => std::env::current_exe(),
    }
}

/// The program's path as a quoted `Exec=` argument.
pub(crate) fn quoted(exe: &Path) -> String {
    let path = exe
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{path}\"")
}

fn entry(exe: &Path) -> String {
    let exec = quoted(exe);
    format!(
        "[Desktop Entry]
Type=Application
Name=Siler
GenericName=Download Manager
Comment=Fast downloads with many connections
Exec={exec} %U
Icon={APP_ID}
Terminal=false
Categories=Network;FileTransfer;
Keywords=download;manager;segmented;
StartupNotify=true
StartupWMClass={APP_ID}
{MARKER}
"
    )
}

/// Our entry, pointing at this executable.
pub fn is_current() -> bool {
    let (Ok(text), Ok(exe)) = (std::fs::read_to_string(entry_path()), exe()) else {
        return false;
    };
    text == entry(&exe)
}

pub fn install() -> io::Result<()> {
    let base = data_home();
    for (size, png) in ICONS {
        let dir = base.join("icons/hicolor").join(size).join("apps");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(format!("{APP_ID}.png")), png)?;
    }
    let dir = base.join("icons/hicolor/scalable/apps");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{APP_ID}.svg")), SVG)?;

    let path = entry_path();
    std::fs::create_dir_all(path.parent().expect("has parent"))?;
    std::fs::write(&path, entry(&exe()?))?;
    // Refresh the caches if the tools exist; desktops pick the files up anyway.
    let _ = siler_core::host::command("update-desktop-database")
        .arg(base.join("applications"))
        .output();
    let _ = siler_core::host::command("gtk-update-icon-cache")
        .args(["-q", "-t"])
        .arg(base.join("icons/hicolor"))
        .output();
    Ok(())
}

/// Installs the entry on start-up when it is missing, or when it is ours but points at
/// another executable. Entries without our marker were written by someone else: leave them.
pub fn ensure() {
    let path = entry_path();
    let ours_or_missing = match std::fs::read_to_string(&path) {
        Ok(text) => text.contains(MARKER),
        Err(_) => true,
    };
    if ours_or_missing && !is_current() {
        match install() {
            Ok(()) => tracing::info!("installed {}", path.display()),
            Err(e) => tracing::warn!("could not install {}: {e}", path.display()),
        }
    }
}
