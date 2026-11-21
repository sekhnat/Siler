//! The site grabber: follows a site's pages to a set depth, finds the files on them that match
//! a filter, and can save the pages themselves (with their images, style sheets and scripts)
//! for offline browsing, their links rewritten to the saved copies.
//!
//! Pages and their small resources are fetched by the grabber; the files it finds become
//! ordinary downloads (reviewed in the batch list, or queued directly by a scheduled project).

mod crawl;
pub mod html;
pub mod paths;
pub mod robots;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use tokio_util::sync::CancellationToken;

use crate::manager::{Inner, now_ms};
use crate::scheduler::{self, Edge, Schedule};
use crate::{Category, Engine, Error, Event, NewDownload, RequestMeta, Result, Settings};

static RUN_IDS: AtomicU64 = AtomicU64::new(1);

/// Which pages the grabber follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Pages on the start page's server only.
    #[default]
    SameHost,
    /// Also the site's other servers (`www.` and `docs.` of `example.com`).
    SameSite,
    /// Only pages in the start page's folder and below.
    Folder,
}

/// Which links count as files to download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileFilter {
    pub categories: Vec<Category>,
    /// Extensions without the dot (`iso`, `zip`).
    pub extensions: Vec<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Files may be on any server (a site's files are often on another one).
    pub any_host: bool,
}

impl Default for FileFilter {
    fn default() -> Self {
        FileFilter {
            categories: vec![
                Category::Compressed,
                Category::Programs,
                Category::Documents,
                Category::Video,
                Category::Music,
            ],
            extensions: Vec::new(),
            min_size: None,
            max_size: None,
            any_host: true,
        }
    }
}

impl FileFilter {
    pub fn matches(&self, url: &url::Url, s: &Settings) -> bool {
        let Some(name) = paths::file_name(url) else {
            return false;
        };
        let ext = paths::extension(url);
        if !ext.is_empty()
            && self
                .extensions
                .iter()
                .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(&ext))
        {
            return true;
        }
        let c = s.category_for(&name);
        c != Category::Other && self.categories.contains(&c)
    }

    fn size_ok(&self, size: Option<u64>) -> bool {
        match size {
            Some(n) => self.min_size.is_none_or(|m| n >= m) && self.max_size.is_none_or(|m| n <= m),
            // Unknown sizes pass unless a minimum was asked for.
            None => self.min_size.is_none(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GrabConfig {
    pub start_url: String,
    /// How many links away from the start page to go (0 = the start page only).
    pub depth: u32,
    pub scope: Scope,
    pub max_pages: u32,
    /// Pages fetched at once.
    pub concurrency: u8,
    /// Leave alone what the site's robots.txt asks crawlers to.
    pub robots: bool,
    /// Save the pages (and what they show) for offline browsing.
    pub save_pages: bool,
    pub filter: FileFilter,
    /// Where pages and files go; the download folder ("Sites" for saved pages, the category
    /// folders for files) when empty.
    pub dir: Option<PathBuf>,
    pub queue_id: Option<i64>,
    /// Skip files an earlier run of the project found.
    pub only_new: bool,
    pub schedule: Schedule,
}

impl Default for GrabConfig {
    fn default() -> Self {
        GrabConfig {
            start_url: String::new(),
            depth: 2,
            scope: Scope::SameHost,
            max_pages: 500,
            concurrency: 4,
            robots: true,
            save_pages: false,
            filter: FileFilter::default(),
            dir: None,
            queue_id: None,
            only_new: false,
            schedule: Schedule::default(),
        }
    }
}

impl GrabConfig {
    pub fn normalized(mut self) -> Self {
        self.start_url = self.start_url.trim().to_string();
        self.depth = self.depth.min(20);
        self.max_pages = self.max_pages.clamp(1, 20_000);
        self.concurrency = self.concurrency.clamp(1, 16);
        self.filter.extensions = self
            .filter
            .extensions
            .iter()
            .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        self.schedule = self.schedule.normalized();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrabProject {
    pub id: i64,
    pub name: String,
    pub config: GrabConfig,
    pub last_run: Option<i64>,
}

/// A file the grabber found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrabFile {
    pub url: String,
    /// The page it was found on.
    pub referrer: String,
    pub filename: Option<String>,
    /// Where it goes (its place in a saved site, or the chosen folder).
    pub dir: Option<PathBuf>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GrabResult {
    pub files: Vec<GrabFile>,
    pub pages: u32,
    pub saved: u32,
    pub errors: u32,
    /// Pages left out because robots.txt asked.
    pub blocked: u32,
    /// The saved start page, to open the site offline.
    pub site_index: Option<PathBuf>,
    /// The files were queued straight away (a scheduled run).
    pub queued: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GrabProgress {
    pub run_id: u64,
    pub project_id: Option<i64>,
    pub pages_done: u32,
    pub pages_queued: u32,
    pub files_found: u32,
    /// Pages, images, style sheets and scripts saved for offline browsing.
    pub saved: u32,
    pub bytes: u64,
    pub current: Option<String>,
    pub finished: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    pub result: Option<GrabResult>,
}

impl Engine {
    pub fn grab_projects(&self) -> Result<Vec<GrabProject>> {
        let rows = self.inner().store().load_grab_projects()?;
        Ok(rows
            .into_iter()
            .map(|(id, name, config, last_run)| GrabProject {
                id,
                name,
                config: serde_json::from_str(&config).unwrap_or_default(),
                last_run,
            })
            .collect())
    }

    /// Saves a project (a new one when `id` is 0) and returns its id.
    pub fn save_grab_project(&self, p: GrabProject) -> Result<i64> {
        let name = p.name.trim();
        if name.is_empty() {
            return Err(Error::Protocol("The project needs a name".into()));
        }
        let config = p.config.normalized();
        crate::probe::parse_url(&config.start_url)?;
        let json = serde_json::to_string(&config).map_err(|e| Error::Protocol(e.to_string()))?;
        self.inner().store().save_grab_project(p.id, name, &json)
    }

    pub fn delete_grab_project(&self, id: i64) -> Result<()> {
        self.inner().store().delete_grab_project(id)
    }

    /// Starts grabbing a site; progress and the result come as [`Event::Grab`].
    pub fn grab_start(&self, config: GrabConfig, project_id: Option<i64>) -> Result<u64> {
        start(self.inner(), config.normalized(), project_id, false)
    }

    pub fn grab_cancel(&self, run_id: u64) {
        if let Some((_, token)) = self.inner().grabs.lock().unwrap().get(&run_id) {
            token.cancel();
        }
    }
}

fn start(
    inner: &Arc<Inner>,
    config: GrabConfig,
    project_id: Option<i64>,
    unattended: bool,
) -> Result<u64> {
    crate::probe::parse_url(&config.start_url)?;
    let run_id = RUN_IDS.fetch_add(1, Relaxed);
    let token = CancellationToken::new();
    {
        let mut grabs = inner.grabs.lock().unwrap();
        if let Some(p) = project_id
            && grabs.values().any(|(q, _)| *q == Some(p))
        {
            return Err(Error::Protocol("This project is already running".into()));
        }
        grabs.insert(run_id, (project_id, token.clone()));
    }
    let me = inner.clone();
    inner.rt.spawn(async move {
        let seen = match project_id.filter(|_| config.only_new) {
            Some(p) => me.store().grab_seen(p).unwrap_or_default(),
            None => HashSet::new(),
        };
        let emit = |p: &GrabProgress| {
            me.emit(Event::Grab(Box::new(GrabProgress {
                run_id,
                project_id,
                ..p.clone()
            })))
        };
        let outcome = crawl::crawl(&me, &config, &seen, &token, &emit).await;
        me.grabs.lock().unwrap().remove(&run_id);
        let mut last = GrabProgress {
            finished: true,
            cancelled: token.is_cancelled(),
            ..Default::default()
        };
        match outcome {
            Ok(mut result) => {
                if let Some(p) = project_id {
                    let urls: Vec<String> = result.files.iter().map(|f| f.url.clone()).collect();
                    let store = me.store();
                    let _ = store.add_grab_seen(p, &urls);
                    let _ = store.set_grab_last_run(p, now_ms());
                }
                if unattended {
                    result.queued = queue_files(&me, &config, &result.files);
                }
                last.pages_done = result.pages;
                last.files_found = result.files.len() as u32;
                last.saved = result.saved;
                last.result = Some(result);
            }
            Err(e) => last.error = Some(e),
        }
        emit(&last);
    });
    Ok(run_id)
}

/// Adds the files of a scheduled run to the project's queue.
fn queue_files(inner: &Arc<Inner>, config: &GrabConfig, files: &[GrabFile]) -> usize {
    let engine = Engine::from_inner(inner.clone());
    files
        .iter()
        .filter(|f| {
            engine
                .add(NewDownload {
                    url: f.url.clone(),
                    filename: f.filename.clone(),
                    dir: f.dir.clone(),
                    queue_id: config.queue_id,
                    meta: RequestMeta {
                        referrer: Some(f.referrer.clone()),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .is_ok()
        })
        .count()
}

/// Starts the scheduled projects whose start time came.
pub(crate) fn run_schedules(inner: &Arc<Inner>, last: jiff::Timestamp, now: jiff::Timestamp) {
    let Ok(projects) = Engine::from_inner(inner.clone()).grab_projects() else {
        return;
    };
    let tz = scheduler::local_tz();
    for mut p in projects {
        let due = p
            .config
            .schedule
            .edges(&tz, last, now)
            .iter()
            .any(|(_, e)| *e == Edge::Start);
        if !due {
            continue;
        }
        tracing::info!("site grab project {}: scheduled start", p.id);
        if let Err(e) = start(inner, p.config.clone(), Some(p.id), true) {
            tracing::info!("site grab project {} didn't start: {e}", p.id);
        }
        if p.config.schedule.once {
            p.config.schedule.enabled = false;
            if let Ok(json) = serde_json::to_string(&p.config) {
                let _ = inner.store().save_grab_project(p.id, &p.name, &json);
            }
        }
    }
}
