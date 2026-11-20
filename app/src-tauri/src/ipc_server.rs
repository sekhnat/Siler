//! Unix-socket server that receives requests relayed by `kstar-nmhost`.

use kstar_core::{NewDownload, OnConflict, Status};
use kstar_ipc::nonblocking::{read_frame, write_frame};
use kstar_ipc::{BrowserItem, PROTOCOL, Request, Response, socket_path};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};
use tokio::net::{UnixListener, UnixStream};

use crate::AppState;
use crate::commands::meta_of;

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = serve(app).await {
            tracing::error!("browser integration socket unavailable: {e}");
        }
    });
}

pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}

async fn serve(app: AppHandle) -> io::Result<()> {
    let path = socket_path();
    let dir = path.parent().expect("socket has a parent dir");
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    if path.exists() {
        if UnixStream::connect(&path).await.is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "another K-Star instance owns the socket",
            ));
        }
        std::fs::remove_file(&path)?; // stale, left by a crash
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!("listening for the browser extension on {}", path.display());

    let my_uid = rustix::process::getuid().as_raw();
    loop {
        let (stream, _) = listener.accept().await?;
        if stream.peer_cred().map(|c| c.uid()).ok() != Some(my_uid) {
            tracing::warn!("rejected a socket connection from another user");
            continue;
        }
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = handle(app, stream).await {
                tracing::debug!("ipc connection ended: {e}");
            }
        });
    }
}

async fn handle(app: AppHandle, mut stream: UnixStream) -> io::Result<()> {
    while let Some(frame) = read_frame(&mut stream).await? {
        let response = match serde_json::from_slice::<Request>(&frame) {
            Ok(req) => dispatch(&app, req),
            Err(e) => Response::Error {
                message: format!("bad request: {e}"),
            },
        };
        write_frame(&mut stream, &serde_json::to_vec(&response)?).await?;
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn dispatch(app: &AppHandle, req: Request) -> Response {
    let Some(state) = app.try_state::<AppState>() else {
        return Response::Error {
            message: "K-Star couldn't start; open it to see why".into(),
        };
    };
    state
        .last_browser_contact
        .store(now_ms(), Ordering::Relaxed);
    match req {
        Request::Ping {
            extension_version,
            protocol,
        } => {
            if let Some(v) = extension_version {
                *state.extension.lock().unwrap() = Some((v, protocol.unwrap_or(0)));
            }
            Response::Pong {
                version: env!("CARGO_PKG_VERSION").to_string(),
                protocol: PROTOCOL,
            }
        }
        Request::Show => {
            crate::show_main(app);
            Response::Ok
        }
        Request::GetStatus => {
            let list = state.engine.list();
            Response::Status {
                active: list.iter().filter(|d| d.status.is_running()).count(),
                queued: list.iter().filter(|d| d.status == Status::Queued).count(),
                speed: list.iter().map(|d| d.speed).sum(),
            }
        }
        Request::AddDownloads { items, silent } => handle_add(
            app,
            items,
            &AddOptions {
                silent,
                ..Default::default()
            },
        ),
    }
}

/// The name a browser item will probably be saved under, for spotting existing files.
fn likely_name(item: &BrowserItem) -> Option<String> {
    item.filename
        .clone()
        .or_else(|| kstar_core::filename::name_from_url(&item.url))
}

/// How downloads from the browser or the command line are added.
#[derive(Debug, Default, Clone)]
pub struct AddOptions {
    /// Add without the dialog.
    pub silent: bool,
    /// Save into this folder instead of the category folder.
    pub dir: Option<PathBuf>,
    /// Add to the queue without starting ("Download later").
    pub later: bool,
    /// The queue to add to; the main queue when absent.
    pub queue_id: Option<i64>,
    /// The file names were chosen by the user (`--filename`).
    pub keep_name: bool,
}

/// Adds downloads from the browser, the command line or a second instance: shows the Add
/// window, or enqueues directly when `silent` or "start browser downloads automatically".
pub fn handle_add(app: &AppHandle, items: Vec<BrowserItem>, opts: &AddOptions) -> Response {
    let silent = opts.silent;
    let Some(state) = app.try_state::<AppState>() else {
        return Response::Error {
            message: "K-Star couldn't start; open it to see why".into(),
        };
    };
    let items: Vec<_> = items
        .into_iter()
        .filter(|i| !i.url.trim().is_empty())
        .collect();
    let count = items.len();
    if count == 0 {
        return Response::Error {
            message: "no URLs given".into(),
        };
    }

    // "Refresh link": the user opened the page to get a fresh link for a download.
    if count == 1
        && let Some(id) = take_refresh_target(&state, &items[0])
    {
        let item = items.into_iter().next().expect("one item");
        let engine = state.engine.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let result = engine
                .refresh_url(id, &item.url, Some(meta_of(&item)))
                .await;
            crate::show_main(&app);
            let _ = app.emit(
                "refresh-done",
                serde_json::json!({ "id": id, "error": result.err().map(|e| e.to_string()) }),
            );
        });
        return Response::Accepted { count: 1 };
    }

    let settings = state.engine.settings();
    let direct = silent || settings.auto_start_browser_downloads;
    tracing::info!(count, silent, "links received");
    // "Ask" needs the dialog when a file of that name is already there.
    let needs_asking = |item: &BrowserItem| {
        settings.on_conflict == OnConflict::Ask
            && !silent
            && likely_name(item).is_some_and(|name| {
                let dir = opts
                    .dir
                    .clone()
                    .unwrap_or_else(|| settings.category_dir(settings.category_for(&name)));
                state.engine.name_status(&dir, &name).0
            })
    };
    if direct && !items.iter().any(needs_asking) {
        let mut errors = Vec::new();
        for item in &items {
            if crate::media::is_stream(&item.url) {
                // A manifest is a stream: its tracks are chosen by default, as a media download.
                let engine = state.engine.clone();
                let app = app.clone();
                let item = item.clone();
                let opts = opts.clone();
                let on_conflict = settings.unattended_conflict();
                tauri::async_runtime::spawn(async move {
                    let url = item.url.clone();
                    let nd = kstar_core::NewMediaDownload {
                        url: url.clone(),
                        meta: meta_of(&item),
                        filename: item.filename.clone(),
                        dir: opts.dir.clone(),
                        start_paused: opts.later,
                        on_conflict: Some(on_conflict),
                        queue_id: opts.queue_id,
                        ..Default::default()
                    };
                    if let Err(e) = engine.add_media(nd).await {
                        // Show the Add window (with the same folder and queue): it explains
                        // why this stream can't be downloaded, or offers it as a file.
                        tracing::warn!("stream {url}: {e}");
                        if let Some(state) = app.try_state::<AppState>() {
                            state.push_pending(vec![item], &opts);
                        }
                        let _ = app.emit("pending-changed", ());
                        crate::open_add_window(&app);
                    }
                });
                continue;
            }
            let nd = NewDownload {
                url: item.url.clone(),
                filename: item.filename.clone(),
                meta: meta_of(item),
                on_conflict: Some(settings.unattended_conflict()),
                dir: opts.dir.clone(),
                start_paused: opts.later,
                queue_id: opts.queue_id,
                ..Default::default()
            };
            if let Err(e) = state.engine.add(nd) {
                errors.push(e.to_string());
            }
        }
        if errors.len() == count {
            return Response::Error {
                message: errors.join("; "),
            };
        }
        Response::Accepted {
            count: count - errors.len(),
        }
    } else {
        state.push_pending(items, opts);
        let _ = app.emit("pending-changed", ());
        crate::open_add_window(app);
        Response::Accepted { count }
    }
}

/// The download waiting for a fresh link, if `item` looks like it (same size when known).
fn take_refresh_target(state: &AppState, item: &BrowserItem) -> Option<i64> {
    let mut waiting = state.awaiting_refresh.lock().unwrap();
    let (id, until) = (*waiting)?;
    if Instant::now() > until {
        *waiting = None;
        return None;
    }
    let d = state.engine.get(id)?;
    let same = match (d.total_size, item.size) {
        (Some(a), Some(b)) => a == b,
        _ => true, // the engine checks again after probing
    };
    if same {
        *waiting = None;
        Some(id)
    } else {
        None
    }
}
