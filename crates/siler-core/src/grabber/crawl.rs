//! The crawl itself: pages breadth first, then the resources of saved pages, then the links
//! of saved pages rewritten to the saved copies.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::html::{self, Kind};
use super::paths::{self, local_path, relative_link};
use super::robots::Robots;
use super::{GrabConfig, GrabFile, GrabProgress, GrabResult, Scope};
use crate::http::{self, Fetched, Http, Limiters, Overflow, Req};
use crate::manager::Inner;
use crate::ratelimit::RateLimiter;
use crate::{RequestMeta, Settings};

/// The largest page or resource the grabber fetches itself.
const MAX_BODY: u64 = 20 * 1024 * 1024;
/// The most resources (images, style sheets, scripts) saved for one site.
const MAX_ASSETS: usize = 5_000;

/// A URL without its fragment, as a key.
fn key(u: &Url) -> String {
    let mut u = u.clone();
    u.set_fragment(None);
    u.to_string()
}

fn bare_host(u: &Url) -> String {
    let h = u.host_str().unwrap_or("").to_ascii_lowercase();
    h.strip_prefix("www.").map(str::to_string).unwrap_or(h)
}

struct ScopeRule {
    host: String,
    site: String,
    folder: String,
    scope: Scope,
}

impl ScopeRule {
    fn new(start: &Url, scope: Scope) -> ScopeRule {
        let host = bare_host(start);
        let site = crate::scope::site_of(&host).to_string();
        let path = start.path();
        let folder = path[..path.rfind('/').map_or(0, |i| i + 1)].to_string();
        ScopeRule {
            host,
            site,
            folder,
            scope,
        }
    }

    fn same_host(&self, u: &Url) -> bool {
        bare_host(u) == self.host
    }

    fn contains(&self, u: &Url) -> bool {
        match self.scope {
            Scope::SameHost => self.same_host(u),
            Scope::SameSite => {
                let h = bare_host(u);
                h == self.site || h.ends_with(&format!(".{}", self.site))
            }
            Scope::Folder => self.same_host(u) && u.path().starts_with(&self.folder),
        }
    }
}

fn is_html(f: &Fetched) -> bool {
    match &f.content_type {
        Some(ct) => {
            let ct = ct.to_ascii_lowercase();
            ct.contains("text/html") || ct.contains("xhtml")
        }
        None => {
            let head =
                String::from_utf8_lossy(&f.body[..f.body.len().min(1024)]).to_ascii_lowercase();
            head.contains("<html") || head.contains("<!doctype html")
        }
    }
}

fn is_css(f: &Fetched, u: &Url) -> bool {
    f.content_type
        .as_deref()
        .is_some_and(|c| c.to_ascii_lowercase().contains("text/css"))
        || paths::extension(u) == "css"
}

async fn fetch(
    http: Arc<Http>,
    global: Arc<RateLimiter>,
    url: Url,
) -> (Url, crate::Result<Fetched>) {
    let run = RateLimiter::new(0);
    let io = Limiters {
        global: &global,
        run: &run,
        count: None,
    };
    let r = http::fetch_bounded(&http, &url, Req::Derived, MAX_BODY, Overflow::Refuse, &io).await;
    (url, r)
}

async fn write_file(path: &Path, body: &[u8]) -> bool {
    if let Some(parent) = path.parent()
        && tokio::fs::create_dir_all(parent).await.is_err()
    {
        return false;
    }
    tokio::fs::write(path, body).await.is_ok()
}

struct SavedPage {
    url: Url,
    local: PathBuf,
    charset: Option<String>,
}

pub(super) async fn crawl(
    inner: &Arc<Inner>,
    cfg: &GrabConfig,
    seen: &HashSet<String>,
    cancel: &CancellationToken,
    emit: &(dyn Fn(&GrabProgress) + Sync),
) -> Result<GrabResult, String> {
    let s: Settings = inner.settings();
    let start = crate::probe::parse_web_url(&cfg.start_url).map_err(|e| e.to_string())?;
    let private_ok = siler_media::policy::is_private_source(&start);
    let http = Arc::new(Http::new(
        inner.net.clone(),
        &start,
        RequestMeta::default(),
        &s.user_agent,
        s.allow_private_media_addresses,
    ));
    let global = inner.global.clone();
    let rule = ScopeRule::new(&start, cfg.scope);
    let root = cfg.dir.clone().unwrap_or_else(|| {
        s.download_dir
            .join(if cfg.save_pages { "Sites" } else { "" })
    });
    let mut progress = GrabProgress::default();
    let mut result = GrabResult::default();

    let robots = if cfg.robots {
        let mut robots_url = start.clone();
        robots_url.set_path("/robots.txt");
        robots_url.set_query(None);
        robots_url.set_fragment(None);
        let run = RateLimiter::new(0);
        let io = Limiters {
            global: &global,
            run: &run,
            count: None,
        };
        let fetched = tokio::select! {
            r = http::fetch_bounded(&http, &robots_url, Req::Derived, 512 * 1024, Overflow::Truncate, &io) => r.ok(),
            _ = cancel.cancelled() => None,
        };
        fetched
            .map(|f| Robots::parse(&String::from_utf8_lossy(&f.body), "Siler"))
            .unwrap_or_default()
    } else {
        Robots::default()
    };
    let allowed = |u: &Url| {
        let mut p = u.path().to_string();
        if let Some(q) = u.query() {
            p.push('?');
            p.push_str(q);
        }
        robots.allowed(&p)
    };

    let mut visited: HashSet<String> = HashSet::from([key(&start)]);
    let mut frontier: VecDeque<(Url, u32)> = VecDeque::from([(start.clone(), 0)]);
    let mut depth_of: HashMap<String, u32> = HashMap::new();
    let mut saved_pages: Vec<SavedPage> = Vec::new();
    // Every saved or downloaded URL and where it is, relative to `root`.
    let mut map: HashMap<String, PathBuf> = HashMap::new();
    let mut files: Vec<GrabFile> = Vec::new();
    let mut file_keys: HashSet<String> = HashSet::new();
    let mut assets: Vec<(Url, u32)> = Vec::new();
    let mut asset_keys: HashSet<String> = HashSet::new();
    let mut start_local: Option<PathBuf> = None;

    let mut add_file = |u: &Url, page: &Url, files: &mut Vec<GrabFile>| {
        let k = key(u);
        if !private_ok && siler_media::policy::check(u, false).is_err() {
            return;
        }
        if file_keys.insert(k.clone()) && !seen.contains(&k) {
            files.push(GrabFile {
                url: k,
                referrer: page.to_string(),
                filename: None,
                dir: None,
                size: None,
            });
        }
    };

    // Pages, breadth first.
    let mut set: JoinSet<(Url, crate::Result<Fetched>)> = JoinSet::new();
    loop {
        while set.len() < cfg.concurrency as usize
            && progress.pages_done + (set.len() as u32) < cfg.max_pages
            && let Some((u, d)) = frontier.pop_front()
        {
            depth_of.insert(key(&u), d);
            set.spawn(fetch(http.clone(), global.clone(), u));
        }
        if set.is_empty() {
            break;
        }
        progress.pages_queued = (frontier.len() + set.len()) as u32;
        let joined = tokio::select! {
            j = set.join_next() => j,
            _ = cancel.cancelled() => break,
        };
        let Some(Ok((asked, fetched))) = joined else {
            continue;
        };
        progress.pages_done += 1;
        result.pages += 1;
        let depth = depth_of.get(&key(&asked)).copied().unwrap_or(0);
        let f = match fetched {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!(url = %asked, "grab: {e}");
                result.errors += 1;
                continue;
            }
        };
        progress.bytes += f.body.len() as u64;
        progress.current = Some(f.final_url.to_string());
        let page = f.final_url.clone();
        visited.insert(key(&page));
        if !rule.contains(&page) && depth > 0 {
            continue; // redirected off the site
        }
        if !is_html(&f) {
            // A page-like address that is really a file (a download script).
            if cfg.filter.matches(&page, &s) || cfg.filter.matches(&asked, &s) {
                add_file(&asked, &page, &mut files);
            }
            continue;
        }
        let charset = html::charset_of(f.content_type.as_deref());
        if cfg.save_pages {
            let local = local_path(&page, true);
            if write_file(&root.join(&local), &f.body).await {
                result.saved += 1;
                progress.saved += 1;
                map.insert(key(&page), local.clone());
                map.insert(key(&asked), local.clone());
                if start_local.is_none() {
                    start_local = Some(local.clone());
                }
                saved_pages.push(SavedPage {
                    url: page.clone(),
                    local,
                    charset: charset.clone(),
                });
            }
        }
        let (found, _) = html::process(&f.body, &page, charset.as_deref(), false, &mut |_, _| None);
        for (kind, link) in found.links {
            let k = key(&link);
            if cfg.filter.matches(&link, &s) {
                if cfg.filter.any_host || rule.same_host(&link) || rule.contains(&link) {
                    add_file(&link, &page, &mut files);
                }
                continue;
            }
            match kind {
                Kind::Page if paths::is_page_like(&link) && rule.contains(&link) => {
                    if depth >= cfg.depth || visited.contains(&k) {
                        continue;
                    }
                    visited.insert(k);
                    if allowed(&link) {
                        frontier.push_back((link, depth + 1));
                    } else {
                        result.blocked += 1;
                    }
                }
                Kind::Asset
                    if cfg.save_pages
                        && asset_keys.len() < MAX_ASSETS
                        && asset_keys.insert(k.clone()) =>
                {
                    assets.push((link, 0));
                }
                _ => {}
            }
        }
        progress.files_found = files.len() as u32;
        emit(&progress);
    }
    set.shutdown().await;
    if cancel.is_cancelled() {
        return Err("The grab was stopped.".into());
    }

    // Sizes, when the filter asks for them.
    if cfg.filter.min_size.is_some() || cfg.filter.max_size.is_some() {
        let ua = s.user_agent.clone();
        let mut sized = JoinSet::new();
        let mut checked: Vec<GrabFile> = Vec::new();
        let mut pending = std::mem::take(&mut files).into_iter();
        loop {
            while sized.len() < cfg.concurrency as usize
                && let Some(f) = pending.next()
            {
                let (net, ua) = (inner.net.clone(), ua.clone());
                sized.spawn(async move {
                    let meta = RequestMeta {
                        referrer: Some(f.referrer.clone()),
                        ..Default::default()
                    };
                    let size = crate::probe::probe(&net, &f.url, &meta, &ua)
                        .await
                        .ok()
                        .and_then(|p| p.total_size);
                    GrabFile { size, ..f }
                });
            }
            if sized.is_empty() {
                break;
            }
            let joined = tokio::select! {
                j = sized.join_next() => j,
                _ = cancel.cancelled() => break,
            };
            if let Some(Ok(f)) = joined
                && cfg.filter.size_ok(f.size)
            {
                checked.push(f);
            }
        }
        files = checked;
    }

    // What saved pages show: images, style sheets (and what they load), scripts, fonts.
    if cfg.save_pages {
        let mut queue: VecDeque<(Url, u32)> = assets.drain(..).collect();
        let mut css_saved: Vec<(Url, PathBuf)> = Vec::new();
        let mut aset: JoinSet<(Url, crate::Result<Fetched>)> = JoinSet::new();
        let mut asset_depth: HashMap<String, u32> = HashMap::new();
        loop {
            while aset.len() < cfg.concurrency as usize
                && let Some((u, d)) = queue.pop_front()
            {
                if map.contains_key(&key(&u)) {
                    continue;
                }
                asset_depth.insert(key(&u), d);
                aset.spawn(fetch(http.clone(), global.clone(), u));
            }
            if aset.is_empty() {
                break;
            }
            let joined = tokio::select! {
                j = aset.join_next() => j,
                _ = cancel.cancelled() => break,
            };
            let Some(Ok((asked, Ok(f)))) = joined else {
                continue;
            };
            progress.bytes += f.body.len() as u64;
            progress.current = Some(f.final_url.to_string());
            let mut local = local_path(&f.final_url, false);
            let css = is_css(&f, &f.final_url);
            if css && local.extension().is_none_or(|e| e != "css") {
                local.set_extension("css");
            }
            if !write_file(&root.join(&local), &f.body).await {
                continue;
            }
            result.saved += 1;
            progress.saved += 1;
            map.insert(key(&asked), local.clone());
            map.insert(key(&f.final_url), local.clone());
            let d = asset_depth.get(&key(&asked)).copied().unwrap_or(0);
            if css {
                let text = String::from_utf8_lossy(&f.body).into_owned();
                if d < 2 {
                    let base = f.final_url.clone();
                    html::css_urls(&text, &mut |u| {
                        if let Ok(abs) = base.join(&html::decode_entities(u))
                            && matches!(abs.scheme(), "http" | "https")
                            && asset_keys.len() < MAX_ASSETS
                            && asset_keys.insert(key(&abs))
                        {
                            queue.push_back((abs, d + 1));
                        }
                        None
                    });
                }
                css_saved.push((f.final_url.clone(), local));
            }
            emit(&progress);
        }
        aset.shutdown().await;
        if cancel.is_cancelled() {
            return Err("The grab was stopped.".into());
        }

        // Files get their place in the saved site, so its pages link to them.
        for file in &mut files {
            if let Ok(u) = Url::parse(&file.url) {
                let local = local_path(&u, false);
                file.dir = local.parent().map(|p| root.join(p));
                file.filename = local.file_name().map(|n| n.to_string_lossy().into_owned());
                map.insert(file.url.clone(), local);
            }
        }

        // Links of saved pages and style sheets now point to the saved copies; links to
        // anything that wasn't saved point to the site itself.
        let link_to = |from: &Path, u: &Url| -> String {
            match map.get(&key(u)) {
                Some(to) => {
                    let mut rel = relative_link(from, to);
                    if let Some(f) = u.fragment() {
                        rel.push('#');
                        rel.push_str(f);
                    }
                    rel
                }
                None => u.to_string(),
            }
        };
        for p in &saved_pages {
            let path = root.join(&p.local);
            let Ok(body) = tokio::fs::read(&path).await else {
                continue;
            };
            let (_, out) = html::process(&body, &p.url, p.charset.as_deref(), true, &mut |_, u| {
                Some(link_to(&p.local, u))
            });
            if !out.is_empty() {
                let _ = tokio::fs::write(&path, out).await;
            }
        }
        for (url, local) in &css_saved {
            let path = root.join(local);
            let Ok(body) = tokio::fs::read(&path).await else {
                continue;
            };
            let text = String::from_utf8_lossy(&body).into_owned();
            let out = html::css_urls(&text, &mut |raw| {
                let abs = url.join(&html::decode_entities(raw)).ok()?;
                Some(link_to(local, &abs))
            });
            let _ = tokio::fs::write(&path, out).await;
        }
        result.site_index = start_local.map(|l| root.join(l));
    } else if let Some(dir) = &cfg.dir {
        for file in &mut files {
            file.dir = Some(dir.clone());
        }
    }

    result.files = files;
    Ok(result)
}
