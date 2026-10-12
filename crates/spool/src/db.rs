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

/// The database, as a list of steps. A new database takes them all in order; one that has
/// taken some takes the rest. A step is never changed once a database has taken it: what
/// comes next is a new step at the end.
const MIGRATIONS: &[&str] = &[
    r#"
    -- The tables.

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

    CREATE TABLE files (
        id INTEGER PRIMARY KEY,
        title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
        rel_path TEXT NOT NULL,
        size INTEGER NOT NULL DEFAULT 0,
        data TEXT NOT NULL
    );

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

    CREATE TABLE history (
        id INTEGER PRIMARY KEY,
        ts INTEGER NOT NULL,
        title_id INTEGER,
        kind TEXT NOT NULL,
        data TEXT NOT NULL
    );

    CREATE TABLE decisions (
        id INTEGER PRIMARY KEY,
        ts INTEGER NOT NULL,
        title_id INTEGER NOT NULL,
        release_title TEXT NOT NULL,
        accepted INTEGER NOT NULL,
        data TEXT NOT NULL
    );

    CREATE TABLE blocklist (
        id INTEGER PRIMARY KEY,
        ts INTEGER NOT NULL,
        title_id INTEGER NOT NULL,
        guid TEXT NOT NULL,
        release_title TEXT NOT NULL,
        reason TEXT NOT NULL
    );

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

    CREATE TABLE plex_items (
        rating_key TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        title TEXT NOT NULL,
        year INTEGER NOT NULL DEFAULT 0,
        tmdb_id INTEGER, tvdb_id INTEGER, imdb_id TEXT,
        data TEXT NOT NULL
    );

    CREATE TABLE indexer_usage (
        day TEXT NOT NULL,
        indexer_id INTEGER NOT NULL,
        requests INTEGER NOT NULL DEFAULT 0,
        grabs INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (day, indexer_id)
    );

    CREATE TABLE watched (
        kind TEXT NOT NULL,
        ref_id INTEGER NOT NULL,
        position REAL NOT NULL DEFAULT 0,
        played INTEGER NOT NULL DEFAULT 0,
        play_count INTEGER NOT NULL DEFAULT 0,
        favorite INTEGER NOT NULL DEFAULT 0,
        last_played_at INTEGER NOT NULL DEFAULT 0,
        updated_at INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (kind, ref_id)
    );

    CREATE TABLE revision (id INTEGER PRIMARY KEY CHECK (id = 1), n INTEGER NOT NULL);

    CREATE TABLE changes (
        kind TEXT NOT NULL,
        ref_id INTEGER NOT NULL,
        title_id INTEGER NOT NULL,
        rev INTEGER NOT NULL,
        removed INTEGER NOT NULL DEFAULT 0,
        at INTEGER NOT NULL,
        PRIMARY KEY (kind, ref_id)
    );

    CREATE TABLE anime (
        id INTEGER PRIMARY KEY,
        depth INTEGER NOT NULL,
        fetched_at INTEGER NOT NULL,
        data TEXT NOT NULL
    );

    CREATE TABLE film_sets (
        id INTEGER PRIMARY KEY,
        fetched_at INTEGER NOT NULL,
        data TEXT NOT NULL
    );

    INSERT INTO revision VALUES (1, 0);

    -- What they are looked up by.

    CREATE UNIQUE INDEX titles_tmdb ON titles(kind, tmdb_id) WHERE tmdb_id IS NOT NULL;

    CREATE UNIQUE INDEX titles_tvdb ON titles(kind, tvdb_id) WHERE tvdb_id IS NOT NULL;

    CREATE INDEX files_title ON files(title_id);

    CREATE INDEX acquisitions_state ON acquisitions(state);

    CREATE INDEX acquisitions_job ON acquisitions(job_id);

    CREATE INDEX history_title ON history(title_id, ts);

    CREATE INDEX decisions_title ON decisions(title_id, ts);

    CREATE INDEX blocklist_title ON blocklist(title_id);

    CREATE UNIQUE INDEX nzbs_release ON nzbs(kind, COALESCE(tmdb_id, 0), COALESCE(tvdb_id, 0), release_title);

    CREATE INDEX nzbs_tmdb ON nzbs(tmdb_id);

    CREATE INDEX nzbs_tvdb ON nzbs(tvdb_id);

    CREATE INDEX plex_tmdb ON plex_items(tmdb_id);

    CREATE INDEX plex_tvdb ON plex_items(tvdb_id);

    CREATE INDEX changes_rev ON changes(rev);

    -- Every change to what a player or another Spool would show is counted, so that either can ask what has changed since it last looked.

    CREATE TRIGGER titles_added AFTER INSERT ON titles
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('title', NEW.id, NEW.id, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER titles_removed AFTER DELETE ON titles
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('title', OLD.id, OLD.id, (SELECT n FROM revision), 1, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER files_added AFTER INSERT ON files
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('file', NEW.id, NEW.title_id, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER files_changed AFTER UPDATE ON files
    WHEN OLD.data IS NOT NEW.data OR OLD.rel_path IS NOT NEW.rel_path OR OLD.size IS NOT NEW.size OR OLD.title_id IS NOT NEW.title_id
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('file', NEW.id, NEW.title_id, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER files_removed AFTER DELETE ON files
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('file', OLD.id, OLD.title_id, (SELECT n FROM revision), 1, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER episodes_added AFTER INSERT ON episodes
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('episode', NEW.id, NEW.title_id, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER episodes_changed AFTER UPDATE ON episodes
    WHEN OLD.data IS NOT NEW.data OR OLD.monitored IS NOT NEW.monitored OR OLD.file_id IS NOT NEW.file_id OR OLD.air_date IS NOT NEW.air_date
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('episode', NEW.id, NEW.title_id, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER episodes_removed AFTER DELETE ON episodes
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('episode', OLD.id, OLD.title_id, (SELECT n FROM revision), 1, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER acquisitions_added AFTER INSERT ON acquisitions
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('activity', NEW.title_id, NEW.title_id, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER acquisitions_changed AFTER UPDATE ON acquisitions
    WHEN OLD.state IS NOT NEW.state
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('activity', NEW.title_id, NEW.title_id, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER acquisitions_removed AFTER DELETE ON acquisitions
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('activity', OLD.title_id, OLD.title_id, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER profiles_renamed AFTER UPDATE ON profiles
    WHEN OLD.name IS NOT NEW.name
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at)
            SELECT 'activity', id, id, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER) FROM titles WHERE profile_id = NEW.id
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER profiles_removed AFTER DELETE ON profiles
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at)
            SELECT 'activity', id, id, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER) FROM titles WHERE profile_id = OLD.id
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER watched_added AFTER INSERT ON watched
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('watched_' || NEW.kind, NEW.ref_id, CASE NEW.kind WHEN 'episode' THEN COALESCE((SELECT title_id FROM episodes WHERE id = NEW.ref_id), 0) ELSE NEW.ref_id END, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER watched_changed AFTER UPDATE ON watched
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('watched_' || NEW.kind, NEW.ref_id, CASE NEW.kind WHEN 'episode' THEN COALESCE((SELECT title_id FROM episodes WHERE id = NEW.ref_id), 0) ELSE NEW.ref_id END, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER watched_removed AFTER DELETE ON watched
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('watched_' || OLD.kind, OLD.ref_id, CASE OLD.kind WHEN 'episode' THEN COALESCE((SELECT title_id FROM episodes WHERE id = OLD.ref_id), 0) ELSE OLD.ref_id END, (SELECT n FROM revision), 1, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;

    CREATE TRIGGER titles_changed AFTER UPDATE ON titles
    WHEN json_remove(OLD.data, '$.id', '$.last_search_at', '$.last_refresh_at', '$.details_at', '$.anime_checked_at', '$.series_checked') IS NOT json_remove(NEW.data, '$.id', '$.last_search_at', '$.last_refresh_at', '$.details_at', '$.anime_checked_at', '$.series_checked')
        OR OLD.monitored IS NOT NEW.monitored OR OLD.profile_id IS NOT NEW.profile_id OR OLD.path IS NOT NEW.path
    BEGIN
        UPDATE revision SET n = n + 1;
        INSERT INTO changes(kind, ref_id, title_id, rev, removed, at) VALUES ('title', NEW.id, NEW.id, (SELECT n FROM revision), 0, CAST(strftime('%s','now') AS INTEGER))
            ON CONFLICT(kind, ref_id) DO UPDATE SET title_id = excluded.title_id, rev = excluded.rev, removed = excluded.removed, at = excluded.at;
    END;
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
