//! Adding titles, refreshing their metadata, and reconciling the library with what is on disk.

use crate::app::{App, Event};
use crate::db::now;
use crate::models::*;
use anyhow::{anyhow, bail, Result};
use spool_core::parser::{is_video_file, parse_episode_path, parse_movie_path};
use std::path::Path;

#[derive(Debug, Default, serde::Serialize)]
pub struct ScanReport {
    pub titles: usize,
    pub files_found: usize,
    pub files_added: usize,
    pub files_missing: usize,
    pub unmatched: Vec<String>,
}

impl App {
    /// Add a title from a metadata lookup.
    pub async fn add_title(&self, kind: Kind, tmdb_id: Option<u32>, tvmaze_id: Option<u32>, profile_id: i64, monitored: bool, search: bool) -> Result<Title> {
        let g = self.settings.general();
        let (mut title, episodes) = match kind {
            Kind::Movie => (self.meta.movie(&g.tmdb_api_key, tmdb_id.ok_or_else(|| anyhow!("a TMDB id is required"))?).await?, vec![]),
            Kind::Series => self.meta.series(tvmaze_id.ok_or_else(|| anyhow!("a TVmaze id is required"))?).await?,
        };
        if self.db.find_title(kind, title.tmdb_id, title.tvdb_id)?.is_some() {
            bail!("{} is already in the library", title.title);
        }
        title.profile_id = profile_id;
        title.monitored = monitored;
        title.added_at = now();
        title.last_refresh_at = now();
        self.ensure_title_path(&mut title)?;
        self.db.save_title(&mut title)?;
        for mut e in episodes {
            e.title_id = title.id;
            self.db.upsert_episode(&e)?;
        }
        self.emit(Event::Title { id: title.id });
        if search && monitored {
            let (app, id) = (self.clone(), title.id);
            tokio::spawn(async move {
                let scope = if kind == Kind::Movie { crate::acquire::Scope::Movie } else { crate::acquire::Scope::Missing };
                if let Err(e) = app.search_with(id, scope, false, true, true).await {
                    tracing::warn!(error = %e, "search after adding a title failed");
                }
            });
        }
        Ok(title)
    }

    /// Pull fresh metadata for one title. User choices (monitoring, profile, path) are kept.
    pub async fn refresh_title(&self, id: i64) -> Result<()> {
        let mut t = self.db.title(id)?.ok_or_else(|| anyhow!("no such title"))?;
        let g = self.settings.general();
        match t.kind {
            Kind::Movie => {
                let (Some(tmdb), false) = (t.tmdb_id, g.tmdb_api_key.is_empty()) else { return Ok(()) };
                let m = self.meta.movie(&g.tmdb_api_key, tmdb).await?;
                t.overview = m.overview;
                t.status = m.status;
                if m.runtime > 0 {
                    t.runtime = m.runtime;
                }
                t.imdb_id = m.imdb_id.or(t.imdb_id);
                t.original_language = m.original_language.or(t.original_language);
                t.poster = m.poster.or(t.poster);
                t.fanart = m.fanart.or(t.fanart);
                t.genres = m.genres;
                t.in_cinemas = m.in_cinemas.or(t.in_cinemas);
                t.digital_release = m.digital_release.or(t.digital_release);
                t.physical_release = m.physical_release.or(t.physical_release);
                for a in m.alt_titles {
                    if !t.alt_titles.contains(&a) {
                        t.alt_titles.push(a);
                    }
                }
            }
            Kind::Series => {
                let (m, episodes) = match (t.tvmaze_id, t.tvdb_id) {
                    (Some(id), _) => self.meta.series(id).await?,
                    (None, Some(tvdb)) => self.meta.series_by_tvdb(tvdb).await?,
                    _ => return Ok(()),
                };
                t.tvmaze_id = m.tvmaze_id.or(t.tvmaze_id);
                t.original_language = m.original_language.or(t.original_language);
                t.overview = m.overview;
                t.status = m.status;
                t.network = m.network.or(t.network);
                t.poster = t.poster.or(m.poster);
                t.genres = m.genres;
                for a in m.alt_titles {
                    if !t.alt_titles.contains(&a) {
                        t.alt_titles.push(a);
                    }
                }
                let existing = self.db.episodes(id)?;
                for mut e in episodes {
                    // Specials are numbered by the provider in ways that do not line up across sources.
                    if e.season == 0 {
                        continue;
                    }
                    e.title_id = id;
                    match existing.iter().find(|x| x.season == e.season && x.episode == e.episode) {
                        Some(old) => {
                            e.monitored = old.monitored;
                            e.file_id = old.file_id;
                            e.absolute = old.absolute;
                            if e.title.is_empty() {
                                e.title = old.title.clone();
                            }
                        }
                        None => {
                            // New episodes follow their season; a new season follows the latest one.
                            let season_known = t.seasons.iter().any(|s| s.number == e.season);
                            if !season_known {
                                let follow = t.seasons.iter().filter(|s| s.number > 0).max_by_key(|s| s.number).map(|s| s.monitored).unwrap_or(true);
                                t.seasons.push(SeasonInfo { number: e.season, monitored: follow });
                            }
                            e.monitored = t.season_monitored(e.season);
                        }
                    }
                    self.db.upsert_episode(&e)?;
                }
            }
        }
        t.last_refresh_at = now();
        self.db.save_title(&mut t)?;
        self.emit(Event::Title { id });
        Ok(())
    }

    /// Compare one title's folder with the database. Never writes to the media volume.
    pub async fn scan_title(&self, id: i64, report: &mut ScanReport) -> Result<()> {
        let title = self.db.title(id)?.ok_or_else(|| anyhow!("no such title"))?;
        report.titles += 1;
        if title.path.is_empty() || !self.volume_ok() {
            return Ok(());
        }
        let root = Path::new(&title.path);
        let mut on_disk = vec![];
        collect(root, &mut on_disk);
        let on_disk: Vec<String> = on_disk
            .into_iter()
            .filter(|p| !p.to_string_lossy().contains("/.spool-recycle/"))
            .filter_map(|p| p.strip_prefix(root).ok().map(|r| r.to_string_lossy().to_string()))
            .filter(|r| is_video_file(r))
            .collect();
        report.files_found += on_disk.len();
        let known = self.db.files(id)?;

        for f in &known {
            if !on_disk.contains(&f.rel_path) {
                report.files_missing += 1;
                self.db.with(|c| c.execute("DELETE FROM files WHERE id = ?1", [f.id]))?;
                self.db.add_history(Some(id), "file_missing", serde_json::json!({"path": root.join(&f.rel_path).to_string_lossy()}))?;
            }
        }
        let episodes = if title.kind == Kind::Series { self.db.episodes(id)? } else { vec![] };
        for rel in on_disk.iter().filter(|r| !known.iter().any(|f| f.rel_path == **r)) {
            let abs = root.join(rel);
            let size = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
            let name = abs.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let flavor = title.kind.flavor();
            let mut file = MediaFile {
                id: 0,
                title_id: id,
                rel_path: rel.clone(),
                size,
                quality: spool_core::quality::parse_quality(&name, flavor),
                release_group: spool_core::parser::parse_release_group(&name, flavor),
                edition: String::new(),
                media_info: self.probe(&abs, &name).await.ok().flatten(),
                scene_name: None,
                languages: vec![],
                added_at: now(),
                extra: false,
                subtitles: vec![],
            };
            let episode_ids: Vec<i64> = match title.kind {
                Kind::Movie => {
                    if let Some(p) = parse_movie_path(&abs.to_string_lossy()) {
                        file.edition = p.edition;
                    }
                    vec![]
                }
                Kind::Series => {
                    let ids: Vec<i64> = parse_episode_path(&abs.to_string_lossy())
                        .map(|p| episodes.iter().filter(|e| e.season == p.season_number() && p.episode_numbers.contains(&e.episode)).map(|e| e.id).collect())
                        .unwrap_or_default();
                    if ids.is_empty() {
                        report.unmatched.push(abs.to_string_lossy().to_string());
                        continue;
                    }
                    ids
                }
            };
            self.db.tx(|tx| {
                tx.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1,?2,?3,?4)", rusqlite::params![id, file.rel_path, file.size as i64, serde_json::to_string(&file)?])?;
                let fid = tx.last_insert_rowid();
                for e in &episode_ids {
                    tx.execute("UPDATE episodes SET file_id = ?2 WHERE id = ?1", rusqlite::params![e, fid])?;
                }
                Ok(())
            })?;
            report.files_added += 1;
        }
        Ok(())
    }

    pub async fn scan_all(&self) -> Result<ScanReport> {
        let mut report = ScanReport::default();
        if !self.volume_ok() {
            bail!("the media volume is not mounted");
        }
        for t in self.db.titles(None)? {
            if let Err(e) = self.scan_title(t.id, &mut report).await {
                tracing::warn!(title = %t.title, error = %e, "scan failed");
            }
        }
        self.emit(Event::Title { id: 0 });
        Ok(report)
    }

    /// For every library file, compare where it is with where Spool would put it.
    /// Returns (matching, differing as (actual, expected)).
    pub fn check_paths(&self) -> Result<(usize, Vec<(String, String)>)> {
        let mut same = 0;
        let mut differ = vec![];
        for t in self.db.titles(None)? {
            let episodes = if t.kind == Kind::Series { self.db.episodes(t.id)? } else { vec![] };
            for f in self.db.files(t.id)? {
                let ext = f.rel_path.rfind('.').map(|i| f.rel_path[i..].to_string()).unwrap_or_default();
                let mut eps: Vec<Episode> = episodes.iter().filter(|e| e.file_id == Some(f.id)).cloned().collect();
                eps.sort_by_key(|e| (e.season, e.episode));
                let expected = self.library_rel_path(&t, &eps, &f, &ext);
                if expected == f.rel_path {
                    same += 1;
                } else {
                    differ.push((format!("{}/{}", t.path, f.rel_path), format!("{}/{}", t.path, expected)));
                }
            }
        }
        Ok((same, differ))
    }
}

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else {
            out.push(p);
        }
    }
}
