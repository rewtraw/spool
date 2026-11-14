//! Albums wanted. An album is found in MusicBrainz and added by its release group; Spool
//! looks for it in the indexers' audio categories, FLAC first and a 320k copy when there is
//! none, downloads it, checks it has the album's tracks, and files it in the music folder
//! as "Artist - Album (Year) [FLAC]".
//!
//! It goes Wanted → Downloading → Have by the steps every wanted thing takes; see [`crate::hunt`].

use super::musicbrainz::{self, Group};
use super::scan;
use crate::app::App;
use crate::db::{json, now, Db};
use crate::hunt::{copy_files, download_files, same_files, Filing, Followed, Hunt, HuntState, Search};
use crate::newznab::Query;
use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use spool_nntp::JobStatus;
use std::path::{Path, PathBuf};

/// How good a copy a release would give.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    #[default]
    Lossy,
    Flac,
}

impl Grade {
    pub fn label(self) -> &'static str {
        match self {
            Grade::Flac => "FLAC",
            Grade::Lossy => "320",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Want {
    pub id: i64,
    /// The MusicBrainz release group.
    pub mbid: String,
    pub title: String,
    pub artist: String,
    pub artist_mbid: String,
    pub year: i32,
    pub kind: String,
    /// The fewest and most tracks its editions have, when MusicBrainz knows.
    pub tracks: Option<(u32, u32)>,
    #[serde(flatten)]
    pub hunt: Hunt,
    pub grade: Grade,
    /// The folder it was filed in.
    pub folder: String,
    pub added_at: i64,
    pub updated_at: i64,
    pub account_id: Option<i64>,
    /// Counts the saves, so that work done on an album that was changed or removed meanwhile
    /// is not saved over the change.
    pub rev: i64,
}

impl Want {
    pub fn cover(&self) -> String {
        Group { mbid: self.mbid.clone(), ..Default::default() }.cover()
    }
}

impl Db {
    pub fn music_wants(&self) -> Result<Vec<Want>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM music_wants ORDER BY added_at DESC")?;
            let rows = s.query_map([], |r| {
                let mut w: Want = json(r.get(1)?)?;
                w.id = r.get(0)?;
                Ok(w)
            })?;
            rows.collect()
        })
    }

    pub fn music_want(&self, id: i64) -> Result<Option<Want>> {
        self.with(|c| {
            c.query_row("SELECT id, data FROM music_wants WHERE id = ?1", [id], |r| {
                let mut w: Want = json(r.get(1)?)?;
                w.id = r.get(0)?;
                Ok(w)
            })
            .optional()
        })
    }

    pub fn music_want_by_group(&self, mbid: &str) -> Result<Option<i64>> {
        self.with(|c| c.query_row("SELECT id FROM music_wants WHERE mbid = ?1", [mbid], |r| r.get(0)).optional())
    }

    pub fn save_music_want(&self, w: &mut Want) -> Result<()> {
        w.updated_at = now();
        w.rev += 1;
        let data = serde_json::to_string(w)?;
        if w.id == 0 {
            w.id = self.with(|c| {
                c.execute("INSERT INTO music_wants(mbid, state, added_at, data) VALUES (?1, ?2, ?3, ?4)", params![w.mbid, w.hunt.state.as_str(), w.added_at, data])?;
                Ok(c.last_insert_rowid())
            })?;
        } else {
            self.with(|c| c.execute("UPDATE music_wants SET state = ?2, data = ?3 WHERE id = ?1", params![w.id, w.hunt.state.as_str(), data]))?;
        }
        Ok(())
    }

    /// Save only if it is still as it was at `rev`: not changed by anyone else, and not
    /// removed. Gives whether it was saved.
    pub fn save_music_want_if(&self, w: &mut Want, rev: i64) -> Result<bool> {
        w.updated_at = now();
        w.rev = rev + 1;
        let data = serde_json::to_string(w)?;
        let n = self.with(|c| c.execute("UPDATE music_wants SET state = ?2, data = ?3 WHERE id = ?1 AND mbid = ?4 AND json_extract(data, '$.rev') = ?5", params![w.id, w.hunt.state.as_str(), data, w.mbid, rev]))?;
        Ok(n > 0)
    }

    pub fn delete_music_want(&self, id: i64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM music_wants WHERE id = ?1", [id]))?;
        Ok(())
    }
}

/// Words as they are compared: lower case, without accents, "&" as "and", punctuation gone.
pub(crate) fn words(s: &str) -> Vec<String> {
    // "Tomorrow's" and "Tomorrows" are the same word.
    let s = deunicode::deunicode(s).to_lowercase().replace('&', " and ").replace('\'', "");
    s.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// Whether every word of `want` is in `name`, in any order. "The" and "and" may be left out.
fn has_all(name: &[String], want: &str) -> bool {
    let w = words(want);
    let needed: Vec<&String> = w.iter().filter(|w| !matches!(w.as_str(), "the" | "and" | "a")).collect();
    let needed = if needed.is_empty() { w.iter().collect() } else { needed };
    !needed.is_empty() && needed.iter().all(|w| name.contains(w))
}

/// Whether an album in the library is the one named. Its title may carry more after the name
/// when it is set off: "Diamond-Jubilee - REALISTIK MASTER", "OK Computer (Remastered)"; but
/// "Play & Play: B Sides" is not "Play".
pub(crate) fn same_album(have: &str, want: &str) -> bool {
    let (h, w) = (words(have), words(want));
    if w.is_empty() || !h.starts_with(&w) {
        return false;
    }
    h.len() == w.len() || crate::books::audio::separated(have).get(w.len()).copied().unwrap_or(false)
}

/// What a release would give, or None when it is not the album or not good enough.
/// Whether a release's name says it is this album by this artist: the album's name straight
/// after the artist's, perhaps with a year between ("Kraftwerk - 1970 - Kraftwerk"), and
/// "The" left off either. Every word of each somewhere in the name is not enough: every
/// release of Kraftwerk's has the words of their first album, "Kraftwerk", in it.
fn names_album(n: &[String], artist: &str, album: &str) -> bool {
    names_album_by(n, artist, album) || names_album_by(n, main_artist(artist), album)
}

/// Who an album is posted under: an album credited to "Alice Coltrane featuring Pharoah
/// Sanders" is Alice Coltrane's.
fn main_artist(artist: &str) -> &str {
    let lower = artist.to_lowercase();
    [" featuring ", " feat. ", " feat ", " ft. ", " with "].iter().filter_map(|sep| lower.find(sep)).min().map_or(artist, |at| &artist[..at])
}

/// What an album may be posted as: its whole name, and, for a name with a part after a comma
/// or colon ("Music From the Unrealized Film Script, Dusk at Cubist Castle"), that part.
fn album_names(title: &str) -> Vec<String> {
    let mut out = vec![title.to_string()];
    if let Some(at) = title.rfind([':', ',']) {
        let part = title[at + 1..].trim();
        if words(part).len() >= 2 {
            out.push(part.to_string());
        }
    }
    out
}

fn names_album_by(n: &[String], artist: &str, album: &str) -> bool {
    let bare = |w: Vec<String>| if w.len() > 1 && w[0] == "the" { w[1..].to_vec() } else { w };
    let (a, t) = (bare(words(artist)), bare(words(album)));
    if a.is_empty() || t.is_empty() {
        return false;
    }
    let year = |w: &String| w.len() == 4 && (w.starts_with("19") || w.starts_with("20")) && w.chars().all(|c| c.is_ascii_digit());
    (0..n.len()).filter(|&i| n[i..].starts_with(&a)).any(|i| {
        let mut rest = &n[i + a.len()..];
        if rest.first().is_some_and(year) {
            rest = &rest[1..];
        }
        if rest.first().is_some_and(|w| w == "the") && t[0] != "the" {
            rest = &rest[1..];
        }
        rest.starts_with(&t)
    })
}

/// A piece of a bigger post rather than a release: `[05 of 20] "04. Track.flac"`, `(02/21)`,
/// or a name that ends in a file's, `… - 04-Vein Melter.flac`.
pub(crate) fn a_piece(name: &str) -> bool {
    let n = name.to_lowercase();
    // A scene name is dotted throughout ("Artist.Album.2010.FLAC"); a file's has spaces.
    let file = n.trim().contains(' ') && n.trim().rsplit_once('.').is_some_and(|(_, ext)| matches!(ext, "flac" | "mp3" | "m4a" | "wav" | "ogg" | "rar" | "zip" | "par2" | "nfo" | "sfv" | "jpg" | "cue" | "log") || (ext.starts_with('r') && ext[1..].chars().all(|c| c.is_ascii_digit()) && ext.len() == 3));
    file || n.contains('"') || n.split(|c: char| !c.is_ascii_alphanumeric() && c != '/').any(|w| w.split_once('/').is_some_and(|(a, b)| !a.is_empty() && !b.is_empty() && a.chars().chain(b.chars()).all(|c| c.is_ascii_digit())))
        || n.split_whitespace().collect::<Vec<_>>().windows(3).any(|w| w[1] == "of" && w[0].trim_start_matches('[').parse::<u32>().is_ok() && w[2].trim_end_matches(']').parse::<u32>().is_ok())
}

pub fn grade(name: &str, size: u64, artist: &str, album: &str) -> Option<Grade> {
    let n = words(name);
    if a_piece(name) || !names_album(&n, artist, album) {
        return None;
    }
    // Everything an artist made, or a box of albums, is not the one album.
    if n.iter().any(|w| matches!(w.as_str(), "discography" | "discografia" | "anthology")) && !has_all(&words(album), "anthology") {
        return None;
    }
    if !(10 << 20..=6u64 << 30).contains(&size) {
        return None;
    }
    let has = |w: &str| n.iter().any(|x| x == w);
    if has("flac") || has("lossless") || has("alac") {
        return Some(Grade::Flac);
    }
    if has("320") || has("320k") || has("320kbps") || has("cbr320") {
        return Some(Grade::Lossy);
    }
    None
}

/// A name that can be a folder: no slashes, colons and the like, and not too long.
pub(crate) fn folder_safe(s: &str) -> String {
    let s: String = s.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { ' ' } else { c }).collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = s.trim_matches(['.', ' ']).to_string();
    s.chars().take(180).collect()
}

pub fn folder_name(w: &Want, grade: Grade) -> String {
    let year = if w.year > 0 { format!(" ({})", w.year) } else { String::new() };
    folder_safe(&format!("{} - {}{year} [{}]", w.artist, w.title, if grade == Grade::Flac { "FLAC" } else { "MP3" }))
}

impl App {
    /// Albums whose names match what was typed, each marked if it is wanted or already here.
    pub async fn album_lookup(&self, q: &str) -> Result<Vec<serde_json::Value>> {
        let found = musicbrainz::search(&self.http, q).await?;
        Ok(self.album_rows(found))
    }

    /// The artist named by what was typed, when it names one outright, and their own albums
    /// and EPs. Asked for beside [`App::album_lookup`], whose answer comes sooner.
    pub async fn artist_lookup(&self, q: &str) -> Result<serde_json::Value> {
        let Some(artist) = musicbrainz::artist(&self.http, q).await? else { return Ok(serde_json::json!({"artist": null, "albums": []})) };
        let own = musicbrainz::discography(&self.http, &artist.mbid).await?;
        Ok(serde_json::json!({"artist": artist, "albums": self.album_rows(own)}))
    }

    fn album_rows(&self, found: Vec<Group>) -> Vec<serde_json::Value> {
        let wants = self.db.music_wants().unwrap_or_default();
        found
            .into_iter()
            .map(|g| {
                let want = wants.iter().find(|w| w.mbid == g.mbid);
                let have = self.album_in_library(&g.artist, &g.title).is_some();
                serde_json::json!({"cover": g.cover(), "group": g, "want_id": want.map(|w| w.id), "state": want.map(|w| w.hunt.state.as_str()), "have": have})
            })
            .collect()
    }

    /// The album in the library by this artist with this name, if there is one.
    pub fn album_in_library(&self, artist: &str, title: &str) -> Option<i64> {
        let lib = self.music.library();
        let a = scan::artist_key(artist);
        lib.albums.iter().find(|x| scan::artist_key(&x.artist) == a && same_album(&x.title, title)).map(|x| x.id)
    }

    pub async fn want_album(&self, mbid: &str, account: Option<i64>) -> Result<Want> {
        if let Some(id) = self.db.music_want_by_group(mbid)? {
            return self.db.music_want(id)?.ok_or_else(|| anyhow!("the album went"));
        }
        let g = musicbrainz::group(&self.http, mbid).await?;
        let tracks = musicbrainz::track_counts(&self.http, mbid).await.unwrap_or_else(|e| {
            tracing::warn!(album = %g.title, error = %e, "track counts not known");
            None
        });
        let mut w = Want { mbid: g.mbid.clone(), title: g.title, artist: g.artist, artist_mbid: g.artist_mbid, year: g.year, kind: g.kind, tracks, added_at: now(), account_id: account, ..Default::default() };
        if self.album_in_library(&w.artist, &w.title).is_some() {
            w.hunt.found("Already in the music folder.");
        }
        self.db.save_music_want(&mut w)?;
        tracing::info!(album = %w.title, artist = %w.artist, "wanted an album");
        self.nudge_want(w.id);
        Ok(w)
    }

    fn want_lock(&self, id: i64) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        self.music.want_locks.lock().entry(id).or_default().clone()
    }

    /// Move an album along in the background.
    pub fn nudge_want(&self, id: i64) {
        let app = self.clone();
        tokio::spawn(async move {
            if let Err(e) = app.advance_want(id).await {
                tracing::warn!(want = id, error = %e, "an album could not be moved along");
            }
        });
    }

    /// Look for it again now.
    /// Releases turned down before are given another chance.
    pub async fn search_want(&self, id: i64) -> Result<Want> {
        let _held = self.want_lock(id).lock_owned().await;
        let mut w = self.db.music_want(id)?.ok_or_else(|| anyhow!("no such album"))?;
        if w.hunt.search_again() {
            self.db.save_music_want(&mut w)?;
            self.nudge_want(id);
        }
        Ok(w)
    }

    /// Stop wanting an album, and stop its download. What was filed stays.
    pub async fn remove_want(&self, id: i64) -> Result<()> {
        let _held = self.want_lock(id).lock_owned().await;
        if let Some(w) = self.db.music_want(id)? {
            if let Some(job) = w.hunt.download() {
                let _ = self.job_remove(job, true);
            }
        }
        self.db.delete_music_want(id)
    }

    /// The "music_wants" task: move along every album not yet here.
    pub async fn music_wants_task(&self) -> Result<String> {
        let wants = self.db.music_wants()?;
        let mut looked = 0;
        for w in wants.iter().filter(|w| w.hunt.state != HuntState::Have) {
            looked += 1;
            if let Err(e) = self.advance_want(w.id).await {
                tracing::warn!(album = %w.title, error = %e, "an album could not be moved along");
            }
        }
        let covers = match self.music_covers().await {
            Ok(0) => String::new(),
            Ok(n) => format!("; {n} covers found"),
            Err(e) => format!("; covers: {e}"),
        };
        Ok(format!("{looked} albums looked at{covers}"))
    }

    /// A download finished or failed: if it was an album's, take it from there.
    pub fn music_job_done(&self, job: &JobStatus) {
        let Ok(wants) = self.db.music_wants() else { return };
        if let Some(w) = wants.into_iter().find(|w| w.hunt.download() == Some(&job.id)) {
            self.nudge_want(w.id);
        }
    }

    /// Take an album as far as it can go now.
    pub async fn advance_want(&self, id: i64) -> Result<()> {
        // Searches take turns; following and filing a download do not wait for them. A turn is
        // taken before the album itself, so a removal never waits behind the queue.
        let searching = |w: &Option<Want>| w.as_ref().is_some_and(|w| w.hunt.state == HuntState::Wanted);
        let mut turn = if searching(&self.db.music_want(id)?) { Some(self.music.wanting.lock().await) } else { None };
        let mut held = self.want_lock(id).lock_owned().await;
        for _ in 0..4 {
            let Some(mut w) = self.db.music_want(id)? else { return Ok(()) };
            if w.hunt.state == HuntState::Wanted && turn.is_none() {
                drop(held);
                turn = Some(self.music.wanting.lock().await);
                held = self.want_lock(id).lock_owned().await;
                continue;
            }
            let rev = w.rev;
            let before = (w.hunt.state, w.hunt.note.clone());
            let job_before = w.hunt.job_id.clone();
            match w.hunt.state {
                HuntState::Wanted => self.look_for_album(&mut w).await?,
                HuntState::Downloading => self.follow_album(&mut w).await,
                HuntState::Have => return Ok(()),
            }
            if !self.db.save_music_want_if(&mut w, rev)? {
                // Changed meanwhile: a download it started would be followed by nothing.
                if let Some(job) = w.hunt.download().filter(|j| job_before.as_deref() != Some(*j)) {
                    let _ = self.job_remove(job, true);
                }
                return Ok(());
            }
            if (w.hunt.state, w.hunt.note.clone()) == before || w.hunt.state != HuntState::Wanted {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn look_for_album(&self, w: &mut Want) -> Result<()> {
        if self.album_in_library(&w.artist, &w.title).is_some() {
            w.hunt.found("Already in the music folder.");
            return Ok(());
        }
        let names = album_names(&w.title);
        let (artist, title) = (w.artist.clone(), w.title.clone());
        let search = Search {
            what: "album",
            name: &title,
            no_folder: self.music_roots_to_file().is_empty().then_some("No music folder Spool manages can be reached to file it in."),
            // Plain words: indexers find nothing for a curly quote, a Unicode hyphen or a slash.
            queries: names.iter().map(|name| Query::Music { text: words(&format!("{} {name}", main_artist(&artist))).join(" ") }).collect(),
            grade: &|r, n| grade(&r.title, r.size, &artist, &names[n]),
            none: "No FLAC or 320k release found.",
        };
        if let Some(g) = self.hunt_search(&mut w.hunt, search).await? {
            w.grade = g;
        }
        Ok(())
    }

    /// See how its download is getting on, and file it when it is done.
    async fn follow_album(&self, w: &mut Want) {
        match self.hunt_follow(&mut w.hunt) {
            Followed::Going => {}
            Followed::Failed(why, at_fault) => self.album_failed(w, &why, at_fault),
            Followed::Finished(job, out) => match self.file_album(w, &out).await {
                Ok(folder) => {
                    self.hunt_clear(&job, &out).await;
                    tracing::info!(album = %w.title, folder = %folder, "filed an album");
                    let r = w.hunt.release.as_ref();
                    let _ = self.db.add_history(None, "album_filed", serde_json::json!({"want_id": w.id, "album": w.title, "artist": w.artist, "release": r.map(|r| &r.title), "indexer": r.map(|r| &r.indexer), "size": r.map(|r| r.size), "folder": folder}));
                    w.folder = folder;
                    w.hunt.filed();
                    // Read straight away, so the album is there to play: after any read
                    // already under way, which may have passed its folder by.
                    let app = self.clone();
                    tokio::spawn(async move {
                        for _ in 0..360 {
                            if app.task_start("music", "Running") {
                                let m = app.music_scan().await.unwrap_or_else(|e| format!("Failed: {e}"));
                                app.task_end("music", &m);
                                return;
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                        }
                    });
                }
                Err(Filing::Wrong(why)) => {
                    self.hunt_clear(&job, &out).await;
                    self.album_failed(w, &why, true);
                }
                Err(Filing::Later(why)) => w.hunt.not_filed_yet(&why),
            },
        }
    }

    fn album_failed(&self, w: &mut Want, why: &str, release_at_fault: bool) {
        tracing::info!(album = %w.title, why, "an album's download did not do");
        let r = w.hunt.release.as_ref();
        let _ = self.db.add_history(None, "album_failed", serde_json::json!({"want_id": w.id, "album": w.title, "artist": w.artist, "release": r.map(|r| &r.title), "indexer": r.map(|r| &r.indexer), "reason": why}));
        w.hunt.failed(why, release_at_fault);
    }

    /// Check a download is the album, and copy it into the music folder.
    async fn file_album(&self, w: &Want, out: &Path) -> std::result::Result<String, Filing> {
        let root = self.music_roots_to_file().into_iter().next().map(PathBuf::from).ok_or_else(|| Filing::Later("no music folder Spool manages can be reached".into()))?;
        let w = w.clone();
        let out = out.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let files = download_files(&out).map_err(|e| Filing::Wrong(format!("{e:#}")))?;
            let audio: Vec<&(PathBuf, PathBuf)> = files.iter().filter(|(p, _)| scan::is_audio(p)).collect();
            if let Some((least, most)) = w.tracks {
                let n = audio.len() as u32;
                if n < least {
                    return Err(Filing::Wrong(format!("it has {n} tracks, and the album has {least}")));
                }
                if n > most * 2 + 2 {
                    return Err(Filing::Wrong(format!("it has {n} tracks, far more than the album's {most}")));
                }
            }
            // The tags of what came are a last check that it is the album, where they say anything.
            let read: Vec<scan::Read> = audio.iter().map(|(p, _)| {
                let m = std::fs::metadata(p).ok();
                scan::read_file(p, m.map(|m| m.len()).unwrap_or(0), 0)
            }).collect();
            let tagged = read.iter().filter(|r| !r.album.is_empty()).count();
            let matching = read.iter().filter(|r| !r.album.is_empty() && (has_all(&words(&r.album), &w.title) || has_all(&words(&w.title), &r.album))).count();
            if tagged > 0 && matching * 2 < tagged {
                let said = read.iter().find(|r| !r.album.is_empty()).map(|r| r.album.clone()).unwrap_or_default();
                return Err(Filing::Wrong(format!("its tags say it is \u{201c}{said}\u{201d}")));
            }
            let flac = read.iter().any(|r| matches!(r.track.suffix.as_str(), "flac" | "wav" | "aiff" | "aif" | "wv" | "ape") || (r.track.suffix == "m4a" && r.track.bit_depth > 0));
            let dest = root.join(folder_name(&w, if flac { Grade::Flac } else { Grade::Lossy }));
            if dest.exists() {
                // Copied before Spool was stopped, perhaps: the same files, the same sizes.
                if same_files(&files, &dest) {
                    return Ok(dest.to_string_lossy().into_owned());
                }
                return Err(Filing::Later(format!("{} is already there", dest.display())));
            }
            copy_files(&files, &dest).map_err(|e| Filing::Later(format!("{e:#}")))?;
            Ok(dest.to_string_lossy().into_owned())
        })
        .await
        .map_err(|e| Filing::Later(e.to_string()))?
    }

    pub fn music_wants_status(&self) -> serde_json::Value {
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        for w in self.db.music_wants().unwrap_or_default() {
            *counts.entry(w.hunt.state.as_str()).or_default() += 1;
        }
        serde_json::json!(counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_are_graded_by_name() {
        let mb = 1 << 20;
        assert_eq!(grade("Radiohead-OK_Computer-REMASTERED-24BIT-WEB-FLAC-2017-FAiNT", 900 * mb, "Radiohead", "OK Computer"), Some(Grade::Flac));
        assert_eq!(grade("Radiohead - OK Computer (1997) [MP3 320]", 120 * mb, "Radiohead", "OK Computer"), Some(Grade::Lossy));
        assert_eq!(grade("Radiohead - OK Computer (1997) [MP3 V0]", 100 * mb, "Radiohead", "OK Computer"), None, "not 320");
        assert_eq!(grade("Radiohead - Kid A [FLAC]", 400 * mb, "Radiohead", "OK Computer"), None, "another album");
        assert_eq!(grade("Radiohead Discography 1993-2016 FLAC", 4000 * mb, "Radiohead", "OK Computer"), None, "all of them");
        assert_eq!(grade("Simon.and.Garfunkel-Bookends-FLAC", 300 * mb, "Simon & Garfunkel", "Bookends"), Some(Grade::Flac));
        assert_eq!(grade("Beatles - Abbey Road FLAC", 400 * mb, "The Beatles", "Abbey Road"), Some(Grade::Flac), "The may be left out");
        assert_eq!(grade("Radiohead - OK Computer FLAC", mb, "Radiohead", "OK Computer"), None, "too small to be it");
        assert_eq!(grade("Boards.Of.Canada-Tomorrows.Harvest-2013-FLAC", 400 * mb, "Boards of Canada", "Tomorrow’s Harvest"), Some(Grade::Flac));
        assert_eq!(grade("Boards.Of.Canada-The.Campfire.Headphase-FLAC", 400 * mb, "Boards of Canada", "The Campfire Headphase"), Some(Grade::Flac));
    }

    #[test]
    fn a_self_titled_album_is_not_every_album_of_its_artist() {
        let mb = 1 << 20;
        let k = |name: &str, album: &str| grade(name, 300 * mb, "Kraftwerk", album);
        assert_eq!(k("Kraftwerk-Kraftwerk-BOOTLEG.REISSUE-CD-FLAC-1994-401", "Kraftwerk"), Some(Grade::Flac));
        assert_eq!(k("Kraftwerk - 1970 - Kraftwerk 1 FLAC", "Kraftwerk"), Some(Grade::Flac));
        assert_eq!(k("Kraftwerk-Greatest Hits [1999] FLAC", "Kraftwerk"), None);
        assert_eq!(k("Kraftwerk-The.Mix-REMASTERED-2LP-FLAC-2020-BEATOCUL", "Kraftwerk"), None);
        assert_eq!(k("Kraftwerk-Autobahn-REMASTERED-LP-FLAC-2020-BEATOCUL", "Kraftwerk"), None);
        assert_eq!(k("Kraftwerk And Organisation-Tone Float-BOOTLEG-CD-FLAC-1996-dL", "Kraftwerk 2"), None);
        assert_eq!(k("Kraftwerk.-.Minimum-Maximum.2005.24bit.SACD.LOSSLESS.FLAC.Disc.2", "Kraftwerk 2"), None);
        assert_eq!(k("Kraftwerk - Die Mensch-Maschine (2009 Remaster) (1978) FLAC", "Die Mensch·Maschine"), Some(Grade::Flac));
        assert_eq!(k(r#"Kraftwerk - 1970 - Kraftwerk 1 (NMR) [05 of 20] "04. Von Himmel Hoch.flac""#, "Kraftwerk"), None, "one file of a post");
        assert_eq!(k(r#"(02/21) "Kraftwerk-2004-Aerodynamik (EP) FLAC.part01.rar" - 185,29 MB"#, "Aerodynamik"), None);
        assert_eq!(grade("Herbie Hancock - 1973 - Head Hunters [DR] [2013 Reissue] [24-96] - 04-Vein Melter.flac", 300 * mb, "Herbie Hancock", "Head Hunters"), None, "one track posted alone");
        assert_eq!(grade("Herbie Hancock - 1973 - Head Hunters [2013 Reissue] [24-96] FLAC", 300 * mb, "Herbie Hancock", "Head Hunters"), Some(Grade::Flac));
        assert_eq!(grade("Herbie.Hancock-Head.Hunters-1973.FLAC", 300 * mb, "Herbie Hancock", "Head Hunters"), Some(Grade::Flac), "a dotted name is a release");
        assert_eq!(grade("Alice Coltrane - Journey In Satchidananda (1971) [FLAC]", 300 * mb, "Alice Coltrane featuring Pharoah Sanders", "Journey in Satchidananda"), Some(Grade::Flac));
    }

    #[test]
    fn an_album_in_the_library_may_say_more_than_its_name() {
        assert!(same_album("Diamond-Jubilee - REALISTIK MASTER - March 27 2024", "Diamond Jubilee"));
        assert!(same_album("OK Computer", "OK Computer"));
        assert!(!same_album("Kid A", "OK Computer"));
        assert!(!same_album("Untrue", ""));
        assert!(same_album("OK Computer (Remastered)", "OK Computer"));
        assert!(!same_album("Play & Play: B Sides", "Play"), "more that runs straight on is another album");
    }

    #[test]
    fn a_long_name_is_also_looked_for_by_its_last_part() {
        assert_eq!(album_names("Music From the Unrealized Film Script, Dusk at Cubist Castle"), ["Music From the Unrealized Film Script, Dusk at Cubist Castle", "Dusk at Cubist Castle"]);
        assert_eq!(album_names("Selected Ambient Works 85–92"), ["Selected Ambient Works 85–92"]);
        assert_eq!(album_names("Live: 1975"), ["Live: 1975"], "one word is not enough to go on");
        assert_eq!(main_artist("Alice Coltrane featuring Pharoah Sanders"), "Alice Coltrane");
        assert_eq!(words("Neu! ’75").join(" "), "neu 75");
        assert_eq!(words("Transient Random‐Noise Bursts").join(" "), "transient random noise bursts");
    }

    #[test]
    fn albums_are_filed_under_a_clean_name() {
        let w = Want { artist: "AC/DC".into(), title: "Back in Black: Deluxe?".into(), year: 1980, ..Default::default() };
        assert_eq!(folder_name(&w, Grade::Flac), "AC DC - Back in Black Deluxe (1980) [FLAC]");
        let w = Want { artist: "Burial".into(), title: "Untrue".into(), ..Default::default() };
        assert_eq!(folder_name(&w, Grade::Lossy), "Burial - Untrue [MP3]");
    }

    #[test]
    fn a_download_is_copied_with_its_disc_folders_and_pictures() {
        let tmp = std::env::temp_dir().join(format!("spool-want-{}", std::process::id()));
        let out = tmp.join("Some.Release-FLAC");
        let album = out.join("Artist - Album");
        std::fs::create_dir_all(album.join("CD1")).unwrap();
        std::fs::create_dir_all(album.join("CD2")).unwrap();
        std::fs::write(album.join("CD1/01 a.flac"), b"a").unwrap();
        std::fs::write(album.join("CD2/01 b.flac"), b"b").unwrap();
        std::fs::write(album.join("cover.jpg"), b"c").unwrap();
        std::fs::write(out.join("release.nfo"), b"n").unwrap();
        let files = download_files(&out).unwrap();
        let mut rel: Vec<String> = files.iter().map(|(_, r)| r.to_string_lossy().into_owned()).collect();
        rel.sort();
        assert_eq!(rel, ["CD1/01 a.flac", "CD2/01 b.flac", "cover.jpg"]);
        let dest = tmp.join("music/Artist - Album (2000) [FLAC]");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        copy_files(&files, &dest).unwrap();
        assert!(dest.join("CD2/01 b.flac").is_file());
        assert!(!tmp.join("music/.spool-Artist - Album (2000) [FLAC]").exists());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
