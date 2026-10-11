//! Driving a helper on the machine that holds the disks.
//!
//! A helper (see [`crate::helper`]) does file work on a NAS's own disks. This is the other
//! end: what Spool keeps about each helper it has been given, how it asks one to do
//! something, and the one place the rest of the server goes to for downloads, so that it
//! need not care whether a download runs in this process or on the helper.
//!
//! Which helper serves which part of the library is not set anywhere. A helper is given the
//! folder this machine sees the NAS under and the folder the helper sees the same thing
//! under; whatever lies in the first is its to work on.
//!
//! A helper that cannot be reached takes nothing with it. Downloads for its part of the
//! library run here and are filed across the share, as they would be without it, and what
//! it was already doing is waited for and not written off.

use crate::app::{App, Event};
use crate::helper::Pushed;
use crate::models::*;
use crate::settings::HelperConf;
use anyhow::{anyhow, bail, Result};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use spool_nntp::{JobState, JobStatus};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What was last learned of a helper.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Seen {
    /// It has been asked at least once since Spool started or its settings changed.
    pub asked: bool,
    pub reachable: bool,
    /// Why it cannot be used, or cannot be used for downloads, in words for a person.
    pub problem: String,
    pub version: String,
    /// It answers and speaks as this Spool does.
    pub usable: bool,
    /// Its folder of finished downloads can be read from this machine, which filing needs.
    pub downloads_seen: bool,
    pub paused: bool,
    /// Room in its downloads folder.
    pub free: Option<u64>,
    pub busy: Vec<String>,
    #[serde(skip)]
    boot: String,
    /// How many times it has answered since it was last looked at closely.
    #[serde(skip)]
    answers: u32,
    #[serde(skip)]
    pushed: Option<Pushed>,
}

pub struct Remote {
    pub conf: HelperConf,
    http: reqwest::Client,
    /// Its downloads as last reported, named and placed as this machine knows them.
    jobs: Mutex<Vec<JobStatus>>,
    /// Downloads handed over a moment ago, which a report already on its way back may not
    /// have in it yet.
    fresh: Mutex<HashMap<String, (Instant, JobStatus)>>,
    seen: Mutex<Seen>,
}

/// What is known of a download when it is asked after.
pub enum Found {
    Job(JobStatus),
    /// Whoever runs it was asked and has no such download.
    Gone,
    /// It runs on a helper that cannot be asked right now.
    Unknown,
}

/// Run something that waits on the network from code that does not. The server's own
/// runtime has threads to spare for it.
fn wait<F: std::future::Future>(f: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(f))
}

impl Remote {
    fn new(conf: HelperConf) -> Remote {
        let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(4)).build().expect("http client");
        Remote { conf, http, jobs: Mutex::new(vec![]), fresh: Mutex::new(HashMap::new()), seen: Mutex::new(Seen::default()) }
    }

    pub fn seen(&self) -> Seen {
        self.seen.lock().clone()
    }

    /// What marks a download as this helper's among all of Spool's.
    fn prefix(&self) -> String {
        format!("@{}:", self.conf.id)
    }

    /// Whether a path on this machine is on the helper's disks.
    pub fn covers(&self, here: &Path) -> bool {
        !self.conf.here.is_empty() && here.starts_with(&self.conf.here)
    }

    /// A path on this machine, as the helper knows it.
    pub fn there(&self, here: &Path) -> Option<PathBuf> {
        if self.conf.here.is_empty() {
            return None;
        }
        here.strip_prefix(&self.conf.here).ok().map(|rest| Path::new(&self.conf.there).join(rest))
    }

    /// A path on the helper, as this machine sees it.
    pub fn here(&self, there: &Path) -> Option<PathBuf> {
        there.strip_prefix(&self.conf.there).ok().map(|rest| Path::new(&self.conf.here).join(rest))
    }

    async fn send(&self, req: reqwest::RequestBuilder, patience: Duration) -> Result<Value> {
        let name = &self.conf.name;
        let resp = req.bearer_auth(&self.conf.key).timeout(patience).send().await.map_err(|e| anyhow!("{name}'s helper did not answer: {}", e.without_url()))?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("{name}'s helper refused: {}", body["error"].as_str().unwrap_or(status.as_str()));
        }
        Ok(body)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.send(self.http.get(format!("{}{path}", self.conf.url.trim_end_matches('/'))), Duration::from_secs(8)).await
    }

    async fn post(&self, path: &str, body: Value, patience: Duration) -> Result<Value> {
        self.send(self.http.post(format!("{}{path}", self.conf.url.trim_end_matches('/'))).json(&body), patience).await
    }

    async fn post_bytes(&self, path: &str, bytes: &[u8]) -> Result<Value> {
        self.send(self.http.post(format!("{}{path}", self.conf.url.trim_end_matches('/'))).body(bytes.to_vec()), Duration::from_secs(60)).await
    }

    /// A download as the helper reports it, renamed and placed as this machine knows it.
    fn adopt(&self, mut job: JobStatus) -> JobStatus {
        job.id = format!("{}{}", self.prefix(), job.id);
        job.output_path = job.output_path.map(|p| self.here(&p).unwrap_or(p));
        job
    }

    pub fn jobs(&self) -> Vec<JobStatus> {
        let mut all = self.jobs.lock().clone();
        let fresh = self.fresh.lock();
        all.extend(fresh.values().filter(|(at, j)| at.elapsed() < Duration::from_secs(15) && !all.iter().any(|a| a.id == j.id)).map(|(_, j)| j.clone()).collect::<Vec<_>>());
        all
    }

    async fn add(&self, nzb: &[u8], name: &str) -> Result<String> {
        let said = self.post_bytes(&format!("/helper/jobs?name={}", urlencoding::encode(name)), nzb).await?;
        let id = format!("{}{}", self.prefix(), said["id"].as_str().ok_or_else(|| anyhow!("{}'s helper gave no name for the download", self.conf.name))?);
        let placeholder: JobStatus = serde_json::from_value(json!({
            "id": id, "name": name, "state": "queued", "total_bytes": spool_nntp::nzb::parse(nzb).map(|n| n.bytes()).unwrap_or(0), "done_bytes": 0, "speed": 0, "eta_secs": null,
            "missing_articles": 0, "damaged_articles": 0, "message": "", "error": null, "output_path": null, "added_at": crate::db::now(),
        }))?;
        let mut fresh = self.fresh.lock();
        fresh.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(15));
        fresh.insert(id.clone(), (Instant::now(), placeholder));
        Ok(id)
    }

    /// One action on one download, named as the helper names it.
    async fn act(&self, own: &str, action: &str, body: Value) -> Result<()> {
        self.post(&format!("/helper/jobs/{own}/{action}"), body, Duration::from_secs(15)).await.map(|_| ())
    }

    async fn remove(&self, own: &str, files: bool) -> Result<()> {
        let url = format!("{}/helper/jobs/{own}?files={files}", self.conf.url.trim_end_matches('/'));
        self.send(self.http.delete(url), Duration::from_secs(30)).await?;
        self.jobs.lock().retain(|j| j.id != format!("{}{own}", self.prefix()));
        Ok(())
    }

    /// File one thing into the library on the helper's own disks. A film copied between two
    /// of its disks takes a while, so it is given a long time to answer.
    pub async fn file_move(&self, from: &Path, to: &Path) -> Result<()> {
        self.post("/helper/fs/move", json!({"from": from, "to": to}), Duration::from_secs(4 * 3600)).await.map(|_| ())
    }

    /// Clear away the folder a finished download left.
    pub async fn remove_output(&self, there: &Path) -> Result<()> {
        self.post("/helper/fs/remove", json!({"path": there}), Duration::from_secs(120)).await.map(|_| ())
    }

    pub async fn stat(&self, there: &Path) -> Result<Value> {
        self.post("/helper/fs/stat", json!({"path": there}), Duration::from_secs(10)).await
    }

    pub async fn move_start(&self, from: &Path, to: &Path, working: &Path, keep_original: bool) -> Result<String> {
        let said = self.post("/helper/moves", json!({"from": from, "to": to, "working": working, "keep_original": keep_original}), Duration::from_secs(30)).await?;
        said["id"].as_str().map(str::to_string).ok_or_else(|| anyhow!("{}'s helper gave no name for the move", self.conf.name))
    }

    pub async fn move_state(&self, id: &str) -> Result<Value> {
        self.get(&format!("/helper/moves/{id}")).await
    }

    pub async fn move_cancel(&self, id: &str) -> Result<()> {
        self.post(&format!("/helper/moves/{id}/cancel"), json!({}), Duration::from_secs(15)).await.map(|_| ())
    }

    pub async fn move_finish(&self, id: &str, remove_original: bool) -> Result<()> {
        self.post(&format!("/helper/moves/{id}/finish"), json!({"remove_original": remove_original}), Duration::from_secs(600)).await.map(|_| ())
    }

    /// Ask the helper how things stand. Returns the downloads that changed since last time.
    async fn ask(&self, wanted: &Pushed) -> Vec<JobStatus> {
        let said = match self.get("/helper/jobs").await {
            Ok(said) => said,
            Err(e) => {
                let mut seen = self.seen.lock();
                (seen.asked, seen.reachable, seen.usable, seen.problem) = (true, false, false, format!("{e:#}"));
                return vec![];
            }
        };
        let boot = said["boot"].as_str().unwrap_or_default().to_string();
        // Met for the first time, or it has restarted: find out what it is and what it sees.
        // And looked at again now and then while its downloads cannot be seen from here, so
        // that mounting the share is noticed without restarting anything.
        let look = {
            let mut seen = self.seen.lock();
            seen.answers += 1;
            seen.boot != boot || !seen.reachable || (seen.usable && !seen.downloads_seen && seen.answers % 30 == 0)
        };
        if look {
            let mut next = Seen { asked: true, reachable: true, boot: boot.clone(), ..Default::default() };
            match self.get("/helper/info").await {
                Ok(info) => {
                    next.version = info["version"].as_str().unwrap_or_default().to_string();
                    if info["api"].as_u64() != Some(crate::helper::API as u64) {
                        next.problem = format!("{}'s helper is version {} and does not speak as this Spool does; update the two together", self.conf.name, next.version);
                    } else {
                        next.usable = true;
                        // Filing reads what was downloaded, so this machine has to see it.
                        let complete = PathBuf::from(info["complete"].as_str().unwrap_or_default());
                        match self.here(&complete) {
                            Some(here) if crate::locations::listable(&here) => next.downloads_seen = true,
                            Some(here) => next.problem = format!("{}'s helper downloads into {}, which this machine should see at {} and cannot; share that folder and mount it, and until then downloads run here", self.conf.name, complete.display(), here.display()),
                            None => next.problem = format!("{}'s helper downloads into {}, which is outside {}; until its downloads folder is inside, downloads run here", self.conf.name, complete.display(), self.conf.there),
                        }
                    }
                }
                Err(e) => next.problem = format!("{e:#}"),
            }
            *self.seen.lock() = next;
        }
        if self.seen.lock().usable && self.seen.lock().pushed.as_ref() != Some(wanted) {
            let url = format!("{}/helper/config", self.conf.url.trim_end_matches('/'));
            match self.send(self.http.put(url).json(wanted), Duration::from_secs(15)).await {
                Ok(_) => self.seen.lock().pushed = Some(wanted.clone()),
                Err(e) => tracing::warn!(helper = %self.conf.name, error = %format!("{e:#}"), "could not give the helper its settings"),
            }
        }
        {
            let mut seen = self.seen.lock();
            seen.paused = said["paused"].as_bool().unwrap_or(false);
            seen.free = said["free"].as_u64();
            seen.busy = said["busy"].as_array().map(|b| b.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
        }
        let now: Vec<JobStatus> = serde_json::from_value::<Vec<JobStatus>>(said["jobs"].clone()).unwrap_or_default().into_iter().map(|j| self.adopt(j)).collect();
        let mut jobs = self.jobs.lock();
        let changed = now
            .iter()
            .filter(|j| jobs.iter().find(|o| o.id == j.id).is_none_or(|o| (o.state, o.done_bytes, o.speed, &o.message, &o.error) != (j.state, j.done_bytes, j.speed, &j.message, &j.error)))
            .cloned()
            .collect();
        *jobs = now;
        changed
    }
}

/// Check the helpers as the settings page sends them, and number the new ones.
pub fn tidy_helpers(list: Vec<HelperConf>, old: &[HelperConf]) -> std::result::Result<Vec<HelperConf>, String> {
    let clean = |p: &str| p.trim().trim_end_matches('/').to_string();
    let mut next = old.iter().chain(&list).map(|h| h.id).max().unwrap_or(0);
    let mut out: Vec<HelperConf> = vec![];
    for mut h in list {
        (h.name, h.url, h.key) = (h.name.trim().to_string(), clean(&h.url), h.key.trim().to_string());
        (h.here, h.there) = (clean(&h.here), clean(&h.there));
        if h.name.is_empty() {
            return Err("every helper needs a name".into());
        }
        if !h.url.starts_with("http://") && !h.url.starts_with("https://") {
            return Err(format!("the address of {}'s helper must start with http:// or https://", h.name));
        }
        if h.key.is_empty() {
            return Err(format!("{}'s helper needs its key", h.name));
        }
        if !h.here.starts_with('/') || !h.there.starts_with('/') {
            return Err(format!("the two folders of {}'s helper must be full paths, starting with /", h.name));
        }
        if h.id == 0 || out.iter().any(|o| o.id == h.id) {
            next += 1;
            h.id = next;
        }
        out.push(h);
    }
    Ok(out)
}

impl App {
    pub fn remotes(&self) -> Vec<Arc<Remote>> {
        self.remotes.lock().clone()
    }

    /// Bring the helpers Spool keeps in line with its settings. One whose settings have not
    /// changed keeps what is known of it.
    pub fn sync_remotes(&self) {
        let wanted: Vec<HelperConf> = self.settings.general().helpers.into_iter().filter(|h| !h.url.trim().is_empty()).collect();
        let mut remotes = self.remotes.lock();
        *remotes = wanted.into_iter().map(|c| remotes.iter().find(|r| r.conf == c).cloned().unwrap_or_else(|| Arc::new(Remote::new(c)))).collect();
    }

    /// What the helpers are given of Spool's settings.
    fn pushed(&self) -> Pushed {
        let g = self.settings.general();
        Pushed { servers: self.settings.servers(), speed_limit: self.settings.speed_limit(), direct_unpack: g.direct_unpack, post_parallel: g.post_parallel as usize }
    }

    /// Ask every helper how things stand, and act on what has changed.
    pub async fn ask_remotes(&self) {
        let wanted = self.pushed();
        let mut problems = vec![];
        for r in self.remotes() {
            let was = r.seen();
            let changed = r.ask(&wanted).await;
            let now = r.seen();
            // What was waiting on the helper to be filed can be finished now.
            if now.usable && !was.usable {
                let app = self.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(e) = app.recover() {
                        tracing::warn!(error = %e, "could not finish interrupted imports");
                    }
                });
            }
            if was.asked && was.reachable != now.reachable {
                if now.reachable {
                    tracing::info!(helper = %r.conf.name, "the helper is back");
                } else {
                    tracing::warn!(helper = %r.conf.name, problem = %now.problem, "the helper cannot be reached; downloads for its part of the library run here until it is back");
                }
                self.emit(Event::Locations);
            }
            if !now.problem.is_empty() {
                problems.push(now.problem.clone());
            }
            for job in changed {
                self.on_job(job);
            }
        }
        let message = (!problems.is_empty()).then(|| problems.join(" "));
        if self.db.set_standing_attention("helper", message.as_deref(), json!({})).unwrap_or(false) {
            self.emit(Event::Attention);
        }
    }

    /// Keep asking the helpers, often enough that a download's progress is worth watching.
    pub fn spawn_remotes(&self) {
        self.sync_remotes();
        let app = self.clone();
        tokio::spawn(async move {
            loop {
                app.ask_remotes().await;
                tokio::time::sleep(Duration::from_millis(if app.remotes().is_empty() { 5000 } else { 1000 })).await;
            }
        });
    }

    /// The helper whose disks a path is on, if it can be used now.
    pub fn remote_for(&self, here: &Path) -> Option<Arc<Remote>> {
        self.remotes().into_iter().find(|r| r.covers(here) && r.seen().usable)
    }

    /// The helper that would download for a title: the one whose disks the title is on, or
    /// would be put on, when it is ready to download.
    fn remote_for_title(&self, title: &Title) -> Option<Arc<Remote>> {
        let folder = if title.path.is_empty() {
            self.placement().for_new(title.kind, self.settings.general().new_titles_location).map(|l| l.root(title.kind).to_string()).unwrap_or_default()
        } else {
            title.path.clone()
        };
        self.remote_for(Path::new(&folder)).filter(|r| r.seen().downloads_seen)
    }

    /// The helper a download belongs to, and the download's name there.
    fn remote_of<'a>(&self, id: &'a str) -> Option<(Option<Arc<Remote>>, &'a str)> {
        let rest = id.strip_prefix('@')?;
        let (helper, own) = rest.split_once(':')?;
        Some((self.remotes().into_iter().find(|r| r.conf.id.to_string() == helper), own))
    }

    /// Every download, wherever it runs.
    pub fn jobs(&self) -> Vec<JobStatus> {
        let mut all = self.engine.jobs();
        for r in self.remotes() {
            all.extend(r.jobs());
        }
        all
    }

    pub fn job_lookup(&self, id: &str) -> Found {
        match self.remote_of(id) {
            None => self.engine.job(id).map(Found::Job).unwrap_or(Found::Gone),
            // Its helper was taken out of the settings, and its downloads with it.
            Some((None, _)) => Found::Gone,
            Some((Some(r), _)) => match r.jobs().into_iter().find(|j| j.id == id) {
                Some(j) => Found::Job(j),
                None if r.seen().reachable => Found::Gone,
                None => Found::Unknown,
            },
        }
    }

    pub fn job(&self, id: &str) -> Option<JobStatus> {
        match self.job_lookup(id) {
            Found::Job(j) => Some(j),
            _ => None,
        }
    }

    /// Where a title's downloads run, and how much room there is for them: the helper's
    /// downloads folder, or this machine's.
    pub fn download_room(&self, title: Option<&Title>) -> (Option<u64>, Vec<JobStatus>) {
        match title.and_then(|t| self.remote_for_title(t)) {
            Some(r) => (r.seen().free, r.jobs()),
            None => (crate::app::free_space(&self.engine.config().incomplete_dir), self.engine.jobs()),
        }
    }

    /// Start a download for a title, on the helper that holds its disks if there is one.
    pub fn job_add(&self, title: &Title, nzb: &[u8], name: &str) -> Result<String> {
        if let Some(r) = self.remote_for_title(title) {
            match wait(r.add(nzb, name)) {
                Ok(id) => return Ok(id),
                Err(e) => tracing::warn!(helper = %r.conf.name, error = %format!("{e:#}"), "the helper would not take the download; running it here"),
            }
        }
        Ok(self.engine.add(nzb, name, None)?)
    }

    fn with_remote(&self, id: &str, local: impl FnOnce() -> Result<()>, remote: impl FnOnce(Arc<Remote>, String) -> Result<()>) -> Result<()> {
        match self.remote_of(id) {
            None => local(),
            Some((Some(r), own)) => remote(r, own.to_string()),
            Some((None, _)) => bail!("the helper this download ran on is no longer in the settings"),
        }
    }

    pub fn job_pause(&self, id: &str) -> Result<()> {
        self.with_remote(id, || Ok(self.engine.pause(id)?), |r, own| wait(r.act(&own, "pause", json!({}))))
    }

    pub fn job_resume(&self, id: &str) -> Result<()> {
        self.with_remote(id, || Ok(self.engine.resume(id)?), |r, own| wait(r.act(&own, "resume", json!({}))))
    }

    pub fn job_retry(&self, id: &str) -> Result<()> {
        self.with_remote(id, || Ok(self.engine.retry(id)?), |r, own| wait(r.act(&own, "retry", json!({}))))
    }

    /// Pause a download for a reason of Spool's own, which is shown beside it.
    pub fn job_hold(&self, id: &str, why: &str) -> Result<()> {
        self.with_remote(id, || Ok(self.engine.pause_because(id, why)?), |r, own| wait(r.act(&own, "hold", json!({"why": why}))))
    }

    pub fn job_remove(&self, id: &str, files: bool) -> Result<()> {
        self.with_remote(id, || Ok(self.engine.remove(id, files)?), |r, own| wait(r.remove(&own, files)))
    }

    /// Pause or resume every download, here and on every helper.
    pub fn queue_pause(&self, pause: bool) {
        if pause {
            self.engine.pause_all();
        } else {
            self.engine.resume_all();
        }
        for r in self.remotes() {
            tokio::spawn(async move {
                if let Err(e) = r.post(&format!("/helper/queue/{}", if pause { "pause" } else { "resume" }), json!({}), Duration::from_secs(15)).await {
                    tracing::warn!(helper = %r.conf.name, error = %format!("{e:#}"), "could not pause or resume the helper's downloads");
                }
            });
        }
    }

    /// Clear away what a finished or abandoned download left on disk.
    pub async fn remove_download_folder(&self, job: Option<&str>, folder: &Path) {
        if let Some((Some(r), _)) = job.and_then(|id| self.remote_of(id)) {
            if let Some(there) = r.there(folder) {
                if let Err(e) = r.remove_output(&there).await {
                    tracing::warn!(folder = %folder.display(), error = %format!("{e:#}"), "the helper did not clear a finished download away");
                }
            }
            return;
        }
        let complete = self.engine.config().complete_dir;
        if folder.starts_with(&complete) && folder != complete {
            let _ = tokio::fs::remove_dir_all(folder).await;
        }
    }

    /// File one thing into the library. When both ends are on a helper's disks the helper
    /// does it there, and nothing crosses the network.
    pub fn file_move(&self, src: &Path, dst: &Path) -> Result<()> {
        if let Some(r) = self.remotes().into_iter().find(|r| r.covers(src) && r.covers(dst)) {
            if !r.seen().usable {
                bail!("{}'s helper cannot be reached, and this is its to file", r.conf.name);
            }
            if let (Some(from), Some(to)) = (r.there(src), r.there(dst)) {
                return wait(r.file_move(&from, &to));
            }
        }
        crate::import::move_file(src, dst)
    }

    /// What the settings page and the check-up show of each helper.
    pub fn helper_report(&self) -> Vec<Value> {
        let places = self.placement();
        self.remotes()
            .iter()
            .map(|r| {
                let seen = r.seen();
                let serves: Vec<String> = places.0.iter().filter(|p| [&p.0.movie_root, &p.0.series_root].into_iter().any(|root| !root.is_empty() && r.covers(Path::new(root)))).map(|p| p.0.name.clone()).collect();
                let running = r.jobs().iter().filter(|j| !j.state.is_terminal() && j.state != JobState::Paused).count();
                json!({"id": r.conf.id, "name": r.conf.name, "url": r.conf.url, "serves": serves, "downloads": running, "seen": seen})
            })
            .collect()
    }
}
