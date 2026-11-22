//! Unix-socket server that receives requests relayed by `siler-nmhost`.

use siler_core::{NewDownload, OnConflict, SourceHint, Status};
use siler_ipc::nonblocking::{read_frame, write_frame};
use siler_ipc::{BrowserItem, PROTOCOL, Request, Response, socket_path};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};
use tokio::net::{UnixListener, UnixStream};

use crate::AppState;
use crate::commands::meta_of;
use crate::i18n::tr;

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
                "another Siler instance owns the socket",
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
            Ok(Request::Ping {
                extension_version,
                subscribe: Some(subscriber),
                protocol,
            }) => {
                // The same checks and bookkeeping as any ping, then the connection stays.
                let pong = dispatch(
                    &app,
                    Request::Ping {
                        extension_version,
                        protocol,
                        subscribe: None,
                    },
                );
                let Response::Pong { version, protocol } = pong else {
                    write_frame(&mut stream, &serde_json::to_vec(&pong)?).await?;
                    continue;
                };
                let hello = Response::Subscribed { version, protocol };
                write_frame(&mut stream, &serde_json::to_vec(&hello)?).await?;
                return stay(&app, stream, subscriber.browser).await;
            }
            Ok(req) => dispatch(&app, req),
            Err(e) => Response::Error {
                message: format!("bad request: {e}"),
            },
        };
        write_frame(&mut stream, &serde_json::to_vec(&response)?).await?;
    }
    Ok(())
}

/// A subscribed browser: questions go out as they come up, and answers come back.
async fn stay(app: &AppHandle, stream: UnixStream, browser: String) -> io::Result<()> {
    let Some(browsers) = app.try_state::<AppState>().map(|s| s.browsers.clone()) else {
        return Ok(());
    };
    let (subscription, mut questions) = browsers.subscribe(browser);
    let (mut from, mut to) = stream.into_split();
    let answers = async {
        while let Some(frame) = read_frame(&mut from).await? {
            match serde_json::from_slice::<Request>(&frame) {
                Ok(Request::LinkFound {
                    request,
                    item,
                    error,
                }) => {
                    let found = match item.filter(|i| !i.url.trim().is_empty()) {
                        Some(item) => Ok(*item),
                        None => Err(error.unwrap_or_else(|| "no link was found".into())),
                    };
                    subscription.answer(request, found);
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("unreadable message from the browser: {e}"),
            }
            if let Some(state) = app.try_state::<AppState>() {
                state
                    .last_browser_contact
                    .store(now_ms(), Ordering::Relaxed);
            }
        }
        Ok(())
    };
    let asking = async {
        while let Some(question) = questions.recv().await {
            write_frame(&mut to, &serde_json::to_vec(&question)?).await?;
        }
        Ok(())
    };
    tokio::select! {
        ended = answers => ended,
        ended = asking => ended,
    }
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
            message: tr("ipc-couldnt-start"),
        };
    };
    state
        .last_browser_contact
        .store(now_ms(), Ordering::Relaxed);
    match req {
        Request::Ping {
            extension_version,
            protocol,
            ..
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
        Request::Beat => Response::Beat,
        Request::LinkFound { .. } => Response::Error {
            message: "nothing was asked on this connection".into(),
        },
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
        .or_else(|| siler_core::filename::name_from_url(&item.url))
}

/// Where links waiting in the Add window came from (shown under its title).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    #[default]
    Browser,
    CommandLine,
    Clipboard,
    /// An imported list or a metalink.
    List,
    /// The site grabber.
    Grabber,
}

/// How downloads from the browser or the command line are added.
#[derive(Debug, Default, Clone)]
pub struct AddOptions {
    pub origin: Origin,
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
            message: tr("ipc-couldnt-start"),
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
        && let Some(id) = state
            .awaiting_refresh
            .take(&items[0], |id| state.engine.get(id))
    {
        let item = items.into_iter().next().expect("one item");
        let engine = state.engine.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let result = engine.refresh_link(id, crate::links::found_of(&item)).await;
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
            let hint = hint_for(item);
            if is_video(item, hint.as_ref()) {
                // A stream: its tracks are chosen by default, as a media download, with what
                // the extension found out about it (a manifest from the page, subtitles).
                let engine = state.engine.clone();
                let app = app.clone();
                let item = item.clone();
                let opts = opts.clone();
                let on_conflict = settings.unattended_conflict();
                tauri::async_runtime::spawn(async move {
                    let url = item.url.clone();
                    let nd = siler_core::NewMediaDownload {
                        url: url.clone(),
                        meta: meta_of(&item),
                        link: crate::links::source_of(&item),
                        hint,
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
                link: crate::links::source_of(item),
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

/// The extension's details of a hand-over's video, as the engine takes them.
fn hint_for(item: &BrowserItem) -> Option<SourceHint> {
    item.media.as_ref().and_then(crate::commands::hint_of)
}

/// Whether a hand-over is a video download: a stream's own link, or a page whose manifest the
/// extension found and sent as text (YouTube). Anything else is a file.
fn is_video(item: &BrowserItem, hint: Option<&SourceHint>) -> bool {
    crate::media::is_stream(&item.url) || hint.is_some_and(|h| h.manifest.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use siler_ipc::{InlineManifest, MediaHint};

    fn item(url: &str, media: Option<MediaHint>) -> BrowserItem {
        BrowserItem {
            url: url.into(),
            media,
            ..Default::default()
        }
    }

    #[test]
    fn a_page_whose_manifest_came_as_text_is_a_video() {
        // What the YouTube recogniser hands over: the watch page, with the manifest it wrote.
        let youtube = item(
            "https://www.youtube.com/watch?v=jNQXAC9IVRw",
            Some(MediaHint {
                site: Some("youtube".into()),
                title: Some("Me at the zoo".into()),
                manifest: Some(InlineManifest {
                    kind: "dash".into(),
                    body: "<MPD/>".into(),
                    base_url: "https://www.youtube.com/watch?v=jNQXAC9IVRw".into(),
                }),
                ..Default::default()
            }),
        );
        let hint = hint_for(&youtube);
        assert!(hint.as_ref().is_some_and(|h| h.manifest.is_some()));
        assert!(is_video(&youtube, hint.as_ref()));
    }

    #[test]
    fn a_stream_link_is_a_video_and_a_named_file_is_a_file() {
        let hls = item("https://cdn.example/v/master.m3u8?t=1", None);
        assert!(is_video(&hls, None));
        // A recognised site's plain file (TikTok's MP4): its details only name it.
        let file = item(
            "https://cdn.example/v/clip.mp4",
            Some(MediaHint {
                site: Some("tiktok".into()),
                title: Some("A clip".into()),
                ..Default::default()
            }),
        );
        let hint = hint_for(&file);
        assert!(!is_video(&file, hint.as_ref()));
    }
}
