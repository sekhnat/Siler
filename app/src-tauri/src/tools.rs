//! 0.7's power features in the app: checksums, mirrors, ZIP previews, lists, numbered
//! batches, the site grabber, the data quota and the command after a download.

use serde::{Deserialize, Serialize};
use siler_core::checksum::{Checksum, Pieces};
use siler_core::extras::{CommandResult, Mirror, MirrorOrigin, VerifyResult, ZipEntry};
use siler_core::grabber::{GrabConfig, GrabFile, GrabProject};
use siler_core::lists::{ImportItem, ListFormat};
use siler_core::quota::QuotaStatus;
use siler_core::zip::{ZipItem, ZipListing};
use siler_core::{Engine, Event, NewDownload};
use siler_ipc::BrowserItem;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_notification::NotificationExt;

use crate::i18n::{tr, tr_args};
use crate::ipc_server::Origin;
use crate::{AppState, open_add_window};

type Res<T> = Result<T, String>;

/// An error for the interface: an engine error in the interface's language.
fn err<E: std::fmt::Display + 'static>(e: E) -> String {
    match (&e as &dyn std::any::Any).downcast_ref::<siler_core::Error>() {
        Some(core) => crate::i18n::error(core),
        None => e.to_string(),
    }
}

/// What a pending item brings beyond its link.
#[derive(Debug, Clone, Default)]
pub struct PendingExtra {
    pub checksum: Option<Checksum>,
    pub mirrors: Vec<Mirror>,
    pub pieces: Option<Pieces>,
}

impl PendingExtra {
    pub fn label(&self) -> Option<String> {
        self.checksum
            .as_ref()
            .map(|c| format!("{} {}…", c.algo.label(), &c.hex[..12.min(c.hex.len())]))
    }
}

/// One item to review in the Add window.
pub struct Review {
    pub item: BrowserItem,
    pub dir: Option<PathBuf>,
    pub queue_id: Option<i64>,
    pub extra: Option<PendingExtra>,
}

/// Puts items into the Add window for review.
pub fn review(app: &AppHandle, items: Vec<Review>, origin: Origin) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if items.is_empty() {
        return;
    }
    {
        let mut p = state.pending.lock().unwrap();
        for r in items {
            let id = state
                .next_pending
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            p.push(crate::Pending {
                id,
                keep_name: r.item.filename.is_some(),
                item: r.item,
                dir: r.dir,
                start_paused: false,
                queue_id: r.queue_id,
                origin,
                extra: r.extra,
            });
        }
    }
    open_add_window(app);
}

fn review_of_download(nd: NewDownload) -> Review {
    Review {
        item: BrowserItem {
            url: nd.url,
            filename: nd.filename,
            referrer: nd.meta.referrer,
            user_agent: nd.meta.user_agent,
            selected: Some(true),
            ..Default::default()
        },
        dir: nd.dir,
        queue_id: nd.queue_id,
        extra: Some(PendingExtra {
            checksum: nd.checksum,
            mirrors: nd.mirrors,
            pieces: nd.pieces,
        }),
    }
}

/// Reacts to engine events the app handles itself. Returns true when the UI needn't see it.
pub fn on_engine_event(app: &AppHandle, engine: &Engine, ev: &Event) -> bool {
    match ev {
        Event::Metalink { files, .. } => {
            review(
                app,
                files.iter().cloned().map(review_of_download).collect(),
                Origin::List,
            );
            true
        }
        Event::CommandDone {
            filename, result, ..
        } => {
            let cfg = engine.settings().after_download;
            if cfg.notify_on_failure && !result.ok() {
                let why = match (&result.error, result.exit_code) {
                    (Some(e), _) => e.clone(),
                    (None, Some(c)) => tr_args("tools-it-exited-with", vec![("code", c.into())]),
                    (None, None) => tr("tools-it-failed"),
                };
                let _ = app
                    .notification()
                    .builder()
                    .title(tr("tools-the-command-failed"))
                    .body(format!(
                        "{}\n{why}",
                        filename.clone().unwrap_or_else(|| tr("notify-download"))
                    ))
                    .show();
            }
            false
        }
        Event::Quota(q) if q.reached => {
            let _ = app
                .notification()
                .builder()
                .title(tr("tools-data-quota-reached"))
                .body(tr("tools-downloads-continue-next-period"))
                .show();
            false
        }
        _ => false,
    }
}

// ---------------------------------------------------------------- lists

fn import_items(app: &AppHandle, engine: &Engine, items: Vec<ImportItem>) -> usize {
    let s = engine.settings();
    let queues = engine.queues();
    let n = items.len();
    let reviews =
        items
            .into_iter()
            .map(|i| {
                let queue_id = i.queue.as_deref().and_then(|name| {
                    queues
                        .iter()
                        .find(|q| q.name.trim().eq_ignore_ascii_case(name.trim()))
                        .map(|q| q.id)
                });
                let dir = if i.folders.is_empty() {
                    i.dir.clone()
                } else {
                    let name = i.filename.clone().unwrap_or_default();
                    let base = i
                        .dir
                        .clone()
                        .unwrap_or_else(|| s.category_dir(s.category_for(&name)));
                    Some(i.folders.iter().fold(base, |d, f| d.join(f)))
                };
                let extra = (i.checksum.is_some() || !i.mirrors.is_empty() || i.pieces.is_some())
                    .then(|| PendingExtra {
                        checksum: i.checksum.clone(),
                        mirrors: i.mirrors.clone(),
                        pieces: i.pieces.clone(),
                    });
                Review {
                    item: BrowserItem {
                        url: i.url,
                        filename: i.filename,
                        referrer: i.referrer,
                        cookies: i.cookies,
                        user_agent: i.user_agent,
                        size: i.size,
                        selected: Some(true),
                        ..Default::default()
                    },
                    dir,
                    queue_id,
                    extra,
                }
            })
            .collect();
    review(app, reviews, Origin::List);
    n
}

/// Reads a list file and puts its downloads into the Add window.
pub fn import_file(app: &AppHandle, path: &Path) -> Res<usize> {
    let state = app.try_state::<AppState>().ok_or("Siler isn't ready")?;
    let meta = std::fs::metadata(path).map_err(err)?;
    if meta.len() > siler_core::lists::MAX_BYTES as u64 {
        return Err(tr("tools-file-too-large-for-a-list"));
    }
    let bytes = std::fs::read(path).map_err(err)?;
    let (_, items) = siler_core::lists::import(&bytes)?;
    Ok(import_items(app, &state.engine, items))
}

#[tauri::command]
pub fn import_list(app: AppHandle, path: PathBuf) -> Res<usize> {
    import_file(&app, &path)
}

#[tauri::command]
pub fn export_list(
    state: State<'_, AppState>,
    path: PathBuf,
    ids: Vec<i64>,
    format: ListFormat,
    include_cookies: bool,
) -> Res<()> {
    let text = state
        .engine
        .export_list(&ids, format, include_cookies)
        .map_err(err)?;
    std::fs::write(&path, text).map_err(err)?;
    if include_cookies {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[derive(Serialize)]
pub struct Expansion {
    count: usize,
    urls: Vec<String>,
}

/// The addresses of a numbered pattern (`file[001-100].jpg`).
#[tauri::command]
pub fn expand_batch(pattern: String) -> Res<Expansion> {
    let urls = siler_core::batch::expand(&pattern)?;
    for u in [urls.first(), urls.last()].into_iter().flatten() {
        let ok = url::Url::parse(u).is_ok_and(|p| {
            matches!(p.scheme(), "http" | "https" | "ftp" | "ftps" | "sftp")
                && p.host_str().is_some()
        });
        if !ok {
            return Err(format!("“{u}” isn't a web, FTP or SFTP address."));
        }
    }
    Ok(Expansion {
        count: urls.len(),
        urls,
    })
}

// ---------------------------------------------------------------- checksums and mirrors

/// Reads a pasted checksum, for the dialogs to show what it is.
#[tauri::command]
pub fn parse_checksum(text: String) -> Res<Checksum> {
    Checksum::parse(&text)
}

/// The checksum to use: what the user typed (empty clears it), else what came with the item.
pub fn checksum_arg(text: Option<&str>, extra: Option<&PendingExtra>) -> Res<Option<Checksum>> {
    match text.map(str::trim) {
        Some("") => Ok(None),
        Some(t) => {
            // The dialog shows a checksum that came with the item as its hex: keep its source.
            if let Some(c) = extra.and_then(|e| e.checksum.as_ref())
                && c.hex.eq_ignore_ascii_case(t)
            {
                return Ok(Some(c.clone()));
            }
            Checksum::parse(t).map(Some)
        }
        None => Ok(extra.and_then(|e| e.checksum.clone())),
    }
}

/// The mirrors to use: the user's list (keeping where known ones came from), else what came
/// with the item.
pub fn mirrors_arg(urls: Option<&[String]>, extra: Option<&PendingExtra>) -> Vec<Mirror> {
    let known = extra.map(|e| e.mirrors.as_slice()).unwrap_or(&[]);
    match urls {
        None => known.to_vec(),
        Some(list) => list
            .iter()
            .map(|u| u.trim())
            .filter(|u| !u.is_empty())
            .map(|u| {
                known
                    .iter()
                    .find(|m| m.url == u)
                    .cloned()
                    .unwrap_or_else(|| Mirror::new(u.to_string(), MirrorOrigin::User))
            })
            .collect(),
    }
}

#[tauri::command]
pub fn accept_unverified(state: State<'_, AppState>, id: i64) -> Res<()> {
    state.engine.accept_unverified(id).map_err(err)
}

#[tauri::command]
pub async fn verify_file(
    state: State<'_, AppState>,
    id: i64,
    checksum: Option<String>,
) -> Res<VerifyResult> {
    let c = match checksum.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => Some(Checksum::parse(t)?),
        _ => None,
    };
    state.engine.verify_file(id, c).await.map_err(err)
}

#[derive(Serialize)]
pub struct Digest {
    algo: &'static str,
    hex: String,
}

#[tauri::command]
pub async fn file_digests(state: State<'_, AppState>, id: i64) -> Res<Vec<Digest>> {
    let d = state.engine.digests(id).await.map_err(err)?;
    Ok(d.into_iter()
        .map(|(a, hex)| Digest {
            algo: a.label(),
            hex,
        })
        .collect())
}

#[tauri::command]
pub fn set_checksum(state: State<'_, AppState>, id: i64, checksum: Option<String>) -> Res<()> {
    let c = match checksum.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => Some(Checksum::parse(t)?),
        _ => None,
    };
    state.engine.set_checksum(id, c).map_err(err)
}

#[tauri::command]
pub async fn set_mirrors(state: State<'_, AppState>, id: i64, urls: Vec<String>) -> Res<()> {
    state.engine.set_mirrors(id, urls).await.map_err(err)
}

// ---------------------------------------------------------------- ZIP

#[tauri::command]
pub async fn zip_list(
    state: State<'_, AppState>,
    url: String,
    pending_id: Option<u64>,
) -> Res<ZipListing> {
    let meta = crate::commands::pending_meta(&state, pending_id).unwrap_or_default();
    state.engine.zip_list(&url, &meta).await.map_err(err)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZipPick {
    /// The archive's address, as listed.
    url: String,
    archive_size: u64,
    /// The folder the archive's files go to (folders inside it are kept below it).
    dir: PathBuf,
    entries: Vec<ZipItem>,
    pending_id: Option<u64>,
    queue_id: Option<i64>,
    #[serde(default)]
    start_paused: bool,
}

/// Adds files picked out of a ZIP archive. Returns how many were added.
#[tauri::command]
pub fn add_zip_entries(state: State<'_, AppState>, pick: ZipPick) -> Res<usize> {
    let meta = crate::commands::pending_meta(&state, pick.pending_id).unwrap_or_default();
    let mut added = 0;
    for item in &pick.entries {
        if item.is_dir || item.unsupported.is_some() {
            continue;
        }
        let Some((folders, name)) = siler_core::zip::target(&item.path) else {
            continue;
        };
        let entry: ZipEntry = item.entry(pick.archive_size);
        state
            .engine
            .add(NewDownload {
                url: pick.url.clone(),
                filename: Some(name),
                dir: Some(folders.iter().fold(pick.dir.clone(), |d, f| d.join(f))),
                meta: meta.clone(),
                zip_entry: Some(entry),
                queue_id: pick.queue_id,
                start_paused: pick.start_paused,
                ..Default::default()
            })
            .map_err(err)?;
        added += 1;
    }
    if added > 0
        && let Some(pid) = pick.pending_id
    {
        state.pending.lock().unwrap().retain(|p| p.id != pid);
    }
    Ok(added)
}

// ---------------------------------------------------------------- quota and command

#[tauri::command]
pub fn quota_status(state: State<'_, AppState>) -> QuotaStatus {
    state.engine.quota_status()
}

#[tauri::command]
pub fn ignore_quota(state: State<'_, AppState>) -> QuotaStatus {
    state.engine.ignore_quota()
}

#[tauri::command]
pub async fn test_command(
    state: State<'_, AppState>,
    command: String,
    path: PathBuf,
) -> Res<CommandResult> {
    Ok(state.engine.test_command(&command, &path).await)
}

// ---------------------------------------------------------------- site grabber

#[tauri::command]
pub fn grab_projects(state: State<'_, AppState>) -> Res<Vec<GrabProject>> {
    state.engine.grab_projects().map_err(err)
}

#[tauri::command]
pub fn save_grab_project(state: State<'_, AppState>, project: GrabProject) -> Res<i64> {
    state.engine.save_grab_project(project).map_err(err)
}

#[tauri::command]
pub fn delete_grab_project(state: State<'_, AppState>, id: i64) -> Res<()> {
    state.engine.delete_grab_project(id).map_err(err)
}

#[tauri::command]
pub fn grab_start(
    state: State<'_, AppState>,
    config: GrabConfig,
    project_id: Option<i64>,
) -> Res<u64> {
    state.engine.grab_start(config, project_id).map_err(err)
}

#[tauri::command]
pub fn grab_cancel(state: State<'_, AppState>, run_id: u64) {
    state.engine.grab_cancel(run_id);
}

/// Puts the files a grab found into the Add window.
#[tauri::command]
pub fn review_grab(app: AppHandle, files: Vec<GrabFile>, queue_id: Option<i64>) -> usize {
    let n = files.len();
    let reviews = files
        .into_iter()
        .map(|f| Review {
            item: BrowserItem {
                url: f.url,
                filename: f.filename,
                referrer: Some(f.referrer.clone()),
                page_url: Some(f.referrer),
                size: f.size,
                selected: Some(true),
                ..Default::default()
            },
            dir: f.dir,
            queue_id,
            extra: None,
        })
        .collect();
    review(&app, reviews, Origin::Grabber);
    n
}

/// Opens a saved site's start page.
#[tauri::command]
pub fn open_saved_site(path: PathBuf) -> Res<()> {
    if !path.is_file() {
        return Err(tr("tools-saved-page-gone"));
    }
    crate::launch::open(&path).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use siler_core::checksum::ChecksumSource;

    #[test]
    fn checksum_and_mirror_arguments() {
        let given = Checksum::parse("900150983cd24fb0d6963f7d28e17f72").unwrap();
        let mut from_file = given.clone();
        from_file.source = ChecksumSource::Metalink;
        let extra = PendingExtra {
            checksum: Some(from_file.clone()),
            mirrors: vec![Mirror::new("https://m/a".into(), MirrorOrigin::Metalink)],
            pieces: None,
        };
        assert_eq!(
            checksum_arg(None, Some(&extra)).unwrap(),
            Some(from_file.clone())
        );
        assert_eq!(
            checksum_arg(Some(&given.hex), Some(&extra))
                .unwrap()
                .unwrap()
                .source,
            ChecksumSource::Metalink
        );
        assert_eq!(checksum_arg(Some(" "), Some(&extra)).unwrap(), None);
        assert!(checksum_arg(Some("nope"), None).is_err());
        let m = mirrors_arg(
            Some(&["https://m/a".into(), "https://n/a".into(), " ".into()]),
            Some(&extra),
        );
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].origin, MirrorOrigin::Metalink);
        assert_eq!(m[1].origin, MirrorOrigin::User);
        assert_eq!(mirrors_arg(None, Some(&extra)).len(), 1);
    }
}
