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

/// Someone who worked on a title, for a player's detail page.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Person {
    /// The metadata source's id for the person: TMDB for films, TVmaze for series.
    pub id: u64,
    pub name: String,
    /// The character played, or the job done.
    pub role: String,
    /// actor, director or writer.
    pub kind: String,
    pub photo: Option<String>,
}

/// The set a film belongs to: a trilogy, a franchise.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Collection {
    pub id: u32,
    pub name: String,
    pub poster: Option<String>,
    pub fanart: Option<String>,
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
    // Shown by players, not used in deciding anything.
    #[serde(default)]
    pub tagline: String,
    /// Audience score out of ten.
    #[serde(default)]
    pub rating: Option<f32>,
    /// Age rating: "PG-13", "R".
    #[serde(default)]
    pub certification: Option<String>,
    #[serde(default)]
    pub people: Vec<Person>,
    #[serde(default)]
    pub collection: Option<Collection>,
    /// When the fields above were last fetched. Zero for titles added before they were kept.
    #[serde(default)]
    pub details_at: i64,
    /// What lies in the title's folder beside the film or the episodes: extras, artwork,
    /// theme music and a media server's notes. Read from disk, never written.
    #[serde(default)]
    pub local: crate::local::Local,
    /// A film that is posted as its episodes: an OVA the metadata source lists as one film.
    /// Learned when a download for it turned out to be a single episode, so that the other
    /// episodes are not downloaded one after another to find out the same.
    #[serde(default)]
    pub in_episodes: bool,
    /// The same work as the series source lists it, when it does: what the film would become
    /// if it were collected as episodes.
    #[serde(default)]
    pub as_series: Option<SeriesTwin>,
    /// Whether the series source has been asked about this film yet.
    #[serde(default)]
    pub series_checked: bool,
    /// The title's entries on AniList, when it is anime: one for a film, one a season for a
    /// series, first season first.
    #[serde(default)]
    pub anilist: Vec<u32>,
    /// When the anime sources were last asked about it.
    #[serde(default)]
    pub anime_checked_at: i64,
    /// Other folders that hold this title too, each on another location: a second copy of a
    /// film, or the seasons of a series the title's own folder lacks.
    #[serde(default)]
    pub also: Vec<String>,
}

impl Default for Title {
    /// A title with nothing known of it: every field as it is read when it is absent. Titles
    /// are built from this, so a new field is named only where it is given a value.
    fn default() -> Title {
        serde_json::from_value(serde_json::json!({"kind": "movie", "title": ""})).expect("an empty title reads")
    }
}

/// A film's listing as a series.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SeriesTwin {
    pub tvmaze_id: u32,
    pub title: String,
    pub episodes: u32,
    /// How long the episodes run together, in minutes; 0 when the source does not say.
    pub minutes: u32,
}

impl Title {
    /// The folder a file of this title is in: the title's own, or the copy's.
    pub fn folder_of<'a>(&'a self, f: &'a MediaFile) -> &'a str {
        f.base.as_deref().unwrap_or(&self.path)
    }

    /// Where a file of this title is.
    pub fn file_path(&self, f: &MediaFile) -> std::path::PathBuf {
        std::path::Path::new(self.folder_of(f)).join(&f.rel_path)
    }

    /// A picture in the title's own folder, written as an address: "file:" and its path, then
    /// its size and date, so the address changes when the picture does.
    fn local_art(&self, kind: crate::local::ArtKind) -> Option<String> {
        self.local.art(kind).map(|a| crate::local::art_address(&self.path, a))
    }

    /// Where the poster comes from: one the user keeps in the title's folder, before the one
    /// the metadata source offers.
    pub fn poster_source(&self) -> Option<String> {
        self.local_art(crate::local::ArtKind::Poster).or_else(|| self.poster.clone())
    }

    /// The same for the wide picture behind the title.
    pub fn fanart_source(&self) -> Option<String> {
        self.local_art(crate::local::ArtKind::Backdrop).or_else(|| self.fanart.clone())
    }
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
    /// A frame from the episode.
    #[serde(default)]
    pub still: Option<String>,
    #[serde(default)]
    pub rating: Option<f32>,
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
    /// The folder the file is in, when that is not the title's own: a copy of the title kept
    /// on another location. Spool lists and plays such a file and never changes it.
    #[serde(default)]
    pub base: Option<String>,
    /// The file this is a compact version of: a smaller copy Spool made of it. See
    /// [`crate::compact`]. It is kept as a version beside the original and never taken for it.
    #[serde(default)]
    pub compact_of: Option<i64>,
}

/// Where one row of the library stands in the order of changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// title, episode, file, activity, or watched_movie, watched_series, watched_episode.
    pub kind: String,
    pub id: i64,
    pub title_id: i64,
    pub rev: i64,
    pub removed: bool,
    pub at: i64,
}

/// How far someone has got with a film or an episode.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Watched {
    /// Seconds in, when stopped part-way.
    pub position: f64,
    pub played: bool,
    pub play_count: u32,
    pub favorite: bool,
    pub last_played_at: i64,
    pub updated_at: i64,
}

/// Someone who plays: a name and a password of their own, and what they have watched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: i64,
    pub name: String,
    /// Kept as a hash; empty when the account has none. Never handed out.
    #[serde(skip_serializing, default)]
    pub password: String,
    /// One that is not may not sign in, and its players are turned away.
    pub enabled: bool,
    /// What it asks for is added at once, without waiting for anyone to say so.
    pub adds: bool,
    /// Away from home it is shown only what has a compact version, and given that version.
    pub compact_away: bool,
    pub created_at: i64,
}

/// Something someone asked to have in the library.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    pub id: i64,
    pub account_id: i64,
    pub kind: Kind,
    pub tmdb_id: Option<u32>,
    pub tvmaze_id: Option<u32>,
    /// A series' other id, which is what the library knows one by when it came from elsewhere.
    pub tvdb_id: Option<u32>,
    pub title: String,
    pub year: u32,
    pub poster: String,
    /// "asked", "added" or "declined".
    pub state: String,
    /// The title it became, once added.
    pub title_id: Option<i64>,
    pub asked_at: i64,
    pub answered_at: i64,
}

/// A player that is signed in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    /// What the player holds to show that it is signed in. Never handed out.
    #[serde(skip_serializing, default)]
    pub token: String,
    pub account_id: i64,
    /// What it said it was: "Living Room", and the program, "Infuse".
    pub name: String,
    pub client: String,
    pub created_at: i64,
    pub seen_at: i64,
}

/// One sitting: someone played something, on something, for a while.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Play {
    pub id: i64,
    pub account_id: i64,
    /// "movie" or "episode", and its id.
    pub kind: String,
    pub ref_id: i64,
    pub title_id: i64,
    pub title: String,
    pub device: String,
    /// The linked machine it came from; empty when it came through this Spool.
    pub source: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub seconds: f64,
    pub position: f64,
    pub finished: bool,
}

/// What one account has played, added up.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayTotals {
    pub plays: u32,
    pub seconds: f64,
    pub seconds_lately: f64,
    pub last_at: i64,
    pub titles: u32,
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
