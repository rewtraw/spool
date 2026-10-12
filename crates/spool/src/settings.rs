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

/// A further place the library lives, beside the main one. See [`crate::locations`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Location {
    /// Given when the location is added and never reused. The main location is zero.
    pub id: u32,
    pub name: String,
    pub movie_root: String,
    pub series_root: String,
    /// The volume that must be mounted for the location to count as there. Empty means the
    /// folders only have to be readable.
    pub volume: String,
    /// Spool may add, replace and delete files here. When false it only reads.
    pub manage: bool,
    /// The profile new films and new series get here; 0 for the usual one. A disk with room
    /// to spare and a small one want different things.
    pub movie_profile: i64,
    pub series_profile: i64,
}

/// A helper on the machine that holds some of the disks. See [`crate::remote`].
/// Another Spool this one is linked to: a helper on the machine that holds some of the
/// disks. See [`crate::link`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LinkConf {
    /// Given when the link is added and never reused.
    pub id: u32,
    pub name: String,
    /// Where it answers this Spool, such as `http://nas:7978`.
    pub url: String,
    pub key: String,
    /// The folder this machine sees the other machine's disks under, and the folder the
    /// other machine sees the same thing under. A path is turned from one to the other by
    /// swapping these at its start. Left empty, Spool finds the two for itself from the
    /// folders the other machine shares and the shares mounted here.
    pub here: String,
    pub there: String,
    /// Players on the same network as it are sent to it for a file it holds, and players
    /// away from home to its Tailscale address when it has one.
    pub play_near: bool,
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
    /// Whether Spool writes its collections into Plex, as collection tags on the items.
    pub plex_collections: bool,
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
    /// Serve the library to players that speak Jellyfin's protocol, such as Infuse.
    pub players_enabled: bool,
    /// The port players connect to. Jellyfin's own is 8096.
    pub players_port: u32,
    /// What a player signs in with. The password is kept as a hash, like the app's.
    pub players_username: String,
    pub players_password: String,
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
    /// Further places the library lives, beside the folders above.
    pub locations: Vec<Location>,
    /// The location new titles are put in; zero is the main one, which is also used whenever
    /// the chosen one cannot take them.
    pub new_titles_location: u32,
    /// The Spools this one is linked to. They were called helpers, and settings saved under
    /// that name are read as they are.
    pub links: Vec<LinkConf>,
    /// The highest number a link has been given, so that none is ever given again.
    pub link_seq: u32,
    /// How compact versions are made.
    pub compact: crate::compact::Compact,
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
            plex_collections: false,
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
            players_enabled: false,
            players_port: 8096,
            players_username: "spool".into(),
            players_password: String::new(),
            min_free_gb: 20,
            space_wait_gb: 100,
            direct_unpack: true,
            post_parallel: 2,
            archive_runner_ups: 2,
            default_movie_profile: 0,
            default_series_profile: 0,
            locations: vec![],
            new_titles_location: 0,
            links: vec![],
            link_seq: 0,
            compact: Default::default(),
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
    /// Save the general settings. Passwords are never stored as typed: whatever arrives in
    /// the clear is replaced by a salted hash on the way in.
    pub fn set_general(&self, g: &General) -> anyhow::Result<()> {
        let mut g = g.clone();
        for password in [&mut g.password, &mut g.players_password] {
            if !password.is_empty() && !is_hashed(password) {
                *password = hash_password(password)?;
            }
        }
        self.db.set_setting("general", &g)
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

const HASH_PREFIX: &str = "spool-argon2id$";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn argon(password: &str, salt: &str) -> anyhow::Result<String> {
    let mut out = [0u8; 32];
    argon2::Argon2::default().hash_password_into(password.as_bytes(), salt.as_bytes(), &mut out).map_err(|e| anyhow::anyhow!("hashing the password: {e}"))?;
    Ok(hex(&out))
}

/// Whether a stored password is a hash made here, as opposed to one typed in the clear.
pub fn is_hashed(stored: &str) -> bool {
    stored.strip_prefix(HASH_PREFIX).and_then(|rest| rest.split_once('$')).is_some_and(|(salt, hash)| salt.len() == 32 && hash.len() == 64 && rest_is_hex(salt) && rest_is_hex(hash))
}

fn rest_is_hex(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A salted Argon2id hash of a password, in the form kept in the settings.
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = &new_key()[..32];
    Ok(format!("{HASH_PREFIX}{salt}${}", argon(password, salt)?))
}

/// Whether what was typed is the password that was stored. A password saved before hashing
/// came in is compared as it is, and is replaced by a hash the next time Spool starts.
pub fn verify_password(stored: &str, given: &str) -> bool {
    let same = |a: &str, b: &str| a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0;
    match stored.strip_prefix(HASH_PREFIX).and_then(|rest| rest.split_once('$')).filter(|_| is_hashed(stored)) {
        Some((salt, hash)) => argon(given, salt).is_ok_and(|h| same(&h, hash)),
        None => same(stored, given),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_hashed_and_still_verify() {
        let h = hash_password("open sesame").unwrap();
        assert!(is_hashed(&h) && !h.contains("sesame"));
        assert!(verify_password(&h, "open sesame"));
        assert!(!verify_password(&h, "open sesame!") && !verify_password(&h, ""));
        // Two hashes of one password differ, so equal passwords cannot be spotted.
        assert_ne!(h, hash_password("open sesame").unwrap());
        // One saved before hashing still works until it is upgraded.
        assert!(!is_hashed("plain") && verify_password("plain", "plain") && !verify_password("plain", "other"));
        assert!(!is_hashed("spool-argon2id$short$nothex"));
    }
}
