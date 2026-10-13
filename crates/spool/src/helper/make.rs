//! Making compact versions for the Spool this machine is linked to.
//!
//! A machine with video hardware and nothing of the library on it can still do the slowest
//! work there is. It is told what a film is and where to read it, reads it from the Spool
//! that asked, makes the compact version in a folder of its own, and keeps it there until
//! that Spool has taken it. One film at a time: the video hardware does one well.
//!
//! It reads only from the machine that asked, at an address put together here from where
//! the request came from, and writes only into its own folder. What it runs is worked out
//! here from what the film is; nothing it is sent is run.
//!
//! Whoever drives it says what is made. A film asked for while another is being made takes
//! its place, so a Spool that was restarted and has forgotten what it asked for is not kept
//! waiting on it.

use super::{Helper, Refused, R};
use crate::compact::{arguments, frames_a_second, run, ways, Compact, CALLED_OFF};
use axum::extract::{ConnectInfo, Path as UrlPath, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;
use spool_core::mediainfo::MediaInfo;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Room kept free beyond what a compact version is expected to take.
const SPARE: u64 = 2 << 30;

pub(super) struct Making {
    /// Where what is made is kept until it is taken. Emptied when the machine starts.
    dir: PathBuf,
    job: Mutex<Option<Arc<Job>>>,
}

struct Job {
    id: String,
    out: PathBuf,
    done: Arc<AtomicU64>,
    speed: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    how: Mutex<&'static str>,
    /// "running", "done", "cancelled", or what went wrong.
    state: Mutex<String>,
}

/// One film to make a compact version of.
#[derive(Deserialize)]
pub(super) struct Ask {
    /// A name for this piece of work, made up by whoever asks.
    id: String,
    /// Where the film is read from on the machine that asks: its port, and the path there.
    port: u16,
    path: String,
    compact: Compact,
    info: MediaInfo,
    /// Roughly how big the compact version will be.
    bytes: u64,
}

impl Making {
    pub(super) fn new(data_dir: &Path) -> Making {
        let dir = data_dir.join("making");
        // Whatever is there was made for a Spool that has since been told it was lost.
        let _ = std::fs::remove_dir_all(&dir);
        Making { dir, job: Mutex::new(None) }
    }

    /// Whether a film is being made right now.
    pub(super) fn running(&self) -> bool {
        self.job.lock().as_ref().is_some_and(|j| *j.state.lock() == "running")
    }
}

impl Job {
    /// Stop it if it is running, and take away what it made.
    fn end(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // One that is running takes its own file away as it stops.
        if *self.state.lock() != "running" {
            let _ = std::fs::remove_file(&self.out);
        }
    }
}

/// What this machine makes compact versions with: "hardware", "software", or "none".
fn means() -> &'static str {
    ways(crate::subs::ffmpeg_installed().as_deref(), false).first().map_or("none", |w| w.word())
}

fn job_of(h: &Helper, id: &str) -> std::result::Result<Arc<Job>, Refused> {
    h.0.making.job.lock().clone().filter(|j| j.id == id).ok_or_else(|| Refused(StatusCode::NOT_FOUND, "no such compact version is being made here".into()))
}

/// What it can make with, how much room it has, and what it is making.
pub(super) async fn info(State(h): State<Helper>) -> R {
    let m = &h.0.making;
    let job = m.job.lock().clone();
    Ok(Json(json!({
        "means": means(),
        "free": crate::app::free_space(h.0.data_dir.as_path()),
        "making": job.filter(|j| *j.state.lock() == "running").map(|j| j.id.clone()),
    })))
}

pub(super) async fn start(State(h): State<Helper>, ConnectInfo(from): ConnectInfo<SocketAddr>, Json(ask): Json<Ask>) -> R {
    let plain = |s: &str, more: &str| !s.is_empty() && s.len() <= 512 && s.chars().all(|c| c.is_ascii_alphanumeric() || more.contains(c));
    if !plain(&ask.id, "-") || ask.id.len() > 64 {
        return Err(Refused(StatusCode::BAD_REQUEST, "the name of the work is not a plain one".into()));
    }
    if !ask.path.starts_with("/link/source/") || !plain(&ask.path, "/-_.?=&%") {
        return Err(Refused(StatusCode::BAD_REQUEST, "that is not where a film is read from".into()));
    }
    let tool = crate::subs::ffmpeg_installed().ok_or_else(|| Refused(StatusCode::SERVICE_UNAVAILABLE, "ffmpeg is not installed on this machine".into()))?;
    let m = &h.0.making;
    std::fs::create_dir_all(&m.dir)?;
    if crate::app::free_space(&m.dir).is_some_and(|free| free < ask.bytes.saturating_add(SPARE)) {
        return Err(Refused(StatusCode::INSUFFICIENT_STORAGE, "this machine has no room to make it".into()));
    }
    // An address of the old kind that arrived dressed as the new kind is the old kind.
    let ip = match from.ip() {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(from.ip()),
        v4 => v4,
    };
    let src = format!("http://{}{}", SocketAddr::new(ip, ask.port), ask.path);
    // The same work asked for twice is under way already.
    if m.job.lock().as_ref().is_some_and(|j| j.id == ask.id) {
        return Err(Refused(StatusCode::CONFLICT, "that is being made already".into()));
    }
    let job = Arc::new(Job { out: m.dir.join(format!("{}.mkv", ask.id)), id: ask.id, done: Default::default(), speed: Default::default(), stop: Default::default(), how: Mutex::new(""), state: Mutex::new("running".into()) });
    if let Some(old) = m.job.lock().replace(job.clone()) {
        old.end();
    }
    tokio::spawn(async move {
        let fps = frames_a_second(&ask.info);
        let mut last = String::new();
        let mut ended = None;
        for way in ways(Some(&tool), ask.compact.software) {
            if job.stop.load(Ordering::Relaxed) {
                ended = Some("cancelled".to_string());
                break;
            }
            *job.how.lock() = way.word();
            job.done.store(0, Ordering::Relaxed);
            match run(&tool, &arguments(&ask.compact, &ask.info, &src, &job.out, way), fps, &job.done, &job.speed, &job.stop).await {
                Ok(()) => {
                    ended = Some("done".to_string());
                    break;
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&job.out).await;
                    if e.to_string() == CALLED_OFF {
                        ended = Some("cancelled".to_string());
                        break;
                    }
                    // Where it was read from is nobody's to read in a log.
                    last = format!("{e:#}").replace(&src, "the film");
                    tracing::info!(way = ?way, error = %last, "that way of making a compact version did not work; trying the next");
                }
            }
        }
        let ended = ended.unwrap_or_else(|| format!("ffmpeg could not do it: {last}"));
        // Told to stop after it had finished: what it made is not wanted.
        if job.stop.load(Ordering::Relaxed) {
            let _ = tokio::fs::remove_file(&job.out).await;
        }
        *job.state.lock() = ended;
    });
    Ok(Json(json!({"ok": true})))
}

/// How far it has got.
pub(super) async fn state(State(h): State<Helper>, UrlPath(id): UrlPath<String>) -> R {
    let job = job_of(&h, &id)?;
    let state = job.state.lock().clone();
    let size = if state == "done" { std::fs::metadata(&job.out).map(|m| m.len()).unwrap_or(0) } else { 0 };
    Ok(Json(json!({
        "state": state,
        "done_ms": job.done.load(Ordering::Relaxed),
        "speed": job.speed.load(Ordering::Relaxed) as f64 / 100.0,
        "how": *job.how.lock(),
        "size": size,
    })))
}

/// What was made, once it is whole.
pub(super) async fn file(State(h): State<Helper>, UrlPath(id): UrlPath<String>, req: Request) -> Response {
    let job = match job_of(&h, &id) {
        Ok(job) => job,
        Err(refused) => return refused.into_response(),
    };
    if *job.state.lock() != "done" {
        return Refused(StatusCode::CONFLICT, "it is not made yet".into()).into_response();
    }
    crate::serve::send_file(&job.out, req.headers(), false).await
}

/// Stop making it, or let go of it once it has been taken.
pub(super) async fn end(State(h): State<Helper>, UrlPath(id): UrlPath<String>) -> R {
    let mut slot = h.0.making.job.lock();
    if let Some(job) = slot.take_if(|j| j.id == id) {
        job.end();
    }
    Ok(Json(json!({"ok": true})))
}
