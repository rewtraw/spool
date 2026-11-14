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

/// A stretch of time as people say it: "hour", "3 hours", "90 minutes".
pub fn span_minutes(m: i64) -> String {
    match m {
        60 => "hour".into(),
        m if m % 60 == 0 => format!("{} hours", m / 60),
        m => format!("{m} minutes"),
    }
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
/// The words a release name gives after its "S01E02", up to where its own words begin, when
/// it gives any: the episode's title, as the release has it.
fn after_number(name: &str) -> Option<Vec<String>> {
    let words: Vec<String> = name.split(['.', ' ', '_', '-', '/']).filter(|w| !w.is_empty()).map(plain).collect();
    let at = words.iter().position(|w| {
        let w = w.as_bytes();
        w.len() >= 4 && w[0] == b's' && w.iter().skip(1).take_while(|c| c.is_ascii_digit()).count() > 0 && w.contains(&b'e')
    })?;
    // What follows the number at once is the release's own words, not a title.
    const RELEASE_WORDS: &[&str] = &["web", "webdl", "webrip", "bluray", "bdrip", "brrip", "hdtv", "dvdrip", "dvd", "remux", "proper", "repack", "real", "internal", "multi", "dual", "x264", "x265", "h264", "h265", "hevc", "avc", "aac", "aac20", "ddp", "dd", "ac3", "eac3", "flac", "hdr", "sdr", "uhd", "amzn", "nf", "dsnp", "hmax", "atvp", "cr", "hulu", "pcok", "itunes", "all4", "subbed", "dubbed", "complete", "extended", "uncut"];
    let ours = |w: &String| RELEASE_WORDS.contains(&w.as_str()) || w.ends_with('p') && w.len() > 1 && w[..w.len() - 1].chars().all(|c| c.is_ascii_digit());
    let after = &words[at + 1..];
    // Its own words straight after the number: no title. Otherwise all that follows, title and
    // the release's own words after it, for a caller to read as far as it needs.
    (!after.first().is_none_or(ours)).then(|| after.to_vec())
}

/// Of the words after a release name's number, the ones before its own words begin.
fn title_words(after: &[String]) -> Vec<String> {
    const RELEASE_WORDS: &[&str] = &["web", "webdl", "webrip", "bluray", "bdrip", "brrip", "hdtv", "dvdrip", "dvd", "remux", "x264", "x265", "h264", "h265", "hevc", "avc", "aac", "aac20", "ddp", "ac3", "eac3", "flac", "hdr", "uhd", "amzn", "nf", "dsnp", "hmax", "atvp", "all4"];
    after.iter().take_while(|w| !RELEASE_WORDS.contains(&w.as_str()) && !(w.ends_with('p') && w.len() > 1 && w[..w.len() - 1].chars().all(|c| c.is_ascii_digit()))).cloned().collect()
}

/// Whether a release's name names this episode: Some(true) when the words after its number
/// are the episode's title or share a word of it, Some(false) when they share none (another
/// show's episode, more often than not), None when it names no episode or the episode has no
/// title worth the name.
pub fn says_episode(name: &str, episode: &crate::models::Episode) -> Option<bool> {
    let said = title_words(&after_number(name)?);
    let title = plain(&episode.title);
    if title.len() < 3 || title.starts_with("episode") || title.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let theirs: Vec<String> = episode.title.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(plain).collect();
    let joined: String = said.concat();
    let shares = said.iter().any(|w| w.len() >= 3 && theirs.contains(w));
    Some(joined.starts_with(&title) || shares)
}

pub fn retitled<'a>(name: &str, season: u32, number: u32, episodes: &'a [crate::models::Episode]) -> Option<&'a crate::models::Episode> {
    let after = after_number(name)?;
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
    fn a_name_says_whether_it_is_this_episode() {
        let ep = |title: &str| -> crate::models::Episode { serde_json::from_value(serde_json::json!({"title_id": 1, "season": 1, "episode": 3, "title": title})).unwrap() };
        let murder = ep("Murder Case");
        assert_eq!(says_episode("Monster.S01E03.Murder.Case.1080p.BluRay.DUAL.FLAC.2.0.x264-KITSUNE", &murder), Some(true));
        assert_eq!(says_episode("Monster.S01E03.Doin.A.Dahmer.EAC3.5.1.1080p.WEBRip.x265-iVy", &murder), Some(false));
        assert_eq!(says_episode("Monster.S01E03.1080p.ALL4.WEB-DL.AAC2.0.H.264-FUZEER", &murder), None);
        assert_eq!(says_episode("Monster.S01E03.1080p.HEVC.x265-MeGusta", &murder), None);
        assert_eq!(says_episode("Show.S01E03.Herr.Dr.Tenma.1080p", &ep("Herr Dr. Tenma")), Some(true));
        assert_eq!(says_episode("Show.S01E03.Pilot.1080p", &ep("Episode 3")), None, "no title worth the name");
        let list = [ep("One"), crate::models::Episode { episode: 4, ..ep("The Real Me") }];
        assert_eq!(retitled("Show.S01E03.The.Real.Me.1080p.WEB.H264-GRP", 1, 3, &list).map(|e| e.episode), Some(4), "a title with a release word in it");
    }

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
