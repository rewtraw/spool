//! Typed access to the database.

use crate::db::{json, now, Db};
use crate::models::*;
use anyhow::Result;
use rusqlite::{params, OptionalExtension, Row};
use spool_core::profile::QualityProfile;

fn title_row(r: &Row) -> rusqlite::Result<Title> {
    let mut t: Title = json(r.get::<_, String>("data")?)?;
    t.id = r.get("id")?;
    Ok(t)
}

fn episode_row(r: &Row) -> rusqlite::Result<Episode> {
    let mut e: Episode = json(r.get::<_, String>("data")?)?;
    e.id = r.get("id")?;
    e.monitored = r.get::<_, i64>("monitored")? != 0;
    e.file_id = r.get("file_id")?;
    Ok(e)
}

fn file_row(r: &Row) -> rusqlite::Result<MediaFile> {
    let mut f: MediaFile = json(r.get::<_, String>("data")?)?;
    f.id = r.get("id")?;
    Ok(f)
}

fn acq_row(r: &Row) -> rusqlite::Result<Acquisition> {
    let mut a: Acquisition = json(r.get::<_, String>("data")?)?;
    a.id = r.get("id")?;
    Ok(a)
}

impl Db {
    // ------------------------------------------------------------ titles

    pub fn save_title(&self, t: &mut Title) -> Result<i64> {
        t.sort_title = sort_title(&t.title);
        let data = serde_json::to_string(t)?;
        let kind = t.kind.as_str();
        let id = self.with(|c| {
            if t.id == 0 {
                c.execute(
                    "INSERT INTO titles(kind, title, year, monitored, profile_id, path, tmdb_id, tvdb_id, tvmaze_id, imdb_id, data) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![kind, t.title, t.year, t.monitored, t.profile_id, t.path, t.tmdb_id, t.tvdb_id, t.tvmaze_id, t.imdb_id, data],
                )?;
                Ok(c.last_insert_rowid())
            } else {
                c.execute(
                    "UPDATE titles SET title=?2, year=?3, monitored=?4, profile_id=?5, path=?6, tmdb_id=?7, tvdb_id=?8, tvmaze_id=?9, imdb_id=?10, data=?11 WHERE id=?1",
                    params![t.id, t.title, t.year, t.monitored, t.profile_id, t.path, t.tmdb_id, t.tvdb_id, t.tvmaze_id, t.imdb_id, data],
                )?;
                Ok(t.id)
            }
        })?;
        t.id = id;
        Ok(id)
    }

    pub fn title(&self, id: i64) -> Result<Option<Title>> {
        self.with(|c| c.query_row("SELECT id, data FROM titles WHERE id = ?1", [id], title_row).optional())
    }

    pub fn titles(&self, kind: Option<Kind>) -> Result<Vec<Title>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM titles WHERE (?1 IS NULL OR kind = ?1) ORDER BY id")?;
            let rows = s.query_map([kind.map(|k| k.as_str())], title_row)?;
            rows.collect()
        })
    }

    pub fn find_title(&self, kind: Kind, tmdb: Option<u32>, tvdb: Option<u32>) -> Result<Option<Title>> {
        self.with(|c| {
            c.query_row(
                "SELECT id, data FROM titles WHERE kind = ?1 AND ((?2 IS NOT NULL AND tmdb_id = ?2) OR (?3 IS NOT NULL AND tvdb_id = ?3)) LIMIT 1",
                params![kind.as_str(), tmdb, tvdb],
                title_row,
            )
            .optional()
        })
    }

    // ------------------------------------------------------------ anime

    /// Keep what AniList said of these works. A work seen only in passing never replaces
    /// what is known of it from being asked about directly.
    pub fn anime_put(&self, works: &[crate::anime::Media]) -> Result<()> {
        self.with(|c| {
            for m in works {
                c.execute(
                    "INSERT INTO anime(id, depth, fetched_at, data) VALUES (?1,?2,?3,?4)
                     ON CONFLICT(id) DO UPDATE SET depth = excluded.depth, fetched_at = excluded.fetched_at, data = excluded.data WHERE excluded.depth >= anime.depth",
                    params![m.id, m.depth, now(), serde_json::to_string(m).unwrap_or_default()],
                )?;
            }
            Ok(())
        })?;
        Ok(())
    }

    pub fn anime(&self, id: u32) -> Result<Option<crate::anime::Media>> {
        let data: Option<String> = self.with(|c| c.query_row("SELECT data FROM anime WHERE id = ?1", [id], |r| r.get(0)).optional())?;
        Ok(data.and_then(|d| serde_json::from_str(&d).ok()))
    }

    pub fn film_set_put(&self, set: &crate::anime::FilmSet) -> Result<()> {
        self.with(|c| c.execute("INSERT OR REPLACE INTO film_sets(id, fetched_at, data) VALUES (?1,?2,?3)", params![set.id, now(), serde_json::to_string(set).unwrap_or_default()]))?;
        Ok(())
    }

    pub fn film_sets(&self) -> Result<Vec<crate::anime::FilmSet>> {
        let rows: Vec<String> = self.with(|c| {
            let mut s = c.prepare("SELECT data FROM film_sets ORDER BY id")?;
            let rows = s.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;
        Ok(rows.iter().filter_map(|d| serde_json::from_str(d).ok()).collect())
    }

    /// The film collections read since then.
    pub fn film_sets_since(&self, since: i64) -> Result<Vec<u32>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id FROM film_sets WHERE fetched_at >= ?1")?;
            let rows = s.query_map([since], |r| r.get::<_, u32>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn anime_all(&self) -> Result<Vec<crate::anime::Media>> {
        let rows: Vec<String> = self.with(|c| {
            let mut s = c.prepare("SELECT data FROM anime ORDER BY id")?;
            let rows = s.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;
        Ok(rows.iter().filter_map(|d| serde_json::from_str(d).ok()).collect())
    }

    pub fn delete_title(&self, id: i64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM titles WHERE id = ?1", [id]))?;
        Ok(())
    }

    // ------------------------------------------------------------ episodes

    /// Insert or update by (title, season, episode). Monitoring and file links are kept on update.
    pub fn upsert_episode(&self, e: &Episode) -> Result<i64> {
        let data = serde_json::to_string(e)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO episodes(title_id, season, episode, air_date, monitored, file_id, data) VALUES (?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(title_id, season, episode) DO UPDATE SET air_date = excluded.air_date, data = excluded.data",
                params![e.title_id, e.season, e.episode, e.air_date, e.monitored, e.file_id, data],
            )?;
            c.query_row("SELECT id FROM episodes WHERE title_id=?1 AND season=?2 AND episode=?3", params![e.title_id, e.season, e.episode], |r| r.get(0))
        })
    }

    pub fn episodes(&self, title_id: i64) -> Result<Vec<Episode>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, monitored, file_id, data FROM episodes WHERE title_id = ?1 ORDER BY season, episode")?;
            let rows = s.query_map([title_id], episode_row)?;
            rows.collect()
        })
    }

    pub fn episode(&self, id: i64) -> Result<Option<Episode>> {
        self.with(|c| c.query_row("SELECT id, monitored, file_id, data FROM episodes WHERE id = ?1", [id], episode_row).optional())
    }

    pub fn episodes_between(&self, from: &str, to: &str) -> Result<Vec<Episode>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, monitored, file_id, data FROM episodes WHERE air_date >= ?1 AND air_date <= ?2 ORDER BY air_date, season, episode")?;
            let rows = s.query_map([from, to], episode_row)?;
            rows.collect()
        })
    }

    pub fn set_episode_monitored(&self, ids: &[i64], monitored: bool) -> Result<()> {
        self.tx(|tx| {
            for id in ids {
                tx.execute("UPDATE episodes SET monitored = ?2 WHERE id = ?1", params![id, monitored])?;
            }
            Ok(())
        })
    }

    pub fn set_season_monitored(&self, title_id: i64, season: u32, monitored: bool) -> Result<()> {
        self.with(|c| c.execute("UPDATE episodes SET monitored = ?3 WHERE title_id = ?1 AND season = ?2", params![title_id, season, monitored]))?;
        Ok(())
    }

    pub fn delete_episode(&self, id: i64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM episodes WHERE id = ?1", [id]))?;
        Ok(())
    }

    // ------------------------------------------------------------ files

    pub fn files(&self, title_id: i64) -> Result<Vec<MediaFile>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM files WHERE title_id = ?1 ORDER BY rel_path")?;
            let rows = s.query_map([title_id], file_row)?;
            rows.collect()
        })
    }

    pub fn file(&self, id: i64) -> Result<Option<MediaFile>> {
        self.with(|c| c.query_row("SELECT id, data FROM files WHERE id = ?1", [id], file_row).optional())
    }

    pub fn all_files(&self) -> Result<Vec<MediaFile>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM files ORDER BY title_id, rel_path")?;
            let rows = s.query_map([], file_row)?;
            rows.collect()
        })
    }

    pub fn all_episodes(&self) -> Result<Vec<Episode>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, monitored, file_id, data FROM episodes ORDER BY title_id, season, episode")?;
            let rows = s.query_map([], episode_row)?;
            rows.collect()
        })
    }

    /// Replace what is recorded about a file, keeping its place in the library.
    pub fn save_file(&self, f: &MediaFile) -> Result<()> {
        let data = serde_json::to_string(f)?;
        self.with(|c| c.execute("UPDATE files SET rel_path = ?2, size = ?3, data = ?4 WHERE id = ?1", params![f.id, f.rel_path, f.size as i64, data]))?;
        Ok(())
    }

    // ------------------------------------------------------------ watched

    /// Everything one account has watched or part-watched, keyed by kind ("movie", "series",
    /// "episode") and id.
    pub fn watched_all(&self, account: i64) -> Result<std::collections::HashMap<(String, i64), Watched>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT kind, ref_id, position, played, play_count, favorite, last_played_at, updated_at FROM watched WHERE account_id = ?1")?;
            let rows = s.query_map([account], |r| {
                Ok((
                    (r.get::<_, String>(0)?, r.get::<_, i64>(1)?),
                    Watched { position: r.get(2)?, played: r.get(3)?, play_count: r.get(4)?, favorite: r.get(5)?, last_played_at: r.get(6)?, updated_at: r.get(7)? },
                ))
            })?;
            rows.collect()
        })
    }

    /// What anybody has played through, by kind and id.
    pub fn played_by_anyone(&self) -> Result<std::collections::HashSet<(String, i64)>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT DISTINCT kind, ref_id FROM watched WHERE played")?;
            let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })
    }

    pub fn watched(&self, account: i64, kind: &str, id: i64) -> Result<Watched> {
        Ok(self
            .with(|c| {
                c.query_row("SELECT position, played, play_count, favorite, last_played_at, updated_at FROM watched WHERE account_id = ?1 AND kind = ?2 AND ref_id = ?3", params![account, kind, id], |r| {
                    Ok(Watched { position: r.get(0)?, played: r.get(1)?, play_count: r.get(2)?, favorite: r.get(3)?, last_played_at: r.get(4)?, updated_at: r.get(5)? })
                })
                .optional()
            })?
            .unwrap_or_default())
    }

    pub fn save_watched(&self, account: i64, kind: &str, id: i64, w: &Watched) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO watched(account_id, kind, ref_id, position, played, play_count, favorite, last_played_at, updated_at) VALUES (?9,?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(account_id, kind, ref_id) DO UPDATE SET position = ?3, played = ?4, play_count = ?5, favorite = ?6, last_played_at = ?7, updated_at = ?8",
                params![kind, id, w.position, w.played, w.play_count, w.favorite, w.last_played_at, now(), account],
            )
        })?;
        Ok(())
    }

    // ------------------------------------------------------------ accounts

    pub fn accounts(&self) -> Result<Vec<Account>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, name, password, enabled, created_at FROM accounts ORDER BY id")?;
            let rows = s.query_map([], |r| Ok(Account { id: r.get(0)?, name: r.get(1)?, password: r.get(2)?, enabled: r.get(3)?, created_at: r.get(4)? }))?;
            rows.collect()
        })
    }

    pub fn account(&self, id: i64) -> Result<Option<Account>> {
        Ok(self.accounts()?.into_iter().find(|a| a.id == id))
    }

    /// Add an account. The password is given already hashed, or empty.
    pub fn add_account(&self, name: &str, password: &str) -> Result<i64> {
        self.with(|c| {
            c.execute("INSERT INTO accounts(name, password, created_at) VALUES (?1, ?2, ?3)", params![name, password, now()])?;
            Ok(c.last_insert_rowid())
        })
    }

    pub fn save_account(&self, a: &Account) -> Result<()> {
        self.with(|c| c.execute("UPDATE accounts SET name = ?2, password = ?3, enabled = ?4 WHERE id = ?1", params![a.id, a.name, a.password, a.enabled]))?;
        Ok(())
    }

    /// Remove an account, and with it its players, what it watched and what it played.
    pub fn remove_account(&self, id: i64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM accounts WHERE id = ?1", [id]))?;
        Ok(())
    }

    /// The players signed in, each with the account it is signed in to.
    pub fn devices(&self) -> Result<Vec<Device>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT token, account_id, name, client, created_at, seen_at FROM devices ORDER BY seen_at DESC, created_at DESC")?;
            let rows = s.query_map([], |r| Ok(Device { token: r.get(0)?, account_id: r.get(1)?, name: r.get(2)?, client: r.get(3)?, created_at: r.get(4)?, seen_at: r.get(5)? }))?;
            rows.collect()
        })
    }

    pub fn add_device(&self, d: &Device) -> Result<()> {
        self.with(|c| c.execute("INSERT INTO devices(token, account_id, name, client, created_at, seen_at) VALUES (?1,?2,?3,?4,?5,?5)", params![d.token, d.account_id, d.name, d.client, d.created_at]))?;
        Ok(())
    }

    pub fn device_seen(&self, token: &str) -> Result<()> {
        self.with(|c| c.execute("UPDATE devices SET seen_at = ?2 WHERE token = ?1", params![token, now()]))?;
        Ok(())
    }

    /// Sign players out: one by its token, or every player of an account.
    pub fn remove_devices(&self, token: Option<&str>, account: Option<i64>) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM devices WHERE token = ?1 OR account_id = ?2", params![token, account]))?;
        Ok(())
    }

    // ------------------------------------------------------------ plays

    /// Begin the record of a sitting. Returns its number.
    pub fn play_begin(&self, p: &Play) -> Result<i64> {
        self.with(|c| {
            c.execute(
                "INSERT INTO plays(account_id, kind, ref_id, title_id, title, device, source, started_at, ended_at, position, finished) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8,?9,?10)",
                params![p.account_id, p.kind, p.ref_id, p.title_id, p.title, p.device, p.source, p.started_at, p.position, p.finished],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Carry a sitting on: more seconds watched, where it has got to, and where it comes from now.
    pub fn play_on(&self, id: i64, seconds: f64, position: f64, source: &str, finished: bool) -> Result<()> {
        self.with(|c| c.execute("UPDATE plays SET seconds = seconds + ?2, position = ?3, source = ?4, finished = finished OR ?5, ended_at = ?6 WHERE id = ?1", params![id, seconds, position, source, finished, now()]))?;
        Ok(())
    }

    /// The latest sittings, newest first: everyone's, or one account's.
    pub fn plays(&self, account: Option<i64>, limit: u32) -> Result<Vec<Play>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, account_id, kind, ref_id, title_id, title, device, source, started_at, ended_at, seconds, position, finished FROM plays WHERE ?1 IS NULL OR account_id = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2")?;
            let rows = s.query_map(params![account, limit], |r| {
                Ok(Play { id: r.get(0)?, account_id: r.get(1)?, kind: r.get(2)?, ref_id: r.get(3)?, title_id: r.get(4)?, title: r.get(5)?, device: r.get(6)?, source: r.get(7)?, started_at: r.get(8)?, ended_at: r.get(9)?, seconds: r.get(10)?, position: r.get(11)?, finished: r.get(12)? })
            })?;
            rows.collect()
        })
    }

    /// For each account: how many sittings, seconds watched in all and since a time, and
    /// when it last played anything.
    pub fn play_totals(&self, since: i64) -> Result<std::collections::HashMap<i64, PlayTotals>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT account_id, COUNT(*), SUM(seconds), SUM(CASE WHEN started_at >= ?1 THEN seconds ELSE 0 END), MAX(ended_at), COUNT(DISTINCT title_id) FROM plays GROUP BY account_id")?;
            let rows = s.query_map([since], |r| Ok((r.get(0)?, PlayTotals { plays: r.get(1)?, seconds: r.get(2)?, seconds_lately: r.get(3)?, last_at: r.get(4)?, titles: r.get(5)? })))?;
            rows.collect()
        })
    }

    // ------------------------------------------------------------ changes

    /// How many changes the library has seen. It only goes up.
    pub fn revision(&self) -> Result<i64> {
        self.with(|c| c.query_row("SELECT n FROM revision", [], |r| r.get(0)))
    }

    /// Where each changed row stands now, oldest change first: one entry per title, episode or
    /// file however often it changed. `kinds` narrows it; empty means everything.
    pub fn changes_since(&self, since: i64, kinds: &[&str], limit: u32) -> Result<Vec<Change>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT kind, ref_id, title_id, rev, removed, at FROM changes WHERE rev > ?1 ORDER BY rev LIMIT ?2")?;
            let rows = s.query_map(params![since, if kinds.is_empty() { limit as i64 } else { -1 }], |r| {
                Ok(Change { kind: r.get(0)?, id: r.get(1)?, title_id: r.get(2)?, rev: r.get(3)?, removed: r.get::<_, i64>(4)? != 0, at: r.get(5)? })
            })?;
            let mut out = vec![];
            for row in rows {
                let row = row?;
                if kinds.is_empty() || kinds.contains(&row.kind.as_str()) {
                    out.push(row);
                    if out.len() as u32 >= limit {
                        break;
                    }
                }
            }
            Ok(out)
        })
    }

    /// The oldest count a caller can still ask from and be told everything, removals included.
    pub fn changes_floor(&self) -> Result<i64> {
        Ok(self.get_setting::<i64>("changes_floor"))
    }

    /// Forget removals older than `before`. Anyone who last looked before the newest of them
    /// is told to read the whole library again.
    pub fn prune_changes(&self, before: i64) -> Result<usize> {
        let newest: Option<i64> = self.with(|c| c.query_row("SELECT MAX(rev) FROM changes WHERE removed = 1 AND at < ?1", [before], |r| r.get(0)))?;
        let Some(newest) = newest else { return Ok(0) };
        self.set_setting("changes_floor", &newest.max(self.changes_floor()?))?;
        self.with(|c| c.execute("DELETE FROM changes WHERE removed = 1 AND at < ?1", [before]))
    }

    /// Every file path in the library, keyed for "does this title have a file" checks.
    pub fn file_counts(&self) -> Result<std::collections::HashMap<i64, (u32, u64)>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT title_id, COUNT(*), COALESCE(SUM(size), 0) FROM files GROUP BY title_id")?;
            let rows = s.query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, u32>(1)?, r.get::<_, i64>(2)? as u64))))?;
            rows.collect()
        })
    }

    /// How many compact versions each title has, and how big they are together.
    pub fn compact_counts(&self) -> Result<std::collections::HashMap<i64, (u32, u64)>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT title_id, COUNT(*), COALESCE(SUM(size), 0) FROM files WHERE json_extract(data, '$.compact_of') IS NOT NULL GROUP BY title_id")?;
            let rows = s.query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, u32>(1)?, r.get::<_, i64>(2)? as u64))))?;
            rows.collect()
        })
    }

    pub fn episode_counts(&self) -> Result<std::collections::HashMap<i64, (u32, u32)>> {
        self.with(|c| {
            let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
            let mut s = c.prepare(
                "SELECT title_id, SUM(CASE WHEN season > 0 AND air_date IS NOT NULL AND air_date <= ?1 THEN 1 ELSE 0 END), SUM(CASE WHEN season > 0 AND file_id IS NOT NULL THEN 1 ELSE 0 END) FROM episodes GROUP BY title_id",
            )?;
            let rows = s.query_map([today], |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, u32>(1)?, r.get::<_, u32>(2)?))))?;
            rows.collect()
        })
    }

    // ------------------------------------------------------------ profiles

    pub fn profiles(&self) -> Result<Vec<QualityProfile>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM profiles ORDER BY kind, id")?;
            let rows = s.query_map([], |r| {
                let mut p: QualityProfile = json(r.get::<_, String>(1)?)?;
                p.id = r.get(0)?;
                Ok(p)
            })?;
            rows.collect()
        })
    }

    pub fn profile(&self, id: i64) -> Result<Option<QualityProfile>> {
        Ok(self.profiles()?.into_iter().find(|p| p.id == id))
    }

    pub fn save_profile(&self, p: &mut QualityProfile) -> Result<i64> {
        let data = serde_json::to_string(p)?;
        let id = self.with(|c| {
            if p.id == 0 {
                c.execute("INSERT INTO profiles(kind, name, data) VALUES (?1,?2,?3)", params![p.kind, p.name, data])?;
                Ok(c.last_insert_rowid())
            } else {
                c.execute("UPDATE profiles SET kind=?2, name=?3, data=?4 WHERE id=?1", params![p.id, p.kind, p.name, data])?;
                Ok(p.id)
            }
        })?;
        p.id = id;
        Ok(id)
    }

    pub fn delete_profile(&self, id: i64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM profiles WHERE id = ?1", [id]))?;
        Ok(())
    }

    // ------------------------------------------------------------ indexers

    pub fn indexers(&self) -> Result<Vec<Indexer>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM indexers ORDER BY id")?;
            let rows = s.query_map([], |r| {
                let mut i: Indexer = json(r.get::<_, String>(1)?)?;
                i.id = r.get(0)?;
                Ok(i)
            })?;
            rows.collect()
        })
    }

    /// Add to today's count of requests made to an indexer and NZBs fetched from it.
    pub fn count_indexer(&self, indexer_id: i64, requests: u32, grabs: u32) -> Result<()> {
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        self.with(|c| {
            c.execute(
                "INSERT INTO indexer_usage(day, indexer_id, requests, grabs) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(day, indexer_id) DO UPDATE SET requests = requests + ?3, grabs = grabs + ?4",
                params![day, indexer_id, requests, grabs],
            )
        })?;
        Ok(())
    }

    /// Today's (requests, grabs) per indexer.
    pub fn indexer_usage_today(&self) -> Result<std::collections::HashMap<i64, (u32, u32)>> {
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        self.with(|c| {
            let mut s = c.prepare("SELECT indexer_id, requests, grabs FROM indexer_usage WHERE day = ?1")?;
            let rows = s.query_map([day], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?;
            rows.collect()
        })
    }

    pub fn save_indexer(&self, i: &mut Indexer) -> Result<i64> {
        let data = serde_json::to_string(i)?;
        let id = self.with(|c| {
            if i.id == 0 {
                c.execute("INSERT INTO indexers(name, data) VALUES (?1,?2)", params![i.name, data])?;
                Ok(c.last_insert_rowid())
            } else {
                c.execute("UPDATE indexers SET name=?2, data=?3 WHERE id=?1", params![i.id, i.name, data])?;
                Ok(i.id)
            }
        })?;
        i.id = id;
        Ok(id)
    }

    pub fn delete_indexer(&self, id: i64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM indexers WHERE id = ?1", [id]))?;
        Ok(())
    }

    // ------------------------------------------------------------ acquisitions

    pub fn save_acquisition(&self, a: &mut Acquisition) -> Result<i64> {
        a.updated_at = now();
        if a.created_at == 0 {
            a.created_at = a.updated_at;
        }
        let data = serde_json::to_string(a)?;
        let id = self.with(|c| {
            if a.id == 0 {
                c.execute(
                    "INSERT INTO acquisitions(title_id, state, job_id, created_at, updated_at, data) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![a.title_id, a.state.as_str(), a.job_id, a.created_at, a.updated_at, data],
                )?;
                Ok(c.last_insert_rowid())
            } else {
                c.execute("UPDATE acquisitions SET state=?2, job_id=?3, updated_at=?4, data=?5 WHERE id=?1", params![a.id, a.state.as_str(), a.job_id, a.updated_at, data])?;
                Ok(a.id)
            }
        })?;
        a.id = id;
        Ok(id)
    }

    /// Hand a download to another title, as when a film becomes a series.
    pub fn move_acquisition(&self, a: &mut Acquisition, title_id: i64) -> Result<()> {
        a.title_id = title_id;
        self.with(|c| c.execute("UPDATE acquisitions SET title_id = ?2 WHERE id = ?1", params![a.id, title_id]))?;
        self.save_acquisition(a)?;
        Ok(())
    }

    pub fn acquisition(&self, id: i64) -> Result<Option<Acquisition>> {
        self.with(|c| c.query_row("SELECT id, data FROM acquisitions WHERE id = ?1", [id], acq_row).optional())
    }

    pub fn acquisition_by_job(&self, job_id: &str) -> Result<Option<Acquisition>> {
        self.with(|c| c.query_row("SELECT id, data FROM acquisitions WHERE job_id = ?1 ORDER BY id DESC LIMIT 1", [job_id], acq_row).optional())
    }

    pub fn active_acquisitions(&self) -> Result<Vec<Acquisition>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM acquisitions WHERE state IN ('downloading','importing','import_blocked') ORDER BY id")?;
            let rows = s.query_map([], acq_row)?;
            rows.collect()
        })
    }

    pub fn recent_acquisitions(&self, limit: u32) -> Result<Vec<Acquisition>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM acquisitions ORDER BY updated_at DESC, id DESC LIMIT ?1")?;
            let rows = s.query_map([limit], acq_row)?;
            rows.collect()
        })
    }

    /// How many downloads were imported at or after `since`, and their combined size in bytes.
    pub fn imported_since(&self, since: i64) -> Result<(u64, u64)> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(json_extract(data, '$.release.size')), 0) FROM acquisitions WHERE state = 'imported' AND updated_at >= ?1",
                [since],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
            )
        })
    }

    pub fn title_acquisitions(&self, title_id: i64) -> Result<Vec<Acquisition>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM acquisitions WHERE title_id = ?1 ORDER BY id DESC LIMIT 50")?;
            let rows = s.query_map([title_id], acq_row)?;
            rows.collect()
        })
    }

    // ------------------------------------------------------------ decisions

    pub fn record_decisions(&self, records: &[DecisionRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        self.tx(|tx| {
            let ts = now();
            for d in records {
                // One row per (title, release): the latest verdict replaces the earlier one.
                tx.execute("DELETE FROM decisions WHERE title_id = ?1 AND release_title = ?2", params![d.title_id, d.release.title])?;
                tx.execute(
                    "INSERT INTO decisions(ts, title_id, release_title, accepted, data) VALUES (?1,?2,?3,?4,?5)",
                    params![ts, d.title_id, d.release.title, d.accepted, serde_json::to_string(d)?],
                )?;
            }
            let ids: std::collections::HashSet<i64> = records.iter().map(|d| d.title_id).collect();
            for id in ids {
                tx.execute(
                    "DELETE FROM decisions WHERE title_id = ?1 AND id NOT IN (SELECT id FROM decisions WHERE title_id = ?1 ORDER BY ts DESC, id DESC LIMIT 300)",
                    [id],
                )?;
            }
            Ok(())
        })
    }

    pub fn decisions(&self, title_id: i64) -> Result<Vec<DecisionRecord>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, ts, data FROM decisions WHERE title_id = ?1 ORDER BY ts DESC, accepted DESC, id DESC")?;
            let rows = s.query_map([title_id], |r| {
                let mut d: DecisionRecord = json(r.get::<_, String>(2)?)?;
                d.id = r.get(0)?;
                d.ts = r.get(1)?;
                Ok(d)
            })?;
            rows.collect()
        })
    }

    pub fn decision(&self, id: i64) -> Result<Option<DecisionRecord>> {
        self.with(|c| {
            c.query_row("SELECT id, ts, data FROM decisions WHERE id = ?1", [id], |r| {
                let mut d: DecisionRecord = json(r.get::<_, String>(2)?)?;
                d.id = r.get(0)?;
                d.ts = r.get(1)?;
                Ok(d)
            })
            .optional()
        })
    }

    // ------------------------------------------------------------ history, blocklist, attention

    pub fn add_history(&self, title_id: Option<i64>, kind: &str, data: serde_json::Value) -> Result<()> {
        self.with(|c| c.execute("INSERT INTO history(ts, title_id, kind, data) VALUES (?1,?2,?3,?4)", params![now(), title_id, kind, data.to_string()]))?;
        Ok(())
    }

    pub fn history(&self, title_id: Option<i64>, limit: u32) -> Result<Vec<HistoryEntry>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, ts, title_id, kind, data FROM history WHERE (?1 IS NULL OR title_id = ?1) ORDER BY ts DESC, id DESC LIMIT ?2")?;
            let rows = s.query_map(params![title_id, limit], |r| {
                Ok(HistoryEntry { id: r.get(0)?, ts: r.get(1)?, title_id: r.get(2)?, kind: r.get(3)?, data: json(r.get::<_, String>(4)?)? })
            })?;
            rows.collect()
        })
    }

    pub fn add_blocklist(&self, title_id: i64, guid: &str, release_title: &str, reason: &str) -> Result<()> {
        self.with(|c| c.execute("INSERT INTO blocklist(ts, title_id, guid, release_title, reason) VALUES (?1,?2,?3,?4,?5)", params![now(), title_id, guid, release_title, reason]))?;
        Ok(())
    }

    pub fn blocklisted(&self, title_id: i64) -> Result<Vec<(String, String)>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT guid, release_title FROM blocklist WHERE title_id = ?1")?;
            let rows = s.query_map([title_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })
    }

    pub fn blocklist(&self) -> Result<Vec<serde_json::Value>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT b.id, b.ts, b.title_id, b.release_title, b.reason, t.title FROM blocklist b LEFT JOIN titles t ON t.id = b.title_id ORDER BY b.ts DESC LIMIT 500")?;
            let rows = s.query_map([], |r| {
                Ok(serde_json::json!({"id": r.get::<_, i64>(0)?, "ts": r.get::<_, i64>(1)?, "title_id": r.get::<_, i64>(2)?, "release_title": r.get::<_, String>(3)?, "reason": r.get::<_, String>(4)?, "title": r.get::<_, Option<String>>(5)?}))
            })?;
            rows.collect()
        })
    }

    pub fn delete_blocklist(&self, id: i64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM blocklist WHERE id = ?1", [id]))?;
        Ok(())
    }

    pub fn add_attention(&self, kind: &str, title_id: Option<i64>, acquisition_id: Option<i64>, message: &str, data: serde_json::Value) -> Result<i64> {
        self.with(|c| {
            c.execute(
                "INSERT INTO attention(ts, kind, title_id, acquisition_id, message, data) VALUES (?1,?2,?3,?4,?5,?6)",
                params![now(), kind, title_id, acquisition_id, message, data.to_string()],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Keep exactly one open item of a kind, with this message; `None` closes it.
    /// Returns whether anything changed.
    pub fn set_standing_attention(&self, kind: &str, message: Option<&str>, data: serde_json::Value) -> Result<bool> {
        let open: Option<(i64, String)> = self.with(|c| c.query_row("SELECT id, message FROM attention WHERE kind = ?1 AND resolved_at IS NULL ORDER BY id DESC LIMIT 1", [kind], |r| Ok((r.get(0)?, r.get(1)?))).optional())?;
        match (open, message) {
            (None, None) => Ok(false),
            (Some((_, old)), Some(new)) if old == new => Ok(false),
            (Some((id, _)), Some(new)) => {
                self.with(|c| c.execute("UPDATE attention SET message = ?2, data = ?3 WHERE id = ?1", params![id, new, data.to_string()]))?;
                Ok(true)
            }
            (Some(_), None) => {
                self.with(|c| c.execute("UPDATE attention SET resolved_at = ?2 WHERE kind = ?1 AND resolved_at IS NULL", params![kind, now()]))?;
                Ok(true)
            }
            (None, Some(new)) => {
                // Dismissed with exactly this message: it stays dismissed until something changes.
                let last: Option<String> = self.with(|c| c.query_row("SELECT message FROM attention WHERE kind = ?1 ORDER BY id DESC LIMIT 1", [kind], |r| r.get(0)).optional())?;
                if last.as_deref() == Some(new) {
                    return Ok(false);
                }
                self.add_attention(kind, None, None, new, data)?;
                Ok(true)
            }
        }
    }

    pub fn attention(&self, include_resolved: bool) -> Result<Vec<Attention>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, ts, kind, title_id, acquisition_id, message, data, resolved_at FROM attention WHERE (?1 OR resolved_at IS NULL) ORDER BY ts DESC LIMIT 200")?;
            let rows = s.query_map([include_resolved], |r| {
                Ok(Attention { id: r.get(0)?, ts: r.get(1)?, kind: r.get(2)?, title_id: r.get(3)?, acquisition_id: r.get(4)?, message: r.get(5)?, data: json(r.get::<_, String>(6)?)?, resolved_at: r.get(7)? })
            })?;
            rows.collect()
        })
    }

    pub fn attention_item(&self, id: i64) -> Result<Option<Attention>> {
        Ok(self.attention(true)?.into_iter().find(|a| a.id == id))
    }

    pub fn resolve_attention(&self, id: i64) -> Result<()> {
        self.with(|c| c.execute("UPDATE attention SET resolved_at = ?2 WHERE id = ?1", params![id, now()]))?;
        Ok(())
    }

    /// Close "download failed" items for a title once what they were about is in the library:
    /// the movie, or every episode the failed download would have brought.
    pub fn resolve_attention_arrived(&self, title_id: i64, episode_ids: &[i64]) -> Result<()> {
        let open: Vec<(i64, String)> = self.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM attention WHERE title_id = ?1 AND kind = 'download_failed' AND resolved_at IS NULL")?;
            let rows = s.query_map([title_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })?;
        for (id, data) in open {
            let data: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
            let wanted: Vec<i64> = data["episode_ids"].as_array().map(|a| a.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
            // An item with no episodes recorded is about the title as a whole.
            let done = if wanted.is_empty() { episode_ids.is_empty() } else { wanted.iter().all(|w| episode_ids.contains(w)) };
            if done {
                self.resolve_attention(id)?;
            }
        }
        Ok(())
    }

    pub fn resolve_attention_for(&self, acquisition_id: i64) -> Result<()> {
        self.with(|c| c.execute("UPDATE attention SET resolved_at = ?2 WHERE acquisition_id = ?1 AND resolved_at IS NULL", params![acquisition_id, now()]))?;
        Ok(())
    }
}

/// Lower-case title without a leading article, for ordering.
pub fn sort_title(title: &str) -> String {
    let lower = title.trim_start_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    for article in ["the ", "a ", "an "] {
        if let Some(rest) = lower.strip_prefix(article) {
            return rest.trim_start_matches(|c: char| !c.is_alphanumeric()).to_string();
        }
    }
    lower
}
