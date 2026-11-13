//! Reading the music folders: every audio file's tags and properties, gathered into albums.
//!
//! A file whose size and modification time are as they were last time is not read again, so
//! a scan after the first one only lists the folders.

use super::Track;
#[cfg(test)]
use super::{Album, Artist, Library};
use crate::app::App;
use crate::db::now;
use anyhow::Result;
use lofty::prelude::*;
use rusqlite::params;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const AUDIO: &[&str] = &["flac", "mp3", "m4a", "mp4", "aac", "alac", "ogg", "oga", "opus", "wav", "aif", "aiff", "wv", "ape"];
const COVERS: &[&str] = &["cover", "folder", "front", "album", "albumart", "artwork"];
const IMAGES: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// One file as read: the track, and what its tags say of the album it is on.
#[derive(Clone, Debug, Default)]
pub struct Read {
    pub track: Track,
    pub album: String,
    pub album_artist: String,
    pub mb_album: String,
}

fn ext(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

fn hidden(p: &Path) -> bool {
    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.') || n.starts_with("._"))
}

/// Every audio file under a folder, with its size and modification time.
pub fn audio_files(root: &Path) -> Vec<(PathBuf, u64, i64)> {
    let mut out = vec![];
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if hidden(&p) {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                dirs.push(p);
            } else if AUDIO.contains(&ext(&p).as_str()) {
                let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
                out.push((p, meta.len(), mtime));
            }
        }
    }
    out.sort();
    out
}

/// "Disc 2", "CD1", "Disk 03": a folder that holds one disc of an album.
fn disc_folder(name: &str) -> Option<u32> {
    let l = name.trim().to_ascii_lowercase();
    let rest = ["disc", "disk", "cd"].iter().find_map(|p| l.strip_prefix(p))?;
    let digits: String = rest.trim_start_matches([' ', '.', '-', '_']).chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// The folder an audio file's album is: the one it is in, or the one above a disc folder.
pub fn album_folder(file: &Path) -> (PathBuf, Option<u32>) {
    let parent = file.parent().unwrap_or(Path::new("/"));
    match parent.file_name().and_then(|n| n.to_str()).and_then(disc_folder) {
        Some(d) => (parent.parent().unwrap_or(parent).to_path_buf(), Some(d)),
        None => (parent.to_path_buf(), None),
    }
}

/// What a folder named like "Artist - Album (1977) [FLAC] [24B-44.1kHz]" says: artist,
/// album and year, as far as it says them.
pub fn from_folder_name(name: &str) -> (String, String, i32) {
    let mut s = name.trim().to_string();
    // Trailing [FLAC], [24B-44.1kHz] and the like.
    while s.ends_with(']') {
        match s.rfind('[') {
            Some(i) => s = s[..i].trim_end().to_string(),
            None => break,
        }
    }
    let mut year = 0;
    if s.ends_with(')') {
        if let Some(i) = s.rfind('(') {
            let inner = &s[i + 1..s.len() - 1];
            if inner.len() == 4 && inner.chars().all(|c| c.is_ascii_digit()) {
                year = inner.parse().unwrap_or(0);
                s = s[..i].trim_end().to_string();
            }
        }
    }
    match s.split_once(" - ") {
        Some((artist, album)) => (artist.trim().to_string(), album.trim().to_string(), year),
        None => (String::new(), s, year),
    }
}

/// What a file named like "07. Artist - Title.flac" or "1-02 Title.wav" says: disc, track
/// and title. The artist's name, when it leads the title, is taken off.
pub fn from_file_name(stem: &str, artist: &str) -> (Option<u32>, Option<u32>, String) {
    let s = stem.trim();
    let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (mut disc, mut track) = (None, None);
    let mut rest = s;
    if digits_end > 0 && digits_end <= 3 {
        let first: u32 = s[..digits_end].parse().unwrap_or(0);
        rest = &s[digits_end..];
        // "1-02": disc and track.
        if let Some(r) = rest.strip_prefix('-') {
            let e = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
            if e > 0 {
                disc = Some(first);
                track = r[..e].parse().ok();
                rest = &r[e..];
            }
        }
        if track.is_none() {
            track = Some(first);
        }
        rest = rest.trim_start_matches(['.', ' ', '-', '_', ')']).trim_start();
    }
    let mut title = rest.to_string();
    if !artist.is_empty() {
        if let Some(t) = title.strip_prefix(&format!("{artist} - ")) {
            title = t.to_string();
        }
    }
    if title.is_empty() {
        title = s.to_string();
    }
    (disc, track, title)
}

/// Read one file's tags and properties. A file lofty cannot read is still a track, named
/// from its file name.
pub fn read_file(path: &Path, size: u64, mtime: i64) -> Read {
    let mut r = Read::default();
    r.track.path = path.to_string_lossy().into_owned();
    r.track.size = size;
    r.track.mtime = mtime;
    r.track.suffix = ext(path);
    if let Ok(f) = lofty::read_from_path(path) {
        let p = f.properties();
        r.track.seconds = p.duration().as_secs_f64();
        r.track.bitrate = p.audio_bitrate().or(p.overall_bitrate()).unwrap_or(0);
        r.track.sample_rate = p.sample_rate().unwrap_or(0);
        r.track.bit_depth = p.bit_depth().unwrap_or(0);
        r.track.channels = p.channels().unwrap_or(0);
        if let Some(t) = f.primary_tag().or_else(|| f.first_tag()) {
            let s = |k: ItemKey| t.get_string(k).map(|v| v.trim().to_string()).unwrap_or_default();
            r.track.title = t.title().map(|v| v.trim().to_string()).unwrap_or_default();
            r.track.artist = t.artist().map(|v| v.trim().to_string()).unwrap_or_default();
            r.album = t.album().map(|v| v.trim().to_string()).unwrap_or_default();
            r.album_artist = s(ItemKey::AlbumArtist);
            r.track.track = t.track().unwrap_or(0);
            r.track.disc = t.disk().unwrap_or(0);
            r.track.year = t.date().map(|d| d.year as i32).unwrap_or(0);
            if r.track.year == 0 {
                r.track.year = s(ItemKey::Year).get(..4).and_then(|y| y.parse().ok()).unwrap_or(0);
            }
            r.track.genre = t.genre().map(|v| v.trim().to_string()).unwrap_or_default();
            r.mb_album = s(ItemKey::MusicBrainzReleaseId);
        }
    }
    r
}

/// The most common non-empty value, the first seen on a tie.
fn most_common<'a>(values: impl Iterator<Item = &'a str>) -> String {
    let mut counts: Vec<(&str, usize)> = vec![];
    for v in values.filter(|v| !v.is_empty()) {
        match counts.iter_mut().find(|(k, _)| *k == v) {
            Some((_, n)) => *n += 1,
            None => counts.push((v, 1)),
        }
    }
    counts.iter().max_by_key(|(_, n)| *n).map(|(v, _)| v.to_string()).unwrap_or_default()
}

/// The picture to show for an album folder: a file named like a cover, any picture, or none.
pub fn cover_in(folder: &Path) -> String {
    let Ok(rd) = std::fs::read_dir(folder) else { return String::new() };
    let mut images: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| !hidden(p) && IMAGES.contains(&ext(p).as_str())).collect();
    images.sort();
    let named = images.iter().find(|p| p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| COVERS.contains(&s.to_ascii_lowercase().as_str())));
    named.or(images.first()).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

/// An album as gathered from its files, before it has an id.
#[derive(Debug, Default)]
pub struct Gathered {
    pub folder: String,
    pub title: String,
    pub artist: String,
    pub year: i32,
    pub genre: String,
    pub cover: String,
    pub mbid: String,
    pub tracks: Vec<Track>,
}

pub const VARIOUS: &str = "Various Artists";
/// An album's cover when its picture is inside its tracks rather than beside them.
pub const EMBEDDED: &str = "embedded";

/// Put read files together into albums, filling in from folder and file names what tags
/// leave out.
pub fn gather(files: Vec<(Read, Option<u32>)>) -> Vec<Gathered> {
    let mut by_folder: HashMap<PathBuf, Vec<(Read, Option<u32>)>> = HashMap::new();
    for (r, disc) in files {
        let (folder, _) = album_folder(Path::new(&r.track.path));
        by_folder.entry(folder).or_default().push((r, disc));
    }
    let mut out = vec![];
    for (folder, reads) in by_folder {
        let name = folder.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let (f_artist, f_album, f_year) = from_folder_name(name);
        let album_artist = most_common(reads.iter().map(|(r, _)| r.album_artist.as_str()));
        let artists: Vec<&str> = reads.iter().map(|(r, _)| r.track.artist.as_str()).filter(|a| !a.is_empty()).collect();
        let distinct = {
            let mut d: Vec<&str> = artists.clone();
            d.sort();
            d.dedup();
            d.len()
        };
        let artist = if !album_artist.is_empty() {
            album_artist
        } else if distinct == 1 {
            artists[0].to_string()
        } else if !f_artist.is_empty() {
            f_artist.clone()
        } else if distinct > 1 {
            VARIOUS.to_string()
        } else {
            "Unknown Artist".to_string()
        };
        let title = Some(most_common(reads.iter().map(|(r, _)| r.album.as_str()))).filter(|t| !t.is_empty()).unwrap_or(f_album);
        let year = reads.iter().map(|(r, _)| r.track.year).filter(|y| *y > 0).min().unwrap_or(f_year);
        let genre = most_common(reads.iter().map(|(r, _)| r.track.genre.as_str()));
        let mbid = most_common(reads.iter().map(|(r, _)| r.mb_album.as_str()));
        let tracks: Vec<Track> = reads
            .into_iter()
            .map(|(r, folder_disc)| {
                let mut t = r.track;
                let stem = Path::new(&t.path).file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let (n_disc, n_track, n_title) = from_file_name(&stem, if t.artist.is_empty() { &artist } else { &t.artist });
                if t.title.is_empty() {
                    t.title = n_title;
                }
                if t.track == 0 {
                    t.track = n_track.unwrap_or(0);
                }
                if t.disc == 0 {
                    t.disc = folder_disc.or(n_disc).unwrap_or(1);
                }
                if t.artist.is_empty() {
                    t.artist = artist.clone();
                }
                if t.year == 0 {
                    t.year = year;
                }
                t
            })
            .collect();
        let mut cover = cover_in(&folder);
        if cover.is_empty() && tracks.iter().any(|t: &Track| super::subsonic::has_picture(std::path::Path::new(&t.path))) {
            cover = EMBEDDED.into();
        }
        out.push(Gathered { folder: folder.to_string_lossy().into_owned(), cover, title, artist, year, genre, mbid, tracks });
    }
    out.sort_by(|a, b| a.folder.cmp(&b.folder));
    out
}

/// A name as it is compared: lower case, accents and "The " taken off.
pub fn artist_key(name: &str) -> String {
    let l = deunicode::deunicode(name).to_lowercase();
    let l = l.trim();
    l.strip_prefix("the ").unwrap_or(l).split_whitespace().collect::<Vec<_>>().join(" ")
}

impl App {
    /// Read the music folders and bring the library up to date.
    pub async fn music_scan(&self) -> Result<String> {
        {
            let mut s = self.music.scan.lock();
            if s.scanning {
                return Ok("already scanning".into());
            }
            *s = super::ScanState { scanning: true, last_at: s.last_at, last_took_ms: s.last_took_ms, ..Default::default() };
        }
        let started = std::time::Instant::now();
        let result = self.music_scan_inner().await;
        let mut s = self.music.scan.lock();
        s.scanning = false;
        s.last_at = now();
        s.last_took_ms = started.elapsed().as_millis() as u64;
        match &result {
            Ok(_) => s.error.clear(),
            Err(e) => s.error = format!("{e:#}"),
        }
        result
    }

    async fn music_scan_inner(&self) -> Result<String> {
        let roots = self.music_roots();
        if roots.is_empty() {
            return Ok("no music folder is set".into());
        }
        let known: HashMap<String, Track> = self.music.library().tracks.iter().map(|t| (t.path.clone(), t.clone())).collect();
        let app = self.clone();
        let roots2 = roots.clone();
        // Folders on a network share can be slow to list and files slow to open: all of it
        // runs on threads of its own.
        let (reads, read_roots) = tokio::task::spawn_blocking(move || {
            let mut reads = vec![];
            let mut read_roots = vec![];
            for (root, online) in roots2 {
                if !online || !crate::locations::listable(Path::new(&root)) {
                    continue;
                }
                read_roots.push(root.clone());
                let files = audio_files(Path::new(&root));
                app.music.scan.lock().seen += files.len();
                let fresh: Vec<_> = files.iter().filter(|(p, size, mtime)| !known.get(&*p.to_string_lossy()).is_some_and(|t| t.size == *size && t.mtime == *mtime)).cloned().collect();
                let fresh_read = read_many(&fresh, &app);
                let mut fresh_read: HashMap<String, Read> = fresh_read.into_iter().map(|r| (r.track.path.clone(), r)).collect();
                for (p, _, _) in files {
                    let key = p.to_string_lossy().into_owned();
                    let (_, disc) = album_folder(&p);
                    let r = match fresh_read.remove(&key) {
                        Some(r) => r,
                        None => {
                            let t = known[&key].clone();
                            Read { album: String::new(), album_artist: String::new(), mb_album: String::new(), track: t }
                        }
                    };
                    reads.push((r, disc));
                }
            }
            (reads, read_roots)
        })
        .await?;
        let read_count = self.music.scan.lock().read;
        // Tracks not read again carry no album tags; their album keeps what it was called.
        let old = self.music.library();
        let mut gathered = tokio::task::spawn_blocking(move || gather(reads)).await?;
        for g in &mut gathered {
            if let Some(a) = old.albums.iter().find(|a| a.folder == g.folder) {
                let all_known = g.tracks.iter().all(|t| old.tracks.iter().any(|o| o.path == t.path && o.mtime == t.mtime && o.size == t.size));
                if all_known {
                    g.title = a.title.clone();
                    g.artist = a.artist.clone();
                    g.year = a.year;
                    g.genre = a.genre.clone();
                    g.mbid = a.mbid.clone();
                }
            }
        }
        let (albums, tracks) = (gathered.len(), gathered.iter().map(|g| g.tracks.len()).sum::<usize>());
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || save(&db, &gathered, &read_roots)).await??;
        let lib = self.db.music_library()?;
        *self.music.library.lock() = Arc::new(lib);
        Ok(format!("{albums} albums, {tracks} tracks; {read_count} files read"))
    }
}

/// Read files four at a time.
fn read_many(files: &[(PathBuf, u64, i64)], app: &App) -> Vec<Read> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out = parking_lot::Mutex::new(vec![]);
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((p, size, mtime)) = files.get(i) else { break };
                let r = read_file(p, *size, *mtime);
                app.music.scan.lock().read += 1;
                out.lock().push(r);
            });
        }
    });
    out.into_inner()
}

/// Write what was gathered, keeping the ids of what was there, and take out what is gone
/// from the folders that were read.
fn save(db: &crate::db::Db, gathered: &[Gathered], read_roots: &[String]) -> Result<()> {
    db.with(|c| {
        let tx = c.unchecked_transaction()?;
        let t = now();
        let mut artist_ids: HashMap<String, i64> = HashMap::new();
        {
            let mut s = tx.prepare("SELECT id, name FROM music_artists")?;
            for row in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
                let (id, name) = row?;
                artist_ids.insert(artist_key(&name), id);
            }
        }
        let mut kept_albums = vec![];
        let mut kept_tracks = vec![];
        for g in gathered {
            let key = artist_key(&g.artist);
            let artist_id = match artist_ids.get(&key) {
                Some(&id) => {
                    tx.execute("UPDATE music_artists SET name = ?2 WHERE id = ?1", params![id, g.artist])?;
                    id
                }
                None => {
                    tx.execute("INSERT INTO music_artists(name, key) VALUES (?1, ?2)", params![g.artist, key])?;
                    let id = tx.last_insert_rowid();
                    artist_ids.insert(key, id);
                    id
                }
            };
            tx.execute(
                "INSERT INTO music_albums(folder, artist_id, title, year, genre, cover, added_at, mbid) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(folder) DO UPDATE SET artist_id = ?2, title = ?3, year = ?4, genre = ?5, cover = ?6, mbid = ?8",
                params![g.folder, artist_id, g.title, g.year, g.genre, g.cover, t, g.mbid],
            )?;
            let album_id: i64 = tx.query_row("SELECT id FROM music_albums WHERE folder = ?1", [&g.folder], |r| r.get(0))?;
            kept_albums.push(album_id);
            for tr in &g.tracks {
                tx.execute(
                    "INSERT INTO music_tracks(album_id, path, title, artist, track, disc, year, genre, seconds, size, mtime, suffix, bitrate, sample_rate, bit_depth, channels, added_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
                     ON CONFLICT(path) DO UPDATE SET album_id = ?1, title = ?3, artist = ?4, track = ?5, disc = ?6, year = ?7, genre = ?8, seconds = ?9, size = ?10, mtime = ?11,
                       suffix = ?12, bitrate = ?13, sample_rate = ?14, bit_depth = ?15, channels = ?16",
                    params![album_id, tr.path, tr.title, tr.artist, tr.track, tr.disc, tr.year, tr.genre, tr.seconds, tr.size as i64, tr.mtime, tr.suffix, tr.bitrate, tr.sample_rate, tr.bit_depth, tr.channels, t],
                )?;
                kept_tracks.push(tr.path.clone());
            }
        }
        // What is gone: tracks under a folder that was read and is no longer there. A folder
        // that could not be read says nothing about its tracks.
        let kept: std::collections::HashSet<&str> = kept_tracks.iter().map(|s| s.as_str()).collect();
        let mut gone = vec![];
        {
            let mut s = tx.prepare("SELECT id, path FROM music_tracks")?;
            for row in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
                let (id, path) = row?;
                if !kept.contains(path.as_str()) && read_roots.iter().any(|r| Path::new(&path).starts_with(r)) {
                    gone.push(id);
                }
            }
        }
        for id in gone {
            tx.execute("DELETE FROM music_tracks WHERE id = ?1", [id])?;
        }
        tx.execute("DELETE FROM music_albums WHERE id NOT IN (SELECT DISTINCT album_id FROM music_tracks)", [])?;
        tx.execute("DELETE FROM music_artists WHERE id NOT IN (SELECT DISTINCT artist_id FROM music_albums)", [])?;
        tx.commit()
    })?;
    Ok(())
}

/// The library as it would be with these albums, for tests.
#[cfg(test)]
pub fn library_of(gathered: Vec<Gathered>) -> Library {
    let mut artists: Vec<Artist> = vec![];
    let mut albums = vec![];
    let mut tracks = vec![];
    for (i, g) in gathered.into_iter().enumerate() {
        let artist_id = match artists.iter().find(|a| a.name == g.artist) {
            Some(a) => a.id,
            None => {
                artists.push(Artist { id: artists.len() as i64 + 1, name: g.artist.clone() });
                artists.len() as i64
            }
        };
        let album_id = i as i64 + 1;
        for mut t in g.tracks {
            t.id = tracks.len() as i64 + 1;
            t.album_id = album_id;
            tracks.push(t);
        }
        albums.push(Album { id: album_id, folder: g.folder, artist_id, artist: g.artist, title: g.title, year: g.year, genre: g.genre, cover: g.cover, added_at: 0, mbid: g.mbid });
    }
    Library::new(artists, albums, tracks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_give_artist_album_and_year() {
        assert_eq!(from_folder_name("Fleetwood Mac - Rumours (Super Deluxe) (1977) [FLAC] [24B-44.1kHz]"), ("Fleetwood Mac".into(), "Rumours (Super Deluxe)".into(), 1977));
        assert_eq!(from_folder_name("Classic Rock Classics"), ("".into(), "Classic Rock Classics".into(), 0));
        assert_eq!(from_folder_name("Cindy Lee - Diamond-Jubilee - REALISTIK MASTER - March 27 2024"), ("Cindy Lee".into(), "Diamond-Jubilee - REALISTIK MASTER - March 27 2024".into(), 0));
    }

    #[test]
    fn file_names_give_disc_track_and_title() {
        assert_eq!(from_file_name("07. Neutral Milk Hotel - Avery Island  April 1st", "Neutral Milk Hotel"), (None, Some(7), "Avery Island  April 1st".into()));
        assert_eq!(from_file_name("1-12. Kingdom Come", "Cindy Lee"), (Some(1), Some(12), "Kingdom Come".into()));
        assert_eq!(from_file_name("122. Little Feat - Dixie Chicken", "Various Artists"), (None, Some(122), "Little Feat - Dixie Chicken".into()));
        assert_eq!(from_file_name("Intro", ""), (None, None, "Intro".into()));
    }

    #[test]
    fn disc_folders_belong_to_the_album_above() {
        let (f, d) = album_folder(Path::new("/m/Fleetwood Mac - Rumours/Disc 3/01. Second Hand News.flac"));
        assert_eq!((f, d), (PathBuf::from("/m/Fleetwood Mac - Rumours"), Some(3)));
        let (f, d) = album_folder(Path::new("/m/Album/01.flac"));
        assert_eq!((f, d), (PathBuf::from("/m/Album"), None));
        assert_eq!(disc_folder("CD1"), Some(1));
        assert_eq!(disc_folder("Discography"), None);
    }

    fn read(path: &str, artist: &str, album: &str, album_artist: &str, title: &str) -> (Read, Option<u32>) {
        let (_, disc) = album_folder(Path::new(path));
        (Read { track: Track { path: path.into(), artist: artist.into(), title: title.into(), ..Default::default() }, album: album.into(), album_artist: album_artist.into(), mb_album: String::new() }, disc)
    }

    #[test]
    fn untagged_files_are_named_from_their_folder_and_files() {
        let g = gather(vec![read("/m/Cindy Lee - Diamond-Jubilee (2024)/1-2. Glitz.wav", "", "", "", ""), read("/m/Cindy Lee - Diamond-Jubilee (2024)/1-1. Diamond Jubilee.wav", "", "", "", "")]);
        assert_eq!(g.len(), 1);
        assert_eq!((g[0].artist.as_str(), g[0].title.as_str(), g[0].year), ("Cindy Lee", "Diamond-Jubilee", 2024));
        let mut t: Vec<_> = g[0].tracks.iter().map(|t| (t.disc, t.track, t.title.as_str())).collect();
        t.sort();
        assert_eq!(t, vec![(1, 1, "Diamond Jubilee"), (1, 2, "Glitz")]);
    }

    #[test]
    fn many_artists_without_an_album_artist_are_a_compilation() {
        let g = gather(vec![read("/m/Classic Rock Classics/02. a.flac", "The Rolling Stones", "Classic Rock Classics", "", "Gimme Shelter"), read("/m/Classic Rock Classics/17. b.flac", "Bruce Springsteen", "Classic Rock Classics", "", "Hungry Heart")]);
        assert_eq!(g[0].artist, VARIOUS);
        assert_eq!(g[0].tracks.iter().map(|t| t.artist.as_str()).collect::<Vec<_>>(), vec!["The Rolling Stones", "Bruce Springsteen"]);
    }

    #[test]
    fn disc_folders_make_one_album_with_its_discs_numbered() {
        let g = gather(vec![read("/m/FM - Rumours/Disc 1/01. x.flac", "Fleetwood Mac", "Rumours", "Fleetwood Mac", "Second Hand News"), read("/m/FM - Rumours/Disc 3/01. y.flac", "Fleetwood Mac", "Rumours", "Fleetwood Mac", "Second Hand News (Early Take)")]);
        assert_eq!(g.len(), 1);
        let lib = library_of(g);
        let order: Vec<_> = lib.tracks_of(1).map(|t| (t.disc, t.track)).collect();
        assert_eq!(order, vec![(1, 1), (3, 1)]);
    }

    #[test]
    fn artists_compare_without_the_and_accents() {
        assert_eq!(artist_key("The Beatles"), artist_key("beatles"));
        assert_eq!(artist_key("Björk"), artist_key("Bjork"));
    }
}
