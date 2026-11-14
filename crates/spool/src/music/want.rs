//! Albums wanted. An album is found in MusicBrainz and added by its release group; Spool
//! looks for it in the indexers' audio categories, FLAC first and a 320k copy when there is
//! none, downloads it, checks it has the album's tracks, and files it in the music folder
//! as "Artist - Album (Year) [FLAC]".
//!
//! An album goes Wanted → Downloading → Have. A release that turns out not to do is
//! remembered and passed over, and the next best taken.

use super::musicbrainz::{self, Group};
use super::scan;
use crate::app::App;
use crate::db::{json, now, Db};
use crate::models::Release;
use crate::newznab::Query;
use anyhow::{anyhow, bail, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use spool_nntp::{JobState, JobStatus};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WantState {
    #[default]
    Wanted,
    Downloading,
    Have,
}

impl WantState {
    pub fn as_str(self) -> &'static str {
        match self {
            WantState::Wanted => "wanted",
            WantState::Downloading => "downloading",
            WantState::Have => "have",
        }
    }
}

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

/// A release that would not do, and why.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TurnedDown {
    pub guid: String,
    pub name: String,
    pub why: String,
    pub at: i64,
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
    pub state: WantState,
    /// What is happening to it, or what it waits for, in words; empty when nothing needs saying.
    pub note: String,
    pub release: Option<Release>,
    pub grade: Grade,
    pub job_id: Option<String>,
    pub turned_down: Vec<TurnedDown>,
    /// The folder it was filed in.
    pub folder: String,
    pub added_at: i64,
    pub updated_at: i64,
    pub searched_at: i64,
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
                c.execute("INSERT INTO music_wants(mbid, state, added_at, data) VALUES (?1, ?2, ?3, ?4)", params![w.mbid, w.state.as_str(), w.added_at, data])?;
                Ok(c.last_insert_rowid())
            })?;
        } else {
            self.with(|c| c.execute("UPDATE music_wants SET state = ?2, data = ?3 WHERE id = ?1", params![w.id, w.state.as_str(), data]))?;
        }
        Ok(())
    }

    /// Save only if it is still as it was at `rev`: not changed by anyone else, and not
    /// removed. Gives whether it was saved.
    pub fn save_music_want_if(&self, w: &mut Want, rev: i64) -> Result<bool> {
        w.updated_at = now();
        w.rev = rev + 1;
        let data = serde_json::to_string(w)?;
        let n = self.with(|c| c.execute("UPDATE music_wants SET state = ?2, data = ?3 WHERE id = ?1 AND mbid = ?4 AND json_extract(data, '$.rev') = ?5", params![w.id, w.state.as_str(), data, w.mbid, rev]))?;
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

/// The release to take: the best grade, then the indexer put first, then the newest.
fn best<'a>(found: &'a [(Release, Grade)], turned_down: &[TurnedDown]) -> Option<&'a (Release, Grade)> {
    found.iter().filter(|(r, _)| !turned_down.iter().any(|t| t.guid == r.guid || t.name == r.title)).max_by_key(|(r, g)| (*g, r.indexer_priority, r.published))
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

/// Files kept with an album beside its tracks.
const KEPT: &[&str] = &["jpg", "jpeg", "png", "webp", "cue", "log", "txt", "pdf"];

/// What a download holds that belongs in the album's folder, each with where it goes in it.
/// Tracks in folders of their own ("CD1", "Disc 2") keep those folders.
pub(crate) fn album_files(out: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let (audio, _) = scan::audio_files(out);
    if audio.is_empty() {
        bail!("the download held no audio files");
    }
    // The album's own folder: the deepest one all the tracks are under, above any disc folders.
    let folders: Vec<PathBuf> = audio.iter().map(|(p, ..)| scan::album_folder(p).0).collect();
    let mut root = folders[0].clone();
    while !folders.iter().all(|f| f.starts_with(&root)) {
        if !root.pop() {
            break;
        }
    }
    if !root.starts_with(out) {
        root = out.to_path_buf();
    }
    let mut files = vec![];
    let mut dirs = vec![root.clone()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d)?.flatten() {
            let p = e.path();
            let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') {
                continue;
            }
            if m.is_dir() {
                dirs.push(p);
            } else if m.is_file() {
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                if audio.iter().any(|(a, ..)| *a == p) || KEPT.contains(&ext.as_str()) {
                    let rel = p.strip_prefix(&root)?.to_path_buf();
                    files.push((p, rel));
                }
            }
        }
    }
    Ok(files)
}

/// Whether `dest` holds every file with the size it has in the download.
pub(crate) fn same_files(files: &[(PathBuf, PathBuf)], dest: &Path) -> bool {
    files.iter().all(|(from, rel)| match (std::fs::metadata(from), std::fs::metadata(dest.join(rel))) {
        (Ok(a), Ok(b)) => a.len() == b.len(),
        _ => false,
    })
}

/// Copy the album into `dest`, by way of a hidden folder beside it so that a copy cut short
/// is never taken for the album.
pub(crate) fn copy_album(files: &[(PathBuf, PathBuf)], dest: &Path) -> Result<()> {
    let parent = dest.parent().ok_or_else(|| anyhow!("no folder to file it in"))?;
    let part = parent.join(format!(".spool-{}", dest.file_name().and_then(|n| n.to_str()).unwrap_or("album")));
    if part.exists() {
        std::fs::remove_dir_all(&part)?;
    }
    let done = (|| -> Result<()> {
        for (from, rel) in files {
            let to = part.join(rel);
            if let Some(p) = to.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(from, &to).map_err(|e| anyhow!("copying {}: {e}", rel.display()))?;
        }
        std::fs::rename(&part, dest)?;
        Ok(())
    })();
    if done.is_err() {
        let _ = std::fs::remove_dir_all(&part);
    }
    done
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
                serde_json::json!({"cover": g.cover(), "group": g, "want_id": want.map(|w| w.id), "state": want.map(|w| w.state.as_str()), "have": have})
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
            w.state = WantState::Have;
            w.note = "Already in the music folder.".into();
        }
        self.db.save_music_want(&mut w)?;
        tracing::info!(album = %w.title, artist = %w.artist, "wanted an album");
        self.nudge_want(w.id);
        Ok(w)
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
        let _held = self.music.wanting.lock().await;
        let mut w = self.db.music_want(id)?.ok_or_else(|| anyhow!("no such album"))?;
        if w.state == WantState::Wanted {
            w.searched_at = 0;
            w.turned_down.clear();
            w.note = "Looking for it.".into();
            self.db.save_music_want(&mut w)?;
            self.nudge_want(id);
        }
        Ok(w)
    }

    /// Stop wanting an album, and stop its download. What was filed stays.
    pub async fn remove_want(&self, id: i64) -> Result<()> {
        let _held = self.music.wanting.lock().await;
        if let Some(w) = self.db.music_want(id)? {
            if let (WantState::Downloading, Some(job)) = (w.state, &w.job_id) {
                let _ = self.job_remove(job, true);
            }
        }
        self.db.delete_music_want(id)
    }

    /// The "music_wants" task: move along every album not yet here.
    pub async fn music_wants_task(&self) -> Result<String> {
        let wants = self.db.music_wants()?;
        let mut looked = 0;
        for w in wants.iter().filter(|w| w.state != WantState::Have) {
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
        if let Some(w) = wants.into_iter().find(|w| w.state == WantState::Downloading && w.job_id.as_deref() == Some(&job.id)) {
            self.nudge_want(w.id);
        }
    }

    /// Take an album as far as it can go now.
    pub async fn advance_want(&self, id: i64) -> Result<()> {
        let _held = self.music.wanting.lock().await;
        for _ in 0..4 {
            let Some(mut w) = self.db.music_want(id)? else { return Ok(()) };
            let rev = w.rev;
            let before = (w.state, w.note.clone());
            let job_before = w.job_id.clone();
            match w.state {
                WantState::Wanted => self.look_for_album(&mut w).await?,
                WantState::Downloading => self.follow_album(&mut w).await?,
                WantState::Have => return Ok(()),
            }
            if !self.db.save_music_want_if(&mut w, rev)? {
                // Changed meanwhile: a download it started would be followed by nothing.
                if let (WantState::Downloading, Some(job)) = (w.state, &w.job_id) {
                    if w.job_id != job_before {
                        let _ = self.job_remove(job, true);
                    }
                }
                return Ok(());
            }
            if (w.state, w.note.clone()) == before || w.state != WantState::Wanted {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn look_for_album(&self, w: &mut Want) -> Result<()> {
        if self.album_in_library(&w.artist, &w.title).is_some() {
            w.state = WantState::Have;
            w.note = "Already in the music folder.".into();
            return Ok(());
        }
        // An album not found is looked for again every six hours.
        if now() - w.searched_at < 6 * 3600 {
            return Ok(());
        }
        // Each release that did not do cost a download from an indexer's daily allowance. After
        // a few in an hour, the rest wait for the next.
        let lately = w.turned_down.iter().filter(|t| now() - t.at < 3600).count();
        if lately >= 3 {
            w.searched_at = now() - 5 * 3600;
            w.note = format!("{lately} releases did not do in the last hour. The next is tried in an hour.");
            return Ok(());
        }
        if !self.is_active() {
            w.note = "Spool is not downloading right now.".into();
            return Ok(());
        }
        if self.music_roots_to_file().is_empty() {
            w.note = "No music folder Spool manages can be reached to file it in.".into();
            return Ok(());
        }
        w.searched_at = now();
        let indexers: Vec<_> = self.db.indexers()?.into_iter().filter(|i| i.enable_search).collect();
        if indexers.is_empty() {
            w.note = "No indexer is set up to search.".into();
            return Ok(());
        }
        let mut found = vec![];
        let mut trouble = vec![];
        for name in album_names(&w.title) {
            // Plain words: indexers find nothing for a curly quote, a Unicode hyphen or a slash.
            let q = Query::Music { text: words(&format!("{} {name}", main_artist(&w.artist))).join(" ") };
            for ix in &indexers {
                match self.query_indexer(ix, &q, false).await {
                    Ok(rs) => found.extend(rs.into_iter().filter_map(|r| grade(&r.title, r.size, &w.artist, &name).map(|g| (r, g)))),
                    Err(e) => trouble.push(e.to_string()),
                }
            }
            if !found.is_empty() {
                break;
            }
        }
        let Some((r, g)) = best(&found, &w.turned_down).cloned() else {
            w.note = if !trouble.is_empty() && found.is_empty() {
                format!("The indexers could not be searched: {}. It is looked for again later.", trouble.join("; "))
            } else if found.is_empty() {
                "No FLAC or 320k release found. It is looked for again every six hours.".into()
            } else {
                "None of the releases found would do. It is looked for again every six hours.".into()
            };
            return Ok(());
        };
        match self.grab_release(&r).await {
            Ok(job) => {
                tracing::info!(album = %w.title, release = %r.title, "downloading an album");
                w.state = WantState::Downloading;
                w.job_id = Some(job);
                w.release = Some(r);
                w.grade = g;
                w.note.clear();
            }
            Err(Grab::Release(why)) => {
                w.turned_down.push(TurnedDown { guid: r.guid.clone(), name: r.title.clone(), why: why.clone(), at: now() });
                w.searched_at = 0;
                w.note = format!("A release would not do: {why}. Trying the next.");
            }
            // Nothing wrong with the release: it is tried again in an hour.
            Err(Grab::Later(why)) => {
                w.searched_at = now() - 5 * 3600;
                w.note = format!("Found {}, but it could not be started: {why}. It is tried again in an hour.", r.title);
            }
        }
        Ok(())
    }

    /// Fetch a release's NZB and start it downloading here.
    pub(crate) async fn grab_release(&self, r: &Release) -> std::result::Result<String, Grab> {
        let later = |s: String| Grab::Later(s);
        if self.settings.servers().iter().all(|s| !s.enabled || s.host.is_empty()) {
            return Err(later("no usenet server is configured".into()));
        }
        if self.indexer_budgets().get(&r.indexer_id).is_some_and(|b| b.grabs_spent()) {
            return Err(later(format!("{}'s download allowance for today is used up", r.indexer)));
        }
        let (free, _) = self.download_room(None);
        let reserve = self.settings.general().min_free_gb as u64 * (1 << 30);
        if free.is_some_and(|f| f < r.size * 2 + reserve) {
            return Err(later("not enough free space to download it".into()));
        }
        let client = self.nzb_client(r.indexer_id, &r.link).map_err(|why| later(format!("{} was not asked for the NZB: {why}", r.indexer)))?;
        let _ = self.db.count_indexer(r.indexer_id, 0, 1);
        let resp = client.get(self.nzb_link(r)).timeout(std::time::Duration::from_secs(150)).send().await.map_err(|e| later(format!("could not fetch the NZB from {}: {}", r.indexer, e.without_url())))?;
        match resp.status().as_u16() {
            200..=299 => {}
            // The indexer no longer has it.
            404 | 410 => return Err(Grab::Release(format!("{} no longer has it", r.indexer))),
            code => return Err(later(format!("{} answered HTTP {code} for the NZB", r.indexer))),
        }
        let nzb = crate::app::read_at_most(resp, crate::app::NZB_MOST).await.map_err(|why| Grab::Release(format!("the NZB {} sent was not kept: {why}", r.indexer)))?;
        if let Ok((found, asked)) = self.engine.availability(&nzb, 24).await {
            if asked > 0 && found * 100 < asked * 90 {
                return Err(Grab::Release("most of it has gone from the usenet servers".into()));
            }
        }
        self.engine.add(&nzb, &r.title, None).map_err(|e| Grab::Release(format!("the NZB could not be read: {e}")))
    }

    /// See how its download is getting on, and file it when it is done.
    async fn follow_album(&self, w: &mut Want) -> Result<()> {
        let Some(job_id) = w.job_id.clone() else {
            w.state = WantState::Wanted;
            return Ok(());
        };
        let job = match self.job_lookup(&job_id) {
            crate::remote::Found::Job(j) => j,
            crate::remote::Found::Unknown => return Ok(()),
            crate::remote::Found::Gone => {
                self.album_failed(w, "the download disappeared from the queue", false);
                return Ok(());
            }
        };
        match job.state {
            JobState::Failed => {
                let _ = self.job_remove(&job_id, true);
                let why = job.error.unwrap_or_else(|| "the download failed".into());
                self.album_failed(w, &why, true);
            }
            JobState::Completed => {
                let Some(out) = job.output_path else {
                    self.album_failed(w, "the download finished without a folder", true);
                    return Ok(());
                };
                match self.file_album(w, &out).await {
                    Ok(folder) => {
                        let _ = self.job_remove(&job_id, true);
                        self.remove_download_folder(Some(&job_id), &out).await;
                        tracing::info!(album = %w.title, folder = %folder, "filed an album");
                        let r = w.release.as_ref();
                        let _ = self.db.add_history(None, "album_filed", serde_json::json!({"want_id": w.id, "album": w.title, "artist": w.artist, "release": r.map(|r| &r.title), "indexer": r.map(|r| &r.indexer), "size": r.map(|r| r.size), "folder": folder}));
                        w.folder = folder;
                        w.state = WantState::Have;
                        w.job_id = None;
                        w.note.clear();
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
                        let _ = self.job_remove(&job_id, true);
                        self.remove_download_folder(Some(&job_id), &out).await;
                        self.album_failed(w, &why, true);
                    }
                    Err(Filing::Later(why)) => w.note = format!("Downloaded, but not filed yet: {why}. It is tried again later."),
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn album_failed(&self, w: &mut Want, why: &str, release_at_fault: bool) {
        tracing::info!(album = %w.title, why, "an album's download did not do");
        let r = w.release.as_ref();
        let _ = self.db.add_history(None, "album_failed", serde_json::json!({"want_id": w.id, "album": w.title, "artist": w.artist, "release": r.map(|r| &r.title), "indexer": r.map(|r| &r.indexer), "reason": why}));
        if let (true, Some(r)) = (release_at_fault, &w.release) {
            w.turned_down.push(TurnedDown { guid: r.guid.clone(), name: r.title.clone(), why: why.to_string(), at: now() });
        }
        w.state = WantState::Wanted;
        w.job_id = None;
        w.release = None;
        w.searched_at = 0;
        w.note = format!("A download did not do: {why}. Trying the next.");
    }

    /// Check a download is the album, and copy it into the music folder.
    async fn file_album(&self, w: &Want, out: &Path) -> std::result::Result<String, Filing> {
        let root = self.music_roots_to_file().into_iter().next().map(PathBuf::from).ok_or_else(|| Filing::Later("no music folder Spool manages can be reached".into()))?;
        let w = w.clone();
        let out = out.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let files = album_files(&out).map_err(|e| Filing::Wrong(format!("{e:#}")))?;
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
            copy_album(&files, &dest).map_err(|e| Filing::Later(format!("{e:#}")))?;
            Ok(dest.to_string_lossy().into_owned())
        })
        .await
        .map_err(|e| Filing::Later(e.to_string()))?
    }

    pub fn music_wants_status(&self) -> serde_json::Value {
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        for w in self.db.music_wants().unwrap_or_default() {
            *counts.entry(w.state.as_str()).or_default() += 1;
        }
        serde_json::json!(counts)
    }
}

pub(crate) enum Grab {
    /// The release is at fault, and is passed over from now on.
    Release(String),
    /// Something else is, and the release may be tried again.
    Later(String),
}

enum Filing {
    /// Not the album, or not all of it.
    Wrong(String),
    /// Could not be filed now, and may be later.
    Later(String),
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
    fn the_best_release_not_turned_down_is_taken() {
        let r = |guid: &str, p: i32| Release { guid: guid.into(), title: guid.into(), indexer_priority: p, ..Default::default() };
        let found = vec![(r("mp3", 50), Grade::Lossy), (r("flac-a", 10), Grade::Flac), (r("flac-b", 20), Grade::Flac)];
        assert_eq!(best(&found, &[]).unwrap().0.guid, "flac-b");
        let down = vec![TurnedDown { guid: "flac-b".into(), ..Default::default() }];
        assert_eq!(best(&found, &down).unwrap().0.guid, "flac-a");
        let down = vec![TurnedDown { guid: "flac-a".into(), ..Default::default() }, TurnedDown { guid: "flac-b".into(), ..Default::default() }];
        assert_eq!(best(&found, &down).unwrap().0.guid, "mp3");
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
        let files = album_files(&out).unwrap();
        let mut rel: Vec<String> = files.iter().map(|(_, r)| r.to_string_lossy().into_owned()).collect();
        rel.sort();
        assert_eq!(rel, ["CD1/01 a.flac", "CD2/01 b.flac", "cover.jpg"]);
        let dest = tmp.join("music/Artist - Album (2000) [FLAC]");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        copy_album(&files, &dest).unwrap();
        assert!(dest.join("CD2/01 b.flac").is_file());
        assert!(!tmp.join("music/.spool-Artist - Album (2000) [FLAC]").exists());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
