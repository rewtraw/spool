//! Mapping a parsed release name to a title in the library.

use crate::parser::{clean_movie_title, clean_series_title, ParsedEpisodeInfo, ParsedMovieInfo};

/// The parts of a library title that matching needs.
#[derive(Clone, Debug)]
pub struct TitleKey {
    pub id: i64,
    pub title: String,
    pub alt_titles: Vec<String>,
    pub year: u32,
    /// Other years the title is known by (festival premiere, regional release).
    pub alt_years: Vec<u32>,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<u32>,
}

/// "Part II" and "Part 2" are the same title. Normalise roman numerals up to twenty to digits.
fn arabic(title: &str) -> String {
    const ROMAN: [&str; 20] = ["I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X", "XI", "XII", "XIII", "XIV", "XV", "XVI", "XVII", "XVIII", "XIX", "XX"];
    let words: Vec<&str> = title.split(|c: char| c == ' ' || c == '.' || c == '_').filter(|w| !w.is_empty()).collect();
    words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let bare = w.trim_matches(|c: char| !c.is_alphanumeric());
            // A lone leading "I" or "X" is far more often a word or a title than a number.
            match ROMAN.iter().position(|r| *r == bare) {
                Some(n) if i > 0 && words.len() > 1 => w.replace(bare, &(n + 1).to_string()),
                _ => w.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn movie_keys(title: &str) -> [String; 2] {
    [clean_movie_title(title), clean_movie_title(&arabic(title))]
}

/// Find the movie a parsed release refers to. The year must agree when the release states one.
pub fn match_movie<'a>(parsed: &ParsedMovieInfo, movies: &'a [TitleKey]) -> Option<&'a TitleKey> {
    if let Some(imdb) = &parsed.imdb_id {
        if let Some(m) = movies.iter().find(|m| m.imdb_id.as_deref() == Some(imdb.as_str())) {
            return Some(m);
        }
    }
    if let Some(tmdb) = parsed.tmdb_id.filter(|t| *t > 0) {
        if let Some(m) = movies.iter().find(|m| m.tmdb_id == Some(tmdb)) {
            return Some(m);
        }
    }
    let wanted: Vec<String> = parsed.titles.iter().flat_map(|t| movie_keys(t)).filter(|t| !t.is_empty()).collect();
    let year_ok = |m: &TitleKey| parsed.year == 0 || parsed.year == m.year || m.alt_years.contains(&parsed.year);
    movies.iter().find(|m| {
        year_ok(m)
            && std::iter::once(&m.title).chain(m.alt_titles.iter()).any(|t| movie_keys(t).iter().any(|c| wanted.contains(c)))
    })
}

/// A series name reduced for comparison. A leading "The" is dropped, because release names add
/// and omit it freely ("The Vision of Escaflowne" is "Vision of Escaflowne").
fn series_name(title: &str) -> String {
    let t = title.trim();
    let rest = t.get(..4).filter(|head| head.eq_ignore_ascii_case("the ") || head.eq_ignore_ascii_case("the.")).map(|_| &t[4..]).filter(|rest| rest.trim().len() >= 3);
    clean_series_title(rest.unwrap_or(t))
}

/// Find the series a parsed release refers to.
pub fn match_series<'a>(parsed: &ParsedEpisodeInfo, series: &'a [TitleKey]) -> Option<&'a TitleKey> {
    let full = series_name(&parsed.series_title);
    let without_year = series_name(&parsed.series_title_without_year);
    let mut names: Vec<String> = vec![full.clone()];
    names.extend(parsed.all_titles.iter().map(|t| series_name(t)));
    let by_name = |name: &str| {
        series.iter().find(|s| std::iter::once(&s.title).chain(s.alt_titles.iter()).any(|t| series_name(t) == name))
    };
    for n in &names {
        if let Some(s) = by_name(n) {
            return Some(s);
        }
    }
    // "Show 2016" in the release, "Show" in the library with a matching first-air year.
    if let Some(year) = parsed.series_year {
        if let Some(s) = series.iter().find(|s| {
            s.year == year && std::iter::once(&s.title).chain(s.alt_titles.iter()).any(|t| series_name(t) == without_year)
        }) {
            return Some(s);
        }
    }
    // "Show" in the release, "Show (2016)" in the library.
    series.iter().find(|s| s.year > 0 && series_name(&format!("{} {}", parsed.series_title, s.year)) == series_name(&s.title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_episode_title, parse_movie_title};

    fn key(id: i64, title: &str, year: u32) -> TitleKey {
        TitleKey { id, title: title.into(), alt_titles: vec![], year, alt_years: vec![], imdb_id: None, tmdb_id: None }
    }

    #[test]
    fn movies_need_title_and_year() {
        let lib = [key(1, "Dracula", 1931), key(2, "Bram Stoker's Dracula", 1992), key(3, "Jacob's Ladder", 1990)];
        let p = parse_movie_title("Bram.Stokers.Dracula.1992.BluRay.Remux.1080p.AVC.TrueHD.7.1-HiFi", false).unwrap();
        assert_eq!(match_movie(&p, &lib).map(|m| m.id), Some(2));
        let p = parse_movie_title("Jacobs.Ladder.2019.1080p.WEB-DL.H264-GRP", false).unwrap();
        assert_eq!(match_movie(&p, &lib).map(|m| m.id), None);
        let mut sequel = key(4, "Patlabor 2: The Movie", 1993);
        sequel.alt_titles = vec!["Patlabor Movie II".into()];
        let p = parse_movie_title("Patlabor.The.Movie.2.1993.MULTi.1080p.BluRay.x264-DEAL", false).unwrap();
        assert_eq!(match_movie(&p, &[sequel]).map(|m| m.id), Some(4));
        assert_eq!(arabic("Rocky III"), "Rocky 3");
        assert_eq!(arabic("I Robot"), "I Robot");
        assert_eq!(arabic("X"), "X");
    }

    #[test]
    fn series_match_with_and_without_year() {
        let lib = [key(1, "Westworld", 2016), key(2, "Civilisation", 1969), key(3, "The Office (US)", 2005)];
        let p = parse_episode_title("Westworld.S01E02.1080p.BluRay.x264-GRP").unwrap();
        assert_eq!(match_series(&p, &lib).map(|s| s.id), Some(1));
        let p = parse_episode_title("Westworld.2016.S01E02.1080p.BluRay.x264-GRP").unwrap();
        assert_eq!(match_series(&p, &lib).map(|s| s.id), Some(1));
        let p = parse_episode_title("The.Office.US.S03E01.720p.HDTV.x264-GRP").unwrap();
        assert_eq!(match_series(&p, &lib).map(|s| s.id), Some(3));
    }

    #[test]
    fn a_leading_article_does_not_matter_for_series() {
        let lib = [key(1, "Vision of Escaflowne", 1996), key(2, "The Office", 2005), key(3, "Them", 2021)];
        let p = parse_episode_title("The.Vision.of.Escaflowne.S01E16.1080p.CR.WEB-DL.AAC2.0.H.264-OLDT").unwrap();
        assert_eq!(match_series(&p, &lib).map(|m| m.id), Some(1));
        let p = parse_episode_title("Office.S02E03.720p.WEB-DL.H264-GRP").unwrap();
        assert_eq!(match_series(&p, &lib).map(|m| m.id), Some(2));
        let p = parse_episode_title("Them.S01E01.1080p.WEB.H264-GRP").unwrap();
        assert_eq!(match_series(&p, &lib).map(|m| m.id), Some(3));
    }

    #[test]
    fn anime_film_names_as_indexers_write_them() {
        let lib = [key(1, "Legend of the Galactic Heroes: My Conquest Is the Sea of Stars", 1988), key(2, "Legend of the Galactic Heroes: Golden Wings", 1992)];
        for name in [
            "Legend.of.the.Galactic.Heroes.My.Conquest.Is.the.Sea.of.Stars.1988.BD.1080p.HEVC.FLAC",
            "Legend of the Galactic Heroes - My Conquest Is the Sea of Stars (1988) (BD 1080p HEVC FLAC) [8EA04235] [P9]",
        ] {
            let p = parse_movie_title(name, false).unwrap_or_else(|| panic!("unparsed: {name}"));
            assert_eq!(match_movie(&p, &lib).map(|m| m.id), Some(1), "{name} parsed as {:?} ({})", p.title(), p.year);
            assert_eq!(p.quality.quality, crate::Quality::Bluray1080p, "{name}");
        }
    }
}
