//! Posters and backdrops, fetched once, shrunk to display size and served from disk.
//!
//! The catalog stores whatever artwork address the metadata source gave, often a multi-megapixel
//! original. Browsers get a small local copy instead, with an address that changes only when the
//! source does, so it can be cached indefinitely.

use crate::app::App;
use crate::models::Title;
use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq)]
pub enum Art {
    Poster,
    Fanart,
}

impl Art {
    pub fn parse(s: &str) -> Option<Art> {
        match s {
            "poster" => Some(Art::Poster),
            "fanart" => Some(Art::Fanart),
            _ => None,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Art::Poster => "poster",
            Art::Fanart => "fanart",
        }
    }
    fn width(self) -> u32 {
        match self {
            Art::Poster => 400,
            Art::Fanart => 1280,
        }
    }
    /// A picture kept in the title's folder comes before the one the metadata source has.
    fn source(self, t: &Title) -> Option<String> {
        match self {
            Art::Poster => t.poster_source(),
            Art::Fanart => t.fanart_source(),
        }
    }
}

/// Short stable tag for a source address, used to name the cached file and version its URL.
pub fn tag(url: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Address of the poster kept with a saved release.
pub fn saved_url(nzb_id: i64, src: &str) -> String {
    format!("/api/archive/{nzb_id}/art?v={}", &tag(src)[..10])
}

/// The same poster at the size a list shows it.
pub fn saved_small_url(nzb_id: i64, src: &str) -> String {
    format!("{}&size=small", saved_url(nzb_id, src))
}

/// The address the web app should use for a title's artwork, or None if it has none.
pub fn local_url(t: &Title, art: Art) -> Option<String> {
    art.source(t).map(|src| format!("/api/art/{}/{}?v={}", t.id, art.name(), &tag(&src)[..10]))
}

/// The address of a title's poster at the size a grid or a list shows it.
pub fn small_url(t: &Title) -> Option<String> {
    t.poster_source().map(|src| format!("/api/art/{}/small?v={}", t.id, &tag(&src)[..10]))
}

/// How wide the small poster is. A phone draws a grid tile about 330 of its own pixels across.
const SMALL: u32 = 300;

/// TMDB serves ready-made sizes; asking for one is cheaper than shrinking the original ourselves.
fn sized_source(src: &str, art: Art) -> (String, bool) {
    if let Some(rest) = src.split("image.tmdb.org/t/p/").nth(1) {
        if let Some((_, file)) = rest.split_once('/') {
            let size = if art == Art::Poster { "w500" } else { "w1280" };
            return (format!("https://image.tmdb.org/t/p/{size}/{file}"), true);
        }
    }
    (src.to_string(), false)
}

/// An image brought down to the small width, or as it was if it is already no wider.
async fn shrink(full: Vec<u8>) -> Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let img = image::load_from_memory(&full)?;
        if img.width() <= SMALL {
            return Ok(full);
        }
        let img = img.resize(SMALL, u32::MAX, image::imageops::FilterType::Lanczos3);
        let mut buf = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 80).encode_image(&img.to_rgb8())?;
        Ok(buf)
    })
    .await?
}

impl App {
    fn art_path(&self, t: &Title, art: Art, src: &str) -> PathBuf {
        self.data_dir.join("art").join(format!("{}-{}-{}.jpg", t.id, art.name(), tag(src)))
    }

    /// Bytes of the cached image, fetching and shrinking it first if this is the first request.
    pub async fn art(&self, title_id: i64, art: Art) -> Result<Vec<u8>> {
        let t = self.db.title(title_id)?.ok_or_else(|| anyhow!("no such title"))?;
        let src = art.source(&t).ok_or_else(|| anyhow!("no artwork"))?;
        let path = self.art_path(&t, art, &src);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(bytes);
        }
        // A picture on a location that is away is not asked for: a share that has gone can
        // keep the asker waiting.
        if crate::local::art_file(&src).is_some() && !self.title_online(&t) {
            bail!("the picture is on a location that cannot be reached");
        }
        let prefix = format!("{}-{}-", t.id, art.name());
        let (url, presized) = sized_source(&src, art);
        self.art_fetch(&path, &url, presized, art.width(), &prefix).await
    }

    /// The poster again, smaller, for grids and lists where the full one is more than the screen
    /// can show. It is made from the full poster, which stays as it is beside it.
    pub async fn art_small(&self, title_id: i64) -> Result<Vec<u8>> {
        let t = self.db.title(title_id)?.ok_or_else(|| anyhow!("no such title"))?;
        let src = t.poster_source().ok_or_else(|| anyhow!("no artwork"))?;
        let path = self.small_path(&t, &src);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(bytes);
        }
        let out = shrink(self.art(title_id, Art::Poster).await?).await?;
        self.art_keep(&path, &out, &format!("{}-small-", t.id)).await?;
        Ok(out)
    }

    fn small_path(&self, t: &Title, src: &str) -> PathBuf {
        self.data_dir.join("art").join(format!("{}-small-{}.jpg", t.id, tag(src)))
    }

    /// Any image by its address, kept under `name`: cast photos, episode stills, and posters
    /// and backdrops at the size a television wants.
    pub async fn art_any(&self, name: &str, src: &str, width: u32) -> Result<Vec<u8>> {
        let path = self.data_dir.join("art").join(format!("{name}-{}.jpg", tag(src)));
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(bytes);
        }
        let (url, presized) = match src.split("image.tmdb.org/t/p/").nth(1).and_then(|rest| rest.split_once('/')) {
            Some((_, file)) => {
                let size = match width {
                    0..=185 => "w185",
                    186..=780 => "w780",
                    _ => "original",
                };
                (format!("https://image.tmdb.org/t/p/{size}/{file}"), true)
            }
            None => (src.to_string(), false),
        };
        self.art_fetch(&path, &url, presized, width, &format!("{name}-")).await
    }

    /// One picture for a whole view of the library, as players show Movies and Shows: these
    /// titles' posters side by side, each cut to a tall strip. Kept under the tag, which
    /// changes when the titles chosen do.
    pub async fn art_view(&self, name: &str, titles: &[i64], tag: &str) -> Result<Vec<u8>> {
        let path = self.data_dir.join("art").join(format!("view-{name}-{tag}.jpg"));
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(bytes);
        }
        let mut posters = vec![];
        for id in titles {
            if let Ok(bytes) = self.art(*id, Art::Poster).await {
                posters.push(bytes);
            }
        }
        if posters.is_empty() {
            bail!("no posters to make a picture of");
        }
        let out = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let (w, h, gap) = (1280u32, 720u32, 4u32);
            let n = posters.len() as u32;
            let mut canvas = image::RgbImage::from_pixel(w, h, image::Rgb([13, 13, 15]));
            let strip = (w - gap * (n - 1)) / n;
            for (i, bytes) in posters.iter().enumerate() {
                let Ok(img) = image::load_from_memory(bytes) else { continue };
                let cut = img.resize_to_fill(strip, h, image::imageops::FilterType::Triangle).to_rgb8();
                image::imageops::replace(&mut canvas, &cut, (i as u32 * (strip + gap)) as i64, 0);
            }
            let mut buf = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 84).encode_image(&canvas)?;
            Ok(buf)
        })
        .await??;
        self.art_keep(&path, &out, &format!("view-{name}-")).await?;
        Ok(out)
    }

    /// That poster at list size, made from the full one, which is kept.
    pub async fn archive_art_small(&self, nzb_id: i64) -> Result<Vec<u8>> {
        let n = self.archive_get(nzb_id)?.ok_or_else(|| anyhow!("no such saved release"))?;
        let src = n.poster.ok_or_else(|| anyhow!("no artwork"))?;
        let path = self.data_dir.join("art").join(format!("saved-small-{}.jpg", tag(&src)));
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(bytes);
        }
        let out = shrink(self.archive_art(nzb_id).await?).await?;
        self.art_keep(&path, &out, "\u{0}").await?;
        Ok(out)
    }

    /// Poster for a saved release whose title may no longer be in the library.
    pub async fn archive_art(&self, nzb_id: i64) -> Result<Vec<u8>> {
        let n = self.archive_get(nzb_id)?.ok_or_else(|| anyhow!("no such saved release"))?;
        let src = n.poster.ok_or_else(|| anyhow!("no artwork"))?;
        let path = self.data_dir.join("art").join(format!("saved-{}.jpg", tag(&src)));
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(bytes);
        }
        // Named by its source alone: nothing older to clear away.
        let (url, presized) = sized_source(&src, Art::Poster);
        self.art_fetch(&path, &url, presized, Art::Poster.width(), "\u{0}").await
    }

    /// Fetch an image, shrink it to `width` and keep it at `path`, replacing cached files whose
    /// names start with `prefix`.
    async fn art_fetch(&self, path: &std::path::Path, url: &str, presized: bool, width: u32, prefix: &str) -> Result<Vec<u8>> {
        let _permit = self.art_fetches.acquire().await?;
        // Another request may have fetched it while this one waited.
        if let Ok(bytes) = tokio::fs::read(path).await {
            return Ok(bytes);
        }
        let raw = match crate::local::art_file(url) {
            // The user's own picture, read where it lies and never changed.
            Some(file) => tokio::fs::read(file).await.map_err(|e| anyhow!("reading {file}: {e}"))?,
            None => {
                let resp = self.http.get(url).timeout(std::time::Duration::from_secs(30)).send().await.map_err(|e| anyhow!("{}", e.without_url()))?;
                if !resp.status().is_success() {
                    bail!("artwork source answered HTTP {}", resp.status().as_u16());
                }
                resp.bytes().await.map_err(|e| anyhow!("{}", e.without_url()))?.to_vec()
            }
        };
        let out = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let img = image::load_from_memory(&raw)?;
            // Already the right size and already JPEG: keep the source's own encoding.
            if presized && img.width() <= width + 120 && raw.starts_with(&[0xFF, 0xD8]) {
                return Ok(raw);
            }
            let img = if img.width() > width { img.resize(width, u32::MAX, image::imageops::FilterType::Lanczos3) } else { img };
            let mut buf = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 82).encode_image(&img.to_rgb8())?;
            Ok(buf)
        })
        .await??;

        self.art_keep(path, &out, prefix).await?;
        Ok(out)
    }

    /// Write an image to the cache at `path`, clearing cached files whose names start with
    /// `prefix`: copies made from an earlier source address for the same thing.
    async fn art_keep(&self, path: &std::path::Path, bytes: &[u8], prefix: &str) -> Result<()> {
        let dir = self.data_dir.join("art");
        tokio::fs::create_dir_all(&dir).await?;
        if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
            while let Ok(Some(e)) = rd.next_entry().await {
                if e.file_name().to_string_lossy().starts_with(prefix) {
                    let _ = tokio::fs::remove_file(e.path()).await;
                }
            }
        }
        let tmp = path.with_extension("tmp");
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    }

    /// Fetch every poster that is not cached yet, a few at a time, and make its small copy, so
    /// the library opens quickly.
    pub async fn warm_art(&self) -> usize {
        let mut fetched = 0;
        for t in self.db.titles(None).unwrap_or_default() {
            let Some(src) = t.poster_source() else { continue };
            let had = self.art_path(&t, Art::Poster, &src).exists();
            if had && self.small_path(&t, &src).exists() {
                continue;
            }
            // The small one is made from the full one, so asking for it brings both.
            if self.art_small(t.id).await.is_ok() && !had {
                fetched += 1;
            }
            tokio::time::sleep(std::time::Duration::from_millis(if had { 5 } else { 60 })).await;
        }
        fetched
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_ready_made_size_from_tmdb() {
        assert_eq!(sized_source("https://image.tmdb.org/t/p/original/abc.jpg", Art::Poster), ("https://image.tmdb.org/t/p/w500/abc.jpg".into(), true));
        assert_eq!(sized_source("https://image.tmdb.org/t/p/w500/abc.jpg", Art::Fanart), ("https://image.tmdb.org/t/p/w1280/abc.jpg".into(), true));
        assert_eq!(sized_source("https://artworks.thetvdb.com/banners/posters/1-2.jpg", Art::Poster), ("https://artworks.thetvdb.com/banners/posters/1-2.jpg".into(), false));
        assert_ne!(tag("a"), tag("b"));
    }
}
