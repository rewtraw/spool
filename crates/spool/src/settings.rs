//! User-editable configuration, stored in the settings table.

use serde::{Deserialize, Serialize};
use spool_core::decision::ProperPolicy;
use spool_core::naming::NamingConfig;
use spool_core::profile::SizeLimit;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Watch and decide, but never download or touch the library.
    #[default]
    Shadow,
    Active,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub mode: Mode,
    pub movie_root: String,
    pub series_root: String,
    /// The volume that must be mounted before Spool reads or writes media. Empty disables the check.
    pub required_volume: String,
    pub downloads_dir: String,
    /// Days a replaced or deleted file is kept before it is removed. Zero deletes at once.
    pub recycle_days: u32,
    pub retention_days: u32,
    pub proper_policy: ProperPolicy,
    /// Language releases must be in, lower-case. Empty accepts any.
    pub language: String,
    pub rss_interval_minutes: u32,
    /// How many missing titles to search per backlog run. Keeps indexer API use predictable.
    pub backlog_batch: u32,
    pub backlog_interval_minutes: u32,
    pub tmdb_api_key: String,
    pub plex_url: String,
    pub plex_token: String,
    pub ffprobe_path: String,
    /// When set, the API and web app require it.
    pub password: String,
    pub api_key: String,
    /// A second key that may only use the read-only MCP tools. Empty disables it.
    pub mcp_read_key: String,
    /// A second folder, ideally on another disk, that also gets each nightly database backup.
    pub backup_dir: String,
    /// Languages every file should have subtitles in, as codes or names: "en", "en, es".
    pub subtitle_languages: String,
    /// Look for missing subtitles without being asked.
    pub subtitles_auto: bool,
    pub opensubtitles_api_key: String,
    pub opensubtitles_username: String,
    pub opensubtitles_password: String,
    /// Where the OpenSubtitles API is, when not the usual place.
    pub opensubtitles_url: String,
    /// Spool stops starting downloads when the downloads disk would drop below this many
    /// gigabytes free once the download has finished.
    pub min_free_gb: u32,
    /// How far past the free space Spool may queue downloads of its own accord, in gigabytes.
    /// They wait, without downloading, until there is room.
    pub space_wait_gb: u32,
    /// Unpack uncompressed RAR posts while they download.
    pub direct_unpack: bool,
    /// Finished downloads verified and unpacked at once. Takes effect when Spool restarts.
    pub post_parallel: u32,
    /// How many runner-up releases to save alongside each one Spool downloads.
    pub archive_runner_ups: u32,
    /// Profiles preselected when adding a title. Zero means the first one.
    pub default_movie_profile: i64,
    pub default_series_profile: i64,
}

impl Default for General {
    fn default() -> Self {
        General {
            mode: Mode::Shadow,
            movie_root: String::new(),
            series_root: String::new(),
            required_volume: String::new(),
            downloads_dir: String::new(),
            recycle_days: 7,
            retention_days: 0,
            proper_policy: ProperPolicy::PreferAndUpgrade,
            language: "english".into(),
            rss_interval_minutes: 15,
            backlog_batch: 5,
            backlog_interval_minutes: 360,
            tmdb_api_key: String::new(),
            plex_url: String::new(),
            plex_token: String::new(),
            ffprobe_path: String::new(),
            password: String::new(),
            api_key: String::new(),
            mcp_read_key: String::new(),
            backup_dir: String::new(),
            subtitle_languages: String::new(),
            subtitles_auto: false,
            opensubtitles_api_key: String::new(),
            opensubtitles_username: String::new(),
            opensubtitles_password: String::new(),
            opensubtitles_url: String::new(),
            min_free_gb: 20,
            space_wait_gb: 100,
            direct_unpack: true,
            post_parallel: 2,
            archive_runner_ups: 2,
            default_movie_profile: 0,
            default_series_profile: 0,
        }
    }
}

/// Size limits keyed by "movie:bluray-1080p" / "tv:webdl-720p".
pub type SizeLimits = HashMap<String, SizeLimit>;

#[derive(Clone)]
pub struct Settings {
    db: crate::db::Db,
}

impl Settings {
    pub fn new(db: crate::db::Db) -> Settings {
        Settings { db }
    }
    pub fn general(&self) -> General {
        self.db.get_setting("general")
    }
    pub fn set_general(&self, g: &General) -> anyhow::Result<()> {
        self.db.set_setting("general", g)
    }
    pub fn naming(&self) -> NamingConfig {
        self.db.get_setting("naming")
    }
    pub fn set_naming(&self, n: &NamingConfig) -> anyhow::Result<()> {
        self.db.set_setting("naming", n)
    }
    pub fn size_limits(&self) -> SizeLimits {
        self.db.get_setting("size_limits")
    }
    pub fn set_size_limits(&self, s: &SizeLimits) -> anyhow::Result<()> {
        self.db.set_setting("size_limits", s)
    }
    pub fn servers(&self) -> Vec<spool_nntp::ServerConfig> {
        self.db.get_setting("usenet_servers")
    }
    pub fn set_servers(&self, s: &[spool_nntp::ServerConfig]) -> anyhow::Result<()> {
        self.db.set_setting("usenet_servers", &s.to_vec())
    }
    pub fn speed_limit(&self) -> u64 {
        self.db.get_setting("speed_limit")
    }
    pub fn set_speed_limit(&self, v: u64) -> anyhow::Result<()> {
        self.db.set_setting("speed_limit", &v)
    }
}

/// A random key for API and MCP access.
pub fn new_key() -> String {
    use std::io::Read;
    let mut buf = [0u8; 24];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).is_err() {
        // No random device: fall back to something unpredictable enough for a home network.
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) ^ ((std::process::id() as u128) << 64);
        buf[..16].copy_from_slice(&n.to_le_bytes());
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}
