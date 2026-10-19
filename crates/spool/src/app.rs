//! Shared application state.

use crate::db::Db;
use crate::models::Play;
use crate::metadata::Metadata;
use crate::settings::{Mode, Settings};
use parking_lot::Mutex;
use serde::Serialize;
use spool_nntp::{Engine, EngineConfig, JobStatus};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Pushed to connected clients as things change.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Job { job: JobStatus },
    Acquisition { id: i64, title_id: i64, state: String },
    Title { id: i64 },
    Decisions { title_id: i64 },
    Attention,
    Task { name: String, running: bool, message: String },
    /// A location came back or went away, which changes how its titles are listed.
    Locations,
}

/// How long a player stays signed in without being seen, in seconds: ninety days.
pub const SIGNED_IN_FOR: i64 = 90 * 86400;

/// A player that is signed in: whose account it is on, and what it said it was.
#[derive(Clone, Debug)]
pub struct Signed {
    /// The token it holds, which is also what it is known by here.
    pub token: String,
    pub account: i64,
    pub account_name: String,
    pub device: String,
    /// When it was last noted as seen, so that this is written down once in a while and
    /// not with every request.
    pub seen: i64,
}

/// What a player says it is playing when it reports how far it has got.
pub struct Sitting<'a> {
    /// "movie" or "episode", and its id.
    pub kind: &'a str,
    pub ref_id: i64,
    pub title_id: i64,
    pub title: &'a str,
    pub position: f64,
    pub paused: bool,
    pub finished: bool,
    pub stopped: bool,
}

/// Something a player is playing.
#[derive(Clone, Debug, Serialize)]
pub struct Playing {
    pub title: String,
    pub title_id: i64,
    /// Who is playing it, and on what.
    pub account: String,
    pub device: String,
    /// The record of this sitting, once the player has said how far it has got.
    #[serde(skip)]
    play: i64,
    #[serde(skip)]
    subject: (String, i64),
    #[serde(skip)]
    reported: i64,
    /// Whether it said it was paused when it last reported.
    #[serde(skip)]
    paused: bool,
    /// The linked machine the player takes it from; empty when it is sent from here.
    pub from: String,
    pub since: i64,
    /// When the player last asked for some of it or said how far it had got.
    pub seen: i64,
    #[serde(skip)]
    path: PathBuf,
}

/// How long a player may say nothing before it is taken to have stopped, in seconds.
const QUIET: i64 = 120;

/// Passwords tried lately, by the address they came from, so a run of wrong guesses shuts
/// the door on whoever made them and not on the rest of the household.
#[derive(Default)]
pub struct Tries(Mutex<HashMap<Option<std::net::IpAddr>, (u32, i64)>>);

impl Tries {
    /// Wrong guesses allowed in a row before the door shuts.
    const TRIES: u32 = 5;
    /// For how long, in seconds, after the last of them.
    const SHUT_FOR: i64 = 60;

    /// Count one more try from here, or refuse it when there have been too many. A try is
    /// counted before the password is checked, so tries sent all at once cannot slip past.
    pub fn take(&self, from: Option<std::net::IpAddr>) -> bool {
        let now = crate::db::now();
        let mut all = self.0.lock();
        all.retain(|_, (_, last)| now - *last < Self::SHUT_FOR);
        let (n, last) = all.entry(from).or_insert((0, now));
        if *n >= Self::TRIES {
            return false;
        }
        (*n, *last) = (*n + 1, now);
        true
    }

    /// The password was right. The try is not counted, but neither are the wrong ones before
    /// it forgiven: signing in to one account must not buy more guesses at another.
    pub fn right(&self, from: Option<std::net::IpAddr>) {
        if let Some((n, _)) = self.0.lock().get_mut(&from) {
            *n = n.saturating_sub(1);
        }
    }
}

pub struct Inner {
    pub db: Db,
    pub settings: Settings,
    pub engine: Engine,
    /// The helpers on other machines, as the settings name them. See [`crate::remote`].
    pub remotes: Mutex<Vec<std::sync::Arc<crate::remote::Remote>>>,
    pub http: reqwest::Client,
    /// The same, held to addresses out on the internet: see [`public_client`].
    pub public_http: reqwest::Client,
    pub meta: Metadata,
    pub events: broadcast::Sender<Event>,
    pub data_dir: PathBuf,
    /// Acquisitions with an import in progress, so two triggers cannot import one download twice.
    pub importing: Mutex<HashSet<i64>>,
    /// One import at a time into each title, so that two downloads of one episode are not both
    /// filed as if the other were not there.
    pub title_imports: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
    /// Last request time per indexer, to space requests out.
    pub indexer_last: Mutex<HashMap<i64, std::time::Instant>>,
    /// Indexers that are failing: (consecutive failures, do not ask again before, last error).
    pub indexer_backoff: Mutex<HashMap<i64, (u32, std::time::Instant, String)>>,
    pub tasks: Mutex<HashMap<String, TaskState>>,
    /// Whether macOS lets this process read the media volume: 0 not checked, 1 waiting for an
    /// answer, 2 allowed, 3 refused. Reading a removable disk needs the user's consent, and the
    /// first read blocks until they give it.
    pub volume_access: std::sync::atomic::AtomicU8,
    /// Whether each further location could be reached when last looked at.
    pub location_state: Mutex<HashMap<u32, bool>>,
    /// Locations being looked at right now, so a share that does not answer is asked once.
    pub location_checking: Mutex<HashSet<u32>>,
    /// Locations that were there and have not answered since, with when they last did not.
    pub location_slow: Mutex<HashMap<u32, std::time::Instant>>,
    /// Counts locations coming and going, for anything that keeps a picture of the library.
    pub location_changes: std::sync::atomic::AtomicU64,
    /// The last look through a location for titles to bring in, and what came of it.
    pub adoption: Mutex<Option<crate::adopt::Adoption>>,
    /// When a player last reported that it was playing something.
    pub last_playing: std::sync::atomic::AtomicI64,
    /// The Spools heard of on the network, once this one is listening.
    pub heard: std::sync::OnceLock<crate::link::find::Heard>,
    /// What each signed-in player is playing, by the token it holds.
    pub playing: Mutex<HashMap<String, Playing>>,
    /// Whether a linked machine opened a file when last asked, and when that was, by the
    /// address it was asked at and the file: asked again only once in a while.
    pub opened: Mutex<HashMap<(String, std::path::PathBuf), (i64, bool)>>,
    /// Held while an ask is kept or added, so that two at once make one ask and one title.
    pub asking: tokio::sync::Mutex<()>,
    /// How many titles each account has had looked up in the minute that began at a time.
    pub looked_up: Mutex<HashMap<i64, (i64, u32)>>,
    /// Titles being moved between locations.
    pub moves: Mutex<crate::relocate::Moves>,
    pub encodes: Mutex<crate::compact::Encodes>,
    /// The port this Spool answers on, once it is listening: where a linked machine reads
    /// a film it is to make a compact version of.
    pub port: std::sync::OnceLock<u16>,
    /// The anime id list, once read.
    pub anime_list: Mutex<Option<Arc<crate::anime::Ids>>>,
    pub sessions: Mutex<HashSet<String>>,
    /// Wrong passwords at sign-in lately, by where they came from.
    pub login_failures: Tries,
    /// Titles last turned away for lack of disk space, with the headroom there was at the time.
    pub space_blocked: Mutex<HashMap<i64, i128>>,
    /// The OpenSubtitles sign-in in use, and when it was made.
    pub subtitle_login: Mutex<Option<(String, i64)>>,
    /// Players that have signed in, by the token each was given. Oldest first.
    /// The players that are signed in, by the token each holds. See [`App::reload_players`].
    pub players: Mutex<HashMap<String, Signed>>,
    /// Wrong passwords from players lately, by where they came from.
    pub player_failures: Tries,
    /// The last few hundred requests players made, for working out what one of them wants.
    pub player_requests: Mutex<std::collections::VecDeque<String>>,
    /// The library as last read for players, with the database's change count and the count of
    /// locations coming and going at the time.
    pub player_library: Mutex<Option<((i64, u64), Arc<crate::jellyfin::items::Data>)>>,
    /// The same, as it is shown to an account held to compact versions while it is away.
    pub player_library_compact: Mutex<Option<((i64, u64), Arc<crate::jellyfin::items::Data>)>>,
    /// The networks that count as home, when they are not to be read from this machine's own.
    pub home: Mutex<Option<Vec<crate::link::Net>>>,
    /// Where players are being served, or why they are not.
    pub player_status: Mutex<String>,
    /// Releases being sent to the downloader right now, keyed by title and release name.
    pub grabbing: Mutex<HashSet<(i64, String)>>,
    /// Jobs the user resumed by hand after Spool held them for lack of disk space.
    pub waiting_for_space: Mutex<HashSet<String>>,
    /// Limits how many artwork downloads run at once.
    pub art_fetches: tokio::sync::Semaphore,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TaskState {
    pub running: bool,
    pub last_run: i64,
    pub last_message: String,
}

#[derive(Clone)]
pub struct App(pub Arc<Inner>);

impl std::ops::Deref for App {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("Spool/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .gzip(true)
        .build()
        .expect("http client")
}

/// Whether an address is out on the internet, rather than this machine, the home network
/// or the tailnet.
pub fn public(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(a) => {
            let [x, y, ..] = a.octets();
            !(a.is_private() || a.is_loopback() || a.is_link_local() || a.is_unspecified() || a.is_broadcast() || a.is_documentation() || a.is_multicast() || x == 0 || (x == 100 && (64..128).contains(&y)) || (x == 198 && (18..20).contains(&y)) || x >= 240)
        }
        std::net::IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(v4) => public(v4.into()),
            None => !(a.is_loopback() || a.is_unspecified() || a.is_multicast() || (a.segments()[0] & 0xfe00) == 0xfc00 || (a.segments()[0] & 0xffc0) == 0xfe80),
        },
    }
}

/// Names looked up to addresses out on the internet only.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let found: Vec<std::net::SocketAddr> = tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            let kept: Vec<std::net::SocketAddr> = found.into_iter().filter(|a| public(a.ip())).collect();
            if kept.is_empty() {
                return Err(format!("{} is on this network, not out on the internet", name.as_str()).into());
            }
            Ok(Box::new(kept.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Whether a link names somewhere out on the internet by its address, or by a name, which
/// [`PublicOnly`] then holds to the same.
fn public_link(url: &reqwest::Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.host_str().is_some_and(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().map_or(true, public))
}

/// A client for links someone out on the internet gave: an indexer names where its NZB is in
/// what it sends back, and one that had gone bad could name a machine here instead. It is
/// held to the internet at every step, redirects included.
pub fn public_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("Spool/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .gzip(true)
        .dns_resolver(Arc::new(PublicOnly))
        // A proxy looks the name up itself, past the resolver.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else if public_link(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("a redirect into this network")
            }
        }))
        .build()
        .expect("http client")
}

/// The most an NZB may be. The largest season packs run to tens of megabytes.
pub const NZB_MOST: usize = 128 << 20;

/// A response's body, up to `most` bytes: past that it is not what was asked for.
pub async fn read_at_most(mut resp: reqwest::Response, most: usize) -> std::result::Result<Vec<u8>, String> {
    if resp.content_length().is_some_and(|n| n > most as u64) {
        return Err(format!("it was larger than {} MB", most >> 20));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.without_url().to_string())? {
        if body.len() + chunk.len() > most {
            return Err(format!("it was larger than {} MB", most >> 20));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl App {
    /// The client to fetch an indexer's NZB link with. An indexer on this network, such as a
    /// local aggregator, hands out links on this network; one out on the internet does not.
    pub fn nzb_client(&self, indexer_id: i64, link: &str) -> std::result::Result<&reqwest::Client, String> {
        let at = self.db.indexers().ok().and_then(|all| all.into_iter().find(|i| i.id == indexer_id)).and_then(|i| reqwest::Url::parse(&i.url).ok());
        // Told by how the indexer was written down, never by what its name looks up to: an
        // indexer that answers for its own name could otherwise make itself look local.
        let here = at.as_ref().and_then(|u| u.host_str()).is_some_and(|h| match h.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
            Ok(ip) => !public(ip),
            Err(_) => crate::api::local_name(h),
        });
        if here {
            return Ok(&self.http);
        }
        match reqwest::Url::parse(link) {
            Ok(url) if public_link(&url) => Ok(&self.public_http),
            _ => Err("its NZB link points into this network, which an indexer out on the internet has no business doing".into()),
        }
    }

    /// Note that a player has asked for a file, and where it is being sent for it.
    pub fn note_playing(&self, who: &Signed, path: &Path, from: Option<&str>) {
        let now = crate::db::now();
        let mut playing = self.playing.lock();
        if let Some(p) = playing.get_mut(&who.token).filter(|p| p.path == path) {
            (p.seen, p.from) = (now, from.unwrap_or_default().to_string());
            return;
        }
        let Some(t) = self.db.titles(None).ok().and_then(|all| all.into_iter().find(|t| !t.path.is_empty() && (path.starts_with(&t.path) || t.also.iter().any(|a| path.starts_with(a))))) else { return };
        playing.insert(who.token.clone(), Playing { title: t.title, title_id: t.id, account: who.account_name.clone(), device: who.device.clone(), play: 0, subject: Default::default(), reported: now, paused: false, from: from.unwrap_or_default().to_string(), since: now, seen: now, path: path.to_path_buf() });
    }

    /// A player said how far it had got, or that it had stopped. What it is playing is
    /// kept as playing, and the sitting is written down: begun the first time it reports,
    /// and carried on each time after, by the time that has passed while it was not paused.
    pub fn note_sitting(&self, who: &Signed, s: &Sitting) {
        let now = crate::db::now();
        let mut playing = self.playing.lock();
        let Some(p) = playing.get_mut(&who.token) else { return };
        p.seen = now;
        if p.play == 0 || p.subject != (s.kind.to_string(), s.ref_id) {
            let begun = Play { account_id: who.account, kind: s.kind.to_string(), ref_id: s.ref_id, title_id: s.title_id, title: s.title.to_string(), device: who.device.clone(), source: p.from.clone(), started_at: now, position: s.position, finished: s.finished, ..Default::default() };
            (p.play, p.subject) = (self.db.play_begin(&begun).unwrap_or(0), (s.kind.to_string(), s.ref_id));
        } else {
            // The time since it last reported was watched if it was playing then, and
            // reported in the meantime as players do, every few seconds. A longer gap is
            // one it was not playing in.
            let gap = now - p.reported;
            let watched = if p.paused || !(0..=30).contains(&gap) { 0 } else { gap };
            let _ = self.db.play_on(p.play, watched as f64, s.position, &p.from, s.finished);
        }
        (p.reported, p.paused) = (now, s.paused);
        if s.stopped {
            playing.remove(&who.token);
        }
    }

    /// Read again who is signed in: the players of every account that may sign in.
    ///
    /// Read while the list is held, so that of two readings the later one is the one that
    /// stands, and a player signed out is never put back by a reading begun before.
    pub fn reload_players(&self) {
        let mut players = self.players.lock();
        let accounts: HashMap<i64, String> = self.db.accounts().unwrap_or_default().into_iter().filter(|a| a.enabled).map(|a| (a.id, a.name)).collect();
        *players = self.db.devices().unwrap_or_default().into_iter().filter_map(|d| Some((d.token.clone(), Signed { token: d.token, account: d.account_id, account_name: accounts.get(&d.account_id)?.clone(), device: d.name, seen: d.seen_at }))).collect();
    }

    /// Whether a player is at home: on one of the networks this machine is on. One that
    /// comes over Tailscale is placed by where it is really reached, and one reached
    /// through a relay is away. A request with no address to judge by is taken as at home.
    pub async fn at_home(&self, player: Option<std::net::IpAddr>) -> bool {
        // An address of the old kind that arrived dressed as the new kind is the old kind.
        let Some(player) = player.map(|p| p.to_canonical()) else { return true };
        let Some(really) = crate::link::tailnet::really(player).await else { return false };
        if let Some(nets) = self.home.lock().as_ref() {
            return nets.iter().any(|n| n.holds(really));
        }
        // Each network as wide as this machine has it, not a guess at its width.
        let mine = if_addrs::get_if_addrs().into_iter().flatten().filter(|i| !i.is_loopback() && !crate::link::tailnet::ours(i.ip())).filter_map(|i| {
            let bits = match &i.addr {
                if_addrs::IfAddr::V4(a) => a.prefixlen,
                if_addrs::IfAddr::V6(a) => a.prefixlen,
            };
            crate::link::Net::parse(&format!("{}/{bits}", i.ip())).ok()
        });
        really.is_loopback() || mine.into_iter().any(|n| n.holds(really))
    }

    /// What is being played now, the longest-running first.
    pub fn playing_now(&self) -> Vec<Playing> {
        let now = crate::db::now();
        let mut playing = self.playing.lock();
        playing.retain(|_, p| now - p.seen < QUIET);
        let mut all: Vec<Playing> = playing.values().cloned().collect();
        all.sort_by_key(|p| p.since);
        all
    }

    pub async fn start(data_dir: &Path, db: Db) -> anyhow::Result<App> {
        Self::start_with(data_dir, db, None).await
    }

    /// As [`start`], asking for film and series details at the given addresses (TMDB's, then
    /// TVmaze's) in place of the real ones.
    pub async fn start_with(data_dir: &Path, db: Db, sources: Option<(&str, &str)>) -> anyhow::Result<App> {
        let sessions: HashSet<String> = db.get_setting::<Vec<String>>("sessions").into_iter().collect();
        let settings = Settings::new(db.clone());
        // API and MCP access always go through a key, so there is always one.
        let mut g = settings.general();
        if g.api_key.is_empty() {
            g.api_key = crate::settings::new_key();
            settings.set_general(&g)?;
        }
        let engine = Engine::start(engine_config(&settings, data_dir)).await?;
        let http = http_client();
        let mut meta = Metadata::new(http.clone());
        if let Some((tmdb, tvmaze)) = sources {
            (meta.tmdb_base, meta.tvmaze_base) = (tmdb.to_string(), tvmaze.to_string());
            // The anime sources stand beside the series source, so a test's stand-in serves all.
            (meta.anilist_base, meta.anime_ids_url) = (format!("{tvmaze}/anilist"), format!("{tvmaze}/anime-ids.json"));
        }
        let (events, _) = broadcast::channel(1024);
        let app = App(Arc::new(Inner {
            db,
            settings,
            engine,
            remotes: Mutex::new(vec![]),
            meta,
            http,
            public_http: public_client(),
            events,
            data_dir: data_dir.to_path_buf(),
            importing: Mutex::new(HashSet::new()),
            title_imports: Mutex::new(HashMap::new()),
            indexer_last: Mutex::new(HashMap::new()),
            indexer_backoff: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
            volume_access: std::sync::atomic::AtomicU8::new(0),
            location_state: Mutex::new(HashMap::new()),
            location_checking: Mutex::new(HashSet::new()),
            location_slow: Mutex::new(HashMap::new()),
            location_changes: std::sync::atomic::AtomicU64::new(0),
            adoption: Mutex::new(None),
            anime_list: Mutex::new(None),
            moves: Mutex::new(Default::default()),
            encodes: Mutex::new(Default::default()),
            port: Default::default(),
            last_playing: Default::default(),
            playing: Default::default(),
            opened: Default::default(),
            asking: Default::default(),
            looked_up: Default::default(),
            heard: Default::default(),
            sessions: Mutex::new(sessions),
            login_failures: Tries::default(),
            space_blocked: Mutex::new(HashMap::new()),
            subtitle_login: Mutex::new(None),
            players: Default::default(),
            player_failures: Tries::default(),
            player_requests: Mutex::new(Default::default()),
            player_library: Mutex::new(None),
            player_library_compact: Mutex::new(None),
            home: Mutex::new(None),
            player_status: Mutex::new(String::new()),
            grabbing: Mutex::new(HashSet::new()),
            waiting_for_space: Mutex::new(HashSet::new()),
            art_fetches: tokio::sync::Semaphore::new(6),
        }));
        app.reload_players();
        Ok(app)
    }

    pub fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    pub fn mode(&self) -> Mode {
        self.settings.general().mode
    }

    pub fn is_active(&self) -> bool {
        self.mode() == Mode::Active
    }

    /// Push current settings into the download engine.
    pub fn apply_engine_config(&self) {
        self.engine.set_config(engine_config(&self.settings, &self.data_dir));
    }

    /// True when the volume media lives on is mounted. Spool never writes media otherwise, so a
    /// missing external disk cannot turn into files on the internal one.
    pub fn volume_ok(&self) -> bool {
        let required = self.settings.general().required_volume;
        if required.is_empty() {
            return true;
        }
        if !is_mount_point(Path::new(&required)) {
            return false;
        }
        self.volume_access.load(std::sync::atomic::Ordering::Relaxed) == 2 || self.check_volume_access(&required)
    }

    /// Try to list the volume on a separate thread, because the call can block on a consent
    /// prompt. Returns true only once a listing has succeeded.
    fn check_volume_access(&self, volume: &str) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        match self.volume_access.load(Relaxed) {
            2 => return true,
            // Waiting for an answer, or refused. The volume watcher clears a refusal to try again.
            1 | 3 => return false,
            _ => {}
        }
        self.volume_access.store(1, Relaxed);
        let (app, path) = (self.clone(), volume.to_string());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let ok = std::fs::read_dir(&path).and_then(|mut d| d.next().transpose()).is_ok();
            app.volume_access.store(if ok { 2 } else { 3 }, Relaxed);
            if ok {
                tracing::info!(volume = %path, "media volume is readable");
            } else {
                tracing::warn!(volume = %path, "the media volume is mounted but cannot be read");
            }
            let _ = tx.send(ok);
        });
        // The usual case answers at once; a pending prompt leaves the thread waiting.
        rx.recv_timeout(std::time::Duration::from_millis(400)).unwrap_or(false)
    }

    /// Why the media volume cannot be used right now, in words for the user.
    pub fn volume_problem(&self) -> Option<String> {
        let required = self.settings.general().required_volume;
        if required.is_empty() || self.volume_ok() {
            return None;
        }
        if !is_mount_point(Path::new(&required)) {
            return Some(format!("{required} is not mounted. Downloads and imports are waiting."));
        }
        let refused = self.volume_access.load(std::sync::atomic::Ordering::Relaxed) == 3;
        Some(if !cfg!(target_os = "macos") {
            format!("{required} is mounted but Spool cannot read it. Check that the user Spool runs as has access to it.")
        } else if refused {
            format!("macOS has refused Spool access to {required}. Allow it under System Settings, Privacy & Security, Files and Folders, then restart Spool.")
        } else {
            format!("Spool is waiting for permission to read {required}. Approve the prompt on this Mac's screen.")
        })
    }

    pub fn task_start(&self, name: &str, message: &str) -> bool {
        {
            let mut t = self.tasks.lock();
            let s = t.entry(name.to_string()).or_default();
            if s.running {
                return false;
            }
            s.running = true;
            s.last_message = message.to_string();
        }
        self.emit(Event::Task { name: name.into(), running: true, message: message.into() });
        true
    }

    pub fn task_end(&self, name: &str, message: &str) {
        {
            let mut t = self.tasks.lock();
            let s = t.entry(name.to_string()).or_default();
            s.running = false;
            s.last_run = crate::db::now();
            s.last_message = message.to_string();
        }
        self.emit(Event::Task { name: name.into(), running: false, message: message.into() });
    }
}

/// Where Spool keeps its database when not told otherwise: the platform's usual place.
pub fn default_data_dir() -> PathBuf {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Spool")
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".local/share")).join("spool")
    }
}

/// The log file Spool writes itself, or None where something else already captures its output
/// (the LaunchAgent on macOS) or `SPOOL_LOG` says where the log is.
pub fn own_log_path(data_dir: &Path) -> Option<PathBuf> {
    if cfg!(target_os = "macos") || std::env::var_os("SPOOL_LOG").is_some() {
        return None;
    }
    Some(data_dir.join("spool.log"))
}

/// Where to read the service log from.
pub fn log_path(data_dir: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("SPOOL_LOG") {
        return p.into();
    }
    own_log_path(data_dir).unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Library/Logs/spool.log"))
}

/// Free bytes on the filesystem holding `p`, or None if it cannot be read.
pub fn free_space(p: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    // Walk up to something that exists; the folder itself may not have been created yet.
    let mut dir = p;
    while !dir.exists() {
        dir = dir.parent()?;
    }
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // On macOS statvfs counts blocks in 32 bits, and on a large share with small blocks the
    // count wraps: a volume with 9.5 TB free was reported as having 0.9. statfs counts in 64.
    #[cfg(target_os = "macos")]
    {
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        Some(st.f_bavail * st.f_bsize as u64)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        Some(st.f_bavail as u64 * st.f_frsize as u64)
    }
}

/// The shares from other machines that are mounted on this one: what each is a share of,
/// as the system writes it (`nas:/export/films`, `//user@nas/films`), and where it is mounted.
pub fn network_mounts() -> Vec<(String, PathBuf)> {
    const KINDS: [&str; 6] = ["nfs", "nfs4", "smbfs", "cifs", "smb3", "afpfs"];
    #[cfg(target_os = "macos")]
    {
        let text = |chars: &[libc::c_char]| unsafe { std::ffi::CStr::from_ptr(chars.as_ptr()) }.to_string_lossy().to_string();
        let mut list: *mut libc::statfs = std::ptr::null_mut();
        // SAFETY: the system fills in a list of its own and says how long it is; it is read
        // at once and not kept.
        let count = unsafe { libc::getmntinfo(&mut list, libc::MNT_NOWAIT) };
        if count <= 0 || list.is_null() {
            return vec![];
        }
        let mounts = unsafe { std::slice::from_raw_parts(list, count as usize) };
        mounts.iter().filter(|m| KINDS.contains(&text(&m.f_fstypename).as_str())).map(|m| (text(&m.f_mntfromname), PathBuf::from(text(&m.f_mntonname)))).collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        // A space in a name is written as \040 there.
        let plain = |word: &str| word.replace("\\040", " ");
        std::fs::read_to_string("/proc/self/mounts")
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let mut words = line.split(' ');
                let (from, on, kind) = (words.next()?, words.next()?, words.next()?);
                KINDS.contains(&kind).then(|| (plain(from), PathBuf::from(plain(on))))
            })
            .collect()
    }
}

/// The disk a folder is on, by where it is mounted, when that is a disk that can be taken
/// away: not the one the system starts from, and not a share from another machine, which
/// is there or not by whether it can be read.
pub fn disk_of(folder: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let (mounted_at, shared) = {
        use std::os::unix::ffi::OsStrExt;
        let text = |chars: &[libc::c_char]| unsafe { std::ffi::CStr::from_ptr(chars.as_ptr()) }.to_string_lossy().to_string();
        let path = std::ffi::CString::new(folder.as_os_str().as_bytes()).ok()?;
        let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: the path is ended with a zero and the result is ours to fill in.
        if unsafe { libc::statfs(path.as_ptr(), &mut fs) } != 0 {
            return None;
        }
        (PathBuf::from(text(&fs.f_mntonname)), ["nfs", "smbfs", "afpfs", "autofs", "webdav"].contains(&text(&fs.f_fstypename).as_str()))
    };
    #[cfg(not(target_os = "macos"))]
    let (mounted_at, shared) = {
        use std::os::unix::fs::MetadataExt;
        let dev = std::fs::metadata(folder).ok()?.dev();
        let top = folder.ancestors().take_while(|a| std::fs::metadata(a).is_ok_and(|m| m.dev() == dev)).last()?.to_path_buf();
        let shared = network_mounts().iter().any(|(_, on)| *on == top);
        (top, shared)
    };
    let boot = [Path::new("/"), Path::new("/System/Volumes/Data")].contains(&mounted_at.as_path());
    (!boot && !shared).then_some(mounted_at)
}

/// The disk that has to be mounted for a pair of library folders to be there: the one
/// both are on, when it is one that can be taken away. Worked out from the folders, so
/// that it is never a thing to be got wrong. When neither folder can be seen just now,
/// what was known before stands.
pub fn disk_needed(folders: [&str; 2], known: &str) -> String {
    let there: Vec<&Path> = folders.iter().filter(|f| !f.is_empty()).map(Path::new).filter(|f| f.is_dir()).collect();
    if there.is_empty() {
        return known.to_string();
    }
    let disks: Vec<Option<PathBuf>> = there.iter().map(|f| disk_of(f)).collect();
    match &disks[0] {
        Some(disk) if disks.iter().all(|d| d.as_ref() == Some(disk)) => disk.to_string_lossy().to_string(),
        _ => String::new(),
    }
}

pub fn is_mount_point(p: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(me), Some(parent)) = (std::fs::metadata(p), p.parent()) else { return false };
    match std::fs::metadata(parent) {
        Ok(up) => me.dev() != up.dev(),
        Err(_) => false,
    }
}

pub fn engine_config(settings: &Settings, data_dir: &Path) -> EngineConfig {
    let g = settings.general();
    let downloads = if g.downloads_dir.is_empty() { data_dir.join("downloads") } else { PathBuf::from(&g.downloads_dir) };
    EngineConfig {
        servers: settings.servers(),
        incomplete_dir: downloads.join("spool-incomplete"),
        complete_dir: downloads.join("spool-complete"),
        par2_path: String::new(),
        sevenzip_path: String::new(),
        speed_limit: settings.speed_limit(),
        direct_unpack: g.direct_unpack,
        post_parallel: g.post_parallel as usize,
    }
}

#[cfg(test)]
mod disk_tests {
    use super::*;

    #[test]
    fn the_disk_a_library_needs_is_worked_out_from_its_folders() {
        let dir = tempfile::tempdir().unwrap();
        let (films, series) = (dir.path().join("Films"), dir.path().join("Series"));
        std::fs::create_dir_all(&films).unwrap();
        let text = |p: &Path| p.to_string_lossy().to_string();
        // Folders on the disk the system starts from need no disk mounted.
        assert_eq!(disk_of(&films), None);
        assert_eq!(disk_needed([&text(&films), ""], "/Volumes/Old"), "", "seen, and found to need none");
        // Folders that cannot be seen just now say nothing, and what was known stands.
        assert_eq!(disk_needed([&text(&series), ""], "/Volumes/Old"), "/Volumes/Old");
        assert_eq!(disk_needed(["", ""], "/Volumes/Old"), "/Volumes/Old");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_guesses_shut_the_door_on_whoever_made_them_alone() {
        let tries = Tries::default();
        let (guesser, household) = (Some("192.168.69.50".parse().unwrap()), Some("192.168.69.20".parse().unwrap()));
        for _ in 0..Tries::TRIES - 1 {
            assert!(tries.take(guesser));
        }
        // Signing in to one account with its password buys no more guesses at another.
        assert!(tries.take(guesser));
        tries.right(guesser);
        assert!(tries.take(guesser), "the fifth wrong guess");
        assert!(!tries.take(guesser), "and none after it");
        assert!(tries.take(household), "someone else in the house can still sign in");
        tries.right(household);
        assert!(!tries.take(guesser));
    }

    #[test]
    fn only_addresses_out_on_the_internet_are_public() {
        for ip in ["8.8.8.8", "104.16.1.1", "2606:4700::1111"] {
            assert!(public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["127.0.0.1", "10.0.0.1", "192.168.69.96", "172.16.5.4", "169.254.1.1", "100.97.6.66", "0.0.0.0", "::1", "fd7a:115c:a1e0::1", "fe80::1", "::ffff:192.168.1.1", "255.255.255.255"] {
            assert!(!public(ip.parse().unwrap()), "{ip}");
        }
        let link = |s: &str| public_link(&reqwest::Url::parse(s).unwrap());
        assert!(link("https://api.nzbgeek.info/api?t=get"));
        assert!(link("https://1.1.1.1/x.nzb"));
        assert!(!link("http://127.0.0.1:7979/api/recycle/empty"));
        assert!(!link("http://[::1]/x"));
        assert!(!link("file:///etc/passwd"));
    }
}
