//! What sits in a title's folder beside the film or the episodes: trailers and other extras,
//! artwork, theme music, and the small files media servers keep their notes in.
//!
//! Plex, Jellyfin, Emby and Kodi agree on most of how these are named, and Spool reads all of
//! their conventions: a folder called "Trailers" or "Behind The Scenes", a file ending in
//! "-trailer" or "-featurette", "poster.jpg" or "Film (1999)-fanart.jpg", "movie.nfo",
//! "theme.mp3", ".plexmatch". Knowing which is which matters twice over. A featurette is not
//! the film, so an upgrade must not replace it; and what the user has put there by hand, a
//! poster above all, is what they want shown.

use crate::models::Kind;
use serde::{Deserialize, Serialize};
use spool_core::parser::is_video_file;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtraKind {
    Trailer,
    Sample,
    BehindTheScenes,
    DeletedScene,
    Featurette,
    Interview,
    Scene,
    Short,
    Clip,
    Other,
    /// A video played behind a title's page, kept in a "backdrops" folder.
    ThemeVideo,
}

impl ExtraKind {
    /// What Jellyfin calls it.
    pub fn jellyfin(self) -> &'static str {
        match self {
            ExtraKind::Trailer => "Trailer",
            ExtraKind::Sample => "Sample",
            ExtraKind::BehindTheScenes => "BehindTheScenes",
            ExtraKind::DeletedScene => "DeletedScene",
            ExtraKind::Featurette => "Featurette",
            ExtraKind::Interview => "Interview",
            ExtraKind::Scene => "Scene",
            ExtraKind::Short => "Short",
            ExtraKind::Clip => "Clip",
            ExtraKind::Other => "Unknown",
            ExtraKind::ThemeVideo => "ThemeVideo",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtKind {
    Poster,
    Backdrop,
    Banner,
    Logo,
    Thumb,
    ClearArt,
    Disc,
    Square,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Extra {
    pub rel_path: String,
    pub kind: ExtraKind,
    /// The file's name without the marker that says what kind it is.
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalArt {
    pub rel_path: String,
    pub kind: ArtKind,
    /// For a season's own artwork, which season; specials are season zero.
    #[serde(default)]
    pub season: Option<u32>,
    /// For a picture of one video, an episode's frame above all, that video's path.
    #[serde(default)]
    pub of: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub modified: i64,
}

/// What a media server's notes say a title is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notes {
    /// The file they were read from; empty when there are none.
    pub rel_path: String,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub year: u32,
}

/// Everything found in a title's folder that is not the film or an episode.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Local {
    pub extras: Vec<Extra>,
    pub art: Vec<LocalArt>,
    pub notes: Notes,
    /// Theme music: "theme.mp3", or what is in a "theme-music" folder.
    pub themes: Vec<String>,
}

impl Local {
    pub fn is_empty(&self) -> bool {
        self.extras.is_empty() && self.art.is_empty() && self.themes.is_empty() && self.notes.rel_path.is_empty()
    }

    /// The title's own picture of a kind, the first by name where there are several.
    pub fn art(&self, kind: ArtKind) -> Option<&LocalArt> {
        self.art.iter().find(|a| a.kind == kind && a.season.is_none() && a.of.is_none())
    }

    pub fn season_art(&self, kind: ArtKind, season: u32) -> Option<&LocalArt> {
        self.art.iter().find(|a| a.kind == kind && a.season == Some(season))
    }

    pub fn art_of(&self, video_rel: &str) -> Option<&LocalArt> {
        self.art.iter().find(|a| a.of.as_deref() == Some(video_rel))
    }

    pub fn trailers(&self) -> impl Iterator<Item = (usize, &Extra)> {
        self.extras.iter().enumerate().filter(|(_, e)| e.kind == ExtraKind::Trailer)
    }

    /// Extras a player lists under a title: everything but trailers, samples and theme videos.
    pub fn features(&self) -> impl Iterator<Item = (usize, &Extra)> {
        self.extras.iter().enumerate().filter(|(_, e)| !matches!(e.kind, ExtraKind::Trailer | ExtraKind::Sample | ExtraKind::ThemeVideo))
    }
}

/// A picture in a title's folder as an address the artwork cache understands: "file:", its
/// path, and after a "#" its size and date, so a changed picture has a new address.
pub fn art_address(title_path: &str, a: &LocalArt) -> String {
    format!("file:{}/{}#{}-{}", title_path, a.rel_path, a.size, a.modified)
}

/// The path in such an address, if it is one.
pub fn art_file(address: &str) -> Option<&str> {
    address.strip_prefix("file:").map(|rest| rest.rsplit_once('#').map(|(p, _)| p).unwrap_or(rest))
}

/// What one file in a title's folder is.
#[derive(Clone, Debug, PartialEq)]
pub enum Class {
    /// The film, a version or part of it, or an episode.
    Main,
    Extra(ExtraKind, String),
    Art(ArtKind, Option<u32>),
    /// A picture named after a video beside it; which it is depends on what that video is.
    PictureOf(String),
    Notes,
    Theme,
    Ignored,
}

const IMAGES: &[&str] = &["jpg", "jpeg", "png", "webp", "tbn"];
const AUDIO: &[&str] = &["mp3", "flac", "m4a", "ogg", "wav", "aac", "opus"];

fn extra_folder(dir: &str) -> Option<ExtraKind> {
    Some(match dir {
        "trailers" | "trailer" => ExtraKind::Trailer,
        "samples" | "sample" => ExtraKind::Sample,
        "behind the scenes" => ExtraKind::BehindTheScenes,
        "deleted scenes" => ExtraKind::DeletedScene,
        "featurettes" => ExtraKind::Featurette,
        "interviews" => ExtraKind::Interview,
        "scenes" => ExtraKind::Scene,
        "shorts" => ExtraKind::Short,
        "clips" => ExtraKind::Clip,
        "other" | "extras" | "extra" | "specials features" | "special features" | "bonus" => ExtraKind::Other,
        "backdrops" => ExtraKind::ThemeVideo,
        _ => return None,
    })
}

/// The marker on the end of an extra's name, and the name without it.
fn extra_suffix(stem: &str) -> Option<(ExtraKind, String)> {
    let lower = stem.to_lowercase();
    // Trailers and samples are marked with any of four separators; the rest with a hyphen only.
    for (word, kind) in [("trailer", ExtraKind::Trailer), ("sample", ExtraKind::Sample)] {
        if lower == word {
            return Some((kind, String::new()));
        }
        for sep in ['-', '.', '_', ' '] {
            if lower.ends_with(&format!("{sep}{word}")) {
                return Some((kind, stem[..stem.len() - word.len() - 1].trim().to_string()));
            }
        }
    }
    for (word, kind) in [
        ("behindthescenes", ExtraKind::BehindTheScenes),
        ("deletedscene", ExtraKind::DeletedScene),
        ("deleted", ExtraKind::DeletedScene),
        ("featurette", ExtraKind::Featurette),
        ("interview", ExtraKind::Interview),
        ("scene", ExtraKind::Scene),
        ("short", ExtraKind::Short),
        ("clip", ExtraKind::Clip),
        ("other", ExtraKind::Other),
        ("extra", ExtraKind::Other),
    ] {
        if lower.ends_with(&format!("-{word}")) {
            return Some((kind, stem[..stem.len() - word.len() - 1].trim().to_string()));
        }
    }
    None
}

fn art_name(word: &str) -> Option<ArtKind> {
    Some(match word {
        "poster" | "folder" | "cover" | "default" | "movie" | "show" | "keyart" => ArtKind::Poster,
        "backdrop" | "fanart" | "background" | "art" => ArtKind::Backdrop,
        "banner" => ArtKind::Banner,
        "logo" | "clearlogo" => ArtKind::Logo,
        "landscape" | "thumb" => ArtKind::Thumb,
        "clearart" => ArtKind::ClearArt,
        "disc" | "cdart" | "discart" => ArtKind::Disc,
        "square" | "squareart" | "backgroundsquare" => ArtKind::Square,
        _ => return None,
    })
}

/// Drop the number that tells one of several pictures from the next: "fanart-2", "backdrop3".
fn unnumbered(word: &str) -> &str {
    let t = word.trim_end_matches(|c: char| c.is_ascii_digit());
    if t.len() == word.len() { word } else { t.trim_end_matches('-') }
}

/// "season 01", "season01", "specials", "season 1 (2019)": the season a folder holds.
pub fn season_folder(dir: &str) -> Option<u32> {
    let lower = dir.trim().to_lowercase();
    if lower == "specials" {
        return Some(0);
    }
    let rest = lower.strip_prefix("season")?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// A season's picture by its name alone: "Season01", "season01a", "season01-poster",
/// "season-specials-banner", "season01-fanart".
fn season_art(stem: &str) -> Option<(ArtKind, u32)> {
    let lower = stem.to_lowercase();
    let rest = lower.strip_prefix("season")?;
    let (season, tail) = if let Some(t) = rest.strip_prefix("-specials") {
        (0, t)
    } else {
        let digits: String = rest.trim_start_matches(['-', ' ']).chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        let at = rest.find(&digits)? + digits.len();
        (digits.parse().ok()?, &rest[at..])
    };
    let tail = tail.trim_start_matches('-');
    // Plex tells several posters apart with a letter: Season01a, Season01b.
    if tail.is_empty() || (tail.len() == 1 && tail.chars().all(|c| c.is_ascii_lowercase())) {
        return Some((ArtKind::Poster, season));
    }
    let word = tail.trim_end_matches(|c: char| c.is_ascii_lowercase() && tail.len() > 1 && art_name(tail).is_none() && art_name(&tail[..tail.len() - 1]).is_some());
    art_name(unnumbered(word)).or_else(|| art_name(unnumbered(&tail[..tail.len().saturating_sub(1)]))).map(|k| (k, season))
}

/// Say what a file is from where it lies in the title's folder and what it is called.
/// `videos` are the stems of the videos beside it, for pictures and notes named after one.
pub fn classify(rel: &str, kind: Kind, sibling_videos: &[String]) -> Class {
    let path = Path::new(rel);
    let dirs: Vec<String> = path.parent().map(|p| p.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect()).unwrap_or_default();
    let file = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if dirs.iter().any(|d| d == ".spool-recycle") || file.starts_with("._") {
        return Class::Ignored;
    }
    if file == ".plexmatch" {
        return if dirs.is_empty() { Class::Notes } else { Class::Ignored };
    }
    let (stem, ext) = match file.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), e.to_lowercase()),
        None => return Class::Ignored,
    };
    let lower = stem.to_lowercase();
    let in_extras = dirs.iter().find_map(|d| extra_folder(d));

    if is_video_file(&file) {
        if let Some(k) = in_extras {
            return Class::Extra(k, stem);
        }
        return match extra_suffix(&stem) {
            // A release name ends in the group that made it, and groups are called all sorts
            // of things: "Film.1999.1080p.BluRay.x264-SCENE" is the film. Only a trailer or a
            // sample is still marked as one by a name like that.
            Some((k, _)) if !matches!(k, ExtraKind::Trailer | ExtraKind::Sample) && spool_core::quality::parse_quality(&stem, kind.flavor()).quality != spool_core::Quality::Unknown => Class::Main,
            Some((k, name)) => Class::Extra(k, if name.is_empty() { stem } else { name }),
            None => Class::Main,
        };
    }
    if AUDIO.contains(&ext.as_str()) {
        return if lower == "theme" || dirs.last().is_some_and(|d| d == "theme-music") { Class::Theme } else { Class::Ignored };
    }
    if ext == "nfo" {
        // The title's own notes: at the top of its folder, or beside a film and named after it.
        let own = dirs.is_empty() && (matches!(lower.as_str(), "movie" | "tvshow") || (kind == Kind::Movie && (sibling_videos.is_empty() || sibling_videos.iter().any(|v| v.to_lowercase() == lower))));
        return if own { Class::Notes } else { Class::Ignored };
    }
    if !IMAGES.contains(&ext.as_str()) || in_extras.is_some() {
        return Class::Ignored;
    }
    // Pictures. A folder of more backdrops first, then names that stand for a season.
    if dirs.last().is_some_and(|d| d == "extrafanart") {
        return Class::Art(ArtKind::Backdrop, None);
    }
    if dirs.iter().any(|d| d == "extrathumbs" || d == ".actors" || d == "metadata") {
        return Class::Ignored;
    }
    let season_here = dirs.last().and_then(|d| season_folder(d));
    if kind == Kind::Series {
        if let Some((k, season)) = season_art(&stem) {
            return Class::Art(k, Some(season));
        }
    }
    // A picture named after a video beside it, with or without saying what it is.
    for v in sibling_videos {
        let v = v.to_lowercase();
        if lower == v {
            return Class::PictureOf(v);
        }
        if let Some(tail) = lower.strip_prefix(&v).and_then(|t| t.strip_prefix('-')) {
            return match art_name(unnumbered(tail)) {
                Some(ArtKind::Thumb) if kind == Kind::Series => Class::PictureOf(v),
                Some(k) if kind == Kind::Movie => Class::Art(k, None),
                Some(k) => Class::Art(k, season_here),
                None => Class::Ignored,
            };
        }
    }
    // A name that says what it is, alone or after a hyphen: "poster", "Film (1999)-fanart-2".
    let word = unnumbered(&lower);
    let found = art_name(word).or_else(|| word.rsplit_once('-').and_then(|(_, tail)| art_name(tail)).filter(|k| *k != ArtKind::Poster || !word.ends_with("-default")));
    match found {
        Some(k) if kind == Kind::Series && !dirs.is_empty() => match season_here {
            Some(season) => Class::Art(k, Some(season)),
            None => Class::Ignored,
        },
        Some(k) if dirs.is_empty() => Class::Art(k, None),
        _ => Class::Ignored,
    }
}

/// A part of a film split across files: "Film-cd1", "Film - part 2", "Film.pt-a".
pub fn is_part(stem: &str) -> bool {
    let lower = stem.to_lowercase();
    let tail = lower.trim_end_matches(|c: char| c.is_ascii_digit());
    let numbered = tail.len() < lower.len();
    let tail = if numbered { tail } else { lower.strip_suffix(['a', 'b', 'c', 'd']).unwrap_or("") };
    let tail = tail.trim_end_matches([' ', '.', '-', '_']);
    ["cd", "dvd", "part", "pt", "disc", "disk"].iter().any(|w| tail.strip_suffix(w).is_some_and(|before| before.ends_with([' ', '.', '-', '_'])))
}

/// When a video has been replaced by one with another name, bring along the files a media
/// server named after the old one, a poster or its notes, so they still belong to it. Nothing
/// is overwritten, and subtitles, which were timed for the old video, are not brought. Returns
/// what was renamed, as (from, to).
pub fn follow_rename(old_video: &Path, new_video: &Path) -> Vec<(std::path::PathBuf, std::path::PathBuf)> {
    let (Some(dir), Some(new_dir)) = (old_video.parent(), new_video.parent()) else { return vec![] };
    let (Some(old_stem), Some(new_stem)) = (old_video.file_stem().map(|s| s.to_string_lossy().to_string()), new_video.file_stem().map(|s| s.to_string_lossy().to_string())) else { return vec![] };
    if old_stem == new_stem || old_video.exists() {
        return vec![];
    }
    let mut moved = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix(&old_stem) else { continue };
        let ext = rest.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
        // "Film.nfo", "Film.jpg", "Film-poster.jpg", "Film-fanart-2.png".
        let named_after = rest.starts_with('.') && rest.matches('.').count() == 1 || rest.starts_with('-');
        if !named_after || !(ext == "nfo" || IMAGES.contains(&ext.as_str())) || !e.path().is_file() {
            continue;
        }
        let to = new_dir.join(format!("{new_stem}{rest}"));
        if !to.exists() && std::fs::rename(e.path(), &to).is_ok() {
            moved.push((e.path(), to));
        }
    }
    moved
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let len = text[start..].find(close)?;
    Some(text[start..start + len].trim())
}

fn digits_after(text: &str, marker: &str) -> Option<u32> {
    let at = text.find(marker)? + marker.len();
    let d: String = text[at..].chars().take_while(|c| c.is_ascii_digit()).collect();
    d.parse().ok()
}

fn imdb_in(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(at) = rest.find("tt") {
        let d: String = rest[at + 2..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if d.len() >= 6 {
            return Some(format!("tt{d}"));
        }
        rest = &rest[at + 2..];
    }
    None
}

/// Read what a notes file says the title is: an .nfo as Kodi, Jellyfin and Emby write them
/// (or one holding only a link to the title's page), or Plex's .plexmatch.
pub fn read_notes(rel_path: &str, text: &str) -> Notes {
    let mut n = Notes { rel_path: rel_path.to_string(), ..Default::default() };
    if rel_path.ends_with(".plexmatch") {
        for line in text.lines() {
            let Some((key, value)) = line.split_once(':') else { continue };
            let value = value.trim();
            match key.trim().to_lowercase().as_str() {
                "tmdbid" => n.tmdb_id = value.parse().ok(),
                "tvdbid" => n.tvdb_id = value.parse().ok(),
                "imdbid" => n.imdb_id = imdb_in(value),
                "title" | "show" => n.title = value.to_string(),
                "year" => n.year = value.parse().unwrap_or(0),
                _ => {}
            }
        }
        return n;
    }
    let lower = text.to_lowercase();
    // <uniqueid type="tmdb">603</uniqueid>, in any order of attributes.
    let mut rest = lower.as_str();
    while let Some(at) = rest.find("<uniqueid") {
        let tag_end = rest[at..].find('>').map(|i| at + i).unwrap_or(rest.len());
        let attrs = &rest[at..tag_end];
        let value = rest.get(tag_end + 1..).and_then(|r| r.find('<').map(|i| r[..i].trim())).unwrap_or("");
        if attrs.contains("\"tmdb\"") || attrs.contains("'tmdb'") {
            n.tmdb_id = n.tmdb_id.or(value.parse().ok());
        } else if attrs.contains("\"tvdb\"") || attrs.contains("'tvdb'") {
            n.tvdb_id = n.tvdb_id.or(value.parse().ok());
        } else if attrs.contains("\"imdb\"") || attrs.contains("'imdb'") {
            n.imdb_id = n.imdb_id.take().or(imdb_in(value));
        }
        rest = &rest[tag_end.min(rest.len())..];
    }
    n.tmdb_id = n.tmdb_id.or_else(|| between(&lower, "<tmdbid>", "<").and_then(|v| v.parse().ok())).or_else(|| digits_after(&lower, "themoviedb.org/movie/"));
    n.tvdb_id = n.tvdb_id.or_else(|| between(&lower, "<tvdbid>", "<").and_then(|v| v.parse().ok())).or_else(|| digits_after(&lower, "thetvdb.com/?tab=series&id=")).or_else(|| digits_after(&lower, "thetvdb.com/dereferrer/series/"));
    n.imdb_id = n.imdb_id.take().or_else(|| between(&lower, "<imdbid>", "<").and_then(imdb_in)).or_else(|| between(&lower, "<imdb_id>", "<").and_then(imdb_in)).or_else(|| between(&lower, "<id>", "<").and_then(imdb_in)).or_else(|| lower.find("imdb.com/title/").and_then(|at| imdb_in(&lower[at..])));
    // The title keeps its capitals, so it is cut from the text as written.
    if let (Some(start), Some(end)) = (lower.find("<title>"), lower.find("</title>")) {
        if start + 7 <= end {
            n.title = text[start + 7..end].trim().replace("&amp;", "&").replace("&apos;", "'").replace("&quot;", "\"");
        }
    }
    n.year = between(&lower, "<year>", "<").and_then(|v| v.parse().ok()).or_else(|| between(&lower, "<premiered>", "<").and_then(|v| v.get(..4)).and_then(|v| v.parse().ok())).unwrap_or(0);
    n
}

/// Walk a title's folder. Returns the films or episodes found, as paths within it, and
/// everything else worth knowing about. Reads the folder and its notes; changes nothing.
pub fn survey(root: &Path, kind: Kind) -> (Vec<String>, Local) {
    let mut files = vec![];
    crate::catalog::collect(root, &mut files);
    let mut rels: Vec<String> = files.iter().filter_map(|p| p.strip_prefix(root).ok()).map(|r| r.to_string_lossy().to_string()).collect();
    rels.sort_by_key(|r| r.to_lowercase());
    let dir_of = |rel: &str| Path::new(rel).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    let stem_of = |rel: &str| Path::new(rel).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    // Videos by folder, for the pictures and notes named after them.
    let videos: Vec<(String, String, String)> = rels.iter().filter(|r| is_video_file(r)).map(|r| (dir_of(r), stem_of(r), r.clone())).collect();

    let mut mains = vec![];
    let mut local = Local::default();
    for rel in &rels {
        let dir = dir_of(rel);
        let siblings: Vec<String> = videos.iter().filter(|v| v.0 == dir).map(|v| v.1.clone()).collect();
        let meta = std::fs::metadata(root.join(rel)).ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = meta.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
        match classify(rel, kind, &siblings) {
            Class::Main => mains.push(rel.clone()),
            Class::Extra(kind, name) => local.extras.push(Extra { rel_path: rel.clone(), kind, name, size }),
            Class::Art(kind, season) => local.art.push(LocalArt { rel_path: rel.clone(), kind, season, of: None, size, modified }),
            Class::PictureOf(stem) => {
                let video = videos.iter().find(|v| v.0 == dir && v.1.to_lowercase() == stem).map(|v| v.2.clone());
                let is_extra = video.as_deref().is_some_and(|v| classify(v, kind, &[]) != Class::Main);
                match (kind, video) {
                    (_, Some(_)) if is_extra => {}
                    // Plex's way of giving a film its poster: a picture with the film's own name.
                    (Kind::Movie, Some(_)) => local.art.push(LocalArt { rel_path: rel.clone(), kind: ArtKind::Poster, season: None, of: None, size, modified }),
                    (Kind::Series, Some(v)) => local.art.push(LocalArt { rel_path: rel.clone(), kind: ArtKind::Thumb, season: None, of: Some(v), size, modified }),
                    _ => {}
                }
            }
            Class::Notes => {
                // The first notes that name the title win; .plexmatch sorts ahead of the rest.
                if local.notes.rel_path.is_empty() || (local.notes.tmdb_id.is_none() && local.notes.tvdb_id.is_none() && local.notes.imdb_id.is_none()) {
                    if let Ok(bytes) = std::fs::read(root.join(rel)) {
                        if bytes.len() <= 1 << 20 {
                            local.notes = read_notes(rel, &String::from_utf8_lossy(&bytes));
                        }
                    }
                }
            }
            Class::Theme => local.themes.push(rel.clone()),
            Class::Ignored => {}
        }
    }
    (mains, local)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn movie(rel: &str, videos: &[&str]) -> Class {
        classify(rel, Kind::Movie, &videos.iter().map(|v| v.to_string()).collect::<Vec<_>>())
    }
    fn show(rel: &str, videos: &[&str]) -> Class {
        classify(rel, Kind::Series, &videos.iter().map(|v| v.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn extras_are_told_from_the_film_by_folder_or_by_name() {
        assert_eq!(movie("Heat (1995).mkv", &[]), Class::Main);
        assert_eq!(movie("Heat (1995) - 2160p.mkv", &[]), Class::Main);
        assert_eq!(movie("Trailers/Teaser.mp4", &[]), Class::Extra(ExtraKind::Trailer, "Teaser".into()));
        assert_eq!(movie("Behind The Scenes/Making Of.mkv", &[]), Class::Extra(ExtraKind::BehindTheScenes, "Making Of".into()));
        assert_eq!(movie("Featurettes/Sub/Deep.mkv", &[]), Class::Extra(ExtraKind::Featurette, "Deep".into()));
        assert_eq!(movie("Extras/Gag Reel.mkv", &[]), Class::Extra(ExtraKind::Other, "Gag Reel".into()));
        assert_eq!(movie("Heat (1995)-trailer.mp4", &[]), Class::Extra(ExtraKind::Trailer, "Heat (1995)".into()));
        assert_eq!(movie("heat.1995.sample.mkv", &[]), Class::Extra(ExtraKind::Sample, "heat.1995".into()));
        assert_eq!(movie("trailer.mkv", &[]), Class::Extra(ExtraKind::Trailer, "trailer".into()));
        assert_eq!(movie("The Coffee Scene-scene.mkv", &[]), Class::Extra(ExtraKind::Scene, "The Coffee Scene".into()));
        assert_eq!(movie("Pacino-interview.mkv", &[]), Class::Extra(ExtraKind::Interview, "Pacino".into()));
        assert_eq!(movie("Cut Ending-deleted.mkv", &[]), Class::Extra(ExtraKind::DeletedScene, "Cut Ending".into()));
        assert_eq!(movie("backdrops/loop.mp4", &[]), Class::Extra(ExtraKind::ThemeVideo, "loop".into()));
        // A release group's name is not a marker, in the names indexers use or in Spool's own.
        assert_eq!(movie("Heat.1995.1080p.BluRay.x264-SCENE.mkv", &[]), Class::Main);
        assert_eq!(movie("Heat (1995) {imdb-tt0113277} [Bluray-1080p][DTS 5.1][x264]-OTHER.mkv", &[]), Class::Main);
        assert_eq!(show("Season 01/Show - S01E01 - Pilot [WEBDL-1080p][AAC 2.0][h264]-EXTRA.mkv", &[]), Class::Main);
        assert_eq!(movie("heat.1995.1080p.bluray.x264-grp-sample.mkv", &[]), Class::Extra(ExtraKind::Sample, "heat.1995.1080p.bluray.x264-grp".into()));
        // Words that only look like markers are part of the name.
        assert_eq!(movie("The Other (1972).mkv", &[]), Class::Main);
        assert_eq!(movie("Trailer Park Boys (2006).mkv", &[]), Class::Main);
        assert_eq!(movie("Short Cuts (1993).mkv", &[]), Class::Main);
        assert_eq!(movie(".spool-recycle/20260101/old.mkv", &[]), Class::Ignored);
        assert_eq!(show("Season 01/Show S01E01.mkv", &[]), Class::Main);
        assert_eq!(show("Season 01/Deleted Scenes/cut.mkv", &[]), Class::Extra(ExtraKind::DeletedScene, "cut".into()));
        assert_eq!(show("Trailers/Season 2 Trailer.mkv", &[]), Class::Extra(ExtraKind::Trailer, "Season 2 Trailer".into()));
    }

    #[test]
    fn artwork_is_recognised_under_every_name_the_servers_use() {
        let art = |k| Class::Art(k, None);
        for name in ["poster.jpg", "folder.jpeg", "cover.png", "default.jpg", "movie.tbn", "Poster.JPG", "poster-2.png", "keyart.jpg"] {
            assert_eq!(movie(name, &["Heat (1995)"]), art(ArtKind::Poster), "{name}");
        }
        for name in ["fanart.jpg", "backdrop.webp", "background.jpg", "art.png", "backdrop-1.jpg", "backdrop2.jpg", "extrafanart/fanart3.jpg"] {
            assert_eq!(movie(name, &["Heat (1995)"]), art(ArtKind::Backdrop), "{name}");
        }
        assert_eq!(movie("logo.png", &[]), art(ArtKind::Logo));
        assert_eq!(movie("clearlogo-1.png", &[]), art(ArtKind::Logo));
        assert_eq!(movie("banner.jpg", &[]), art(ArtKind::Banner));
        assert_eq!(movie("landscape.jpg", &[]), art(ArtKind::Thumb));
        assert_eq!(movie("clearart.png", &[]), art(ArtKind::ClearArt));
        assert_eq!(movie("discart.png", &[]), art(ArtKind::Disc));
        assert_eq!(movie("squareArt.jpg", &[]), art(ArtKind::Square));
        // Named after the film, the way Kodi and Plex write them.
        assert_eq!(movie("Heat (1995)-poster.jpg", &["Heat (1995)"]), art(ArtKind::Poster));
        assert_eq!(movie("Heat (1995)-fanart-1.jpg", &["Heat (1995)"]), art(ArtKind::Backdrop));
        assert_eq!(movie("Heat (1995)-clearlogo.png", &["Heat (1995)"]), art(ArtKind::Logo));
        assert_eq!(movie("Heat (1995).jpg", &["Heat (1995)"]), Class::PictureOf("heat (1995)".into()));
        // Not artwork: a picture that says nothing about what it is, or sits with the extras.
        assert_eq!(movie("Spider-Man.jpg", &["Heat (1995)"]), Class::Ignored);
        assert_eq!(movie("holiday.jpg", &[]), Class::Ignored);
        assert_eq!(movie("Trailers/poster.jpg", &[]), Class::Ignored);
        assert_eq!(movie("extrathumbs/thumb1.jpg", &[]), Class::Ignored);

        assert_eq!(show("poster.jpg", &[]), art(ArtKind::Poster));
        assert_eq!(show("show.jpg", &[]), art(ArtKind::Poster));
        assert_eq!(show("Season 01/Season01.jpg", &[]), Class::Art(ArtKind::Poster, Some(1)));
        assert_eq!(show("Season 01/season01b.jpg", &[]), Class::Art(ArtKind::Poster, Some(1)));
        assert_eq!(show("season02-poster.jpg", &[]), Class::Art(ArtKind::Poster, Some(2)));
        assert_eq!(show("season02-banner.jpg", &[]), Class::Art(ArtKind::Banner, Some(2)));
        assert_eq!(show("season-specials-poster.jpg", &[]), Class::Art(ArtKind::Poster, Some(0)));
        assert_eq!(show("Season 03/poster.jpg", &[]), Class::Art(ArtKind::Poster, Some(3)));
        assert_eq!(show("Specials/folder.jpg", &[]), Class::Art(ArtKind::Poster, Some(0)));
        assert_eq!(show("Season 01/Show S01E01-thumb.jpg", &["Show S01E01"]), Class::PictureOf("show s01e01".into()));
        assert_eq!(show("Season 01/Show S01E01.jpg", &["Show S01E01"]), Class::PictureOf("show s01e01".into()));
    }

    #[test]
    fn notes_and_theme_music_are_found_where_each_server_leaves_them() {
        assert_eq!(movie("movie.nfo", &["Heat (1995)"]), Class::Notes);
        assert_eq!(movie("Heat (1995).nfo", &["Heat (1995)"]), Class::Notes);
        assert_eq!(movie("release-group.nfo", &["Heat (1995)"]), Class::Ignored, "a release's own notes say nothing a server wrote");
        assert_eq!(show("tvshow.nfo", &[]), Class::Notes);
        assert_eq!(show("Season 01/Show S01E01.nfo", &["Show S01E01"]), Class::Ignored);
        assert_eq!(show(".plexmatch", &[]), Class::Notes);
        assert_eq!(movie("theme.mp3", &[]), Class::Theme);
        assert_eq!(show("theme-music/opening.flac", &[]), Class::Theme);
        assert_eq!(movie("soundtrack.mp3", &[]), Class::Ignored);

        let kodi = read_notes("movie.nfo", "<movie><title>Fast &amp; Furious</title><year>2009</year><uniqueid type=\"imdb\">tt1013752</uniqueid><uniqueid default=\"true\" type=\"tmdb\">13804</uniqueid></movie>");
        assert_eq!((kodi.tmdb_id, kodi.imdb_id.as_deref(), kodi.title.as_str(), kodi.year), (Some(13804), Some("tt1013752"), "Fast & Furious", 2009));
        let old = read_notes("Heat (1995).nfo", "<movie>\n<id>tt0113277</id>\n<tmdbid>949</tmdbid>\n<premiered>1995-12-15</premiered></movie>");
        assert_eq!((old.tmdb_id, old.imdb_id.as_deref(), old.year), (Some(949), Some("tt0113277"), 1995));
        assert_eq!(read_notes("movie.nfo", "https://www.imdb.com/title/tt0113277/\n").imdb_id.as_deref(), Some("tt0113277"));
        assert_eq!(read_notes("movie.nfo", "https://www.themoviedb.org/movie/949-heat").tmdb_id, Some(949));
        let show_notes = read_notes("tvshow.nfo", "<tvshow><title>The Wire</title><uniqueid type='tvdb' default='true'>79126</uniqueid></tvshow>");
        assert_eq!((show_notes.tvdb_id, show_notes.title.as_str()), (Some(79126), "The Wire"));
        let plex = read_notes(".plexmatch", "title: The Wire\nyear: 2002\ntvdbid: 79126\nimdbid: tt0306414\n");
        assert_eq!((plex.tvdb_id, plex.imdb_id.as_deref(), plex.year), (Some(79126), Some("tt0306414"), 2002));
    }

    #[test]
    fn a_film_in_parts_is_known_by_its_markers() {
        for name in ["Film-cd1", "Film (1999) - part2", "Film.pt-1", "Film disc 2", "Film-cda", "Film_dvd1"] {
            assert!(is_part(name), "{name}");
        }
        for name in ["Film (1999)", "Film (1999) - 2160p", "Apt", "District 9", "Part", "Se7en"] {
            assert!(!is_part(name), "{name}");
        }
        assert_eq!(season_folder("Season 01"), Some(1));
        assert_eq!(season_folder("season 2 (2019)"), Some(2));
        assert_eq!(season_folder("Specials"), Some(0));
        assert_eq!(season_folder("Seasonal"), None);
    }
}

#[cfg(test)]
mod against_a_real_library {
    use super::*;

    /// Not a test of the code so much as of a library: given every file path Spool has on
    /// record (SPOOL_PATHS, a JSON list), say which this module would no longer count as the
    /// film or an episode. Run by hand before a release that changes the rules.
    #[test]
    #[ignore]
    fn which_recorded_files_would_stop_being_the_film() {
        let Ok(path) = std::env::var("SPOOL_PATHS") else { return };
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let (mut files, mut changed) = (0, 0);
        for r in &rows {
            let kind = if r["kind"] == "movie" { Kind::Movie } else { Kind::Series };
            for f in r["files"].as_array().unwrap() {
                files += 1;
                let rel = f["rel"].as_str().unwrap();
                let class = classify(rel, kind, &[]);
                if class != Class::Main {
                    changed += 1;
                    println!("{:?}  monitored={}  {} :: {rel}", class, r["monitored"], r["title"]);
                }
            }
        }
        println!("{files} files on record, {changed} would no longer count as the film or an episode");
    }
}
