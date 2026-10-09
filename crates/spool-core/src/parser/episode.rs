//! Episode release-title parsing. Ported from Sonarr's `Parser.cs` (GPLv3).
//!
//! The pattern table in `sonarr_regexes.rs` is generated from upstream. Named groups carry a
//! `__N` suffix because .NET keeps every capture of a repeated group and Rust keeps one; see
//! `tools/gen_sonarr_regexes.py`.

use super::common::*;
use super::movie::reverse_title;
use crate::quality::{parse_quality, Flavor, QualityModel};
use crate::sonarr_regexes::SONARR_TITLE_REGEXES;
use fancy_regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Debug, Default, Serialize)]
pub struct ParsedEpisodeInfo {
    pub series_title: String,
    pub series_title_without_year: String,
    pub series_year: Option<u32>,
    pub all_titles: Vec<String>,
    pub season_numbers: Vec<u32>,
    pub episode_numbers: Vec<u32>,
    pub absolute_episode_numbers: Vec<u32>,
    pub special_absolute_episode_numbers: Vec<f64>,
    pub air_date: Option<String>,
    pub daily_part: Option<u32>,
    pub full_season: bool,
    pub is_partial_season: bool,
    pub season_part: u32,
    pub is_season_extra: bool,
    pub special: bool,
    pub is_split_episode: bool,
    pub is_mini_series: bool,
    pub quality: Option<QualityModel>,
    pub release_group: Option<String>,
    pub languages: Vec<String>,
    pub release_tokens: String,
    pub release_title: String,
}

impl ParsedEpisodeInfo {
    pub fn season_number(&self) -> u32 {
        self.season_numbers.first().copied().unwrap_or(0)
    }
    pub fn is_multi_season(&self) -> bool {
        self.season_numbers.len() > 1
    }
    pub fn is_daily(&self) -> bool {
        self.air_date.is_some()
    }
    pub fn is_absolute(&self) -> bool {
        !self.absolute_episode_numbers.is_empty() && self.episode_numbers.is_empty()
    }
}

struct Pattern {
    rx: Regex,
    /// (base group name, capture index), in pattern order.
    groups: Vec<(String, usize)>,
}

static PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    SONARR_TITLE_REGEXES
        .iter()
        .map(|(_, p)| {
            let rx = re(p);
            let groups = rx
                .capture_names()
                .enumerate()
                .filter_map(|(i, n)| n.and_then(|n| n.rsplit_once("__").map(|(base, _)| (base.to_string(), i))))
                .collect();
            Pattern { rx, groups }
        })
        .collect()
});

impl Pattern {
    /// Every capture of a logical group, ordered by position in the text.
    fn all<'a>(&self, c: &Captures<'a>, base: &str) -> Vec<Match<'a>> {
        let mut v: Vec<Match<'a>> =
            self.groups.iter().filter(|(b, _)| b == base).filter_map(|(_, i)| c.get(*i)).collect();
        v.sort_by_key(|m| m.start());
        v
    }
    /// What `.Groups[name]` returns in .NET: the capture of the last group that participated.
    fn last<'a>(&self, c: &Captures<'a>, base: &str) -> Option<Match<'a>> {
        self.groups.iter().rev().filter(|(b, _)| b == base).find_map(|(_, i)| c.get(*i))
    }
    fn value<'a>(&self, c: &Captures<'a>, base: &str) -> &'a str {
        self.last(c, base).map(|m| m.as_str()).unwrap_or("")
    }
}

static SEASON_FOLDERS: LazyLock<Regex> = LazyLock::new(|| re(r"^(Season[ ._-]*\d+|Specials)$"));
static REVERSED_TITLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:^|[-._ ])(p027|p0801|\d{2,3}E-?\d{2}S)[-._ ]"));
static SIMPLE_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)(?:(480|540|576|720|1080|1440|2160)[ip]|[xh][\W_]?26[456]|DD\W?5\W1|[<>?*]|848x480|1280x720|1920x1080|3840x2160|4096x2160|(?<![a-f0-9])(8|10)[ -]?(b(?![a-z0-9])|bit))\s*?")
});
static SIX_DIGIT_AIR_DATE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)(?<=[_.-])(?<airdate>(?<!\d)(?<airyear>[1-9]\d{1})(?<airmonth>[0-1][0-9])(?<airday>[0-3][0-9]))(?=[_.-])")
});
static YEAR_IN_TITLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^(?<title>.+?)[-_. ]+?[\(\[]?(?<year>\d{4})[\]\)]?"));
static TITLE_COMPONENTS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^(?:(?<a1>.+?) \((?<b1>.+?)\)|(?<a2>.+?) \| (?<b2>.+?)|(?<a3>.+?) AKA (?<b3>.+?)|(?<a4>.+?) / (?<b4>.+?))$")
});
static SEASON_FOLDER: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^(?:S|Season|Saison|Series|Stagione|Sezon)[-_. ]*(?<season>(?<!\d+)\d{1,4}(?!\d+))(?:[_. ]+(?!\d+)|$)")
});
static SIMPLE_EPISODE_NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^[ex]?(?<episode>(?<!\d+)\d{1,3}(?!\d+))(?:[ex-](?<episode2>(?<!\d+)\d{1,3}(?!\d+)))?(?:[_. ](?!\d+)(?<remaining>.+)|$)")
});
static PATH_PRE_SUB: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)(?<separator>^|[-_. ])S(?<season>(?<!\d+)\d{1,2}(?!\d+)) (?<episode>(?<!\d+)\d{1,2}(?!\d+))(?!\d+)")
});

const NUMBERS: [&str; 10] = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine"];
const SHORT_MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn parse_number(v: &str) -> Option<u32> {
    let norm: String = v.nfkc().collect();
    norm.parse().ok().or_else(|| NUMBERS.iter().position(|n| *n == v.to_lowercase()).map(|i| i as u32))
}

fn parse_decimal(v: &str) -> Option<f64> {
    let norm: String = v.nfkc().collect();
    norm.parse().ok()
}

struct InvalidDate;

fn title_info(info: &mut ParsedEpisodeInfo, title_captures: &[String]) {
    let title = info.series_title.clone();
    match crate::parser::common::caps(&YEAR_IN_TITLE, &title) {
        Ok(Some(c)) => {
            info.series_title_without_year = group(&c, "title").unwrap_or("").to_string();
            info.series_year = group(&c, "year").and_then(|y| y.parse().ok());
        }
        _ => info.series_title_without_year = title.clone(),
    }
    if let Ok(Some(c)) = crate::parser::common::caps(&TITLE_COMPONENTS, &info.series_title_without_year) {
        for (a, b) in [("a1", "b1"), ("a2", "b2"), ("a3", "b3"), ("a4", "b4")] {
            if let (Some(x), Some(y)) = (group(&c, a), group(&c, b)) {
                info.all_titles = vec![x.to_string(), y.to_string()];
            }
        }
    } else if title_captures.len() > 1 {
        info.all_titles = title_captures.iter().map(|t| t.replace(['.', '_'], " ")).collect();
    }
}

fn from_match(p: &Pattern, c: &Captures, release_title: &str) -> Result<Option<ParsedEpisodeInfo>, InvalidDate> {
    let series_name = p.value(c, "title").replace(['.', '_'], " ");
    let series_name = replace_all(&REQUEST_INFO, &series_name, "").trim_matches(' ').to_string();
    let air_year: i32 = p.value(c, "airyear").parse().unwrap_or(0);
    let mut last_index = p.last(c, "title").map(|m| m.end()).unwrap_or(0);
    let mut info = ParsedEpisodeInfo { release_title: release_title.to_string(), ..Default::default() };

    if air_year < 1900 {
        let episodes = p.all(c, "episode");
        let absolutes = p.all(c, "absoluteepisode");

        if let (Some(f), Some(l)) = (episodes.first(), episodes.last()) {
            let (Some(first), Some(last)) = (parse_number(f.as_str()), parse_number(l.as_str())) else {
                return Ok(None);
            };
            if first > last {
                return Ok(None);
            }
            info.episode_numbers = (first..=last).collect();
            last_index = last_index.max(l.end());
            info.special = p.last(c, "special").is_some();
            info.is_split_episode = p.last(c, "splitepisode").is_some();
        }

        if let (Some(f), Some(l)) = (absolutes.first(), absolutes.last()) {
            let (Some(first), Some(last)) = (parse_decimal(f.as_str()), parse_decimal(l.as_str())) else {
                return Ok(None);
            };
            if first > last {
                return Ok(None);
            }
            if first.fract() != 0.0 || last.fract() != 0.0 {
                if absolutes.len() != 1 {
                    return Ok(None);
                }
                info.special_absolute_episode_numbers = vec![first];
                info.special = true;
                last_index = last_index.max(f.end());
            } else {
                info.absolute_episode_numbers = (first as u32..=last as u32).collect();
                if p.last(c, "special").is_some() {
                    info.special = true;
                }
                last_index = last_index.max(l.end());
            }
        }

        if episodes.is_empty() && absolutes.is_empty() {
            if !p.value(c, "extras").trim().is_empty() {
                info.is_season_extra = true;
            }
            let season_part = p.value(c, "seasonpart");
            if !season_part.trim().is_empty() {
                info.season_part = season_part.parse().unwrap_or(0);
                info.is_partial_season = true;
            } else if p.last(c, "special").is_some() {
                info.special = true;
            } else {
                info.full_season = true;
            }
        }

        if episodes.len() == 2 {
            if let Some(count) = p.last(c, "episodecount") {
                if episodes[1].as_str() == count.as_str() {
                    info.episode_numbers.clear();
                    info.full_season = true;
                }
            }
        }

        let mut seasons: Vec<u32> = Vec::new();
        for s in p.all(c, "season") {
            if let Ok(n) = s.as_str().parse::<u32>() {
                seasons.push(n);
                last_index = last_index.max(s.end());
            }
        }
        // A repeated season group keeps only its last capture here, so recover the ones in between.
        let season_matches = p.all(c, "season");
        if season_matches.len() > 2 {
            let span = &c.get(0).map(|m| m.as_str()).unwrap_or("");
            let base = c.get(0).map(|m| m.start()).unwrap_or(0);
            let (from, to) = (season_matches[0].start() - base, season_matches[season_matches.len() - 1].end() - base);
            if let Some(between) = span.get(from..to) {
                for tok in between.split(|ch: char| !ch.is_ascii_digit()).filter(|t| !t.is_empty() && t.len() <= 2) {
                    if let Ok(n) = tok.parse::<u32>() {
                        seasons.push(n);
                    }
                }
            }
        }
        seasons.sort_unstable();
        seasons.dedup();
        match seasons.len() {
            0 => {
                if info.absolute_episode_numbers.is_empty() && !info.episode_numbers.is_empty() {
                    info.season_numbers = vec![1];
                    info.is_mini_series = true;
                }
            }
            2 => info.season_numbers = (seasons[0]..=seasons[1]).collect(),
            _ => info.season_numbers = seasons,
        }
    } else {
        let num = |name: &str| p.value(c, name).parse::<u32>().unwrap_or(0);
        let (mut month, mut day);
        if p.last(c, "ambiguousairmonth").is_some() && p.last(c, "ambiguousairday").is_some() {
            month = num("ambiguousairmonth");
            day = num("ambiguousairday");
            if day <= 12 && month <= 12 {
                return Err(InvalidDate);
            }
        } else if let Some(short) = p.last(c, "shortairmonth") {
            let s = short.as_str().to_lowercase();
            month = SHORT_MONTHS.iter().position(|m| *m == s).map(|i| i as u32 + 1).ok_or(InvalidDate)?;
            day = num("airday");
        } else {
            month = num("airmonth");
            day = num("airday");
        }
        if month > 12 {
            std::mem::swap(&mut month, &mut day);
        }
        let date = chrono::NaiveDate::from_ymd_opt(air_year, month, day).ok_or(InvalidDate)?;
        let today = chrono::Local::now().date_naive();
        if date > today + chrono::Duration::days(14) {
            return Err(InvalidDate);
        }
        if date < chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap() && p.value(c, "titleyear").trim().is_empty() {
            return Err(InvalidDate);
        }
        for g in ["airyear", "airmonth", "airday"] {
            if let Some(m) = p.last(c, g) {
                last_index = last_index.max(m.end());
            }
        }
        info.air_date = Some(date.format("%Y-%m-%d").to_string());
        info.daily_part = p.last(c, "part").and_then(|m| m.as_str().parse().ok());
    }

    info.release_tokens = if last_index < release_title.len() && release_title.is_char_boundary(last_index) {
        release_title[last_index..].to_string()
    } else {
        release_title.to_string()
    };
    info.series_title = series_name;
    let title_captures: Vec<String> = p.all(c, "title").iter().map(|m| m.as_str().to_string()).collect();
    title_info(&mut info, &title_captures);
    Ok(Some(info))
}

/// Parse a release or file name as television.
pub fn parse_episode_title(title: &str) -> Option<ParsedEpisodeInfo> {
    if !validate_before_parsing(title, true) {
        return None;
    }
    if is_match(&SEASON_FOLDERS, &remove_file_extension(title)) {
        return None;
    }
    let title = reverse_title(title, &REVERSED_TITLE);
    let release_title = remove_file_extension(&title).replace('【', "[").replace('】', "]");
    let release_title = pre_substitute(&release_title);

    let simple = replace_all(&SIMPLE_TITLE, &release_title, "");
    let simple = replace_all(&WEBSITE_PREFIX, &simple, "");
    let simple = replace_all(&WEBSITE_POSTFIX, &simple, "");
    let simple = replace_all(&CLEAN_TORRENT_SUFFIX, &simple, "");
    let mut simple = clean_quality_brackets(&simple, Flavor::Tv);

    if let Ok(Some(c)) = crate::parser::common::caps(&SIX_DIGIT_AIR_DATE, &simple) {
        let (y, m, d) = (group(&c, "airyear").unwrap_or(""), group(&c, "airmonth").unwrap_or(""), group(&c, "airday").unwrap_or(""));
        if m != "00" || d != "00" {
            let fixed = format!("20{y}.{m}.{d}");
            let airdate = group(&c, "airdate").unwrap_or("").to_string();
            simple = simple.replace(&airdate, &fixed);
        }
    }

    for p in PATTERNS.iter() {
        let Ok(Some(c)) = crate::parser::common::caps(&p.rx, &simple) else { continue };
        match from_match(p, &c, &release_title) {
            Err(InvalidDate) => break,
            Ok(None) => continue,
            Ok(Some(mut info)) => {
                if info.full_season && info.release_tokens.to_lowercase().contains("special") {
                    info.full_season = false;
                    info.special = true;
                }
                info.languages = parse_languages(&info.release_tokens);
                info.quality = Some(parse_quality(&title, Flavor::Tv));
                info.release_group = parse_release_group(&release_title, Flavor::Tv);
                let sub = p.value(&c, "subgroup");
                if !sub.trim().is_empty() {
                    info.release_group = Some(sub.to_string());
                }
                return Some(info);
            }
        }
    }
    None
}

/// Parse a file path as television, using the season folder when the file name alone is not enough.
pub fn parse_episode_path(path: &str) -> Option<ParsedEpisodeInfo> {
    let (dir, file) = match path.rsplit_once('/') {
        Some((d, f)) => (d.rsplit('/').next().unwrap_or(""), f),
        None => ("", path),
    };
    let file_name = replace_all(&PATH_PRE_SUB, file, "${separator}S${season}E${episode}");
    let ext = file_name.rfind('.').map(|i| &file_name[i..]).unwrap_or("");
    let stem = file_name.rfind('.').map(|i| &file_name[..i]).unwrap_or(&file_name);

    let mut result = parse_episode_title(&file_name);

    if !dir.is_empty() {
        if let Ok(Some(ep)) = crate::parser::common::caps(&SIMPLE_EPISODE_NUMBER, &file_name) {
            let weak = match &result {
                None => true,
                Some(r) => r.is_mini_series || !r.absolute_episode_numbers.is_empty(),
            };
            if weak {
                if let Ok(Some(sm)) = crate::parser::common::caps(&SEASON_FOLDER, dir) {
                    let first = group(&ep, "episode").and_then(parse_number).unwrap_or(0);
                    let last = group(&ep, "episode2").and_then(parse_number).unwrap_or(first);
                    let mut path_title = format!("S{}E{:02}", group(&sm, "season").unwrap_or("0"), first);
                    if first != last {
                        path_title.push_str(&format!("-E{last:02}"));
                    }
                    if let Some(rem) = group(&ep, "remaining") {
                        path_title.push(' ');
                        path_title.push_str(rem);
                    }
                    return parse_episode_title(&path_title);
                }
            }
        }
    }

    if result.is_none() {
        if let Ok(number) = stem.parse::<u32>() {
            result = parse_episode_title(dir);
            match &mut result {
                Some(r) if r.absolute_episode_numbers.contains(&number) => r.absolute_episode_numbers = vec![number],
                Some(r) if r.episode_numbers.contains(&number) => r.episode_numbers = vec![number],
                _ => result = None,
            }
        }
    }
    result
        .or_else(|| parse_episode_title(&format!("{dir} {file_name}")))
        .or_else(|| parse_episode_title(&format!("{dir}{ext}")))
}
