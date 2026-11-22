//! A port the browser keeps open, to be asked for fresh links. It waits for Siler when it
//! isn't running and never starts it.

use siler_ipc::{Response, read_frame, socket_path, write_frame};
use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const AGAIN: Duration = Duration::from_secs(3);

fn kind(frame: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(frame).ok()?;
    Some(v.get("type")?.as_str()?.to_string())
}

/// Whether a frame asks to stay subscribed.
pub fn asked(frame: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(frame)
        .ok()
        .and_then(|v| v.get("subscribe").map(|s| !s.is_null()))
        .unwrap_or(false)
}

fn to_browser(frame: &[u8]) -> bool {
    write_frame(&mut io::stdout().lock(), frame).is_ok()
}

fn say(message: &Response) -> bool {
    serde_json::to_vec(message).is_ok_and(|frame| to_browser(&frame))
}

type Connection = Arc<Mutex<Option<UnixStream>>>;

/// Relays between the port and Siler until the browser closes the port.
pub fn run(subscription: Vec<u8>, input: &mut impl Read) {
    let app: Connection = Arc::new(Mutex::new(None));
    let wanted = Arc::new(Mutex::new(subscription));
    let (shared, first) = (app.clone(), wanted.clone());
    // It ends with the process, which ends with the port.
    std::thread::spawn(move || keep_connected(&shared, &first));

    while let Ok(Some(frame)) = read_frame(input) {
        match kind(&frame).as_deref() {
            // The host's own: it keeps the port alive with Siler running or not.
            Some("beat") => {
                if !say(&Response::Beat) {
                    break;
                }
            }
            _ => {
                if asked(&frame) {
                    wanted.lock().unwrap().clone_from(&frame);
                }
                if let Some(stream) = app.lock().unwrap().as_mut()
                    && let Err(e) = write_frame(stream, &frame)
                {
                    crate::log(&format!("Siler didn't take a message: {e}"));
                }
            }
        }
    }
}

fn keep_connected(app: &Connection, wanted: &Mutex<Vec<u8>>) {
    let mut told = false;
    loop {
        if let Ok(stream) = UnixStream::connect(socket_path())
            && let Ok(mut reader) = stream.try_clone()
        {
            let subscribed = {
                let mut slot = app.lock().unwrap();
                let first = wanted.lock().unwrap().clone();
                let stream = slot.insert(stream);
                write_frame(stream, &first).is_ok()
            };
            if subscribed {
                while let Ok(Some(frame)) = read_frame(&mut reader) {
                    told = false;
                    if !to_browser(&frame) {
                        std::process::exit(0);
                    }
                }
            }
            *app.lock().unwrap() = None;
        }
        if !told {
            if !say(&Response::Waiting) {
                std::process::exit(0);
            }
            told = true;
        }
        std::thread::sleep(AGAIN);
    }
}
