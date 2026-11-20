//! Sounds for download events, from the desktop's sound theme (freedesktop sound naming)
//! or a file the user picked. Played with whichever player the system has, so K-Star
//! doesn't link an audio stack.

use kstar_core::SoundSettings;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sound {
    Complete,
    Failed,
    QueueDone,
}

impl Sound {
    /// Sound names to try, best first.
    fn names(self) -> &'static [&'static str] {
        match self {
            Sound::Complete => &["complete", "completion-success", "outcome-success"],
            Sound::Failed => &["dialog-error", "completion-fail", "dialog-warning"],
            Sound::QueueDone => &["message-new-instant", "outcome-success", "complete"],
        }
    }
}

/// The theme set in KDE's `kdeglobals` (`[Sounds] Theme=`).
fn kde_theme(kdeglobals: &str) -> Option<String> {
    let mut in_sounds = false;
    for line in kdeglobals.lines().map(str::trim) {
        if line.starts_with('[') {
            in_sounds = line == "[Sounds]";
        } else if in_sounds && let Some(v) = line.strip_prefix("Theme=") {
            return Some(v.trim().to_string()).filter(|v| !v.is_empty());
        }
    }
    None
}

/// Themes to look in, best first.
fn themes(configured: Option<String>, kde: bool) -> Vec<String> {
    let mut t: Vec<String> = configured.into_iter().collect();
    if kde {
        t.push("ocean".into());
    }
    t.push("freedesktop".into());
    t.dedup();
    t
}

/// Finds the file for `sound` in `themes` under the `sounds` folders of `data_dirs`.
fn find(sound: Sound, themes: &[String], data_dirs: &[PathBuf]) -> Option<PathBuf> {
    for name in sound.names() {
        for theme in themes {
            for dir in data_dirs {
                for ext in ["oga", "ogg", "wav"] {
                    let p = dir
                        .join("sounds")
                        .join(theme)
                        .join("stereo")
                        .join(format!("{name}.{ext}"));
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = dirs::data_dir().into_iter().collect();
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').map(PathBuf::from));
    dirs
}

fn theme_file(sound: Sound) -> Option<PathBuf> {
    let kde = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.contains("KDE"));
    let configured = dirs::config_dir()
        .and_then(|c| std::fs::read_to_string(c.join("kdeglobals")).ok())
        .and_then(|t| kde_theme(&t));
    find(sound, &themes(configured, kde), &data_dirs())
}

fn file_for(s: &SoundSettings, sound: Sound) -> Option<PathBuf> {
    let custom = match sound {
        Sound::Complete => &s.complete_file,
        Sound::Failed => &s.failed_file,
        Sound::QueueDone => &s.queue_done_file,
    };
    custom
        .clone()
        .filter(|p| p.is_file())
        .or_else(|| theme_file(sound))
}

fn play_file(path: &Path) {
    let players: [(&str, &[&str]); 3] = [
        ("pw-play", &[]),
        ("paplay", &[]),
        ("canberra-gtk-play", &["-f"]),
    ];
    for (player, args) in players {
        let started = Command::new(player)
            .args(args)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match started {
            Ok(_) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                tracing::debug!("{player}: {e}");
                return;
            }
        }
    }
    tracing::debug!("no sound player found (pw-play, paplay or canberra-gtk-play)");
}

/// Plays the sound for an event if sounds are on for it.
pub fn event(s: &SoundSettings, sound: Sound) {
    let on = s.enabled
        && match sound {
            Sound::Complete => s.on_complete,
            Sound::Failed => s.on_failed,
            Sound::QueueDone => s.on_queue_done,
        };
    if on {
        play(s, sound);
    }
}

/// Plays a sound regardless of the switches (the Test button).
pub fn play(s: &SoundSettings, sound: Sound) {
    let s = s.clone();
    std::thread::spawn(move || match file_for(&s, sound) {
        Some(path) => play_file(&path),
        None => tracing::debug!("no sound file for {sound:?}"),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        let share = tmp.path().to_path_buf();
        let put = |theme: &str, file: &str| {
            let d = share.join("sounds").join(theme).join("stereo");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(file), b"x").unwrap();
        };
        put("freedesktop", "complete.oga");
        put("freedesktop", "dialog-error.oga");
        put("ocean", "outcome-success.oga");
        put("ocean", "dialog-error.ogg");

        let kde = themes(None, true);
        assert_eq!(kde, ["ocean", "freedesktop"]);
        let dirs = [PathBuf::from("/nonexistent"), share.clone()];
        // The first name that exists anywhere wins over a better theme.
        assert_eq!(
            find(Sound::Complete, &kde, &dirs).unwrap(),
            share.join("sounds/freedesktop/stereo/complete.oga")
        );
        assert_eq!(
            find(Sound::Failed, &kde, &dirs).unwrap(),
            share.join("sounds/ocean/stereo/dialog-error.ogg")
        );
        assert_eq!(
            find(Sound::QueueDone, &themes(None, false), &dirs).unwrap(),
            share.join("sounds/freedesktop/stereo/complete.oga")
        );
        assert!(find(Sound::Complete, &["none".into()], &dirs).is_none());
    }

    #[test]
    fn kde_theme_setting() {
        let text = "[General]\nTheme=foo\n\n[Sounds]\nTheme=oxygen\n";
        assert_eq!(kde_theme(text).as_deref(), Some("oxygen"));
        assert_eq!(kde_theme("[Sounds]\nEnable=true\n"), None);
        assert_eq!(
            themes(Some("oxygen".into()), true),
            ["oxygen", "ocean", "freedesktop"]
        );
    }
}
