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

/// What a release's name says, read as a film's.
pub fn movie(name: &str) -> Option<ParsedMovieInfo> {
    static KEPT: LazyLock<Mutex<HashMap<String, Option<ParsedMovieInfo>>>> = LazyLock::new(Default::default);
    remembered(&KEPT, name, || spool_core::parser::parse_movie_title(name, false))
}

#[cfg(test)]
mod tests {
    use super::*;

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
