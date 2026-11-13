//! Music. A location may have a music folder beside its films and series; Spool reads the
//! albums in it (see [`scan`]) and serves them to music players over the OpenSubsonic API
//! (see [`subsonic`]), and to its own web app.
//!
//! An album is a folder of audio files, with any "Disc 2" folders inside it counted as part
//! of it. What an album and its tracks are called comes from their tags, and from the folder
//! and file names where the tags say nothing. Ids are kept from one scan to the next: a
//! track by its path, an album by its folder, an artist by its name.

pub mod musicbrainz;
pub mod scan;
pub mod subsonic;
pub mod want;
mod xml;

use crate::app::App;
use crate::db::{now, Db};
use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::params;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Artist {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Album {
    pub id: i64,
    pub folder: String,
    pub artist_id: i64,
    pub artist: String,
    pub title: String,
    pub year: i32,
    pub genre: String,
    /// An image file beside the tracks, or empty to take the picture inside the first track.
    pub cover: String,
    pub added_at: i64,
    pub mbid: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Track {
    pub id: i64,
    pub album_id: i64,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub track: u32,
    pub disc: u32,
    pub year: i32,
    pub genre: String,
    pub seconds: f64,
    pub size: u64,
    pub mtime: i64,
    pub suffix: String,
    /// Kilobits a second.
    pub bitrate: u32,
    pub sample_rate: u32,
    pub bit_depth: u8,
    pub channels: u8,
    pub added_at: i64,
    /// What the file's own tags say of its album, kept so an album is named the same whichever
    /// of its files were read last.
    #[serde(skip)]
    pub album_tag: String,
    #[serde(skip)]
    pub album_artist_tag: String,
    #[serde(skip)]
    pub mb_album_tag: String,
}

impl Track {
    pub fn content_type(&self) -> &'static str {
        content_type(&self.suffix)
    }
}

pub fn content_type(suffix: &str) -> &'static str {
    match suffix {
        "flac" => "audio/flac",
        "mp3" => "audio/mpeg",
        "m4a" | "mp4" | "aac" | "alac" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "aif" | "aiff" => "audio/aiff",
        "wv" => "audio/x-wavpack",
        "ape" => "audio/x-ape",
        _ => "application/octet-stream",
    }
}

/// The whole music library as one value, read from the database after each scan. A
/// household's music is a few thousand tracks, so players are answered from memory.
#[derive(Default)]
pub struct Library {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub tracks: Vec<Track>,
    artist_at: HashMap<i64, usize>,
    album_at: HashMap<i64, usize>,
    track_at: HashMap<i64, usize>,
    /// Each album's tracks in playing order.
    pub album_tracks: HashMap<i64, Vec<usize>>,
    /// Each artist's albums, oldest first.
    pub artist_albums: HashMap<i64, Vec<usize>>,
}

impl Library {
    pub fn new(artists: Vec<Artist>, albums: Vec<Album>, tracks: Vec<Track>) -> Library {
        let artist_at = artists.iter().enumerate().map(|(i, a)| (a.id, i)).collect();
        let album_at = albums.iter().enumerate().map(|(i, a)| (a.id, i)).collect();
        let track_at = tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        let mut album_tracks: HashMap<i64, Vec<usize>> = HashMap::new();
        for (i, t) in tracks.iter().enumerate() {
            album_tracks.entry(t.album_id).or_default().push(i);
        }
        for list in album_tracks.values_mut() {
            list.sort_by(|&a, &b| (tracks[a].disc, tracks[a].track, &tracks[a].path).cmp(&(tracks[b].disc, tracks[b].track, &tracks[b].path)));
        }
        let mut artist_albums: HashMap<i64, Vec<usize>> = HashMap::new();
        for (i, a) in albums.iter().enumerate() {
            artist_albums.entry(a.artist_id).or_default().push(i);
        }
        for list in artist_albums.values_mut() {
            list.sort_by(|&a, &b| (albums[a].year, &albums[a].title).cmp(&(albums[b].year, &albums[b].title)));
        }
        Library { artists, albums, tracks, artist_at, album_at, track_at, album_tracks, artist_albums }
    }

    pub fn artist(&self, id: i64) -> Option<&Artist> {
        self.artist_at.get(&id).map(|&i| &self.artists[i])
    }
    pub fn album(&self, id: i64) -> Option<&Album> {
        self.album_at.get(&id).map(|&i| &self.albums[i])
    }
    pub fn track(&self, id: i64) -> Option<&Track> {
        self.track_at.get(&id).map(|&i| &self.tracks[i])
    }
    pub fn tracks_of(&self, album: i64) -> impl Iterator<Item = &Track> {
        self.album_tracks.get(&album).into_iter().flatten().map(|&i| &self.tracks[i])
    }
    pub fn albums_of(&self, artist: i64) -> impl Iterator<Item = &Album> {
        self.artist_albums.get(&artist).into_iter().flatten().map(|&i| &self.albums[i])
    }
    /// An album's length in seconds and size in bytes.
    pub fn album_totals(&self, album: i64) -> (usize, f64, u64) {
        self.tracks_of(album).fold((0, 0.0, 0), |(n, s, b), t| (n + 1, s + t.seconds, b + t.size))
    }
}

/// What a person has done with a track, album or artist: starred it, rated it, played it.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Mark {
    pub starred_at: i64,
    pub rating: u8,
    pub plays: i64,
    pub played_at: i64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Playlist {
    pub id: i64,
    pub account_id: i64,
    pub name: String,
    pub comment: String,
    pub public: bool,
    pub tracks: Vec<i64>,
    pub created_at: i64,
    pub changed_at: i64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ScanState {
    pub scanning: bool,
    pub seen: usize,
    pub read: usize,
    pub last_at: i64,
    pub last_took_ms: u64,
    pub error: String,
}

pub struct Music {
    pub library: Mutex<Arc<Library>>,
    pub scan: Mutex<ScanState>,
    /// Sign-ins already checked, so a player that signs every request is not made to wait
    /// for a password hash each time. Keyed by account, its stored hash and what was given.
    pub signed: Mutex<HashMap<(i64, String, [u8; 32]), std::time::Instant>>,
    pub now_playing: subsonic::NowPlaying,
    /// Password hashes are slow on purpose; a few are checked at a time, however many arrive.
    pub checking: tokio::sync::Semaphore,
    /// Held while a wanted album is moved along, so that one is worked on at a time.
    pub wanting: tokio::sync::Mutex<()>,
}

impl Music {
    pub fn new(db: &Db) -> Music {
        let library = db.music_library().unwrap_or_default();
        Music { library: Mutex::new(Arc::new(library)), scan: Mutex::new(ScanState::default()), signed: Mutex::new(HashMap::new()), now_playing: Default::default(), checking: tokio::sync::Semaphore::new(2), wanting: Default::default() }
    }
    pub fn library(&self) -> Arc<Library> {
        self.library.lock().clone()
    }
}

impl App {
    /// Whether a file Spool is about to send is one of the library's: inside a music folder,
    /// and not a link to somewhere else.
    pub fn music_file_ok(&self, path: &std::path::Path) -> bool {
        self.music_roots().iter().any(|(r, _)| path.starts_with(r)) && std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
    }
}

impl Db {
    pub fn music_library(&self) -> Result<Library> {
        self.with(|c| {
            let artists = c
                .prepare("SELECT id, name FROM music_artists")?
                .query_map([], |r| Ok(Artist { id: r.get(0)?, name: r.get(1)? }))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let names: HashMap<i64, String> = artists.iter().map(|a| (a.id, a.name.clone())).collect();
            let albums = c
                .prepare("SELECT id, folder, artist_id, title, year, genre, cover, added_at, mbid FROM music_albums")?
                .query_map([], |r| {
                    let artist_id: i64 = r.get(2)?;
                    Ok(Album { id: r.get(0)?, folder: r.get(1)?, artist_id, artist: names.get(&artist_id).cloned().unwrap_or_default(), title: r.get(3)?, year: r.get(4)?, genre: r.get(5)?, cover: r.get(6)?, added_at: r.get(7)?, mbid: r.get(8)? })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let tracks = c
                .prepare("SELECT id, album_id, path, title, artist, track, disc, year, genre, seconds, size, mtime, suffix, bitrate, sample_rate, bit_depth, channels, added_at, album_tag, album_artist_tag, mb_album_tag FROM music_tracks")?
                .query_map([], track_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Library::new(artists, albums, tracks))
        })
    }

    pub fn music_marks(&self, account: i64) -> Result<HashMap<String, Mark>> {
        self.with(|c| {
            c.prepare("SELECT item, starred_at, rating, plays, played_at FROM music_marks WHERE account_id = ?1")?
                .query_map([account], |r| Ok((r.get::<_, String>(0)?, Mark { starred_at: r.get(1)?, rating: r.get(2)?, plays: r.get(3)?, played_at: r.get(4)? })))?
                .collect()
        })
    }

    /// Change one mark: `f` is given the present one, or the default.
    pub fn music_mark(&self, account: i64, item: &str, f: impl FnOnce(&mut Mark)) -> Result<()> {
        let mut m = self.music_marks(account)?.remove(item).unwrap_or_default();
        f(&mut m);
        self.with(|c| {
            c.execute(
                "INSERT INTO music_marks(account_id, item, starred_at, rating, plays, played_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(account_id, item) DO UPDATE SET starred_at = ?3, rating = ?4, plays = ?5, played_at = ?6",
                params![account, item, m.starred_at, m.rating, m.plays, m.played_at],
            )
        })?;
        Ok(())
    }

    /// The playlists a person can see: their own and the ones others made public.
    pub fn music_playlists(&self, account: i64) -> Result<Vec<Playlist>> {
        self.with(|c| {
            c.prepare("SELECT id, account_id, name, comment, public, tracks, created_at, changed_at FROM music_playlists WHERE account_id = ?1 OR public ORDER BY name")?
                .query_map([account], |r| {
                    let tracks: String = r.get(5)?;
                    Ok(Playlist { id: r.get(0)?, account_id: r.get(1)?, name: r.get(2)?, comment: r.get(3)?, public: r.get(4)?, tracks: serde_json::from_str(&tracks).unwrap_or_default(), created_at: r.get(6)?, changed_at: r.get(7)? })
                })?
                .collect()
        })
    }

    pub fn save_music_playlist(&self, p: &Playlist) -> Result<i64> {
        let tracks = serde_json::to_string(&p.tracks)?;
        self.with(|c| {
            if p.id == 0 {
                c.execute(
                    "INSERT INTO music_playlists(account_id, name, comment, public, tracks, created_at, changed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![p.account_id, p.name, p.comment, p.public, tracks, now()],
                )?;
                Ok(c.last_insert_rowid())
            } else {
                c.execute("UPDATE music_playlists SET name = ?2, comment = ?3, public = ?4, tracks = ?5, changed_at = ?6 WHERE id = ?1", params![p.id, p.name, p.comment, p.public, tracks, now()])?;
                Ok(p.id)
            }
        })
    }

    pub fn delete_music_playlist(&self, id: i64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM music_playlists WHERE id = ?1", [id]))?;
        Ok(())
    }
}

pub(crate) fn track_row(r: &rusqlite::Row) -> rusqlite::Result<Track> {
    Ok(Track {
        id: r.get(0)?,
        album_id: r.get(1)?,
        path: r.get(2)?,
        title: r.get(3)?,
        artist: r.get(4)?,
        track: r.get(5)?,
        disc: r.get(6)?,
        year: r.get(7)?,
        genre: r.get(8)?,
        seconds: r.get(9)?,
        size: r.get::<_, i64>(10)? as u64,
        mtime: r.get(11)?,
        suffix: r.get(12)?,
        bitrate: r.get(13)?,
        sample_rate: r.get(14)?,
        bit_depth: r.get(15)?,
        channels: r.get(16)?,
        added_at: r.get(17)?,
        album_tag: r.get(18)?,
        album_artist_tag: r.get(19)?,
        mb_album_tag: r.get(20)?,
    })
}

impl App {
    /// The music folders of every location, with whether each can be read now.
    pub fn music_roots(&self) -> Vec<(String, bool)> {
        let placement = self.placement();
        placement.0.iter().filter(|(l, _)| !l.music_root.is_empty()).map(|(l, online)| (l.music_root.clone(), *online)).collect()
    }

    /// The music folders Spool may file albums into: those of locations it manages, and
    /// that can be reached now.
    pub fn music_roots_to_file(&self) -> Vec<String> {
        let placement = self.placement();
        placement.0.iter().filter(|(l, online)| *online && l.manage && !l.music_root.is_empty()).map(|(l, _)| l.music_root.clone()).collect()
    }

    pub fn music_status(&self) -> serde_json::Value {
        let lib = self.music.library();
        serde_json::json!({
            "roots": self.music_roots().into_iter().map(|(r, online)| serde_json::json!({"path": r, "online": online})).collect::<Vec<_>>(),
            "artists": lib.artists.len(), "albums": lib.albums.len(), "tracks": lib.tracks.len(),
            "bytes": lib.tracks.iter().map(|t| t.size).sum::<u64>(),
            "scan": *self.music.scan.lock(),
        })
    }
}
