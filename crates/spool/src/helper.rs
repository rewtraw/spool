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

/// What the two sides must agree on to work together. Raised when either would
/// misunderstand the other.
pub const API: u32 = 1;

/// What a helper is started with.
#[derive(Clone, Debug)]
pub struct Conf {
    /// Whoever drives the helper sends this with every request.
    pub key: String,
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
/// How long the name of a request for a download is remembered, in seconds.
const REMEMBERED: u64 = 3600;

impl Conf {
    /// From the environment, which is how a container is told things.
    pub fn from_env() -> Result<Conf> {
        let list = |name: &str| -> Vec<PathBuf> { std::env::var(name).unwrap_or_default().split(':').map(str::trim).filter(|p| !p.is_empty()).map(PathBuf::from).collect() };
        let conf = Conf {
            key: std::env::var("SPOOL_HELPER_KEY").unwrap_or_default().trim().to_string(),
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
        if self.key.len() < 16 {
            bail!("SPOOL_HELPER_KEY must be set to a key of at least 16 characters; the Spool that drives this helper is given the same one");
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
    signer: Signer,
    streams: Arc<tokio::sync::Semaphore>,
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
        let shares = conf.roots.iter().map(|p| Share::new(p, true)).chain(conf.read_roots.iter().map(|p| Share::new(p, false))).collect();
        let signer = Signer::new(&conf.key);
        Ok(Helper(Arc::new(Inner {
            engine,
            conf,
            data_dir: data_dir.to_path_buf(),
            boot,
            pushed: Mutex::new(pushed),
            saving: Default::default(),
            shares,
            signer,
            streams: Arc::new(tokio::sync::Semaphore::new(STREAMS)),
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
        let play = Router::new().route("/link/play/v1/file", get(play)).with_state(self.clone());
        let hello = Router::new().route("/link", get(hello)).layer(axum::middleware::from_fn_with_state(self.clone(), guard)).with_state(self.clone());
        play.merge(hello).merge(self.work())
    }

    fn work(&self) -> Router {
        Router::new()
            .route("/helper/info", get(info))
            .route("/helper/config", put(config))
            .route("/helper/jobs", get(jobs).post(add))
            .route("/helper/jobs/{id}", axum::routing::delete(remove))
            .route("/helper/jobs/{id}/{action}", post(job_action))
            .route("/helper/queue/{action}", post(queue_action))
            .route("/helper/availability", post(availability))
            .route("/helper/fs/stat", post(stat))
            .route("/helper/fs/move", post(file_move))
            .route("/helper/fs/remove", post(output_remove))
            .route("/helper/moves", post(move_start))
            .route("/helper/moves/{id}", get(move_state))
            .route("/helper/moves/{id}/cancel", post(move_cancel))
            .route("/helper/moves/{id}/finish", post(move_finish))
            .layer(axum::extract::DefaultBodyLimit::max(64 << 20))
            .layer(axum::middleware::from_fn_with_state(self.clone(), allowed_to_work))
            .layer(axum::middleware::from_fn_with_state(self.clone(), guard))
            .with_state(self.clone())
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

/// What this end of the link is, speaks and allows.
async fn hello(State(h): State<Helper>) -> Json<Hello> {
    let grants = h.0.conf.grants;
    Json(Hello {
        boot: h.0.boot.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocols: Protocols { work: if grants.work { crate::link::WORK.to_vec() } else { vec![] }, play: if grants.play { crate::link::PLAY.to_vec() } else { vec![] } },
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
    let ticket = match h.0.signer.verify(token, now()) {
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

/// Nothing is answered without the key, and the comparison takes the same time whatever is sent.
async fn guard(State(h): State<Helper>, req: Request, next: Next) -> Response {
    let sent = req.headers().get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    let (a, b) = (sent.as_bytes(), h.0.conf.key.as_bytes());
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
        "api": API,
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

async fn jobs(State(h): State<Helper>) -> R {
    Ok(Json(json!({
        "boot": h.0.boot,
        "jobs": h.0.engine.jobs(),
        "paused": h.0.engine.is_paused(),
        "free": crate::app::free_space(&h.0.conf.downloads),
        "busy": h.busy(),
    })))
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
    tracing::info!(%bind, downloads = %conf.downloads.display(), files_into = ?conf.roots, only_reads = ?conf.read_roots, "Spool's helper is running");
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
