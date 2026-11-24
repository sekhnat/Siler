//! Progress windows, as IDM has them: one per download, opened when the user starts it,
//! which can be put away in the tray. Those in the tray share one icon (`tray::stack`).
//! When the download finishes, its window closes and a "Download complete" window may open.

use siler_core::extras::OnDone;
use siler_core::{AfterDone, DownloadInfo, Event, Status};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::AppState;
use crate::i18n::{tr, tr_args};

const PROGRESS: &str = "progress-";
const COMPLETE: &str = "complete-";
pub const WIDTH: f64 = 560.0;
/// With the connections shown, and without.
pub const TALL: f64 = 570.0;
pub const SHORT: f64 = 324.0;

#[derive(Default)]
struct Dialogs {
    /// Open progress windows, and whether each is put away in the tray.
    open: BTreeMap<i64, bool>,
    /// Started by the user, so a window opens once they really start.
    wanted: BTreeSet<i64>,
    /// A progress window was shown for them (for the "Download complete" window).
    shown: BTreeSet<i64>,
}

static DIALOGS: Mutex<Dialogs> = Mutex::new(Dialogs {
    open: BTreeMap::new(),
    wanted: BTreeSet::new(),
    shown: BTreeSet::new(),
});

fn dialogs() -> std::sync::MutexGuard<'static, Dialogs> {
    DIALOGS.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn label(id: i64) -> String {
    format!("{PROGRESS}{id}")
}

fn id_of(label: &str, prefix: &str) -> Option<i64> {
    label.strip_prefix(prefix)?.parse().ok()
}

/// The ids of the progress windows in the tray, in the order they were opened.
pub fn in_tray() -> Vec<i64> {
    dialogs()
        .open
        .iter()
        .filter(|(_, tray)| **tray)
        .map(|(id, _)| *id)
        .collect()
}

/// The downloads whose progress windows are on screen, so need the progress events.
pub fn shown() -> Vec<i64> {
    dialogs()
        .open
        .iter()
        .filter(|(_, tray)| !**tray)
        .map(|(id, _)| *id)
        .collect()
}

/// The user started `id`: its window opens when it starts running.
pub fn want(app: &AppHandle, id: i64) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if !state.engine.settings().progress_dialog {
        return;
    }
    dialogs().wanted.insert(id);
    // It may have started before it was wanted; whoever takes it from `wanted` opens it.
    let running = state
        .engine
        .get(id)
        .is_some_and(|d| matches!(d.status, Status::Probing | Status::Downloading));
    if running && dialogs().wanted.remove(&id) {
        open(app, id, None);
    }
}

/// Opens the progress window of `id`, or brings it back.
///
/// Always on the main thread. Built from another thread, a window is made on the main thread
/// but only known by its label once the asking thread hears back: a second request in between
/// (a double click, the download starting) found no window and built another with the same
/// label, which then had no working connection to Siler and could not be closed.
pub fn open(app: &AppHandle, id: i64, token: Option<String>) {
    let on_main = app.clone();
    if let Err(e) = app.run_on_main_thread(move || open_here(&on_main, id, token)) {
        tracing::error!("could not open the progress window: {e}");
    }
}

fn open_here(app: &AppHandle, id: i64, token: Option<String>) {
    let label = label(id);
    if app.get_webview_window(&label).is_some() {
        restore(app, id, token);
        return;
    }
    let details = app
        .try_state::<AppState>()
        .is_none_or(|s| s.engine.settings().ui.progress_details);
    let built = WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::App(format!("index.html#progress/{id}").into()),
    )
    .title(tr("window-progress"))
    .inner_size(WIDTH, if details { TALL } else { SHORT })
    .min_inner_size(460.0, SHORT)
    .decorations(false)
    .disable_drag_drop_handler()
    .center()
    .focused(true)
    .build();
    match built {
        Ok(_) => {
            let mut d = dialogs();
            d.open.insert(id, false);
            d.shown.insert(id);
        }
        Err(e) => tracing::error!("could not open the progress window: {e}"),
    }
    crate::tray::stack::sync();
}

/// Shows a window again, in front with the activation token of the click that asked.
fn raise(app: &AppHandle, label: String, token: Option<String>) {
    let on_main = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(w) = on_main.get_webview_window(&label) else {
            return;
        };
        if let Some(token) = token
            && let Ok(g) = w.gtk_window()
        {
            use gtk::prelude::GtkWindowExt;
            g.set_startup_id(&token);
        }
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        // Progress was skipped while it was away.
        let _ = w.emit_to(w.label(), "progress-shown", ());
    });
}

/// Puts the window of `id` away in the tray.
pub fn to_tray(app: &AppHandle, id: i64) {
    if !crate::tray::available(app) {
        // Nothing could bring it back: minimise it instead.
        if let Some(w) = app.get_webview_window(&label(id)) {
            let _ = w.minimize();
        }
        return;
    }
    if let Some(w) = app.get_webview_window(&label(id)) {
        let _ = w.hide();
        dialogs().open.insert(id, true);
    }
    crate::tray::stack::sync();
}

pub fn restore(app: &AppHandle, id: i64, token: Option<String>) {
    if let Some(tray) = dialogs().open.get_mut(&id) {
        *tray = false;
    }
    raise(app, label(id), token);
    crate::tray::stack::sync();
}

pub fn restore_all(app: &AppHandle, token: Option<String>) {
    for id in in_tray() {
        restore(app, id, token.clone());
    }
}

/// Closes the progress window of `id`, if it is open (on the main thread, as it opens).
pub fn close(app: &AppHandle, id: i64) {
    let on_main = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(w) = on_main.get_webview_window(&label(id)) {
            let _ = w.destroy();
        }
    });
}

/// A window is gone.
pub fn destroyed(label: &str) {
    if let Some(id) = id_of(label, PROGRESS) {
        dialogs().open.remove(&id);
        crate::tray::stack::sync();
    }
}

/// On the main thread, for the reason [`open`] gives.
fn open_complete(app: &AppHandle, id: i64) {
    let on_main = app.clone();
    let _ = app.run_on_main_thread(move || open_complete_here(&on_main, id));
}

fn open_complete_here(app: &AppHandle, id: i64) {
    let label = format!("{COMPLETE}{id}");
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::App(format!("index.html#complete/{id}").into()),
    )
    .title(tr("window-download-complete"))
    .inner_size(500.0, 300.0)
    .min_inner_size(420.0, 280.0)
    .decorations(false)
    .disable_drag_drop_handler()
    .center()
    .focused(true)
    .build();
    if let Err(e) = built {
        tracing::error!("could not open the download complete window: {e}");
    }
}

/// Follows the engine for the windows: opens the wanted ones, and closes them when their
/// download finishes or goes away.
pub fn on_event(app: &AppHandle, ev: &Event) {
    match ev {
        Event::Upsert { download } => {
            let id = download.id;
            let start = {
                let mut d = dialogs();
                match download.status {
                    Status::Probing | Status::Downloading => d.wanted.remove(&id),
                    Status::Completed | Status::Failed => {
                        d.wanted.remove(&id);
                        false
                    }
                    _ => false,
                }
            };
            if start {
                open(app, id, None);
            }
        }
        Event::Removed { id } => {
            {
                let mut d = dialogs();
                d.wanted.remove(id);
                d.shown.remove(id);
            }
            close(app, *id);
            let (on_main, id) = (app.clone(), *id);
            let _ = app.run_on_main_thread(move || {
                if let Some(w) = on_main.get_webview_window(&format!("{COMPLETE}{id}")) {
                    let _ = w.destroy();
                }
            });
        }
        Event::Completed { id, filename, path } => completed(app, *id, filename, path),
        _ => {}
    }
}

fn completed(app: &AppHandle, id: i64, filename: &str, path: &std::path::Path) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let settings = state.engine.settings();
    let on_done: OnDone = state
        .engine
        .get(id)
        .and_then(|d| d.on_done)
        .unwrap_or_default();
    let had_window = {
        let mut d = dialogs();
        d.shown.remove(&id) | d.open.contains_key(&id)
    };
    close(app, id);
    if on_done
        .complete_dialog
        .unwrap_or(settings.complete_dialog && had_window)
    {
        open_complete(app, id);
    }
    if on_done.open_file
        && let Err(e) = crate::launch::open(path)
    {
        tracing::warn!("couldn't open the finished file: {e}");
    }
    if on_done.after != AfterDone::Nothing {
        crate::power::schedule(
            app,
            on_done.after,
            tr_args(
                "power-download-finished",
                vec![("name", filename.to_string().into())],
            ),
        );
    }
}

// ---------------------------------------------------------------- commands

type Res<T> = Result<T, String>;

fn engine(app: &AppHandle) -> Res<siler_core::Engine> {
    app.try_state::<AppState>()
        .map(|s| s.engine.clone())
        .ok_or_else(|| tr("command-siler-isnt-running"))
}

#[tauri::command(async)]
pub fn get_download(app: AppHandle, id: i64) -> Res<DownloadInfo> {
    engine(&app)?.get(id).ok_or_else(|| tr("error-not-found"))
}

#[tauri::command]
pub fn show_progress(app: AppHandle, id: i64) {
    open(&app, id, None);
}

#[tauri::command]
pub fn progress_to_tray(app: AppHandle, id: i64) {
    to_tray(&app, id);
}

#[tauri::command]
pub fn progress_restore(app: AppHandle, id: i64) {
    crate::tray::panel::close(&app);
    restore(&app, id, None);
}

#[tauri::command]
pub fn progress_restore_all(app: AppHandle) {
    crate::tray::panel::close(&app);
    restore_all(&app, None);
}

/// The downloads whose windows are in the tray.
#[tauri::command(async)]
pub fn progress_in_tray(app: AppHandle) -> Vec<DownloadInfo> {
    let Ok(engine) = engine(&app) else {
        return Vec::new();
    };
    in_tray()
        .into_iter()
        .filter_map(|id| engine.get(id))
        .collect()
}

#[tauri::command(async)]
pub fn set_on_done(app: AppHandle, id: i64, on_done: Option<OnDone>) -> Res<()> {
    engine(&app)?
        .set_on_done(id, on_done)
        .map_err(|e| e.to_string())
}

fn change_settings(app: &AppHandle, f: impl FnOnce(&mut siler_core::Settings)) -> Res<()> {
    let engine = engine(app)?;
    let mut s = engine.settings();
    f(&mut s);
    let saved = engine.set_settings(s).map_err(|e| e.to_string())?;
    let _ = app.emit("settings-changed", saved);
    Ok(())
}

/// Remembers the speed limit for the server of `url`; 0 forgets it.
#[tauri::command(async)]
pub fn remember_server_speed_limit(app: AppHandle, url: String, bytes_per_sec: u64) -> Res<()> {
    change_settings(&app, |s| s.remember_speed_limit(&url, bytes_per_sec))
}

/// The speed limit remembered for the server of `url`; 0 = none.
#[tauri::command(async)]
pub fn server_speed_limit(app: AppHandle, url: String) -> Res<u64> {
    let s = engine(&app)?.settings();
    Ok(siler_core::server_speed_limit(
        &s.server_limits,
        &siler_core::server_host(&url),
    ))
}

/// Shows the connections in progress windows, or hides them.
#[tauri::command(async)]
pub fn set_progress_details(app: AppHandle, shown: bool) -> Res<()> {
    change_settings(&app, |s| s.ui.progress_details = shown)
}

/// "Don't show this again" in the "Download complete" window.
#[tauri::command]
pub fn set_complete_dialog(app: AppHandle, shown: bool) -> Res<()> {
    change_settings(&app, |s| s.complete_dialog = shown)
}

/// Opens a finished file with an application the user picks from the desktop's chooser.
#[tauri::command]
pub async fn open_with(app: AppHandle, id: i64) -> Res<()> {
    let path = crate::commands::finished_path(&app, id)?;
    match crate::launch::open_with_chooser(&path).await {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::info!("no application chooser ({e}); opening with the default one");
            crate::commands::off_thread(move || {
                crate::launch::open(&path).map_err(|e| e.to_string())
            })
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_name_their_download() {
        assert_eq!(id_of(&label(42), PROGRESS), Some(42));
        assert_eq!(id_of("complete-7", COMPLETE), Some(7));
        assert_eq!(id_of("progress-x", PROGRESS), None);
        assert_eq!(id_of("main", PROGRESS), None);
    }
}
