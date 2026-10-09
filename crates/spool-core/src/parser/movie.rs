//! Movie release-title parsing. Ported from Radarr's `Parser.cs` (GPLv3).

use super::common::*;
use crate::quality::{parse_quality, Flavor, QualityModel};
use fancy_regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

#[derive(Clone, Debug, Serialize)]
pub struct ParsedMovieInfo {
    /// Primary title first, then any AKA alternatives.
    pub titles: Vec<String>,
    pub year: u32,
    pub edition: String,
    pub release_group: Option<String>,
    pub quality: QualityModel,
    pub languages: Vec<String>,
    pub hardcoded_subs: Option<String>,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<u32>,
    pub release_title: String,
}

impl ParsedMovieInfo {
    pub fn title(&self) -> &str {
        self.titles.first().map(String::as_str).unwrap_or("")
    }
}

const EDITION: &str = r"\(?\b(?<edition>(((Recut.|Extended.|Ultimate.)?(Director.?s|Collector.?s|Theatrical|Ultimate|Extended|Despecialized|(Special|Rouge|Final|Assembly|Imperial|Diamond|Signature|Hunter|Rekall)(?=(.(Cut|Edition|Version)))|\d{2,3}(th)?.Anniversary)(?:.(Cut|Edition|Version))?(.(Extended|Uncensored|Remastered|Unrated|Uncut|Open.?Matte|IMAX|Fan.?Edit))?|((Uncensored|Remastered|Unrated|Uncut|Open?.Matte|IMAX|Fan.?Edit|Restored|((2|3|4)in1))))))\b\)?";

static REPORT_EDITION: LazyLock<Regex> = LazyLock::new(|| re(&format!("(?i)^.+?{EDITION}")));
static HARDCODED_SUBS: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b((?<hcsub>(\w+(?<!SOFT|MULTI|HORRIBLE)SUBS?))|(?<hc>(HC|SUBBED)))\b"));

static TITLE_REGEXES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Anime [Subgroup] with year explicitly in parentheses, e.g. [Sub] Title 1993 (1993) [VHS]
        re(r"(?i)^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+?)\s*\((?<year>(1(8|9)|20)\d{2})\).*?(?<hash>\[\w{8}\])?(?:$|\.)"),
        // Anime [Subgroup] and Year
        re(r"(?i)^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+?)?(?:(?:[-_\W](?<![)\[!]))*(?<year>(1(8|9)|20)\d{2}(?!p|i|x|\d+|\]|\W\d+)))+.*?(?<hash>\[\w{8}\])?(?:$|\.)"),
        // Anime [Subgroup] no year, versioned title, hash
        re(r"(?i)^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+?)((v)(?:\d{1,2})(?:([-_. ])))(\[.*)?(?:[\[(][^])])?.*?(?<hash>\[\w{8}\])(?:$|\.)"),
        // Anime [Subgroup] no year, info in double sets of brackets, hash
        re(r"(?i)^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+?)(\[.*).*?(?<hash>\[\w{8}\])(?:$|\.)"),
        // Anime [Subgroup] no year, info in parentheses or brackets, hash
        re(r"(?i)^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+)(?:[\[(][^])]).*?(?<hash>\[\w{8}\])(?:$|\.)"),
        // Some german or french tracker formats (missing year, ...)
        re(&format!(r"(?i)^(?<title>(?![(\[]).+?)((\W|_))({EDITION}.{{1,3}})?(?:(?<!(19|20)\d{{2}}.*?)(?<!(?:Good|The)[_ .-])(German|TrueFrench))(.+?)(?=((19|20)\d{{2}}|$))(?<year>(19|20)\d{{2}}(?!p|i|\d+|\]|\W\d+))?(\W+|_|$)(?!\\)")),
        // Special, Despecialized, etc. Edition Movies, e.g: Mission.Impossible.3.Special.Edition.2011
        re(&format!(r"(?i)^(?<title>(?![(\[]).+?)?(?:(?:[-_\W](?<![)\[!]))*{EDITION}.{{1,3}}(?<year>(1(8|9)|20)\d{{2}}(?!p|i|\d+|\]|\W\d+)))+(\W+|_|$)(?!\\)")),
        // Normal movie format, e.g: Mission.Impossible.3.2011
        re(r"(?i)^(?<title>(?![(\[]).+?)?(?:(?:[-_\W](?<![)\[!]))*(?<year>(1(8|9)|20)\d{2}(?!p|i|(1(8|9)|20)\d{2}|\]|\W(1(8|9)|20)\d{2})))+(\W+|_|$)(?!\\)"),
        // PassThePopcorn Torrent names: Star.Wars[PassThePopcorn]
        re(r"(?i)^(?<title>.+?)?(?:(?:[-_\W](?<![()\[!]))*(?<year>(\[\w *\])))+(\W+|_|$)(?!\\)"),
        // That did not work? Maybe some tool uses [] for years. Who would do that?
        re(r"(?i)^(?<title>(?![(\[]).+?)?(?:(?:[-_\W](?<![)!]))*(?<year>(1(8|9)|20)\d{2}(?!p|i|\d+|\W\d+)))+(\W+|_|$)(?!\\)"),
        // As a last resort for movies that have ( or [ in their title.
        re(r"(?i)^(?<title>.+?)?(?:(?:[-_\W](?<![)\[!]))*(?<year>(1(8|9)|20)\d{2}(?!p|i|\d+|\]|\W\d+)))+(\W+|_|$)(?!\\)"),
    ]
});

static FOLDER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:(?:[-_\W](?<![)!]))*(?<year>(19|20)\d{2}(?!p|i|\d+|\W\d+)))+(\W+|_|$)(?<title>.+?)?$"));

static REVERSED_TITLE: LazyLock<Regex> = LazyLock::new(|| re(r"(?:^|[-._ ])(p027|p0801)[-._ ]"));
static ALTERNATIVE_TITLE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)[ ]+(?:AKA|\/)[ ]+"));
static BRACKETED_ALT_TITLE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(.*) \([ ]*AKA[ ]+(.*)\)"));
static NORMALIZE_ALT_TITLE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)[ ]+(?:A\.K\.A\.)[ ]+"));
static REPORT_IMDB: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(?<imdbid>tt\d{7,8})"));
static REPORT_TMDB: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)tmdb(id)?-(?<tmdbid>\d+)"));
static SIMPLE_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)(?:(480|540|576|720|1080|2160)[ip]|[xh][\W_]?26[45]|DD\W?5\W1|[<>?*]|848x480|1280x720|1920x1080|3840x2160|4096x2160|(8|10)b(it)?|10-bit)\s*?(?![a-b0-9])")
});
static SIMPLE_RELEASE_TITLE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\s*(?:[<>?*|])"));

fn reverse_if_needed(title: &str, rx: &Regex) -> String {
    if is_match(rx, title) {
        let bare = remove_file_extension(title);
        let rev: String = bare.chars().rev().collect();
        format!("{rev}{}", &title[bare.len()..])
    } else {
        title.to_string()
    }
}

pub(crate) fn reverse_title(title: &str, rx: &Regex) -> String {
    reverse_if_needed(title, rx)
}

pub fn parse_edition(title: &str) -> String {
    match crate::parser::common::caps(&REPORT_EDITION, title) {
        Ok(Some(c)) => group(&c, "edition").map(|e| e.replace('.', " ")).unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn parse_hardcoded_subs(title: &str) -> Option<String> {
    let c = crate::parser::common::caps_iter(&HARDCODED_SUBS, title).filter_map(|c| c.ok()).last()?;
    if let Some(s) = group(&c, "hcsub") {
        Some(s.to_string())
    } else if c.name("hc").is_some() {
        Some("Generic Hardcoded Subs".to_string())
    } else {
        None
    }
}

fn movie_name_from_title_group(raw: &str) -> String {
    let name = raw.replace('_', " ");
    let name = replace_all(&NORMALIZE_ALT_TITLE, &name, " AKA ");
    let name = replace_all(&REQUEST_INFO, &name, "");
    let name = name.trim_matches(' ');
    let parts: Vec<&str> = name.split('.').collect();
    let mut out = String::new();
    let mut previous_acronym = false;
    for (n, part) in parts.iter().enumerate() {
        let next = parts.get(n + 1).copied().unwrap_or("");
        let lower = part.to_lowercase();
        let is_int = |s: &str| s.parse::<i64>().is_ok();
        if part.chars().count() == 1
            && lower != "a"
            && !is_int(part)
            && (previous_acronym || n < parts.len() - 1)
            && (previous_acronym || next.chars().count() != 1 || !is_int(next))
        {
            out.push_str(part);
            out.push('.');
            previous_acronym = true;
        } else if lower == "a" && (previous_acronym || next.chars().count() == 1) {
            out.push_str(part);
            out.push('.');
            previous_acronym = true;
        } else if lower == "dr" {
            out.push_str(part);
            out.push('.');
            previous_acronym = true;
        } else {
            if previous_acronym {
                out.push(' ');
                previous_acronym = false;
            }
            out.push_str(part);
            out.push(' ');
        }
    }
    out.trim_matches(' ').to_string()
}

/// Parse a release or file name as a movie. `is_dir` also tries the year-first folder form.
pub fn parse_movie_title(title: &str, is_dir: bool) -> Option<ParsedMovieInfo> {
    if !validate_before_parsing(title, false) {
        return None;
    }
    let original = title;
    let title = reverse_if_needed(title, &REVERSED_TITLE);
    let release_title = remove_file_extension(&title);
    let release_title = release_title.trim_matches(|c| c == '-' || c == '_').replace('【', "[").replace('】', "]");

    let simple = replace_all(&SIMPLE_TITLE, &release_title, "");
    let simple = replace_all(&WEBSITE_PREFIX, &simple, "");
    let simple = replace_all(&WEBSITE_POSTFIX, &simple, "");
    let simple = replace_all(&CLEAN_TORRENT_SUFFIX, &simple, "");
    let simple = clean_quality_brackets(&simple, Flavor::Movie);

    let mut regexes: Vec<&Regex> = TITLE_REGEXES.iter().collect();
    if is_dir {
        regexes.push(&FOLDER_REGEX);
    }

    for rx in regexes {
        let Ok(Some(c)) = crate::parser::common::caps(&rx, &simple) else { continue };
        let Some(title_m) = c.name("title") else { continue };
        if title_m.as_str() == "(" {
            continue;
        }
        let name = movie_name_from_title_group(title_m.as_str());
        let year = group(&c, "year").and_then(|y| y.parse::<u32>().ok()).unwrap_or(0);
        let mut edition = group(&c, "edition").map(|e| e.replace('.', " ")).unwrap_or_default();

        let mut titles = vec![name.clone()];
        let unbracketed = replace_all(&BRACKETED_ALT_TITLE, &name, "$1 AKA $2");
        for alt in ALTERNATIVE_TITLE.split(&unbracketed).filter_map(|s| s.ok()) {
            if !alt.trim().is_empty() && alt != name {
                titles.push(alt.to_string());
            }
        }

        // Upstream blanks the title out of the release name before looking for group and language,
        // so that words in the title are not mistaken for either.
        let mut simple_release = replace_all(&SIMPLE_RELEASE_TITLE, &release_title, "");
        let (start, end) = (title_m.start(), title_m.end());
        if !title_m.as_str().trim().is_empty()
            && end <= simple_release.len()
            && simple_release.is_char_boundary(start)
            && simple_release.is_char_boundary(end)
        {
            let placeholder = if title_m.as_str().contains('.') { "A.Movie" } else { "A Movie" };
            simple_release.replace_range(start..end, placeholder);
        }

        let mut release_group = parse_release_group(&simple_release, Flavor::Movie);
        if let Some(sub) = group(&c, "subgroup") {
            if !sub.trim().is_empty() {
                release_group = Some(sub.to_string());
            }
        }
        let lang_source = match &release_group {
            Some(g) if !g.trim().is_empty() => simple_release.replace(g.as_str(), "RlsGrp"),
            _ => simple_release.clone(),
        };
        if edition.trim().is_empty() {
            edition = parse_edition(&simple_release);
        }
        let imdb_id = caps(&REPORT_IMDB, &simple_release)
            .ok()
            .flatten()
            .and_then(|c| group(&c, "imdbid").map(str::to_string))
            .filter(|s| s.len() == 9 || s.len() == 10);
        let tmdb_id = caps(&REPORT_TMDB, &simple_release)
            .ok()
            .flatten()
            .and_then(|c| group(&c, "tmdbid").and_then(|s| s.parse().ok()));

        return Some(ParsedMovieInfo {
            titles,
            year,
            edition,
            release_group,
            quality: parse_quality(&title, Flavor::Movie),
            languages: parse_languages(&lang_source),
            hardcoded_subs: parse_hardcoded_subs(&title),
            imdb_id,
            tmdb_id,
            release_title: original.to_string(),
        });
    }
    None
}

/// Parse a file path as a movie, falling back to the containing folder name.
pub fn parse_movie_path(path: &str) -> Option<ParsedMovieInfo> {
    let (dir, file) = match path.rsplit_once('/') {
        Some((d, f)) => (d.rsplit('/').next().unwrap_or(""), f),
        None => ("", path),
    };
    let ext = file.rfind('.').map(|i| &file[i..]).unwrap_or("");
    parse_movie_title(file, true)
        .or_else(|| parse_movie_title(&format!("{dir} {file}"), false))
        .or_else(|| parse_movie_title(&format!("{dir}{ext}"), false))
}
