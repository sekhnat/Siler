mod commands;
mod desktop;
mod integration;
mod ipc_server;
mod logging;
mod tray;

use kstar_core::{Engine, Event};
use kstar_ipc::BrowserItem;
use serde::Serialize;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_notification::{Action, ActionType, NotificationExt};
use tokio::sync::broadcast::error::RecvError;

/// A download handed over by the browser that is waiting for the user to confirm it.
pub struct Pending {
    pub id: u64,
    pub item: BrowserItem,
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
}

impl AppState {
    pub fn push_pending(&self, items: Vec<BrowserItem>) {
        let mut p = self.pending.lock().unwrap();
        for item in items {
            let id = self.next_pending.fetch_add(1, Ordering::Relaxed);
            p.push(Pending { id, item });
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
        .map(|s| s.engine.list().iter().filter(|d| d.status.is_running()).count())
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
        let failed = n.extra().get("failed").and_then(|v| v.as_bool()) == Some(true);
        let app = handle.clone();
        let action = performed.action_id().to_string();
        // The handler runs on the notification thread; do the work elsewhere.
        tauri::async_runtime::spawn(async move {
            let result = match (failed, action.as_str()) {
                (true, _) => {
                    show_main(&app);
                    let _ = app.emit("select-download", id);
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

fn forward_engine_events(app: AppHandle, engine: Engine) {
    let mut rx = engine.subscribe();
    tauri::async_runtime::spawn(async move {
        let mut last_tray = Instant::now() - Duration::from_secs(5);
        let mut last_tray_text = String::new();
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if engine.settings().notify_on_complete {
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
                    if let Event::Progress { items, total_speed } = &ev {
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

/// `kstar [--background] [--silent] [URL…]`
struct Args {
    background: bool,
    silent: bool,
    urls: Vec<String>,
}

fn parse_args(args: &[String]) -> Args {
    let mut a = Args {
        background: false,
        silent: false,
        urls: Vec::new(),
    };
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--background" => a.background = true,
            "--silent" => a.silent = true,
            s if s.starts_with("http://") || s.starts_with("https://") => a.urls.push(s.into()),
            s => tracing::debug!("ignoring argument {s:?}"),
        }
    }
    a
}

fn add_from_args(app: &AppHandle, args: &Args) {
    if args.urls.is_empty() {
        return;
    }
    let items = args
        .urls
        .iter()
        .map(|url| BrowserItem {
            url: url.clone(),
            ..Default::default()
        })
        .collect();
    let response = ipc_server::handle_add(app, items, args.silent);
    if let kstar_ipc::Response::Error { message } = response {
        tracing::warn!("could not add the downloads from the command line: {message}");
        show_main(app);
    }
}

fn open_engine(app: &tauri::App) -> Result<Engine, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::block_on(async { Engine::open(&data_dir) })
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

    logging::init();
    let args = parse_args(&argv);

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            let args = parse_args(&argv);
            if !args.urls.is_empty() {
                add_from_args(app, &args);
            } else if !args.background {
                show_main(app);
            }
        }))
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
                    app.manage(AppState {
                        engine: engine.clone(),
                        pending: Mutex::new(Vec::new()),
                        next_pending: AtomicU64::new(1),
                        last_browser_contact: AtomicI64::new(0),
                        extension: Mutex::new(None),
                        awaiting_refresh: Mutex::new(None),
                        ui_visible: AtomicBool::new(false),
                        tray_available: AtomicBool::new(false),
                    });
                    desktop::ensure();
                    setup_notifications(app.handle());
                    forward_engine_events(app.handle().clone(), engine);
                    ipc_server::start(app.handle().clone());
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
                    add_from_args(app.handle(), &args);
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
            commands::name_status,
            commands::add_download,
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

    #[test]
    fn command_line() {
        let argv: Vec<String> = ["kstar", "--silent", "https://x/a.iso", "file:///etc", "--background"]
            .map(String::from)
            .to_vec();
        let a = parse_args(&argv);
        assert!(a.silent && a.background);
        assert_eq!(a.urls, ["https://x/a.iso"]);
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
        };
        assert_eq!(
            tray_status(&[item(50, Some(100)), item(5, None)], 5_100_000),
            "2 downloading · 5.1 MB/s · 50%"
        );
    }
}
