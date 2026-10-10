//! The records Spool keeps.

use serde::{Deserialize, Serialize};
use spool_core::mediainfo::MediaInfo;
use spool_core::QualityModel;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Movie,
    Series,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Movie => "movie",
            Kind::Series => "series",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "movie" => Some(Kind::Movie),
            "series" => Some(Kind::Series),
            _ => None,
        }
    }
    pub fn flavor(self) -> spool_core::Flavor {
        match self {
            Kind::Movie => spool_core::Flavor::Movie,
            Kind::Series => spool_core::Flavor::Tv,
        }
    }
    /// The profile kind this title uses.
    pub fn profile_kind(self) -> &'static str {
        match self {
            Kind::Movie => "movie",
            Kind::Series => "tv",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SeasonInfo {
    pub number: u32,
    pub monitored: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Title {
    #[serde(default)]
    pub id: i64,
    pub kind: Kind,
    pub title: String,
    #[serde(default)]
    pub sort_title: String,
    #[serde(default)]
    pub year: u32,
    #[serde(default)]
    pub overview: String,
    /// Movie: announced, in_cinemas, released. Series: continuing, ended, upcoming.
    #[serde(default)]
    pub status: String,
    /// Minutes.
    #[serde(default)]
    pub runtime: u32,
    #[serde(default)]
    pub tmdb_id: Option<u32>,
    #[serde(default)]
    pub tvdb_id: Option<u32>,
    #[serde(default)]
    pub tvmaze_id: Option<u32>,
    #[serde(default)]
    pub imdb_id: Option<String>,
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub fanart: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub alt_titles: Vec<String>,
    #[serde(default)]
    pub alt_years: Vec<u32>,
    #[serde(default = "yes")]
    pub monitored: bool,
    #[serde(default)]
    pub profile_id: i64,
    /// Absolute folder for this title on the media volume.
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub added_at: i64,
    // movie
    #[serde(default)]
    pub in_cinemas: Option<String>,
    #[serde(default)]
    pub digital_release: Option<String>,
    #[serde(default)]
    pub physical_release: Option<String>,
    /// announced, in_cinemas or released.
    #[serde(default = "released")]
    pub minimum_availability: String,
    #[serde(default)]
    pub studio: Option<String>,
    // series
    #[serde(default)]
    pub network: Option<String>,
    #[serde(default)]
    pub seasons: Vec<SeasonInfo>,
    #[serde(default = "yes")]
    pub season_folder: bool,
    #[serde(default)]
    pub first_aired: Option<String>,
    /// The language the title was made in, as the release parser names it ("japanese").
    #[serde(default)]
    pub original_language: Option<String>,
    #[serde(default)]
    pub last_search_at: i64,
    #[serde(default)]
    pub last_refresh_at: i64,
}

fn yes() -> bool {
    true
}
fn released() -> String {
    "released".into()
}

impl Title {
    pub fn key(&self) -> spool_core::matching::TitleKey {
        spool_core::matching::TitleKey {
            id: self.id,
            title: self.title.clone(),
            alt_titles: self.alt_titles.clone(),
            year: self.year,
            alt_years: self.alt_years.clone(),
            imdb_id: self.imdb_id.clone(),
            tmdb_id: self.tmdb_id,
        }
    }

    /// Whether a movie has reached the point its minimum availability asks for.
    pub fn movie_available(&self, today: chrono::NaiveDate) -> bool {
        let date = |s: &Option<String>| s.as_deref().and_then(|d| d.get(..10)).and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
        let cinemas = date(&self.in_cinemas);
        let home = [date(&self.digital_release), date(&self.physical_release)].into_iter().flatten().min();
        match self.minimum_availability.as_str() {
            "announced" => true,
            "in_cinemas" => cinemas.is_some_and(|d| d <= today) || home.is_some_and(|d| d <= today),
            _ => match (home, cinemas) {
                (Some(h), _) => h <= today,
                // No home release date known: assume 90 days after cinemas.
                (None, Some(c)) => c + chrono::Duration::days(90) <= today,
                (None, None) => self.year > 0 && (self.year as i32) < chrono::Datelike::year(&today),
            },
        }
    }

    pub fn season_monitored(&self, season: u32) -> bool {
        self.seasons.iter().find(|s| s.number == season).map(|s| s.monitored).unwrap_or(true)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Episode {
    #[serde(default)]
    pub id: i64,
    pub title_id: i64,
    pub season: u32,
    pub episode: u32,
    #[serde(default)]
    pub absolute: Option<u32>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub overview: String,
    /// Local air date, YYYY-MM-DD.
    #[serde(default)]
    pub air_date: Option<String>,
    #[serde(default)]
    pub air_date_utc: Option<String>,
    #[serde(default)]
    pub runtime: u32,
    #[serde(default = "yes")]
    pub monitored: bool,
    #[serde(default)]
    pub file_id: Option<i64>,
}

impl Episode {
    pub fn has_aired(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        if let Some(utc) = self.air_date_utc.as_deref().and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok()) {
            return utc <= now;
        }
        self.air_date
            .as_deref()
            .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .is_some_and(|d| d < now.date_naive())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaFile {
    #[serde(default)]
    pub id: i64,
    pub title_id: i64,
    /// Path relative to the title folder.
    pub rel_path: String,
    #[serde(default)]
    pub size: u64,
    pub quality: QualityModel,
    #[serde(default)]
    pub release_group: Option<String>,
    #[serde(default)]
    pub edition: String,
    #[serde(default)]
    pub media_info: Option<MediaInfo>,
    /// Release name the file came from.
    #[serde(default)]
    pub scene_name: Option<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub added_at: i64,
    /// A further version of a film, kept beside the main one on purpose: a different cut, or
    /// a 4K copy next to a small one. Upgrades replace the main file and leave these alone.
    #[serde(default)]
    pub extra: bool,
    /// Subtitle files kept beside the video.
    #[serde(default)]
    pub subtitles: Vec<crate::subs::Sidecar>,
}

/// One release as an indexer reported it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Release {
    pub guid: String,
    pub title: String,
    pub indexer_id: i64,
    pub indexer: String,
    /// URL the NZB is fetched from.
    pub link: String,
    pub info_url: String,
    pub size: u64,
    /// Unix seconds.
    pub published: i64,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<u32>,
    pub indexer_priority: i32,
}

impl Release {
    pub fn age_days(&self) -> f64 {
        if self.published <= 0 {
            return 0.0;
        }
        ((chrono::Utc::now().timestamp() - self.published) as f64 / 86400.0).max(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcqState {
    /// Sent to the downloader.
    Downloading,
    /// Downloaded; being identified and moved into the library.
    Importing,
    Imported,
    Failed,
    /// Downloaded, but Spool could not decide what it is or where it goes.
    ImportBlocked,
    Cancelled,
}

impl AcqState {
    pub fn as_str(self) -> &'static str {
        match self {
            AcqState::Downloading => "downloading",
            AcqState::Importing => "importing",
            AcqState::Imported => "imported",
            AcqState::Failed => "failed",
            AcqState::ImportBlocked => "import_blocked",
            AcqState::Cancelled => "cancelled",
        }
    }
    pub fn is_active(self) -> bool {
        matches!(self, AcqState::Downloading | AcqState::Importing | AcqState::ImportBlocked)
    }
}

/// One attempt to get one release into the library, from grab to import.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Acquisition {
    #[serde(default)]
    pub id: i64,
    pub title_id: i64,
    #[serde(default)]
    pub episode_ids: Vec<i64>,
    pub state: AcqState,
    pub release: Release,
    pub quality: QualityModel,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub output_path: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// Why this release was chosen, in a sentence.
    #[serde(default)]
    pub reason: String,
    /// Chosen knowing it may be lower quality than the file it replaces: a smaller copy, or a
    /// release picked by hand. The import does not question it.
    #[serde(default)]
    pub replace_better: bool,
    /// Kept beside the film's existing file as another version, replacing nothing.
    #[serde(default)]
    pub extra_version: bool,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

/// A recorded verdict on one release for one title.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecisionRecord {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub ts: i64,
    pub title_id: i64,
    pub release: Release,
    pub quality: QualityModel,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub release_group: Option<String>,
    /// What the release covers, for display: "S01E02", "Season 1", "".
    #[serde(default)]
    pub covers: String,
    #[serde(default)]
    pub episode_ids: Vec<i64>,
    pub accepted: bool,
    pub rejections: Vec<spool_core::decision::Rejection>,
    /// rss, search or manual.
    #[serde(default)]
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Indexer {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "movie_cats")]
    pub movie_categories: Vec<u32>,
    #[serde(default = "tv_cats")]
    pub tv_categories: Vec<u32>,
    #[serde(default = "yes")]
    pub enable_rss: bool,
    #[serde(default = "yes")]
    pub enable_search: bool,
    #[serde(default = "priority")]
    pub priority: i32,
    #[serde(default = "yes")]
    pub movies: bool,
    #[serde(default = "yes")]
    pub tv: bool,
    /// Daily allowance of API requests and of NZB downloads. Zero means go by what the indexer
    /// reports, or no limit if it reports none.
    #[serde(default)]
    pub daily_requests: u32,
    #[serde(default)]
    pub daily_grabs: u32,
}

/// Where an indexer stands against its daily allowance.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IndexerBudget {
    pub requests: u32,
    pub grabs: u32,
    /// Zero means unlimited as far as Spool knows.
    pub request_limit: u32,
    pub grab_limit: u32,
    /// "set" when the limits come from settings, "reported" when from the indexer itself.
    pub source: Option<String>,
}

impl IndexerBudget {
    fn used(part: u32, limit: u32, percent: u32) -> bool {
        limit > 0 && part as u64 * 100 >= limit as u64 * percent as u64
    }
    /// Too little left to spend on searches nobody asked for.
    pub fn requests_low(&self) -> bool {
        Self::used(self.requests, self.request_limit, 90)
    }
    pub fn grabs_spent(&self) -> bool {
        Self::used(self.grabs, self.grab_limit, 100)
    }
    /// Running down: equal releases are better taken from another indexer.
    pub fn grabs_low(&self) -> bool {
        Self::used(self.grabs, self.grab_limit, 70)
    }
}

fn movie_cats() -> Vec<u32> {
    vec![2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060]
}
fn tv_cats() -> Vec<u32> {
    vec![5030, 5040]
}
fn priority() -> i32 {
    25
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attention {
    pub id: i64,
    pub ts: i64,
    /// import_blocked, download_failed, unmatched_file, config.
    pub kind: String,
    pub title_id: Option<i64>,
    pub acquisition_id: Option<i64>,
    pub message: String,
    pub data: serde_json::Value,
    pub resolved_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub ts: i64,
    pub title_id: Option<i64>,
    /// grabbed, would_grab, imported, download_failed, file_deleted, upgraded.
    pub kind: String,
    pub data: serde_json::Value,
}

/// Episodes for absolute numbers ("Show - 27"), counting regular episodes across seasons in
/// order, as anime releases do. None unless every number maps to an episode.
pub fn episodes_by_absolute<'a>(episodes: &'a [Episode], numbers: &[u32]) -> Option<Vec<&'a Episode>> {
    let mut regular: Vec<&Episode> = episodes.iter().filter(|e| e.season > 0).collect();
    regular.sort_by_key(|e| (e.season, e.episode));
    let found: Vec<&Episode> = numbers.iter().filter_map(|n| regular.get((*n as usize).checked_sub(1)?).copied()).collect();
    (!found.is_empty() && found.len() == numbers.len()).then_some(found)
}
