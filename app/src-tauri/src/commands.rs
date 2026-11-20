use kstar_core::proxy::ProxyRoute;
use kstar_core::{
    Category, DownloadInfo, EditDownload, Login, LoginRequest, NewDownload, OnConflict,
    ProbeResult, QueueMove, RequestMeta, Settings, Status,
};
use kstar_ipc::BrowserItem;
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::integration::{self, IntegrationStatus};
use crate::logins::{self, SavedLogin};
use crate::{AppState, PendingView, StartupError};

type Res<T> = Result<T, String>;

fn err(e: impl Display) -> String {
    e.to_string()
}

/// The request context to replay for a download handed over by the browser.
pub fn meta_of(item: &BrowserItem) -> RequestMeta {
    let body = item
        .body
        .as_deref()
        .and_then(kstar_core::decode_base64)
        .filter(|b| b.len() <= kstar_core::MAX_BODY);
    RequestMeta {
        referrer: item.referrer.clone().or_else(|| item.page_url.clone()),
        page_url: item.page_url.clone(),
        cookies: item.cookies.clone(),
        user_agent: item.user_agent.clone(),
        headers: item.headers.clone(),
        method: item.method.clone().filter(|_| body.is_some()),
        body,
    }
}

fn pending_meta(state: &AppState, pending_id: Option<u64>) -> Option<RequestMeta> {
    let id = pending_id?;
    let p = state.pending.lock().unwrap();
    p.iter().find(|p| p.id == id).map(|p| meta_of(&p.item))
}

fn forget_pending(state: &AppState, pending_id: Option<u64>) {
    if let Some(pid) = pending_id {
        state.pending.lock().unwrap().retain(|p| p.id != pid);
    }
}

#[tauri::command]
pub fn startup_error(app: AppHandle) -> Option<String> {
    app.try_state::<StartupError>().map(|e| e.0.clone())
}

#[tauri::command]
pub fn list_downloads(state: State<'_, AppState>) -> Vec<DownloadInfo> {
    state.engine.list()
}

/// A failed probe; `login` is set when the server wants a username and password.
#[derive(Serialize)]
pub struct ProbeError {
    message: String,
    login: Option<LoginRequest>,
}

#[tauri::command]
pub async fn probe_url(
    state: State<'_, AppState>,
    url: String,
    pending_id: Option<u64>,
) -> Result<ProbeResult, ProbeError> {
    let meta = pending_meta(&state, pending_id).unwrap_or_default();
    let mut result = state.engine.probe(&url, &meta).await;
    // A saved login is used without asking.
    if let Err(e) = &result
        && let Some(req) = e.login_request()
        && !req.rejected
        && let Some(login) = logins::find(&req.host).await
    {
        let _ = state.engine.set_login(&req.host, login);
        result = state.engine.probe(&url, &meta).await;
    }
    result.map_err(|e| ProbeError {
        message: e.to_string(),
        login: e.login_request(),
    })
}

/// Uses a login for a server (resuming its downloads), optionally saving it in the keyring.
#[tauri::command]
pub async fn submit_login(
    state: State<'_, AppState>,
    host: String,
    username: String,
    password: String,
    remember: bool,
) -> Res<()> {
    let login = Login { username, password };
    if remember {
        // Save first: resuming may need the login again before this returns.
        let saved = logins::save(&host, &login).await;
        state.engine.set_login(&host, login).map_err(err)?;
        saved
    } else {
        state.engine.set_login(&host, login).map_err(err)
    }
}

#[tauri::command]
pub async fn saved_logins() -> Vec<SavedLogin> {
    logins::list().await
}

#[tauri::command]
pub async fn forget_saved_login(state: State<'_, AppState>, host: String) -> Res<()> {
    state.engine.forget_login(&host);
    logins::forget(&host).await
}

#[tauri::command]
pub async fn keyring_available() -> bool {
    logins::available().await
}

#[derive(Serialize)]
pub struct ProxyStatus {
    summary: String,
    note: Option<String>,
}

/// Where connections go now, for the settings screen.
#[tauri::command]
pub fn proxy_status(state: State<'_, AppState>) -> ProxyStatus {
    let route: ProxyRoute = state.engine.proxy_route();
    ProxyStatus {
        summary: route.summary(),
        note: route.note,
    }
}

/// Saves the manual proxy's password in the keyring and uses it right away.
#[tauri::command]
pub async fn set_proxy_password(state: State<'_, AppState>, password: Option<String>) -> Res<()> {
    let password = password.filter(|p| !p.is_empty());
    state.engine.set_proxy_password(password.clone());
    logins::set_proxy_password(password.as_deref()).await
}

#[tauri::command]
pub async fn proxy_password_saved() -> bool {
    logins::proxy_password_saved().await
}

#[derive(Serialize)]
pub struct NameStatus {
    exists: bool,
    suggested: String,
}

/// Whether `dir/name` already exists, and a free name to use instead.
#[tauri::command]
pub fn name_status(state: State<'_, AppState>, dir: PathBuf, name: String) -> NameStatus {
    let (exists, suggested) = state.engine.name_status(&dir, &name);
    NameStatus { exists, suggested }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddArgs {
    url: String,
    filename: Option<String>,
    dir: Option<PathBuf>,
    category: Option<Category>,
    #[serde(default)]
    start_paused: bool,
    connections: Option<u8>,
    on_conflict: Option<OnConflict>,
    pending_id: Option<u64>,
}

#[tauri::command]
pub fn add_download(state: State<'_, AppState>, args: AddArgs) -> Res<i64> {
    let meta = pending_meta(&state, args.pending_id).unwrap_or_default();
    let id = state
        .engine
        .add(NewDownload {
            url: args.url,
            filename: args.filename.filter(|f| !f.trim().is_empty()),
            dir: args.dir,
            category: args.category,
            meta,
            start_paused: args.start_paused,
            connections: args.connections,
            on_conflict: args.on_conflict,
        })
        .map_err(err)?;
    forget_pending(&state, args.pending_id);
    Ok(id)
}

#[tauri::command]
pub fn pending_adds(state: State<'_, AppState>) -> Vec<PendingView> {
    let pending = state.pending.lock().unwrap();
    let single = pending.len() == 1;
    pending
        .iter()
        .map(|p| {
            let name = p
                .item
                .filename
                .clone()
                .or_else(|| kstar_core::filename::name_from_url(&p.item.url));
            PendingView {
                id: p.id,
                url: p.item.url.clone(),
                filename: p.item.filename.clone(),
                referrer: p.item.referrer.clone(),
                page_url: p.item.page_url.clone(),
                size: p.item.size,
                mime: p.item.mime.clone(),
                method: p.item.method.clone(),
                selected: p.item.selected.unwrap_or(true),
                dir: p.dir.clone(),
                start_paused: p.start_paused,
                keep_name: p.keep_name,
                resumable: if single {
                    name.and_then(|n| state.engine.find_resumable(&n, p.item.size))
                } else {
                    None
                },
            }
        })
        .collect()
}

#[tauri::command]
pub fn dismiss_pending(state: State<'_, AppState>, id: u64) {
    state.pending.lock().unwrap().retain(|p| p.id != id);
}

#[tauri::command]
pub fn pause(state: State<'_, AppState>, id: i64) -> Res<()> {
    state.engine.pause(id).map_err(err)
}

#[tauri::command]
pub fn resume(state: State<'_, AppState>, id: i64) -> Res<()> {
    state.engine.resume(id).map_err(err)
}

#[tauri::command]
pub fn remove(state: State<'_, AppState>, id: i64, delete_file: bool) -> Res<()> {
    state.engine.remove(id, delete_file).map_err(err)
}

#[tauri::command]
pub fn remove_many(state: State<'_, AppState>, ids: Vec<i64>, delete_files: bool) -> Res<usize> {
    state.engine.remove_many(&ids, delete_files).map_err(err)
}

#[tauri::command]
pub fn pause_all(state: State<'_, AppState>) -> Res<()> {
    state.engine.pause_all().map_err(err)
}

#[tauri::command]
pub fn resume_all(state: State<'_, AppState>) -> Res<()> {
    state.engine.resume_all().map_err(err)
}

#[tauri::command]
pub fn start_now(state: State<'_, AppState>, id: i64) -> Res<()> {
    state.engine.start_now(id).map_err(err)
}

#[tauri::command]
pub fn start_queue(state: State<'_, AppState>) -> Res<()> {
    state.engine.start_queue().map_err(err)
}

#[tauri::command]
pub fn stop_queue(state: State<'_, AppState>) -> Res<()> {
    state.engine.stop_queue().map_err(err)
}

#[tauri::command]
pub fn move_in_queue(state: State<'_, AppState>, ids: Vec<i64>, to: QueueMove) -> Res<()> {
    state.engine.move_in_queue(&ids, to).map_err(err)
}

#[tauri::command]
pub async fn restart(state: State<'_, AppState>, id: i64) -> Res<()> {
    state.engine.restart(id).await.map_err(err)
}

#[tauri::command]
pub async fn redownload(state: State<'_, AppState>, id: i64, replace: bool) -> Res<()> {
    state.engine.redownload(id, replace).await.map_err(err)
}

/// Gives a download a new link. With `pending_id`, the link (and its cookies) come from a
/// browser hand-over waiting in the Add window.
#[tauri::command]
pub async fn refresh_url(
    state: State<'_, AppState>,
    id: i64,
    url: String,
    pending_id: Option<u64>,
) -> Res<()> {
    let meta = pending_meta(&state, pending_id);
    state
        .engine
        .refresh_url(id, &url, meta)
        .await
        .map_err(err)?;
    forget_pending(&state, pending_id);
    Ok(())
}

/// How long "Refresh link" waits for the browser to hand over a new link.
const REFRESH_WAIT: Duration = Duration::from_secs(120);

/// Opens the page the download came from, so the user can click the link again; the next
/// matching hand-over from the browser updates this download. Returns whether a page was
/// known.
#[tauri::command]
pub fn await_refresh(app: AppHandle, state: State<'_, AppState>, id: i64) -> Res<bool> {
    let d = state.engine.get(id).ok_or("Download not found")?;
    *state.awaiting_refresh.lock().unwrap() = Some((id, Instant::now() + REFRESH_WAIT));
    let page = d.page_url.or(d.referrer);
    if let Some(page) = &page {
        app.opener().open_url(page, None::<&str>).map_err(err)?;
    }
    Ok(page.is_some())
}

#[tauri::command]
pub fn cancel_await_refresh(state: State<'_, AppState>) {
    *state.awaiting_refresh.lock().unwrap() = None;
}

#[tauri::command]
pub fn set_connections(state: State<'_, AppState>, id: i64, connections: u8) -> Res<()> {
    state.engine.set_connections(id, connections).map_err(err)
}

#[tauri::command]
pub async fn edit_download(
    state: State<'_, AppState>,
    id: i64,
    filename: Option<String>,
    dir: Option<PathBuf>,
) -> Res<()> {
    state
        .engine
        .edit(id, EditDownload { filename, dir })
        .await
        .map_err(err)
}

#[tauri::command]
pub fn set_speed_limit(state: State<'_, AppState>, id: i64, bytes_per_sec: u64) -> Res<()> {
    state.engine.set_speed_limit(id, bytes_per_sec).map_err(err)
}

#[tauri::command]
pub fn default_categories() -> Vec<kstar_core::CategoryRule> {
    kstar_core::default_categories()
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.engine.settings()
}

#[tauri::command]
pub fn set_settings(state: State<'_, AppState>, settings: Settings) -> Res<Settings> {
    state.engine.set_settings(settings).map_err(err)
}

fn download(app: &AppHandle, id: i64) -> Res<DownloadInfo> {
    app.try_state::<AppState>()
        .ok_or("K-Star isn't running")?
        .engine
        .get(id)
        .ok_or_else(|| "Download not found".to_string())
}

fn path_of(app: &AppHandle, id: i64) -> Res<PathBuf> {
    let state = app.try_state::<AppState>().ok_or("K-Star isn't running")?;
    state
        .engine
        .file_path(id)
        .ok_or_else(|| "This download has no file yet".to_string())
}

/// Opens a finished file with its default application.
pub fn open_path(app: &AppHandle, id: i64) -> Res<()> {
    let d = download(app, id)?;
    if d.status != Status::Completed {
        return Err("The download hasn't finished yet".into());
    }
    let path = path_of(app, id)?;
    if !path.exists() {
        return Err(kstar_core::Error::FileMissing.to_string());
    }
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(err)
}

/// Shows the file (or the partial file) selected in its folder.
pub fn reveal(app: &AppHandle, id: i64) -> Res<()> {
    let path = path_of(app, id)?;
    if path.exists() {
        return app.opener().reveal_item_in_dir(&path).map_err(err);
    }
    match path.parent().filter(|d| d.exists()) {
        Some(dir) => app
            .opener()
            .open_path(dir.to_string_lossy(), None::<&str>)
            .map_err(err),
        None => Err("The folder no longer exists".into()),
    }
}

#[tauri::command]
pub fn open_file(app: AppHandle, id: i64) -> Res<()> {
    open_path(&app, id)
}

#[tauri::command]
pub fn show_in_folder(app: AppHandle, id: i64) -> Res<()> {
    reveal(&app, id)
}

#[tauri::command]
pub fn open_url(app: AppHandle, url: String) -> Res<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("Only web links can be opened".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(err)
}

#[tauri::command]
pub fn open_log_folder(app: AppHandle) -> Res<()> {
    let dir = crate::logging::dir();
    std::fs::create_dir_all(&dir).map_err(err)?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(err)
}

fn status_of(state: &AppState) -> IntegrationStatus {
    integration::status(
        state.last_browser_contact.load(Ordering::Relaxed),
        state.extension.lock().unwrap().clone(),
        state.tray_available.load(Ordering::Relaxed),
    )
}

#[tauri::command]
pub fn integration_status(state: State<'_, AppState>) -> IntegrationStatus {
    status_of(&state)
}

#[tauri::command]
pub fn install_integration(state: State<'_, AppState>) -> Res<IntegrationStatus> {
    integration::install().map_err(err)?;
    crate::desktop::install().map_err(err)?;
    Ok(status_of(&state))
}

/// Opens the signed extension with Firefox, which offers to install it.
#[tauri::command]
pub fn install_extension() -> Res<()> {
    let xpi = integration::signed_xpi()
        .ok_or("No signed extension yet. Run scripts/sign-extension.sh first (see the README).")?;
    std::process::Command::new("firefox")
        .arg(&xpi)
        .spawn()
        .map(drop)
        .map_err(|e| format!("Couldn't start Firefox: {e}"))
}

#[tauri::command]
pub fn show_main_window(app: AppHandle) {
    crate::show_main(&app);
}

#[tauri::command]
pub fn request_quit(app: AppHandle) {
    crate::request_quit(&app);
}

/// Quits after the user confirmed; `dont_ask` turns the confirmation off for next time.
#[tauri::command]
pub fn quit_app(app: AppHandle, dont_ask: bool) -> Res<()> {
    if dont_ask && let Some(state) = app.try_state::<AppState>() {
        let mut s = state.engine.settings();
        s.confirm_quit = false;
        state.engine.set_settings(s).map_err(err)?;
    }
    app.exit(0);
    Ok(())
}
