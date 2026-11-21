//! Opening files, folders and web links with the user's default applications, and
//! starting Firefox. In place of the opener plugin, whose launchers inherit an AppImage's
//! environment and hand it to the player or browser they start (see
//! [`kstar_core::host`]).

use std::ffi::OsStr;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// How long a launcher has to fail before it counts as having started the application.
const SETTLE: Duration = Duration::from_millis(1500);

/// Opens `target` (a path or a web link) with its default application. Each launcher the
/// desktop may have (`xdg-open`, `gio open`, …) is tried until one doesn't fail.
pub fn open(target: impl AsRef<OsStr>) -> io::Result<()> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no launcher found to open it with");
    for mut cmd in open::commands(target) {
        kstar_core::host::prepare(&mut cmd);
        let started = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
        match started.and_then(settle) {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Starts a program of the user's (e.g. Firefox) without waiting for it.
pub fn start(program: &str, args: &[&OsStr]) -> io::Result<()> {
    let child = kstar_core::host::command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    reap(child);
    Ok(())
}

/// Waits a moment for a launcher to fail; one still running then is left to finish.
fn settle(mut child: Child) -> io::Result<()> {
    let until = Instant::now() + SETTLE;
    while Instant::now() < until {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!("the launcher failed ({status})")))
            };
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    reap(child);
    Ok(())
}

/// Collects the program's exit in the background, so it doesn't linger as a zombie.
fn reap(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}
