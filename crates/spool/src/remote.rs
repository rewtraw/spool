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
//! A helper is one end of a link (see [`crate::link`]), and before anything is asked of it
//! the two settle what they both speak and what it allows. One that hands files to players
//! is sent the players named in its settings, each with a ticket for the one file.
//!
//! A helper that cannot be reached takes nothing with it. Downloads for its part of the
//! library run here and are filed across the share, as they would be without it, and what
//! it was already doing is waited for and not written off.

use crate::app::{App, Event};
use crate::helper::Pushed;
use crate::link::{pair, tailnet};
use crate::link::ticket::{plain, Signer};
use crate::local::{Blocked, FolderSummary, Survey};
use crate::link::{agreed, Grants, Hello, Net, Share};
use crate::models::*;
use crate::settings::{Folder, LinkConf};
use anyhow::{anyhow, bail, Result};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use spool_nntp::{JobState, JobStatus};
use std::collections::HashMap;
use std::net::IpAddr;
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
    /// It answers, does work for this Spool, and the two speak of work alike.
    pub usable: bool,
    /// It hands files to players who come with a ticket.
    pub play: bool,
    /// It looks through its own folders when asked, and says what is in them.
    pub see: bool,
    /// It makes compact versions on its video hardware, and hands them back.
    pub make: bool,
    /// What it would make them with: "hardware", "software" or "none".
    pub make_means: String,
    /// What it allows this Spool.
    pub grants: Grants,
    /// The folders it shares.
    pub shares: Vec<Share>,
    /// Its folder of finished downloads can be read from this machine, which filing needs.
    pub downloads_seen: bool,
    pub paused: bool,
    /// Room in its downloads folder.
    pub free: Option<u64>,
    pub busy: Vec<String>,
    #[serde(skip)]
    boot: String,
    #[serde(skip)]
    pushed: Option<Pushed>,
    /// Its last report of its downloads could not be read, so what is kept of them is old.
    #[serde(skip)]
    stale: bool,
    /// When to ask again what it is: something was wrong, and may have been put right.
    #[serde(skip)]
    again: Option<Instant>,
    /// The mark on its last report. One that marks its reports keeps the next request
    /// waiting until something has changed.
    #[serde(skip)]
    stamp: String,
}

/// Why a linked Spool did not do what it was asked.
#[derive(Debug)]
pub enum Fail {
    /// No answer came. `sent` is false only when it is certain that nothing reached it.
    Away { why: String, sent: bool },
    /// It answered, and said no.
    Refused { status: u16, why: String },
    /// It answered in a way that cannot be read.
    Garbled(String),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Away { why, .. } | Fail::Refused { why, .. } | Fail::Garbled(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Fail {}

type Asked<T> = std::result::Result<T, Fail>;

/// The name tickets to read a film for a compact version are made out in, where a share's
/// name would be.
const SOURCE: &str = "make";

/// How long a helper is left to answer when it has nothing new to say, in seconds.
const HOLD: u64 = 25;

pub struct Remote {
    pub conf: LinkConf,
    http: reqwest::Client,
    signer: Signer,
    /// The address it last answered from, which says what network it is on.
    at: Mutex<Option<IpAddr>>,
    /// Where it answers over Tailscale, when it is on Tailscale as well as the network
    /// Spool reaches it on: the address a player away from home is sent to.
    tailnet: Mutex<Option<String>>,
    /// Something is keeping in step with it.
    following: std::sync::atomic::AtomicBool,
    /// When the folders it is mounted under here were last looked for.
    sought: Mutex<Option<Instant>>,
    /// Its downloads as last reported, named and placed as this machine knows them.
    jobs: Mutex<Vec<JobStatus>>,
    /// Downloads handed over a moment ago, which a report already on its way back may not
    /// have in it yet.
    fresh: Mutex<HashMap<String, (Instant, JobStatus)>>,
    seen: Mutex<Seen>,
    /// It lately could not make a compact version it was given, and is given no more until then.
    make_rest: Mutex<Option<Instant>>,
}

/// How long a machine that could not make a compact version is given no more of them.
const MAKE_REST: Duration = Duration::from_secs(600);

/// Link with the Spool at an address by the code it gave: the two agree a key, which is
/// returned once the other side has taken it. Nothing is kept here; whoever asked puts the
/// key in the link's settings.
pub async fn pair(url: &str, code: &str) -> std::result::Result<String, String> {
    let base = url.trim().trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err("the address must start with http:// or https://".into());
    }
    if pair::tidy(code).len() < pair::DIGITS {
        return Err(format!("the code has {} digits", pair::DIGITS));
    }
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(4)).timeout(Duration::from_secs(15)).build().map_err(|e| e.to_string())?;
    let said = |status: reqwest::StatusCode, body: &Value| match (status.as_u16(), body["error"].as_str()) {
        (404, None) => "nothing there links with a code".to_string(),
        (_, Some(why)) => why.to_string(),
        (code, None) => format!("it answered {code}"),
    };
    let away = |e: reqwest::Error| format!("it did not answer: {}", e.without_url());
    let (asking, msg) = pair::Asking::start(code);
    let resp = http.post(format!("{base}/link/pair/v1/start")).json(&json!({"msg": msg})).send().await.map_err(away)?;
    let (status, body) = (resp.status(), resp.json::<Value>().await.unwrap_or_default());
    if !status.is_success() {
        return Err(said(status, &body));
    }
    let (Some(session), Some(gave), Some(proof)) = (body["session"].as_str(), body["msg"].as_str(), body["proof"].as_str()) else { return Err("it answered in a way this Spool cannot read".into()) };
    let (key, proof) = asking.finish(gave, proof).map_err(|e| match e {
        pair::Failed::WrongCode => "the code is not the one it gave".to_string(),
        pair::Failed::Garbled => "it answered in a way this Spool cannot read".to_string(),
    })?;
    // The answer to this may be lost after the other side has taken the key. Asked again,
    // it says so; and failing that, the key itself is tried.
    let finish = json!({"session": session, "proof": proof});
    let mut why = String::new();
    for _ in 0..3 {
        match http.post(format!("{base}/link/pair/v1/finish")).json(&finish).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(key),
            // It may have taken the key, lost its answer, and started again since.
            Ok(resp) => {
                let status = resp.status();
                why = said(status, &resp.json::<Value>().await.unwrap_or_default());
                break;
            }
            Err(e) => {
                why = away(e);
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
    let taken = http.get(format!("{base}/link")).bearer_auth(&key).send().await.is_ok_and(|r| r.status().is_success());
    if taken { Ok(key) } else { Err(why) }
}

/// Something a linked machine is asked to find in its own folders.
trait Looked: serde::de::DeserializeOwned + Send + 'static {
    /// What it is called in the address it is asked for at.
    const WHAT: &'static str;
    /// Reading the same folder from this machine.
    fn local(root: &Path, kind: Kind) -> std::result::Result<Self, Blocked>;
    /// Whether every name in it is one that can be joined to a folder here and stay inside.
    fn sound(&self) -> bool;
}

impl Looked for Survey {
    const WHAT: &'static str = "survey";
    fn local(root: &Path, kind: Kind) -> std::result::Result<Self, Blocked> {
        crate::local::survey(root, kind, true)
    }
    fn sound(&self) -> bool {
        let l = &self.local;
        self.videos.iter().map(|v| &v.rel).chain(l.extras.iter().map(|e| &e.rel_path)).chain(l.art.iter().map(|a| &a.rel_path)).chain(&l.themes).all(|rel| plain(rel)) && (l.notes.rel_path.is_empty() || plain(&l.notes.rel_path))
    }
}

impl Looked for Vec<FolderSummary> {
    const WHAT: &'static str = "folders";
    fn local(root: &Path, kind: Kind) -> std::result::Result<Self, Blocked> {
        crate::local::folders(root, kind, true)
    }
    fn sound(&self) -> bool {
        self.iter().all(|f| plain(&f.name) && !f.name.contains('/'))
    }
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
    fn new(conf: LinkConf) -> Remote {
        let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(4)).build().expect("http client");
        let signer = Signer::new(&conf.key);
        Remote { conf, http, signer, at: Mutex::new(None), tailnet: Mutex::new(None), following: Default::default(), sought: Mutex::new(None), jobs: Mutex::new(vec![]), fresh: Mutex::new(HashMap::new()), seen: Mutex::new(Seen::default()), make_rest: Mutex::new(None) }
    }

    /// Where a player at this address can take a file from the linked machine itself: an
    /// address with a ticket for that one file. None when the player is not one of those
    /// sent there, or the machine is not handing files out just now.
    pub fn direct(&self, here: &Path, player: &[IpAddr]) -> Option<String> {
        if !self.conf.play_near {
            return None;
        }
        // One on the machine's own network takes the file where Spool reaches the machine.
        // One elsewhere on Tailscale takes it over Tailscale, if the machine is there too.
        let base = if self.near(player) {
            self.conf.url.clone()
        } else if player.iter().any(|p| tailnet::ours(*p)) {
            self.tailnet.lock().clone()?
        } else {
            return None;
        };
        let there = self.there(here)?;
        let seen = self.seen.lock();
        if !(seen.reachable && seen.play) {
            return None;
        }
        let (share, rel) = crate::link::place(&seen.shares, &there)?;
        Some(format!("{}/link/play/v1/file?ticket={}", base.trim_end_matches('/'), self.signer.issue(&share.id, &rel, crate::db::now().max(0) as u64)))
    }

    /// Whether a player is on the network this machine is reached on: the same stretch of
    /// addresses as the one it answers from, or Tailscale when that is how it is reached.
    fn near(&self, player: &[IpAddr]) -> bool {
        let Some(at) = *self.at.lock() else { return false };
        if tailnet::ours(at) {
            return player.iter().any(|p| tailnet::ours(*p));
        }
        player.iter().any(|p| Net::around(at).holds(*p))
    }

    pub fn seen(&self) -> Seen {
        self.seen.lock().clone()
    }

    /// Whether any of the library is its to hold or work on. One that only makes compact
    /// versions holds none: its being away is no trouble, and nobody is told of it.
    pub fn holds_library(&self) -> bool {
        let g = self.seen.lock().grants;
        !self.conf.folders.is_empty() || g.work || g.play || g.see
    }

    /// Whether it is to be given a compact version to make now: it offers to, its settings
    /// say now is a time for it, and it has not just failed at one.
    pub fn makes(&self) -> bool {
        let seen = self.seen.lock();
        seen.reachable && seen.make && self.conf.make.now() && self.make_rest.lock().is_none_or(|until| Instant::now() >= until)
    }

    /// Give it no more compact versions to make for a while.
    pub fn rest_from_making(&self) {
        *self.make_rest.lock() = Some(Instant::now() + MAKE_REST);
    }

    /// A ticket on which it may read one file from this Spool, to make a compact version of.
    pub fn source_ticket(&self, file_id: i64) -> String {
        self.signer.issue(SOURCE, &file_id.to_string(), crate::db::now().max(0) as u64)
    }

    /// The file a ticket of ours lets it read, if the ticket is good.
    pub fn source_of(&self, token: &str) -> Option<i64> {
        let t = self.signer.verify(token, crate::db::now().max(0) as u64).ok()?;
        (t.share == SOURCE).then(|| t.rel.parse().ok()).flatten()
    }

    /// Ask it to make a compact version. See [`crate::helper::make`].
    pub async fn make_start(&self, ask: Value) -> std::result::Result<(), Fail> {
        self.post("/link/make/v1/jobs", ask, Duration::from_secs(20)).await.map(|_| ())
    }

    pub async fn make_state(&self, id: &str) -> std::result::Result<Value, Fail> {
        self.get(&format!("/link/make/v1/jobs/{id}")).await
    }

    /// Have it stop making one, or let go of one that has been taken.
    pub async fn make_end(&self, id: &str) {
        let url = format!("{}/link/make/v1/jobs/{id}", self.conf.url.trim_end_matches('/'));
        let _ = self.send(self.http.delete(url), Duration::from_secs(8)).await;
    }

    /// Bring what it made to a file here, counting the bytes as they come.
    pub async fn make_fetch(&self, id: &str, to: &Path, got: &std::sync::atomic::AtomicU64, stop: &std::sync::atomic::AtomicBool) -> Result<u64> {
        use tokio::io::AsyncWriteExt;
        let url = format!("{}/link/make/v1/jobs/{id}/file", self.conf.url.trim_end_matches('/'));
        let mut resp = tokio::time::timeout(Duration::from_secs(30), self.http.get(url).bearer_auth(&self.conf.key).send()).await.map_err(|_| anyhow!("it did not begin to send"))?.map_err(|e| anyhow!("{}", e.without_url()))?;
        if !resp.status().is_success() {
            bail!("it would not hand it over: {}", resp.status());
        }
        let mut file = tokio::fs::File::create(to).await?;
        let mut all = 0u64;
        // A machine that stops sending is not waited on for ever.
        while let Some(chunk) = tokio::time::timeout(Duration::from_secs(60), resp.chunk()).await.map_err(|_| anyhow!("it stopped sending"))?.map_err(|e| anyhow!("{}", e.without_url()))? {
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                bail!("it was called off");
            }
            file.write_all(&chunk).await?;
            all += chunk.len() as u64;
            got.store(all, std::sync::atomic::Ordering::Relaxed);
        }
        file.sync_all().await?;
        Ok(all)
    }

    /// What marks a download as this helper's among all of Spool's.
    fn prefix(&self) -> String {
        format!("@{}:", self.conf.id)
    }

    /// Whether a path on this machine is on the helper's disks.
    pub fn covers(&self, here: &Path) -> bool {
        self.conf.folders.iter().any(|f| here.starts_with(&f.here))
    }

    /// Turn a path from one machine's name for it to the other's, by the folder it is in:
    /// the innermost, where one folder lies inside another.
    fn turned(&self, path: &Path, from: impl Fn(&Folder) -> &str, to: impl Fn(&Folder) -> &str) -> Option<PathBuf> {
        let folder = self.conf.folders.iter().filter(|f| path.starts_with(from(f))).max_by_key(|f| from(f).len())?;
        Some(Path::new(to(folder)).join(path.strip_prefix(from(folder)).ok()?))
    }

    /// A path on this machine, as the helper knows it.
    pub fn there(&self, here: &Path) -> Option<PathBuf> {
        self.turned(here, |f| &f.here, |f| &f.there)
    }

    /// A path on the helper, as this machine sees it.
    pub fn here(&self, there: &Path) -> Option<PathBuf> {
        self.turned(there, |f| &f.there, |f| &f.here)
    }

    /// The folders it shares that have not been found on this machine.
    pub fn unfound(&self) -> Vec<PathBuf> {
        self.seen.lock().shares.iter().map(|s| s.path.clone()).filter(|p| self.here(p).is_none()).collect()
    }

    async fn send(&self, req: reqwest::RequestBuilder, patience: Duration) -> Asked<Value> {
        let name = &self.conf.name;
        let away = |e: reqwest::Error| Fail::Away { sent: !e.is_connect(), why: format!("{name}'s helper did not answer: {}", e.without_url()) };
        let resp = req.bearer_auth(&self.conf.key).timeout(patience).send().await.map_err(away)?;
        if let Some(from) = resp.remote_addr() {
            *self.at.lock() = Some(from.ip());
        }
        let status = resp.status();
        let body: Option<Value> = serde_json::from_slice(&resp.bytes().await.map_err(away)?).ok();
        if !status.is_success() {
            let why = body.as_ref().and_then(|b| b["error"].as_str()).unwrap_or(status.as_str());
            return Err(Fail::Refused { status: status.as_u16(), why: format!("{name}'s helper refused: {why}") });
        }
        // An answer that cannot be read is never taken for one that says there is nothing.
        body.ok_or_else(|| Fail::Garbled(format!("{name}'s helper answered in a way this Spool cannot read")))
    }

    async fn get(&self, path: &str) -> Asked<Value> {
        self.send(self.http.get(format!("{}{path}", self.conf.url.trim_end_matches('/'))), Duration::from_secs(8)).await
    }

    async fn post(&self, path: &str, body: Value, patience: Duration) -> Asked<Value> {
        self.send(self.http.post(format!("{}{path}", self.conf.url.trim_end_matches('/'))).json(&body), patience).await
    }

    async fn post_bytes(&self, path: &str, bytes: &[u8]) -> Asked<Value> {
        self.send(self.http.post(format!("{}{path}", self.conf.url.trim_end_matches('/'))).body(bytes.to_vec()), Duration::from_secs(30)).await
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

    async fn add(&self, nzb: &[u8], name: &str) -> Asked<String> {
        // The request is given a name of its own. If its answer is lost it is asked again
        // under that name, and the helper starts the download once however often it hears.
        let path = format!("/link/work/v1/jobs?name={}&rid={}", urlencoding::encode(name), &crate::settings::new_key()[..16]);
        // Once one try may have got through, the helper may be downloading, whatever a
        // later try says: only its own word, under the same name, settles it.
        let mut in_doubt: Option<String> = None;
        let mut own = None;
        for _ in 0..3 {
            match self.post_bytes(&path, nzb).await {
                Ok(said) => match said["id"].as_str() {
                    Some(id) => {
                        own = Some(id.to_string());
                        break;
                    }
                    None => in_doubt = Some(format!("{}'s helper gave no name for the download", self.conf.name)),
                },
                Err(Fail::Away { sent: true, why } | Fail::Garbled(why)) => in_doubt = Some(why),
                Err(e) if in_doubt.is_none() => return Err(e),
                Err(_) => {}
            }
        }
        let garbled = || Fail::Garbled(format!("{}'s helper gave no name for the download", self.conf.name));
        let Some(own) = own else { return Err(Fail::Away { sent: true, why: in_doubt.unwrap_or_default() }) };
        let id = format!("{}{own}", self.prefix());
        let placeholder: JobStatus = serde_json::from_value(json!({
            "id": id, "name": name, "state": "queued", "total_bytes": spool_nntp::nzb::parse(nzb).map(|n| n.bytes()).unwrap_or(0), "done_bytes": 0, "speed": 0, "eta_secs": null,
            "missing_articles": 0, "damaged_articles": 0, "message": "", "error": null, "output_path": null, "added_at": crate::db::now(),
        }))
        .map_err(|_| garbled())?;
        let mut fresh = self.fresh.lock();
        fresh.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(15));
        fresh.insert(id.clone(), (Instant::now(), placeholder));
        Ok(id)
    }

    /// One action on one download, named as the helper names it.
    async fn act(&self, own: &str, action: &str, body: Value) -> Result<()> {
        Ok(self.post(&format!("/link/work/v1/jobs/{own}/{action}"), body, Duration::from_secs(15)).await.map(|_| ())?)
    }

    async fn remove(&self, own: &str, files: bool) -> Result<()> {
        let url = format!("{}/link/work/v1/jobs/{own}?files={files}", self.conf.url.trim_end_matches('/'));
        self.send(self.http.delete(url), Duration::from_secs(30)).await?;
        self.jobs.lock().retain(|j| j.id != format!("{}{own}", self.prefix()));
        Ok(())
    }

    /// File one thing into the library on the helper's own disks. A film copied between two
    /// of its disks takes a while, so it is given a long time to answer.
    pub async fn file_move(&self, from: &Path, to: &Path) -> Result<()> {
        Ok(self.post("/link/work/v1/fs/move", json!({"from": from, "to": to}), Duration::from_secs(4 * 3600)).await.map(|_| ())?)
    }

    /// Clear away the folder a finished download left.
    pub async fn remove_output(&self, there: &Path) -> Result<()> {
        Ok(self.post("/link/work/v1/fs/remove", json!({"path": there}), Duration::from_secs(120)).await.map(|_| ())?)
    }

    pub async fn stat(&self, there: &Path) -> Result<Value> {
        Ok(self.post("/link/work/v1/fs/stat", json!({"path": there}), Duration::from_secs(10)).await?)
    }

    pub async fn move_start(&self, from: &Path, to: &Path, working: &Path, keep_original: bool) -> Result<String> {
        let said = self.post("/link/work/v1/moves", json!({"from": from, "to": to, "working": working, "keep_original": keep_original}), Duration::from_secs(30)).await?;
        said["id"].as_str().map(str::to_string).ok_or_else(|| anyhow!("{}'s helper gave no name for the move", self.conf.name))
    }

    /// How a move stands. `Ok(None)` is the helper saying it knows of no such move, which
    /// is what one that has restarted says.
    pub async fn move_state(&self, id: &str) -> Result<Option<Value>> {
        match self.get(&format!("/link/work/v1/moves/{id}")).await {
            Ok(said) => Ok(Some(said)),
            Err(Fail::Refused { status: 404, .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn move_cancel(&self, id: &str) -> Result<()> {
        Ok(self.post(&format!("/link/work/v1/moves/{id}/cancel"), json!({}), Duration::from_secs(15)).await.map(|_| ())?)
    }

    pub async fn move_finish(&self, id: &str, remove_original: bool) -> Result<()> {
        Ok(self.post(&format!("/link/work/v1/moves/{id}/finish"), json!({"remove_original": remove_original}), Duration::from_secs(600)).await.map(|_| ())?)
    }

    /// Have the linked machine look through one of its own folders, known here as `here`.
    /// None when it does not do that for this Spool, or the folder is in none of the
    /// folders it shares, and whoever asks has to look for themselves.
    async fn look<T: Looked>(&self, here: &Path, kind: Kind) -> Option<Asked<T>> {
        let there = self.there(here)?;
        let (share, rel) = {
            let seen = self.seen.lock();
            if !(seen.reachable && seen.see) {
                return None;
            }
            let share = seen.shares.iter().find(|s| there.starts_with(&s.path))?;
            (share.id.clone(), there.strip_prefix(&share.path).ok()?.to_string_lossy().to_string())
        };
        let said = self.post(&format!("/link/see/v1/{}", T::WHAT), json!({"share": share, "rel": rel, "kind": kind}), Duration::from_secs(120)).await;
        let garbled = || Fail::Garbled(format!("{}'s helper said what is in {} in a way this Spool cannot read", self.conf.name, here.display()));
        Some(said.and_then(|v| serde_json::from_value::<T>(v).ok().filter(T::sound).ok_or_else(garbled)))
    }

    /// What is known of the helper when it does not answer.
    fn away(&self, why: String) {
        let mut seen = self.seen.lock();
        (seen.asked, seen.reachable, seen.usable, seen.play, seen.see, seen.make, seen.problem, seen.again) = (true, false, false, false, false, false, why, None);
    }

    /// Find out what the helper is: what it speaks, what it allows, and what it shares.
    /// Nothing is asked of it until this has been settled.
    async fn meet(&self) {
        let name = &self.conf.name;
        let mut next = Seen { asked: true, reachable: true, ..Default::default() };
        match self.get("/link").await {
            Ok(said) => match serde_json::from_value::<Hello>(said) {
                Ok(hello) => {
                    let work = hello.grants.work && agreed(crate::link::WORK, &hello.protocols.work).is_some();
                    next.play = hello.grants.play && agreed(crate::link::PLAY, &hello.protocols.play).is_some();
                    next.see = hello.grants.see && agreed(crate::link::SEE, &hello.protocols.see).is_some();
                    let make = hello.grants.make && agreed(crate::link::MAKE, &hello.protocols.make).is_some();
                    (next.boot, next.version, next.grants, next.shares) = (hello.boot, hello.version, hello.grants, hello.shares);
                    if make {
                        // Only video hardware is worth sending a film across the network to.
                        next.make_means = self.get("/link/make/v1/info").await.ok().and_then(|i| i["means"].as_str().map(str::to_string)).unwrap_or_default();
                        next.make = next.make_means == "hardware";
                    }
                    if work {
                        self.meet_work(&mut next).await;
                    } else if hello.grants.work {
                        next.problem = format!("{name}'s helper is version {} and does not speak of work as this Spool does; update the two", next.version);
                    } else if !(next.play || next.see || make) {
                        next.problem = format!("{name}'s helper allows this Spool nothing it can use");
                    }
                }
                Err(_) => next.problem = format!("{name}'s helper answered in a way this Spool cannot read"),
            },
            Err(Fail::Away { why, .. }) => return self.away(why),
            Err(Fail::Refused { why, .. } | Fail::Garbled(why)) => next.problem = why,
        }
        // Whatever is wrong may be put right without anything here being restarted: the
        // share mounted, the helper updated, its key corrected. One that only plays is not
        // asked for downloads, so this is also how it is known to be there still.
        let secs = if !next.problem.is_empty() || (next.usable && !next.downloads_seen) { Some(20) } else if !next.usable { Some(5) } else { None };
        next.again = secs.map(|secs| Instant::now() + Duration::from_secs(secs));
        let boot = next.boot.clone();
        *self.seen.lock() = next;
        *self.tailnet.lock() = self.on_tailscale(&boot).await;
    }

    /// Where the machine answers over Tailscale, if it is on this machine's Tailscale under
    /// the name the link has and is not reached that way already. It is the same machine
    /// only if it says it started when this one did: a name alone settles nothing.
    async fn on_tailscale(&self, boot: &str) -> Option<String> {
        if boot.is_empty() || self.at.lock().is_none_or(tailnet::ours) {
            return None;
        }
        let url = reqwest::Url::parse(&self.conf.url).ok()?;
        for ip in tailnet::addresses_of(&self.conf.name).await {
            let mut there = url.clone();
            if there.set_ip_host(ip).is_err() {
                continue;
            }
            let there = there.as_str().trim_end_matches('/').to_string();
            let Ok(answer) = self.http.get(format!("{there}/link")).bearer_auth(&self.conf.key).timeout(Duration::from_secs(3)).send().await else { continue };
            if answer.json::<Hello>().await.is_ok_and(|hello| hello.boot == boot) {
                return Some(there);
            }
        }
        None
    }

    /// The part of meeting a helper that is about work: that it speaks as this Spool does,
    /// and that what it downloads can be read from here, which filing needs.
    async fn meet_work(&self, next: &mut Seen) {
        let name = &self.conf.name;
        let info = match self.get("/link/work/v1/info").await {
            Ok(info) => info,
            Err(e) => return next.problem = e.to_string(),
        };
        next.usable = true;
        let complete = PathBuf::from(info["complete"].as_str().unwrap_or_default());
        match self.here(&complete) {
            Some(here) => {
                let looked = here.clone();
                if tokio::task::spawn_blocking(move || crate::locations::listable(&looked)).await.unwrap_or(false) {
                    next.downloads_seen = true;
                } else {
                    next.problem = format!("{name}'s helper downloads into {}, which this machine should see at {} and cannot; share that folder and mount it, and until then downloads run here", complete.display(), here.display());
                }
            }
            None => next.problem = format!("{name}'s helper downloads into {}, which has not been found among the shares mounted on this machine. Share that folder and mount it here, and Spool finds it by itself; until then downloads run here", complete.display()),
        }
    }

    /// Ask the helper how things stand. Returns the downloads that changed since last time.
    /// With `wait`, a helper that marks its reports is left to answer when something has
    /// changed, or after that many seconds.
    async fn ask(&self, wanted: &Pushed, wait: u64) -> Vec<JobStatus> {
        let due = {
            let seen = self.seen.lock();
            !seen.asked || !seen.reachable || seen.again.is_some_and(|at| Instant::now() >= at)
        };
        if due {
            self.meet().await;
        }
        if !self.seen.lock().usable {
            return vec![];
        }
        // Its settings first: a request that waits for news would keep them waiting too.
        if self.seen.lock().pushed.as_ref() != Some(wanted) {
            let url = format!("{}/link/work/v1/config", self.conf.url.trim_end_matches('/'));
            match self.send(self.http.put(url).json(wanted), Duration::from_secs(15)).await {
                Ok(_) => self.seen.lock().pushed = Some(wanted.clone()),
                Err(e) => tracing::warn!(helper = %self.conf.name, error = %e, "could not give the helper its settings"),
            }
        }
        let stamp = self.seen.lock().stamp.clone();
        let asked = match (wait, stamp.is_empty()) {
            (0, _) | (_, true) => self.get("/link/work/v1/jobs").await,
            _ => self.send(self.http.get(format!("{}/link/work/v1/jobs", self.conf.url.trim_end_matches('/'))).query(&[("since", stamp.as_str()), ("wait", &wait.to_string())]), Duration::from_secs(wait + 15)).await,
        };
        let said = match asked {
            Ok(said) => said,
            Err(Fail::Away { why, .. }) => {
                self.away(why);
                return vec![];
            }
            Err(Fail::Garbled(why)) => return self.unread(why),
            // It no longer does what it did when it was met. Nothing more is asked of it,
            // and nobody is sent to it, until it has been met again.
            Err(Fail::Refused { why, .. }) => {
                let mut seen = self.seen.lock();
                (seen.usable, seen.play, seen.stale, seen.problem, seen.again) = (false, false, true, why, Some(Instant::now()));
                return vec![];
            }
        };
        // It has restarted, and may not be what it was.
        if said["boot"].as_str() != Some(self.seen.lock().boot.as_str()) {
            self.meet().await;
            if !self.seen.lock().usable {
                return vec![];
            }
        }
        // Every download has to be understood, or none of the report is believed: a
        // download missing from a report that could only half be read has not gone.
        let read: Option<Vec<JobStatus>> = said["jobs"].as_array().and_then(|list| list.iter().map(|j| serde_json::from_value::<JobStatus>(j.clone()).ok()).collect());
        let Some(now) = read else { return self.unread(format!("{}'s helper reported its downloads in a way this Spool cannot read; what was last known of them is kept", self.conf.name)) };
        let now: Vec<JobStatus> = now.into_iter().map(|j| self.adopt(j)).collect();
        {
            let mut seen = self.seen.lock();
            if std::mem::take(&mut seen.stale) {
                seen.problem.clear();
            }
            seen.stamp = said["stamp"].as_str().unwrap_or_default().to_string();
            seen.paused = said["paused"].as_bool().unwrap_or(false);
            seen.free = said["free"].as_u64();
            seen.busy = said["busy"].as_array().map(|b| b.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
        }
        let mut jobs = self.jobs.lock();
        let changed = now
            .iter()
            .filter(|j| jobs.iter().find(|o| o.id == j.id).is_none_or(|o| (o.state, o.done_bytes, o.speed, &o.message, &o.error) != (j.state, j.done_bytes, j.speed, &j.message, &j.error)))
            .cloned()
            .collect();
        *jobs = now;
        changed
    }

    /// The helper answered, and what it said of its downloads is of no use. What was known
    /// of them is kept, and marked as old.
    fn unread(&self, why: String) -> Vec<JobStatus> {
        let mut seen = self.seen.lock();
        (seen.stale, seen.problem) = (true, why);
        vec![]
    }

    /// Whether a download missing from the helper's last report has truly gone.
    fn report_is_current(&self) -> bool {
        let seen = self.seen.lock();
        seen.reachable && seen.usable && !seen.stale
    }
}

/// Check the links as the settings page sends them, and number the new ones. A number is
/// never given twice, so a download that ran on a link since removed is never taken for
/// another's. Returns the links and the highest number given so far.
pub fn tidy_links(list: Vec<LinkConf>, old: &[LinkConf], seq: u32) -> std::result::Result<(Vec<LinkConf>, u32), String> {
    let clean = |p: &str| p.trim().trim_end_matches('/').to_string();
    let web = |url: &str| url.starts_with("http://") || url.starts_with("https://");
    let mut next = old.iter().chain(&list).map(|h| h.id).max().unwrap_or(0).max(seq);
    let mut out: Vec<LinkConf> = vec![];
    for mut h in list {
        (h.name, h.url, h.key) = (h.name.trim().to_string(), clean(&h.url), h.key.trim().to_string());
        for f in &mut h.folders {
            (f.here, f.there) = (clean(&f.here), clean(&f.there));
        }
        if h.name.is_empty() {
            return Err("every helper needs a name".into());
        }
        if !web(&h.url) {
            return Err(format!("the address of {}'s helper must start with http:// or https://", h.name));
        }
        if h.key.is_empty() {
            return Err(format!("{}'s helper needs its key", h.name));
        }
        if h.folders.iter().any(|f| !f.here.starts_with('/') || !f.there.starts_with('/')) {
            return Err(format!("the folders of {}'s helper must be full paths, starting with /", h.name));
        }
        // A folder is one helper's to work in, never two's.
        let shared = |a: &LinkConf, b: &LinkConf| a.folders.iter().any(|x| b.folders.iter().any(|y| Path::new(&x.here).starts_with(&y.here) || Path::new(&y.here).starts_with(&x.here)));
        if let Some(other) = out.iter().find(|o| shared(o, &h)) {
            return Err(format!("{} and {} are given the same folder on this machine; each folder is one helper's", other.name, h.name));
        }
        // A number is kept only by the link that already has it. Whatever else is sent, a
        // new link gets a new number.
        if !old.iter().any(|o| o.id == h.id) || out.iter().any(|o| o.id == h.id) {
            next = next.checked_add(1).ok_or("no more links can be numbered")?;
            h.id = next;
        }
        out.push(h);
    }
    Ok((out, next))
}

impl App {
    pub fn remotes(&self) -> Vec<Arc<Remote>> {
        self.remotes.lock().clone()
    }

    /// Bring the helpers Spool keeps in line with its settings. One whose settings have not
    /// changed keeps what is known of it.
    pub fn sync_remotes(&self) {
        let wanted: Vec<LinkConf> = self.settings.general().links.into_iter().filter(|h| !h.url.trim().is_empty()).collect();
        let mut remotes = self.remotes.lock();
        *remotes = wanted.into_iter().map(|c| remotes.iter().find(|r| r.conf == c).cloned().unwrap_or_else(|| Arc::new(Remote::new(c)))).collect();
    }

    /// What the helpers are given of Spool's settings.
    fn pushed(&self) -> Pushed {
        let g = self.settings.general();
        Pushed { servers: self.settings.servers(), speed_limit: self.settings.speed_limit(), direct_unpack: g.direct_unpack, post_parallel: g.post_parallel as usize }
    }

    /// Ask one helper how things stand, and act on what has changed.
    async fn turn(&self, r: &Arc<Remote>, wait: u64) {
        let was = r.seen();
        let changed = r.ask(&self.pushed(), wait).await;
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
            } else if r.holds_library() {
                tracing::warn!(helper = %r.conf.name, problem = %now.problem, "the helper cannot be reached; downloads for its part of the library run here until it is back");
            } else {
                // A machine that only makes compact versions may be asleep, and nothing waits on it.
                tracing::info!(machine = %r.conf.name, "a linked machine that makes compact versions cannot be reached; it is given none until it is back");
            }
            self.emit(Event::Locations);
        }
        for job in changed {
            self.on_job(job);
        }
        self.settle_folders(r, false).await;
        // A machine that has come back, or whose hours have come, takes up the line.
        self.compact_start();
        let problems: Vec<String> = self.remotes().iter().filter(|r| r.holds_library()).map(|r| r.seen().problem).filter(|p| !p.is_empty()).collect();
        let message = (!problems.is_empty()).then(|| problems.join(" "));
        if self.db.set_standing_attention("helper", message.as_deref(), json!({})).unwrap_or(false) {
            self.emit(Event::Attention);
        }
    }

    /// Find each folder a linked machine works in among the folders on this one. The
    /// same files are under different names on the two machines, and nothing is taken from
    /// the names: the other machine marks each of its folders with a file nobody could
    /// guess the name of, and the folder here that has that file in it is the same folder.
    /// One it may not write in is known by a file it holds, of the same size, and only if
    /// one folder here has it.
    ///
    /// `places` is where to look: the shares mounted on this machine, and the folders
    /// already in use. Each is looked in, and the folders inside it two deep.
    pub async fn find_folders(&self, r: &Arc<Remote>, places: Vec<PathBuf>) -> Vec<Folder> {
        let Ok(said) = r.post("/link/work/v1/marks", json!({}), Duration::from_secs(20)).await else { return vec![] };
        let marks: Vec<Value> = said["folders"].as_array().cloned().unwrap_or_default();
        let found = tokio::task::spawn_blocking(move || {
            // What tells a folder here for the one a mark is of.
            let is = |dir: &Path, mark: &Value| match (mark["name"].as_str(), mark["sample"].as_str(), mark["size"].as_u64()) {
                (Some(name), _, _) if plain(name) && !name.contains('/') => dir.join(name).symlink_metadata().is_ok(),
                (None, Some(rel), Some(size)) if plain(rel) => std::fs::metadata(dir.join(rel)).is_ok_and(|m| m.is_file() && m.len() == size),
                _ => false,
            };
            let mut hits: Vec<Vec<PathBuf>> = vec![vec![]; marks.len()];
            let (mut level, mut looked) = (places, 0usize);
            for depth in 0..3 {
                let mut inside = vec![];
                for dir in &level {
                    looked += 1;
                    for (hit, mark) in hits.iter_mut().zip(&marks) {
                        if is(dir, mark) && !hit.contains(dir) {
                            hit.push(dir.clone());
                        }
                    }
                    // No deeper once every marked folder has been found, or after a great many.
                    if depth < 2 && looked < 5000 && hits.iter().zip(&marks).any(|(h, m)| h.is_empty() && m["name"].is_string()) {
                        inside.extend(std::fs::read_dir(dir).into_iter().flatten().flatten().filter(|e| !e.file_name().to_string_lossy().starts_with('.')).map(|e| e.path()).filter(|p| p.is_dir()));
                    }
                }
                level = inside;
            }
            // A folder is found when exactly one here answers to it: two that do settle nothing.
            marks.iter().zip(hits).filter_map(|(mark, hit)| match hit.as_slice() {
                [here] => Some(Folder { here: here.to_string_lossy().to_string(), there: mark["there"].as_str()?.to_string() }),
                _ => None,
            }).collect::<Vec<Folder>>()
        })
        .await
        .unwrap_or_default();
        let _ = r.post("/link/work/v1/marks/clear", json!({}), Duration::from_secs(10)).await;
        found
    }

    /// Whether a link's folders want finding: it has none, or one of the other machine's
    /// folders has no place here.
    fn folders_wanted(r: &Remote) -> bool {
        r.conf.folders.is_empty() || !r.unfound().is_empty() || r.seen().problem.contains("has not been found among the shares")
    }

    /// Find a link's folders when they want finding, keep them, and take the link up afresh
    /// with them. Looked for no more than twice a minute, unless `now`.
    pub async fn settle_folders(&self, r: &Arc<Remote>, now: bool) {
        let seen = r.seen();
        if !(seen.reachable && seen.grants.work) || !(now || Self::folders_wanted(r)) {
            return;
        }
        {
            let mut sought = r.sought.lock();
            if !now && sought.is_some_and(|last| last.elapsed() < Duration::from_secs(30)) {
                return;
            }
            *sought = Some(Instant::now());
        }
        // Where to look: every share mounted here, the folders of every further location
        // (asking after one mounts a share that is mounted when wanted), and where its
        // folders were.
        let mut places: Vec<PathBuf> = tokio::task::spawn_blocking(crate::app::network_mounts).await.unwrap_or_default().into_iter().map(|(_, on)| on).collect();
        places.extend(self.settings.general().locations.iter().flat_map(|l| [l.movie_root.clone(), l.series_root.clone()]).filter(|p| !p.is_empty()).map(PathBuf::from));
        places.extend(r.conf.folders.iter().map(|f| PathBuf::from(&f.here)));
        places.sort();
        places.dedup();
        // What was known of a folder that was not found this time stands: its share may
        // only be away for now.
        let mut found = self.find_folders(r, places).await;
        found.extend(r.conf.folders.iter().filter(|old| !found.iter().any(|f| f.there == old.there)).cloned().collect::<Vec<_>>());
        found.sort_by(|a, b| a.there.cmp(&b.there));
        if found == r.conf.folders {
            return;
        }
        let mut g = self.settings.general();
        let Some(link) = g.links.iter_mut().find(|l| l.id == r.conf.id) else { return };
        tracing::info!(helper = %r.conf.name, folders = found.len(), "found its folders among those on this machine");
        link.folders = found;
        if self.settings.set_general(&g).is_ok() {
            self.sync_remotes();
            self.emit(Event::Locations);
        }
    }

    /// Ask every helper how things stand, now, and act on what has changed.
    pub async fn ask_remotes(&self) {
        // Each is asked on its own, so one that is slow to answer holds up none of the others.
        futures::future::join_all(self.remotes().iter().map(|r| self.turn(r, 0))).await;
    }

    /// Whether a helper is still one of those in the settings, as it was when it was made.
    pub(crate) fn listed(&self, r: &Arc<Remote>) -> bool {
        self.remotes.lock().iter().any(|x| Arc::ptr_eq(x, r))
    }

    /// Keep in step with one helper for as long as it is in the settings. One that marks
    /// its reports is asked and left to answer when it has news, so a change is heard at
    /// once and nothing is sent while nothing happens. Any other is asked once a second.
    async fn follow(self, r: Arc<Remote>) {
        let mut quiet = 0u32;
        while self.listed(&r) {
            let began = Instant::now();
            tokio::select! {
                _ = self.turn(&r, HOLD) => {}
                // Settings it should have, or its leaving the settings, do not wait on it.
                _ = async {
                    loop {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        if !self.listed(&r) || r.seen.lock().pushed.as_ref().is_some_and(|had| *had != self.pushed()) {
                            break;
                        }
                    }
                } => {}
            }
            let seen = r.seen();
            quiet = if seen.reachable { 0 } else { (quiet + 1).min(4) };
            // Never more than once a second; one that is away is tried less and less often,
            // and one that only plays is asked nothing, only looked in on.
            let pause = match (seen.reachable, seen.usable) {
                (false, _) => Duration::from_millis(1000 * (1u64 << quiet) + began.elapsed().subsec_millis() as u64 % 500),
                (true, false) => Duration::from_secs(5),
                (true, true) => Duration::from_secs(1).saturating_sub(began.elapsed()),
            };
            tokio::time::sleep(pause).await;
        }
        r.following.store(false, std::sync::atomic::Ordering::Release);
    }

    /// Listen for Spools that say they are on the network. Not being able to costs
    /// nothing but that their addresses have to be typed.
    pub fn listen_for_spools(&self) {
        match crate::link::find::Heard::listen() {
            Ok(heard) => drop(self.heard.set(heard)),
            Err(e) => tracing::warn!(error = %e, "could not listen for Spools on the network"),
        }
    }

    /// The Spools heard of that this one is not linked to already.
    pub fn spools_found(&self) -> Vec<crate::link::find::Found> {
        let linked: Vec<String> = self.settings.general().links.into_iter().map(|l| l.url).collect();
        self.heard.get().map(|h| h.found(&linked)).unwrap_or_default()
    }

    /// Keep in step with the helpers in the settings, each on its own.
    pub fn spawn_remotes(&self) {
        self.sync_remotes();
        let app = self.clone();
        tokio::spawn(async move {
            loop {
                for r in app.remotes() {
                    if !r.following.swap(true, std::sync::atomic::Ordering::AcqRel) {
                        tokio::spawn(app.clone().follow(r));
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
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
                None if r.report_is_current() => Found::Gone,
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
                // It was asked and no answer came back. It may be downloading, so the same
                // thing is not started here beside it.
                Err(Fail::Away { sent: true, why } | Fail::Garbled(why)) => bail!("{why}. It may have taken the download, so it was not started here as well"),
                Err(e) => tracing::warn!(helper = %r.conf.name, error = %e, "the helper would not take the download; running it here"),
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
                if let Err(e) = r.post(&format!("/link/work/v1/queue/{}", if pause { "pause" } else { "resume" }), json!({}), Duration::from_secs(15)).await {
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

    /// Look through a folder: asked of the linked Spool whose disks it is on, where there
    /// is one that tells, and read from here otherwise. A linked Spool that was asked and
    /// could not say is not second-guessed across the share: the share shows the same disks,
    /// and what could not be seen there is not known to be gone. The one exception is a
    /// folder with links in it, which the linked Spool will not follow and this one may.
    async fn look<T: Looked>(&self, root: &Path, kind: Kind) -> Result<T> {
        if let Some(r) = self.remotes().into_iter().find(|r| r.covers(root)) {
            match r.look::<T>(root, kind).await {
                Some(Ok(found)) => return Ok(found),
                Some(Err(Fail::Refused { status: 409, .. })) | None => {}
                Some(Err(e)) => return Err(e.into()),
            }
        }
        let root = root.to_path_buf();
        Ok(tokio::task::spawn_blocking(move || T::local(&root, kind)).await??)
    }

    /// The films or episodes in a title's folder, and what lies beside them.
    pub async fn survey(&self, root: &Path, kind: Kind) -> Result<Survey> {
        self.look(root, kind).await
    }

    /// The folders in a library folder that hold a film or episodes.
    pub async fn folders(&self, root: &Path, kind: Kind) -> Result<Vec<FolderSummary>> {
        self.look(root, kind).await
    }

    /// Where a player can take a file from the machine that holds it, if there is a link to
    /// that machine that hands files out and the player is one of those sent there.
    /// `player` is every address the player is known by: the one it came from, and the one
    /// it is really at when it came over Tailscale. Returns the machine's name and the address.
    pub fn play_direct(&self, file: &Path, player: &[IpAddr]) -> Option<(String, String)> {
        self.remotes().iter().find_map(|r| Some((r.conf.name.clone(), r.direct(file, player)?)))
    }

    /// Whether the machine a player is about to be sent to can open the file just now. It is
    /// asked for the start of the file with the player's own ticket, and what it said stands
    /// for a minute, or a quarter of one when it could not. A machine that is slow to say is
    /// taken as one that cannot, so that the file is sent from here instead.
    pub async fn opens_there(&self, file: &Path, there: &str) -> bool {
        const STANDS: i64 = 60;
        let now = crate::db::now();
        // The same machine may be asked at its address at home or its Tailscale one, and
        // answering at one says nothing of the other.
        let key = (there.split('?').next().unwrap_or(there).to_string(), file.to_path_buf());
        if let Some((at, ok)) = self.opened.lock().get(&key).copied() {
            if now - at < if ok { STANDS } else { STANDS / 4 } {
                return ok;
            }
        }
        let ok = match self.http.head(there).timeout(std::time::Duration::from_secs(2)).send().await {
            Ok(r) if r.status().is_success() => true,
            // Busy with as many players as it takes: true of this moment and not of the file,
            // so this player has the film from here and the next is asked after afresh.
            Ok(r) if r.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE => return false,
            _ => false,
        };
        if !ok {
            tracing::warn!(file = %file.display(), "a linked machine could not open a file for a player; sending it from here");
        }
        let mut opened = self.opened.lock();
        opened.retain(|_, (at, _)| now - *at < STANDS);
        opened.insert(key, (now, ok));
        ok
    }

    /// What the settings page and the check-up show of each link.
    pub fn link_report(&self) -> Vec<Value> {
        let places = self.placement();
        self.remotes()
            .iter()
            .map(|r| {
                let seen = r.seen();
                let serves: Vec<String> = places.0.iter().filter(|p| [&p.0.movie_root, &p.0.series_root].into_iter().any(|root| !root.is_empty() && r.covers(Path::new(root)))).map(|p| p.0.name.clone()).collect();
                let running = r.jobs().iter().filter(|j| !j.state.is_terminal() && j.state != JobState::Paused).count();
                json!({"id": r.conf.id, "name": r.conf.name, "url": r.conf.url, "serves": serves, "downloads": running, "seen": seen, "at": *r.at.lock(), "tailnet": *r.tailnet.lock(), "folders": r.conf.folders, "unfound": r.unfound()})
            })
            .collect()
    }
}
