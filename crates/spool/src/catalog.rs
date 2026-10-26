//! Adding titles, refreshing their metadata, and reconciling the library with what is on disk.

use crate::app::{App, Event};
use crate::db::now;
use crate::metadata::SeriesIds;
use crate::models::*;
use anyhow::{anyhow, bail, Result};
use spool_core::parser::{parse_episode_path, parse_movie_path};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, serde::Serialize)]
pub struct ScanReport {
    pub titles: usize,
    pub files_found: usize,
    pub files_added: usize,
    pub files_missing: usize,
    /// Records that had taken a trailer or another extra for the film, now put right.
    pub extras: usize,
    pub unmatched: Vec<String>,
    /// Titles whose folders could not be looked through to the end, so nothing was
    /// concluded about them this time.
    pub put_off: Vec<String>,
    /// Episode files found filed as another episode than the one their release named by
    /// title, and filed again as that one.
    pub refiled: usize,
}

/// A path without its extension, and the extension with its dot.
fn stem(rel: &str) -> String {
    rel.rfind('.').map(|i| rel[..i].to_string()).unwrap_or_else(|| rel.to_string())
}

fn extension(rel: &str) -> String {
    rel.rfind('.').map(|i| rel[i..].to_string()).unwrap_or_default()
}

impl App {
    /// Add a title from a metadata lookup.
    pub async fn add_title(&self, kind: Kind, tmdb_id: Option<u32>, tvmaze_id: Option<u32>, profile_id: i64, monitored: bool, search: bool) -> Result<Title> {
        self.add_title_on(kind, tmdb_id, tvmaze_id, profile_id, monitored, search, None).await
    }

    /// Add a title, on the location named or, with none named, the one new titles go to.
    #[allow(clippy::too_many_arguments)]
    pub async fn add_title_on(&self, kind: Kind, tmdb_id: Option<u32>, tvmaze_id: Option<u32>, profile_id: i64, monitored: bool, search: bool, location: Option<u32>) -> Result<Title> {
        let g = self.settings.general();
        let (mut title, episodes) = match kind {
            Kind::Movie => (self.meta.movie(&g.tmdb_api_key, tmdb_id.ok_or_else(|| anyhow!("a TMDB id is required"))?).await?, vec![]),
            Kind::Series if tvmaze_id.is_none() && tmdb_id.is_none() => bail!("a TVmaze or TMDB id is required"),
            Kind::Series => self.meta.series(&g.tmdb_api_key, &SeriesIds { tvmaze: tvmaze_id, tmdb: tmdb_id, ..Default::default() }).await?,
        };
        if self.db.find_title(kind, title.tmdb_id, title.tvdb_id)?.is_some() {
            bail!("{} is already in the library", title.title);
        }
        title.profile_id = profile_id;
        title.monitored = monitored;
        title.added_at = now();
        title.last_refresh_at = now();
        // Before anything is searched for: is it anime, and is this film a series somewhere else?
        self.learn_anime(&mut title).await;
        self.ask_series_source(&mut title).await;
        self.learn_series_tmdb(&mut title).await;
        self.ensure_title_path_on(&mut title, location)?;
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

    /// Ask the series source whether it lists this film as a series. A work that one source
    /// calls a film and the other a run of episodes is usually posted as the episodes, and a
    /// single episode's name carries the film's whole title; knowing beforehand is what keeps
    /// one from being downloaded as the film. A source that cannot be reached is asked again
    /// at the next refresh.
    /// A series' id at the film source, which players such as Infuse find it by. Asked once it is
    /// known by another id; one the library already gives another series is not taken, and a
    /// source that cannot be reached is asked again at the next refresh.
    async fn learn_series_tmdb(&self, t: &mut Title) {
        if t.kind != Kind::Series || t.tmdb_id.is_some() {
            return;
        }
        let key = self.settings.general().tmdb_api_key;
        match self.meta.series_tmdb(&key, t.tvdb_id, t.imdb_id.as_deref()).await {
            Ok(Some(tmdb)) => {
                let taken = self.db.find_title(Kind::Series, Some(tmdb), None).ok().flatten().is_some_and(|other| other.id != t.id);
                if !taken {
                    t.tmdb_id = Some(tmdb);
                }
            }
            Ok(None) => {}
            Err(e) => tracing::debug!(title = %t.title, error = %e, "the film source could not be asked for a series' id"),
        }
    }

    async fn ask_series_source(&self, t: &mut Title) {
        if t.kind != Kind::Movie {
            return;
        }
        match self.meta.series_twin(t.imdb_id.as_deref(), &t.title, t.year, t.runtime).await {
            Ok(mut twin) => {
                // An OVA the series source lists under another name is still found by the
                // series id the anime list has for it.
                let tvdb = t.anilist.first().and_then(|a| self.anime_ids().and_then(|ids| ids.back.get(a).cloned())).filter(|k| !k.film && k.season >= 1).and_then(|k| k.tvdb);
                if let (None, Some(tvdb)) = (&twin, tvdb) {
                    if let Ok((show, episodes)) = self.meta.series(&self.settings.general().tmdb_api_key, &SeriesIds::tvdb(tvdb)).await {
                        let regular: Vec<&Episode> = episodes.iter().filter(|e| e.season > 0).collect();
                        if let (Some(tvmaze_id), true) = (show.tvmaze_id, regular.len() >= 2) {
                            twin = Some(SeriesTwin { tvmaze_id, title: show.title, episodes: regular.len() as u32, minutes: regular.iter().map(|e| e.runtime).sum() });
                        }
                    }
                }
                t.series_checked = true;
                if twin.is_some() {
                    t.in_episodes = true;
                }
                t.as_series = twin;
            }
            Err(e) => tracing::debug!(title = %t.title, error = %e, "could not ask whether a film is listed as a series"),
        }
    }

    /// Turn a film that is posted in episodes into the series it is listed as elsewhere. The
    /// series takes the film's monitoring; a download of the film that turned out to be an
    /// episode is filed as that episode; the film's entry, which has no file, is removed.
    pub async fn film_to_series(&self, id: i64, tvmaze_id: Option<u32>) -> Result<Title> {
        let mut film = self.db.title(id)?.ok_or_else(|| anyhow!("no such title"))?;
        if film.kind != Kind::Movie {
            bail!("{} is already a series", film.title);
        }
        if !self.db.files(id)?.is_empty() {
            bail!("{} has a file as a film; remove the file first, or keep it as a film", film.title);
        }
        if tvmaze_id.is_none() && film.as_series.is_none() {
            self.ask_series_source(&mut film).await;
            self.db.save_title(&mut film)?;
        }
        let Some(tvmaze) = tvmaze_id.or(film.as_series.as_ref().map(|s| s.tvmaze_id)) else {
            bail!("no series called {} was found; add the series from Discover and remove the film", film.title);
        };
        let g = self.settings.general();
        let (listed, _) = self.meta.series(&g.tmdb_api_key, &SeriesIds { tvmaze: Some(tvmaze), ..Default::default() }).await?;
        let series = match self.db.find_title(Kind::Series, None, listed.tvdb_id)?.filter(|_| listed.tvdb_id.is_some()) {
            Some(have) => have,
            None => self.add_title(Kind::Series, None, Some(tvmaze), self.profile_for_new(Kind::Series, g.new_titles_location), film.monitored, false).await?,
        };

        // What was downloaded as the film and stopped: the release is judged again as part of
        // the series, which says which episodes it is. Anything still on its way is stopped.
        let tc = self.title_context(series.clone())?;
        let mut carried = vec![];
        for mut a in self.db.title_acquisitions(id)?.into_iter().filter(|a| a.state.is_active()) {
            let on_disk = a.output_path.as_ref().is_some_and(|p| std::path::Path::new(p).exists());
            if a.state != AcqState::ImportBlocked || !on_disk {
                self.cancel_acquisition(a.id, false).await?;
                continue;
            }
            a.episode_ids = self.episodes_of(&tc, &a.release);
            a.reason = format!("downloaded as the film {}", film.title);
            self.db.resolve_attention_for(a.id)?;
            self.db.move_acquisition(&mut a, series.id)?;
            carried.push(a.id);
        }
        self.db.add_history(Some(series.id), "was_film", serde_json::json!({"title": film.title, "year": film.year, "downloads": carried.len()}))?;
        let folder = film.path.clone();
        self.db.delete_title(id)?;
        // The film's folder was made for a file that never came. Only an empty one goes.
        if !folder.is_empty() {
            let _ = std::fs::remove_dir(&folder);
        }
        self.emit(Event::Title { id });
        self.emit(Event::Title { id: series.id });
        self.emit(Event::Attention);

        let (app, sid, monitored) = (self.clone(), series.id, series.monitored);
        tokio::spawn(async move {
            for a in carried {
                if let Err(e) = app.import(a, None).await {
                    tracing::warn!(error = %e, "filing a film's download as an episode failed");
                }
            }
            if monitored {
                if let Err(e) = app.search_with(sid, crate::acquire::Scope::Missing, false, true, true).await {
                    tracing::warn!(error = %e, "search after turning a film into a series failed");
                }
            }
        });
        Ok(series)
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
                t.studio = m.studio.or(t.studio);
                t.tagline = m.tagline;
                t.rating = m.rating.or(t.rating);
                t.certification = m.certification.or(t.certification);
                t.people = m.people;
                t.collection = m.collection;
                t.details_at = m.details_at;
                for a in m.alt_titles {
                    if !t.alt_titles.contains(&a) {
                        t.alt_titles.push(a);
                    }
                }
                // Only a film still to be found is worth the question.
                if !t.series_checked && self.db.files(id)?.is_empty() {
                    self.ask_series_source(&mut t).await;
                }
            }
            Kind::Series => {
                let key = self.settings.general().tmdb_api_key;
                let listed = match (t.from_tmdb, t.tmdb_id) {
                    (true, Some(tmdb)) => Some(self.meta.tmdb_series(&key, tmdb).await?),
                    (true, None) => None,
                    (false, _) => self.meta.tvmaze_listing(&SeriesIds::of(&t)).await?,
                };
                let Some((m, episodes)) = listed else {
                    if t.from_tmdb || SeriesIds::of(&t).is_empty() {
                        return Ok(());
                    }
                    bail!("not found");
                };
                t.tvmaze_id = m.tvmaze_id.or(t.tvmaze_id);
                t.tvdb_id = t.tvdb_id.or(m.tvdb_id);
                t.imdb_id = t.imdb_id.or(m.imdb_id);
                self.learn_series_tmdb(&mut t).await;
                t.original_language = m.original_language.or(t.original_language);
                t.overview = m.overview;
                t.status = m.status;
                t.network = m.network.or(t.network);
                t.poster = t.poster.or(m.poster);
                t.fanart = t.fanart.or(m.fanart);
                t.genres = m.genres;
                t.rating = m.rating.or(t.rating);
                if !m.people.is_empty() {
                    t.people = m.people;
                }
                t.details_at = m.details_at;
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
                            e.still = e.still.or(old.still.clone());
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

    /// Compare one title's folders with the database: its own, and any others that hold it on
    /// other locations. Never writes to the media volume. A folder that cannot be reached is
    /// passed over: files that cannot be seen are not files that have gone.
    pub async fn scan_title(&self, id: i64, report: &mut ScanReport) -> Result<()> {
        // One look through a title's folders at a time: what one finds gone may be a file
        // the other is in the middle of renaming.
        static LOOKING: std::sync::LazyLock<parking_lot::Mutex<std::collections::HashMap<i64, std::sync::Arc<tokio::sync::Mutex<()>>>>> = std::sync::LazyLock::new(Default::default);
        let turn = LOOKING.lock().entry(id).or_default().clone();
        let _held = turn.lock().await;
        let title = self.db.title(id)?.ok_or_else(|| anyhow!("no such title"))?;
        report.titles += 1;
        if title.path.is_empty() {
            return Ok(());
        }
        let places = self.placement();
        if places.online(&title) {
            self.scan_folder(&title, None, report).await?;
            // Filing again that cannot be finished now is tried again next time; it does not
            // stop the folder being looked through.
            if title.kind == Kind::Series {
                match self.refile_by_titles(&title).await {
                    Ok(n) => report.refiled += n,
                    Err(e) => {
                        tracing::warn!(title = %title.title, error = %format!("{e:#}"), "episodes could not all be filed again by their titles");
                        self.db.add_history(Some(id), "refile_stopped", serde_json::json!({"error": format!("{e:#}")}))?;
                    }
                }
            }
        }
        for folder in title.also.iter().filter(|f| places.online_at(f)) {
            self.scan_folder(&title, Some(folder), report).await?;
        }
        Ok(())
    }

    /// One folder of a title: its own when `copy` is None, else a copy of it kept elsewhere.
    /// In the title's own folder every film and episode is a library file. In a copy, a film
    /// is a version kept beside the title's own, and an episode is taken only where the
    /// title has no file for it, so that a series may have its seasons in two places.
    async fn scan_folder(&self, title: &Title, copy: Option<&String>, report: &mut ScanReport) -> Result<()> {
        let id = title.id;
        let root = Path::new(copy.map(String::as_str).unwrap_or(&title.path));
        // Only the film or the episodes are library files. Trailers, featurettes, samples and
        // artwork are noted beside the title and otherwise left out of everything.
        let crate::local::Survey { videos: on_disk, local } = self.survey(root, title.kind).await?;
        report.files_found += on_disk.len();
        let known: Vec<MediaFile> = self.db.files(id)?.into_iter().filter(|f| f.base.as_ref() == copy).collect();

        for f in &known {
            if !on_disk.iter().any(|v| v.rel == f.rel_path) {
                self.db.with(|c| c.execute("DELETE FROM files WHERE id = ?1", [f.id]))?;
                if local.extras.iter().any(|e| e.rel_path == f.rel_path) {
                    // An earlier scan took an extra for the film. The file is still there.
                    report.extras += 1;
                    self.db.add_history(Some(id), "not_the_film", serde_json::json!({"path": root.join(&f.rel_path).to_string_lossy()}))?;
                } else {
                    report.files_missing += 1;
                    self.db.add_history(Some(id), "file_missing", serde_json::json!({"path": root.join(&f.rel_path).to_string_lossy()}))?;
                }
            }
        }
        let episodes = if title.kind == Kind::Series { self.db.episodes(id)? } else { vec![] };
        let stem = |rel: &str| Path::new(rel).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        // The originals first, so that a compact version finds the file it was made from
        // already on record when its turn comes.
        let mut fresh: Vec<&crate::local::VideoFile> = on_disk.iter().filter(|v| !known.iter().any(|f| f.rel_path == v.rel)).collect();
        fresh.sort_by_key(|v| crate::compact::compact_source_stem(&stem(&v.rel)).is_some());
        // A linked machine may tell of a file a moment before the share shows it here. It is
        // taken up when it can be read from here, which its details need; and while an
        // original waits, so do the compact versions, which would otherwise be put on record
        // as belonging to nothing.
        let mut waiting = false;
        for crate::local::VideoFile { rel, size } in fresh {
            let (abs, size) = (root.join(rel), *size);
            let is_compact = crate::compact::compact_source_stem(&stem(rel)).is_some();
            if (waiting && is_compact) || !abs.exists() {
                waiting |= !is_compact;
                continue;
            }
            // A compact version is known by its name, and belongs to the file it was made
            // from: the one of that name, wherever the title keeps it, or failing that the
            // file of the episode it names.
            let compact_of: Option<i64> = match crate::compact::compact_source_stem(&stem(rel)) {
                None => None,
                Some(from) => {
                    let all = self.db.files(id)?;
                    all.iter().find(|f| f.compact_of.is_none() && stem(&f.rel_path) == from).map(|f| f.id).or_else(|| {
                        let named = parse_episode_path(&abs.to_string_lossy()).filter(|_| title.kind == Kind::Series)?;
                        episodes.iter().find(|e| e.season == named.season_number() && named.episode_numbers.contains(&e.episode)).and_then(|e| e.file_id).filter(|f| all.iter().any(|x| x.id == *f && x.compact_of.is_none()))
                    })
                }
            };
            let name = abs.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let flavor = title.kind.flavor();
            let episode_ids: Vec<i64> = match title.kind {
                Kind::Movie => vec![],
                // It is a version of an episode's file, not the episode's file.
                Kind::Series if compact_of.is_some() => vec![],
                Kind::Series => {
                    let ids: Vec<i64> = parse_episode_path(&abs.to_string_lossy())
                        .map(|p| episodes.iter().filter(|e| e.season == p.season_number() && p.episode_numbers.contains(&e.episode)).filter(|e| copy.is_none() || e.file_id.is_none()).map(|e| e.id).collect())
                        .unwrap_or_default();
                    if ids.is_empty() {
                        // In a copy, an episode the title already has is simply not needed.
                        if copy.is_none() {
                            report.unmatched.push(abs.to_string_lossy().to_string());
                        }
                        continue;
                    }
                    ids
                }
            };
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
                extra: compact_of.is_some() || (copy.is_some() && title.kind == Kind::Movie),
                subtitles: vec![],
                base: copy.cloned(),
                compact_of,
            };
            if title.kind == Kind::Movie {
                if let Some(p) = parse_movie_path(&abs.to_string_lossy()) {
                    file.edition = p.edition;
                }
            }
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
        // A compact version whose original is no longer on record stands by itself from now
        // on: it is the film if there is no other, and an episode's file if the episode has none.
        let all = self.db.files(id)?;
        for f in all.iter().filter(|f| f.compact_of.is_some_and(|from| !all.iter().any(|x| x.id == from))) {
            let mut f = f.clone();
            f.compact_of = None;
            f.extra = title.kind == Kind::Movie && all.iter().any(|x| !x.extra);
            self.db.save_file(&f)?;
            if title.kind == Kind::Series {
                if let Some(named) = parse_episode_path(&title.file_path(&f).to_string_lossy()) {
                    for e in episodes.iter().filter(|e| e.season == named.season_number() && named.episode_numbers.contains(&e.episode)) {
                        self.db.with(|c| c.execute("UPDATE episodes SET file_id = ?2 WHERE id = ?1 AND file_id IS NULL", rusqlite::params![e.id, f.id]))?;
                    }
                }
            }
        }
        if copy.is_none() {
            self.settle_versions(title, &known)?;
            self.keep_local(id, local)?;
        }
        Ok(())
    }

    /// Note what a title's folder holds beside the film, if that has changed.
    fn keep_local(&self, id: i64, local: crate::local::Local) -> Result<()> {
        let Some(mut t) = self.db.title(id)? else { return Ok(()) };
        if t.local != local {
            t.local = local;
            self.db.save_title(&mut t)?;
            self.emit(Event::Title { id });
        }
        Ok(())
    }

    /// A film folder with several films' worth of video holds one film and versions of it kept
    /// beside it, unless they are the parts of one film split across files. One is the main
    /// file, which an upgrade replaces; the rest are marked as versions, which it leaves alone.
    /// `before` is what was on record ahead of this look at the folder.
    fn settle_versions(&self, title: &Title, before: &[MediaFile]) -> Result<()> {
        if title.kind != Kind::Movie {
            return Ok(());
        }
        let files = self.db.files(title.id)?;
        let stem = |f: &MediaFile| Path::new(&f.rel_path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let main: Vec<&MediaFile> = files.iter().filter(|f| !f.extra).collect();
        if main.len() < 2 || main.iter().all(|f| crate::local::is_part(&stem(f))) {
            return Ok(());
        }
        // The file already known as the film stays the film; failing that, the largest is. (By
        // path as well as number: a record just removed can lend its number to a new one.)
        let was: Vec<&&MediaFile> = main.iter().filter(|f| before.iter().any(|b| b.id == f.id && b.rel_path == f.rel_path && !b.extra)).collect();
        let keep = match was.as_slice() {
            [one] => one.id,
            _ => main.iter().max_by_key(|f| f.size).map(|f| f.id).unwrap_or(0),
        };
        for f in main.into_iter().filter(|f| f.id != keep) {
            let mut f = f.clone();
            f.extra = true;
            self.db.save_file(&f)?;
        }
        Ok(())
    }

    /// Look again at what a title's folder holds beside the film, without concluding anything
    /// about files that are not there. Records that took an extra for the film are corrected.
    pub async fn survey_title(&self, id: i64) -> Result<()> {
        let title = self.db.title(id)?.ok_or_else(|| anyhow!("no such title"))?;
        if title.path.is_empty() || !self.title_online(&title) {
            return Ok(());
        }
        let local = self.survey(Path::new(&title.path), title.kind).await?.local;
        let known = self.db.files(id)?;
        for f in known.iter().filter(|f| local.extras.iter().any(|e| e.rel_path == f.rel_path)) {
            self.db.with(|c| c.execute("DELETE FROM files WHERE id = ?1", [f.id]))?;
            self.db.add_history(Some(id), "not_the_film", serde_json::json!({"path": Path::new(&title.path).join(&f.rel_path).to_string_lossy()}))?;
        }
        self.settle_versions(&title, &known)?;
        self.keep_local(id, local)
    }

    /// The same for every title, a little at a time.
    pub async fn survey_all(&self) -> usize {
        let mut looked = 0;
        for t in self.db.titles(None).unwrap_or_default() {
            if self.survey_title(t.id).await.is_ok() {
                looked += 1;
            }
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }
        looked
    }

    pub async fn scan_all(&self) -> Result<ScanReport> {
        let mut report = ScanReport::default();
        if self.placement().0.iter().all(|(_, online)| !online) {
            bail!("the media volume is not mounted");
        }
        for t in self.db.titles(None)? {
            if let Err(e) = self.scan_title(t.id, &mut report).await {
                tracing::warn!(title = %t.title, error = %format!("{e:#}"), "its folder could not be looked through; nothing is concluded about it");
                report.put_off.push(t.title.clone());
            }
        }
        self.emit(Event::Title { id: 0 });
        Ok(report)
    }

    /// Put right episode files filed by a release's own numbering when the release named its
    /// episode by title, "Show.S01E12.Double.Dare": each is made the file of the episode of that
    /// title and renamed for it, with the subtitles beside it. A file is only moved onto an
    /// episode that has no file or whose file is moving away too, and each is first stepped
    /// aside, so files that trade places do not meet. Nothing is moved onto a name already
    /// there, nor through a link out of the title's folder. Each move is written down as it is
    /// made, and an episode is given its file once the file has its name, so that if this
    /// stops part way the next look through the folder takes it up again.
    /// Returns how many were filed again.
    async fn refile_by_titles(&self, title: &Title) -> Result<usize> {
        // Not while it is being moved, nor while something is being imported into it; it is
        // done the next time its folder is looked through instead.
        if self.moving(title.id) {
            return Ok(0);
        }
        let turn = self.title_imports.lock().entry(title.id).or_default().clone();
        let Ok(_held) = turn.try_lock() else { return Ok(0) };
        let episodes = self.db.episodes(title.id)?;
        let all = self.db.files(title.id)?;
        // A file with a compact version made from it is left, so the two keep one name.
        let files: Vec<MediaFile> = all.iter().filter(|f| f.base.is_none() && f.compact_of.is_none() && !f.extra && !all.iter().any(|c| c.compact_of == Some(f.id))).cloned().collect();
        let mut moves: Vec<(MediaFile, Episode)> = vec![];
        for f in &files {
            let Some(scene) = f.scene_name.as_deref() else { continue };
            let Some(p) = crate::names::episode(scene) else { continue };
            let [n] = p.episode_numbers[..] else { continue };
            let Some(named) = crate::names::retitled(scene, p.season_number(), n, &episodes) else { continue };
            let held: Vec<i64> = episodes.iter().filter(|e| e.file_id == Some(f.id)).map(|e| e.id).collect();
            // A file found to hold two episodes was filed by more than its name.
            if held.len() > 1 {
                continue;
            }
            let named_right = f.rel_path == self.library_rel_path(title, std::slice::from_ref(named), f, &extension(&f.rel_path));
            if held != [named.id] || !named_right {
                moves.push((f.clone(), named.clone()));
            }
        }
        // Onto an episode with no file, or one whose file is moving too; one file to an
        // episode. Leaving one out may leave another with nowhere to go, so until none changes.
        loop {
            let leaving: HashSet<i64> = moves.iter().map(|(f, _)| f.id).collect();
            let mut taken: HashSet<i64> = HashSet::new();
            let before = moves.len();
            moves.retain(|(f, e)| e.file_id.is_none_or(|g| g == f.id || leaving.contains(&g)) && taken.insert(e.id));
            if moves.len() == before {
                break;
            }
        }
        if moves.is_empty() {
            return Ok(0);
        }
        let root = PathBuf::from(&title.path);
        let home = std::fs::canonicalize(&root)?;
        let renamed = |f: &MediaFile, to: &str| -> MediaFile {
            let (from_stem, to_stem) = (stem(&f.rel_path), stem(to));
            let mut g = f.clone();
            g.rel_path = to.to_string();
            // A subtitle goes by the video's name, or by the name it was stepped aside under
            // when a time that stopped part way moved the video on without it.
            let mark = format!(".spool-refile-{}", f.id);
            for s in &mut g.subtitles {
                let rest = s.rel_path.find(&format!("{mark}.")).map(|i| &s.rel_path[i + mark.len()..]).or_else(|| s.rel_path.strip_prefix(&from_stem));
                if let Some(rest) = rest {
                    s.rel_path = format!("{to_stem}{rest}");
                }
            }
            g
        };
        let mut aside = vec![];
        for (f, e) in &moves {
            // Its own name for the while, by its number, which no other file shares, and still
            // a video's name, so it is known for what it is if it is left there. One left there
            // by a time that stopped part way is already aside.
            let mark = format!(".spool-refile-{}", f.id);
            let step = if stem(&f.rel_path).ends_with(&mark) { f.clone() } else { renamed(f, &format!("{}{mark}{}", stem(&f.rel_path), extension(&f.rel_path))) };
            self.shift(&root, &home, f, &step).await?;
            aside.push((step, e.clone()));
        }
        let mut done = 0;
        for (f, e) in &aside {
            let to = self.library_rel_path(title, std::slice::from_ref(e), f, &extension(&f.rel_path));
            self.shift(&root, &home, f, &renamed(f, &to)).await?;
            self.db.tx(|tx| {
                tx.execute("UPDATE episodes SET file_id = NULL WHERE title_id = ?1 AND file_id = ?2", rusqlite::params![title.id, f.id])?;
                tx.execute("UPDATE episodes SET file_id = ?2 WHERE id = ?1", rusqlite::params![e.id, f.id])?;
                Ok(())
            })?;
            self.db.add_history(Some(title.id), "refiled", serde_json::json!({"release": f.scene_name, "as": format!("S{:02}E{:02}", e.season, e.episode), "path": to}))?;
            self.emit(Event::Title { id: title.id });
            done += 1;
        }
        Ok(done)
    }

    /// Say what quality a file is, when what it was taken for is wrong: a name that said
    /// nothing true of it, or a guess made before its picture was looked at. A file named as
    /// Spool names files is named again for it, with the subtitles beside it; one named
    /// otherwise, or with a compact version made from it that shares its name, keeps its name.
    pub async fn set_quality(&self, file_id: i64, quality: spool_core::quality::Quality) -> Result<MediaFile> {
        let f = self.db.file(file_id)?.ok_or_else(|| anyhow!("no such file"))?;
        let title = self.db.title(f.title_id)?.ok_or_else(|| anyhow!("no such title"))?;
        if f.base.is_some() {
            bail!("this copy is on another location, and Spool does not change it");
        }
        if self.moving(title.id) {
            bail!("{} is being moved; try again when it is done", title.title);
        }
        let turn = self.title_imports.lock().entry(title.id).or_default().clone();
        let Ok(_held) = turn.try_lock() else { bail!("something is being imported into {}; try again when it is done", title.title) };
        let mut eps: Vec<Episode> = self.db.episodes(title.id)?.into_iter().filter(|e| e.file_id == Some(f.id)).collect();
        eps.sort_by_key(|e| (e.season, e.episode));
        let ext = extension(&f.rel_path);
        let named_by_spool = f.rel_path == self.library_rel_path(&title, &eps, &f, &ext);
        let shares_name = self.db.files(title.id)?.iter().any(|c| c.compact_of == Some(f.id));
        let mut g = f.clone();
        g.quality.quality = quality;
        // Written down with the first thing moved, so a time that fails before then leaves
        // the file as it was, to be tried again.
        if named_by_spool && !shares_name {
            let to = self.library_rel_path(&title, &eps, &g, &ext);
            if to != g.rel_path {
                let (from_stem, to_stem) = (stem(&g.rel_path), stem(&to));
                let mut moved = g.clone();
                moved.rel_path = to;
                for s in &mut moved.subtitles {
                    if let Some(rest) = s.rel_path.strip_prefix(&from_stem) {
                        s.rel_path = format!("{to_stem}{rest}");
                    }
                }
                let root = PathBuf::from(&title.path);
                let home = std::fs::canonicalize(&root)?;
                self.shift(&root, &home, &g, &moved).await?;
                g = moved;
            }
        }
        self.db.save_file(&g)?;
        self.db.add_history(Some(title.id), "quality_set", serde_json::json!({"file": f.id, "from": f.quality.quality.key(), "to": quality.key(), "path": g.rel_path}))?;
        self.emit(Event::Title { id: title.id });
        Ok(g)
    }

    /// Move a video and the subtitles beside it from the names one record gives to those
    /// another gives, writing each down as it goes. Nothing is put where something already is,
    /// and nothing is moved through a link out of the title's folder. The subtitles go first:
    /// while the video keeps its name, those that went are known by theirs, and those that did
    /// not by the video's.
    async fn shift(&self, root: &Path, home: &Path, from: &MediaFile, to: &MediaFile) -> Result<()> {
        let (app, root, home, from, to) = (self.clone(), root.to_path_buf(), home.to_path_buf(), from.clone(), to.clone());
        tokio::task::spawn_blocking(move || -> Result<()> {
            let inside = |rel: &str| -> Result<PathBuf> {
                let p = root.join(rel);
                let parent = p.parent().ok_or_else(|| anyhow!("{rel} has no folder"))?;
                if Path::new(rel).components().any(|c| !matches!(c, std::path::Component::Normal(_))) || !std::fs::canonicalize(parent)?.starts_with(&home) {
                    bail!("{rel} is not inside the title's folder");
                }
                Ok(p)
            };
            let mut now = from.clone();
            for (sa, sb) in from.subtitles.iter().zip(&to.subtitles) {
                if sa.rel_path == sb.rel_path {
                    continue;
                }
                let (a, b) = (inside(&sa.rel_path)?, inside(&sb.rel_path)?);
                // Gone since it was found: it is forgotten, as looking beside the video would.
                if !a.exists() && !b.exists() {
                    now.subtitles.retain(|s| s.rel_path != sa.rel_path);
                    app.db.save_file(&now)?;
                    continue;
                }
                if b.exists() {
                    bail!("{} is there already; {} is left as it is", sb.rel_path, sa.rel_path);
                }
                app.file_move(&a, &b)?;
                for s in now.subtitles.iter_mut().filter(|s| s.rel_path == sa.rel_path) {
                    s.rel_path = sb.rel_path.clone();
                }
                app.db.save_file(&now)?;
            }
            let (a, b) = (inside(&from.rel_path)?, inside(&to.rel_path)?);
            if a != b {
                if b.exists() {
                    bail!("{} is there already; {} is left as it is", to.rel_path, from.rel_path);
                }
                app.file_move(&a, &b)?;
                now.rel_path = to.rel_path.clone();
                app.db.save_file(&now)?;
            }
            Ok(())
        })
        .await?
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
                    if f.base.is_some() {
                        continue;
                    }
                    differ.push((format!("{}/{}", t.path, f.rel_path), format!("{}/{}", t.path, expected)));
                }
            }
        }
        Ok((same, differ))
    }
}
