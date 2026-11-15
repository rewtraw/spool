//! User-editable configuration, stored in the settings table.

use serde::{Deserialize, Serialize};
use spool_core::naming::NamingConfig;

/// A further place the library lives, beside the main one. See [`crate::locations`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Location {
    /// Given when the location is added and never reused. The main location is zero.
    pub id: u32,
    pub name: String,
    pub movie_root: String,
    pub series_root: String,
    /// Where its music is, one folder an album; empty when it holds none. See [`crate::music`].
    pub music_root: String,
    /// Where its audiobooks are, a folder for each author and in it one for each book; empty
    /// when it holds none. See [`crate::books::audio`].
    pub audiobook_root: String,
    /// Where videos from YouTube and other sites go, a folder for each channel; empty when it
    /// holds none. See [`crate::videos`].
    pub video_root: String,
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

impl Location {
    /// A main location with no folders yet.
    pub fn main() -> Location {
        Location { id: 0, name: "Library".into(), manage: true, ..Default::default() }
    }
}

/// One folder on a linked machine: where it is there, and where the same folder is here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Folder {
    pub here: String,
    pub there: String,
}

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
    /// Each folder the other machine works in, as it knows it and as this machine sees
    /// it. Found by Spool, which keeps them here so that it knows whose a path is before
    /// it has looked again; never typed in.
    pub folders: Vec<Folder>,
    /// Players on the same network as it are sent to it for a file it holds, and players
    /// away from home to its Tailscale address when it has one.
    pub play_near: bool,
    /// When it is given films to make compact versions of, if it offers to make them.
    pub make: MakeWhen,
}

impl General {
    /// The main location: the first place the library is, always managed, and where new
    /// titles go when the place chosen for them cannot take them. A new Spool has one with
    /// no folders yet.
    pub fn main(&self) -> Location {
        self.locations.iter().find(|l| l.id == 0).cloned().unwrap_or_else(Location::main)
    }

    /// The main location, to change, made first if there is none.
    pub fn main_mut(&mut self) -> &mut Location {
        if !self.locations.iter().any(|l| l.id == 0) {
            self.locations.insert(0, Location::main());
        }
        self.locations.iter_mut().find(|l| l.id == 0).expect("just made")
    }

    /// Whether Plex is to be used: it is turned on, and there is an address and a token for it.
    pub fn plex_ready(&self) -> bool {
        self.plex_on && !self.plex_url.is_empty() && !self.plex_token.is_empty()
    }
}

/// When a linked machine is given compact versions to make.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MakeWhen {
    #[default]
    Always,
    /// From eleven at night until seven in the morning. One begun before seven is finished.
    Night,
    Never,
}

impl MakeWhen {
    pub fn now(self) -> bool {
        use chrono::Timelike;
        match self {
            MakeWhen::Always => true,
            MakeWhen::Night => !(7..23).contains(&chrono::Local::now().hour()),
            MakeWhen::Never => false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Paused: nothing is searched for or fetched by itself, and downloads wait. What Spool
    /// would have fetched is noted; what someone asks for is put in line, to start on resuming.
    pub paused: bool,
    /// How yt-dlp signs in to sites that want it, Instagram above all: the name of a browser on
    /// this machine to borrow the sign-in of ("safari", "chrome", "firefox"), or the full path of
    /// a cookies.txt file. Empty signs in nowhere. See [`crate::videos`].
    pub video_cookies: String,
    pub downloads_dir: String,
    /// Language releases must be in, lower-case. Empty accepts any.
    pub language: String,
    pub tmdb_api_key: String,
    pub plex_url: String,
    pub plex_token: String,
    /// Whether Plex is used at all. Off, its address and token are kept and nothing is
    /// asked of it or shown from it.
    pub plex_on: bool,
    /// Whether Spool writes its collections into Plex, as collection tags on the items.
    pub plex_collections: bool,
    /// A second folder, ideally on another disk, that also gets each nightly database backup.
    pub backup_dir: String,
    /// Languages every file should have subtitles in, as codes or names: "en", "en, es".
    pub subtitle_languages: String,
    /// Look for missing subtitles without being asked.
    pub subtitles_auto: bool,
    pub opensubtitles_api_key: String,
    pub opensubtitles_username: String,
    pub opensubtitles_password: String,
    /// Serve the library to players that speak Jellyfin's protocol, such as Infuse.
    pub players_enabled: bool,
    /// Spool stops starting downloads when the downloads disk would drop below this many
    /// gigabytes free once the download has finished.
    pub min_free_gb: u32,
    /// Profiles preselected when adding a title. Zero means the first one.
    pub default_movie_profile: i64,
    pub default_series_profile: i64,
    /// Every place the library lives: the main one (see [`General::main`]) and any others.
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
    /// How books are got and where they go.
    pub books: crate::books::BookSettings,
}

impl Default for General {
    fn default() -> Self {
        General {
            paused: false,
            video_cookies: String::new(),
            downloads_dir: String::new(),
            language: "english".into(),
            tmdb_api_key: String::new(),
            plex_url: String::new(),
            plex_token: String::new(),
            plex_on: true,
            plex_collections: false,
            backup_dir: String::new(),
            subtitle_languages: String::new(),
            subtitles_auto: false,
            opensubtitles_api_key: String::new(),
            opensubtitles_username: String::new(),
            opensubtitles_password: String::new(),
            players_enabled: false,
            min_free_gb: 20,
            default_movie_profile: 0,
            default_series_profile: 0,
            locations: vec![],
            new_titles_location: 0,
            links: vec![],
            link_seq: 0,
            compact: Default::default(),
            books: Default::default(),
        }
    }
}

/// The keys that let other programs in, kept apart from the settings: they are never sent to
/// a browser with them, and nothing typed into Settings changes them.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Access {
    /// Full use of the API and of MCP. Made at first start.
    pub api_key: String,
    /// The read-only MCP tools alone. Empty turns it off.
    pub mcp_read_key: String,
    /// Signs the tickets that let a player on this device play one file. Made at first start.
    pub ticket_key: String,
}

#[derive(Clone)]
pub struct Settings {
    db: crate::db::Db,
    /// Held while the settings are read, changed and written, so two changes made at once
    /// each keep the other's.
    writing: std::sync::Arc<parking_lot::Mutex<()>>,
}

impl Settings {
    pub fn new(db: crate::db::Db) -> Settings {
        Settings { db, writing: Default::default() }
    }
    pub fn general(&self) -> General {
        match self.db.get_setting_checked::<General>("general") {
            Ok(g) => g.unwrap_or_default(),
            Err(e) => {
                // Said loudly rather than passed over: the defaults stand in until it is put right.
                tracing::error!(error = %e, "the saved settings cannot be read; using the defaults");
                General::default()
            }
        }
    }
    /// Write the settings whole. Changes go through [`Settings::update`].
    pub fn set_general(&self, g: &General) -> anyhow::Result<()> {
        let _held = self.writing.lock();
        self.db.set_setting("general", g)
    }
    /// Change the settings as they stand when the change is made, and write them, with no
    /// other change able to come between the reading and the writing.
    pub fn update<T>(&self, change: impl FnOnce(&mut General) -> anyhow::Result<T>) -> anyhow::Result<(General, T)> {
        let _held = self.writing.lock();
        let mut g = self.general();
        let out = change(&mut g)?;
        self.db.set_setting("general", &g)?;
        Ok((g, out))
    }
    pub fn access(&self) -> Access {
        self.db.get_setting("access")
    }
    pub fn set_access(&self, a: &Access) -> anyhow::Result<()> {
        self.db.set_setting("access", a)
    }
    pub fn naming(&self) -> NamingConfig {
        self.db.get_setting("naming")
    }
    pub fn set_naming(&self, n: &NamingConfig) -> anyhow::Result<()> {
        self.db.set_setting("naming", n)
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

/// What used to be settings, and is now simply how Spool works.
pub mod fixed {
    /// How often the indexers' feeds of new releases are read.
    pub const RSS_MINUTES: i64 = 15;
    /// How often missing titles are searched for, and how many each time: few enough to keep
    /// within any indexer's allowance, which is guarded besides.
    pub const BACKLOG_MINUTES: i64 = 360;
    pub const BACKLOG_BATCH: usize = 5;
    /// Days a replaced or deleted file waits in the recycle folder.
    pub const RECYCLE_DAYS: i64 = 7;
    /// How far past the free space downloads Spool finds by itself may queue, waiting for room.
    pub const SPACE_WAIT_BYTES: u64 = 100 << 30;
    /// Runner-up releases saved to the archive beside each one downloaded.
    pub const RUNNER_UPS: usize = 2;
    /// Finished downloads verified and unpacked at once.
    pub const POST_PARALLEL: usize = 2;

    /// The port players connect to: Jellyfin's own, unless `SPOOL_PLAYERS_PORT` names another
    /// (the demo does, to keep out of the way of a real server).
    pub fn players_port() -> u16 {
        std::env::var("SPOOL_PLAYERS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8096)
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
    #[test]
    fn settings_saved_before_plex_could_be_turned_off_have_it_on() {
        let g: super::General = serde_json::from_str(r#"{"plex_url":"http://plex:32400","plex_token":"t"}"#).unwrap();
        assert!(g.plex_on && g.plex_ready());
        let off: super::General = serde_json::from_str(r#"{"plex_url":"http://plex:32400","plex_token":"t","plex_on":false}"#).unwrap();
        assert!(!off.plex_ready() && !off.plex_token.is_empty());
    }

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
