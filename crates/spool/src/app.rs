//! Shared application state.

use crate::db::Db;
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
}

pub struct Inner {
    pub db: Db,
    pub settings: Settings,
    pub engine: Engine,
    pub http: reqwest::Client,
    pub meta: Metadata,
    pub events: broadcast::Sender<Event>,
    pub data_dir: PathBuf,
    /// Acquisitions with an import in progress, so two triggers cannot import one download twice.
    pub importing: Mutex<HashSet<i64>>,
    /// Last request time per indexer, to space requests out.
    pub indexer_last: Mutex<HashMap<i64, std::time::Instant>>,
    /// Indexers that are failing: (consecutive failures, do not ask again before, last error).
    pub indexer_backoff: Mutex<HashMap<i64, (u32, std::time::Instant, String)>>,
    pub tasks: Mutex<HashMap<String, TaskState>>,
    /// Whether macOS lets this process read the media volume: 0 not checked, 1 waiting for an
    /// answer, 2 allowed, 3 refused. Reading a removable disk needs the user's consent, and the
    /// first read blocks until they give it.
    pub volume_access: std::sync::atomic::AtomicU8,
    pub sessions: Mutex<HashSet<String>>,
    /// Wrong passwords lately: (how many, when the last one was).
    pub login_failures: Mutex<(u32, i64)>,
    /// Titles last turned away for lack of disk space, with the headroom there was at the time.
    pub space_blocked: Mutex<HashMap<i64, i128>>,
    /// The OpenSubtitles sign-in in use, and when it was made.
    pub subtitle_login: Mutex<Option<(String, i64)>>,
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

impl App {
    pub async fn start(data_dir: &Path, db: Db) -> anyhow::Result<App> {
        let sessions: HashSet<String> = db.get_setting::<Vec<String>>("sessions").into_iter().collect();
        let settings = Settings::new(db.clone());
        // API and MCP access always go through a key, so there is always one.
        let mut g = settings.general();
        if g.api_key.is_empty() {
            g.api_key = crate::settings::new_key();
            settings.set_general(&g)?;
        }
        // A password saved in the clear by an earlier version is hashed where it sits.
        if !g.password.is_empty() && !crate::settings::is_hashed(&g.password) {
            settings.set_general(&g)?;
            tracing::info!("the app password is now stored as a hash");
        }
        let engine = Engine::start(engine_config(&settings, data_dir)).await?;
        let http = http_client();
        let (events, _) = broadcast::channel(1024);
        Ok(App(Arc::new(Inner {
            db,
            settings,
            engine,
            meta: Metadata::new(http.clone()),
            http,
            events,
            data_dir: data_dir.to_path_buf(),
            importing: Mutex::new(HashSet::new()),
            indexer_last: Mutex::new(HashMap::new()),
            indexer_backoff: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
            volume_access: std::sync::atomic::AtomicU8::new(0),
            sessions: Mutex::new(sessions),
            login_failures: Mutex::new((0, 0)),
            space_blocked: Mutex::new(HashMap::new()),
            subtitle_login: Mutex::new(None),
            grabbing: Mutex::new(HashSet::new()),
            waiting_for_space: Mutex::new(HashSet::new()),
            art_fetches: tokio::sync::Semaphore::new(6),
        })))
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
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize as u64)
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
