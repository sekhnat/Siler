mod clipboard;
mod commands;
mod desktop;
mod inhibit;
mod integration;
mod ipc_server;
mod logging;
mod logins;
mod media;
mod power;
mod sound;
mod tray;

use kstar_core::{Engine, Event, MAIN_QUEUE};
use kstar_ipc::BrowserItem;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_notification::{Action, ActionType, NotificationExt};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch;

use ipc_server::{AddOptions, Origin};

/// A download handed over by the browser that is waiting for the user to confirm it.
pub struct Pending {
    pub id: u64,
    pub item: BrowserItem,
    /// Folder given on the command line.
    pub dir: Option<PathBuf>,
    /// `--queue`: suggest "Download later".
    pub start_paused: bool,
    /// `--queue=NAME`: the queue to suggest.
    pub queue_id: Option<i64>,
    /// `--filename`: keep the given name.
    pub keep_name: bool,
    pub origin: Origin,
}

#[derive(Serialize, Clone)]
pub struct PendingView {
    pub id: u64,
    pub url: String,
    pub filename: Option<String>,
    pub referrer: Option<String>,
    pub page_url: Option<String>,
    pub size: Option<u64>,
    pub mime: Option<String>,
    pub method: Option<String>,
    pub selected: bool,
    pub dir: Option<PathBuf>,
    pub start_paused: bool,
    pub queue_id: Option<i64>,
    pub keep_name: bool,
    pub origin: Origin,
    /// A paused or failed download this one looks like, which its link could continue.
    pub resumable: Option<kstar_core::DownloadInfo>,
}

/// Set instead of [`AppState`] when K-Star couldn't start; the window shows it.
pub struct StartupError(pub String);

pub struct AppState {
    pub engine: Engine,
    pub pending: Mutex<Vec<Pending>>,
    pub next_pending: AtomicU64,
    /// Unix ms of the last message from the browser extension.
    pub last_browser_contact: AtomicI64,
    /// Version and protocol the extension reported in its last ping.
    pub extension: Mutex<Option<(String, u32)>>,
    /// "Refresh link" waiting for the next browser download: (download id, until).
    pub awaiting_refresh: Mutex<Option<(i64, Instant)>>,
    /// The main window is on screen (not hidden in the tray); progress events are
    /// skipped otherwise.
    pub ui_visible: AtomicBool,
    pub tray_available: AtomicBool,
    pub power: power::PowerState,
}

impl AppState {
    pub fn push_pending(&self, items: Vec<BrowserItem>, opts: &AddOptions) {
        let mut p = self.pending.lock().unwrap();
        for item in items {
            let id = self.next_pending.fetch_add(1, Ordering::Relaxed);
            p.push(Pending {
                id,
                dir: opts.dir.clone(),
                start_paused: opts.later,
                queue_id: opts.queue_id,
                keep_name: opts.keep_name && item.filename.is_some(),
                origin: opts.origin,
                item,
            });
        }
    }
}

pub fn set_ui_visible(app: &AppHandle, visible: bool) {
    if let Some(state) = app.try_state::<AppState>() {
        let was = state.ui_visible.swap(visible, Ordering::Relaxed);
        if visible && !was {
            // Progress was skipped while hidden.
            let _ = app.emit("engine-resync", ());
        }
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    set_ui_visible(app, true);
}

/// Opens the compact "Download file" window that confirms browser-captured downloads.
pub fn open_add_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("add") {
        let _ = w.emit("pending-changed", ());
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, "add", WebviewUrl::App("index.html#add".into()))
        .title("New download — K-Star")
        .inner_size(640.0, 520.0)
        .min_inner_size(540.0, 440.0)
        .always_on_top(true)
        .disable_drag_drop_handler()
        .center()
        .focused(true)
        .build();
    if let Err(e) = built {
        tracing::error!("could not open the add window: {e}");
        show_main(app);
    }
}

fn running_count(app: &AppHandle) -> usize {
    app.try_state::<AppState>()
        .map(|s| {
            s.engine
                .list()
                .iter()
                .filter(|d| d.status.is_running())
                .count()
        })
        .unwrap_or(0)
}

/// Quit from the tray, Ctrl+Q or the close button: asks first while downloads are running.
pub fn request_quit(app: &AppHandle) {
    let confirm = app
        .try_state::<AppState>()
        .is_some_and(|s| s.engine.settings().confirm_quit);
    let running = running_count(app);
    if confirm && running > 0 {
        show_main(app);
        let _ = app.emit("confirm-quit", running);
    } else {
        app.exit(0);
    }
}

const COMPLETE_ACTIONS: &str = "download-complete";

fn setup_notifications(app: &AppHandle) {
    let n = app.notification();
    let _ = n.register_action_types(vec![
        ActionType::builder(COMPLETE_ACTIONS)
            .actions(vec![
                Action::builder("open", "Open").build(),
                Action::builder("folder", "Show in folder").build(),
            ])
            .build(),
    ]);
    let handle = app.clone();
    let _ = n.on_action(move |performed| {
        let Some(n) = performed.notification() else {
            return;
        };
        let Some(id) = n.extra().get("download_id").and_then(|v| v.as_i64()) else {
            return;
        };
        let failed = n.extra().get("failed").and_then(|v| v.as_bool()) == Some(true)
            || n.extra().get("login").and_then(|v| v.as_bool()) == Some(true);
        let login = n.extra().get("login").and_then(|v| v.as_bool()) == Some(true);
        let app = handle.clone();
        let action = performed.action_id().to_string();
        // The handler runs on the notification thread; do the work elsewhere.
        tauri::async_runtime::spawn(async move {
            let result = match (failed, action.as_str()) {
                (true, _) => {
                    show_main(&app);
                    let _ = app.emit("select-download", id);
                    if login {
                        let _ = app.emit("ask-login", id);
                    }
                    Ok(())
                }
                (false, "open") => commands::open_path(&app, id),
                (false, _) => commands::reveal(&app, id),
            };
            if let Err(e) = result {
                tracing::warn!("notification action failed: {e}");
                show_main(&app);
                let _ = app.emit("select-download", id);
            }
        });
    });
}

fn notify(app: &AppHandle, title: &str, body: &str, id: i64, failed: bool) {
    let mut b = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .extra("download_id", id)
        .extra("failed", failed);
    if !failed {
        b = b.action_type_id(COMPLETE_ACTIONS);
    }
    let _ = b.show();
}

/// A server wants a login: use the saved one, or ask (now if the window is on screen,
/// else through a notification).
async fn login_required(
    app: AppHandle,
    engine: Engine,
    id: i64,
    filename: Option<String>,
    request: kstar_core::LoginRequest,
) {
    if !request.rejected
        && let Some(login) = logins::find(&request.host).await
    {
        tracing::info!("using the saved login for {}", request.host);
        if let Err(e) = engine.set_login(&request.host, login) {
            tracing::warn!("couldn't resume after logging in: {e}");
        }
        return;
    }
    let visible = app
        .try_state::<AppState>()
        .is_some_and(|s| s.ui_visible.load(Ordering::Relaxed));
    if visible {
        let _ = app.emit("ask-login", id);
    } else {
        let name = filename.as_deref().unwrap_or("A download");
        let _ = app
            .notification()
            .builder()
            .title("Login needed")
            .body(format!(
                "{name}\n{} asks for a username and password",
                request.host
            ))
            .extra("download_id", id)
            .extra("login", true)
            .show();
    }
}

fn tray_status(items: &[kstar_core::ProgressItem], total_speed: u64) -> String {
    if items.is_empty() {
        return "No active downloads".into();
    }
    let (done, total) = items
        .iter()
        .filter_map(|i| i.total_size.map(|t| (i.downloaded, t)))
        .fold((0u64, 0u64), |(d, t), (a, b)| (d + a, t + b));
    let mut s = format!(
        "{} downloading · {}/s",
        items.len(),
        human_bytes(total_speed)
    );
    if let Some(pct) = (done * 100).checked_div(total) {
        s.push_str(&format!(" · {pct}%"));
    }
    s
}

fn human_bytes(b: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", units[u])
    }
}

fn forward_engine_events(app: AppHandle, engine: Engine, activity: watch::Sender<usize>) {
    let mut rx = engine.subscribe();
    tauri::async_runtime::spawn(async move {
        let mut last_tray = Instant::now() - Duration::from_secs(5);
        let mut last_tray_text = String::new();
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let settings = engine.settings();
                    match &ev {
                        Event::Completed { .. } => {
                            sound::event(&settings.sounds, sound::Sound::Complete);
                            power::check_all_done(&app);
                        }
                        Event::Failed { .. } => {
                            sound::event(&settings.sounds, sound::Sound::Failed);
                            power::check_all_done(&app);
                        }
                        Event::QueueDone {
                            queue_id,
                            name,
                            after_done,
                            completed,
                            failed,
                        } => {
                            tracing::info!(
                                "queue {name} done: {completed} finished, {failed} failed"
                            );
                            // The main queue "finishes" whenever the list empties; only a
                            // chosen action makes that worth a sound.
                            if *queue_id != MAIN_QUEUE
                                || *after_done != kstar_core::AfterDone::Nothing
                            {
                                sound::event(&settings.sounds, sound::Sound::QueueDone);
                            }
                            power::schedule(
                                &app,
                                *after_done,
                                format!("The queue “{name}” has finished"),
                            );
                            power::check_all_done(&app);
                        }
                        _ => {}
                    }
                    if settings.notify_on_complete {
                        match &ev {
                            Event::Completed { id, filename, .. } => {
                                notify(&app, "Download complete", filename, *id, false)
                            }
                            Event::Failed {
                                id,
                                filename,
                                error,
                            } => {
                                let name = filename.as_deref().unwrap_or("Download");
                                notify(
                                    &app,
                                    "Download failed",
                                    &format!("{name}\n{error}"),
                                    *id,
                                    true,
                                )
                            }
                            _ => {}
                        }
                    }
                    if let Event::LoginRequired {
                        id,
                        filename,
                        request,
                    } = &ev
                    {
                        tauri::async_runtime::spawn(login_required(
                            app.clone(),
                            engine.clone(),
                            *id,
                            filename.clone(),
                            request.clone(),
                        ));
                        continue;
                    }
                    if let Event::Progress { items, total_speed } = &ev {
                        let n = items.len();
                        activity.send_if_modified(|v| std::mem::replace(v, n) != n);
                        let text = tray_status(items, *total_speed);
                        if text != last_tray_text
                            && (last_tray.elapsed() >= Duration::from_secs(1) || items.is_empty())
                        {
                            last_tray = Instant::now();
                            last_tray_text = text.clone();
                            tray::set_status(text).await;
                        }
                        let visible = app
                            .try_state::<AppState>()
                            .is_some_and(|s| s.ui_visible.load(Ordering::Relaxed));
                        if !visible {
                            continue;
                        }
                    }
                    let _ = app.emit("engine", &ev);
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = app.emit("engine-resync", ());
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// `kill`/Ctrl+C should checkpoint downloads just like quitting from the tray.
fn exit_on_signals(app: AppHandle) {
    use tokio::signal::unix::{SignalKind, signal};
    tauri::async_runtime::spawn(async move {
        let (Ok(mut term), Ok(mut int)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) else {
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        app.exit(0);
    });
}

const USAGE: &str = "\
Usage: kstar [OPTIONS] [URL…]

  --background       start without showing the window (tray only)
  --silent           add the URLs without asking
  --queue[=NAME]     add the URLs to a queue (the main one) without starting them
  --start-queue[=NAME]
                     start the downloads waiting in a queue (the main one)
  --dir PATH         save the URLs in this folder
  --filename NAME    save the URL under this name (one URL only)
  -h, --help         show this help
  -V, --version      show the version

When K-Star is already running, the options go to it.
";

/// The command line, from the first instance or forwarded from a second one.
#[derive(Debug, Default, PartialEq)]
struct Args {
    background: bool,
    silent: bool,
    queue: bool,
    queue_name: Option<String>,
    start_queue: bool,
    start_queue_name: Option<String>,
    dir: Option<PathBuf>,
    filename: Option<String>,
    urls: Vec<String>,
    help: bool,
    version: bool,
}

/// Parses `argv` (program name first); a relative `--dir` is taken from `cwd`.
fn parse_args(argv: &[String], cwd: &Path) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = argv.iter().skip(1);
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "--background" => a.background = true,
            "--silent" => a.silent = true,
            "--queue" => {
                a.queue = true;
                a.queue_name = inline.clone().filter(|v| !v.trim().is_empty());
            }
            "--start-queue" => {
                a.start_queue = true;
                a.start_queue_name = inline.clone().filter(|v| !v.trim().is_empty());
            }
            "--dir" => a.dir = Some(cwd.join(value("--dir")?)),
            "--filename" => a.filename = Some(value("--filename")?),
            "-h" | "--help" => a.help = true,
            "-V" | "--version" => a.version = true,
            s if s.starts_with("http://") || s.starts_with("https://") => a.urls.push(s.into()),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s => tracing::debug!("ignoring argument {s:?}"),
        }
    }
    if a.filename.is_some() && a.urls.len() != 1 {
        return Err("--filename needs exactly one URL".into());
    }
    Ok(a)
}

/// Acts on a command line forwarded from a second `kstar` process.
fn forwarded_args(app: &AppHandle, argv: &[String], cwd: &str) {
    let args = match parse_args(argv, Path::new(cwd)) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("ignoring the command line {argv:?}: {e}");
            return;
        }
    };
    if let Err(e) = add_from_args(app, &args) {
        tracing::warn!("{e}");
        let _ = app.notification().builder().title("K-Star").body(e).show();
        return;
    }
    if args.urls.is_empty() && !args.background && !args.start_queue {
        show_main(app);
    }
}

/// The queue called `name` (any case), or the main queue without a name.
fn queue_named(engine: &Engine, name: Option<&str>) -> Result<i64, String> {
    let Some(name) = name else {
        return Ok(MAIN_QUEUE);
    };
    engine
        .queues()
        .iter()
        .find(|q| q.name.trim().eq_ignore_ascii_case(name.trim()))
        .map(|q| q.id)
        .ok_or_else(|| format!("there's no queue called “{name}”"))
}

fn add_from_args(app: &AppHandle, args: &Args) -> Result<(), String> {
    let Some(state) = app.try_state::<AppState>() else {
        return Ok(());
    };
    let queue_id = queue_named(&state.engine, args.queue_name.as_deref())?;
    let start_queue = queue_named(&state.engine, args.start_queue_name.as_deref())?;
    if !args.urls.is_empty() {
        let items = args
            .urls
            .iter()
            .map(|url| BrowserItem {
                url: url.clone(),
                filename: args.filename.clone(),
                ..Default::default()
            })
            .collect();
        let opts = AddOptions {
            origin: Origin::CommandLine,
            silent: args.silent,
            dir: args.dir.clone(),
            later: args.queue,
            queue_id: args.queue_name.is_some().then_some(queue_id),
            keep_name: args.filename.is_some(),
        };
        let response = ipc_server::handle_add(app, items, &opts);
        if let kstar_ipc::Response::Error { message } = response {
            tracing::warn!("could not add the downloads from the command line: {message}");
            show_main(app);
        }
    }
    if args.start_queue
        && let Err(e) = state.engine.start_queue(start_queue)
    {
        tracing::warn!("could not start the queue: {e}");
    }
    Ok(())
}

fn open_engine(app: &tauri::App) -> Result<Engine, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    // Known before the engine resumes anything, so a media download picked up at start can
    // be rebuilt.
    let ffmpeg = media::find_ffmpeg(app.handle());
    tauri::async_runtime::block_on(async {
        // The manual proxy's password is the only secret needed before anything runs.
        let proxy_password = match Engine::peek_settings(&data_dir) {
            Ok(s) if s.proxy.needs_password() => logins::proxy_password().await,
            _ => None,
        };
        Engine::open_with(&data_dir, proxy_password, ffmpeg)
    })
    .map_err(|e| {
        format!(
            "K-Star couldn't open its database in {}:\n{e}",
            data_dir.display()
        )
    })
    .inspect(|_| tracing::info!("data in {}", data_dir.display()))
    .inspect_err(|e| tracing::error!("{e}"))
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.iter().any(|a| a == "--install-integration") {
        // Used by scripts/install-dev.sh.
        let browsers = integration::install();
        let desktop = desktop::install();
        for (what, r) in [
            ("browser integration", browsers.map(|_| ())),
            ("desktop entry", desktop),
        ] {
            match r {
                Ok(()) => println!("installed the {what}"),
                Err(e) => {
                    eprintln!("could not install the {what}: {e}");
                    std::process::exit(1);
                }
            }
        }
        return;
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let args = match parse_args(&argv, &cwd) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("kstar: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if args.help {
        print!("{USAGE}");
        return;
    }
    if args.version {
        println!("K-Star {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    logging::init();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            forwarded_args(app, &argv, &cwd)
        }))
        .plugin(tauri_plugin_drag::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_window_state::Builder::new()
                .with_denylist(&["add"])
                .skip_initial_state("add")
                .build(),
        )
        .setup(move |app| {
            match open_engine(app) {
                Ok(engine) => {
                    let ffmpeg = engine.ffmpeg_status();
                    if ffmpeg.ok {
                        tracing::info!(
                            "media: using {}",
                            ffmpeg.path.as_deref().unwrap_or("FFmpeg")
                        );
                    } else {
                        tracing::warn!("media: FFmpeg is not usable: {:?}", ffmpeg.missing);
                    }
                    app.manage(AppState {
                        engine: engine.clone(),
                        pending: Mutex::new(Vec::new()),
                        next_pending: AtomicU64::new(1),
                        last_browser_contact: AtomicI64::new(0),
                        extension: Mutex::new(None),
                        awaiting_refresh: Mutex::new(None),
                        ui_visible: AtomicBool::new(false),
                        tray_available: AtomicBool::new(false),
                        power: power::PowerState::default(),
                    });
                    desktop::ensure();
                    setup_notifications(app.handle());
                    let activity = inhibit::spawn(engine.clone());
                    forward_engine_events(app.handle().clone(), engine, activity);
                    ipc_server::start(app.handle().clone());
                    clipboard::spawn(app.handle().clone());
                    exit_on_signals(app.handle().clone());
                    let handle = app.handle().clone();
                    let has_tray = tauri::async_runtime::block_on(tray::spawn(handle.clone()));
                    app.state::<AppState>()
                        .tray_available
                        .store(has_tray, Ordering::Relaxed);
                    // Without a tray, a hidden window couldn't be brought back.
                    if !args.background || !has_tray {
                        show_main(app.handle());
                    }
                    if let Err(e) = add_from_args(app.handle(), &args) {
                        eprintln!("kstar: {e}");
                        std::process::exit(2);
                    }
                }
                Err(message) => {
                    app.manage(StartupError(message));
                    show_main(app.handle());
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    let app = window.app_handle();
                    let Some(state) = app.try_state::<AppState>() else {
                        return; // start-up error: just close
                    };
                    api.prevent_close();
                    if state.engine.settings().close_to_tray
                        && state.tray_available.load(Ordering::Relaxed)
                    {
                        let _ = window.hide();
                        set_ui_visible(app, false);
                    } else {
                        request_quit(app);
                    }
                }
                WindowEvent::Focused(true) => set_ui_visible(window.app_handle(), true),
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::startup_error,
            commands::list_downloads,
            commands::probe_url,
            commands::probe_media,
            commands::name_status,
            commands::add_download,
            commands::add_media,
            commands::media_diagnostics,
            commands::pending_adds,
            commands::dismiss_pending,
            commands::pause,
            commands::resume,
            commands::remove,
            commands::remove_many,
            commands::pause_all,
            commands::resume_all,
            commands::start_now,
            commands::start_queue,
            commands::stop_queue,
            commands::move_in_queue,
            commands::restart,
            commands::redownload,
            commands::refresh_url,
            commands::await_refresh,
            commands::cancel_await_refresh,
            commands::set_connections,
            commands::edit_download,
            commands::set_speed_limit,
            commands::default_categories,
            commands::get_settings,
            commands::set_settings,
            commands::open_file,
            commands::show_in_folder,
            commands::open_url,
            commands::open_log_folder,
            commands::integration_status,
            commands::install_integration,
            commands::install_extension,
            commands::show_main_window,
            commands::request_quit,
            commands::quit_app,
            commands::submit_login,
            commands::saved_logins,
            commands::forget_saved_login,
            commands::keyring_available,
            commands::proxy_status,
            commands::set_proxy_password,
            commands::proxy_password_saved,
            commands::list_queues,
            commands::create_queue,
            commands::update_queue,
            commands::delete_queue,
            commands::move_to_queue,
            commands::set_sync,
            commands::check_for_update,
            commands::power_caps,
            commands::power_pending,
            commands::power_cancel,
            commands::power_now,
            commands::get_when_done,
            commands::set_when_done,
            commands::test_sound,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build the K-Star app");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            if let Some(state) = app.try_state::<AppState>() {
                tauri::async_runtime::block_on(state.engine.shutdown());
            }
            ipc_server::cleanup();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        let argv: Vec<String> = std::iter::once("kstar")
            .chain(args.iter().copied())
            .map(String::from)
            .collect();
        parse_args(&argv, Path::new("/home/me"))
    }

    #[test]
    fn command_line() {
        let a = parse(&["--silent", "https://x/a.iso", "file:///etc", "--background"]).unwrap();
        assert!(a.silent && a.background && !a.queue);
        assert_eq!(a.urls, ["https://x/a.iso"]);

        let a = parse(&[
            "--queue",
            "--dir",
            "Films",
            "--filename=b.mkv",
            "http://x/b",
        ])
        .unwrap();
        assert!(a.queue);
        assert_eq!(a.dir, Some(PathBuf::from("/home/me/Films")));
        assert_eq!(a.filename.as_deref(), Some("b.mkv"));

        let a = parse(&["--dir=/mnt/media", "--start-queue"]).unwrap();
        assert_eq!(a.dir, Some(PathBuf::from("/mnt/media")));
        assert!(a.start_queue && a.urls.is_empty() && a.start_queue_name.is_none());

        // Only the `=` form names a queue, so `--queue URL` still means the main queue.
        let a = parse(&["--queue", "https://x/c", "--start-queue=Night"]).unwrap();
        assert!(a.queue && a.queue_name.is_none());
        assert_eq!(a.urls, ["https://x/c"]);
        assert_eq!(a.start_queue_name.as_deref(), Some("Night"));
        let a = parse(&["--queue=Night owls", "https://x/d"]).unwrap();
        assert_eq!(a.queue_name.as_deref(), Some("Night owls"));

        assert!(parse(&["-h"]).unwrap().help);
        assert!(parse(&["--version"]).unwrap().version);
        assert_eq!(parse(&["--dir"]).unwrap_err(), "--dir needs a value");
        assert_eq!(
            parse(&["--frobnicate"]).unwrap_err(),
            "unknown option --frobnicate"
        );
        assert_eq!(
            parse(&["--filename", "x", "http://a/1", "http://a/2"]).unwrap_err(),
            "--filename needs exactly one URL"
        );
    }

    #[test]
    fn tray_text() {
        assert_eq!(tray_status(&[], 0), "No active downloads");
        let item = |d, t| kstar_core::ProgressItem {
            id: 1,
            downloaded: d,
            total_size: t,
            speed: 0,
            active_connections: 1,
            segments: vec![],
            media: None,
        };
        assert_eq!(
            tray_status(&[item(50, Some(100)), item(5, None)], 5_100_000),
            "2 downloading · 5.1 MB/s · 50%"
        );
    }
}
