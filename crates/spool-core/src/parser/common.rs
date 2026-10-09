//! Helpers shared by the movie and episode parsers. Ported from Radarr/Sonarr (GPLv3).

use fancy_regex::Regex;
pub(crate) type Captures<'a> = fancy_regex::Captures<'a, str>;
pub(crate) type Match<'a> = fancy_regex::Match<'a>;
use std::sync::LazyLock;

pub(crate) fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|e| panic!("bad regex {p}: {e}"))
}

pub(crate) fn caps<'a>(r: &Regex, s: &'a str) -> fancy_regex::Result<Option<Captures<'a>>> {
    r.captures(s)
}

pub(crate) fn caps_iter<'r, 'a>(r: &'r Regex, s: &'a str) -> fancy_regex::CaptureMatches<'r, 'a, str> {
    r.captures_iter(s)
}

pub(crate) fn find<'a>(r: &Regex, s: &'a str) -> fancy_regex::Result<Option<Match<'a>>> {
    r.find(s)
}

pub(crate) fn find_iter<'r, 'a>(r: &'r Regex, s: &'a str) -> fancy_regex::Matches<'r, 'a, str> {
    r.find_iter(s)
}

pub(crate) fn is_match(r: &Regex, s: &str) -> bool {
    r.is_match(s).unwrap_or(false)
}

pub(crate) fn replace_all(r: &Regex, s: &str, rep: &str) -> String {
    r.replace_all(s, rep).into_owned()
}

pub(crate) fn group<'a>(c: &Captures<'a>, name: &str) -> Option<&'a str> {
    c.name(name).map(|m| m.as_str())
}

pub const MEDIA_EXTENSIONS: &[&str] = &[
    ".webm", ".m4v", ".3gp", ".nsv", ".ty", ".strm", ".rm", ".rmvb", ".m3u", ".ifo", ".mov", ".qt", ".divx",
    ".xvid", ".bivx", ".nrg", ".pva", ".wmv", ".asf", ".asx", ".ogm", ".ogv", ".m2v", ".avi", ".bin", ".dat",
    ".dvr-ms", ".mpg", ".mpeg", ".mp4", ".avc", ".vp3", ".svq3", ".nuv", ".viv", ".dv", ".fli", ".flv", ".wpl",
    ".img", ".iso", ".vob", ".mkv", ".mk3d", ".ts", ".wtv", ".m2ts",
];

/// True for extensions Spool treats as playable video.
pub fn is_video_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    match lower.rfind('.') {
        Some(i) => {
            let ext = &lower[i..];
            MEDIA_EXTENSIONS.contains(&ext) && !matches!(ext, ".m3u" | ".strm" | ".wpl" | ".asx" | ".nrg" | ".bin" | ".dat" | ".ifo")
        }
        None => false,
    }
}

static FILE_EXTENSION: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\.[a-z0-9]{2,4}$"));

pub fn remove_file_extension(title: &str) -> String {
    if let Ok(Some(m)) = crate::parser::common::find(&FILE_EXTENSION, title) {
        let ext = m.as_str().to_ascii_lowercase();
        if MEDIA_EXTENSIONS.contains(&ext.as_str()) || ext == ".par2" || ext == ".nzb" {
            return title[..m.start()].to_string();
        }
    }
    title.to_string()
}

pub(crate) static WEBSITE_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^(?:(?:\[|\()\s*)?(?:www\.)?[-a-z0-9-]{1,256}\.(?<!Naruto-Kun\.)(?:[a-z]{2,6}\.[a-z]{2,6}|xn--[a-z0-9-]{4,}|[a-z]{2,})\b(?:\s*(?:\]|\))|[ -]{2,})[ -]*")
});
pub(crate) static WEBSITE_POSTFIX: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)(?:\[\s*)?(?:www\.)?[-a-z0-9-]{1,256}\.(?:xn--[a-z0-9-]{4,}|[a-z]{2,6})\b(?:\s*\])$"));
pub(crate) static CLEAN_TORRENT_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\[(?:ettv|rartv|rarbg|cttv|publichd)\]$"));
pub(crate) static CLEAN_QUALITY_BRACKETS: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\[[a-z0-9 ._-]+\]$"));
pub(crate) static REQUEST_INFO: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:\[.+?\])+"));

static REJECT_HASHED: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^[0-9a-zA-Z]{32}",
        r"^[a-z0-9]{24}$",
        r"^[A-Z]{11}\d{3}$",
        r"^[a-z]{12}\d{3}$",
        r"^Backup_\d{5,}S\d{2}-\d{2}$",
        r"^123$",
        r"(?i)^abc$",
        r"(?i)^abc[-_. ]xyz",
        r"(?i)^b00bs$",
    ]
    .iter()
    .map(|p| re(p))
    .collect()
});

static REJECT_HASHED_TV: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [r"^\d{6}_\d{2}$", r"^[0-9a-zA-Z]{30}", r"^[0-9a-zA-Z]{26}", r"^[0-9a-zA-Z]{39}", r"^[0-9a-zA-Z]{24}"]
        .iter()
        .map(|p| re(p))
        .collect()
});

pub(crate) fn validate_before_parsing(title: &str, tv: bool) -> bool {
    let lower = title.to_lowercase();
    if lower.contains("password") && lower.contains("yenc") {
        return false;
    }
    if !title.chars().any(|c| c.is_alphanumeric()) {
        return false;
    }
    let bare = remove_file_extension(title);
    if REJECT_HASHED.iter().any(|r| is_match(r, &bare)) {
        return false;
    }
    if tv && REJECT_HASHED_TV.iter().any(|r| is_match(r, &bare)) {
        return false;
    }
    true
}

/// Strip trailing `[quality]` brackets only when they really describe quality.
pub(crate) fn clean_quality_brackets(s: &str, flavor: crate::quality::Flavor) -> String {
    match crate::parser::common::find(&CLEAN_QUALITY_BRACKETS, s) {
        Ok(Some(m)) => {
            if crate::quality::parse_quality_name(m.as_str(), flavor).quality != crate::quality::Quality::Unknown {
                s[..m.start()].to_string()
            } else {
                s.to_string()
            }
        }
        _ => s.to_string(),
    }
}

// ---------------------------------------------------------------- release group

struct GroupRegexes {
    group: Regex,
    exact: Regex,
    exception: Regex,
    clean: Regex,
    exact_first: bool,
}

// Upstream uses one look-behind with an optional back-reference to part2. Checking after the first
// part and again after the optional second part is equivalent and needs no back-reference.
static MOVIE_GROUP: LazyLock<GroupRegexes> = LazyLock::new(|| {
    const NOT: &str = r"(?<!WEB-(?:DL|Rip)|Blu-Ray|480p|576p|720p|1080p|2160p|DTS-HD|DTS-X|DTS-MA|DTS-ES|-ES|-EN|-CAT|-ENG|-JAP|-GER|-FRA|-FRE|-ITA|-HDRip|\d{1,2}-bit|[ ._]\d{4}-\d{2}|-\d{2}|tmdb(?:id)?-\d+|tt\d{7,8})";
    GroupRegexes {
        group: re(&format!(
            r"(?i)-(?<releasegroup>[a-z0-9]+{NOT}(?<part2>-[a-z0-9]+)?{NOT}(?!.+?(?:480p|576p|720p|1080p|2160p)))(?:\b|[-._ ]|$)|[-._ ]\[(?<releasegroup2>[a-z0-9]+)\]$"
        )),
        exact: re(r"(?i)\b(?<releasegroup>KRaLiMaRKo|E\.N\.D|D\-Z0N3|Koten_Gars|BluDragon|ZØNEHD|HQMUX|VARYG|YIFY|YTS(.(MX|LT|AG))?|TMd|Eml HDTeam|LMain|DarQ|BEN THE MEN|TAoE|QxR|126811)\b"),
        exception: re(r"(?i)(?<=[._ \[])(?<releasegroup>(Silence|afm72|Panda|Ghost|MONOLITH|Tigole|Joy|ImE|UTR|t3nzin|Anime Time|Project Angel|Hakata Ramen|HONE|GiLG|Vyndros|SEV|Garshasp|Kappa|Natty|RCVR|SAMPA|YOGI|r00t|EDGE2020|RZeroX|FreetheFish|Anna|Bandi|Qman|theincognito|HDO|DusIctv|DHD|CtrlHD|-ZR-|ADC|XZVN|RH|Kametsu|Celdra)(?=\]|\)))"),
        clean: re(r"(?i)(-(RP|1|NZBGeek|Obfuscated|Obfuscation|Scrambled|sample|Pre|postbot|xpost|Rakuv[a-z0-9]*|WhiteRev|BUYMORE|AsRequested|AlternativeToRequested|GEROV|Z0iDS3N|Chamele0n|4P|4Planet|AlteZachen|RePACKPOST))+$"),
        exact_first: true,
    }
});

static TV_GROUP: LazyLock<GroupRegexes> = LazyLock::new(|| {
    const NOT: &str = r"(?<!HDTV|SDTV|WEB-DL|Blu-Ray|480p|576p|720p|1080p|2160p|DTS-HD|DTS-X|DTS-MA|DTS-ES|-ES|-EN|-CAT|-GER|-FRA|-FRE|-ITA|\d{1,2}-bit|[ ._]\d{4}-\d{2}|-\d{2})";
    const C: &str = "[A-Za-zÀ-ÖØ-öø-ÿ0-9]";
    GroupRegexes {
        group: re(&format!(
            r"(?i)-(?<releasegroup>{C}+{NOT}(?<part2>-{C}+)?{NOT}(?!.+?(?:HDTV|SDTV|480p|576p|720p|1080p|2160p)))(?:\b|[-._ ]|$)|[-._ ]\[(?<releasegroup2>{C}+)\]$"
        )),
        exact: re(r"(?i)(?:(?<releasegroup>Fight-BB|VARYG|E\.N\.D|KRaLiMaRKo|BluDragon|DarQ|KCRT|BEN[_. ]THE[_. ]MEN|TAoE|QxR|Vialle)\b)"),
        exception: re(r"(?i)(?<=[._ \[])(?<releasegroup>(Joy|ImE|UTR|t3nzin|Anime Time|Project Angel|Hakata Ramen|HONE|Vyndros|SEV|Garshasp|Kappa|Natty|RCVR|SAMPA|YOGI|r00t|EDGE2020|Celdra)(?=\]|\)))"),
        clean: re(r"(?i)^(.*?[-._ ](S\d+E\d+)[-._ ])|(-(RP|1|NZBGeek|\[N-Z-B\]|Obfuscated|Scrambled|sample|Pre|postbot|xpost|Rakuv[a-z0-9]*|WhiteRev|BUYMORE|AsRequested|AlternativeToRequested|GEROV|Z0iDS3N|Chamele0n|4P|4Planet|AlteZachen|RePACKPOST))+$"),
        exact_first: false,
    }
});
static INVALID_RELEASE_GROUP: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^([se]\d+|[0-9a-f]{8})$"));
static ANIME_RELEASE_GROUP: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^(?:\[(?<subgroup>(?!\s).+?(?<!\s))\](?:_|-|\s|\.)?)"));

pub fn parse_release_group(title: &str, flavor: crate::quality::Flavor) -> Option<String> {
    let rx: &GroupRegexes = match flavor {
        crate::quality::Flavor::Movie => &MOVIE_GROUP,
        crate::quality::Flavor::Tv => &TV_GROUP,
    };
    let mut title = remove_file_extension(title.trim());
    if flavor == crate::quality::Flavor::Tv {
        title = pre_substitute(&title);
    }
    let title = replace_all(&WEBSITE_PREFIX, &title, "");
    let title = replace_all(&CLEAN_TORRENT_SUFFIX, &title, "");

    if let Ok(Some(c)) = crate::parser::common::caps(&ANIME_RELEASE_GROUP, &title) {
        return group(&c, "subgroup").map(str::to_string);
    }
    let title = replace_all(&rx.clean, &title, "");

    let last = |r: &Regex| {
        crate::parser::common::caps_iter(&r, &title)
            .filter_map(|c| c.ok())
            .last()
            .and_then(|c| group(&c, "releasegroup").or_else(|| group(&c, "releasegroup2")).map(str::to_string))
    };
    let order: [&Regex; 2] = if rx.exact_first { [&rx.exact, &rx.exception] } else { [&rx.exception, &rx.exact] };
    for r in order {
        if let Some(g) = last(r) {
            return Some(g);
        }
    }
    let g = last(&rx.group)?;
    if g.parse::<i64>().is_ok() || is_match(&INVALID_RELEASE_GROUP, &g) {
        return None;
    }
    Some(g)
}

static PRE_SUB_F1RST: LazyLock<Regex> = LazyLock::new(|| re(r"\.E(\d{2,4})\.\d{6}\.(.*-(F1RST|NEXT))$"));
static PRE_SUB_YEAR_INFO: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?<title>.+?(?=[ ._-]\()).+?\((?<year>\d{4})\/(?<info>S[^\/]+)"));

/// Sonarr's pre-substitutions. The CJK fansub rewrites are not ported.
pub(crate) fn pre_substitute(title: &str) -> String {
    let t = replace_all(&PRE_SUB_F1RST, title, ".S01E$1.$2");
    replace_all(&PRE_SUB_YEAR_INFO, &t, "${title} (${year}) - ${info} ")
}

// ---------------------------------------------------------------- languages

static LANGUAGE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?ix)(?:\W|_|^)(?<english>\beng\b)|
        (?<italian>\b(?:ita|italian)\b)|
        (?<german>(?:swiss)?german\b|videomann|ger[.\ ]dub|\bger\b)|
        (?<flemish>flemish)|
        (?<bulgarian>bgaudio)|
        (?<romanian>rodubbed)|
        (?<portuguese_br>\b(dublado|pt-BR)\b)|
        (?<greek>greek)|
        (?<french>\b(?:FR|VO|VF|VFF|VFQ|VFI|VF2|TRUEFRENCH|FRENCH|FRE|FRA)\b)|
        (?<russian>\b(?:rus|ru)\b)|
        (?<hungarian>\b(?:HUNDUB|HUN)\b)|
        (?<hebrew>\b(?:HebDub|HebDubbed)\b)|
        (?<polish>\b(?:PL\W?DUB|DUB\W?PL|LEK\W?PL|PL\W?LEK)\b)|
        (?<chinese>\[(?:CH[ST]|BIG5|GB)\]|简|繁|字幕)|
        (?<ukrainian>(?:(?:\dx)?UKR))|
        (?<spanish>\b(?:español|castellano)\b)|
        (?<catalan>\b(?:catalan?|catalán|català)\b)|
        (?<latvian>\b(?:lat|lav|lv)\b)|
        (?<telugu>\btel\b)|
        (?<vietnamese>\bVIE\b)|
        (?<japanese>\bJAP\b)|
        (?<korean>\bKOR\b)|
        (?<urdu>\burdu\b)|
        (?<original>\b(?:orig|original)\b)")
});
static LANGUAGE_CS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?x)(?:(?i)(?<!SUB[\W|_|^]))(?:(?<english>\bEN\b)|
        (?<lithuanian>\bLT\b)|
        (?<czech>\bCZ\b)|
        (?<polish>\bPL\b)|
        (?<bulgarian>\bBG\b)|
        (?<slovak>\bSK\b)|
        (?<german>\bDE\b)|
        (?<spanish>\b(?<!DTS[._\ -])ES\b))(?:(?i)(?![\W|_|^]SUB))")
});
static GERMAN_DL: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(?<!WEB[-_. ]?)\bDL\b"));
static GERMAN_ML: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bML\b"));

const LANGUAGE_WORDS: &[(&str, &str)] = &[
    ("english", "english"), ("spanish", "spanish"), ("danish", "danish"), ("dutch", "dutch"),
    ("japanese", "japanese"), ("icelandic", "icelandic"), ("mandarin", "chinese"), ("cantonese", "chinese"),
    ("chinese", "chinese"), ("korean", "korean"), ("russian", "russian"), ("romanian", "romanian"),
    ("hindi", "hindi"), ("arabic", "arabic"), ("thai", "thai"), ("bulgarian", "bulgarian"), ("polish", "polish"),
    ("vietnamese", "vietnamese"), ("swedish", "swedish"), ("norwegian", "norwegian"), ("finnish", "finnish"),
    ("turkish", "turkish"), ("portuguese", "portuguese"), ("brazilian", "portuguese_br"),
    ("hungarian", "hungarian"), ("hebrew", "hebrew"), ("ukrainian", "ukrainian"), ("persian", "persian"),
    ("bengali", "bengali"), ("slovak", "slovak"), ("latvian", "latvian"), ("latino", "spanish_latino"),
    ("tamil", "tamil"), ("telugu", "telugu"), ("malayalam", "malayalam"), ("kannada", "kannada"),
    ("albanian", "albanian"), ("afrikaans", "afrikaans"), ("marathi", "marathi"), ("tagalog", "tagalog"),
];

/// The name this parser uses for a language, from its ISO 639-1 code as metadata sources give it.
pub fn language_from_iso(code: &str) -> Option<&'static str> {
    Some(match code.to_lowercase().as_str() {
        "en" => "english", "fr" => "french", "de" => "german", "es" => "spanish", "it" => "italian", "ja" => "japanese", "ko" => "korean",
        "zh" | "cn" => "chinese", "ru" => "russian", "sv" => "swedish", "da" => "danish", "nl" => "dutch", "no" | "nb" | "nn" => "norwegian",
        "fi" => "finnish", "pl" => "polish", "pt" => "portuguese", "tr" => "turkish", "hu" => "hungarian", "cs" => "czech", "el" => "greek",
        "he" => "hebrew", "hi" => "hindi", "th" => "thai", "vi" => "vietnamese", "ar" => "arabic", "uk" => "ukrainian", "ro" => "romanian",
        "bg" => "bulgarian", "fa" => "persian", "bn" => "bengali", "ta" => "tamil", "te" => "telugu", "is" => "icelandic", "lt" => "lithuanian",
        "lv" => "latvian", "sk" => "slovak", "ca" => "catalan", "af" => "afrikaans", "sq" => "albanian", "kn" => "kannada", "ml" => "malayalam",
        "mr" => "marathi", "tl" => "tagalog", "ur" => "urdu",
        _ => return None,
    })
}

/// Languages named in a release title, lower-case. Empty means nothing was stated.
pub fn parse_languages(title: &str) -> Vec<String> {
    let lower = title.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    fn add(out: &mut Vec<String>, l: &str) {
        if !out.iter().any(|x| x == l) {
            out.push(l.to_string());
        }
    }
    for (word, lang) in LANGUAGE_WORDS {
        if lower.contains(word) {
            add(&mut out, lang);
        }
    }
    for r in [&*LANGUAGE_CS, &*LANGUAGE] {
        for c in caps_iter(r, title).filter_map(|c| c.ok()) {
            for name in r.capture_names().flatten() {
                if c.name(name).is_some() {
                    add(&mut out, name);
                }
            }
        }
    }
    if out.len() == 1 && out[0] == "german" {
        if is_match(&GERMAN_DL, title) {
            add(&mut out, "original");
        } else if is_match(&GERMAN_ML, title) {
            add(&mut out, "original");
            add(&mut out, "english");
        }
    }
    out
}

// ---------------------------------------------------------------- title cleaning

static NORMALIZE_MOVIE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)((?:\b|_)(?<!^|[^a-zA-Z0-9_']\w[^a-zA-Z0-9_'])([aà](?!$|[^a-zA-Z0-9_']\w[^a-zA-Z0-9_'])|an|the|and|or|of)(?!$)(?:\b|_))|\W|_")
});
static NORMALIZE_SERIES: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)((?:\b|_)(?<!^)([aà](?!$)|an|the|and|or|of)(?!$)(?:\b|_))|\W|_"));
static PERCENT: LazyLock<Regex> = LazyLock::new(|| re(r"(?<=\b\d+)%"));

fn remove_diacritics(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .map(|c| match c {
            'ð' | 'đ' => 'd',
            'Ð' | 'Đ' => 'D',
            'ø' => 'o',
            'Ø' => 'O',
            'ł' => 'l',
            'Ł' => 'L',
            'ı' => 'i',
            other => other,
        })
        .collect()
}

fn german_umlauts(s: &str) -> String {
    s.replace('ä', "ae")
        .replace('ö', "oe")
        .replace('ü', "ue")
        .replace('Ä', "Ae")
        .replace('Ö', "Oe")
        .replace('Ü', "Ue")
        .replace('ß', "ss")
}

/// Radarr's `CleanMovieTitle`: the key used to match a parsed title to a movie.
pub fn clean_movie_title(title: &str) -> String {
    if title.trim().is_empty() || title.parse::<i64>().is_ok() {
        return title.to_string();
    }
    remove_diacritics(&german_umlauts(&replace_all(&NORMALIZE_MOVIE, title, "").to_lowercase()))
}

/// Sonarr's `CleanSeriesTitle`: the key used to match a parsed title to a series.
pub fn clean_series_title(title: &str) -> String {
    if title.trim().is_empty() || title.parse::<i64>().is_ok() {
        return title.to_string();
    }
    let t = replace_all(&PERCENT, title, "percent");
    remove_diacritics(&replace_all(&NORMALIZE_SERIES, &t, "").to_lowercase())
}
