//! Names: comparing them, and reading what a release's name says.
//!
//! Reading a release name runs a long list of patterns and takes a millisecond or two. The
//! same names are read again every time a title's page is opened or a search is judged, so
//! what each one said is remembered.

use parking_lot::Mutex;
use spool_core::parser::{ParsedEpisodeInfo, ParsedMovieInfo};
use std::collections::HashMap;
use std::sync::LazyLock;

/// A name down to its letters and digits, in lower case: how two spellings of one name are
/// found to be the same.
pub fn plain(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// How many readings are kept before the memory is cleared and filled again.
const KEPT: usize = 40_000;

fn remembered<T: Clone>(kept: &Mutex<HashMap<String, Option<T>>>, name: &str, read: impl FnOnce() -> Option<T>) -> Option<T> {
    if let Some(known) = kept.lock().get(name) {
        return known.clone();
    }
    let fresh = read();
    let mut kept = kept.lock();
    if kept.len() >= KEPT {
        kept.clear();
    }
    kept.insert(name.to_string(), fresh.clone());
    fresh
}

/// What a release's name says, read as an episode's.
pub fn episode(name: &str) -> Option<ParsedEpisodeInfo> {
    static KEPT: LazyLock<Mutex<HashMap<String, Option<ParsedEpisodeInfo>>>> = LazyLock::new(Default::default);
    remembered(&KEPT, name, || spool_core::parser::parse_episode_title(name))
}

/// A number at the end of a series' own title read as its season: "Blue.Submarine.No.6.E01"
/// is episode 1 of Blue Submarine No. 6, not season 6 of Blue Submarine No.
pub fn title_keeps_its_number(mut p: ParsedEpisodeInfo, t: &crate::models::Title) -> ParsedEpisodeInfo {
    if let [season] = p.season_numbers[..] {
        let whole = format!("{} {season}", p.series_title);
        if std::iter::once(&t.title).chain(t.alt_titles.iter()).any(|n| plain(n) == plain(&whole)) {
            p.series_title_without_year = whole.clone();
            p.series_title = whole;
            p.season_numbers = vec![1];
        }
    }
    p
}

/// What a release's name says, read as a film's.
pub fn movie(name: &str) -> Option<ParsedMovieInfo> {
    static KEPT: LazyLock<Mutex<HashMap<String, Option<ParsedMovieInfo>>>> = LazyLock::new(Default::default);
    remembered(&KEPT, name, || spool_core::parser::parse_movie_title(name, false))
}

/// The episode a name says it is by its title, when that is not the one its number says.
/// Some releases number a series in an order of their own and give each episode its title
/// after the number, "Show.S01E12.Double.Dare.1080p": the title is believed. Only a title
/// that is one episode's whole name, word for word from just after the number, is taken,
/// and only when the episode numbered says something else.
pub fn retitled<'a>(name: &str, season: u32, number: u32, episodes: &'a [crate::models::Episode]) -> Option<&'a crate::models::Episode> {
    let words: Vec<String> = name.split(['.', ' ', '_', '-', '/']).filter(|w| !w.is_empty()).map(plain).collect();
    let at = words.iter().position(|w| {
        let w = w.as_bytes();
        w.len() >= 4 && w[0] == b's' && w.iter().skip(1).take_while(|c| c.is_ascii_digit()).count() > 0 && w.contains(&b'e')
    })?;
    let after = &words[at + 1..];
    // What follows the number at once is the release's own words, not a title.
    const RELEASE_WORDS: &[&str] = &["web", "webdl", "webrip", "bluray", "bdrip", "brrip", "hdtv", "dvdrip", "dvd", "remux", "proper", "repack", "real", "internal", "multi", "dual", "x264", "x265", "h264", "h265", "hevc", "avc", "aac", "aac20", "ddp", "dd", "ac3", "flac", "hdr", "sdr", "uhd", "amzn", "nf", "dsnp", "hmax", "atvp", "cr", "hulu", "pcok", "itunes", "subbed", "dubbed", "complete", "extended", "uncut"];
    if after.first().is_none_or(|w| RELEASE_WORDS.contains(&w.as_str()) || w.ends_with('p') && w[..w.len() - 1].chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    // Each episode whose title the words after the number begin with, longest title first.
    let says = |e: &crate::models::Episode| {
        let t = plain(&e.title);
        let mut joined = String::new();
        t.len() >= 3 && after.iter().any(|w| {
            joined.push_str(w);
            joined == t
        })
    };
    let numbered = episodes.iter().find(|e| e.season == season && e.episode == number);
    if numbered.is_some_and(says) {
        return None;
    }
    let mut named = episodes.iter().filter(|e| says(e));
    match (named.next(), named.next()) {
        (Some(e), None) => Some(e),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_episode_named_by_its_title_is_the_one_with_that_title() {
        let ep = |season: u32, episode: u32, title: &str| -> crate::models::Episode { serde_json::from_value(serde_json::json!({"title_id": 1, "season": season, "episode": episode, "title": title})).unwrap() };
        let list = [ep(1, 12, "Trick or Treat"), ep(1, 14, "Double Dare"), ep(1, 9, "The Week at Grandma's"), ep(1, 11, "Too Much TV"), ep(2, 1, "Go")];
        let at = |name: &str, s, n| retitled(name, s, n, &list).map(|e| (e.season, e.episode));
        assert_eq!(at("The.Berenstain.Bears.2003.S01E12.Double.Dare.AAC2.0.1080p.WEBRip.x265-PoF", 1, 12), Some((1, 14)));
        assert_eq!(at("The.Berenstain.Bears.2003.S01E10.Trick.or.Treat.AAC2.0.1080p.WEBRip.x265-PoF", 1, 10), Some((1, 12)));
        assert_eq!(at("The.Berenstain.Bears.2003.S01E06.The.Week.at.Grandmas.1080p.WEBRip-PoF", 1, 6), Some((1, 9)));
        assert_eq!(at("The.Berenstain.Bears.S01E12.Trick.or.Treat.1080p", 1, 12), None, "the number and the title agree");
        assert_eq!(at("The.Berenstain.Bears.S01E12.1080p.WEBRip.x265-PoF", 1, 12), None, "no title");
        assert_eq!(at("The.Berenstain.Bears.S01E12.Double.Trouble.1080p", 1, 12), None, "only a whole title");
        assert_eq!(at("The.Berenstain.Bears.S01E12.Go.Fishing.1080p", 1, 12), None, "too short to say");
        let web = [ep(1, 12, "Trick or Treat"), ep(1, 3, "Web"), ep(1, 4, "1080p Dreams")];
        assert_eq!(retitled("Show.S01E12.WEB-DL.1080p-GRP", 1, 12, &web).map(|e| e.episode), None, "the release's own words are not a title");
        assert_eq!(retitled("Show.S01E12.1080p.Dreams.WEB-GRP", 1, 12, &web).map(|e| e.episode), None);
    }

    #[test]
    fn a_name_read_twice_says_the_same_and_spellings_compare_equal() {
        let name = "Some.Show.S02E03.1080p.WEB-DL.x264-GRP";
        let (first, again) = (episode(name).unwrap(), episode(name).unwrap());
        assert_eq!((first.season_number(), &first.episode_numbers), (2, &vec![3]));
        assert_eq!(first.episode_numbers, again.episode_numbers);
        assert!(episode("").is_none() && episode("").is_none());
        assert_eq!(plain("Ghost in the Shell: Arise - Border:2"), plain("ghost in the shell arise border 2"));
    }
}
