//! Native messaging host for the Siler browser extension.
//!
//! Firefox starts this program and talks to it over stdin/stdout. Each message is
//! relayed to the running Siler app over its Unix socket; if the app isn't running it is
//! started in the background first. The reply is relayed back to the browser.

use siler_ipc::{Response, log_dir, read_frame, socket_path, write_frame};
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The log is started over once it grows past this.
const MAX_LOG: u64 = 1024 * 1024;

/// Appends a line to `nmhost.log`. Logging must never break the relay, so errors are ignored.
fn log(msg: &str) {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("nmhost.log");
    let too_big = std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG);
    let file = if too_big {
        File::create(&path)
    } else {
        OpenOptions::new().create(true).append(true).open(&path)
    };
    if let Ok(mut f) = file {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

fn main() {
    // Firefox passes the manifest path and extension id as arguments; nothing to do with them.
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = BufReader::new(stdin.lock());
    let mut output = BufWriter::new(stdout.lock());

    loop {
        let frame = match read_frame(&mut input) {
            Ok(Some(f)) => f,
            Ok(None) => break, // browser closed the port
            Err(e) => {
                log(&format!("bad input from the browser: {e}"));
                break;
            }
        };
        let reply = relay(&frame).unwrap_or_else(|e| {
            if !(is_ping(&frame) && e.kind() == io::ErrorKind::NotConnected) {
                log(&format!("relay failed: {e}"));
            }
            serde_json::to_vec(&Response::Error {
                message: e.to_string(),
            })
            .expect("serializable")
        });
        if write_frame(&mut output, &reply).is_err() {
            break;
        }
    }
}

fn relay(frame: &[u8]) -> io::Result<Vec<u8>> {
    let mut stream = connect_or_launch(is_ping(frame))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    write_frame(&mut stream, frame)?;
    read_frame(&mut stream)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "Siler closed the connection"))
}

fn is_ping(frame: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(frame)
        .ok()
        .is_some_and(|v| v.get("type").and_then(|t| t.as_str()) == Some("ping"))
}

/// Connects to the app; for anything but a health-check ping, starts the app if needed.
fn connect_or_launch(ping: bool) -> io::Result<UnixStream> {
    let path = socket_path();
    if let Ok(s) = UnixStream::connect(&path) {
        return Ok(s);
    }
    if ping {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "Siler is not running",
        ));
    }
    launch_app()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut delay = Duration::from_millis(50);
    loop {
        std::thread::sleep(delay);
        match UnixStream::connect(&path) {
            Ok(s) => return Ok(s),
            Err(e) if Instant::now() >= deadline => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("Siler did not start: {e}"),
                ));
            }
            Err(_) => delay = (delay * 2).min(Duration::from_millis(500)),
        }
    }
}

fn app_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("SILER_APP") {
        return p.into();
    }
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from));
    // Copied out of an AppImage: the AppImage to start is noted beside this host.
    if let Some(app) = dir
        .as_ref()
        .and_then(|d| std::fs::read_to_string(d.join("siler-app")).ok())
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.is_absolute() && p.exists())
    {
        return app;
    }
    if let Some(sibling) = dir.map(|d| d.join("siler")).filter(|p| p.exists()) {
        return sibling;
    }
    PathBuf::from("siler")
}

fn launch_app() -> io::Result<()> {
    let bin = app_binary();
    log(&format!("starting {}", bin.display()));
    // The app logs to its own file; this catches what happens before that is set up
    // (GTK warnings, crashes during start-up).
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let stderr = File::create(dir.join("siler-stderr.log"))
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    Command::new(&bin)
        .arg("--background")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .process_group(0) // don't die with the browser's process group
        .spawn()
        .map(drop)
        .inspect_err(|e| log(&format!("could not start {}: {e}", bin.display())))
}
