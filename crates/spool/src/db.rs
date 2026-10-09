//! SQLite storage. Rows keep the columns queries filter on, plus a JSON document with the rest,
//! so the schema stays small while the models grow.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

const MIGRATIONS: &[&str] = &[
    r#"
    CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE profiles (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, data TEXT NOT NULL);
    CREATE TABLE indexers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, data TEXT NOT NULL);
    CREATE TABLE titles (
        id INTEGER PRIMARY KEY,
        kind TEXT NOT NULL,
        title TEXT NOT NULL,
        year INTEGER NOT NULL DEFAULT 0,
        monitored INTEGER NOT NULL DEFAULT 1,
        profile_id INTEGER NOT NULL DEFAULT 0,
        path TEXT NOT NULL DEFAULT '',
        tmdb_id INTEGER, tvdb_id INTEGER, tvmaze_id INTEGER, imdb_id TEXT,
        data TEXT NOT NULL
    );
    CREATE UNIQUE INDEX titles_tmdb ON titles(kind, tmdb_id) WHERE tmdb_id IS NOT NULL;
    CREATE UNIQUE INDEX titles_tvdb ON titles(kind, tvdb_id) WHERE tvdb_id IS NOT NULL;
    CREATE TABLE files (
        id INTEGER PRIMARY KEY,
        title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
        rel_path TEXT NOT NULL,
        size INTEGER NOT NULL DEFAULT 0,
        data TEXT NOT NULL
    );
    CREATE INDEX files_title ON files(title_id);
    CREATE TABLE episodes (
        id INTEGER PRIMARY KEY,
        title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
        season INTEGER NOT NULL,
        episode INTEGER NOT NULL,
        air_date TEXT,
        monitored INTEGER NOT NULL DEFAULT 1,
        file_id INTEGER REFERENCES files(id) ON DELETE SET NULL,
        data TEXT NOT NULL,
        UNIQUE(title_id, season, episode)
    );
    CREATE TABLE acquisitions (
        id INTEGER PRIMARY KEY,
        title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
        state TEXT NOT NULL,
        job_id TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        data TEXT NOT NULL
    );
    CREATE INDEX acquisitions_state ON acquisitions(state);
    CREATE INDEX acquisitions_job ON acquisitions(job_id);
    CREATE TABLE history (
        id INTEGER PRIMARY KEY,
        ts INTEGER NOT NULL,
        title_id INTEGER,
        kind TEXT NOT NULL,
        data TEXT NOT NULL
    );
    CREATE INDEX history_title ON history(title_id, ts);
    CREATE TABLE decisions (
        id INTEGER PRIMARY KEY,
        ts INTEGER NOT NULL,
        title_id INTEGER NOT NULL,
        release_title TEXT NOT NULL,
        accepted INTEGER NOT NULL,
        data TEXT NOT NULL
    );
    CREATE INDEX decisions_title ON decisions(title_id, ts);
    CREATE TABLE blocklist (
        id INTEGER PRIMARY KEY,
        ts INTEGER NOT NULL,
        title_id INTEGER NOT NULL,
        guid TEXT NOT NULL,
        release_title TEXT NOT NULL,
        reason TEXT NOT NULL
    );
    CREATE INDEX blocklist_title ON blocklist(title_id);
    CREATE TABLE attention (
        id INTEGER PRIMARY KEY,
        ts INTEGER NOT NULL,
        kind TEXT NOT NULL,
        title_id INTEGER,
        acquisition_id INTEGER,
        message TEXT NOT NULL,
        data TEXT NOT NULL,
        resolved_at INTEGER
    );
    CREATE TABLE journal (
        id INTEGER PRIMARY KEY,
        ts INTEGER NOT NULL,
        acquisition_id INTEGER,
        state TEXT NOT NULL,
        data TEXT NOT NULL
    );
    "#,
    r#"
    -- Every NZB Spool has fetched, kept so a release can be downloaded again without an indexer.
    -- Keyed by the film or series, not the library row, so it outlives removing a title.
    CREATE TABLE nzbs (
        id INTEGER PRIMARY KEY,
        kind TEXT NOT NULL,
        tmdb_id INTEGER, tvdb_id INTEGER, imdb_id TEXT,
        name TEXT NOT NULL,
        year INTEGER NOT NULL DEFAULT 0,
        release_title TEXT NOT NULL,
        size INTEGER NOT NULL DEFAULT 0,
        fetched_at INTEGER NOT NULL,
        data TEXT NOT NULL
    );
    CREATE UNIQUE INDEX nzbs_release ON nzbs(kind, COALESCE(tmdb_id, 0), COALESCE(tvdb_id, 0), release_title);
    CREATE INDEX nzbs_tmdb ON nzbs(tmdb_id);
    CREATE INDEX nzbs_tvdb ON nzbs(tvdb_id);

    -- What Plex has, as last read from the server.
    CREATE TABLE plex_items (
        rating_key TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        title TEXT NOT NULL,
        year INTEGER NOT NULL DEFAULT 0,
        tmdb_id INTEGER, tvdb_id INTEGER, imdb_id TEXT,
        data TEXT NOT NULL
    );
    CREATE INDEX plex_tmdb ON plex_items(tmdb_id);
    CREATE INDEX plex_tvdb ON plex_items(tvdb_id);
    "#,
    r#"
    -- Requests made and NZBs fetched per indexer per local day, to respect daily allowances.
    CREATE TABLE indexer_usage (
        day TEXT NOT NULL,
        indexer_id INTEGER NOT NULL,
        requests INTEGER NOT NULL DEFAULT 0,
        grabs INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (day, indexer_id)
    );
    "#,
];

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        // The database holds indexer keys and usenet passwords.
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Self::init(conn)
    }

    pub fn memory() -> Result<Db> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Db> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = {}; COMMIT;", i + 1)).with_context(|| format!("migration {}", i + 1))?;
        }
        Ok(Db(Arc::new(Mutex::new(conn))))
    }

    /// Run a closure with the connection. Keep it short: every caller shares one connection.
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let conn = self.0.lock();
        Ok(f(&conn)?)
    }

    /// Run a closure inside a transaction; an error rolls everything back.
    pub fn tx<T>(&self, f: impl FnOnce(&rusqlite::Transaction) -> Result<T>) -> Result<T> {
        let mut conn = self.0.lock();
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    pub fn get_setting<T: DeserializeOwned + Default>(&self, key: &str) -> T {
        self.with(|c| c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get::<_, String>(0)).optional())
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let v = serde_json::to_string(value)?;
        self.with(|c| c.execute("INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, v]))?;
        Ok(())
    }

    /// Write a consistent copy of the database to `dest`.
    pub fn backup(&self, dest: &Path) -> Result<()> {
        let _ = std::fs::remove_file(dest);
        self.with(|c| c.execute("VACUUM INTO ?1", [dest.to_string_lossy()]))?;
        Ok(())
    }
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn json<T: DeserializeOwned>(s: String) -> rusqlite::Result<T> {
    serde_json::from_str(&s).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}
