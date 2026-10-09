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
    fn source(self, t: &Title) -> Option<&str> {
        match self {
            Art::Poster => t.poster.as_deref(),
            Art::Fanart => t.fanart.as_deref(),
        }
    }
}

/// Short stable tag for a source address, used to name the cached file and version its URL.
fn tag(url: &str) -> String {
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

/// The address the web app should use for a title's artwork, or None if it has none.
pub fn local_url(t: &Title, art: Art) -> Option<String> {
    art.source(t).map(|src| format!("/api/art/{}/{}?v={}", t.id, art.name(), &tag(src)[..10]))
}

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

impl App {
    fn art_path(&self, t: &Title, art: Art, src: &str) -> PathBuf {
        self.data_dir.join("art").join(format!("{}-{}-{}.jpg", t.id, art.name(), tag(src)))
    }

    /// Bytes of the cached image, fetching and shrinking it first if this is the first request.
    pub async fn art(&self, title_id: i64, art: Art) -> Result<Vec<u8>> {
        let t = self.db.title(title_id)?.ok_or_else(|| anyhow!("no such title"))?;
        let src = art.source(&t).ok_or_else(|| anyhow!("no artwork"))?.to_string();
        let path = self.art_path(&t, art, &src);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(bytes);
        }
        let prefix = format!("{}-{}-", t.id, art.name());
        self.art_fetch(&path, &src, art, &prefix).await
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
        self.art_fetch(&path, &src, Art::Poster, "\u{0}").await
    }

    /// Fetch an image, shrink it to display size and keep it at `path`, replacing cached files
    /// whose names start with `prefix`.
    async fn art_fetch(&self, path: &std::path::Path, src: &str, art: Art, prefix: &str) -> Result<Vec<u8>> {
        let _permit = self.art_fetches.acquire().await?;
        // Another request may have fetched it while this one waited.
        if let Ok(bytes) = tokio::fs::read(path).await {
            return Ok(bytes);
        }
        let (url, presized) = sized_source(src, art);
        let resp = self.http.get(&url).timeout(std::time::Duration::from_secs(30)).send().await.map_err(|e| anyhow!("{}", e.without_url()))?;
        if !resp.status().is_success() {
            bail!("artwork source answered HTTP {}", resp.status().as_u16());
        }
        let raw = resp.bytes().await.map_err(|e| anyhow!("{}", e.without_url()))?.to_vec();
        let width = art.width();
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

        let dir = self.data_dir.join("art");
        tokio::fs::create_dir_all(&dir).await?;
        // Drop copies made from an earlier source address for this title.
        if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
            while let Ok(Some(e)) = rd.next_entry().await {
                if e.file_name().to_string_lossy().starts_with(prefix) {
                    let _ = tokio::fs::remove_file(e.path()).await;
                }
            }
        }
        let tmp = path.with_extension("tmp");
        tokio::fs::write(&tmp, &out).await?;
        tokio::fs::rename(&tmp, path).await?;
        Ok(out)
    }

    /// Fetch every poster that is not cached yet, a few at a time, so the library opens quickly.
    pub async fn warm_art(&self) -> usize {
        let mut fetched = 0;
        for t in self.db.titles(None).unwrap_or_default() {
            let Some(src) = t.poster.clone() else { continue };
            if self.art_path(&t, Art::Poster, &src).exists() {
                continue;
            }
            if self.art(t.id, Art::Poster).await.is_ok() {
                fetched += 1;
            }
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
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
