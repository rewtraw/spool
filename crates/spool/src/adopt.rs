//! Bringing in a library that already exists on a location.
//!
//! It happens in two steps, so nothing is added on a guess. Looking walks the location's
//! folders, reads what each is from its name, asks the metadata sources, and reports: what it
//! is sure of, what is already in the library, what could be one of several things, and what
//! it could not place. Adding then takes the sure ones, and any the user has settled, and
//! records each title where it lies. No file is moved, renamed or changed by either step.

use crate::app::{App, Event};
use crate::db::now;
use crate::models::*;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One thing a folder could be.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Match {
    pub tmdb_id: Option<u32>,
    pub tvmaze_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub title: String,
    pub year: u32,
    pub poster: Option<String>,
}

/// A folder on the location and what was made of it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Found {
    pub folder: String,
    pub kind: Kind,
    /// The name and year as read from the folder.
    pub name: String,
    pub year: u32,
    pub files: usize,
    pub bytes: u64,
    /// new: sure what it is, and not in the library. have: already in the library. unclear:
    /// could be any of `matches`. unknown: nothing found. added, failed: after adding.
    pub state: String,
    pub note: String,
    /// What it could be; for a new one, the first is what it is.
    pub matches: Vec<Match>,
}

/// How far looking through a location, or adding from it, has got.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Adoption {
    pub location: u32,
    pub running: bool,
    /// looking, adding, or done.
    pub step: String,
    pub done: usize,
    pub total: usize,
    pub message: String,
    pub found: Vec<Found>,
}

/// A folder the user has settled: which of the things it could be it is.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Pick {
    pub folder: String,
    pub tmdb_id: Option<u32>,
    pub tvmaze_id: Option<u32>,
    pub tvdb_id: Option<u32>,
}

/// What a folder's name says it holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Named {
    pub name: String,
    pub year: u32,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
}

fn is_year(s: &str) -> Option<u32> {
    (s.len() == 4).then(|| s.parse::<u32>().ok()).flatten().filter(|y| (1880..=2100).contains(y))
}

/// Read a folder name: "Heat (1995)", "Heat (1995) {imdb-tt0113277}", "Heat [tmdbid-949]",
/// "Heat 1995", or a release name such as "Heat.1995.1080p.BluRay.x264-GRP".
pub fn read_name(folder: &str) -> Named {
    let mut out = Named::default();
    // Ids are kept in braces or square brackets, by more than one convention.
    let mut plain = String::new();
    let mut rest = folder;
    while let Some(open) = rest.find(['{', '[']) {
        let close = if rest[open..].starts_with('{') { '}' } else { ']' };
        let Some(len) = rest[open..].find(close) else { break };
        let tag = rest[open + 1..open + len].to_ascii_lowercase();
        if let Some((kind, id)) = tag.split_once(['-', '=', ':']) {
            match kind.trim().trim_end_matches("id") {
                "tmdb" => out.tmdb_id = id.trim().parse().ok(),
                "tvdb" => out.tvdb_id = id.trim().parse().ok(),
                "imdb" => out.imdb_id = Some(id.trim().to_string()).filter(|i| i.starts_with("tt")),
                _ => {}
            }
        }
        plain.push_str(&rest[..open]);
        rest = &rest[open + len + 1..];
    }
    plain.push_str(rest);
    let mut plain = plain.trim().to_string();

    // A release name: words joined by dots or underscores, the title ending where the year,
    // the season or the resolution begins.
    if !plain.contains(' ') && plain.contains(['.', '_']) {
        let mut words = vec![];
        for w in plain.split(['.', '_']) {
            let lower = w.to_ascii_lowercase();
            let season = lower.len() >= 3 && lower.starts_with('s') && lower[1..].chars().take(2).all(|c| c.is_ascii_digit());
            let resolution = lower.ends_with('p') && lower.len() >= 4 && lower[..lower.len() - 1].chars().all(|c| c.is_ascii_digit());
            if let Some(y) = is_year(w).filter(|_| !words.is_empty()) {
                out.year = y;
                break;
            }
            if season || resolution {
                break;
            }
            words.push(w);
        }
        out.name = words.join(" ");
        return out;
    }
    // "Title (1995)": the last bracketed year, with whatever follows it dropped.
    if let Some((at, y)) = plain.match_indices('(').filter_map(|(i, _)| plain[i + 1..].find(')').and_then(|len| is_year(&plain[i + 1..i + 1 + len])).map(|y| (i, y))).next_back() {
        if !plain[..at].trim().is_empty() {
            out.year = y;
            plain.truncate(at);
        }
    } else if let Some((head, tail)) = plain.rsplit_once(' ') {
        if let Some(y) = is_year(tail).filter(|_| !head.trim().is_empty()) {
            out.year = y;
            plain = head.to_string();
        }
    }
    out.name = plain.trim().trim_end_matches(['-', ',']).trim().to_string();
    out
}

/// A title reduced to what two spellings of it share.
fn plain(s: &str) -> String {
    s.to_lowercase().replace('&', " and ").chars().filter(|c| c.is_alphanumeric()).collect()
}

/// The one candidate a folder's name points to, if it points to one.
fn sure<'a>(named: &Named, candidates: &'a [Title]) -> Option<&'a Title> {
    let same: Vec<&Title> = candidates.iter().filter(|t| plain(&t.title) == plain(&named.name) || t.alt_titles.iter().any(|a| plain(a) == plain(&named.name))).collect();
    if named.year == 0 {
        // Without a year, a name shared by several things settles nothing.
        return (same.len() == 1).then(|| same[0]);
    }
    same.iter().find(|t| t.year == named.year).or_else(|| same.iter().find(|t| t.year.abs_diff(named.year) <= 1)).copied()
}

fn as_match(t: &Title) -> Match {
    Match { tmdb_id: t.tmdb_id, tvmaze_id: t.tvmaze_id, tvdb_id: t.tvdb_id, title: t.title.clone(), year: t.year, poster: t.poster.clone() }
}

/// A folder on a location, before anything has been asked about it.
struct Folder {
    path: PathBuf,
    files: usize,
    bytes: u64,
    /// What a media server's notes in the folder say it is.
    notes: crate::local::Notes,
}

/// The folders directly inside a library folder that hold a film or episodes, with how much.
/// Trailers and other extras do not count: a folder with only those holds nothing to add.
fn folders(root: &str, kind: Kind) -> Vec<Folder> {
    let Ok(rd) = std::fs::read_dir(root) else { return vec![] };
    let mut out = vec![];
    for e in rd.flatten() {
        let path = e.path();
        if !path.is_dir() || e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let (videos, local) = crate::local::survey(&path, kind);
        if !videos.is_empty() {
            let bytes = videos.iter().filter_map(|f| std::fs::metadata(path.join(f)).ok()).map(|m| m.len()).sum();
            out.push(Folder { path: path.clone(), files: videos.len(), bytes, notes: local.notes });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

impl App {
    /// Note that work on a location has stopped short, so nothing is left looking busy.
    pub fn adoption_failed(&self, why: &str) {
        self.adoption_set(|a| {
            a.running = false;
            a.step = "done".into();
            a.message = why.to_string();
        });
        if self.tasks.lock().get("adopt").is_some_and(|t| t.running) {
            self.task_end("adopt", why);
        }
        self.adoption_keep();
    }

    /// Keep what the work on a location came to, so it is still there to act on after
    /// another location has been looked through, or Spool has been restarted.
    fn adoption_keep(&self) {
        if let Some(a) = self.adoption.lock().clone().filter(|a| !a.running) {
            let _ = self.db.set_setting(&format!("found_{}", a.location), &a);
        }
    }

    /// What looking through a location found, or how far work on it has got: the work under
    /// way if it is on this location, or else what was kept from the last time.
    pub fn adoption_of(&self, location: u32) -> Option<Adoption> {
        if let Some(now) = self.adoption.lock().clone().filter(|a| a.location == location) {
            return Some(now);
        }
        Some(self.db.get_setting::<Adoption>(&format!("found_{location}"))).filter(|a| a.location == location && !a.message.is_empty())
    }

    fn adoption_set(&self, f: impl FnOnce(&mut Adoption)) {
        if let Some(a) = self.adoption.lock().as_mut() {
            f(a);
        }
    }

    /// Work out what one folder is.
    async fn recognise(&self, kind: Kind, f: &Folder, here: &[Title]) -> Found {
        let (folder, files, bytes) = (f.path.as_path(), f.files, f.bytes);
        let folder_name = folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut named = read_name(&folder_name);
        // What Plex, Jellyfin or Kodi wrote down about the folder says more than its name does.
        named.tmdb_id = named.tmdb_id.or(f.notes.tmdb_id);
        named.tvdb_id = named.tvdb_id.or(f.notes.tvdb_id);
        named.imdb_id = named.imdb_id.take().or(f.notes.imdb_id.clone());
        if named.year == 0 {
            named.year = f.notes.year;
        }
        let mut found = Found { folder: folder.to_string_lossy().to_string(), kind, name: named.name.clone(), year: named.year, files, bytes, state: "unknown".into(), note: String::new(), matches: vec![] };
        if let Some(t) = here.iter().find(|t| Path::new(&t.path) == folder || t.also.iter().any(|a| Path::new(a) == folder)) {
            found.state = "have".into();
            found.note = "already in the library".into();
            found.matches = vec![as_match(t)];
            return found;
        }
        let key = self.settings.general().tmdb_api_key;
        // An id in the folder's name settles it; otherwise the name is searched for.
        let by_id = match kind {
            Kind::Movie => named.tmdb_id.is_some() || named.imdb_id.is_some(),
            Kind::Series => named.tvdb_id.is_some(),
        };
        let looked: Result<Vec<Title>> = match kind {
            Kind::Movie => match (&named.tmdb_id, &named.imdb_id) {
                (Some(id), _) => self.meta.search_movies(&key, &format!("tmdb:{id}")).await,
                (_, Some(id)) => self.meta.search_movies(&key, id).await,
                _ if named.year > 0 => self.meta.search_movies(&key, &format!("{} ({})", named.name, named.year)).await,
                _ => self.meta.search_movies(&key, &named.name).await,
            },
            Kind::Series => match named.tvdb_id {
                Some(id) => self.meta.search_series(&format!("tvdb:{id}")).await,
                None => self.meta.search_series(&named.name).await,
            },
        };
        let mut candidates = match looked {
            Ok(c) => c,
            Err(e) => {
                found.note = format!("{e:#}");
                return found;
            }
        };
        // A folder cannot have a colon in its name, so "Show - The Subtitle" often stands for
        // "Show: The Subtitle". Asked for that way when the name as written finds nothing.
        if candidates.is_empty() && !by_id && named.name.contains(" - ") {
            let retold = named.name.replace(" - ", ": ");
            let again = match kind {
                Kind::Movie if named.year > 0 => self.meta.search_movies(&key, &format!("{retold} ({})", named.year)).await,
                Kind::Movie => self.meta.search_movies(&key, &retold).await,
                Kind::Series => self.meta.search_series(&retold).await,
            };
            if let Ok(more) = again {
                if !more.is_empty() {
                    named.name = retold;
                    candidates = more;
                }
            }
        }
        let chosen = if by_id { candidates.first() } else { sure(&named, &candidates) };
        match chosen {
            Some(t) => {
                found.matches = vec![as_match(t)];
                match self.db.find_title(kind, t.tmdb_id, t.tvdb_id).ok().flatten() {
                    // In the library and without a file, while this folder has one: a title
                    // brought over from another manager, whose files were here all along. It
                    // is the same title; what changes is where Spool looks for it.
                    Some(mine) if files > 0 && self.db.files(mine.id).map(|f| f.is_empty()).unwrap_or(false) => {
                        found.state = "new".into();
                        found.note = "in the library without a file; it will be pointed at this folder".into();
                    }
                    // In the library with its files somewhere else: this folder is a second copy
                    // of it, or holds the seasons the other lacks. Recorded beside the first.
                    Some(mine) if files > 0 && !mine.path.is_empty() => {
                        found.state = "new".into();
                        found.note = format!("also in the library, at {}; this folder will be kept as another copy of it", mine.path);
                    }
                    Some(mine) => {
                        found.state = "have".into();
                        found.note = if mine.path.is_empty() { "already in the library, with no files yet".into() } else { format!("already in the library, at {}", mine.path) };
                    }
                    None => found.state = "new".into(),
                }
            }
            None if candidates.is_empty() => found.note = "nothing by that name was found".into(),
            None => {
                found.state = "unclear".into();
                found.note = "more than one thing it could be".into();
                found.matches = candidates.iter().take(5).map(as_match).collect();
            }
        }
        found
    }

    /// Walk a location's folders and say what each is, changing nothing. Runs to completion;
    /// progress is in `adoption`.
    pub async fn adopt_look(&self, location_id: u32) -> Result<()> {
        let places = self.placement();
        let (loc, online) = places.0.iter().find(|p| p.0.id == location_id).cloned().ok_or_else(|| anyhow!("no such location"))?;
        if !online {
            bail!("{} cannot be reached right now", loc.name);
        }
        if !self.task_start("adopt", &format!("Looking through {}", loc.name)) {
            bail!("a location is already being looked through");
        }
        *self.adoption.lock() = Some(Adoption { location: location_id, running: true, step: "looking".into(), ..Default::default() });
        let roots = [(Kind::Movie, loc.movie_root.clone()), (Kind::Series, loc.series_root.clone())];
        let listed = tokio::task::spawn_blocking(move || roots.into_iter().filter(|(_, r)| !r.is_empty()).flat_map(|(k, r)| folders(&r, k).into_iter().map(move |f| (k, f))).collect::<Vec<_>>()).await.unwrap_or_default();
        self.adoption_set(|a| a.total = listed.len());
        let here = self.db.titles(None).unwrap_or_default();
        for (kind, folder) in listed {
            let found = self.recognise(kind, &folder, &here).await;
            self.adoption_set(|a| {
                a.done += 1;
                a.found.push(found);
            });
            // Unhurried: the metadata sources are shared with everyone else.
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        let count = |state: &str| self.adoption.lock().as_ref().map(|a| a.found.iter().filter(|f| f.state == state).count()).unwrap_or(0);
        let message = format!("{} to add, {} already in the library, {} unclear, {} not recognised", count("new"), count("have"), count("unclear"), count("unknown"));
        self.adoption_set(|a| {
            a.running = false;
            a.step = "done".into();
            a.message = message.clone();
        });
        self.task_end("adopt", &message);
        self.adoption_keep();
        Ok(())
    }

    /// Record one folder as a title where it lies.
    /// Point a title that is in the library without a file at the folder that holds it. Its
    /// monitoring and profile stay as they were; nothing is moved on either disk.
    async fn point_here(&self, mut mine: Title, folder: &str) -> Result<(Title, String)> {
        let from = std::mem::replace(&mut mine.path, folder.to_string());
        self.db.save_title(&mut mine)?;
        let mut report = crate::catalog::ScanReport::default();
        self.scan_title(mine.id, &mut report).await?;
        self.db.add_history(Some(mine.id), "adopted", serde_json::json!({"path": folder, "files": report.files_added, "was_at": from}))?;
        Ok((mine, String::new()))
    }

    /// Record a folder on another location as a further copy of a title that has its files
    /// elsewhere. A film there becomes a version to choose from; of a series, the episodes
    /// the title's own folder lacks are taken from it.
    async fn copy_here(&self, mut mine: Title, folder: &str) -> Result<(Title, String)> {
        if !mine.also.iter().any(|a| a == folder) {
            mine.also.push(folder.to_string());
            self.db.save_title(&mut mine)?;
        }
        let mut report = crate::catalog::ScanReport::default();
        self.scan_title(mine.id, &mut report).await?;
        self.db.add_history(Some(mine.id), "adopted", serde_json::json!({"path": folder, "files": report.files_added, "copy": true}))?;
        Ok((mine, String::new()))
    }

    /// A title already in the library that a folder turns out to hold: pointed at the folder
    /// if it had no file, or given the folder as a further copy if it had.
    async fn known_here(&self, mine: Title, folder: &str) -> Result<(Title, String)> {
        if self.db.files(mine.id)?.is_empty() {
            self.point_here(mine, folder).await
        } else {
            self.copy_here(mine, folder).await
        }
    }

    async fn adopt_one(&self, kind: Kind, folder: &str, m: &Match, monitor: bool) -> Result<(Title, String)> {
        let g = self.settings.general();
        // Known already by the ids the match carries: no need to ask the sources about it.
        if let Some(mine) = self.db.find_title(kind, m.tmdb_id, m.tvdb_id)? {
            return self.known_here(mine, folder).await;
        }
        let (mut title, episodes) = match kind {
            Kind::Movie => (self.meta.movie(&g.tmdb_api_key, m.tmdb_id.ok_or_else(|| anyhow!("no TMDB id"))?).await?, vec![]),
            Kind::Series => match (m.tvmaze_id, m.tvdb_id) {
                (Some(id), _) => self.meta.series(id).await?,
                (_, Some(id)) => self.meta.series_by_tvdb(id).await?,
                _ => bail!("no TVmaze or TVDB id"),
            },
        };
        if let Some(mine) = self.db.find_title(kind, title.tmdb_id, title.tvdb_id)? {
            return self.known_here(mine, folder).await;
        }
        title.path = folder.to_string();
        title.monitored = monitor;
        // The profile set for the place the folder is on: a shelf of remuxes brought in under
        // a profile meant for a small disk would be marked for shrinking.
        title.profile_id = self.profile_for_folder(kind, folder);
        title.added_at = now();
        title.last_refresh_at = now();
        self.db.save_title(&mut title)?;
        for mut e in episodes {
            e.title_id = title.id;
            e.monitored = monitor;
            self.db.upsert_episode(&e)?;
        }
        let mut report = crate::catalog::ScanReport::default();
        self.scan_title(title.id, &mut report).await?;
        // A film that runs much longer or shorter than the one it was taken for may be another
        // film: a cue to look, since nothing else about a folder says its name is wrong.
        let mut doubt = String::new();
        if kind == Kind::Movie && title.runtime > 0 {
            let ran = self.db.files(title.id)?.iter().filter(|f| !f.extra).filter_map(|f| f.media_info.as_ref()).map(|i| i.runtime_secs / 60.0).fold(0.0, f64::max);
            if ran > 1.0 && (ran - title.runtime as f64).abs() > (title.runtime as f64 * 0.25).max(12.0) {
                doubt = format!("the file runs {ran:.0} minutes and {} runs {}: check it is the right film", title.title, title.runtime);
            }
        }
        self.db.add_history(Some(title.id), "adopted", serde_json::json!({"path": folder, "files": report.files_added, "doubt": doubt}))?;
        Ok((title, doubt))
    }

    /// Add what the last look was sure of, and whatever `picks` settles. Runs to completion.
    pub async fn adopt_add(&self, location_id: u32, picks: Vec<Pick>, monitor: bool) -> Result<()> {
        let looked = self.adoption.lock().clone().filter(|a| a.location == location_id).ok_or_else(|| anyhow!("look through the location first"))?;
        if !self.placement().0.iter().any(|p| p.0.id == location_id && p.1) {
            bail!("the location cannot be reached right now");
        }
        let mut work: Vec<(usize, Match)> = vec![];
        for (i, f) in looked.found.iter().enumerate() {
            match picks.iter().find(|p| p.folder == f.folder) {
                Some(p) if f.state != "have" && f.state != "added" => work.push((i, Match { tmdb_id: p.tmdb_id, tvmaze_id: p.tvmaze_id, tvdb_id: p.tvdb_id, ..Default::default() })),
                None if f.state == "new" => work.extend(f.matches.first().cloned().map(|m| (i, m))),
                _ => {}
            }
        }
        if !self.task_start("adopt", &format!("Adding {} titles", work.len())) {
            bail!("a location is already being looked through");
        }
        self.adoption_set(|a| {
            (a.running, a.done, a.total) = (true, 0, work.len());
            a.step = "adding".into();
        });
        let mut added = 0;
        for (i, m) in work {
            let (kind, folder) = (looked.found[i].kind, looked.found[i].folder.clone());
            let result = self.adopt_one(kind, &folder, &m, monitor).await;
            self.adoption_set(|a| {
                a.done += 1;
                match &result {
                    Ok((t, doubt)) => {
                        a.found[i].state = "added".into();
                        a.found[i].note = doubt.clone();
                        a.found[i].matches = vec![as_match(t)];
                    }
                    Err(e) => {
                        a.found[i].state = "failed".into();
                        a.found[i].note = format!("{e:#}");
                    }
                }
            });
            if let Ok((t, _)) = result {
                added += 1;
                self.emit(Event::Title { id: t.id });
            }
        }
        let message = format!("Added {added} titles");
        self.adoption_set(|a| {
            a.running = false;
            a.step = "done".into();
            a.message = message.clone();
        });
        self.task_end("adopt", &message);
        self.adoption_keep();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_are_read_in_the_ways_people_write_them() {
        let n = |s: &str| read_name(s);
        assert_eq!((n("Heat (1995)").name.as_str(), n("Heat (1995)").year), ("Heat", 1995));
        assert_eq!(n("Heat (1995) {imdb-tt0113277}"), Named { name: "Heat".into(), year: 1995, imdb_id: Some("tt0113277".into()), ..Default::default() });
        assert_eq!(n("Heat (1995) [tmdbid-949]").tmdb_id, Some(949));
        assert_eq!(n("The Wire {tvdb-79126}"), Named { name: "The Wire".into(), tvdb_id: Some(79126), ..Default::default() });
        assert_eq!((n("Blade Runner 2049 (2017)").name.as_str(), n("Blade Runner 2049 (2017)").year), ("Blade Runner 2049", 2017));
        assert_eq!((n("Heat 1995").name.as_str(), n("Heat 1995").year), ("Heat", 1995));
        assert_eq!((n("Dangerous Liaisons (1988) Remux").name.as_str(), n("Dangerous Liaisons (1988) Remux").year), ("Dangerous Liaisons", 1988));
        assert_eq!((n("Heat.1995.1080p.BluRay.x264-GRP").name.as_str(), n("Heat.1995.1080p.BluRay.x264-GRP").year), ("Heat", 1995));
        assert_eq!(n("The.Wire.S01.1080p.BluRay.x264-GRP").name, "The Wire");
        assert_eq!((n("2001 A Space Odyssey (1968)").name.as_str(), n("2001 A Space Odyssey (1968)").year), ("2001 A Space Odyssey", 1968));
        assert_eq!(n("1917 (2019)").name, "1917");
        assert_eq!(n("Solaris").year, 0);
    }

    #[test]
    fn a_name_points_to_one_thing_or_to_none() {
        let t = |title: &str, year: u32| serde_json::from_value::<Title>(serde_json::json!({"kind": "movie", "title": title, "year": year})).unwrap();
        let list = vec![t("Solaris", 2002), t("Solaris", 1972), t("Solaris Rising", 2010)];
        assert_eq!(sure(&Named { name: "Solaris".into(), year: 1972, ..Default::default() }, &list).map(|t| t.year), Some(1972));
        assert!(sure(&Named { name: "Solaris".into(), ..Default::default() }, &list).is_none(), "two films share the name");
        assert!(sure(&Named { name: "Solaris".into(), year: 1999, ..Default::default() }, &list).is_none());
        let amp = vec![t("Fast & Furious", 2009)];
        assert_eq!(sure(&Named { name: "Fast and Furious".into(), year: 2010, ..Default::default() }, &amp).map(|t| t.year), Some(2009));
    }
}
