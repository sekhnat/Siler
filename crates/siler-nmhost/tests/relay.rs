//! The host as the browser runs it, with a socket of the test's in Siler's place.

use serde_json::{Value, json};
use siler_ipc::{read_frame, write_frame};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const PATIENCE: Duration = Duration::from_secs(10);

struct Host {
    child: Child,
    to: Option<ChildStdin>,
    from: mpsc::Receiver<Value>,
    launched: PathBuf,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Host {
    fn start() -> Host {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("siler");
        let launched = dir.path().join("launched");
        std::fs::write(&app, format!("#!/bin/sh\n: > '{}'\n", launched.display())).unwrap();
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(dir.path().join("siler-rt")).unwrap();
        std::fs::create_dir(dir.path().join("siler-rt/siler")).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_siler-nmhost"))
            .env("XDG_RUNTIME_DIR", dir.path().join("siler-rt"))
            .env("XDG_STATE_HOME", dir.path().join("state"))
            .env("SILER_APP", &app)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = child.stdout.take().unwrap();
        let (tx, from) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(Some(frame)) = read_frame(&mut out) {
                if tx.send(serde_json::from_slice(&frame).unwrap()).is_err() {
                    break;
                }
            }
        });
        Host {
            to: child.stdin.take(),
            child,
            from,
            launched,
            socket: dir.path().join("siler-rt/siler/siler.sock"),
            _dir: dir,
        }
    }

    fn send(&mut self, v: Value) {
        write_frame(self.to.as_mut().unwrap(), v.to_string().as_bytes()).unwrap();
    }

    fn next(&self) -> Value {
        self.from
            .recv_timeout(PATIENCE)
            .expect("a message from the host")
    }

    fn next_of(&self, kind: &str) -> Value {
        let until = Instant::now() + PATIENCE;
        loop {
            let v = self.next();
            if v["type"] == kind {
                return v;
            }
            assert!(Instant::now() < until, "no {kind}: {v}");
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn accept(listener: &UnixListener) -> UnixStream {
    listener.set_nonblocking(true).unwrap();
    let until = Instant::now() + PATIENCE;
    loop {
        if let Ok((s, _)) = listener.accept() {
            s.set_nonblocking(false).unwrap();
            s.set_read_timeout(Some(PATIENCE)).unwrap();
            return s;
        }
        assert!(Instant::now() < until, "the host didn't connect");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read(s: &mut UnixStream) -> Value {
    serde_json::from_slice(&read_frame(s).unwrap().expect("a frame")).unwrap()
}

fn subscribe() -> Value {
    json!({"type": "ping", "protocol": 2, "subscribe": {"browser": "b-1"}})
}

#[test]
fn waits_for_siler_and_never_starts_it() {
    let mut host = Host::start();
    host.send(subscribe());
    assert_eq!(host.next()["type"], "waiting");
    host.send(json!({"type": "beat"}));
    assert_eq!(host.next_of("beat")["type"], "beat");
    assert!(!host.launched.exists(), "it started Siler");

    let listener = UnixListener::bind(&host.socket).unwrap();
    let mut app = accept(&listener);
    assert_eq!(read(&mut app)["subscribe"]["browser"], "b-1");
    write_frame(
        &mut app,
        br#"{"type":"subscribed","version":"9","protocol":2}"#,
    )
    .unwrap();
    assert_eq!(host.next_of("subscribed")["version"], "9");

    // Both ways, whenever either side has something to say.
    write_frame(&mut app, br#"{"type":"find_link","request":1}"#).unwrap();
    assert_eq!(host.next_of("find_link")["request"], 1);
    write_frame(&mut app, br#"{"type":"find_link","request":2}"#).unwrap();
    assert_eq!(host.next_of("find_link")["request"], 2);
    host.send(json!({"type": "link_found", "request": 2, "error": "none"}));
    assert_eq!(read(&mut app)["request"], 2);

    // Siler restarts: the host subscribes again by itself.
    drop(app);
    assert_eq!(host.next_of("waiting")["type"], "waiting");
    let mut app = accept(&listener);
    assert_eq!(read(&mut app)["subscribe"]["browser"], "b-1");
    assert!(!host.launched.exists(), "it started Siler");
}

#[test]
fn ends_with_the_browsers_port() {
    let mut host = Host::start();
    host.send(subscribe());
    host.next();
    drop(host.to.take());
    let until = Instant::now() + PATIENCE;
    while host.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < until, "the host stayed");
        std::thread::sleep(Duration::from_millis(50));
    }
}
