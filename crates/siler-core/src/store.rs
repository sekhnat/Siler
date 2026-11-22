//! SQLite persistence for the download list and settings.

use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};
use std::path::{Path, PathBuf};

use crate::extras::Extras;
use crate::media::{JobKind, MediaRecord};
use crate::queue::Queue;
use crate::{Category, OnConflict, RequestMeta, Result, Segment, Settings, Status};

/// Everything Siler knows about one download, including secrets needed to resume it.
#[derive(Debug, Clone)]
pub(crate) struct Record {
    pub id: i64,
    pub url: String,
    pub final_url: Option<String>,
    pub filename: Option<String>,
    pub dir: Option<PathBuf>,
    pub category: Category,
    pub total_size: Option<u64>,
    pub downloaded: u64,
    pub status: Status,
    pub resumable: Option<bool>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub meta: RequestMeta,
    pub connections: u8,
    pub speed_limit: u64,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub segments: Vec<Segment>,
    /// Queue order: lower starts first.
    pub position: i64,
    pub on_conflict: OnConflict,
    pub queue_id: i64,
    /// Check the server for a newer version this often (seconds); 0 = never.
    pub sync_every: i64,
    /// Last check for a newer version (ms since the epoch).
    pub sync_checked_at: Option<i64>,
    /// Not saved: the server wanted a login the last time this download ran.
    pub needs_login: Option<crate::LoginRequest>,
    /// Not saved: the SFTP server's key has to be trusted first.
    pub needs_host_key: Option<crate::HostKeyRequest>,
    pub kind: JobKind,
    /// The media details of a media download.
    pub media: Option<MediaRecord>,
    pub extras: Extras,
}

const SCHEMA_V1: &str = "
CREATE TABLE downloads (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    url           TEXT NOT NULL,
    final_url     TEXT,
    filename      TEXT,
    dir           TEXT,
    category      TEXT NOT NULL,
    total_size    INTEGER,
    downloaded    INTEGER NOT NULL DEFAULT 0,
    status        TEXT NOT NULL,
    resumable     INTEGER,
    error         TEXT,
    notice        TEXT,
    etag          TEXT,
    last_modified TEXT,
    meta          TEXT NOT NULL,
    connections   INTEGER NOT NULL,
    speed_limit   INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    completed_at  INTEGER,
    segments      TEXT NOT NULL DEFAULT '[]'
);
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

const SCHEMA_V2: &str = "
ALTER TABLE downloads ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
UPDATE downloads SET position = id;
ALTER TABLE downloads ADD COLUMN on_conflict TEXT NOT NULL DEFAULT 'rename';
";

const SCHEMA_V4: &str = "
ALTER TABLE downloads ADD COLUMN kind TEXT NOT NULL DEFAULT 'file';
ALTER TABLE downloads ADD COLUMN media TEXT;
";

const SCHEMA_V3: &str = "
CREATE TABLE queues (
    id       INTEGER PRIMARY KEY,
    name     TEXT NOT NULL,
    position INTEGER NOT NULL,
    config   TEXT NOT NULL
);
INSERT INTO queues (id, name, position, config) VALUES (1, 'Main queue', 1, '{}');
ALTER TABLE downloads ADD COLUMN queue_id INTEGER NOT NULL DEFAULT 1;
ALTER TABLE downloads ADD COLUMN sync_every INTEGER NOT NULL DEFAULT 0;
ALTER TABLE downloads ADD COLUMN sync_checked_at INTEGER;
";

const SCHEMA_V5: &str = "
ALTER TABLE downloads ADD COLUMN extras TEXT NOT NULL DEFAULT '{}';
CREATE TABLE grab_projects (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL,
    config     TEXT NOT NULL,
    last_run   INTEGER,
    created_at INTEGER NOT NULL
);
CREATE TABLE grab_seen (
    project_id INTEGER NOT NULL,
    url        TEXT NOT NULL,
    PRIMARY KEY (project_id, url)
);
";

/// A saved site-grabber project: id, name, config JSON, last run.
pub(crate) type GrabRow = (i64, String, String, Option<i64>);

pub(crate) struct Store {
    conn: Connection,
}

fn i(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn u(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path)?;
        restrict_permissions(path);
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 1 {
            self.conn.execute_batch(SCHEMA_V1)?;
            self.conn.pragma_update(None, "user_version", 1)?;
        }
        if version < 2 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(SCHEMA_V2)?;
            tx.pragma_update(None, "user_version", 2)?;
            tx.commit()?;
        }
        if version < 3 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(SCHEMA_V3)?;
            tx.pragma_update(None, "user_version", 3)?;
            tx.commit()?;
        }
        if version < 4 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(SCHEMA_V4)?;
            tx.pragma_update(None, "user_version", 4)?;
            tx.commit()?;
        }
        if version < 5 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(SCHEMA_V5)?;
            tx.pragma_update(None, "user_version", 5)?;
            tx.commit()?;
        }
        Ok(())
    }

    fn values(r: &Record) -> Vec<(&'static str, Box<dyn ToSql>)> {
        vec![
            (":url", Box::new(r.url.clone())),
            (":final_url", Box::new(r.final_url.clone())),
            (":filename", Box::new(r.filename.clone())),
            (
                ":dir",
                Box::new(r.dir.as_ref().map(|d| d.to_string_lossy().into_owned())),
            ),
            (":category", Box::new(r.category.as_str())),
            (":total_size", Box::new(r.total_size.map(i))),
            (":downloaded", Box::new(i(r.downloaded))),
            (":status", Box::new(r.status.as_str())),
            (":resumable", Box::new(r.resumable)),
            (":error", Box::new(r.error.clone())),
            (":notice", Box::new(r.notice.clone())),
            (":etag", Box::new(r.etag.clone())),
            (":last_modified", Box::new(r.last_modified.clone())),
            (":meta", Box::new(to_json(&r.meta))),
            (":connections", Box::new(r.connections)),
            (":speed_limit", Box::new(i(r.speed_limit))),
            (":created_at", Box::new(r.created_at)),
            (":completed_at", Box::new(r.completed_at)),
            (":segments", Box::new(to_json(&r.segments))),
            (":position", Box::new(r.position)),
            (":on_conflict", Box::new(r.on_conflict.as_str())),
            (":queue_id", Box::new(r.queue_id)),
            (":sync_every", Box::new(r.sync_every)),
            (":sync_checked_at", Box::new(r.sync_checked_at)),
            (":kind", Box::new(r.kind.as_str())),
            (":media", Box::new(r.media.as_ref().map(to_json))),
            (":extras", Box::new(to_json(&r.extras))),
        ]
    }

    fn exec(&self, sql: &str, values: &[(&'static str, Box<dyn ToSql>)]) -> Result<usize> {
        let refs: Vec<(&str, &dyn ToSql)> = values.iter().map(|(k, v)| (*k, v.as_ref())).collect();
        Ok(self.conn.prepare_cached(sql)?.execute(refs.as_slice())?)
    }

    pub fn insert(&self, r: &Record) -> Result<i64> {
        self.exec(
            "INSERT INTO downloads (url, final_url, filename, dir, category, total_size, downloaded, status,
                resumable, error, notice, etag, last_modified, meta, connections, speed_limit, created_at,
                completed_at, segments, position, on_conflict, queue_id, sync_every, sync_checked_at,
                kind, media, extras)
             VALUES (:url, :final_url, :filename, :dir, :category, :total_size, :downloaded, :status,
                :resumable, :error, :notice, :etag, :last_modified, :meta, :connections, :speed_limit,
                :created_at, :completed_at, :segments, :position, :on_conflict, :queue_id,
                :sync_every, :sync_checked_at, :kind, :media, :extras)",
            &Self::values(r),
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update(&self, r: &Record) -> Result<()> {
        let mut values = Self::values(r);
        values.push((":id", Box::new(r.id)));
        self.exec(
            "UPDATE downloads SET url=:url, final_url=:final_url, filename=:filename, dir=:dir,
                category=:category, total_size=:total_size, downloaded=:downloaded, status=:status,
                resumable=:resumable, error=:error, notice=:notice, etag=:etag,
                last_modified=:last_modified, meta=:meta, connections=:connections,
                speed_limit=:speed_limit, created_at=:created_at, completed_at=:completed_at,
                segments=:segments, position=:position, on_conflict=:on_conflict,
                queue_id=:queue_id, sync_every=:sync_every, sync_checked_at=:sync_checked_at,
                kind=:kind, media=:media, extras=:extras
             WHERE id=:id",
            &values,
        )?;
        Ok(())
    }

    pub fn update_progress(&self, id: i64, downloaded: u64, segments: &[Segment]) -> Result<()> {
        let segs = serde_json::to_string(segments).unwrap_or_else(|_| "[]".into());
        self.conn
            .prepare_cached("UPDATE downloads SET downloaded=?1, segments=?2 WHERE id=?3")?
            .execute(params![i(downloaded), segs, id])?;
        Ok(())
    }

    pub fn delete_many(&self, ids: &[i64]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM downloads WHERE id=?1")?;
            for id in ids {
                stmt.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_positions(&self, positions: &[(i64, i64)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached("UPDATE downloads SET position=?1 WHERE id=?2")?;
            for (id, pos) in positions {
                stmt.execute([pos, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_all(&self) -> Result<Vec<Record>> {
        let mut stmt = self.conn.prepare("SELECT * FROM downloads ORDER BY id")?;
        let rows = stmt.query_map([], row_to_record)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn load_queues(&self) -> Result<Vec<Queue>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, config FROM queues ORDER BY position, id")?;
        let rows = stmt.query_map([], |r| {
            Ok(Queue::from_row(
                r.get(0)?,
                r.get(1)?,
                &r.get::<_, String>(2)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Inserts the queue, or updates it when its id exists. Returns the id.
    pub fn save_queue(&self, q: &Queue) -> Result<i64> {
        if q.id > 0 {
            self.conn.execute(
                "UPDATE queues SET name=?1, config=?2 WHERE id=?3",
                params![q.name, q.config_json(), q.id],
            )?;
            return Ok(q.id);
        }
        self.conn.execute(
            "INSERT INTO queues (name, position, config)
             VALUES (?1, (SELECT COALESCE(MAX(position), 0) + 1 FROM queues), ?2)",
            params![q.name, q.config_json()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Deletes a queue; its downloads move to the main queue.
    pub fn delete_queue(&self, id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("UPDATE downloads SET queue_id=1 WHERE queue_id=?1", [id])?;
        tx.execute("DELETE FROM queues WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// Saved site-grabber projects: (id, name, config JSON, last run).
    pub fn load_grab_projects(&self) -> Result<Vec<GrabRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, config, last_run FROM grab_projects ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Inserts the project, or updates it when its id exists. Returns the id.
    pub fn save_grab_project(&self, id: i64, name: &str, config: &str) -> Result<i64> {
        if id > 0 {
            self.conn.execute(
                "UPDATE grab_projects SET name=?1, config=?2 WHERE id=?3",
                params![name, config, id],
            )?;
            return Ok(id);
        }
        self.conn.execute(
            "INSERT INTO grab_projects (name, config, created_at) VALUES (?1, ?2, ?3)",
            params![name, config, crate::manager::now_ms()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn delete_grab_project(&self, id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM grab_seen WHERE project_id=?1", [id])?;
        tx.execute("DELETE FROM grab_projects WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_grab_last_run(&self, id: i64, at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE grab_projects SET last_run=?1 WHERE id=?2",
            params![at, id],
        )?;
        Ok(())
    }

    /// Files a project already found.
    pub fn grab_seen(&self, id: i64) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT url FROM grab_seen WHERE project_id=?1")?;
        let rows = stmt.query_map([id], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn add_grab_seen(&self, id: i64, urls: &[String]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO grab_seen (project_id, url) VALUES (?1, ?2)",
            )?;
            for u in urls {
                stmt.execute(params![id, u])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_settings(&self) -> Result<Option<Settings>> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key='settings'", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
    }

    /// A small value kept beside the settings (e.g. the quota's current period).
    pub fn load_value(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn save_value(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        let json = serde_json::to_string(s).expect("settings serialize");
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES ('settings', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [json],
        )?;
        Ok(())
    }
}

fn row_to_record(row: &Row) -> rusqlite::Result<Record> {
    let meta: String = row.get("meta")?;
    let segments: String = row.get("segments")?;
    let category: String = row.get("category")?;
    let status: String = row.get("status")?;
    Ok(Record {
        id: row.get("id")?,
        url: row.get("url")?,
        final_url: row.get("final_url")?,
        filename: row.get("filename")?,
        dir: row.get::<_, Option<String>>("dir")?.map(PathBuf::from),
        category: Category::parse(&category),
        total_size: row.get::<_, Option<i64>>("total_size")?.map(u),
        downloaded: u(row.get("downloaded")?),
        status: Status::parse(&status),
        resumable: row.get("resumable")?,
        error: row.get("error")?,
        notice: row.get("notice")?,
        etag: row.get("etag")?,
        last_modified: row.get("last_modified")?,
        meta: serde_json::from_str(&meta).unwrap_or_default(),
        connections: row.get("connections")?,
        speed_limit: u(row.get("speed_limit")?),
        created_at: row.get("created_at")?,
        completed_at: row.get("completed_at")?,
        segments: serde_json::from_str(&segments).unwrap_or_default(),
        position: row.get("position")?,
        on_conflict: OnConflict::parse(&row.get::<_, String>("on_conflict")?),
        queue_id: row.get("queue_id")?,
        sync_every: row.get("sync_every")?,
        sync_checked_at: row.get("sync_checked_at")?,
        needs_login: None,
        needs_host_key: None,
        kind: JobKind::parse(&row.get::<_, String>("kind")?),
        media: row
            .get::<_, Option<String>>("media")?
            .and_then(|m| serde_json::from_str(&m).ok()),
        extras: serde_json::from_str(&row.get::<_, String>("extras")?).unwrap_or_default(),
    })
}

/// The database holds forwarded cookies, so keep it private to the user.
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

fn to_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_a_version_1_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("siler.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            for (url, status) in [
                ("https://x/a.iso", "paused"),
                ("https://x/b.iso", "completed"),
            ] {
                conn.execute(
                    "INSERT INTO downloads (url, category, status, meta, connections, created_at)
                     VALUES (?1, 'programs', ?2, '{\"referrer\":\"https://x/\",\"cookies\":\"a=b\",\"user_agent\":null,\"headers\":[]}', 8, 1)",
                    [url, status],
                )
                .unwrap();
            }
        }
        let store = Store::open(&path).unwrap();
        let recs = store.load_all().unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].position, recs[0].id);
        assert_eq!(recs[1].position, recs[1].id);
        assert_eq!(recs[0].on_conflict, OnConflict::Rename);
        assert_eq!(recs[0].status, Status::Paused);
        assert_eq!(recs[0].meta.cookies.as_deref(), Some("a=b"));
        assert_eq!(recs[0].meta.method, None);
        assert_eq!(recs[0].queue_id, 1);
        assert_eq!(recs[0].sync_every, 0);
        let queues = store.load_queues().unwrap();
        assert_eq!(queues.len(), 1);
        assert_eq!(queues[0].name, "Main queue");
        assert_eq!(queues[0].max_concurrent, 0);
        assert!(recs[0].extras.is_empty());
        let mut r = recs[0].clone();
        r.extras.checksum =
            crate::checksum::Checksum::parse("900150983cd24fb0d6963f7d28e17f72").ok();
        store.update(&r).unwrap();
        assert_eq!(store.load_all().unwrap()[0].extras, r.extras);
        store.save_value("quota", "{}").unwrap();
        assert_eq!(store.load_value("quota").unwrap().as_deref(), Some("{}"));
        assert!(store.load_settings().unwrap().is_none());
        // Opening again is a no-op.
        drop(store);
        assert_eq!(Store::open(&path).unwrap().load_all().unwrap().len(), 2);
    }
}
