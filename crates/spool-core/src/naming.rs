//! File and folder naming. Token handling follows Radarr's and Sonarr's `FileNameBuilder` (GPLv3)
//! closely enough that Spool computes the same paths those apps already wrote to disk.

use crate::mediainfo::MediaInfo;
use crate::parser::common::{is_match, re, replace_all};
use crate::quality::{Flavor, QualityModel};
use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColonReplacement {
    Delete,
    Dash,
    SpaceDash,
    SpaceDashSpace,
    Smart,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NamingConfig {
    pub rename: bool,
    pub replace_illegal_characters: bool,
    pub movie_colon_replacement: ColonReplacement,
    pub series_colon_replacement: ColonReplacement,
    pub movie_file_format: String,
    pub movie_folder_format: String,
    pub episode_file_format: String,
    pub series_folder_format: String,
    pub season_folder_format: String,
    pub specials_folder_format: String,
}

impl Default for NamingConfig {
    fn default() -> Self {
        NamingConfig {
            rename: true,
            replace_illegal_characters: true,
            movie_colon_replacement: ColonReplacement::SpaceDash,
            series_colon_replacement: ColonReplacement::Smart,
            movie_file_format: "{Movie CleanTitle} {(Release Year)} {imdb-{ImdbId}} {edition-{Edition Tags}} {[Custom Formats]}{[Quality Full]}{[MediaInfo 3D]}{[MediaInfo VideoDynamicRangeType]}{[Mediainfo AudioCodec}{ Mediainfo AudioChannels}][{Mediainfo VideoCodec}]{-Release Group}".into(),
            movie_folder_format: "{Movie Title} ({Release Year})".into(),
            episode_file_format: "{Series Title} - S{season:00}E{episode:00} - {Episode Title} {Quality Full}".into(),
            series_folder_format: "{Series Title}".into(),
            season_folder_format: "Season {season}".into(),
            specials_folder_format: "Specials".into(),
        }
    }
}

static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)(?<tag>\{(?<tagprefix>[-{ ._\[(]*)(?:imdb(?:id)?-|tmdb(?:id)?-|tvdb(?:id)?-|edition-))?\{(?<prefix>[-{ ._\[(]*)(?<token>(?:[a-z0-9]+)(?:(?<separator>[- ._]+)(?:[a-z0-9]+))?)(?::(?<format>[ ,a-z0-9|+-]+(?<![- ])))?(?<suffix>[-} ._)\]]*)\}")
});
static CLEANUP: LazyLock<Regex> = LazyLock::new(|| re(r"([- ._])(\1)+"));
static TRIM_SEPARATORS: LazyLock<Regex> = LazyLock::new(|| re(r"[- ._]+$"));
static SCENIFY_REMOVE: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"(?i)(?<=\s)(,|<|>|\/|\\|;|:|'|"|\||`|’|~|!|\?|@|$|%|^|\*|-|_|=){1}(?=\s)|('|`|’|:|\?|,)(?=(?:(?:s|m|t|ve|ll|d|re)\s)|\s|$)|(\(|\)|\[|\]|\{|\})"#)
});
static EDITION_ORDINAL: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)((?:\b|_)\d{1,3}(?:st|th|rd|nd|mm)(?:\b|_))"));
static EDITION_UPPER: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)((?:\b|_)(?:IMAX|3D|SDR|HDR|DV)(?:\b|_))"));
static RESERVED: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(?:aux|com[1-9]|con|lpt[1-9]|nul|prn)\."));

const BAD: [&str; 8] = ["\\", "/", "<", ">", "?", "*", "|", "\""];
const GOOD: [&str; 8] = ["+", "+", "", "", "!", "-", "", ""];

fn remove_diacritics(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfd().filter(|c| !unicode_normalization::char::is_combining_mark(*c)).collect()
}

/// Radarr's `CleanTitle`, used by `{Movie CleanTitle}`.
pub fn clean_title(title: &str) -> String {
    let t = title.replace('&', "and").replace('/', " ");
    remove_diacritics(&replace_all(&SCENIFY_REMOVE, &t, ""))
}

fn clean_file_name(name: &str, replace_illegal: bool, colon: ColonReplacement) -> String {
    let mut r = name.to_string();
    if replace_illegal {
        r = match colon {
            ColonReplacement::Smart => r.replace(": ", " - ").replace(':', "-"),
            ColonReplacement::Delete => r.replace(':', ""),
            ColonReplacement::Dash => r.replace(':', "-"),
            ColonReplacement::SpaceDash => r.replace(':', " -"),
            ColonReplacement::SpaceDashSpace => r.replace(':', " - "),
        };
    } else {
        r = r.replace(':', "");
    }
    for i in 0..BAD.len() {
        r = r.replace(BAD[i], if replace_illegal { GOOD[i] } else { "" });
    }
    r.trim_start_matches([' ', '.']).trim_end_matches(' ').to_string()
}

fn title_case(s: &str) -> String {
    let mut out = String::new();
    let mut start = true;
    for c in s.chars() {
        if c.is_alphanumeric() || c == '\'' {
            if start {
                out.extend(c.to_uppercase());
            } else {
                out.push(c);
            }
            start = false;
        } else {
            out.push(c);
            start = true;
        }
    }
    out
}

fn edition_token(edition: &str) -> String {
    let e = title_case(&edition.to_lowercase());
    let e = EDITION_ORDINAL.replace_all(&e, |c: &crate::parser::common::Captures| c[1].to_lowercase()).into_owned();
    EDITION_UPPER.replace_all(&e, |c: &crate::parser::common::Captures| c[1].to_uppercase()).into_owned()
}

type Tokens = HashMap<String, String>;

fn replace_tokens(pattern: &str, tokens: &Tokens, replace_illegal: bool, colon: ColonReplacement, default_group: &str) -> String {
    TOKEN
        .replace_all(pattern, |c: &crate::parser::common::Captures| {
            let g = |n: &str| c.name(n).map(|m| m.as_str()).unwrap_or("");
            let token = g("token");
            let key = token.to_lowercase().replace(['.', '-', '_'], " ");
            let (prefix, suffix) = (g("prefix"), g("suffix"));
            let mut text = match key.as_str() {
                "release group" => match tokens.get("release group").filter(|v| !v.trim().is_empty()) {
                    Some(v) => v.clone(),
                    None if prefix.is_empty() && suffix.is_empty() => default_group.to_string(),
                    None => String::new(),
                },
                "season" | "episode" => {
                    let width = g("format").len();
                    tokens
                        .get(&key)
                        .map(|v| v.split('-').map(|n| format!("{:0>width$}", n, width = width.max(1))).collect::<Vec<_>>().join("-"))
                        .unwrap_or_default()
                }
                _ => tokens.get(&key).cloned().unwrap_or_default(),
            };
            text = text.trim().to_string();
            let letters: Vec<char> = token.chars().filter(|ch| ch.is_alphabetic()).collect();
            if !letters.is_empty() && letters.iter().all(|ch| ch.is_lowercase()) {
                text = text.to_lowercase();
            } else if !letters.is_empty() && letters.iter().all(|ch| ch.is_uppercase()) {
                text = text.to_uppercase();
            }
            let sep = g("separator");
            if !sep.trim().is_empty() {
                text = text.replace(' ', sep);
            }
            text = clean_file_name(&text, replace_illegal, colon);
            if text.trim().is_empty() {
                String::new()
            } else {
                format!("{}{}{}{}", g("tag"), prefix, text, suffix)
            }
        })
        .into_owned()
}

fn finish_component(s: &str) -> String {
    let c = s.trim();
    let c = CLEANUP.replace_all(c, |m: &crate::parser::common::Captures| m[1].to_string()).into_owned();
    let c = replace_all(&TRIM_SEPARATORS, &c, "");
    if is_match(&RESERVED, &c) {
        c.replacen('.', "_", 1)
    } else {
        c
    }
}

fn media_tokens(t: &mut Tokens, mi: Option<&MediaInfo>) {
    let Some(mi) = mi else { return };
    t.insert("mediainfo videocodec".into(), mi.video_codec.clone());
    t.insert("mediainfo video".into(), mi.video_codec.clone());
    t.insert("mediainfo audiocodec".into(), mi.audio_codec.clone());
    t.insert("mediainfo audio".into(), mi.audio_codec.clone());
    t.insert("mediainfo audiochannels".into(), mi.audio_channels_formatted());
    t.insert("mediainfo videodynamicrangetype".into(), mi.dynamic_range_type.clone());
    t.insert("mediainfo videodynamicrange".into(), if mi.dynamic_range_type.is_empty() { String::new() } else { "HDR".into() });
    t.insert("mediainfo videobitdepth".into(), mi.bit_depth.max(8).to_string());
    t.insert("mediainfo 3d".into(), if mi.is_3d { "3D".into() } else { String::new() });
}

fn quality_tokens(t: &mut Tokens, q: Option<&QualityModel>, flavor: Flavor) {
    let Some(q) = q else { return };
    let title = q.quality.name(flavor);
    let proper = if q.revision.version > 1 { "Proper" } else { "" };
    let real = if q.revision.real > 0 { "REAL" } else { "" };
    t.insert("quality full".into(), format!("{title} {proper} {real}"));
    t.insert("quality title".into(), title.into());
    t.insert("quality proper".into(), proper.into());
    t.insert("quality real".into(), real.into());
}

pub struct MovieNameInput<'a> {
    pub title: &'a str,
    pub year: u32,
    pub imdb_id: Option<&'a str>,
    pub tmdb_id: Option<u32>,
    pub quality: Option<&'a QualityModel>,
    pub media_info: Option<&'a MediaInfo>,
    pub edition: &'a str,
    pub release_group: Option<&'a str>,
    /// Scene name or original file name, used when renaming is off.
    pub original_name: &'a str,
}

fn movie_tokens(m: &MovieNameInput) -> Tokens {
    let mut t = Tokens::new();
    t.insert("movie title".into(), m.title.into());
    t.insert("movie cleantitle".into(), clean_title(m.title));
    t.insert("release year".into(), if m.year == 0 { String::new() } else { m.year.to_string() });
    t.insert("imdbid".into(), m.imdb_id.unwrap_or("").into());
    t.insert("tmdbid".into(), m.tmdb_id.map(|i| i.to_string()).unwrap_or_default());
    t.insert("original title".into(), m.original_name.into());
    if !m.edition.trim().is_empty() {
        t.insert("edition tags".into(), edition_token(m.edition));
    }
    if let Some(g) = m.release_group {
        t.insert("release group".into(), g.into());
    }
    quality_tokens(&mut t, m.quality, Flavor::Movie);
    media_tokens(&mut t, m.media_info);
    t
}

/// File name without extension.
pub fn movie_file_name(cfg: &NamingConfig, m: &MovieNameInput) -> String {
    if !cfg.rename {
        return clean_file_name(m.original_name, cfg.replace_illegal_characters, cfg.movie_colon_replacement);
    }
    let t = movie_tokens(m);
    let s = replace_tokens(&cfg.movie_file_format, &t, cfg.replace_illegal_characters, cfg.movie_colon_replacement, "Radarr");
    finish_component(&s)
}

pub fn movie_folder_name(cfg: &NamingConfig, title: &str, year: u32, imdb_id: Option<&str>, tmdb_id: Option<u32>) -> String {
    let m = MovieNameInput { title, year, imdb_id, tmdb_id, quality: None, media_info: None, edition: "", release_group: None, original_name: "" };
    let t = movie_tokens(&m);
    let s = replace_tokens(&cfg.movie_folder_format, &t, cfg.replace_illegal_characters, cfg.movie_colon_replacement, "");
    let s = CLEANUP.replace_all(&s, |m: &crate::parser::common::Captures| m[1].to_string()).into_owned();
    s.trim_matches([' ', '.']).to_string()
}

pub struct EpisodeNameInput<'a> {
    pub series_title: &'a str,
    pub series_year: u32,
    pub season: u32,
    /// Episode numbers in this file, ascending.
    pub episodes: &'a [u32],
    pub episode_titles: &'a [&'a str],
    pub air_date: Option<&'a str>,
    pub quality: Option<&'a QualityModel>,
    pub media_info: Option<&'a MediaInfo>,
    pub release_group: Option<&'a str>,
    pub original_name: &'a str,
}

static PART_SUFFIX: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\s*\((\d+|part \d+)\)$|\s*part \d+$"));

fn episode_title(titles: &[&str]) -> String {
    if titles.len() <= 1 {
        return titles.first().copied().unwrap_or("").to_string();
    }
    let stripped: Vec<String> = titles.iter().map(|t| replace_all(&PART_SUFFIX, t, "")).collect();
    let mut distinct: Vec<String> = Vec::new();
    for s in stripped {
        if !distinct.contains(&s) {
            distinct.push(s);
        }
    }
    distinct.join(" + ")
}

/// File name without extension.
pub fn episode_file_name(cfg: &NamingConfig, e: &EpisodeNameInput) -> String {
    if !cfg.rename {
        return clean_file_name(e.original_name, cfg.replace_illegal_characters, cfg.series_colon_replacement);
    }
    let mut t = Tokens::new();
    t.insert("series title".into(), e.series_title.into());
    t.insert("series cleantitle".into(), clean_title(e.series_title));
    t.insert("series year".into(), if e.series_year == 0 { String::new() } else { e.series_year.to_string() });
    t.insert("season".into(), e.season.to_string());
    t.insert("episode".into(), e.episodes.iter().map(|n| n.to_string()).collect::<Vec<_>>().join("-"));
    t.insert("episode title".into(), episode_title(e.episode_titles));
    t.insert("episode cleantitle".into(), clean_title(&episode_title(e.episode_titles)));
    t.insert("air date".into(), e.air_date.unwrap_or("").into());
    t.insert("original title".into(), e.original_name.into());
    if let Some(g) = e.release_group {
        t.insert("release group".into(), g.into());
    }
    quality_tokens(&mut t, e.quality, Flavor::Tv);
    media_tokens(&mut t, e.media_info);
    let s = replace_tokens(&cfg.episode_file_format, &t, cfg.replace_illegal_characters, cfg.series_colon_replacement, "Sonarr");
    finish_component(&s)
}

pub fn series_folder_name(cfg: &NamingConfig, title: &str, year: u32) -> String {
    let mut t = Tokens::new();
    t.insert("series title".into(), title.into());
    t.insert("series cleantitle".into(), clean_title(title));
    t.insert("series year".into(), if year == 0 { String::new() } else { year.to_string() });
    let s = replace_tokens(&cfg.series_folder_format, &t, cfg.replace_illegal_characters, cfg.series_colon_replacement, "");
    let s = CLEANUP.replace_all(&s, |m: &crate::parser::common::Captures| m[1].to_string()).into_owned();
    s.trim_matches([' ', '.']).to_string()
}

pub fn season_folder_name(cfg: &NamingConfig, season: u32) -> String {
    if season == 0 {
        return cfg.specials_folder_format.clone();
    }
    let mut t = Tokens::new();
    t.insert("season".into(), season.to_string());
    let s = replace_tokens(&cfg.season_folder_format, &t, cfg.replace_illegal_characters, cfg.series_colon_replacement, "");
    s.trim_matches([' ', '.']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality::{Quality, Revision};

    fn mi(v: &str, a: &str, ch: f64, hdr: &str) -> MediaInfo {
        MediaInfo { video_codec: v.into(), audio_codec: a.into(), audio_channels: ch, dynamic_range_type: hdr.into(), ..Default::default() }
    }

    #[test]
    fn movie_names_match_files_radarr_wrote() {
        let cfg = NamingConfig::default();
        let q = QualityModel { quality: Quality::Bluray1080p, revision: Revision { version: 2, real: 0, is_repack: false } };
        let info = mi("x265", "EAC3", 5.1, "HDR10");
        let name = movie_file_name(&cfg, &MovieNameInput {
            title: "Elizabeth", year: 1998, imdb_id: Some("tt0127536"), tmdb_id: None, quality: Some(&q),
            media_info: Some(&info), edition: "", release_group: Some("SM737"), original_name: "",
        });
        assert_eq!(name, "Elizabeth (1998) {imdb-tt0127536} [Bluray-1080p Proper][HDR10][EAC3 5.1][x265]-SM737");

        let q = QualityModel::new(Quality::Bluray1080p);
        let info = mi("x265", "EAC3", 5.1, "DV HDR10");
        let name = movie_file_name(&cfg, &MovieNameInput {
            title: "Jacob's Ladder", year: 1990, imdb_id: Some("tt0099871"), tmdb_id: None, quality: Some(&q),
            media_info: Some(&info), edition: "", release_group: Some("HiDt"), original_name: "",
        });
        assert_eq!(name, "Jacobs Ladder (1990) {imdb-tt0099871} [Bluray-1080p][DV HDR10][EAC3 5.1][x265]-HiDt");
        assert_eq!(movie_folder_name(&cfg, "Jacob's Ladder", 1990, None, None), "Jacob's Ladder (1990)");
    }

    #[test]
    fn episode_names() {
        let cfg = NamingConfig::default();
        let q = QualityModel::new(Quality::Webdl1080p);
        let name = episode_file_name(&cfg, &EpisodeNameInput {
            series_title: "Westworld", series_year: 2016, season: 1, episodes: &[1], episode_titles: &["The Original"],
            air_date: None, quality: Some(&q), media_info: None, release_group: None, original_name: "",
        });
        assert_eq!(name, "Westworld - S01E01 - The Original WEBDL-1080p");
        let name = episode_file_name(&cfg, &EpisodeNameInput {
            series_title: "Show: Name", series_year: 0, season: 2, episodes: &[3, 4], episode_titles: &["Start (1)", "Start (2)"],
            air_date: None, quality: Some(&q), media_info: None, release_group: None, original_name: "",
        });
        assert_eq!(name, "Show - Name - S02E03-04 - Start WEBDL-1080p");
        assert_eq!(season_folder_name(&cfg, 3), "Season 3");
        assert_eq!(season_folder_name(&cfg, 0), "Specials");
    }
}
