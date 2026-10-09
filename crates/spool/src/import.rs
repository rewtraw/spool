//! Moving a finished download into the library.
//!
//! A file move and a database commit cannot be one atomic step, so every import is written to a
//! journal first and carried out in steps that are each safe to repeat. After a crash, `recover`
//! finishes or re-runs whatever the journal says was in flight.

use crate::app::{App, Event};
use crate::db::now;
use crate::models::*;
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use spool_core::mediainfo::MediaInfo;
use spool_core::naming::{self, EpisodeNameInput, MovieNameInput};
use spool_core::parser::{is_video_file, parse_episode_path, parse_movie_title};
use spool_core::quality::{parse_quality_name, Flavor};
use spool_core::{Quality, QualityModel};
use std::path::{Path, PathBuf};

/// One file's move into the library, as recorded in the journal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub acquisition_id: Option<i64>,
    pub title_id: i64,
    pub episode_ids: Vec<i64>,
    pub src: String,
    pub dst: String,
    /// Existing library files this one replaces: (file id, absolute path, recycle path).
    pub replaces: Vec<(i64, String, String)>,
    pub file: MediaFile,
}

#[derive(Debug)]
pub enum Blocked {
    /// Could not be imported without a decision from the user.
    Needs(String),
    /// Temporary condition; try again later.
    Retry(String),
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if dir.is_file() {
        out.push(dir.to_path_buf());
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Video files in a download, biggest first, without samples.
pub fn video_files(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut all = vec![];
    walk(root, &mut all);
    let mut vids: Vec<(PathBuf, u64)> = all
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(is_video_file))
        .filter_map(|p| std::fs::metadata(&p).ok().map(|m| (p, m.len())))
        .collect();
    vids.sort_by(|a, b| b.1.cmp(&a.1));
    let biggest = vids.first().map(|v| v.1).unwrap_or(0);
    vids.retain(|(p, size)| {
        let lower = p.to_string_lossy().to_lowercase();
        let sampleish = lower.contains("sample") || lower.contains("/proof/") || lower.contains("-trailer");
        !(sampleish && (*size < biggest / 5 || *size < 100 << 20) && *size != biggest)
    });
    vids
}

fn move_file(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    // Different volume: copy to a temporary name, check the size, then swap it in.
    let tmp = dst.with_extension(format!("{}.spool-partial", dst.extension().and_then(|e| e.to_str()).unwrap_or("tmp")));
    let copied = std::fs::copy(src, &tmp).with_context(|| format!("copying to {}", tmp.display()))?;
    let expect = std::fs::metadata(src)?.len();
    if copied != expect {
        let _ = std::fs::remove_file(&tmp);
        bail!("copy was {copied} bytes, expected {expect}");
    }
    std::fs::File::open(&tmp)?.sync_all()?;
    std::fs::rename(&tmp, dst)?;
    std::fs::remove_file(src)?;
    Ok(())
}

impl App {
    fn ffprobe_path(&self) -> Option<PathBuf> {
        let configured = self.settings.general().ffprobe_path;
        if !configured.is_empty() {
            return Some(PathBuf::from(configured));
        }
        ["/opt/homebrew/bin/ffprobe", "/usr/local/bin/ffprobe", "/usr/bin/ffprobe"].iter().map(PathBuf::from).find(|p| p.is_file()).or_else(|| {
            std::env::var("PATH").ok().and_then(|p| p.split(':').map(|d| Path::new(d).join("ffprobe")).find(|p| p.is_file()))
        })
    }

    /// Probe a file. Ok(None) when ffprobe is not installed; an error when the file is not playable video.
    pub async fn probe(&self, path: &Path, scene_name: &str) -> Result<Option<MediaInfo>> {
        let Some(tool) = self.ffprobe_path() else { return Ok(None) };
        let out = tokio::process::Command::new(tool)
            .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams", "-show_frames", "-read_intervals", "%+#1", "-select_streams", "v:0", "-show_entries", "frame=media_type,side_data_list"])
            .arg(path)
            .output()
            .await?;
        // A second pass without stream selection gives audio and subtitle streams.
        let streams = tokio::process::Command::new(self.ffprobe_path().unwrap()).args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"]).arg(path).output().await?;
        if !streams.status.success() {
            bail!("ffprobe could not read the file: {}", String::from_utf8_lossy(&streams.stderr).lines().next().unwrap_or("unknown error"));
        }
        let mut probe: serde_json::Value = serde_json::from_slice(&streams.stdout)?;
        if let Ok(frames) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            probe["frames"] = frames["frames"].clone();
        }
        match MediaInfo::from_ffprobe(&probe, scene_name) {
            Some(mi) => Ok(Some(mi)),
            None => bail!("the file has no video stream"),
        }
    }

    /// Where a replaced or deleted file is parked. Empty when files are deleted outright.
    fn recycle_path(&self, title: &Title, file_abs: &Path) -> PathBuf {
        let g = self.settings.general();
        if g.recycle_days == 0 {
            return PathBuf::new();
        }
        let root = match title.kind {
            Kind::Movie => g.movie_root,
            Kind::Series => g.series_root,
        };
        let root = if root.is_empty() { Path::new(&title.path).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default() } else { root };
        let day = chrono::Local::now().format("%Y%m%d").to_string();
        let name = file_abs.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        Path::new(&root).join(".spool-recycle").join(day).join(format!("{}-{}", now(), name))
    }

    /// Make sure the title has a folder path, deriving one from the naming settings if not.
    pub fn ensure_title_path(&self, title: &mut Title) -> Result<()> {
        if !title.path.is_empty() {
            return Ok(());
        }
        let g = self.settings.general();
        let naming_cfg = self.settings.naming();
        let (root, folder) = match title.kind {
            Kind::Movie => (g.movie_root, naming::movie_folder_name(&naming_cfg, &title.title, title.year, title.imdb_id.as_deref(), title.tmdb_id)),
            Kind::Series => (g.series_root, naming::series_folder_name(&naming_cfg, &title.title, title.year)),
        };
        if root.is_empty() {
            bail!("no library folder is configured for {}", if title.kind == Kind::Movie { "movies" } else { "television" });
        }
        title.path = Path::new(&root).join(folder).to_string_lossy().to_string();
        self.db.save_title(title)?;
        Ok(())
    }

    /// Where a file belongs in the library, relative to the title folder.
    pub fn library_rel_path(&self, title: &Title, episodes: &[Episode], file: &MediaFile, ext: &str) -> String {
        let cfg = self.settings.naming();
        let original = file.scene_name.clone().unwrap_or_default();
        match title.kind {
            Kind::Movie => {
                let name = naming::movie_file_name(&cfg, &MovieNameInput {
                    title: &title.title,
                    year: title.year,
                    imdb_id: title.imdb_id.as_deref(),
                    tmdb_id: title.tmdb_id,
                    quality: Some(&file.quality),
                    media_info: file.media_info.as_ref(),
                    edition: &file.edition,
                    release_group: file.release_group.as_deref(),
                    original_name: &original,
                });
                format!("{name}{ext}")
            }
            Kind::Series => {
                let season = episodes.first().map(|e| e.season).unwrap_or(0);
                let numbers: Vec<u32> = episodes.iter().map(|e| e.episode).collect();
                let titles: Vec<&str> = episodes.iter().map(|e| e.title.as_str()).collect();
                let name = naming::episode_file_name(&cfg, &EpisodeNameInput {
                    series_title: &title.title,
                    series_year: title.year,
                    season,
                    episodes: &numbers,
                    episode_titles: &titles,
                    air_date: episodes.first().and_then(|e| e.air_date.as_deref()),
                    quality: Some(&file.quality),
                    media_info: file.media_info.as_ref(),
                    release_group: file.release_group.as_deref(),
                    original_name: &original,
                });
                if title.season_folder {
                    format!("{}/{name}{ext}", naming::season_folder_name(&cfg, season))
                } else {
                    format!("{name}{ext}")
                }
            }
        }
    }

    /// Carry out one journalled operation. Every step tolerates having been done already.
    fn execute(&self, journal_id: i64, op: &Operation) -> Result<()> {
        let (src, dst) = (Path::new(&op.src), Path::new(&op.dst));
        // With recycling off, an old file is only removed once its replacement is safely in place.
        let aside = |p: &Path| PathBuf::from(format!("{}.spool-old", p.display()));
        for (_, old, recycle) in &op.replaces {
            let old = Path::new(old);
            if !old.exists() {
                continue;
            }
            if !recycle.is_empty() {
                if old != dst || src.exists() {
                    move_file(old, Path::new(recycle)).with_context(|| format!("recycling {}", old.display()))?;
                }
            } else if old == dst && src.exists() {
                // Same name as the file it replaces: step it aside until the new one has landed.
                std::fs::rename(old, aside(old))?;
            }
        }
        if src.exists() {
            if dst.exists() {
                bail!("{} already exists and is not a file Spool is replacing", dst.display());
            }
            move_file(src, dst)?;
        } else if !dst.exists() {
            bail!("neither the downloaded file nor its destination exists");
        }
        for (_, old, recycle) in &op.replaces {
            if recycle.is_empty() {
                let old = Path::new(old);
                if old != dst && old.exists() {
                    std::fs::remove_file(old).with_context(|| format!("deleting {}", old.display()))?;
                }
                let _ = std::fs::remove_file(aside(old));
            }
        }

        self.db.tx(|tx| {
            for (id, _, _) in &op.replaces {
                tx.execute("DELETE FROM files WHERE id = ?1", [id])?;
            }
            let existing: Option<i64> = {
                use rusqlite::OptionalExtension;
                tx.query_row("SELECT id FROM files WHERE title_id = ?1 AND rel_path = ?2", params![op.title_id, op.file.rel_path], |r| r.get(0)).optional()?
            };
            let file_id = match existing {
                Some(id) => {
                    tx.execute("UPDATE files SET size = ?2, data = ?3 WHERE id = ?1", params![id, op.file.size as i64, serde_json::to_string(&op.file)?])?;
                    id
                }
                None => {
                    tx.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1,?2,?3,?4)", params![op.title_id, op.file.rel_path, op.file.size as i64, serde_json::to_string(&op.file)?])?;
                    tx.last_insert_rowid()
                }
            };
            for ep in &op.episode_ids {
                tx.execute("UPDATE episodes SET file_id = ?2 WHERE id = ?1", params![ep, file_id])?;
            }
            tx.execute("UPDATE journal SET state = 'done' WHERE id = ?1", [journal_id])?;
            Ok(())
        })
    }

    fn journal(&self, op: &Operation) -> Result<i64> {
        self.db.with(|c| {
            c.execute("INSERT INTO journal(ts, acquisition_id, state, data) VALUES (?1,?2,'planned',?3)", params![now(), op.acquisition_id, serde_json::to_string(op).unwrap_or_default()])?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Finish imports that were interrupted. Called once at startup, before anything else moves files.
    pub fn recover(&self) -> Result<usize> {
        let pending: Vec<(i64, String)> = self.db.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM journal WHERE state = 'planned' ORDER BY id")?;
            let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })?;
        let mut done = 0;
        for (id, data) in pending {
            let Ok(op) = serde_json::from_str::<Operation>(&data) else { continue };
            match self.execute(id, &op) {
                Ok(()) => {
                    done += 1;
                    tracing::info!(dst = %op.dst, "finished an interrupted import");
                }
                Err(e) => {
                    tracing::error!(dst = %op.dst, error = %e, "could not finish an interrupted import");
                    self.db.with(|c| c.execute("UPDATE journal SET state = 'failed' WHERE id = ?1", [id]))?;
                    let _ = self.db.add_attention("import_blocked", Some(op.title_id), op.acquisition_id, &format!("An import was interrupted and could not be finished: {e}"), serde_json::json!({"src": op.src, "dst": op.dst}));
                }
            }
        }
        // Old finished entries are only useful for a while.
        self.db.with(|c| c.execute("DELETE FROM journal WHERE state = 'done' AND ts < ?1", [now() - 30 * 86400]))?;
        Ok(done)
    }

    /// Work out what each video file in a download is, and where it goes.
    async fn plan(&self, acq: &Acquisition, title: &Title, root: &Path, forced_episodes: Option<&[i64]>) -> std::result::Result<Vec<Operation>, Blocked> {
        let vids = video_files(root);
        if vids.is_empty() {
            return Err(Blocked::Needs("the download contains no video files".into()));
        }
        let flavor = title.kind.flavor();
        let episodes = self.db.episodes(title.id).map_err(|e| Blocked::Retry(e.to_string()))?;
        let files = self.db.files(title.id).map_err(|e| Blocked::Retry(e.to_string()))?;
        let profile = self.title_context(title.clone()).map_err(|e| Blocked::Retry(e.to_string()))?.profile;

        // (video path, size, episodes it holds)
        let mut targets: Vec<(PathBuf, u64, Vec<Episode>)> = vec![];
        match title.kind {
            Kind::Movie => targets.push((vids[0].0.clone(), vids[0].1, vec![])),
            Kind::Series => {
                let wanted: Vec<i64> = forced_episodes.map(|f| f.to_vec()).unwrap_or_else(|| acq.episode_ids.clone());
                for (path, size) in &vids {
                    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string();
                    let parsed = parse_episode_path(&format!("{}/{}", acq.release.title, rel)).or_else(|| parse_episode_path(&rel));
                    let mut eps: Vec<Episode> = match &parsed {
                        Some(p) if !p.episode_numbers.is_empty() => episodes.iter().filter(|e| e.season == p.season_number() && p.episode_numbers.contains(&e.episode)).cloned().collect(),
                        Some(p) if p.air_date.is_some() => episodes.iter().filter(|e| e.air_date == p.air_date).cloned().collect(),
                        Some(p) if p.is_absolute() => episodes_by_absolute(&episodes, &p.absolute_episode_numbers).map(|f| f.into_iter().cloned().collect()).unwrap_or_default(),
                        _ => vec![],
                    };
                    // One video for a release that names its episodes: trust the release over the file name.
                    if (eps.is_empty() || forced_episodes.is_some()) && vids.len() == 1 && !wanted.is_empty() {
                        eps = episodes.iter().filter(|e| wanted.contains(&e.id)).cloned().collect();
                    }
                    if !eps.is_empty() {
                        eps.sort_by_key(|e| (e.season, e.episode));
                        targets.push((path.clone(), *size, eps));
                    }
                }
                if targets.is_empty() {
                    return Err(Blocked::Needs("could not tell which episodes the downloaded files are".into()));
                }
            }
        }

        let mut ops = vec![];
        for (path, size, eps) in targets {
            let file_name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let ext = file_name.rfind('.').map(|i| file_name[i..].to_lowercase()).unwrap_or_default();
            // The release name is the better witness for quality; a file inside a pack may say more.
            let from_file = parse_quality_name(&file_name, flavor);
            let quality: QualityModel = if vids.len() > 1 && from_file.quality != Quality::Unknown { from_file } else if acq.quality.quality != Quality::Unknown { acq.quality } else { spool_core::quality::parse_quality(&file_name, flavor) };
            let parsed_movie = if flavor == Flavor::Movie { parse_movie_title(&acq.release.title, false) } else { None };
            let release_group = parsed_movie
                .as_ref()
                .and_then(|p| p.release_group.clone())
                .or_else(|| spool_core::parser::parse_release_group(&acq.release.title, flavor))
                .or_else(|| spool_core::parser::parse_release_group(&file_name, flavor));
            let media_info = match self.probe(&path, &acq.release.title).await {
                Ok(mi) => mi,
                Err(e) => return Err(Blocked::Needs(format!("{file_name} is not playable video: {e}"))),
            };
            if let (Kind::Movie, Some(mi)) = (title.kind, &media_info) {
                if title.runtime > 0 && mi.runtime_secs > 0.0 && mi.runtime_secs < title.runtime as f64 * 60.0 * 0.4 {
                    return Err(Blocked::Needs(format!("{file_name} runs {:.0} minutes but the movie is {} minutes; it may be a sample", mi.runtime_secs / 60.0, title.runtime)));
                }
            }
            let file = MediaFile {
                id: 0,
                title_id: title.id,
                rel_path: String::new(),
                size,
                quality,
                release_group,
                edition: parsed_movie.as_ref().map(|p| p.edition.clone()).unwrap_or_default(),
                media_info,
                scene_name: Some(acq.release.title.clone()),
                languages: parsed_movie.map(|p| p.languages).unwrap_or_default(),
                added_at: now(),
            };
            let rel = self.library_rel_path(title, &eps, &file, &ext);
            let dst = Path::new(&title.path).join(&rel);

            // What does this replace?
            let old: Vec<&MediaFile> = match title.kind {
                Kind::Movie => files.iter().collect(),
                Kind::Series => {
                    let ids: Vec<i64> = eps.iter().filter_map(|e| e.file_id).collect();
                    files.iter().filter(|f| ids.contains(&f.id)).collect()
                }
            };
            for o in &old {
                if forced_episodes.is_none() && !acq.replace_better && profile.compare(o.quality.quality, quality.quality) == std::cmp::Ordering::Greater {
                    return Err(Blocked::Needs(format!(
                        "the library already has {} as {}, which is better than this download ({})",
                        o.rel_path,
                        o.quality.full_name(flavor),
                        quality.full_name(flavor)
                    )));
                }
            }
            let replaces = old
                .iter()
                .map(|o| {
                    let abs = Path::new(&title.path).join(&o.rel_path);
                    (o.id, abs.to_string_lossy().to_string(), self.recycle_path(title, &abs).to_string_lossy().to_string())
                })
                .collect();
            ops.push(Operation {
                acquisition_id: (acq.id != 0).then_some(acq.id),
                title_id: title.id,
                episode_ids: eps.iter().map(|e| e.id).collect(),
                src: path.to_string_lossy().to_string(),
                dst: dst.to_string_lossy().to_string(),
                replaces,
                file: MediaFile { rel_path: rel, ..file },
            });
        }
        Ok(ops)
    }

    /// Import a finished download. Safe to call more than once for the same acquisition.
    pub async fn import(&self, acq_id: i64, forced_episodes: Option<Vec<i64>>) -> Result<()> {
        if !self.importing.lock().insert(acq_id) {
            return Ok(());
        }
        let result = self.import_inner(acq_id, forced_episodes).await;
        self.importing.lock().remove(&acq_id);
        result
    }

    async fn import_inner(&self, acq_id: i64, forced_episodes: Option<Vec<i64>>) -> Result<()> {
        let mut acq = self.db.acquisition(acq_id)?.ok_or_else(|| anyhow!("no such acquisition"))?;
        if matches!(acq.state, AcqState::Imported | AcqState::Cancelled) {
            return Ok(());
        }
        let mut title = self.db.title(acq.title_id)?.ok_or_else(|| anyhow!("the title was removed"))?;
        let Some(output) = acq.output_path.clone() else { bail!("the download has no output folder") };
        let root = PathBuf::from(&output);

        if !self.volume_ok() {
            acq.error = Some("the media volume is not mounted; the import will be retried".into());
            self.db.save_acquisition(&mut acq)?;
            return Ok(());
        }
        acq.state = AcqState::Importing;
        acq.error = None;
        self.db.save_acquisition(&mut acq)?;
        self.emit(Event::Acquisition { id: acq.id, title_id: acq.title_id, state: acq.state.as_str().into() });

        let block = |app: &App, acq: &mut Acquisition, why: String| -> Result<()> {
            acq.state = AcqState::ImportBlocked;
            acq.error = Some(why.clone());
            app.db.save_acquisition(acq)?;
            app.db.resolve_attention_for(acq.id)?;
            app.db.add_attention("import_blocked", Some(acq.title_id), Some(acq.id), &why, serde_json::json!({"release": acq.release.title, "path": acq.output_path}))?;
            app.emit(Event::Acquisition { id: acq.id, title_id: acq.title_id, state: acq.state.as_str().into() });
            app.emit(Event::Attention);
            Ok(())
        };

        if !root.exists() {
            return block(self, &mut acq, "the downloaded files are no longer on disk".into());
        }
        if let Err(e) = self.ensure_title_path(&mut title) {
            return block(self, &mut acq, e.to_string());
        }
        let ops = match self.plan(&acq, &title, &root, forced_episodes.as_deref()).await {
            Ok(ops) => ops,
            Err(Blocked::Needs(why)) => return block(self, &mut acq, why),
            Err(Blocked::Retry(why)) => {
                acq.state = AcqState::Downloading;
                acq.error = Some(why);
                self.db.save_acquisition(&mut acq)?;
                return Ok(());
            }
        };

        let mut imported = vec![];
        for op in &ops {
            let id = self.journal(op)?;
            let (app, op2) = (self.clone(), op.clone());
            let res = tokio::task::spawn_blocking(move || app.execute(id, &op2)).await?;
            if let Err(e) = res {
                self.db.with(|c| c.execute("UPDATE journal SET state = 'failed' WHERE id = ?1", [id]))?;
                return block(self, &mut acq, format!("could not move the file into the library: {e:#}"));
            }
            imported.push(op);
        }

        let upgraded = ops.iter().any(|o| !o.replaces.is_empty());
        for op in &imported {
            self.db.add_history(
                Some(title.id),
                if op.replaces.is_empty() { "imported" } else { "upgraded" },
                serde_json::json!({"release": acq.release.title, "path": op.dst, "quality": op.file.quality, "size": op.file.size, "episode_ids": op.episode_ids,
                    "replaced": op.replaces.iter().map(|r| r.1.clone()).collect::<Vec<_>>()}),
            )?;
        }
        // The download folder has served its purpose.
        let complete_dir = self.engine.config().complete_dir;
        if root.starts_with(&complete_dir) && root != complete_dir {
            let _ = tokio::fs::remove_dir_all(&root).await;
        }
        if let Some(job) = &acq.job_id {
            let _ = self.engine.remove(job, true);
        }
        acq.state = AcqState::Imported;
        acq.error = None;
        self.db.save_acquisition(&mut acq)?;
        self.db.resolve_attention_for(acq.id)?;
        self.db.resolve_attention_arrived(acq.title_id, &acq.episode_ids)?;
        self.emit(Event::Acquisition { id: acq.id, title_id: acq.title_id, state: acq.state.as_str().into() });
        self.emit(Event::Title { id: title.id });
        self.emit(Event::Attention);
        tracing::info!(title = %title.title, release = %acq.release.title, upgraded, "imported");

        let folders: Vec<String> = ops.iter().filter_map(|o| Path::new(&o.dst).parent().map(|p| p.to_string_lossy().to_string())).collect();
        let app = self.clone();
        tokio::spawn(async move {
            for f in folders {
                if let Err(e) = app.plex_refresh(&f).await {
                    tracing::warn!(error = %e, "Plex was not notified");
                }
            }
        });
        Ok(())
    }

    /// Move a library file to the recycle folder and forget it.
    pub fn delete_file(&self, file_id: i64) -> Result<()> {
        let file = self.db.file(file_id)?.ok_or_else(|| anyhow!("no such file"))?;
        let title = self.db.title(file.title_id)?.ok_or_else(|| anyhow!("no such title"))?;
        if !self.volume_ok() {
            bail!("the media volume is not mounted");
        }
        let abs = Path::new(&title.path).join(&file.rel_path);
        if abs.exists() {
            let recycle = self.recycle_path(&title, &abs);
            if recycle.as_os_str().is_empty() {
                std::fs::remove_file(&abs).with_context(|| format!("deleting {}", abs.display()))?;
            } else {
                move_file(&abs, &recycle)?;
            }
            // Leave no empty folder behind for Plex to show.
            if let Some(parent) = abs.parent() {
                let _ = std::fs::remove_dir(parent);
                if title.kind == Kind::Series {
                    let _ = std::fs::remove_dir(&title.path);
                }
            }
        }
        self.db.with(|c| c.execute("DELETE FROM files WHERE id = ?1", [file_id]))?;
        self.db.add_history(Some(title.id), "file_deleted", serde_json::json!({"path": abs.to_string_lossy()}))?;
        self.emit(Event::Title { id: title.id });
        Ok(())
    }

    fn recycle_roots(&self) -> Vec<PathBuf> {
        let g = self.settings.general();
        [g.movie_root, g.series_root].into_iter().filter(|r| !r.is_empty()).map(|r| Path::new(&r).join(".spool-recycle")).collect()
    }

    /// Number of files and total bytes waiting in the recycle folders.
    pub fn recycle_usage(&self) -> (usize, u64) {
        if !self.volume_ok() {
            return (0, 0);
        }
        let mut files = vec![];
        for root in self.recycle_roots() {
            walk(&root, &mut files);
        }
        let bytes = files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
        (files.len(), bytes)
    }

    /// Delete recycled files. With `all`, everything; otherwise only what is older than the
    /// configured number of days. Returns bytes freed.
    pub fn clean_recycle(&self, all: bool) -> Result<u64> {
        let g = self.settings.general();
        if !self.volume_ok() {
            bail!("the media volume is not mounted");
        }
        let cutoff = (chrono::Local::now() - chrono::Duration::days(g.recycle_days as i64)).format("%Y%m%d").to_string();
        let mut freed = 0;
        for root in self.recycle_roots() {
            let Ok(rd) = std::fs::read_dir(&root) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let dated = name.len() == 8 && name.chars().all(|c| c.is_ascii_digit());
                if all || g.recycle_days == 0 || (dated && name < cutoff) {
                    let mut inside = vec![];
                    walk(&e.path(), &mut inside);
                    let size: u64 = inside.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
                    let ok = if e.path().is_dir() { std::fs::remove_dir_all(e.path()).is_ok() } else { std::fs::remove_file(e.path()).is_ok() };
                    if ok {
                        freed += size;
                    }
                }
            }
        }
        Ok(freed)
    }

    /// Tell Plex that a folder changed.
    pub async fn plex_refresh(&self, folder: &str) -> Result<()> {
        let g = self.settings.general();
        if g.plex_url.is_empty() || g.plex_token.is_empty() {
            return Ok(());
        }
        let base = g.plex_url.trim_end_matches('/');
        let sections: serde_json::Value = self
            .http
            .get(format!("{base}/library/sections"))
            .header("X-Plex-Token", &g.plex_token)
            .header("Accept", "application/json")
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| anyhow!("{}", e.without_url()))?
            .error_for_status()
            .map_err(|e| anyhow!("{}", e.without_url()))?
            .json()
            .await
            .map_err(|e| anyhow!("{}", e.without_url()))?;
        for section in sections["MediaContainer"]["Directory"].as_array().into_iter().flatten() {
            let matches = section["Location"].as_array().into_iter().flatten().any(|l| l["path"].as_str().is_some_and(|p| folder.starts_with(p)));
            if matches {
                let key = section["key"].as_str().unwrap_or_default();
                self.http
                    .get(format!("{base}/library/sections/{key}/refresh?path={}", urlencoding::encode(folder)))
                    .header("X-Plex-Token", &g.plex_token)
                    .timeout(std::time::Duration::from_secs(15))
                    .send()
                    .await
                    .map_err(|e| anyhow!("{}", e.without_url()))?
                    .error_for_status()
                    .map_err(|e| anyhow!("{}", e.without_url()))?;
                return Ok(());
            }
        }
        Ok(())
    }
}
