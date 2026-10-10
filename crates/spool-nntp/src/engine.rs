//! The download engine: a durable queue of jobs, a pool of NNTP connections per server, and
//! bounded, resumable assembly of decoded articles into files.
//!
//! Durability model: decoded data is written straight to its final offset. Every ten seconds the
//! engine takes a snapshot of which segments are written, flushes the files, then saves the
//! snapshot. After a crash, anything newer than the last saved snapshot is fetched again.

use crate::nntp::{Connection, FetchError, ServerConfig};
use crate::nzb::{self, Nzb};
use crate::yenc::{self, Decoded};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Notify, Semaphore};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    pub servers: Vec<ServerConfig>,
    pub incomplete_dir: PathBuf,
    pub complete_dir: PathBuf,
    /// Path to the `par2` helper. Found on PATH when empty.
    pub par2_path: String,
    /// Path to `7zz`. Found on PATH when empty.
    pub sevenzip_path: String,
    /// Bytes per second across all connections. Zero is unlimited.
    pub speed_limit: u64,
    /// Unpack uncompressed RAR posts while they download, deleting each volume once verified.
    pub direct_unpack: bool,
    /// How many finished downloads may be verified, repaired and unpacked at the same time.
    /// Read when the engine starts.
    pub post_parallel: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig { servers: vec![], incomplete_dir: PathBuf::new(), complete_dir: PathBuf::new(), par2_path: String::new(), sevenzip_path: String::new(), speed_limit: 0, direct_unpack: true, post_parallel: 2 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Downloading,
    Paused,
    Verifying,
    Repairing,
    Extracting,
    Finishing,
    Completed,
    Failed,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, JobState::Completed | JobState::Failed)
    }
    fn in_post(self) -> bool {
        matches!(self, JobState::Verifying | JobState::Repairing | JobState::Extracting | JobState::Finishing)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct FileProgress {
    /// Name on disk inside the job directory, once the first article has told us.
    pub name: Option<String>,
    pub done: Vec<bool>,
    /// Segments no server could supply.
    pub missing: Vec<u32>,
    /// Segments written despite failing their checksum on every server.
    pub damaged: Vec<u32>,
    /// PAR2 recovery volumes are fetched only if repair needs them.
    pub deferred: bool,
    pub size: Option<u64>,
}

impl FileProgress {
    pub fn complete(&self) -> bool {
        self.done.iter().all(|d| *d)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct JobRecord {
    pub id: String,
    pub name: String,
    pub password: Option<String>,
    pub added_at: i64,
    pub state: JobState,
    pub message: String,
    pub error: Option<String>,
    pub output_path: Option<PathBuf>,
    pub files: Vec<FileProgress>,
    pub total_bytes: u64,
    pub done_bytes: u64,
    /// The archive was unpacked while downloading; nothing is left to verify or extract.
    #[serde(default)]
    pub direct_done: bool,
    /// Unpacking while downloading was tried and is not possible for this job.
    #[serde(default)]
    pub direct_off: bool,
}

/// What callers see of a job.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobStatus {
    pub id: String,
    pub name: String,
    pub state: JobState,
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub speed: u64,
    pub eta_secs: Option<u64>,
    pub missing_articles: usize,
    pub damaged_articles: usize,
    /// One line describing what is happening now, written for a person.
    pub message: String,
    pub error: Option<String>,
    pub output_path: Option<PathBuf>,
    pub added_at: i64,
    /// The archive was unpacked while it downloaded.
    #[serde(default)]
    pub unpacked_directly: bool,
}

pub(crate) struct Job {
    pub id: String,
    pub dir: PathBuf,
    pub nzb: Nzb,
    pub rec: Mutex<JobRecord>,
    handles: Mutex<HashMap<usize, Arc<File>>>,
    cancel: AtomicBool,
    speed: AtomicU64,
    /// Bytes actually received, for speed. Progress also counts segments given up on.
    fetched: AtomicU64,
    /// Set while the job is being checked, repaired and unpacked. That work fetches recovery
    /// data under the downloading state, and the queue must not take the job for a new download.
    in_post: AtomicBool,
}

impl Job {
    pub fn status(&self) -> JobStatus {
        let r = self.rec.lock();
        let speed = self.speed.load(Ordering::Relaxed);
        let remaining = r.total_bytes.saturating_sub(r.done_bytes);
        JobStatus {
            id: r.id.clone(),
            name: r.name.clone(),
            state: r.state,
            total_bytes: r.total_bytes,
            done_bytes: r.done_bytes,
            speed: if r.state == JobState::Downloading { speed } else { 0 },
            eta_secs: if r.state == JobState::Downloading && speed > 0 { Some(remaining / speed) } else { None },
            missing_articles: r.files.iter().map(|f| f.missing.len()).sum(),
            damaged_articles: r.files.iter().map(|f| f.damaged.len()).sum(),
            message: r.message.clone(),
            error: r.error.clone(),
            output_path: r.output_path.clone(),
            added_at: r.added_at,
            unpacked_directly: r.direct_done,
        }
    }

    pub fn set_state(&self, state: JobState, message: impl Into<String>) {
        let mut r = self.rec.lock();
        r.state = state;
        r.message = message.into();
    }

    pub fn save(&self) -> std::io::Result<()> {
        let snapshot = self.rec.lock().clone();
        self.save_record(&snapshot)
    }

    fn save_record(&self, rec: &JobRecord) -> std::io::Result<()> {
        let tmp = self.dir.join("job.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(rec)?)?;
        std::fs::rename(tmp, self.dir.join("job.json"))
    }

    /// Snapshot progress, make the data it describes durable, then persist the snapshot.
    fn checkpoint(&self) -> std::io::Result<()> {
        let snapshot = self.rec.lock().clone();
        let handles: Vec<Arc<File>> = self.handles.lock().values().cloned().collect();
        for h in handles {
            h.sync_data()?;
        }
        self.save_record(&snapshot)
    }

    fn handle(&self, file: usize, article_name: &str) -> std::io::Result<Arc<File>> {
        if let Some(h) = self.handles.lock().get(&file) {
            return Ok(h.clone());
        }
        let name = {
            let mut r = self.rec.lock();
            match r.files[file].name.clone() {
                Some(n) => n,
                None => {
                    let mut name = sanitize_name(article_name)
                        .or_else(|| self.nzb.files[file].subject_filename().and_then(|n| sanitize_name(&n)))
                        .unwrap_or_else(|| format!("file_{file:04}"));
                    if r.files.iter().any(|f| f.name.as_deref() == Some(name.as_str())) {
                        name = format!("{file:04}_{name}");
                    }
                    r.files[file].name = Some(name.clone());
                    name
                }
            }
        };
        let f = Arc::new(std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(self.dir.join(&name))?);
        self.handles.lock().insert(file, f.clone());
        Ok(f)
    }

    fn write_segment(&self, file: usize, seg: usize, d: &Decoded, damaged: bool) -> std::io::Result<()> {
        let h = self.handle(file, &d.name)?;
        h.write_all_at(&d.data, d.offset)?;
        self.fetched.fetch_add(self.nzb.files[file].segments[seg].bytes, Ordering::Relaxed);
        let mut r = self.rec.lock();
        let bytes = self.nzb.files[file].segments[seg].bytes;
        let fp = &mut r.files[file];
        if !fp.done[seg] {
            fp.done[seg] = true;
            fp.size = Some(d.file_size);
            if damaged {
                fp.damaged.push(seg as u32);
            }
            r.done_bytes += bytes;
        }
        Ok(())
    }

    fn mark_missing(&self, file: usize, seg: usize) {
        let mut r = self.rec.lock();
        let bytes = self.nzb.files[file].segments[seg].bytes;
        let fp = &mut r.files[file];
        if !fp.done[seg] {
            fp.done[seg] = true;
            fp.missing.push(seg as u32);
            r.done_bytes += bytes;
        }
    }

    pub(crate) fn close_handles(&self) {
        self.handles.lock().clear();
    }
}

/// Reduce an article-supplied name to a safe file name inside the job directory.
pub(crate) fn sanitize_name(raw: &str) -> Option<String> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    let cleaned = fit_name(cleaned.trim().trim_start_matches('.'));
    if cleaned.is_empty() || cleaned == ".." {
        None
    } else {
        Some(cleaned)
    }
}

/// Shorten a name to what a filesystem accepts for one path component, which is counted in
/// bytes, so a long name in Chinese or Japanese runs out far sooner than it looks. The
/// extension is kept; the cut falls between characters.
pub(crate) fn fit_name(name: &str) -> String {
    const LIMIT: usize = 200;
    if name.len() <= LIMIT {
        return name.to_string();
    }
    let ext = name.rfind('.').map(|i| &name[i..]).filter(|e| e.len() <= 12 && e.is_ascii()).unwrap_or("");
    let stem = &name[..name.len() - ext.len()];
    let mut cut = LIMIT - ext.len();
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{ext}", stem[..cut].trim_end())
}

pub(crate) fn safe_dir_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { ' ' } else { c }).collect();
    let s = s.trim().trim_matches('.').trim();
    // A folder name has no extension to keep.
    let mut cut = s.len().min(200);
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let s = s[..cut].trim().to_string();
    if s.is_empty() {
        "download".into()
    } else {
        s
    }
}

// ---------------------------------------------------------------- dispatch

struct Work {
    file: usize,
    seg: usize,
    /// Bit per server that has already failed to supply this segment.
    tried: u64,
    attempts: u8,
    /// A copy that decoded but failed its checksum, kept in case nothing better turns up.
    bad: Option<Decoded>,
}

struct Dispatch {
    queues: Vec<VecDeque<Work>>,
    in_flight: usize,
    down: u64,
    /// Server indexes at each priority level, most preferred level first.
    levels: Vec<Vec<usize>>,
    server_level: Vec<usize>,
    unreachable: bool,
    server_errors: Vec<Option<String>>,
}

impl Dispatch {
    fn new(servers: &[ServerConfig]) -> Dispatch {
        let mut priorities: Vec<u32> = servers.iter().map(|s| s.priority).collect();
        priorities.sort_unstable();
        priorities.dedup();
        let levels: Vec<Vec<usize>> = priorities.iter().map(|p| (0..servers.len()).filter(|i| servers[*i].priority == *p).collect()).collect();
        let server_level = servers.iter().map(|s| priorities.iter().position(|p| *p == s.priority).unwrap()).collect();
        Dispatch { queues: levels.iter().map(|_| VecDeque::new()).collect(), in_flight: 0, down: 0, levels, server_level, unreachable: false, server_errors: vec![None; servers.len()] }
    }

    /// The most preferred level that still has a server worth asking.
    fn route(&self, w: &Work) -> Option<usize> {
        self.levels.iter().position(|servers| servers.iter().any(|s| (w.tried | self.down) & (1 << s) == 0))
    }

    fn push(&mut self, w: Work, front: bool) -> Option<Work> {
        if self.unreachable {
            // Nothing was learned about this segment; keep it for the next attempt.
            self.queues[0].push_back(w);
            return None;
        }
        match self.route(&w) {
            Some(level) => {
                if front {
                    self.queues[level].push_front(w)
                } else {
                    self.queues[level].push_back(w)
                }
                None
            }
            None => Some(w),
        }
    }

    fn take(&mut self, server: usize, max: usize) -> Vec<Work> {
        let q = &mut self.queues[self.server_level[server]];
        let mut out = Vec::new();
        let mut i = 0;
        while i < q.len() && out.len() < max {
            if q[i].tried & (1 << server) == 0 {
                out.push(q.remove(i).unwrap());
            } else {
                i += 1;
            }
        }
        self.in_flight += out.len();
        out
    }

    /// The server answered and does not have a usable copy. Returns the work if nobody is left to ask.
    fn failed_on(&mut self, mut w: Work, server: usize) -> Option<Work> {
        self.in_flight -= 1;
        w.tried |= 1 << server;
        self.push(w, false)
    }

    /// The connection broke before we got an answer.
    fn requeue(&mut self, mut w: Work, server: usize) -> Option<Work> {
        w.attempts += 1;
        // An article that keeps killing the connection is treated as one this server cannot supply.
        if w.attempts >= 6 {
            return self.failed_on(w, server);
        }
        self.in_flight -= 1;
        self.push(w, true)
    }

    fn mark_down(&mut self, server: usize, why: String) -> Vec<Work> {
        self.down |= 1 << server;
        self.server_errors[server] = Some(why);
        let all: u64 = (0..self.server_level.len()).fold(0, |m, i| m | (1 << i));
        if self.down & all == all {
            self.unreachable = true;
            return vec![];
        }
        let pending: Vec<Work> = self.queues.iter_mut().flat_map(|q| q.drain(..)).collect();
        pending.into_iter().filter_map(|w| self.push(w, false)).collect()
    }

    fn finished(&self) -> bool {
        self.unreachable || (self.in_flight == 0 && self.queues.iter().all(|q| q.is_empty()))
    }
}

// ---------------------------------------------------------------- rate limit

struct Limiter {
    rate: AtomicU64,
    state: Mutex<(Instant, f64)>,
}

impl Limiter {
    fn new(rate: u64) -> Limiter {
        Limiter { rate: AtomicU64::new(rate), state: Mutex::new((Instant::now(), 0.0)) }
    }
    async fn consume(&self, bytes: u64) {
        let rate = self.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return;
        }
        let wait = {
            let mut s = self.state.lock();
            let now = Instant::now();
            let earned = now.duration_since(s.0).as_secs_f64() * rate as f64;
            s.1 = (s.1 + earned).min(rate as f64) - bytes as f64;
            s.0 = now;
            if s.1 < 0.0 {
                -s.1 / rate as f64
            } else {
                0.0
            }
        };
        if wait > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(wait.min(5.0))).await;
        }
    }
}

// ---------------------------------------------------------------- engine

pub(crate) struct Inner {
    pub cfg: RwLock<EngineConfig>,
    jobs: Mutex<Vec<Arc<Job>>>,
    wake: Notify,
    tx: broadcast::Sender<JobStatus>,
    paused: AtomicBool,
    /// Limits how many jobs are post-processed at once.
    post_slots: Semaphore,
    semaphores: Mutex<HashMap<String, Arc<Semaphore>>>,
    limiter: Limiter,
    shutdown: AtomicBool,
}

#[derive(Clone)]
pub struct Engine(Arc<Inner>);

#[derive(Debug, PartialEq)]
pub(crate) enum FetchOutcome {
    Complete,
    Stopped,
    /// No server could be reached at all; nothing was learned about the articles.
    Unreachable(String),
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Nzb(#[from] nzb::NzbError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("no such job")]
    NotFound,
    #[error("NZB has more than 64 usenet servers configured")]
    TooManyServers,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Inner {
    fn semaphore(&self, s: &ServerConfig) -> Arc<Semaphore> {
        self.semaphores.lock().entry(s.id.clone()).or_insert_with(|| Arc::new(Semaphore::new(s.connections.max(1) as usize))).clone()
    }

    pub(crate) fn publish(&self, job: &Job) {
        let _ = self.tx.send(job.status());
    }

    fn stopped(&self, job: &Job) -> bool {
        job.cancel.load(Ordering::Relaxed)
            || self.paused.load(Ordering::Relaxed)
            || self.shutdown.load(Ordering::Relaxed)
            || job.rec.lock().state == JobState::Paused
    }

    /// Fetch the given files of a job. Safe to call again after a crash or pause: finished
    /// segments are skipped.
    pub(crate) async fn fetch_files(self: &Arc<Self>, job: &Arc<Job>, files: &[usize]) -> FetchOutcome {
        let servers: Vec<ServerConfig> = self.cfg.read().servers.iter().filter(|s| s.enabled && !s.host.is_empty()).cloned().collect();
        if servers.is_empty() {
            return FetchOutcome::Unreachable("no usenet servers are configured".into());
        }
        let mut dispatch = Dispatch::new(&servers);
        {
            let r = job.rec.lock();
            for &f in files {
                for (s, done) in r.files[f].done.iter().enumerate() {
                    if !*done {
                        dispatch.queues[0].push_back(Work { file: f, seg: s, tried: 0, attempts: 0, bad: None });
                    }
                }
            }
        }
        let dispatch = Arc::new(Mutex::new(dispatch));
        let idle = Arc::new(Notify::new());

        let mut workers = Vec::new();
        for (si, server) in servers.iter().enumerate() {
            for _ in 0..server.connections.max(1) {
                workers.push(tokio::spawn(worker(self.clone(), job.clone(), server.clone(), si, dispatch.clone(), idle.clone())));
            }
        }

        // Sample speed, publish status and checkpoint while the workers run.
        let sampler = {
            let (inner, job) = (self.clone(), job.clone());
            tokio::spawn(async move {
                let mut last = job.fetched.load(Ordering::Relaxed);
                let mut last_at = Instant::now();
                let mut last_checkpoint = Instant::now();
                let mut smooth = 0f64;
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    // Under load this task can be late; divide by the time that really passed.
                    let (done, now) = (job.fetched.load(Ordering::Relaxed), Instant::now());
                    let rate = done.saturating_sub(last) as f64 / now.duration_since(last_at).as_secs_f64().max(0.001);
                    smooth = if smooth == 0.0 { rate } else { smooth * 0.7 + rate * 0.3 };
                    job.speed.store(smooth as u64, Ordering::Relaxed);
                    (last, last_at) = (done, now);
                    inner.publish(&job);
                    if last_checkpoint.elapsed() >= Duration::from_secs(10) {
                        last_checkpoint = Instant::now();
                        let j = job.clone();
                        let _ = tokio::task::spawn_blocking(move || j.checkpoint()).await;
                    }
                }
            })
        };

        for w in workers {
            let _ = w.await;
        }
        sampler.abort();
        job.speed.store(0, Ordering::Relaxed);
        let j = job.clone();
        let _ = tokio::task::spawn_blocking(move || j.checkpoint()).await;

        let d = dispatch.lock();
        if d.unreachable {
            let why = d.server_errors.iter().flatten().cloned().collect::<Vec<_>>().join("; ");
            return FetchOutcome::Unreachable(why);
        }
        if d.in_flight == 0 && d.queues.iter().all(|q| q.is_empty()) {
            FetchOutcome::Complete
        } else {
            FetchOutcome::Stopped
        }
    }
}

fn settle(job: &Job, leftovers: Vec<Work>) {
    if leftovers.is_empty() {
        return;
    }
    for w in leftovers {
        finalize_failure(job, w);
    }
    // Recovery sets rarely cover more than a tenth of a post. Once a quarter of it is known to be
    // gone from every server, fetching the rest only wastes time and bandwidth.
    let total: usize = job.nzb.files.iter().map(|f| f.segments.len()).sum();
    let mut r = job.rec.lock();
    let missing: usize = r.files.iter().map(|f| f.missing.len()).sum();
    if missing >= 50 && missing * 4 >= total && !r.state.is_terminal() {
        r.state = JobState::Failed;
        r.message = "Failed".into();
        r.error = Some(format!("too much of the post is missing from every server ({missing} of {total} articles so far); it cannot be repaired"));
        job.cancel.store(true, Ordering::Relaxed);
    }
}

/// Nobody could supply a clean copy. Keep a damaged one if we have it so repair needs fewer blocks.
fn finalize_failure(job: &Job, w: Work) {
    match w.bad {
        Some(d) => {
            if job.write_segment(w.file, w.seg, &d, true).is_err() {
                job.mark_missing(w.file, w.seg);
            }
        }
        None => job.mark_missing(w.file, w.seg),
    }
}

async fn worker(inner: Arc<Inner>, job: Arc<Job>, server: ServerConfig, si: usize, dispatch: Arc<Mutex<Dispatch>>, idle: Arc<Notify>) {
    let sem = inner.semaphore(&server);
    let Ok(_permit) = sem.acquire_owned().await else { return };
    let mut conn: Option<Connection> = None;
    let mut connect_failures = 0u32;
    let depth = server.pipeline.clamp(1, 16) as usize;

    loop {
        if inner.stopped(&job) {
            break;
        }
        let batch = {
            let mut d = dispatch.lock();
            if d.finished() || d.down & (1 << si) != 0 {
                break;
            }
            d.take(si, depth)
        };
        if batch.is_empty() {
            let _ = tokio::time::timeout(Duration::from_millis(150), idle.notified()).await;
            continue;
        }

        if conn.is_none() {
            match Connection::connect(&server).await {
                Ok(c) => {
                    conn = Some(c);
                    connect_failures = 0;
                }
                Err(e) => {
                    connect_failures += 1;
                    let fatal = matches!(e, FetchError::Auth(_)) || connect_failures >= 3;
                    let leftovers = {
                        let mut d = dispatch.lock();
                        let mut leftovers: Vec<Work> = Vec::new();
                        if fatal {
                            tracing::warn!(server = %server.name, error = %e, "usenet server unavailable for this job");
                            leftovers.extend(d.mark_down(si, format!("{}: {e}", server.name)));
                        }
                        for w in batch {
                            d.in_flight -= 1;
                            leftovers.extend(d.push(w, true));
                        }
                        leftovers
                    };
                    settle(&job, leftovers);
                    idle.notify_waiters();
                    if fatal {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500 * (1 << connect_failures.min(4)) as u64)).await;
                    continue;
                }
            }
        }

        let c = conn.as_mut().unwrap();
        let mut send_failed = false;
        for w in &batch {
            let id = &job.nzb.files[w.file].segments[w.seg].message_id;
            if c.send_body(id).await.is_err() {
                send_failed = true;
                break;
            }
        }

        let mut broken = send_failed;
        for mut w in batch {
            if broken {
                let left = dispatch.lock().requeue(w, si);
                settle(&job, left.into_iter().collect());
                continue;
            }
            match c.read_body().await {
                Ok(body) => {
                    let wire = body.len() as u64;
                    let decoded = tokio::task::block_in_place(|| yenc::decode(&body));
                    drop(body);
                    match decoded {
                        Ok(d) if d.crc_ok => {
                            let (j, f, s) = (job.clone(), w.file, w.seg);
                            let res = tokio::task::block_in_place(move || j.write_segment(f, s, &d, false));
                            dispatch.lock().in_flight -= 1;
                            if let Err(e) = res {
                                tracing::error!(error = %e, "write failed; stopping job");
                                let mut r = job.rec.lock();
                                r.state = JobState::Failed;
                                r.error = Some(format!("could not write to disk: {e}"));
                                job.cancel.store(true, Ordering::Relaxed);
                            }
                        }
                        other => {
                            if let (Ok(d), None) = (other, &w.bad) {
                                w.bad = Some(d);
                            }
                            let left = dispatch.lock().failed_on(w, si);
                            settle(&job, left.into_iter().collect());
                        }
                    }
                    inner.limiter.consume(wire).await;
                }
                Err(FetchError::Missing) => {
                    let left = dispatch.lock().failed_on(w, si);
                    settle(&job, left.into_iter().collect());
                }
                Err(e) => {
                    tracing::debug!(server = %server.name, error = %e, "connection lost");
                    broken = true;
                    let left = dispatch.lock().requeue(w, si);
                    settle(&job, left.into_iter().collect());
                }
            }
        }
        if broken {
            conn = None;
        }
        idle.notify_waiters();
    }
    if let Some(c) = conn {
        c.quit().await;
    }
    idle.notify_waiters();
}

impl Engine {
    /// Start the engine and resume whatever is in the incomplete directory.
    pub async fn start(cfg: EngineConfig) -> std::io::Result<Engine> {
        // Directories are created when the first job needs them, never at startup: if the disk
        // they live on is not mounted, starting must not create stand-ins somewhere else.
        let (tx, _) = broadcast::channel(256);
        let limit = cfg.speed_limit;
        let inner = Arc::new(Inner {
            cfg: RwLock::new(cfg.clone()),
            jobs: Mutex::new(Vec::new()),
            wake: Notify::new(),
            tx,
            paused: AtomicBool::new(false),
            post_slots: Semaphore::new(cfg.post_parallel.clamp(1, 8)),
            semaphores: Mutex::new(HashMap::new()),
            limiter: Limiter::new(limit),
            shutdown: AtomicBool::new(false),
        });

        let mut loaded: Vec<Arc<Job>> = Vec::new();
        for entry in std::fs::read_dir(&cfg.incomplete_dir).into_iter().flatten().flatten() {
            let dir = entry.path();
            let (Ok(rec), Ok(raw)) = (std::fs::read(dir.join("job.json")), std::fs::read(dir.join("source.nzb"))) else { continue };
            let (Ok(mut rec), Ok(nzb)) = (serde_json::from_slice::<JobRecord>(&rec), nzb::parse(&raw)) else { continue };
            if rec.state == JobState::Downloading || rec.state.in_post() {
                // Interrupted mid-flight. Downloading resumes; post-processing starts over from verify.
                rec.state = JobState::Queued;
                rec.message = "Resuming after restart".into();
            }
            loaded.push(Arc::new(Job { id: rec.id.clone(), dir, nzb, rec: Mutex::new(rec), handles: Mutex::new(HashMap::new()), cancel: AtomicBool::new(false), speed: AtomicU64::new(0), fetched: AtomicU64::new(0), in_post: AtomicBool::new(false) }));
        }
        loaded.sort_by_key(|j| j.rec.lock().added_at);
        *inner.jobs.lock() = loaded;

        let engine = Engine(inner);
        let runner = engine.clone();
        tokio::spawn(async move { runner.run().await });
        Ok(engine)
    }

    async fn run(self) {
        let inner = self.0.clone();
        loop {
            if inner.shutdown.load(Ordering::Relaxed) {
                return;
            }
            let next = if inner.paused.load(Ordering::Relaxed) {
                None
            } else {
                inner.jobs.lock().iter().find(|j| !j.in_post.load(Ordering::Relaxed) && matches!(j.rec.lock().state, JobState::Queued | JobState::Downloading)).cloned()
            };
            let Some(job) = next else {
                let _ = tokio::time::timeout(Duration::from_secs(2), inner.wake.notified()).await;
                continue;
            };

            let files: Vec<usize> = {
                let mut r = job.rec.lock();
                r.state = JobState::Downloading;
                r.message = "Downloading".into();
                r.error = None;
                (0..r.files.len()).filter(|i| !r.files[*i].deferred && !r.files[*i].complete()).collect()
            };
            inner.publish(&job);
            // Alongside the download, a second task unpacks each RAR volume as it completes.
            let (stop, downloaded) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
            let mut unpacker = {
                let wanted = inner.cfg.read().direct_unpack && {
                    let r = job.rec.lock();
                    !r.direct_done && !r.direct_off && r.password.is_none()
                };
                wanted.then(|| crate::direct::plan(&job)).flatten().map(|volumes| {
                    crate::direct::tidy(&job);
                    tokio::spawn(crate::direct::run(job.clone(), volumes, stop.clone(), downloaded.clone()))
                })
            };
            let fetched = inner.fetch_files(&job, &files).await;
            if fetched != FetchOutcome::Complete {
                // Paused, stopped or cut off: the unpacker finishes the volume in hand and saves its place.
                stop.store(true, Ordering::Relaxed);
                if let Some(u) = unpacker.take() {
                    if let Ok(crate::direct::Outcome::GaveUp(why)) = u.await {
                        tracing::info!(job = %job.id, reason = %why, "not unpacking while downloading");
                        job.rec.lock().direct_off = true;
                    }
                }
            }
            match fetched {
                FetchOutcome::Complete => {
                    job.close_handles();
                    job.set_state(JobState::Verifying, "Checking the download");
                    let _ = job.save();
                    inner.publish(&job);
                    let (inner2, job2) = (inner.clone(), job.clone());
                    job.in_post.store(true, Ordering::Relaxed);
                    tokio::spawn(async move {
                        let _slot = inner2.post_slots.acquire().await;
                        if let Some(u) = unpacker {
                            job2.set_state(JobState::Extracting, "Unpacking");
                            inner2.publish(&job2);
                            downloaded.store(true, Ordering::Relaxed);
                            match u.await {
                                Ok(crate::direct::Outcome::Unpacked) => job2.rec.lock().direct_done = true,
                                Ok(crate::direct::Outcome::GaveUp(why)) => {
                                    tracing::info!(job = %job2.id, reason = %why, "unpacked the ordinary way instead");
                                    job2.rec.lock().direct_off = true;
                                }
                                _ => {}
                            }
                            let _ = job2.save();
                        }
                        crate::post::process(&inner2, &job2).await;
                        let _ = job2.save();
                        job2.in_post.store(false, Ordering::Relaxed);
                        inner2.publish(&job2);
                        // Cut short while fetching recovery data, it goes back to the queue.
                        inner2.wake.notify_one();
                    });
                }
                FetchOutcome::Stopped => {
                    job.close_handles();
                    let _ = job.save();
                    inner.publish(&job);
                }
                FetchOutcome::Unreachable(why) => {
                    job.close_handles();
                    {
                        let mut r = job.rec.lock();
                        r.state = JobState::Queued;
                        r.message = format!("Waiting to retry: {why}");
                    }
                    let _ = job.save();
                    inner.publish(&job);
                    let _ = tokio::time::timeout(Duration::from_secs(30), inner.wake.notified()).await;
                }
            }
        }
    }

    /// Queue an NZB. Returns the job id.
    pub fn add(&self, nzb_bytes: &[u8], name: &str, password: Option<String>) -> Result<String, EngineError> {
        let nzb = nzb::parse(nzb_bytes)?;
        let cfg = self.0.cfg.read().clone();
        if cfg.servers.len() > 64 {
            return Err(EngineError::TooManyServers);
        }
        let id = format!("{:x}{:04x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64, std::process::id() as u16);
        let clean = safe_dir_name(name.trim_end_matches(".nzb"));
        // {{password}} suffix convention
        let (clean, name_password) = match (clean.find("{{"), clean.rfind("}}")) {
            (Some(a), Some(b)) if b > a => (clean[..a].trim().to_string(), Some(clean[a + 2..b].to_string())),
            _ => (clean, None),
        };
        let dir = cfg.incomplete_dir.join(format!("{clean}.{}", &id[id.len().saturating_sub(6)..]));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("source.nzb"), nzb_bytes)?;

        // Recovery volumes wait until repair asks for them, as long as names let us tell them apart.
        let names: Vec<Option<String>> = nzb.files.iter().map(|f| f.subject_filename()).collect();
        let has_index = names.iter().flatten().any(|n| crate::par2::is_par2(n) && !crate::par2::is_recovery_volume(n));
        let files: Vec<FileProgress> = nzb
            .files
            .iter()
            .zip(&names)
            .map(|(f, n)| FileProgress {
                done: vec![false; f.segments.len()],
                deferred: has_index && n.as_deref().is_some_and(crate::par2::is_recovery_volume),
                ..Default::default()
            })
            .collect();
        let total_bytes = nzb.files.iter().zip(&files).filter(|(_, p)| !p.deferred).map(|(f, _)| f.bytes()).sum();
        let rec = JobRecord {
            id: id.clone(),
            name: clean,
            password: password.or(name_password).or(nzb.password.clone()).filter(|p| !p.is_empty()),
            added_at: now(),
            state: JobState::Queued,
            message: "Queued".into(),
            error: None,
            output_path: None,
            files,
            total_bytes,
            done_bytes: 0,
            direct_done: false,
            direct_off: false,
        };
        let job = Arc::new(Job { id: id.clone(), dir, nzb, rec: Mutex::new(rec), handles: Mutex::new(HashMap::new()), cancel: AtomicBool::new(false), speed: AtomicU64::new(0), fetched: AtomicU64::new(0), in_post: AtomicBool::new(false) });
        job.save()?;
        self.0.jobs.lock().push(job.clone());
        self.0.publish(&job);
        self.0.wake.notify_one();
        Ok(id)
    }

    fn find(&self, id: &str) -> Result<Arc<Job>, EngineError> {
        self.0.jobs.lock().iter().find(|j| j.id == id).cloned().ok_or(EngineError::NotFound)
    }

    pub fn jobs(&self) -> Vec<JobStatus> {
        self.0.jobs.lock().iter().map(|j| j.status()).collect()
    }

    pub fn job(&self, id: &str) -> Option<JobStatus> {
        self.find(id).ok().map(|j| j.status())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<JobStatus> {
        self.0.tx.subscribe()
    }

    pub fn pause(&self, id: &str) -> Result<(), EngineError> {
        self.pause_because(id, "Paused")
    }

    /// Pause a job and say why, for pauses the user did not ask for.
    pub fn pause_because(&self, id: &str, why: &str) -> Result<(), EngineError> {
        let job = self.find(id)?;
        {
            let mut r = job.rec.lock();
            if matches!(r.state, JobState::Queued | JobState::Downloading) {
                r.state = JobState::Paused;
                r.message = why.into();
            }
        }
        let _ = job.save();
        self.0.publish(&job);
        Ok(())
    }

    pub fn resume(&self, id: &str) -> Result<(), EngineError> {
        let job = self.find(id)?;
        {
            let mut r = job.rec.lock();
            if r.state == JobState::Paused {
                r.state = JobState::Queued;
                r.message = "Queued".into();
            }
        }
        let _ = job.save();
        self.0.publish(&job);
        self.0.wake.notify_one();
        Ok(())
    }

    /// Put a failed job back in the queue. Segments already written are kept.
    pub fn retry(&self, id: &str) -> Result<(), EngineError> {
        let job = self.find(id)?;
        {
            let mut r = job.rec.lock();
            if r.state == JobState::Failed {
                for (fp, f) in r.files.iter_mut().zip(&job.nzb.files) {
                    for s in fp.missing.drain(..).chain(fp.damaged.drain(..)) {
                        fp.done[s as usize] = false;
                        let _ = f;
                    }
                }
                let done: u64 = r.files.iter().zip(&job.nzb.files).map(|(fp, f)| fp.done.iter().zip(&f.segments).filter(|(d, _)| **d).map(|(_, s)| s.bytes).sum::<u64>()).sum();
                r.done_bytes = done;
                r.state = JobState::Queued;
                r.error = None;
                r.message = "Queued for retry".into();
            }
        }
        job.cancel.store(false, Ordering::Relaxed);
        let _ = job.save();
        self.0.publish(&job);
        self.0.wake.notify_one();
        Ok(())
    }

    /// Remove a job. `delete_files` also removes its working directory; output already moved to
    /// the complete directory is never touched.
    pub fn remove(&self, id: &str, delete_files: bool) -> Result<(), EngineError> {
        let job = self.find(id)?;
        job.cancel.store(true, Ordering::Relaxed);
        self.0.jobs.lock().retain(|j| j.id != id);
        job.close_handles();
        let dir = job.dir.clone();
        if delete_files || job.rec.lock().state == JobState::Completed {
            // The workers may still be writing for a moment; remove once they have let go.
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let _ = tokio::fs::remove_dir_all(&dir).await;
            });
        } else {
            let _ = std::fs::remove_file(dir.join("job.json"));
        }
        self.0.wake.notify_one();
        Ok(())
    }

    pub fn pause_all(&self) {
        self.0.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume_all(&self) {
        self.0.paused.store(false, Ordering::Relaxed);
        self.0.wake.notify_one();
    }

    pub fn is_paused(&self) -> bool {
        self.0.paused.load(Ordering::Relaxed)
    }

    pub fn config(&self) -> EngineConfig {
        self.0.cfg.read().clone()
    }

    /// Apply new settings. Server changes take effect for the next job.
    pub fn set_config(&self, cfg: EngineConfig) {
        self.0.limiter.rate.store(cfg.speed_limit, Ordering::Relaxed);
        self.0.semaphores.lock().clear();
        *self.0.cfg.write() = cfg;
        self.0.wake.notify_one();
    }

    /// Stop taking work. In-flight articles finish and progress is saved.
    pub async fn shutdown(&self) {
        self.0.shutdown.store(true, Ordering::Relaxed);
        self.0.wake.notify_waiters();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let jobs: Vec<Arc<Job>> = self.0.jobs.lock().clone();
        for job in jobs {
            let _ = tokio::task::spawn_blocking(move || job.checkpoint()).await;
        }
    }

    /// How much of an NZB is still on the configured servers, judged from an even sample of its
    /// articles. Returns (found, asked). Uses one connection per server and fetches nothing.
    pub async fn availability(&self, nzb_bytes: &[u8], sample: usize) -> Result<(usize, usize), String> {
        let nzb = nzb::parse(nzb_bytes).map_err(|e| e.to_string())?;
        let ids: Vec<&str> = nzb.files.iter().flat_map(|f| f.segments.iter().map(|s| s.message_id.as_str())).collect();
        if ids.is_empty() {
            return Ok((0, 0));
        }
        let step = (ids.len() / sample.max(1)).max(1);
        let asked: Vec<&str> = ids.iter().step_by(step).take(sample.max(1)).copied().collect();
        let mut servers: Vec<ServerConfig> = self.0.cfg.read().servers.iter().filter(|s| s.enabled && !s.host.is_empty()).cloned().collect();
        servers.sort_by_key(|s| s.priority);
        if servers.is_empty() {
            return Err("no usenet servers are configured".into());
        }
        let mut found = vec![false; asked.len()];
        let mut reached = false;
        let mut last_error = String::new();
        for server in &servers {
            if found.iter().all(|f| *f) {
                break;
            }
            let mut conn = match Connection::connect(server).await {
                Ok(c) => c,
                Err(e) => {
                    last_error = format!("{}: {e}", server.name);
                    continue;
                }
            };
            reached = true;
            for (i, id) in asked.iter().enumerate() {
                if found[i] {
                    continue;
                }
                match conn.stat(id).await {
                    Ok(present) => found[i] = present,
                    Err(e) => {
                        last_error = format!("{}: {e}", server.name);
                        break;
                    }
                }
            }
            conn.quit().await;
        }
        if !reached {
            return Err(last_error);
        }
        Ok((found.iter().filter(|f| **f).count(), asked.len()))
    }

    /// Check that a server accepts our connection and credentials.
    pub async fn test_server(cfg: &ServerConfig) -> Result<(), String> {
        match Connection::connect(cfg).await {
            Ok(c) => {
                c.quit().await;
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

pub(crate) fn job_dir_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect())
        .unwrap_or_default();
    v.retain(|p| !matches!(p.file_name().and_then(|n| n.to_str()), Some("job.json" | "job.json.tmp" | "source.nzb")));
    v.sort();
    v
}
