use serde::{Deserialize, Serialize};
use siler_core::proxy::ProxyRoute;
use siler_core::{
    AfterDone, Category, ContainerChoice, DownloadInfo, EditDownload, Login, LoginRequest,
    NewDownload, NewMediaDownload, OnConflict, ProbeResult, Queue, QueueMove, RequestMeta,
    Selection, Settings, SourceHint, Status, SubtitleMode,
};
use siler_ipc::BrowserItem;
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::i18n::{tr, tr_args};
use crate::integration::{self, IntegrationStatus};
use crate::logins::{self, SavedLogin};
use crate::{AppState, PendingView, StartupError};
use crate::{power, sound};

type Res<T> = Result<T, String>;

/// An error for the interface: an engine error in the interface's language, anything else
/// as it says itself.
fn err<E: Display + 'static>(e: E) -> String {
    match (&e as &dyn std::any::Any).downcast_ref::<siler_core::Error>() {
        Some(core) => crate::i18n::error(core),
        None => e.to_string(),
    }
}

/// The request context to replay for a download handed over by the browser.
pub fn meta_of(item: &BrowserItem) -> RequestMeta {
    let body = item
        .body
        .as_deref()
        .and_then(siler_core::decode_base64)
        .filter(|b| b.len() <= siler_core::MAX_BODY);
    RequestMeta {
        referrer: item.referrer.clone().or_else(|| item.page_url.clone()),
        page_url: item.page_url.clone(),
        cookies: item.cookies.clone(),
        user_agent: item.user_agent.clone(),
        headers: item.headers.clone(),
        method: item.method.clone().filter(|_| body.is_some()),
        body,
        origin: None,
    }
    // Whatever link this ends up on, it was given for this one.
    .bound_to(&item.url)
}

pub(crate) fn pending_meta(state: &AppState, pending_id: Option<u64>) -> Option<RequestMeta> {
    let id = pending_id?;
    let p = state.pending.lock().unwrap();
    p.iter().find(|p| p.id == id).map(|p| meta_of(&p.item))
}

/// What the extension said about the video of a pending item, as long as the dialog still
/// has its link (an edited link is a different source).
fn pending_hint(state: &AppState, pending_id: Option<u64>, url: &str) -> Option<SourceHint> {
    let id = pending_id?;
    let p = state.pending.lock().unwrap();
    let item = &p.iter().find(|p| p.id == id)?.item;
    if item.url != url {
        return None;
    }
    hint_of(item.media.as_ref()?)
}

/// The extension's hint as the engine takes it: the two have the same shape.
pub fn hint_of(m: &siler_ipc::MediaHint) -> Option<SourceHint> {
    serde_json::to_value(m)
        .and_then(serde_json::from_value)
        .ok()
}

/// Where a pending item's link came from, as long as the dialog still has that link.
fn pending_link(state: &AppState, pending_id: Option<u64>, url: &str) -> siler_core::LinkSource {
    let p = state.pending.lock().unwrap();
    p.iter()
        .find(|p| Some(p.id) == pending_id && p.item.url == url)
        .map(|p| crate::links::source_of(&p.item))
        .unwrap_or_default()
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
    /// An SFTP server whose key has to be trusted first.
    host_key: Option<Box<siler_core::HostKeyRequest>>,
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
        host_key: match &e {
            siler_core::Error::HostKeyUnknown(r) => Some(r.clone()),
            _ => None,
        },
    })
}

/// Trusts an SFTP server's key (Siler's own known_hosts), resuming the downloads waiting for it.
/// The version and how Siler was installed.
#[derive(Serialize)]
pub struct AppInfo {
    version: &'static str,
    install: crate::updates::InstallKind,
}

/// The interface's language as the app resolved it (Settings, else the system's), so the
/// window shows the same one as the tray and notifications.
#[tauri::command]
pub fn ui_language() -> String {
    crate::i18n::language().to_string()
}

#[tauri::command]
pub fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        install: crate::updates::install_kind(),
    }
}

/// What the last update check found (this session), if anything.
#[tauri::command]
pub fn update_status() -> Option<crate::updates::UpdateInfo> {
    crate::updates::LAST.lock().unwrap().clone()
}

#[tauri::command]
pub async fn check_updates_now(app: AppHandle) -> Res<crate::updates::UpdateInfo> {
    crate::updates::check(&app).await
}

/// Updates an AppImage in place and restarts it.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Res<()> {
    crate::updates::install(&app).await
}

/// The Chromium extension, copied where it stays put for "Load unpacked", and the
/// Chromium-based browsers found.
#[derive(Serialize)]
pub struct ChromiumExtension {
    path: String,
    browsers: Vec<&'static str>,
}

#[tauri::command]
pub fn chromium_extension() -> Res<ChromiumExtension> {
    let path = crate::integration::chromium_extension().map_err(err)?;
    Ok(ChromiumExtension {
        path: path.to_string_lossy().into_owned(),
        browsers: crate::integration::chromium_browsers()
            .into_iter()
            .map(|(n, _)| n)
            .collect(),
    })
}

/// Opens a Chromium browser's extensions page (to turn on developer mode and load the
/// extension). Web links can't open `chrome://` pages, so the browser is started with it.
#[tauri::command]
pub fn open_chromium_extensions(browser: String) -> Res<()> {
    let (_, prog) = crate::integration::chromium_browsers()
        .into_iter()
        .find(|(n, _)| *n == browser)
        .ok_or_else(|| format!("{browser} wasn't found"))?;
    let page = match browser.as_str() {
        "Edge" => "edge://extensions/",
        "Brave" => "brave://extensions/",
        "Vivaldi" => "vivaldi://extensions/",
        "Opera" => "opera://extensions/",
        _ => "chrome://extensions/",
    };
    crate::launch::start(prog, &[std::ffi::OsStr::new(page)]).map_err(err)
}

/// Ends a recording and keeps what was recorded.
#[tauri::command]
pub fn finish_download(state: State<'_, AppState>, id: i64) -> Res<()> {
    state.engine.finish(id).map_err(err)
}

#[tauri::command]
pub fn trust_host_key(state: State<'_, AppState>, request: siler_core::HostKeyRequest) -> Res<()> {
    state.engine.trust_host_key(&request).map_err(err)
}

/// Uses a login for a server (resuming its downloads), optionally saving it in the keyring.
#[tauri::command]
pub async fn submit_login(
    state: State<'_, AppState>,
    host: String,
    username: String,
    password: String,
    remember: bool,
    ticket: bool,
) -> Res<()> {
    let login = if ticket {
        Login::kerberos_ticket()
    } else {
        Login { username, password }
    };
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

/// "Test with an address": which way it would go under these settings, saved or not.
#[tauri::command]
pub async fn check_proxy(
    state: State<'_, AppState>,
    settings: siler_core::proxy::ProxySettings,
    url: String,
) -> Res<siler_core::proxy::ProxyCheck> {
    state
        .engine
        .check_proxy_with(&settings, &url)
        .await
        .map_err(err)
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
    queue_id: Option<i64>,
    /// A checksum as typed or pasted; empty clears one that came with the item.
    checksum: Option<String>,
    /// Mirrors, one address each; absent keeps the ones that came with the item.
    mirrors: Option<Vec<String>>,
}

#[tauri::command]
pub fn add_download(app: AppHandle, state: State<'_, AppState>, args: AddArgs) -> Res<i64> {
    let meta = pending_meta(&state, args.pending_id).unwrap_or_default();
    let extra = args.pending_id.and_then(|id| {
        let p = state.pending.lock().unwrap();
        p.iter().find(|p| p.id == id).and_then(|p| p.extra.clone())
    });
    let checksum = crate::tools::checksum_arg(args.checksum.as_deref(), extra.as_ref())?;
    let mirrors = crate::tools::mirrors_arg(args.mirrors.as_deref(), extra.as_ref());
    let link = pending_link(&state, args.pending_id, &args.url);
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
            queue_id: args.queue_id,
            checksum,
            mirrors,
            pieces: extra.and_then(|e| e.pieces),
            zip_entry: None,
            link,
        })
        .map_err(err)?;
    forget_pending(&state, args.pending_id);
    if !args.start_paused {
        crate::progress::want(&app, id);
    }
    Ok(id)
}

/// Looks at a link as a media source: a stream with its tracks, or why it isn't one. A saved
/// login is used without asking, as for files; otherwise the error says which login is needed.
#[tauri::command]
pub async fn probe_media(
    state: State<'_, AppState>,
    url: String,
    pending_id: Option<u64>,
) -> Result<siler_core::MediaProbe, ProbeError> {
    let meta = pending_meta(&state, pending_id).unwrap_or_default();
    let hint = pending_hint(&state, pending_id, &url);
    let mut result = state.engine.probe_media(&url, &meta, hint.as_ref()).await;
    if let Err(e) = &result
        && let Some(req) = e.login_request()
        && !req.rejected
        && let Some(login) = logins::find(&req.host).await
    {
        let _ = state.engine.set_login(&req.host, login);
        result = state.engine.probe_media(&url, &meta, hint.as_ref()).await;
    }
    result.map_err(|e| ProbeError {
        message: e.to_string(),
        login: e.login_request(),
        host_key: None,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMediaArgs {
    url: String,
    filename: Option<String>,
    dir: Option<PathBuf>,
    selection: Option<Selection>,
    container: Option<ContainerChoice>,
    subtitles: Option<SubtitleMode>,
    #[serde(default)]
    start_paused: bool,
    on_conflict: Option<OnConflict>,
    pending_id: Option<u64>,
    queue_id: Option<i64>,
}

/// Adds a stream as a media download with the chosen tracks, or the default ones.
#[tauri::command]
pub async fn add_media(app: AppHandle, state: State<'_, AppState>, args: AddMediaArgs) -> Res<i64> {
    let meta = pending_meta(&state, args.pending_id).unwrap_or_default();
    let hint = pending_hint(&state, args.pending_id, &args.url);
    let link = pending_link(&state, args.pending_id, &args.url);
    let id = state
        .engine
        .add_media(NewMediaDownload {
            link,
            url: args.url,
            meta,
            hint,
            filename: args.filename.filter(|f| !f.trim().is_empty()),
            dir: args.dir,
            selection: args.selection,
            container: args.container,
            subtitles: args.subtitles,
            start_paused: args.start_paused,
            on_conflict: args.on_conflict,
            queue_id: args.queue_id,
        })
        .await
        .map_err(err)?;
    forget_pending(&state, args.pending_id);
    if !args.start_paused {
        crate::progress::want(&app, id);
    }
    Ok(id)
}

/// What the FFmpeg check found, for Settings → Media.
#[tauri::command]
pub fn media_diagnostics(state: State<'_, AppState>) -> siler_core::FfmpegStatus {
    state.engine.ffmpeg_status()
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
                .or_else(|| siler_core::filename::name_from_url(&p.item.url));
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
                queue_id: p.queue_id,
                keep_name: p.keep_name,
                origin: p.origin,
                site: p.item.media.as_ref().and_then(|m| m.site.clone()),
                title: p.item.media.as_ref().and_then(|m| m.title.clone()),
                stream: p.item.media.as_ref().is_some_and(|m| m.manifest.is_some()),
                checksum: p.extra.as_ref().and_then(|e| e.label()),
                mirrors: p.extra.as_ref().map_or(0, |e| e.mirrors.len()),
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
pub fn resume(app: AppHandle, state: State<'_, AppState>, id: i64) -> Res<()> {
    state.engine.resume(id).map_err(err)?;
    crate::progress::want(&app, id);
    Ok(())
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
pub fn start_now(app: AppHandle, state: State<'_, AppState>, id: i64) -> Res<()> {
    state.engine.start_now(id).map_err(err)?;
    crate::progress::want(&app, id);
    Ok(())
}

#[tauri::command]
pub fn start_queue(state: State<'_, AppState>, queue_id: i64) -> Res<()> {
    state.engine.start_queue(queue_id).map_err(err)
}

/// Stopping a queue by hand only holds back what hasn't started (like before);
/// the schedule's stop also pauses running downloads.
#[tauri::command]
pub fn stop_queue(
    state: State<'_, AppState>,
    queue_id: i64,
    pause_running: Option<bool>,
) -> Res<()> {
    state
        .engine
        .stop_queue(queue_id, pause_running.unwrap_or(false))
        .map_err(err)
}

#[tauri::command]
pub fn list_queues(state: State<'_, AppState>) -> Vec<Queue> {
    state.engine.queues()
}

#[tauri::command]
pub fn create_queue(state: State<'_, AppState>, queue: Queue) -> Res<i64> {
    state.engine.create_queue(queue).map_err(err)
}

#[tauri::command]
pub fn update_queue(state: State<'_, AppState>, queue: Queue) -> Res<()> {
    state.engine.update_queue(queue).map_err(err)
}

#[tauri::command]
pub fn delete_queue(state: State<'_, AppState>, queue_id: i64) -> Res<()> {
    state.engine.delete_queue(queue_id).map_err(err)
}

#[tauri::command]
pub fn move_to_queue(state: State<'_, AppState>, ids: Vec<i64>, queue_id: i64) -> Res<()> {
    state.engine.move_to_queue(&ids, queue_id).map_err(err)
}

/// Keep a finished download up to date: check every `every_secs` seconds (0 = off).
#[tauri::command]
pub fn set_sync(state: State<'_, AppState>, id: i64, every_secs: u64) -> Res<()> {
    state.engine.set_sync(id, every_secs).map_err(err)
}

#[tauri::command]
pub fn check_for_update(state: State<'_, AppState>, id: i64) -> Res<()> {
    state.engine.check_for_update(id).map_err(err)
}

#[tauri::command]
pub async fn power_caps() -> power::PowerCaps {
    power::caps(&power::Logind).await
}

#[tauri::command]
pub fn power_pending(state: State<'_, AppState>) -> Option<power::PendingPowerView> {
    state.power.view()
}

#[tauri::command]
pub fn power_cancel(app: AppHandle, state: State<'_, AppState>) {
    if state.power.cancel() {
        tracing::info!("cancelled the action after downloads");
    }
    if let Some(w) = app.get_webview_window("countdown") {
        let _ = w.close();
    }
    let _ = app.emit("power-countdown", None::<power::PendingPowerView>);
}

#[tauri::command]
pub async fn power_now(app: AppHandle) {
    power::run_now(&app).await;
}

#[tauri::command]
pub fn get_when_done(state: State<'_, AppState>) -> AfterDone {
    *state.power.when_done.lock().unwrap()
}

/// "When all downloads finish": a one-off action, cleared once it has run.
pub fn set_when_done_action(app: &AppHandle, action: AfterDone) {
    if let Some(state) = app.try_state::<AppState>() {
        *state.power.when_done.lock().unwrap() = action;
        tracing::info!("when all downloads finish: {action:?}");
        let _ = app.emit("when-done-changed", action);
        crate::tray::refresh();
    }
}

#[tauri::command]
pub fn set_when_done(app: AppHandle, action: AfterDone) {
    set_when_done_action(&app, action);
}

#[tauri::command]
pub fn test_sound(
    state: State<'_, AppState>,
    sound: sound::Sound,
    settings: Option<siler_core::SoundSettings>,
) {
    let s = settings.unwrap_or_else(|| state.engine.settings().sounds);
    sound::play(&s, sound);
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
    let found = siler_core::FoundLink {
        meta: pending_meta(&state, pending_id),
        hint: pending_hint(&state, pending_id, &url),
        source: Some(pending_link(&state, pending_id, &url)).filter(|s| !s.is_empty()),
        url,
    };
    state.engine.refresh_link(id, found).await.map_err(err)?;
    forget_pending(&state, pending_id);
    Ok(())
}

/// How long "Refresh link" waits for the browser to hand over a new link.
const REFRESH_WAIT: Duration = Duration::from_secs(120);

/// Opens the page the download came from, so the user can click the link again; the next
/// matching hand-over from the browser updates this download. Returns whether a page was
/// known.
#[tauri::command]
pub fn await_refresh(state: State<'_, AppState>, id: i64) -> Res<bool> {
    open_page_for_link(&state, id)
}

/// Also what the expired link's notification and the tray panel's row do.
pub(crate) fn open_page_for_link(state: &AppState, id: i64) -> Res<bool> {
    let d = state.engine.get(id).ok_or_else(|| tr("error-not-found"))?;
    state
        .awaiting_refresh
        .wait(id, Instant::now() + REFRESH_WAIT);
    let page = d.page_url.or(d.referrer);
    if let Some(page) = &page {
        crate::launch::open(page).map_err(err)?;
    }
    Ok(page.is_some())
}

#[tauri::command]
pub fn cancel_await_refresh(state: State<'_, AppState>, id: i64) {
    state.awaiting_refresh.stop(id);
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
pub fn default_categories() -> Vec<siler_core::CategoryRule> {
    siler_core::default_categories()
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.engine.settings()
}

#[tauri::command]
pub fn set_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> Res<Settings> {
    let before = state.engine.settings().ui;
    let saved = state.engine.set_settings(settings).map_err(err)?;
    if saved.ui.skin != before.skin || saved.ui.toolbar_style != before.toolbar_style {
        crate::skins::changed(&app);
    }
    Ok(saved)
}

fn download(app: &AppHandle, id: i64) -> Res<DownloadInfo> {
    app.try_state::<AppState>()
        .ok_or_else(|| tr("command-siler-isnt-running"))?
        .engine
        .get(id)
        .ok_or_else(|| tr("error-not-found"))
}

fn path_of(app: &AppHandle, id: i64) -> Res<PathBuf> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| tr("command-siler-isnt-running"))?;
    state
        .engine
        .file_path(id)
        .ok_or_else(|| tr("command-no-file-yet"))
}

/// The file of a finished download, which is still there.
pub fn finished_path(app: &AppHandle, id: i64) -> Res<PathBuf> {
    let d = download(app, id)?;
    if d.status != Status::Completed {
        return Err(tr("command-not-finished-yet"));
    }
    let path = path_of(app, id)?;
    if !path.exists() {
        return Err(siler_core::Error::FileMissing.to_string());
    }
    Ok(path)
}

/// Opens a finished file with its default application.
pub fn open_path(app: &AppHandle, id: i64) -> Res<()> {
    crate::launch::open(finished_path(app, id)?).map_err(err)
}

/// Shows the file (or the partial file) selected in its folder.
pub fn reveal(app: &AppHandle, id: i64) -> Res<()> {
    let path = path_of(app, id)?;
    if path.exists() {
        return app.opener().reveal_item_in_dir(&path).map_err(err);
    }
    match path.parent().filter(|d| d.exists()) {
        Some(dir) => crate::launch::open(dir).map_err(err),
        None => Err(tr("command-folder-gone")),
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
pub fn open_url(url: String) -> Res<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(tr("command-only-web-links"));
    }
    crate::launch::open(&url).map_err(err)
}

#[tauri::command]
pub fn open_log_folder() -> Res<()> {
    let dir = crate::logging::dir();
    std::fs::create_dir_all(&dir).map_err(err)?;
    crate::launch::open(&dir).map_err(err)
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

/// Siler starts (in the tray) when you log in.
#[tauri::command]
pub fn autostart_enabled() -> bool {
    crate::autostart::enabled()
}

#[tauri::command]
pub fn set_autostart(on: bool) -> Res<()> {
    crate::autostart::set(on).map_err(err)
}

/// Opens the signed extension with Firefox, which offers to install it.
#[tauri::command]
pub fn install_extension() -> Res<()> {
    let xpi = integration::xpi_for_firefox().map_err(err)?;
    crate::launch::start("firefox", &[xpi.as_os_str()]).map_err(|e| {
        tr_args(
            "command-couldnt-start-firefox",
            vec![("why", e.to_string().into())],
        )
    })
}

#[tauri::command]
pub fn close_tray_panel(app: AppHandle) {
    crate::tray::panel::close(&app);
}

#[tauri::command]
pub fn resize_tray_panel(app: AppHandle, height: i32) {
    crate::tray::panel::resize(&app, height);
}

/// Shows a download in the main window, and what it is waiting for.
#[tauri::command]
pub fn show_download(app: AppHandle, id: i64) -> Res<()> {
    let d = download(&app, id)?;
    crate::show_main(&app);
    let _ = app.emit("select-download", id);
    if d.needs_login.is_some() || d.needs_host_key.is_some() {
        let _ = app.emit("ask-login", id);
    } else if d.needs_link {
        let _ = app.emit("ask-link", id);
    }
    Ok(())
}

/// The speed limit for everything, changed from the panel: the main window follows.
#[tauri::command]
pub fn set_global_speed_limit(app: AppHandle, limit: u64) -> Res<Settings> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| tr("command-siler-isnt-running"))?;
    let mut s = state.engine.settings();
    s.global_speed_limit = limit;
    let s = state.engine.set_settings(s).map_err(err)?;
    let _ = app.emit("settings-changed", s.clone());
    Ok(s)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// The Add window's link can be edited: the first link's cookies don't follow it.
    #[tokio::test]
    async fn an_edited_link_doesnt_get_the_first_links_cookies() {
        let server = std::net::TcpListener::bind("127.0.0.2:0").unwrap();
        let edited = format!("http://{}/f.bin", server.local_addr().unwrap());
        let request = std::thread::spawn(move || {
            let (mut s, _) = server.accept().unwrap();
            let (mut buf, mut got) = ([0u8; 4096], Vec::new());
            while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut buf).unwrap() {
                    0 => break,
                    n => got.extend_from_slice(&buf[..n]),
                }
            }
            s.write_all(
                b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx",
            )
            .unwrap();
            String::from_utf8_lossy(&got).to_ascii_lowercase()
        });
        let handed_over = BrowserItem {
            url: "http://127.0.0.1:9/first.bin".into(),
            cookies: Some("session=secret".into()),
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let engine = siler_core::Engine::open(tmp.path().join("data")).unwrap();
        engine.probe(&edited, &meta_of(&handed_over)).await.unwrap();
        let request = request.join().unwrap();
        assert!(!request.contains("session=secret"), "{request}");
    }
}
