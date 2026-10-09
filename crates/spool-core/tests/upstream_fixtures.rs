//! Runs Radarr's and Sonarr's own parser test cases against Spool's port.
//!
//! These are conformance measurements, not unit tests: the port is allowed a small, listed set of
//! known differences. Each group asserts a minimum pass rate so regressions fail the build.
//! Run with `--nocapture` to see every failing case.

use serde_json::Value;
use spool_core::parser::*;
use spool_core::quality::{parse_quality, Flavor, Quality};

fn load(name: &str) -> Value {
    let p = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn cases<'a>(v: &'a Value, fixture: &str, method: &str) -> Vec<&'a Vec<Value>> {
    v[fixture][method]["cases"].as_array().map(|a| a.iter().filter_map(|c| c.as_array()).collect()).unwrap_or_default()
}

struct Tally {
    name: String,
    pass: usize,
    fail: Vec<String>,
}

impl Tally {
    fn new(name: &str) -> Self {
        Tally { name: name.to_string(), pass: 0, fail: vec![] }
    }
    fn check(&mut self, ok: bool, detail: impl FnOnce() -> String) {
        if ok {
            self.pass += 1
        } else {
            self.fail.push(detail())
        }
    }
    fn finish(self, min_rate: f64) {
        let total = self.pass + self.fail.len();
        let rate = if total == 0 { 1.0 } else { self.pass as f64 / total as f64 };
        println!("{:<44} {:>4}/{:<4} {:.1}%", self.name, self.pass, total, rate * 100.0);
        for f in &self.fail {
            println!("    FAIL {f}");
        }
        assert!(total > 0, "{}: no cases loaded", self.name);
        assert!(rate >= min_rate, "{}: pass rate {:.3} below {:.3}", self.name, rate, min_rate);
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn quality_for_method(method: &str, flavor: Flavor) -> Option<Quality> {
    let m = method.strip_prefix("should_parse_")?.strip_suffix("_quality")?;
    Some(match m {
        "ts" => Quality::Telesync,
        "cam" => Quality::Cam,
        "sdtv" => Quality::Sdtv,
        "dvd" => Quality::Dvd,
        "dvdr" => Quality::DvdR,
        "webdl480p" => Quality::Webdl480p,
        "webrip480p" => Quality::Webrip480p,
        "bluray480p" => Quality::Bluray480p,
        "bluray576p" => Quality::Bluray576p,
        "hdtv720p" => Quality::Hdtv720p,
        "hdtv1080p" => Quality::Hdtv1080p,
        "hdtv2160p" => Quality::Hdtv2160p,
        "webdl720p" => Quality::Webdl720p,
        "webrip720p" => Quality::Webrip720p,
        "webdl1080p" => Quality::Webdl1080p,
        "webrip1080p" => Quality::Webrip1080p,
        "webdl2160p" => Quality::Webdl2160p,
        "webrip2160p" => Quality::Webrip2160p,
        "bluray720p" => Quality::Bluray720p,
        "bluray1080p" => Quality::Bluray1080p,
        "bluray2160p" => Quality::Bluray2160p,
        "remux720p_as_bluray720p" => Quality::Bluray720p,
        "remux1080p" | "bluray1080p_remux" | "bluray_1080p_remux" => Quality::Remux1080p,
        "remux2160p" | "bluray2160p_remux" | "bluray_2160p_remux" => Quality::Remux2160p,
        "brdisk_1080p" => Quality::BrDisk,
        "raw" => Quality::RawHd,
        _ => {
            let _ = flavor;
            return None;
        }
    })
}

fn run_quality(file: &str, flavor: Flavor, min: f64) {
    let v = load(file);
    let mut t = Tally::new(&format!("{file} quality"));
    let mut unmapped = vec![];
    for (method, body) in v["QualityParserFixture"].as_object().unwrap() {
        let Some(expected) = quality_for_method(method, flavor) else {
            if method.ends_with("_quality") {
                unmapped.push(method.clone());
            }
            continue;
        };
        for c in body["cases"].as_array().unwrap() {
            let title = s(&c[0]);
            let proper = c.get(1).and_then(|p| p.as_bool());
            let got = parse_quality(title, flavor);
            // Radarr's fixture checks source and resolution only for TELESYNC.
            let ok_quality = got.quality == expected;
            let ok_proper = proper.map(|p| (got.revision.version > 1) == p).unwrap_or(true);
            t.check(ok_quality && ok_proper, || format!("{title} => {:?} v{} (want {:?})", got.quality, got.revision.version, expected));
        }
    }
    for c in cases(&v, "QualityParserFixture", "should_be_able_to_parse_repack") {
        let got = parse_quality(s(&c[0]), flavor);
        let ok = got.revision.is_repack == c[1].as_bool().unwrap() && got.revision.version as i64 == c[2].as_i64().unwrap();
        t.check(ok, || format!("repack {} => {:?}", s(&c[0]), got.revision));
    }
    assert!(unmapped.is_empty(), "unmapped quality methods: {unmapped:?}");
    t.finish(min);
}

#[test]
fn radarr_quality() {
    run_quality("radarr", Flavor::Movie, 1.0);
}

#[test]
fn sonarr_quality() {
    run_quality("sonarr", Flavor::Tv, 1.0);
}

#[test]
fn radarr_movie_titles() {
    let v = load("radarr");
    let mut t = Tally::new("radarr movie title");
    for c in cases(&v, "ParserFixture", "should_parse_movie_title") {
        let got = parse_movie_title(s(&c[0]), false);
        let title = got.as_ref().map(|g| g.title().to_string()).unwrap_or_default();
        t.check(title == s(&c[1]), || format!("{} => {:?} (want {:?})", s(&c[0]), title, s(&c[1])));
    }
    for c in cases(&v, "ParserFixture", "should_parse_movie_year") {
        let got = parse_movie_title(s(&c[0]), false).map(|g| g.year as i64).unwrap_or(-1);
        t.check(got == c[1].as_i64().unwrap(), || format!("year {} => {got}", s(&c[0])));
    }
    for c in cases(&v, "ParserFixture", "should_parse_movie_alternative_titles") {
        let got = parse_movie_title(s(&c[0]), false).map(|g| g.titles).unwrap_or_default();
        let want: Vec<String> = c[1].as_array().unwrap().iter().map(|x| s(x).to_string()).collect();
        t.check(got == want, || format!("alt {} => {got:?} (want {want:?})", s(&c[0])));
    }
    for c in cases(&v, "ParserFixture", "should_parse_tmdb_id") {
        let got = parse_movie_title(s(&c[0]), false).and_then(|g| g.tmdb_id).unwrap_or(0) as i64;
        t.check(got == c[1].as_i64().unwrap(), || format!("tmdb {} => {got}", s(&c[0])));
    }
    for c in cases(&v, "ParserFixture", "should_parse_imdb_in_title") {
        let got = parse_movie_title(s(&c[0]), false).and_then(|g| g.imdb_id).unwrap_or_default();
        t.check(got == s(&c[1]), || format!("imdb {} => {got}", s(&c[0])));
    }
    for c in cases(&v, "ParserFixture", "should_parse_german_movie") {
        let got = parse_movie_title(s(&c[0]), false);
        let (title, edition, year) = got.map(|g| (g.title().to_string(), g.edition, g.year as i64)).unwrap_or_default();
        t.check(title == s(&c[1]) && edition == s(&c[2]) && year == c[3].as_i64().unwrap(), || {
            format!("german {} => {title:?} {edition:?} {year} (want {:?} {:?} {})", s(&c[0]), s(&c[1]), s(&c[2]), c[3])
        });
    }
    t.finish(1.0);
}

#[test]
fn radarr_editions() {
    let v = load("radarr");
    let mut t = Tally::new("radarr edition");
    for c in cases(&v, "EditionParserFixture", "should_parse_edition") {
        let got = parse_movie_title(s(&c[0]), false).map(|g| g.edition).unwrap_or_default();
        t.check(got == s(&c[1]), || format!("{} => {got:?} (want {:?})", s(&c[0]), s(&c[1])));
    }
    for c in cases(&v, "EditionParserFixture", "should_not_parse_edition") {
        let got = parse_movie_title(s(&c[0]), false).map(|g| g.edition).unwrap_or_default();
        t.check(got.is_empty(), || format!("{} => {got:?} (want none)", s(&c[0])));
    }
    t.finish(1.0);
}

fn run_release_groups(file: &str, flavor: Flavor, min: f64) {
    let v = load(file);
    let mut t = Tally::new(&format!("{file} release group"));
    for (method, body) in v["ReleaseGroupParserFixture"].as_object().unwrap() {
        for c in body["cases"].as_array().unwrap() {
            let title = s(&c[0]);
            let want = c.get(1).and_then(|x| x.as_str()).map(str::to_string);
            let got = parse_release_group(title, flavor);
            let got = got.filter(|g| !g.is_empty());
            let ok = if c.as_array().unwrap().len() == 1 { got.is_none() } else { got == want };
            t.check(ok, || format!("[{method}] {title} => {got:?} (want {want:?})"));
        }
    }
    t.finish(min);
}

#[test]
fn radarr_release_groups() {
    run_release_groups("radarr", Flavor::Movie, 1.0);
}

#[test]
fn sonarr_release_groups() {
    run_release_groups("sonarr", Flavor::Tv, 1.0);
}

#[test]
fn radarr_crap() {
    let v = load("radarr");
    let mut t = Tally::new("radarr rejects junk");
    for c in cases(&v, "CrapParserFixture", "should_not_parse_crap") {
        t.check(parse_movie_title(s(&c[0]), false).is_none(), || format!("{} parsed", s(&c[0])));
    }
    t.finish(1.0);
}

#[test]
fn sonarr_single_episodes() {
    let v = load("sonarr");
    let mut t = Tally::new("sonarr single episode");
    for c in cases(&v, "SingleEpisodeParserFixture", "should_parse_single_episode") {
        let got = parse_episode_title(s(&c[0]));
        let ok = got.as_ref().is_some_and(|g| {
            g.series_title == s(&c[1])
                && g.season_number() as i64 == c[2].as_i64().unwrap()
                && g.episode_numbers.len() == 1
                && g.episode_numbers[0] as i64 == c[3].as_i64().unwrap()
                && g.absolute_episode_numbers.is_empty()
                && !g.full_season
        });
        t.check(ok, || {
            format!("{} => {:?} (want {:?} S{}E{})", s(&c[0]), got.map(|g| (g.series_title, g.season_numbers, g.episode_numbers, g.absolute_episode_numbers)), s(&c[1]), c[2], c[3])
        });
    }
    t.finish(1.0);
}

#[test]
fn sonarr_multi_episodes() {
    let v = load("sonarr");
    let mut t = Tally::new("sonarr multi episode");
    for c in cases(&v, "MultiEpisodeParserFixture", "should_parse_multiple_episodes") {
        let got = parse_episode_title(s(&c[0]));
        let want: Vec<u32> = c[3].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as u32).collect();
        let ok = got.as_ref().is_some_and(|g| {
            g.series_title == s(&c[1]) && g.season_number() as i64 == c[2].as_i64().unwrap() && g.episode_numbers == want
        });
        t.check(ok, || format!("{} => {:?} (want {:?} S{} {:?})", s(&c[0]), got.map(|g| (g.series_title, g.season_numbers, g.episode_numbers)), s(&c[1]), c[2], want));
    }
    t.finish(1.0);
}

#[test]
fn sonarr_seasons() {
    let v = load("sonarr");
    let mut t = Tally::new("sonarr season pack");
    for c in cases(&v, "SeasonParserFixture", "should_parse_full_season_release") {
        let got = parse_episode_title(s(&c[0]));
        let ok = got.as_ref().is_some_and(|g| {
            g.series_title == s(&c[1]) && g.season_number() as i64 == c[2].as_i64().unwrap() && g.full_season && g.episode_numbers.is_empty()
        });
        t.check(ok, || format!("{} => {:?}", s(&c[0]), got.map(|g| (g.series_title, g.season_numbers, g.episode_numbers, g.full_season))));
    }
    for c in cases(&v, "SeasonParserFixture", "should_parse_multi_season_release") {
        let got = parse_episode_title(s(&c[0]));
        let want: Vec<u32> = c[2].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as u32).collect();
        let ok = got.as_ref().is_some_and(|g| g.series_title == s(&c[1]) && g.season_numbers == want && g.full_season);
        t.check(ok, || format!("multi {} => {:?} (want {want:?})", s(&c[0]), got.map(|g| (g.series_title, g.season_numbers))));
    }
    for c in cases(&v, "SeasonParserFixture", "should_parse_partial_season_release") {
        let got = parse_episode_title(s(&c[0]));
        let ok = got.as_ref().is_some_and(|g| {
            g.series_title == s(&c[1]) && g.season_number() as i64 == c[2].as_i64().unwrap() && g.is_partial_season && g.season_part as i64 == c[3].as_i64().unwrap()
        });
        t.check(ok, || format!("partial {} => {:?}", s(&c[0]), got.map(|g| (g.series_title, g.season_numbers, g.season_part))));
    }
    for c in cases(&v, "SeasonParserFixture", "should_parse_season_extras") {
        let got = parse_episode_title(s(&c[0]));
        let ok = got.as_ref().is_some_and(|g| g.series_title == s(&c[1]) && g.season_number() as i64 == c[2].as_i64().unwrap() && g.is_season_extra);
        t.check(ok, || format!("extras {}", s(&c[0])));
    }
    t.finish(1.0);
}

#[test]
fn sonarr_daily() {
    let v = load("sonarr");
    let mut t = Tally::new("sonarr daily");
    for c in cases(&v, "DailyEpisodeParserFixture", "should_parse_daily_episode") {
        let got = parse_episode_title(s(&c[0]));
        let want = format!("{:04}-{:02}-{:02}", c[2].as_i64().unwrap(), c[3].as_i64().unwrap(), c[4].as_i64().unwrap());
        let ok = got.as_ref().is_some_and(|g| g.series_title == s(&c[1]) && g.air_date.as_deref() == Some(&want));
        t.check(ok, || format!("{} => {:?} (want {:?} {want})", s(&c[0]), got.map(|g| (g.series_title, g.air_date)), s(&c[1])));
    }
    t.finish(1.0);
}

#[test]
fn sonarr_mini_series_and_paths() {
    let v = load("sonarr");
    let mut t = Tally::new("sonarr mini-series and paths");
    for c in cases(&v, "MiniSeriesEpisodeParserFixture", "should_parse_mini_series_episode") {
        let got = parse_episode_title(s(&c[0]));
        let ok = got.as_ref().is_some_and(|g| g.series_title == s(&c[1]) && g.episode_numbers == vec![c[2].as_u64().unwrap() as u32] && g.season_number() == 1);
        t.check(ok, || format!("{} => {:?}", s(&c[0]), got.map(|g| (g.series_title, g.season_numbers, g.episode_numbers))));
    }
    for c in cases(&v, "PathParserFixture", "should_parse_from_path") {
        let path = s(&c[0]).replace('\\', "/");
        let got = parse_episode_path(&path);
        let ok = got.as_ref().is_some_and(|g| {
            g.season_number() as i64 == c[1].as_i64().unwrap() && g.episode_numbers.first().copied().unwrap_or(0) as i64 == c[2].as_i64().unwrap()
        });
        t.check(ok, || format!("path {path} => {:?} (want S{}E{})", got.map(|g| (g.season_numbers, g.episode_numbers)), c[1], c[2]));
    }
    t.finish(1.0);
}

#[test]
fn sonarr_series_titles() {
    let v = load("sonarr");
    let mut t = Tally::new("sonarr series title cleaning");
    for c in cases(&v, "ParserFixture", "should_parse_series_name") {
        let got = parse_episode_title(s(&c[0])).map(|g| clean_series_title(&g.series_title)).unwrap_or_else(|| clean_series_title(s(&c[0])));
        let want = clean_series_title(s(&c[1]));
        t.check(got == want, || format!("{} => {got:?} (want {want:?})", s(&c[0])));
    }
    for c in cases(&v, "NormalizeSeriesTitleFixture", "should_remove_special_characters_and_casing") {
        let got = clean_series_title(s(&c[0]));
        t.check(got == s(&c[1]), || format!("clean {} => {got:?} (want {:?})", s(&c[0]), s(&c[1])));
    }
    for c in cases(&v, "CrapParserFixture", "should_not_parse_crap") {
        t.check(parse_episode_title(s(&c[0])).is_none(), || format!("junk parsed: {}", s(&c[0])));
    }
    t.finish(1.0);
}
