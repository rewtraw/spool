//! Covers for albums that came without one. The picture comes from the Cover Art Archive, by
//! the MusicBrainz release group the album was wanted as, the release its tags name, or a
//! search by artist and title, and goes beside the tracks as `cover.jpg`, where every player
//! finds it. Only into a folder Spool manages, and never over a picture already there.

use super::musicbrainz;
use super::scan;
use super::Album;
use crate::app::App;
use anyhow::Result;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

/// When each album folder was last looked for, so one the archive has nothing for is not
/// asked about again for a week.
static TRIED: LazyLock<Mutex<HashMap<String, i64>>> = LazyLock::new(Default::default);
const AGAIN: i64 = 7 * 24 * 3600;

impl App {
    /// Find covers for the albums without one. Says how many were found.
    pub async fn music_covers(&self) -> Result<usize> {
        let roots = self.music_roots_to_file();
        let lib = self.music.library();
        let wants = self.db.music_wants().unwrap_or_default();
        let now = crate::db::now();
        let bare: Vec<Album> = lib
            .albums
            .iter()
            .filter(|a| a.cover.is_empty() && roots.iter().any(|r| Path::new(&a.folder).starts_with(r) && Path::new(&a.folder) != Path::new(r)))
            .filter(|a| TRIED.lock().get(&a.folder).is_none_or(|t| now - t > AGAIN))
            .cloned()
            .collect();
        let mut found = 0;
        for a in bare {
            TRIED.lock().insert(a.folder.clone(), now);
            let url = match wants.iter().find(|w| w.folder == a.folder && !w.mbid.is_empty()) {
                Some(w) => format!("https://coverartarchive.org/release-group/{}/front-500", w.mbid),
                None if !a.mbid.is_empty() => format!("https://coverartarchive.org/release/{}/front-500", a.mbid),
                None => match self.group_of(&a).await {
                    Some(mbid) => format!("https://coverartarchive.org/release-group/{mbid}/front-500"),
                    None => continue,
                },
            };
            match self.fetch_cover(&url, &a.folder, &roots).await {
                Ok(true) => found += 1,
                Ok(false) => {}
                Err(e) => tracing::debug!(album = %a.title, error = %e, "no cover"),
            }
        }
        if found > 0 {
            self.music_scan().await?;
        }
        Ok(found)
    }

    /// The MusicBrainz release group an album in the library is, by a search for its artist
    /// and title, when one answers to both.
    async fn group_of(&self, a: &Album) -> Option<String> {
        let found = musicbrainz::search(&self.http, &format!("{} {}", a.artist, a.title)).await.ok()?;
        let artist = scan::artist_key(&a.artist);
        found.into_iter().find(|g| scan::artist_key(&g.artist) == artist && super::want::same_album(&a.title, &g.title)).map(|g| g.mbid)
    }

    /// Fetch a picture and put it beside an album's tracks. False when there is none to have.
    async fn fetch_cover(&self, url: &str, folder: &str, roots: &[String]) -> Result<bool> {
        let r = self.http.get(url).timeout(Duration::from_secs(30)).send().await?;
        if !r.status().is_success() {
            return Ok(false);
        }
        if !r.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("image/")) {
            return Ok(false);
        }
        let bytes = r.bytes().await?;
        if bytes.len() > 20 << 20 {
            return Ok(false);
        }
        let (folder, roots) = (folder.to_string(), roots.to_vec());
        tokio::task::spawn_blocking(move || -> Result<bool> {
            let dir = Path::new(&folder);
            // Followed links and all, it must still be inside a folder Spool files music into.
            if !roots.iter().any(|r| crate::books::audio::inside(dir, Path::new(r))) || !scan::cover_in(dir).is_empty() {
                return Ok(false);
            }
            // Written whole beside it first, hidden, then linked in under its name, which fails
            // rather than replace a picture that appeared meanwhile: a cut-short write never
            // leaves a broken cover where every player would take it for the real one.
            let part = dir.join(".cover.jpg.part");
            let written = std::fs::write(&part, &bytes).and_then(|_| std::fs::hard_link(&part, dir.join("cover.jpg")));
            let _ = std::fs::remove_file(&part);
            match written {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(e) => Err(e.into()),
            }
        })
        .await?
    }
}
