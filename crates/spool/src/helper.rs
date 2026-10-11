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
}

impl Conf {
    /// From the environment, which is how a container is told things.
    pub fn from_env() -> Result<Conf> {
        let list = |name: &str| -> Vec<PathBuf> { std::env::var(name).unwrap_or_default().split(':').map(str::trim).filter(|p| !p.is_empty()).map(PathBuf::from).collect() };
        let conf = Conf {
            key: std::env::var("SPOOL_HELPER_KEY").unwrap_or_default().trim().to_string(),
            roots: list("SPOOL_HELPER_ROOTS"),
            read_roots: list("SPOOL_HELPER_READ_ROOTS"),
            downloads: PathBuf::from(std::env::var("SPOOL_HELPER_DOWNLOADS").unwrap_or_default().trim()),
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
        let pushed: Pushed = std::fs::read(data_dir.join("helper.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let engine = Engine::start(Self::engine_config(&conf, &pushed)).await?;
        let boot = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        Ok(Helper(Arc::new(Inner { engine, conf, data_dir: data_dir.to_path_buf(), boot, pushed: Mutex::new(pushed), ops: Mutex::new(HashMap::new()), filing: AtomicUsize::new(0), filed: AtomicUsize::new(0), moved: AtomicUsize::new(0) })))
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
        let moving = self.0.ops.lock().values().filter(|o| *o.state.lock() == "running").count();
        if moving > 0 {
            reasons.push(format!("{moving} {} being moved", if moving == 1 { "title is" } else { "titles are" }));
        }
        reasons
    }

    pub fn router(&self) -> Router {
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
            .layer(axum::middleware::from_fn_with_state(self.clone(), guard))
            .with_state(self.clone())
    }
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

async fn config(State(h): State<Helper>, Json(pushed): Json<Pushed>) -> R {
    if *h.0.pushed.lock() != pushed {
        // It holds Usenet passwords, so nobody else on the machine is to read it.
        let path = h.0.data_dir.join("helper.json");
        std::fs::write(&path, serde_json::to_vec(&pushed)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
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
}

/// The NZB is the body, as it came from the indexer.
async fn add(State(h): State<Helper>, Query(q): Query<Named>, body: axum::body::Bytes) -> R {
    let id = h.0.engine.add(&body, &q.name, None)?;
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
    let _ = tokio::fs::remove_dir_all(&p).await;
    Ok(Json(json!({"ok": true})))
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
    if working.parent() != to.parent() {
        return Err(Refused(StatusCode::BAD_REQUEST, "the working folder must be beside where the title is going".into()));
    }
    if !from.is_dir() {
        return Err(Refused(StatusCode::CONFLICT, format!("{} is not there", from.display())));
    }
    if to.exists() && std::fs::read_dir(&to).map(|mut d| d.next().is_some()).unwrap_or(true) {
        return Err(Refused(StatusCode::CONFLICT, format!("{} is already there", to.display())));
    }
    let op = Arc::new(Op { from: from.clone(), done: Default::default(), total: AtomicU64::new(0), stop: Default::default(), state: Mutex::new("running".into()), files: Mutex::new(vec![]), renamed: AtomicBool::new(false) });
    let id = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.0.ops.lock().insert(id.clone(), op.clone());
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
