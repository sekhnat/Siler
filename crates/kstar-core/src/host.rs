//! Starting the user's own programs (players, file managers, Firefox, `gsettings`,
//! `notify-send`) from K-Star.
//!
//! Run as an AppImage, K-Star's environment points into the AppImage: `LD_LIBRARY_PATH`
//! names its bundled libraries, and `PATH`, `XDG_DATA_DIRS`, the GTK, GIO, GSettings,
//! GStreamer, Qt and Python paths name its folders. A system program that inherits these
//! loads the bundled libraries in place of its own and can fail to start (mpv: "symbol
//! lookup error: /usr/lib/libcurl.so.4: undefined symbol: nghttp2_…"). [`command`] gives a
//! program the environment without them. Programs inside the AppImage keep it.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;

/// What the AppImage runtime sets about itself.
const OWN: &[&str] = &[
    "APPDIR",
    "APPIMAGE",
    "APPIMAGE_ARCH",
    "APPIMAGE_UID",
    "APPOFFSET",
    "ARGV0",
    "OWD",
];

/// A command for `program` with the environment the user's system gave K-Star, when the
/// program isn't one of the AppImage's own.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program.as_ref());
    prepare(&mut cmd);
    cmd
}

/// Gives a command made elsewhere (e.g. by the `open` crate) the same environment.
pub fn prepare(cmd: &mut Command) {
    if let Some(appdir) = std::env::var_os("APPDIR").filter(|d| !d.is_empty())
        && !inside(Path::new(cmd.get_program()).as_os_str(), &appdir)
    {
        apply(cmd, changes(std::env::vars_os(), &appdir));
    }
}

/// The changes that undo an AppImage's environment: `Some` sets a variable, `None` removes
/// it. A list of paths loses its entries inside `appdir`, and is removed if nothing is
/// left.
pub fn changes(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
    appdir: &OsStr,
) -> Vec<(OsString, Option<OsString>)> {
    let mut out = Vec::new();
    for (name, value) in vars {
        if OWN.iter().any(|o| name == OsStr::new(o)) {
            out.push((name, None));
            continue;
        }
        let bytes = value.as_bytes();
        if !bytes.windows(appdir.len()).any(|w| w == appdir.as_bytes()) {
            continue;
        }
        let kept: Vec<&[u8]> = bytes
            .split(|&b| b == b':')
            .filter(|e| !e.is_empty() && !inside(OsStr::from_bytes(e), appdir))
            .collect();
        let value = (!kept.is_empty()).then(|| OsStr::from_bytes(&kept.join(&b':')).to_owned());
        out.push((name, value));
    }
    out
}

fn apply(cmd: &mut Command, changes: Vec<(OsString, Option<OsString>)>) {
    for (name, value) in changes {
        match value {
            Some(v) => cmd.env(name, v),
            None => cmd.env_remove(name),
        };
    }
}

/// Whether `path` is `appdir` or under it (the runtime writes some entries as `dir//usr`).
fn inside(path: &OsStr, appdir: &OsStr) -> bool {
    let (p, d) = (path.as_bytes(), appdir.as_bytes());
    let d = d.strip_suffix(b"/").unwrap_or(d);
    p.strip_prefix(d)
        .is_some_and(|rest| rest.is_empty() || rest[0] == b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(vars: &[(&str, &str)]) -> Vec<(String, Option<String>)> {
        let vars = vars
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        changes(vars, OsStr::new("/tmp/.mount_K-StarAb12"))
            .into_iter()
            .map(|(k, v)| {
                (
                    k.into_string().unwrap(),
                    v.map(|v| v.into_string().unwrap()),
                )
            })
            .collect()
    }

    #[test]
    fn appimage_entries_are_dropped_and_the_systems_kept() {
        let got = run(&[
            (
                "LD_LIBRARY_PATH",
                "/tmp/.mount_K-StarAb12/usr/lib/:/tmp/.mount_K-StarAb12/usr/lib/x86_64-linux-gnu/",
            ),
            (
                "PATH",
                "/tmp/.mount_K-StarAb12/usr/bin/:/usr/local/bin:/usr/bin",
            ),
            (
                "XDG_DATA_DIRS",
                "/tmp/.mount_K-StarAb12/usr/share/:/usr/share:/home/u/.local/share",
            ),
            (
                "GIO_MODULE_DIR",
                "/tmp/.mount_K-StarAb12//usr/lib/gio/modules",
            ),
            ("PYTHONPATH", "/tmp/.mount_K-StarAb12/usr/share/pyshared/:"),
            ("APPDIR", "/tmp/.mount_K-StarAb12"),
            ("APPIMAGE", "/home/u/Applications/K-Star"),
            ("OWD", "/home/u"),
            ("HOME", "/home/u"),
            ("WAYLAND_DISPLAY", "wayland-0"),
        ]);
        let want: Vec<(String, Option<String>)> = [
            ("LD_LIBRARY_PATH", None),
            ("PATH", Some("/usr/local/bin:/usr/bin")),
            ("XDG_DATA_DIRS", Some("/usr/share:/home/u/.local/share")),
            ("GIO_MODULE_DIR", None),
            ("PYTHONPATH", None),
            ("APPDIR", None),
            ("APPIMAGE", None),
            ("OWD", None),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn a_similarly_named_folder_is_not_the_appimage() {
        let got = run(&[(
            "PATH",
            "/tmp/.mount_K-StarAb12x/bin:/tmp/.mount_K-StarAb12/usr/bin",
        )]);
        assert_eq!(
            got,
            [(
                "PATH".to_string(),
                Some("/tmp/.mount_K-StarAb12x/bin".to_string())
            )]
        );
    }

    /// Run by `a_system_program_gets_the_systems_environment`, in a child that looks like
    /// K-Star inside an AppImage.
    #[test]
    #[ignore]
    fn appimage_child() {
        let out = command("sh")
            .args([
                "-c",
                r#"printf '%s|%s|%s' "${LD_LIBRARY_PATH-unset}" "$PATH" "${APPDIR-unset}""#,
            ])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "unset|/usr/bin:/bin|unset"
        );
    }

    #[test]
    fn a_system_program_gets_the_systems_environment() {
        let fake = "/tmp/.mount_K-StarTest";
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "host::tests::appimage_child",
                "--ignored",
                "--quiet",
            ])
            .env("APPDIR", fake)
            .env("LD_LIBRARY_PATH", format!("{fake}/usr/lib/"))
            .env("PATH", format!("{fake}/usr/bin/:/usr/bin:/bin"))
            .status()
            .unwrap();
        assert!(status.success(), "the child saw the AppImage's environment");
    }

    #[test]
    fn programs_inside_the_appimage_are_its_own() {
        let d = OsStr::new("/tmp/.mount_K-StarAb12");
        assert!(inside(
            OsStr::new("/tmp/.mount_K-StarAb12/usr/bin/ffmpeg"),
            d
        ));
        assert!(!inside(OsStr::new("/usr/bin/mpv"), d));
        assert!(!inside(OsStr::new("mpv"), d));
    }
}
