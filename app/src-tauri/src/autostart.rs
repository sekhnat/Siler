//! Starting Siler when you log in: an XDG autostart entry
//! (`~/.config/autostart/siler.desktop`) that runs `siler --background`.
//!
//! The file itself is the setting, so turning it off in the desktop's own autostart
//! settings (KDE, GNOME) is respected: they delete the file or add `Hidden=true`.

use std::io;
use std::path::{Path, PathBuf};

use crate::desktop::{APP_ID, MARKER};

fn config_home() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
}

pub fn path() -> PathBuf {
    config_home()
        .join("autostart")
        .join(format!("{APP_ID}.desktop"))
}

fn entry(exe: &Path) -> String {
    let exec = crate::desktop::quoted(exe);
    format!(
        "[Desktop Entry]
Type=Application
Name=Siler
Comment=Start Siler in the tray
Exec={exec} --background
Icon={APP_ID}
Terminal=false
X-GNOME-Autostart-enabled=true
{MARKER}
"
    )
}

/// Whether an autostart file's text is our entry and hasn't been switched off.
fn ours_and_enabled(text: &str) -> bool {
    let off = text.lines().map(str::trim).any(|l| {
        l.eq_ignore_ascii_case("Hidden=true")
            || l.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
    });
    text.contains(MARKER) && !off
}

/// Siler starts when you log in.
pub fn enabled() -> bool {
    std::fs::read_to_string(path()).is_ok_and(|t| ours_and_enabled(&t))
}

/// Turns starting at login on or off. Turning it off removes only our own entry.
pub fn set(on: bool) -> io::Result<()> {
    let path = path();
    if on {
        std::fs::create_dir_all(path.parent().expect("has parent"))?;
        std::fs::write(&path, entry(&crate::desktop::exe()?))
    } else {
        match std::fs::read_to_string(&path) {
            Ok(text) if text.contains(MARKER) => std::fs::remove_file(&path),
            Ok(_) => Err(io::Error::other(format!(
                "{} was not made by Siler; remove it in your desktop's autostart settings",
                path.display()
            ))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// On start-up: when starting at login is on, keep the entry pointing at this executable
/// (the AppImage may have been moved or replaced).
pub fn ensure() {
    let path = path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(exe) = crate::desktop::exe() else {
        return;
    };
    if ours_and_enabled(&text) && text != entry(&exe) {
        match std::fs::write(&path, entry(&exe)) {
            Ok(()) => tracing::info!("updated {}", path.display()),
            Err(e) => tracing::warn!("could not update {}: {e}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_starts_in_the_tray() {
        let text = entry(Path::new("/home/u/Apps/Siler \"x\".AppImage"));
        assert!(text.contains("Exec=\"/home/u/Apps/Siler \\\"x\\\".AppImage\" --background\n"));
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(ours_and_enabled(&text));
    }

    #[test]
    fn switched_off_or_foreign_entries_are_not_enabled() {
        let ours = entry(Path::new("/usr/bin/siler"));
        assert!(!ours_and_enabled(&format!("{ours}Hidden=true\n")));
        assert!(!ours_and_enabled(&ours.replace(
            "X-GNOME-Autostart-enabled=true",
            "X-GNOME-Autostart-enabled=false"
        )));
        assert!(!ours_and_enabled(
            "[Desktop Entry]\nExec=siler --background\n"
        ));
    }
}
