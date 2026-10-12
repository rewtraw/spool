//! Adding titles, refreshing their metadata, and reconciling the library with what is on disk.

use crate::app::{App, Event};
use crate::db::now;
use crate::models::*;
use anyhow::{anyhow, bail, Result};
use spool_core::parser::{parse_episode_path, parse_movie_path};
use std::path::Path;

#[derive(Debug, Default, serde::Serialize)]
pub struct ScanReport {
    pub titles: usize,
    pub files_found: usize,
    pub files_added: usize,
    pub files_missing: usize,
    /// Records that had taken a trailer or another extra for the film, now put right.
    pub extras: usize,
    pub unmatched: Vec<String>,
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
            Kind::Series => self.meta.series(tvmaze_id.ok_or_else(|| anyhow!("a TVmaze id is required"))?).await?,
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
                    if let Ok((show, episodes)) = self.meta.series_by_tvdb(tvdb).await {
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
        let (listed, _) = self.meta.series(tvmaze).await?;
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
        let title = self.db.title(id)?.ok_or_else(|| anyhow!("no such title"))?;
        report.titles += 1;
        if title.path.is_empty() {
            return Ok(());
        }
        let places = self.placement();
        if places.online(&title) {
            self.scan_folder(&title, None, report).await?;
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
