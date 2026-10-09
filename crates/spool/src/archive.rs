//! The NZB archive: every NZB Spool fetches is kept, so a release can be downloaded again later
//! without asking an indexer, and searches still return something when the indexers are down.
//!
//! An archived NZB is only a list of article ids. Whether the articles still exist is a separate
//! question, answered by asking the Usenet servers for a sample of them.

use crate::app::App;
use crate::db::{json, now};
use crate::models::*;
use anyhow::{anyhow, bail, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use spool_core::QualityModel;
use std::io::{Read, Write};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedNzb {
    #[serde(default)]
    pub id: i64,
    pub kind: Kind,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    /// Title of the film or series when this was saved.
    pub name: String,
    pub year: u32,
    /// Where the poster came from, so the archive can still show one after the title is removed.
    #[serde(default)]
    pub poster: Option<String>,
    pub release: Release,
    pub quality: QualityModel,
    /// "S01E02", "Season 1" or empty.
    #[serde(default)]
    pub covers: String,
    /// grabbed, runner_up or manual.
    pub source: String,
    pub fetched_at: i64,
    /// Compressed size on disk.
    #[serde(default)]
    pub stored_bytes: u64,
    #[serde(default)]
    pub checked_at: i64,
    /// Articles found and articles asked for at the last availability check.
    #[serde(default)]
    pub available: Option<(usize, usize)>,
}

impl SavedNzb {
    /// Enough of the post is there to be worth downloading. A few missing articles are normal
    /// and recovery data covers them.
    pub fn usable(&self) -> Option<bool> {
        self.available.map(|(found, asked)| asked > 0 && found * 100 >= asked * 90)
    }
}

/// Minimum age of an availability result before it is asked again during a search.
const RECHECK_SECS: i64 = 24 * 3600;

impl App {
    fn nzb_path(&self, id: i64) -> PathBuf {
        self.data_dir.join("nzb").join(format!("{:03}", id % 1000)).join(format!("{id}.nzb.gz"))
    }

    fn saved_row(r: &rusqlite::Row) -> rusqlite::Result<SavedNzb> {
        let mut n: SavedNzb = json(r.get::<_, String>(1)?)?;
        n.id = r.get(0)?;
        Ok(n)
    }

    /// Keep an NZB. Saving the same release for the same title again is a no-op.
    pub fn archive_save(&self, title: &Title, release: &Release, quality: QualityModel, covers: &str, source: &str, nzb: &[u8]) -> Result<i64> {
        if spool_nntp::nzb::parse(nzb).is_err() {
            bail!("not a valid NZB");
        }
        let existing: Option<i64> = self.db.with(|c| {
            c.query_row(
                "SELECT id FROM nzbs WHERE kind = ?1 AND COALESCE(tmdb_id, 0) = ?2 AND COALESCE(tvdb_id, 0) = ?3 AND release_title = ?4",
                params![title.kind.as_str(), title.tmdb_id.unwrap_or(0), title.tvdb_id.unwrap_or(0), release.title],
                |r| r.get(0),
            )
            .optional()
        })?;
        if let Some(id) = existing {
            // A runner-up that later gets downloaded is recorded as such.
            if source == "grabbed" {
                if let Some(mut n) = self.archive_get(id)? {
                    n.source = "grabbed".into();
                    self.archive_update(&n)?;
                }
            }
            return Ok(id);
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(nzb)?;
        let packed = gz.finish()?;
        let mut clean = release.clone();
        // The indexer's link carries the account's key and is useless once the NZB is here.
        clean.link = String::new();
        let saved = SavedNzb {
            id: 0,
            kind: title.kind,
            tmdb_id: title.tmdb_id,
            tvdb_id: title.tvdb_id,
            imdb_id: title.imdb_id.clone(),
            name: title.title.clone(),
            year: title.year,
            poster: title.poster.clone(),
            release: clean,
            quality,
            covers: covers.to_string(),
            source: source.to_string(),
            fetched_at: now(),
            stored_bytes: packed.len() as u64,
            checked_at: 0,
            available: None,
        };
        let id = self.db.with(|c| {
            c.execute(
                "INSERT INTO nzbs(kind, tmdb_id, tvdb_id, imdb_id, name, year, release_title, size, fetched_at, data) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![saved.kind.as_str(), saved.tmdb_id, saved.tvdb_id, saved.imdb_id, saved.name, saved.year, saved.release.title, saved.release.size as i64, saved.fetched_at, serde_json::to_string(&saved).unwrap_or_default()],
            )?;
            Ok(c.last_insert_rowid())
        })?;
        let path = self.nzb_path(id);
        let write = || -> Result<()> {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, &packed)?;
            std::fs::rename(&tmp, &path)?;
            Ok(())
        };
        if let Err(e) = write() {
            self.db.with(|c| c.execute("DELETE FROM nzbs WHERE id = ?1", [id]))?;
            return Err(e);
        }
        Ok(id)
    }

    fn archive_update(&self, n: &SavedNzb) -> Result<()> {
        self.db.with(|c| c.execute("UPDATE nzbs SET data = ?2 WHERE id = ?1", params![n.id, serde_json::to_string(n).unwrap_or_default()]))?;
        Ok(())
    }

    pub fn archive_get(&self, id: i64) -> Result<Option<SavedNzb>> {
        self.db.with(|c| c.query_row("SELECT id, data FROM nzbs WHERE id = ?1", [id], Self::saved_row).optional())
    }

    /// The NZB itself.
    pub fn archive_read(&self, id: i64) -> Result<Vec<u8>> {
        let packed = std::fs::read(self.nzb_path(id)).map_err(|e| anyhow!("the saved NZB is missing from disk: {e}"))?;
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&packed[..]).read_to_end(&mut out)?;
        Ok(out)
    }

    /// Saved NZBs for one film or series, whether or not it is in the library right now.
    pub fn archive_for(&self, kind: Kind, tmdb_id: Option<u32>, tvdb_id: Option<u32>) -> Result<Vec<SavedNzb>> {
        if tmdb_id.is_none() && tvdb_id.is_none() {
            return Ok(vec![]);
        }
        self.db.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM nzbs WHERE kind = ?1 AND ((?2 IS NOT NULL AND tmdb_id = ?2) OR (?3 IS NOT NULL AND tvdb_id = ?3)) ORDER BY fetched_at DESC")?;
            let rows = s.query_map(params![kind.as_str(), tmdb_id, tvdb_id], Self::saved_row)?;
            rows.collect()
        })
    }

    pub fn archive_all(&self) -> Result<Vec<SavedNzb>> {
        self.db.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM nzbs ORDER BY name, fetched_at DESC")?;
            let rows = s.query_map([], Self::saved_row)?;
            rows.collect()
        })
    }

    pub fn archive_find(&self, title: &Title, release_title: &str) -> Result<Option<SavedNzb>> {
        Ok(self.archive_for(title.kind, title.tmdb_id, title.tvdb_id)?.into_iter().find(|n| n.release.title == release_title))
    }

    pub fn archive_delete(&self, id: i64) -> Result<()> {
        let _ = std::fs::remove_file(self.nzb_path(id));
        self.db.with(|c| c.execute("DELETE FROM nzbs WHERE id = ?1", [id]))?;
        Ok(())
    }

    /// Ask the Usenet servers how much of a saved release still exists, and remember the answer.
    pub async fn archive_check(&self, id: i64) -> Result<SavedNzb> {
        let mut n = self.archive_get(id)?.ok_or_else(|| anyhow!("no such saved NZB"))?;
        let bytes = self.archive_read(id)?;
        let result = self.engine.availability(&bytes, 24).await.map_err(|e| anyhow!("could not check: {e}"))?;
        n.available = Some(result);
        n.checked_at = now();
        self.archive_update(&n)?;
        Ok(n)
    }

    /// Saved releases for a title as search results, each checked for availability if that has
    /// not been done recently. Ones that are gone come back marked, so the verdict can say why.
    pub async fn archive_candidates(&self, title: &Title) -> Vec<SavedNzb> {
        let mut out = vec![];
        for n in self.archive_for(title.kind, title.tmdb_id, title.tvdb_id).unwrap_or_default() {
            let stale = now() - n.checked_at > RECHECK_SECS;
            let n = if stale { self.archive_check(n.id).await.unwrap_or(n) } else { n };
            out.push(n);
        }
        out
    }

    /// Put the film or series a saved release belongs to back in the library, unmonitored.
    /// Nothing is downloaded; the saved releases are then one click away on its page.
    pub async fn archive_restore(&self, id: i64) -> Result<Title> {
        let n = self.archive_get(id)?.ok_or_else(|| anyhow::anyhow!("no such saved release"))?;
        if let Some(t) = self.db.find_title(n.kind, n.tmdb_id, n.tvdb_id)? {
            return Ok(t);
        }
        let g = self.settings.general();
        match n.kind {
            Kind::Movie => self.add_title(Kind::Movie, n.tmdb_id, None, g.default_movie_profile, false, false).await,
            Kind::Series => {
                let tvdb = n.tvdb_id.ok_or_else(|| anyhow::anyhow!("this saved release has no TVDB id to look the series up by"))?;
                let (found, _) = self.meta.series_by_tvdb(tvdb).await?;
                self.add_title(Kind::Series, None, found.tvmaze_id, g.default_series_profile, false, false).await
            }
        }
    }

    /// The link Spool uses for a release that lives in the archive.
    pub fn archive_link(id: i64) -> String {
        format!("archive:{id}")
    }

    pub fn archive_id_from_link(link: &str) -> Option<i64> {
        link.strip_prefix("archive:").and_then(|i| i.parse().ok())
    }

    /// Fetch and save the NZBs of releases that were acceptable but not chosen, so a fallback is
    /// on hand if the chosen one fails or the indexer is unreachable later.
    pub fn archive_runner_ups(&self, title: &Title, runner_ups: Vec<DecisionRecord>) {
        if runner_ups.is_empty() {
            return;
        }
        let (app, title) = (self.clone(), title.clone());
        tokio::spawn(async move {
            for d in runner_ups {
                if d.release.link.is_empty() || App::archive_id_from_link(&d.release.link).is_some() {
                    continue;
                }
                if app.archive_find(&title, &d.release.title).ok().flatten().is_some() {
                    continue;
                }
                // A fallback copy is a nicety; it is not worth an allowance that is running down.
                if app.indexer_budgets().get(&d.release.indexer_id).is_some_and(|b| b.grabs_low()) {
                    continue;
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let _ = app.db.count_indexer(d.release.indexer_id, 0, 1);
                let fetched = async {
                    let resp = app.http.get(&d.release.link).timeout(std::time::Duration::from_secs(150)).send().await?;
                    resp.error_for_status()?.bytes().await
                }
                .await;
                match fetched {
                    Ok(bytes) => {
                        if let Err(e) = app.archive_save(&title, &d.release, d.quality, &d.covers, "runner_up", &bytes) {
                            tracing::debug!(release = %d.release.title, error = %e, "runner-up NZB not saved");
                        }
                    }
                    Err(e) => tracing::debug!(release = %d.release.title, error = %e.without_url(), "runner-up NZB not fetched"),
                }
            }
        });
    }
}
