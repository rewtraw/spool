//! The file work, done on the machine that holds the disks.
//!
//! Spool runs on one machine and may keep part of its library on another, a NAS, which it
//! reaches over a network share. `spool helper` is a small program for the NAS itself. It
//! downloads, repairs and unpacks onto the NAS's own disks, files what arrived with a local
//! move, and moves a title from one of its folders to another without a byte crossing the
//! network. It knows nothing of the catalogue and decides nothing: the Spool that drives it
//! (see [`crate::remote`]) says what to fetch and what goes where.
//!
//! It touches only the folders it was started with. Every path it is given is checked against
//! them, and a folder named as only to be read is never written to or removed from.
//!
//! It is one end of a link (see [`crate::link`]) and holds itself to what it was started
//! allowing: the work above, and playing, which is handing one file to a player who comes
//! with a ticket from the Spool that drives it.

use crate::local::{Blocked, FolderSummary, Survey};
use crate::link::pair;
use crate::link::ticket::{self, Signer};
use crate::link::{Grants, Hello, Protocols, Share};
use crate::relocate::{copy_all, listing, remove_all, CALLED_OFF};
use anyhow::{anyhow, bail, Context, Result};
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spool_nntp::{Engine, EngineConfig, JobState, ServerConfig};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What a helper is started with.
#[derive(Clone, Debug)]
pub struct Conf {
    /// Whoever drives the helper sends this with every request. Empty when the helper is
    /// to be linked with a code instead, and holds whatever key comes of that.
    pub key: String,
    /// The code to link it with, when it is given one; it makes one up otherwise.
    pub code: Option<String>,
    /// The folders it may write to: the library folders it files into.
    pub roots: Vec<PathBuf>,
    /// Folders it may read and copy from, and never change.
    pub read_roots: Vec<PathBuf>,
    /// Where it downloads to. A folder of its own, on the same disks as the library if a
    /// finished download is to be filed by renaming it.
    pub downloads: PathBuf,
    /// What whoever holds the key may do here.
    pub grants: Grants,
}

/// How many parts of files may be on their way to players at once. A player holds a few at
/// a time and lets go of them as it skips about; past this, one more is turned away at once
/// and not kept waiting.
const STREAMS: usize = 48;
/// How many folders may be looked through at once, and how long one more waits its turn.
/// The longest an asker is kept waiting for news.
const HOLD: u64 = 30;
const LOOKS: usize = 4;
const LOOK_WAIT: Duration = Duration::from_secs(60);
/// How long the name of a request for a download is remembered, in seconds.
const REMEMBERED: u64 = 3600;

impl Conf {
    /// From the environment, which is how a container is told things.
    pub fn from_env() -> Result<Conf> {
        let list = |name: &str| -> Vec<PathBuf> { std::env::var(name).unwrap_or_default().split(':').map(str::trim).filter(|p| !p.is_empty()).map(PathBuf::from).collect() };
        let conf = Conf {
            key: std::env::var("SPOOL_HELPER_KEY").unwrap_or_default().trim().to_string(),
            code: std::env::var("SPOOL_LINK_CODE").ok().map(|c| pair::tidy(&c)).filter(|c| !c.is_empty()),
            roots: list("SPOOL_HELPER_ROOTS"),
            read_roots: list("SPOOL_HELPER_READ_ROOTS"),
            downloads: PathBuf::from(std::env::var("SPOOL_HELPER_DOWNLOADS").unwrap_or_default().trim()),
            // A helper does everything it can for its Spool unless it is told to do less.
            grants: match std::env::var("SPOOL_HELPER_GRANTS") {
                Ok(list) => Grants::parse(&list).map_err(|why| anyhow!("SPOOL_HELPER_GRANTS: {why}"))?,
                Err(_) => Grants::ALL,
            },
        };
        conf.checked()
    }

    pub fn checked(self) -> Result<Conf> {
        if !self.key.is_empty() && self.key.len() < 16 {
            bail!("SPOOL_HELPER_KEY must be a key of at least 16 characters; the Spool that drives this helper is given the same one. Leave it out to link the two with a code instead");
        }
        if self.code.as_ref().is_some_and(|c| c.len() < pair::DIGITS) {
            bail!("SPOOL_LINK_CODE must have at least {} digits", pair::DIGITS);
        }
        if self.roots.is_empty() {
            bail!("SPOOL_HELPER_ROOTS must name at least one folder the helper may file into, separated by colons");
        }
        if self.downloads.as_os_str().is_empty() {
            bail!("SPOOL_HELPER_DOWNLOADS must name the folder to download into");
        }
        for p in self.roots.iter().chain(&self.read_roots).chain([&self.downloads]) {
            if !p.is_absolute() {
                bail!("{} must be a full path, starting with /", p.display());
            }
        }
        Ok(self)
    }
}

/// The part of the settings that the driving Spool hands over: the Usenet servers and how
/// hard to work. Kept on disk so that downloads carry on after the helper restarts, whether
/// or not Spool is there to say it again.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pushed {
    pub servers: Vec<ServerConfig>,
    pub speed_limit: u64,
    pub direct_unpack: bool,
    pub post_parallel: usize,
}

/// A folder being moved from one place on the helper's disks to another.
struct Op {
    from: PathBuf,
    done: Arc<AtomicU64>,
    total: AtomicU64,
    stop: Arc<AtomicBool>,
    /// "running", "done", "cancelled", or what went wrong.
    state: Mutex<String>,
    /// What was copied, so that exactly that is taken away from the original afterwards.
    files: Mutex<Vec<(PathBuf, u64)>>,
    /// The folder was renamed into place, so there is no original left to take away.
    renamed: AtomicBool,
    /// When it ended. One that nobody came back for is forgotten a day later.
    ended: Mutex<Option<Instant>>,
}

#[derive(Clone)]
pub struct Helper(Arc<Inner>);

struct Inner {
    engine: Engine,
    conf: Conf,
    data_dir: PathBuf,
    /// Different each time the helper starts, so its driver can tell that it did.
    boot: String,
    pushed: Mutex<Pushed>,
    /// Held while the settings are written, so two changes are never written across each other.
    saving: tokio::sync::Mutex<()>,
    /// The folders it works in, as the other end of the link is told of them.
    shares: Vec<Share>,
    /// The key it shares with the Spool it is linked to, and what is made from it. None
    /// until it has been linked: until then it does nothing for anybody.
    linked: parking_lot::RwLock<Option<Arc<Linked>>>,
    pairing: Mutex<Pairing>,
    /// The disk each shared folder was on when the helper started. A folder found on
    /// another since is one whose disk has gone from under it.
    disks: Vec<Option<u64>>,
    /// What was mounted inside the shared folders then: a disk of its own under a shared
    /// folder, which may go while the folder above it stays.
    mounts: Vec<PathBuf>,
    streams: Arc<tokio::sync::Semaphore>,
    looks: Arc<tokio::sync::Semaphore>,
    /// Downloads lately taken, by the name the asker gave the request and with when, so that
    /// one asked for twice because the answer was lost is started once. Written down, so
    /// that this holds across a restart as the downloads themselves do.
    taken: Mutex<HashMap<String, (String, u64)>>,
    ops: Mutex<HashMap<String, Arc<Op>>>,
    /// Files being filed into the library right now.
    filing: AtomicUsize,
    /// How many files it has filed and titles it has moved since it started.
    filed: AtomicUsize,
    moved: AtomicUsize,
}

/// The link's key, and what signs and checks tickets with it.
struct Linked {
    key: String,
    signer: Signer,
}

impl Linked {
    fn new(key: String) -> Arc<Linked> {
        Arc::new(Linked { signer: Signer::new(&key), key })
    }
}

/// How many wrong codes are taken before none is for a while, and how long that while is
/// the first time, in seconds. It doubles each time, up to a day. Anyone on the network
/// can use tries up without knowing the code, so they come back; a code is guessed no
/// faster for it.
const TRIES: u32 = 5;
const REST: u64 = 600;
const LONGEST_REST: u64 = 24 * 3600;
/// How long a try may take. No more are ever under way than there are tries to be had.
const TRY_FOR: Duration = Duration::from_secs(60);

/// Where a helper stands in being linked with a code.
#[derive(Default)]
struct Pairing {
    /// The code it gave. None when it was started with a key, or has been linked.
    code: Option<String>,
    /// Tries that did not end in a link. Kept across restarts for as long as the code is
    /// the same, so that starting it again gives no more guesses at one code.
    failures: u32,
    /// How many times the tries have run out, and when codes are taken again.
    rests: u32,
    resting_until: u64,
    /// Tries under way, by a name made up for each.
    tries: HashMap<String, (Instant, pair::Giving)>,
    /// The try that linked it, kept so that an asker whose answer was lost may ask again.
    done: Option<(String, pair::Giving)>,
}

struct Refused(StatusCode, String);

impl<E: Into<anyhow::Error>> From<E> for Refused {
    fn from(e: E) -> Self {
        Refused(StatusCode::BAD_REQUEST, format!("{:#}", e.into()))
    }
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

type R = std::result::Result<Json<Value>, Refused>;

fn under(p: &Path, root: &Path) -> bool {
    p.starts_with(root)
}

impl Helper {
    pub async fn start(data_dir: &Path, conf: Conf) -> Result<Helper> {
        std::fs::create_dir_all(data_dir)?;
        let pushed: Pushed = match std::fs::read(data_dir.join("helper.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "the settings Spool gave last time cannot be read; waiting to be given them again");
                Pushed::default()
            }),
            Err(_) => Pushed::default(),
        };
        let engine = Engine::start(Self::engine_config(&conf, &pushed)).await?;
        let boot = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let shares: Vec<Share> = conf.roots.iter().map(|p| Share::new(p, true)).chain(conf.read_roots.iter().map(|p| Share::new(p, false))).collect();
        let disks = shares.iter().map(|s| disk_of(&s.path)).collect();
        let mounts = mounted().into_iter().filter(|m| shares.iter().any(|s| m.starts_with(&s.path) && *m != s.path)).collect();
        // The key it was started with; failing that the one agreed last time; failing that
        // it waits to be linked with a code. A key on disk that cannot be read is not
        // taken for no key: that would open it to be linked again by whoever comes first.
        let kept = data_dir.join("link.json");
        let key = if !conf.key.is_empty() {
            Some(conf.key.clone())
        } else if kept.exists() {
            let said: Value = serde_json::from_slice(&std::fs::read(&kept)?).with_context(|| format!("{} holds the key this helper was linked with and cannot be read; delete it to link the helper afresh", kept.display()))?;
            Some(said["key"].as_str().filter(|k| k.len() >= 16).ok_or_else(|| anyhow!("{} holds no key; delete it to link the helper afresh", kept.display()))?.to_string())
        } else {
            None
        };
        let mut pairing = Pairing::default();
        if key.is_none() {
            let code = conf.code.clone().unwrap_or_else(pair::new_code);
            let tried: Value = std::fs::read(data_dir.join("tries.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
            if tried["code"].as_str() == Some(code_mark(&code).as_str()) {
                (pairing.failures, pairing.rests, pairing.resting_until) = (tried["failures"].as_u64().unwrap_or(0) as u32, tried["rests"].as_u64().unwrap_or(0) as u32, tried["until"].as_u64().unwrap_or(0));
            }
            if pairing.resting_until > now() {
                tracing::warn!(minutes = (pairing.resting_until - now()).div_ceil(60), "this helper has been given too many wrong codes and takes none for a while; start it with a new SPOOL_LINK_CODE to link it now");
            } else {
                tracing::info!(code = %pair::shown(&code), "this helper is not linked yet: in Spool, under Settings, Library, link another Spool and type this code");
            }
            pairing.code = Some(code);
        }
        Ok(Helper(Arc::new(Inner {
            engine,
            conf,
            data_dir: data_dir.to_path_buf(),
            boot,
            pushed: Mutex::new(pushed),
            saving: Default::default(),
            shares,
            linked: parking_lot::RwLock::new(key.map(Linked::new)),
            pairing: Mutex::new(pairing),
            disks,
            mounts,
            streams: Arc::new(tokio::sync::Semaphore::new(STREAMS)),
            looks: Arc::new(tokio::sync::Semaphore::new(LOOKS)),
            taken: Mutex::new(std::fs::read(data_dir.join("taken.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()),
            ops: Mutex::new(HashMap::new()),
            filing: AtomicUsize::new(0),
            filed: AtomicUsize::new(0),
            moved: AtomicUsize::new(0),
        })))
    }

    fn engine_config(conf: &Conf, pushed: &Pushed) -> EngineConfig {
        EngineConfig {
            servers: pushed.servers.clone(),
            incomplete_dir: conf.downloads.join("incomplete"),
            complete_dir: conf.downloads.join("complete"),
            speed_limit: pushed.speed_limit,
            direct_unpack: pushed.direct_unpack,
            post_parallel: pushed.post_parallel.max(1),
            ..Default::default()
        }
    }

    /// How many files it has filed and how many titles it has moved since it started.
    pub fn work_done(&self) -> (usize, usize) {
        (self.0.filed.load(Ordering::Relaxed), self.0.moved.load(Ordering::Relaxed))
    }

    pub fn engine(&self) -> &Engine {
        &self.0.engine
    }

    fn complete(&self) -> PathBuf {
        self.0.conf.downloads.join("complete")
    }

    /// A path the helper was given, held to the folders it was started with. To write, it
    /// must be inside one it may change; to read, inside any of them or its downloads.
    fn allowed(&self, given: &str, write: bool) -> std::result::Result<PathBuf, Refused> {
        let p = PathBuf::from(given);
        if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
            return Err(Refused(StatusCode::BAD_REQUEST, format!("{given} is not a plain full path")));
        }
        let c = &self.0.conf;
        // Strictly inside: the folders themselves are never the thing moved or removed.
        let inside = |root: &PathBuf| under(&p, root) && p != *root;
        let ok = c.roots.iter().any(inside) || inside(&c.downloads) || (!write && c.read_roots.iter().any(inside));
        if !ok {
            let what = if c.read_roots.iter().any(inside) { "is in a folder this helper only reads" } else { "is outside the folders this helper was given" };
            return Err(Refused(StatusCode::FORBIDDEN, format!("{given} {what}")));
        }
        Ok(p)
    }

    /// What would be interrupted if the helper were stopped now, in words for a person.
    pub fn busy(&self) -> Vec<String> {
        let mut reasons = vec![];
        let working = self.0.engine.jobs().iter().filter(|j| matches!(j.state, JobState::Verifying | JobState::Repairing | JobState::Extracting | JobState::Finishing)).count();
        if working > 0 {
            reasons.push(format!("{working} {} being checked or unpacked", if working == 1 { "download is" } else { "downloads are" }));
        }
        let filing = self.0.filing.load(Ordering::Relaxed);
        if filing > 0 {
            reasons.push(format!("{filing} {} being filed into the library", if filing == 1 { "file is" } else { "files are" }));
        }
        let playing = STREAMS - self.0.streams.available_permits();
        if playing > 0 {
            reasons.push(format!("{playing} {} being sent to players", if playing == 1 { "part of a film is" } else { "parts of films are" }));
        }
        let moving = self.0.ops.lock().values().filter(|o| *o.state.lock() == "running").count();
        if moving > 0 {
            reasons.push(format!("{moving} {} being moved", if moving == 1 { "title is" } else { "titles are" }));
        }
        reasons
    }

    /// Everything the helper answers. The work is version 1 of the work protocol, and is
    /// answered only to whoever holds the key; a file is handed to whoever holds a ticket.
    pub fn router(&self) -> Router {
        let play = Router::new().route("/link/play/v1/file", get(play)).route("/link/pair/v1/start", post(pair_start)).route("/link/pair/v1/finish", post(pair_finish)).with_state(self.clone());
        let hello = Router::new().route("/link", get(hello)).layer(axum::middleware::from_fn_with_state(self.clone(), guard)).with_state(self.clone());
        play.merge(hello).merge(self.see()).merge(self.work())
    }

    /// Version 1 of the see protocol: what is in the folders that are shared.
    fn see(&self) -> Router {
        Router::new()
            .route("/link/see/v1/survey", post(look::<Survey>))
            .route("/link/see/v1/folders", post(look::<Vec<FolderSummary>>))
            .layer(axum::middleware::from_fn_with_state(self.clone(), allowed_to_see))
            .layer(axum::middleware::from_fn_with_state(self.clone(), guard))
            .with_state(self.clone())
    }

    /// The folder a look is asked for, if it is in a shared folder whose disk is there.
    /// When records are removed on the strength of the answer (`removes`), a shared folder
    /// that is empty is taken for one whose disk has gone: saying "nothing here" of it would
    /// have every record of its files removed. So is one found on another disk than it was
    /// on, or with a disk gone that was mounted on the way to the folder asked for.
    fn looked_at(&self, ask: &Look, removes: bool) -> std::result::Result<PathBuf, Refused> {
        let i = self.0.shares.iter().position(|s| s.id == ask.share).ok_or_else(|| Refused(StatusCode::NOT_FOUND, "no folder is shared under that name".into()))?;
        if !(ask.rel.is_empty() || ticket::plain(&ask.rel)) {
            return Err(Refused(StatusCode::BAD_REQUEST, format!("{} is not a place inside a shared folder", ask.rel)));
        }
        let root = &self.0.shares[i].path;
        let target = if ask.rel.is_empty() { root.clone() } else { root.join(&ask.rel) };
        let gone = |what: String| Refused(StatusCode::SERVICE_UNAVAILABLE, what);
        let where_it_was = disk_of(root).is_some_and(|d| self.0.disks[i].is_none_or(|was| was == d));
        if !where_it_was {
            return Err(gone(format!("{} cannot be read, or is not on the disk it was on", root.display())));
        }
        if !std::fs::read_dir(root).is_ok_and(|mut d| !removes || d.next().is_some_and(|e| e.is_ok())) {
            return Err(gone(format!("{} cannot be read or is empty", root.display())));
        }
        let now = mounted();
        if let Some(lost) = self.0.mounts.iter().find(|m| target.starts_with(m) && !now.contains(m)) {
            return Err(gone(format!("the disk that was at {} is not there", lost.display())));
        }
        // No link is followed on the way to the folder, any more than inside it.
        let mut at = root.clone();
        for part in Path::new(&ask.rel).components() {
            at.push(part);
            if std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(Refused(StatusCode::CONFLICT, format!("{} is a link, and links are not followed here", at.display())));
            }
        }
        Ok(target)
    }

    fn work(&self) -> Router {
        Router::new()
            .route("/link/work/v1/info", get(info))
            .route("/link/work/v1/config", put(config))
            .route("/link/work/v1/jobs", get(jobs).post(add))
            .route("/link/work/v1/jobs/{id}", axum::routing::delete(remove))
            .route("/link/work/v1/jobs/{id}/{action}", post(job_action))
            .route("/link/work/v1/queue/{action}", post(queue_action))
            .route("/link/work/v1/availability", post(availability))
            .route("/link/work/v1/fs/stat", post(stat))
            .route("/link/work/v1/fs/move", post(file_move))
            .route("/link/work/v1/fs/remove", post(output_remove))
            .route("/link/work/v1/moves", post(move_start))
            .route("/link/work/v1/moves/{id}", get(move_state))
            .route("/link/work/v1/moves/{id}/cancel", post(move_cancel))
            .route("/link/work/v1/moves/{id}/finish", post(move_finish))
            .layer(axum::extract::DefaultBodyLimit::max(64 << 20))
            .layer(axum::middleware::from_fn_with_state(self.clone(), allowed_to_work))
            .layer(axum::middleware::from_fn_with_state(self.clone(), guard))
            .with_state(self.clone())
    }

    /// Say on the network that it is here, under a name and at a port, so that a Spool
    /// can list it and its address need not be typed. Not being able to costs nothing but that.
    pub fn announce(&self, name: &str, port: u16) {
        let h = self.clone();
        if let Err(e) = crate::link::find::announce(name, port, move || h.linked().is_none()) {
            tracing::warn!(error = %e, "could not say on the network that this helper is here; give Spool its address instead");
        }
    }

    fn linked(&self) -> Option<Arc<Linked>> {
        self.0.linked.read().clone()
    }

    /// Write down how many tries a code has had, so a restart does not forget.
    fn keep_tries(&self, code: &str, p: &Pairing) -> Result<()> {
        write_private(&self.0.data_dir.join("tries.json"), &serde_json::to_vec(&json!({"code": code_mark(code), "failures": p.failures, "rests": p.rests, "until": p.resting_until}))?)
    }

    /// Open the one file a ticket names. See [`open_inside`].
    fn open_shared(&self, t: &ticket::Ticket) -> std::result::Result<(std::fs::File, PathBuf), Refused> {
        let share = self.0.shares.iter().find(|s| s.id == t.share).ok_or_else(|| Refused(StatusCode::NOT_FOUND, "the file is not there".into()))?;
        match open_inside(&share.path, &t.rel) {
            Ok(file) => Ok((file, share.path.join(&t.rel))),
            Err(Inside::Link) => Err(Refused(StatusCode::FORBIDDEN, "the file is reached through a link, and links in a shared folder are not followed".into())),
            Err(Inside::Missing) => Err(Refused(StatusCode::NOT_FOUND, "the file is not there".into())),
        }
    }
}

/// Why a file inside a shared folder could not be opened.
enum Inside {
    Missing,
    /// Something on the way to it is a link.
    Link,
}

/// Open a file by its place inside a folder, without ever leaving the folder. The folder is
/// opened once, and then each name on the way is opened from the one before it, following
/// no link. Nothing that is renamed or swapped for a link while this runs can lead it out:
/// it holds each folder open, not the folder's name.
#[cfg(unix)]
fn open_inside(root: &Path, rel: &str) -> std::result::Result<std::fs::File, Inside> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    if !ticket::plain(rel) {
        return Err(Inside::Missing);
    }
    // The shared folder itself may be reached by a link; that is whoever started the
    // helper's to choose.
    let mut at: OwnedFd = std::fs::File::open(root).map_err(|_| Inside::Missing)?.into();
    let mut parts = rel.split('/').peekable();
    while let Some(part) = parts.next() {
        let name = CString::new(std::ffi::OsStr::new(part).as_bytes()).map_err(|_| Inside::Missing)?;
        let last = parts.peek().is_none();
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | if last { 0 } else { libc::O_DIRECTORY };
        let fd = unsafe { libc::openat(at.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            // A link is refused in different words on different systems, so it is looked at.
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            let is_link = unsafe { libc::fstatat(at.as_raw_fd(), name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } == 0 && st.st_mode & libc::S_IFMT == libc::S_IFLNK;
            return Err(if is_link { Inside::Link } else { Inside::Missing });
        }
        at = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    let file = std::fs::File::from(at);
    match file.metadata() {
        Ok(m) if m.is_file() => Ok(file),
        _ => Err(Inside::Missing),
    }
}

#[cfg(not(unix))]
fn open_inside(root: &Path, rel: &str) -> std::result::Result<std::fs::File, Inside> {
    let (root, path) = (root.canonicalize().map_err(|_| Inside::Missing)?, root.join(rel).canonicalize().map_err(|_| Inside::Missing)?);
    if !ticket::plain(rel) || !under(&path, &root) || path == root {
        return Err(Inside::Link);
    }
    std::fs::File::open(path).map_err(|_| Inside::Missing)
}

/// Where disks are mounted, as Linux lists them; nothing on a system that does not.
fn mounted() -> Vec<PathBuf> {
    std::fs::read_to_string("/proc/self/mountinfo").map(|list| mount_points(&list)).unwrap_or_default()
}

/// The mount points in a list of mounts, of which the fifth word on each line is one, with
/// a space, tab, newline or backslash in a name written as a backslash and three digits.
fn mount_points(list: &str) -> Vec<PathBuf> {
    let plain = |word: &str| {
        let (bytes, mut out, mut i) = (word.as_bytes(), Vec::new(), 0);
        while i < bytes.len() {
            let code = (bytes[i] == b'\\').then(|| bytes.get(i + 1..i + 4)).flatten().and_then(|d| std::str::from_utf8(d).ok()).and_then(|d| u8::from_str_radix(d, 8).ok());
            out.push(code.unwrap_or(bytes[i]));
            i += if code.is_some() { 4 } else { 1 };
        }
        PathBuf::from(String::from_utf8_lossy(&out).to_string())
    };
    list.lines().filter_map(|line| line.split(' ').nth(4)).map(plain).collect()
}

/// The disk a folder is on, as the system numbers it.
fn disk_of(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| m.dev())
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path).ok().map(|_| 0)
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The work is done only where it was allowed when the helper was started.
async fn allowed_to_work(State(h): State<Helper>, req: Request, next: Next) -> Response {
    if !h.0.conf.grants.work {
        return Refused(StatusCode::FORBIDDEN, "this machine was not started to do work for another".into()).into_response();
    }
    next.run(req).await
}

/// What is in the shared folders is told only where that was allowed.
async fn allowed_to_see(State(h): State<Helper>, req: Request, next: Next) -> Response {
    if !h.0.conf.grants.see {
        return Refused(StatusCode::FORBIDDEN, "this machine was not started to say what is in its folders".into()).into_response();
    }
    next.run(req).await
}

/// Which folder to look through, by the share it is in and its place there, and for what.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Look {
    share: String,
    #[serde(default)]
    rel: String,
    kind: crate::models::Kind,
}

/// Something that can be found by looking through a folder here. No link is followed, so
/// nothing outside the shared folders is told of.
trait Findable: Serialize + Send + 'static {
    /// Whether records are removed on the strength of the answer.
    const REMOVES: bool;
    fn find(root: &Path, kind: crate::models::Kind) -> std::result::Result<Self, Blocked>
    where
        Self: Sized;
}

impl Findable for Survey {
    const REMOVES: bool = true;
    fn find(root: &Path, kind: crate::models::Kind) -> std::result::Result<Self, Blocked> {
        crate::local::survey(root, kind, false)
    }
}

impl Findable for Vec<FolderSummary> {
    const REMOVES: bool = false;
    fn find(root: &Path, kind: crate::models::Kind) -> std::result::Result<Self, Blocked> {
        crate::local::folders(root, kind, false)
    }
}

/// Look through one folder and say what is in it. An answer is a whole one: a folder that
/// could not be read to the end is refused, not answered with what was read.
async fn look<T: Findable>(State(h): State<Helper>, Json(ask): Json<Look>) -> R {
    // Its turn is kept until the looking is over, whether or not whoever asked is still there.
    let turn = tokio::time::timeout(LOOK_WAIT, h.0.looks.clone().acquire_owned()).await.ok().and_then(|t| t.ok()).ok_or_else(|| Refused(StatusCode::SERVICE_UNAVAILABLE, "too many folders are being looked through at once".into()))?;
    let found = tokio::task::spawn_blocking(move || {
        let _turn = turn;
        let root = h.looked_at(&ask, T::REMOVES)?;
        T::find(&root, ask.kind).map_err(|e| Refused(if matches!(e, Blocked::Link(_)) { StatusCode::CONFLICT } else { StatusCode::SERVICE_UNAVAILABLE }, e.to_string()))
    })
    .await
    .map_err(|_| Refused(StatusCode::INTERNAL_SERVER_ERROR, "the look did not finish".into()))??;
    Ok(Json(serde_json::to_value(found)?))
}

/// What this end of the link is, speaks and allows.
async fn hello(State(h): State<Helper>) -> Json<Hello> {
    let grants = h.0.conf.grants;
    Json(Hello {
        boot: h.0.boot.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocols: Protocols::allowed(grants),
        grants,
        // Without leave to see or play, what is here is nobody's business.
        shares: if grants.see || grants.play || grants.work { h.0.shares.clone() } else { vec![] },
    })
}

/// One file, to whoever holds a ticket for it. The ticket is the whole of the request:
/// nothing else in the address chooses what is sent.
async fn play(State(h): State<Helper>, req: Request) -> Response {
    let no_store = |r: Response| ([(axum::http::header::CACHE_CONTROL, "private, no-store"), (axum::http::header::REFERRER_POLICY, "no-referrer")], r).into_response();
    if !h.0.conf.grants.play {
        return no_store(Refused(StatusCode::FORBIDDEN, "this machine does not hand files to players".into()).into_response());
    }
    let mut given = req.uri().query().unwrap_or("").split('&').filter_map(|pair| pair.strip_prefix("ticket="));
    let (Some(token), None) = (given.next(), given.next()) else { return no_store(Refused(StatusCode::BAD_REQUEST, "one ticket is needed".into()).into_response()) };
    let Some(linked) = h.linked() else { return no_store(Refused(StatusCode::FORBIDDEN, "this machine is not linked to a Spool yet".into()).into_response()) };
    let ticket = match linked.signer.verify(token, now()) {
        Ok(t) => t,
        Err(ticket::Refused::Expired) => return no_store(Refused(StatusCode::GONE, "the ticket has run out".into()).into_response()),
        Err(ticket::Refused::Bad) => return no_store(Refused(StatusCode::UNAUTHORIZED, "the ticket is not one of ours".into()).into_response()),
    };
    // Taken before the file is opened, and kept until the last of it has gone.
    let Ok(permit) = h.0.streams.clone().try_acquire_owned() else {
        return no_store((StatusCode::SERVICE_UNAVAILABLE, [(axum::http::header::RETRY_AFTER, "2")], "too many files are being played at once").into_response());
    };
    let opened = {
        let h = h.clone();
        tokio::task::spawn_blocking(move || h.open_shared(&ticket)).await
    };
    let (file, path) = match opened {
        Ok(Ok(found)) => found,
        Ok(Err(refused)) => return no_store(refused.into_response()),
        Err(_) => return no_store(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    };
    let head = req.method() == axum::http::Method::HEAD;
    no_store(crate::serve::send(tokio::fs::File::from_std(file), &path, req.headers(), head, permit).await)
}

/// A mark that says whether a code is the one tries were counted against, without being it.
fn code_mark(code: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, format!("spool/link/pair/tries/{code}").as_bytes()).as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairStart {
    /// The asker's message.
    msg: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairFinish {
    session: String,
    proof: String,
}

/// The first half of being linked with a code: the asker's message is answered with this
/// side's, and its proof that it holds the code. Each try counts against the code until
/// it ends in a link, and it is counted before it is answered.
async fn pair_start(State(h): State<Helper>, Json(b): Json<PairStart>) -> R {
    if h.linked().is_some() {
        return Err(Refused(StatusCode::CONFLICT, "this helper is linked already. One linked with a code is linked afresh by stopping it, deleting link.json from its data folder and starting it again; one started with a key is linked by that key".into()));
    }
    let mut p = h.0.pairing.lock();
    let Some(code) = p.code.clone() else { return Err(Refused(StatusCode::CONFLICT, "this helper is not waiting to be linked".into())) };
    p.tries.retain(|_, (began, _)| began.elapsed() < TRY_FOR);
    if p.resting_until > now() {
        return Err(Refused(StatusCode::FORBIDDEN, format!("too many wrong codes have been tried; it takes none for another {} minutes, or start the helper with a new code", (p.resting_until - now()).div_ceil(60))));
    }
    let (giving, msg, proof) = pair::Giving::answer(&code, &b.msg).map_err(|_| Refused(StatusCode::BAD_REQUEST, "that is not the start of a link".into()))?;
    p.failures += 1;
    if p.failures >= TRIES {
        (p.failures, p.resting_until, p.rests) = (0, now() + (REST << p.rests.min(16)).min(LONGEST_REST), p.rests + 1);
    }
    h.keep_tries(&code, &p)?;
    let session = pair::session();
    p.tries.insert(session.clone(), (Instant::now(), giving));
    Ok(Json(json!({"session": session, "msg": msg, "proof": proof})))
}

/// The second half: the asker proves that it holds the code too. The key is written down
/// before the asker is told, and from then on the helper is linked and takes no more codes.
async fn pair_finish(State(h): State<Helper>, Json(b): Json<PairFinish>) -> R {
    let mut p = h.0.pairing.lock();
    // The answer to this was lost the first time, and the asker is asking again.
    if p.done.as_ref().is_some_and(|(session, giving)| *session == b.session && giving.proved(&b.proof)) {
        return Ok(Json(json!({"linked": true})));
    }
    let Some((began, giving)) = p.tries.remove(&b.session) else { return Err(Refused(StatusCode::NOT_FOUND, "no such try; start again".into())) };
    if began.elapsed() >= TRY_FOR || h.linked().is_some() {
        return Err(Refused(StatusCode::CONFLICT, "that try has run out; start again".into()));
    }
    if !giving.proved(&b.proof) {
        return Err(Refused(StatusCode::UNAUTHORIZED, "the code is not the one this helper gave".into()));
    }
    let key = giving.key().to_string();
    write_private(&h.0.data_dir.join("link.json"), &serde_json::to_vec(&json!({"key": key}))?)?;
    *h.0.linked.write() = Some(Linked::new(key));
    let code = p.code.take().unwrap_or_default();
    (p.failures, p.rests, p.resting_until, p.done) = (0, 0, 0, Some((b.session, giving)));
    p.tries.clear();
    let _ = h.keep_tries(&code, &p);
    tracing::info!("this helper is linked to a Spool");
    Ok(Json(json!({"linked": true})))
}

/// Nothing is answered without the key, and the comparison takes the same time whatever is sent.
async fn guard(State(h): State<Helper>, req: Request, next: Next) -> Response {
    let sent = req.headers().get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    // One that is not linked yet has no key, and nothing sent is the key to it.
    let Some(linked) = h.linked() else { return Refused(StatusCode::UNAUTHORIZED, "this helper is not linked to a Spool yet".into()).into_response() };
    let (a, b) = (sent.as_bytes(), linked.key.as_bytes());
    let same = a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0;
    if !same {
        return Refused(StatusCode::UNAUTHORIZED, "the key is missing or wrong".into()).into_response();
    }
    next.run(req).await
}

async fn info(State(h): State<Helper>) -> R {
    let c = &h.0.conf;
    let root = |p: &PathBuf, writable: bool| json!({"path": p, "writable": writable, "there": p.is_dir(), "free": crate::app::free_space(p)});
    let roots: Vec<Value> = c.roots.iter().map(|p| root(p, true)).chain(c.read_roots.iter().map(|p| root(p, false))).collect();
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "boot": h.0.boot,
        "downloads": c.downloads,
        "complete": h.complete(),
        "roots": roots,
        "servers": h.0.pushed.lock().servers.len(),
        "filed": h.work_done().0,
        "moved": h.work_done().1,
        "busy": h.busy(),
    })))
}

/// Write a file that holds passwords: under another name first and then renamed, so it is
/// never half written, and readable by nobody else from its first byte.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.new");
    let _ = std::fs::remove_file(&tmp);
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let mut file = open.open(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

async fn config(State(h): State<Helper>, Json(pushed): Json<Pushed>) -> R {
    let _one_at_a_time = h.0.saving.lock().await;
    if *h.0.pushed.lock() != pushed {
        write_private(&h.0.data_dir.join("helper.json"), &serde_json::to_vec(&pushed)?)?;
        h.0.engine.set_config(Helper::engine_config(&h.0.conf, &pushed));
        *h.0.pushed.lock() = pushed;
    }
    Ok(Json(json!({"ok": true})))
}

/// How things stand, with a mark that is the same for as long as nothing in it changes.
fn report(h: &Helper) -> Value {
    let mut report = json!({
        "boot": h.0.boot,
        "jobs": h.0.engine.jobs(),
        "paused": h.0.engine.is_paused(),
        "free": crate::app::free_space(&h.0.conf.downloads),
        "busy": h.busy(),
    });
    // Room on the disk comes and goes by the byte with whatever else the machine is doing,
    // which is not news. It is marked to the nearest gigabyte.
    let mut marked = report.clone();
    marked["free"] = json!(report["free"].as_u64().map(|free| free >> 30));
    let digest = ring::digest::digest(&ring::digest::SHA256, marked.to_string().as_bytes());
    report["stamp"] = json!(digest.as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect::<String>());
    report
}

#[derive(Deserialize)]
struct Since {
    /// The mark on the report the asker already has.
    #[serde(default)]
    since: String,
    /// How long to wait for something to change before answering anyway, in seconds.
    #[serde(default)]
    wait: u64,
}

/// How things stand. Asked with the mark of the report it already has, the asker is kept
/// waiting until something has changed, so that it hears at once without asking over and
/// over. The whole report is sent each time, and the mark is made from all of it, so
/// nothing that changes can be missed.
async fn jobs(State(h): State<Helper>, Query(q): Query<Since>) -> R {
    let until = Instant::now() + Duration::from_secs(q.wait.min(HOLD));
    loop {
        let now = report(&h);
        if q.since.is_empty() || now["stamp"].as_str() != Some(q.since.as_str()) || Instant::now() >= until {
            return Ok(Json(now));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[derive(Deserialize)]
struct Named {
    name: String,
    /// A name the asker made up for this request. Asked again under the same one, the
    /// download already started is named and no second one is.
    #[serde(default)]
    rid: String,
}

/// The NZB is the body, as it came from the indexer.
async fn add(State(h): State<Helper>, Query(q): Query<Named>, body: axum::body::Bytes) -> R {
    let mut taken = h.0.taken.lock();
    if let Some((id, _)) = taken.get(&q.rid).filter(|_| !q.rid.is_empty()) {
        return Ok(Json(json!({"id": id})));
    }
    let id = h.0.engine.add(&body, &q.name, None)?;
    if !q.rid.is_empty() {
        // Kept far longer than anyone goes on asking again for.
        let at = now();
        taken.retain(|_, (_, then)| at.saturating_sub(*then) < REMEMBERED);
        taken.insert(q.rid, (id.clone(), at));
        if let Err(e) = write_private(&h.0.data_dir.join("taken.json"), &serde_json::to_vec(&*taken)?) {
            tracing::warn!(error = %format!("{e:#}"), "could not write down which downloads were asked for");
        }
    }
    Ok(Json(json!({"id": id})))
}

async fn job_action(State(h): State<Helper>, UrlPath((id, action)): UrlPath<(String, String)>, body: Option<Json<Value>>) -> R {
    let e = &h.0.engine;
    match action.as_str() {
        "pause" => e.pause(&id)?,
        "resume" => e.resume(&id)?,
        "retry" => e.retry(&id)?,
        "hold" => e.pause_because(&id, body.as_ref().and_then(|b| b.0["why"].as_str()).unwrap_or("Waiting"))?,
        other => return Err(Refused(StatusCode::NOT_FOUND, format!("no such action: {other}"))),
    }
    Ok(Json(json!({"ok": true})))
}

async fn remove(State(h): State<Helper>, UrlPath(id): UrlPath<String>, Query(q): Query<HashMap<String, String>>) -> R {
    h.0.engine.remove(&id, q.get("files").is_some_and(|f| f == "true"))?;
    Ok(Json(json!({"ok": true})))
}

async fn queue_action(State(h): State<Helper>, UrlPath(action): UrlPath<String>) -> R {
    match action.as_str() {
        "pause" => h.0.engine.pause_all(),
        "resume" => h.0.engine.resume_all(),
        other => return Err(Refused(StatusCode::NOT_FOUND, format!("no such action: {other}"))),
    }
    Ok(Json(json!({"paused": h.0.engine.is_paused()})))
}

async fn availability(State(h): State<Helper>, Query(q): Query<HashMap<String, String>>, body: axum::body::Bytes) -> R {
    let sample = q.get("sample").and_then(|s| s.parse().ok()).unwrap_or(24);
    let (found, asked) = h.0.engine.availability(&body, sample).await.map_err(|e| anyhow!(e))?;
    Ok(Json(json!({"found": found, "asked": asked})))
}

#[derive(Deserialize)]
struct OnePath {
    path: String,
}

/// Whether a path is there, as the helper sees it. Its driver asks this to be sure the two
/// of them are looking at the same folder.
async fn stat(State(h): State<Helper>, Json(b): Json<OnePath>) -> R {
    let p = h.allowed(&b.path, false)?;
    let m = std::fs::metadata(&p).ok();
    Ok(Json(json!({"exists": m.is_some(), "dir": m.as_ref().is_some_and(|m| m.is_dir()), "size": m.as_ref().map(|m| m.len())})))
}

#[derive(Deserialize)]
struct FromTo {
    from: String,
    to: String,
}

/// File one thing into the library: renamed when it can be, copied, checked and swapped in
/// when the two are on different disks.
async fn file_move(State(h): State<Helper>, Json(b): Json<FromTo>) -> R {
    let (from, to) = (h.allowed(&b.from, true)?, h.allowed(&b.to, true)?);
    if h.0.conf.downloads == to || under(&to, &h.0.conf.downloads) {
        return Err(Refused(StatusCode::FORBIDDEN, "nothing is filed into the downloads folder".into()));
    }
    h.0.filing.fetch_add(1, Ordering::Relaxed);
    let moved = tokio::task::spawn_blocking(move || crate::import::move_file(&from, &to)).await;
    h.0.filing.fetch_sub(1, Ordering::Relaxed);
    moved??;
    h.0.filed.fetch_add(1, Ordering::Relaxed);
    Ok(Json(json!({"ok": true})))
}

/// Clear away what a finished download left: only ever a folder inside the helper's own
/// folder of finished downloads.
async fn output_remove(State(h): State<Helper>, Json(b): Json<OnePath>) -> R {
    let p = h.allowed(&b.path, true)?;
    let complete = h.complete();
    if !under(&p, &complete) || p == complete {
        return Err(Refused(StatusCode::FORBIDDEN, format!("{} is not a finished download", p.display())));
    }
    match tokio::fs::remove_dir_all(&p).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(anyhow!("{} could not be cleared away: {e}", p.display()).into()),
        _ => Ok(Json(json!({"ok": true}))),
    }
}

#[derive(Deserialize)]
struct MoveAsk {
    from: String,
    to: String,
    /// The name the copy is made under, beside where it is going.
    working: String,
    /// The original stays where it is, so the folder is copied even on one disk.
    keep_original: bool,
}

async fn move_start(State(h): State<Helper>, Json(b): Json<MoveAsk>) -> R {
    // The original is read; it is only written to if it is to be taken away afterwards.
    let from = h.allowed(&b.from, !b.keep_original)?;
    let (to, working) = (h.allowed(&b.to, true)?, h.allowed(&b.working, true)?);
    // The working folder is cleared before it is used, so it is only ever one of Spool's
    // own, beside where the title is going, and never a folder that holds anything else.
    let own = working.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".spool-moving-"));
    if working.parent() != to.parent() || !own {
        return Err(Refused(StatusCode::BAD_REQUEST, "the working folder must be beside where the title is going, with a name beginning .spool-moving-".into()));
    }
    let overlap = |a: &Path, b: &Path| under(a, b) || under(b, a);
    if overlap(&from, &to) {
        return Err(Refused(StatusCode::BAD_REQUEST, "a title cannot be moved into itself".into()));
    }
    if overlap(&working, &from) || overlap(&working, &to) {
        return Err(Refused(StatusCode::BAD_REQUEST, "the working folder cannot be the title's own folder, or inside it".into()));
    }
    if !from.is_dir() {
        return Err(Refused(StatusCode::CONFLICT, format!("{} is not there", from.display())));
    }
    if to.exists() && std::fs::read_dir(&to).map(|mut d| d.next().is_some()).unwrap_or(true) {
        return Err(Refused(StatusCode::CONFLICT, format!("{} is already there", to.display())));
    }
    let op = Arc::new(Op { from: from.clone(), done: Default::default(), total: AtomicU64::new(0), stop: Default::default(), state: Mutex::new("running".into()), files: Mutex::new(vec![]), renamed: AtomicBool::new(false), ended: Mutex::new(None) });
    let id = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    {
        let mut ops = h.0.ops.lock();
        ops.retain(|_, o| o.ended.lock().is_none_or(|at| at.elapsed() < Duration::from_secs(24 * 3600)));
        ops.insert(id.clone(), op.clone());
    }
    let (keep, counted) = (b.keep_original, h.clone());
    tokio::task::spawn_blocking(move || {
        let run = || -> Result<()> {
            let files = listing(&from);
            op.total.store(files.iter().map(|f| f.1).sum(), Ordering::Relaxed);
            *op.files.lock() = files.clone();
            let _ = std::fs::remove_dir_all(&working);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
            }
            // On one disk a folder is moved by renaming it, which takes no time.
            if !keep && crate::relocate::pace().is_none() && std::fs::rename(&from, &to).is_ok() {
                op.renamed.store(true, Ordering::Relaxed);
                op.done.store(op.total.load(Ordering::Relaxed), Ordering::Relaxed);
                return Ok(());
            }
            let copied = copy_all(&from, &working, &files, &op.done, &op.stop).and_then(|_| std::fs::rename(&working, &to).with_context(|| format!("naming {}", to.display())));
            if copied.is_err() {
                let _ = std::fs::remove_dir_all(&working);
            }
            copied
        };
        let ended = match run() {
            Ok(()) => "done".to_string(),
            Err(e) if e.to_string() == CALLED_OFF => "cancelled".to_string(),
            Err(e) => format!("{e:#}"),
        };
        if ended == "done" {
            counted.0.moved.fetch_add(1, Ordering::Relaxed);
        }
        *op.ended.lock() = Some(Instant::now());
        *op.state.lock() = ended;
    });
    Ok(Json(json!({"id": id})))
}

fn op_of(h: &Helper, id: &str) -> std::result::Result<Arc<Op>, Refused> {
    h.0.ops.lock().get(id).cloned().ok_or_else(|| Refused(StatusCode::NOT_FOUND, "no such move; the helper may have been restarted in the middle of it".into()))
}

async fn move_state(State(h): State<Helper>, UrlPath(id): UrlPath<String>) -> R {
    let op = op_of(&h, &id)?;
    let state = op.state.lock().clone();
    Ok(Json(json!({"state": state, "done": op.done.load(Ordering::Relaxed), "total": op.total.load(Ordering::Relaxed)})))
}

async fn move_cancel(State(h): State<Helper>, UrlPath(id): UrlPath<String>) -> R {
    op_of(&h, &id)?.stop.store(true, Ordering::Relaxed);
    Ok(Json(json!({"ok": true})))
}

/// The move is recorded where the catalogue is kept, so the original may go: exactly the
/// files that were copied, and nothing if the original is to stay.
async fn move_finish(State(h): State<Helper>, UrlPath(id): UrlPath<String>, Json(b): Json<Value>) -> R {
    let op = op_of(&h, &id)?;
    if *op.state.lock() == "running" {
        return Err(Refused(StatusCode::CONFLICT, "the move is still under way".into()));
    }
    if b["remove_original"].as_bool() == Some(true) && *op.state.lock() == "done" && !op.renamed.load(Ordering::Relaxed) {
        // Asked for again here: a folder this helper only reads is never emptied.
        let from = h.allowed(&op.from.to_string_lossy(), true)?;
        let files = op.files.lock().clone();
        tokio::task::spawn_blocking(move || remove_all(&from, &files)).await?;
    }
    h.0.ops.lock().remove(&id);
    Ok(Json(json!({"ok": true})))
}

/// Run a helper until it is told to stop.
pub async fn serve(bind: &str, data_dir: &Path, conf: Conf) -> Result<()> {
    for dir in [conf.downloads.join("incomplete"), conf.downloads.join("complete")] {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    // A move cut short by a restart is forgotten, so what it had copied is cleared away.
    for root in &conf.roots {
        for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(".spool-moving-") {
                tracing::info!(folder = %e.path().display(), "clearing away a copy an interrupted move left behind");
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let helper = Helper::start(data_dir, conf.clone()).await?;
    let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("binding {bind}"))?;
    let name = std::env::var("SPOOL_NAME").ok().filter(|n| !n.trim().is_empty()).unwrap_or_else(crate::link::find::host_name);
    helper.announce(&name, listener.local_addr()?.port());
    tracing::info!(%bind, %name, downloads = %conf.downloads.display(), files_into = ?conf.roots, only_reads = ?conf.read_roots, "Spool's helper is running");
    let engine = helper.engine().clone();
    axum::serve(listener, helper.router())
        .with_graceful_shutdown(async move {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            tracing::info!("shutting down; saving download progress");
            engine.shutdown().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_points_are_read_from_the_list_linux_keeps() {
        let list = "36 35 0:31 / /turk/movies rw,noatime shared:1 - zfs Goliath/media/movies rw\n37 35 0:32 / /turk/tv\\040shows/a\\134b rw - zfs Goliath/tv rw\nshort line\n";
        assert_eq!(mount_points(list), [PathBuf::from("/turk/movies"), PathBuf::from("/turk/tv shows/a\\b")]);
        assert!(mount_points("").is_empty());
    }

    #[test]
    fn a_file_is_opened_inside_its_folder_and_no_link_is_followed() {
        use std::io::Read;
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let (share, outside) = (tmp.path().join("share"), tmp.path().join("outside"));
        for dir in [share.join("Film (2001)/Extras"), outside.clone()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(share.join("Film (2001)/Extras/film.mkv"), b"the film").unwrap();
        std::fs::write(outside.join("film.mkv"), b"something else").unwrap();
        let read = |root: &Path, rel: &str| open_inside(root, rel).map(|mut f| {
            let mut all = String::new();
            f.read_to_string(&mut all).unwrap();
            all
        });
        let refused = |root: &Path, rel: &str| match open_inside(root, rel) {
            Ok(_) => "opened",
            Err(Inside::Link) => "link",
            Err(Inside::Missing) => "missing",
        };
        assert_eq!(read(&share, "Film (2001)/Extras/film.mkv").ok().as_deref(), Some("the film"));
        // The shared folder may itself be reached by a link.
        symlink(&share, tmp.path().join("mounted")).unwrap();
        assert_eq!(read(&tmp.path().join("mounted"), "Film (2001)/Extras/film.mkv").ok().as_deref(), Some("the film"));
        assert_eq!(refused(&share, "Film (2001)/Extras/none.mkv"), "missing");
        assert_eq!(refused(&share, "Film (2001)/Extras"), "missing", "a folder is not a file");
        assert_eq!(refused(&share, "Film (2001)/Extras/film.mkv/more"), "missing");
        assert_eq!(refused(&share, "../outside/film.mkv"), "missing");
        // A link as the file, and a link as a folder on the way, whether it leads out of the
        // share or to somewhere else inside it.
        symlink(outside.join("film.mkv"), share.join("Film (2001)/leak.mkv")).unwrap();
        symlink(&outside, share.join("Way out")).unwrap();
        symlink(share.join("Film (2001)"), share.join("Same film")).unwrap();
        assert_eq!(refused(&share, "Film (2001)/leak.mkv"), "link");
        assert_eq!(refused(&share, "Way out/film.mkv"), "link");
        assert_eq!(refused(&share, "Same film/Extras/film.mkv"), "link");
        // A folder swapped for a link after it was looked at is the case that matters, and
        // it is the same case: the folder that is now a link is not gone through.
        std::fs::rename(share.join("Film (2001)/Extras"), share.join("Film (2001)/Extras.was")).unwrap();
        symlink(&outside, share.join("Film (2001)/Extras")).unwrap();
        assert_eq!(refused(&share, "Film (2001)/Extras/film.mkv"), "link");
    }
}
