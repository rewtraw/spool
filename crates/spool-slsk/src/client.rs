//! The client: signed in to the server, listening for peers, asking for files and sending
//! those asked of it, and answering searches that reach it.
//!
//! Peers are reached the way current clients do it: the server is asked to tell the peer to
//! connect here, while a connection is tried there directly; whichever comes first is used.
//! The search network is joined as a leaf: a parent passes searches on, and no children are
//! taken.

use crate::jobs::{now, FileState, Job, Jobs};
use crate::msg::{self, distrib, init, peer, server, SearchReply};
use crate::share::Shares;
use crate::wire::{code32, code8, read_frame, In, Out};
use crate::Fetch;
use anyhow::{anyhow, bail, Result};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch, Notify, Semaphore};
use tokio::time::timeout;

/// A file asked for is asked for again after this long, in case the asking was lost.
pub(crate) const ASK_AGAIN: i64 = 600;
/// A connection to a peer that carries nothing for this long is closed.
const PEER_IDLE: Duration = Duration::from_secs(300);
/// The most a message from the server may be.
const MESSAGE_MOST: usize = 64 << 20;
/// The most a message from a peer may be: a search's results, a folder's files.
const PEER_MESSAGE_MOST: usize = 16 << 20;
/// How many connections to peers are open at once, all told; more are turned away.
const CONNECTIONS: usize = 256;
/// A write to a peer that has not gone through after this long is given up, with the connection.
const WRITE_WAIT: Duration = Duration::from_secs(120);
/// How many files one person may have waiting in the upload queue.
const QUEUE_PER_USER: usize = 1000;

#[derive(Clone, Debug)]
pub struct Config {
    pub username: String,
    pub password: String,
    /// Where the server answers: `server.slsknet.org:2242`.
    pub server: String,
    /// The port peers reach this client on.
    pub port: u16,
    /// Where the downloads are kept track of between runs.
    pub state: PathBuf,
    /// How many uploads go at once.
    pub upload_slots: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Upload {
    pub user: String,
    pub name: String,
    pub size: u64,
    pub sent: u64,
}

/// How the client stands.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    /// Signed in to the server.
    pub online: bool,
    /// What is wrong, in words; empty when nothing.
    pub problem: String,
    pub port: u16,
    /// Whether peers can be listened for on the port.
    pub listening: bool,
    /// Who passes searches on to this client.
    pub parent: Option<String>,
    pub shared_files: usize,
    pub shared_folders: usize,
    pub uploading: Vec<Upload>,
    pub queued_uploads: usize,
    /// Bytes sent to others since starting.
    pub uploaded: u64,
}

type Tx = mpsc::UnboundedSender<Vec<u8>>;
/// Those waiting for a peer's address, by the peer.
type AddressWaits = HashMap<String, Vec<oneshot::Sender<(Ipv4Addr, u16)>>>;
/// Answers awaited to transfer requests of ours, by peer and token: allowed, or why not.
type AnswerWaits = HashMap<(String, u32), oneshot::Sender<(bool, String)>>;

struct Going {
    user: String,
    name: String,
    size: u64,
    sent: Arc<AtomicU64>,
}

#[derive(Default)]
struct Uploads {
    waiting: VecDeque<(String, String)>,
    going: Vec<Going>,
    sent: u64,
    /// Bytes a second, from the last upload that finished.
    speed: u32,
}

struct Inner {
    cfg: Config,
    stop: watch::Sender<bool>,
    token: AtomicU32,
    online: AtomicBool,
    problem: Mutex<String>,
    listen_problem: Mutex<String>,
    listening: AtomicBool,
    server: Mutex<Option<Tx>>,
    shares: Mutex<Arc<Shares>>,
    excluded: Mutex<Arc<Vec<String>>>,
    peers: Mutex<HashMap<String, Tx>>,
    dialing: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    addresses: Mutex<AddressWaits>,
    pierced: Mutex<HashMap<u32, oneshot::Sender<TcpStream>>>,
    searches: Mutex<HashMap<u32, mpsc::UnboundedSender<SearchReply>>>,
    jobs: Mutex<Jobs>,
    answers: Mutex<AnswerWaits>,
    uploads: Mutex<Uploads>,
    parent: Mutex<Option<String>>,
    seeking_parent: AtomicBool,
    answering: Arc<Semaphore>,
    /// Connections to peers, each holding one while it is open.
    conns: Arc<Semaphore>,
    /// Lists of everything shared being made for those who browse, a few at a time.
    browsing: Arc<Semaphore>,
    /// Signed out because the account signed in somewhere else; not signed in again until asked.
    relogged: AtomicBool,
    kick: Notify,
}

/// A Soulseek client. Cloned, it is the same client; it runs until stopped.
#[derive(Clone)]
pub struct Client(Arc<Inner>);

impl Client {
    pub fn start(cfg: Config) -> Client {
        let (stop, _) = watch::channel(false);
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1);
        let jobs = Jobs::load(&cfg.state);
        let inner = Arc::new(Inner {
            cfg,
            stop,
            token: AtomicU32::new(seed | 1),
            online: AtomicBool::new(false),
            problem: Mutex::new(String::new()),
            listen_problem: Mutex::new(String::new()),
            listening: AtomicBool::new(false),
            server: Mutex::new(None),
            shares: Mutex::new(Arc::new(Shares::default())),
            excluded: Mutex::new(Arc::new(vec![])),
            peers: Mutex::new(HashMap::new()),
            dialing: Mutex::new(HashMap::new()),
            addresses: Mutex::new(HashMap::new()),
            pierced: Mutex::new(HashMap::new()),
            searches: Mutex::new(HashMap::new()),
            jobs: Mutex::new(jobs),
            answers: Mutex::new(HashMap::new()),
            uploads: Mutex::new(Uploads::default()),
            parent: Mutex::new(None),
            seeking_parent: AtomicBool::new(false),
            answering: Arc::new(Semaphore::new(8)),
            conns: Arc::new(Semaphore::new(CONNECTIONS)),
            browsing: Arc::new(Semaphore::new(2)),
            relogged: AtomicBool::new(false),
            kick: Notify::new(),
        });
        tokio::spawn(inner.clone().server_loop());
        tokio::spawn(inner.clone().listen_loop());
        tokio::spawn(inner.clone().drive_loop());
        Client(inner)
    }

    /// Stop: sign out, stop listening, and keep the downloads as they are for next time.
    pub fn stop(&self) {
        let _ = self.0.stop.send(true);
        let path = self.0.cfg.state.clone();
        self.0.jobs.lock().save(&path);
    }

    pub fn online(&self) -> bool {
        self.0.online.load(Ordering::Relaxed)
    }

    /// Signed out because the account was signed in somewhere else. A new client signs in again.
    pub fn relogged(&self) -> bool {
        self.0.relogged.load(Ordering::Relaxed)
    }

    pub fn status(&self) -> Status {
        let i = &self.0;
        let shares = i.shares.lock().clone();
        let u = i.uploads.lock();
        let problem = i.problem.lock().clone();
        Status {
            online: i.online.load(Ordering::Relaxed),
            problem: if problem.is_empty() { i.listen_problem.lock().clone() } else { problem },
            port: i.cfg.port,
            listening: i.listening.load(Ordering::Relaxed),
            parent: i.parent.lock().clone(),
            shared_files: shares.files(),
            shared_folders: shares.folders(),
            uploading: u.going.iter().map(|g| Upload { user: g.user.clone(), name: g.name.clone(), size: g.size, sent: g.sent.load(Ordering::Relaxed) }).collect(),
            queued_uploads: u.waiting.len(),
            uploaded: u.sent,
        }
    }

    /// Share these files from now on, in place of those shared before.
    pub fn share(&self, shares: Shares) {
        let m = Out::new().u32(shares.folders() as u32).u32(shares.files() as u32).msg(server::SHARED_FOLDERS_FILES);
        *self.0.shares.lock() = Arc::new(shares);
        self.0.to_server(m);
    }

    /// Search the network, and gather what peers send back for as long as `wait`.
    pub async fn search(&self, query: &str, wait: Duration) -> Result<Vec<SearchReply>> {
        if !self.online() {
            bail!("not signed in to Soulseek");
        }
        let i = &self.0;
        let token = i.next_token();
        let (tx, mut rx) = mpsc::unbounded_channel();
        i.searches.lock().insert(token, tx);
        i.to_server(Out::new().u32(token).str(query).msg(server::FILE_SEARCH));
        let until = tokio::time::Instant::now() + wait;
        let mut out = vec![];
        while let Ok(Some(r)) = tokio::time::timeout_at(until, rx.recv()).await {
            if out.len() < 5000 {
                out.push(r);
            }
        }
        i.searches.lock().remove(&token);
        Ok(out)
    }

    /// Ask a person for these files, to be put in `dest`. Gives the download's number.
    pub fn download(&self, user: &str, dest: &Path, files: Vec<Fetch>) -> u64 {
        let id = self.0.jobs.lock().add(user, dest, files);
        self.0.kick.notify_one();
        id
    }

    pub fn job(&self, id: u64) -> Option<Job> {
        self.0.jobs.lock().list.iter().find(|j| j.id == id).cloned()
    }

    pub fn jobs(&self) -> Vec<Job> {
        self.0.jobs.lock().list.clone()
    }

    /// Stop a download and forget it. What it received is left for the caller to clear away.
    pub fn remove(&self, id: u64) {
        let mut j = self.0.jobs.lock();
        j.list.retain(|j| j.id != id);
        j.dirty = true;
    }
}

/// Plain words for why the server would not sign in.
fn refusal(reason: &str, detail: &str) -> (String, bool) {
    match reason {
        "INVALIDPASS" => ("The password is wrong for that username.".into(), false),
        "INVALIDUSERNAME" => (format!("That username cannot be used{}.", if detail.is_empty() { String::new() } else { format!(": {}", detail.trim_end_matches('.')) }), false),
        "EMPTYPASSWORD" => ("A password is needed.".into(), false),
        "INVALIDVERSION" => ("The server no longer takes this version of Spool.".into(), false),
        "SVRFULL" => ("The server is full; trying again later.".into(), true),
        "SVRPRIVATE" => ("The server takes no new accounts.".into(), false),
        other => (format!("The server would not sign in: {other}"), true),
    }
}

enum Ended {
    Relogged,
    Refused(String, bool),
    Lost(String),
}

impl Inner {
    fn next_token(&self) -> u32 {
        self.token.fetch_add(1, Ordering::Relaxed)
    }

    fn stopped(&self) -> bool {
        *self.stop.borrow()
    }

    fn to_server(&self, m: Vec<u8>) -> bool {
        self.server.lock().as_ref().is_some_and(|t| t.send(m).is_ok())
    }

    // ---- The server.

    async fn server_loop(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        let mut backoff = 5;
        loop {
            if self.stopped() {
                return;
            }
            let ended = tokio::select! {
                e = self.clone().session() => e,
                _ = stop.changed() => return,
            };
            *self.server.lock() = None;
            self.online.store(false, Ordering::Relaxed);
            *self.parent.lock() = None;
            let pause = match ended {
                Ended::Relogged => {
                    // Signing in again would sign the other one out, and so on back and forth.
                    tracing::warn!("signed out of Soulseek: the account signed in somewhere else");
                    *self.problem.lock() = "The account signed in somewhere else, so Spool signed out. Save the Soulseek settings to sign in here again.".into();
                    self.relogged.store(true, Ordering::Relaxed);
                    let _ = stop.changed().await;
                    return;
                }
                Ended::Refused(why, again) => {
                    tracing::warn!(why = %why, "Soulseek would not sign in");
                    *self.problem.lock() = why;
                    if !again {
                        // Not tried again until the account is changed, which starts a new client.
                        let _ = stop.changed().await;
                        return;
                    }
                    600
                }
                Ended::Lost(why) => {
                    tracing::info!(why = %why, "lost the Soulseek server");
                    *self.problem.lock() = format!("Lost the server: {why}. Trying again.");
                    backoff = (backoff * 2).min(300);
                    backoff
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(pause)) => {}
                _ = stop.changed() => return,
            }
        }
    }

    async fn session(self: Arc<Self>) -> Ended {
        let s = match timeout(Duration::from_secs(20), TcpStream::connect(&self.cfg.server)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Ended::Lost(e.to_string()),
            Err(_) => return Ended::Lost("it did not answer".into()),
        };
        let _ = s.set_nodelay(true);
        let (mut r, mut w) = s.into_split();
        use md5::Digest;
        let sum: String = md5::Md5::digest(format!("{}{}", self.cfg.username, self.cfg.password).as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        let login = Out::new().str(&self.cfg.username).str(&self.cfg.password).u32(msg::MAJOR_VERSION).str(&sum).u32(msg::MINOR_VERSION).msg(server::LOGIN);
        if let Err(e) = w.write_all(&login).await {
            return Ended::Lost(e.to_string());
        }
        let first = match timeout(Duration::from_secs(30), read_frame(&mut r, MESSAGE_MOST)).await {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => return Ended::Lost(e.to_string()),
            Err(_) => return Ended::Lost("no answer to signing in".into()),
        };
        let Ok((server::LOGIN, body)) = code32(&first) else { return Ended::Lost("the server answered something else".into()) };
        let mut i = In::new(body);
        if !i.bool().unwrap_or(false) {
            let reason = i.str().unwrap_or_default();
            let detail = i.str().unwrap_or_default();
            let (why, again) = refusal(&reason, &detail);
            return Ended::Refused(why, again);
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let writer = tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if w.write_all(&m).await.is_err() {
                    break;
                }
            }
        });
        *self.server.lock() = Some(tx.clone());
        let shares = self.shares.lock().clone();
        let me = &self.cfg.username;
        for m in [
            Out::new().u32(self.cfg.port as u32).msg(server::SET_WAIT_PORT),
            Out::new().i32(2).msg(server::SET_STATUS),
            Out::new().u32(shares.folders() as u32).u32(shares.files() as u32).msg(server::SHARED_FOLDERS_FILES),
            Out::new().bool(true).msg(server::HAVE_NO_PARENT),
            Out::new().str(me).msg(server::BRANCH_ROOT),
            Out::new().u32(0).msg(server::BRANCH_LEVEL),
            Out::new().bool(false).msg(server::ACCEPT_CHILDREN),
        ] {
            let _ = tx.send(m);
        }
        self.online.store(true, Ordering::Relaxed);
        self.problem.lock().clear();
        tracing::info!(user = %me, "signed in to Soulseek");
        self.kick.notify_one();
        // Messages are read whole by a task of their own: a read cut short to send a ping
        // would leave the rest of a message to be taken for the next.
        let (ftx, mut frx) = mpsc::channel::<std::io::Result<Vec<u8>>>(64);
        let reader = tokio::spawn(async move {
            loop {
                let f = read_frame(&mut r, MESSAGE_MOST).await;
                let end = f.is_err();
                if ftx.send(f).await.is_err() || end {
                    break;
                }
            }
        });
        let mut ping = tokio::time::interval(Duration::from_secs(300));
        ping.tick().await;
        let ended = loop {
            tokio::select! {
                f = frx.recv() => match f {
                    Some(Ok(f)) => {
                        if let Ok((code, body)) = code32(&f) {
                            if code == server::RELOGGED {
                                break Ended::Relogged;
                            }
                            self.on_server(code, body);
                        }
                    }
                    Some(Err(e)) => break Ended::Lost(e.to_string()),
                    None => break Ended::Lost("the connection closed".into()),
                },
                _ = ping.tick() => {
                    let _ = tx.send(Out::new().msg(server::PING));
                }
            }
        };
        reader.abort();
        writer.abort();
        ended
    }

    fn on_server(self: &Arc<Self>, code: u32, body: &[u8]) {
        let mut i = In::new(body);
        let r: std::io::Result<()> = (|| {
            match code {
                server::GET_PEER_ADDRESS => {
                    let (user, ip, port) = (i.str()?, msg::ip(i.u32()?), i.u32()? as u16);
                    for w in self.addresses.lock().remove(&user).unwrap_or_default() {
                        let _ = w.send((ip, port));
                    }
                }
                server::CONNECT_TO_PEER => {
                    let (user, kind, ip, port, token) = (i.str()?, i.str()?, msg::ip(i.u32()?), i.u32()? as u16, i.u32()?);
                    tokio::spawn(self.clone().pierce(user, kind, SocketAddr::from((ip, port)), token));
                }
                server::FILE_SEARCH => {
                    let (user, token, query) = (i.str()?, i.u32()?, i.str()?);
                    self.answer(user, token, query);
                }
                server::POSSIBLE_PARENTS => {
                    let n = i.count(12)?;
                    let mut list = vec![];
                    for _ in 0..n {
                        list.push((i.str()?, msg::ip(i.u32()?), i.u32()? as u16));
                    }
                    self.seek_parent(list);
                }
                server::EMBEDDED_MESSAGE => {
                    if i.u8()? == distrib::SEARCH {
                        let s = msg::read_distrib_search(i.rest())?;
                        self.answer(s.user, s.token, s.query);
                    }
                }
                server::RESET_DISTRIBUTED => {
                    *self.parent.lock() = None;
                    self.no_parent();
                }
                server::EXCLUDED_SEARCH_PHRASES => {
                    let n = i.count(4)?;
                    let mut list = vec![];
                    for _ in 0..n {
                        list.push(i.str()?.to_lowercase());
                    }
                    *self.excluded.lock() = Arc::new(list);
                }
                server::CANT_CONNECT_TO_PEER => {
                    // The waiter learns at once that no connection is coming.
                    self.pierced.lock().remove(&i.u32()?);
                }
                _ => {}
            }
            Ok(())
        })();
        if let Err(e) = r {
            tracing::debug!(code, error = %e, "a message from the Soulseek server could not be read");
        }
    }

    // ---- Reaching peers.

    async fn address(&self, user: &str) -> Option<(Ipv4Addr, u16)> {
        let (tx, rx) = oneshot::channel();
        self.addresses.lock().entry(user.to_string()).or_default().push(tx);
        self.to_server(Out::new().str(user).msg(server::GET_PEER_ADDRESS));
        let got = timeout(Duration::from_secs(15), rx).await.ok().and_then(|r| r.ok());
        if got.is_none() {
            let mut a = self.addresses.lock();
            if let Some(w) = a.get_mut(user) {
                w.retain(|s| !s.is_closed());
                if w.is_empty() {
                    a.remove(user);
                }
            }
        }
        got
    }

    /// A connection of this kind ("P", "F") to a peer, opened from either end.
    async fn connect(self: &Arc<Self>, user: &str, kind: &str) -> Result<TcpStream> {
        let token = self.next_token();
        let (ptx, prx) = oneshot::channel();
        self.pierced.lock().insert(token, ptx);
        self.to_server(Out::new().u32(token).str(user).str(kind).msg(server::CONNECT_TO_PEER));
        let me = self.clone();
        let direct = async move {
            let (ip, port) = me.address(user).await.ok_or_else(|| anyhow!("the server gave no address for {user}"))?;
            if port == 0 {
                bail!("{user} is not online");
            }
            let mut s = timeout(Duration::from_secs(10), TcpStream::connect((ip, port))).await.map_err(|_| anyhow!("{user} did not answer"))??;
            s.write_all(&Out::new().str(&me.cfg.username).str(kind).u32(0).msg8(init::PEER_INIT)).await?;
            Ok::<_, anyhow::Error>(s)
        };
        let indirect = timeout(Duration::from_secs(30), prx);
        tokio::pin!(direct, indirect);
        let (mut direct_done, mut indirect_done) = (false, false);
        let mut why = String::new();
        let got = loop {
            tokio::select! {
                r = &mut direct, if !direct_done => match r {
                    Ok(s) => break Ok(s),
                    Err(e) => {
                        direct_done = true;
                        why = e.to_string();
                        if why.ends_with("is not online") {
                            break Err(anyhow!(why.clone()));
                        }
                    }
                },
                r = &mut indirect, if !indirect_done => match r {
                    Ok(Ok(s)) => break Ok(s),
                    _ => indirect_done = true,
                },
            }
            if direct_done && indirect_done {
                break Err(anyhow!("could not reach {user}: {why}"));
            }
        };
        self.pierced.lock().remove(&token);
        let s = got?;
        let _ = s.set_nodelay(true);
        Ok(s)
    }

    /// The connection to talk to a peer over, opened if there is none.
    async fn peer(self: &Arc<Self>, user: &str) -> Result<Tx> {
        let open = |i: &Inner| i.peers.lock().get(user).filter(|t| !t.is_closed()).cloned();
        if let Some(t) = open(self) {
            return Ok(t);
        }
        let lock = self.dialing.lock().entry(user.to_string()).or_default().clone();
        let got = async {
            let _held = lock.lock().await;
            if let Some(t) = open(self) {
                return Ok(t);
            }
            let s = self.connect(user, "P").await?;
            self.adopt_peer(user.to_string(), s).ok_or_else(|| anyhow!("too many connections are open"))
        }
        .await;
        let mut d = self.dialing.lock();
        // The map's and this one: nobody else is waiting to dial them.
        if Arc::strong_count(&lock) <= 2 {
            d.remove(user);
        }
        got
    }

    async fn send_peer(self: &Arc<Self>, user: &str, m: Vec<u8>) -> Result<()> {
        self.peer(user).await?.send(m).map_err(|_| anyhow!("the connection to {user} closed"))
    }

    fn reply(self: &Arc<Self>, user: &str, m: Vec<u8>) {
        let me = self.clone();
        let user = user.to_string();
        tokio::spawn(async move {
            let _ = me.send_peer(&user, m).await;
        });
    }

    /// Talk to a peer over this connection from now on. None when too many are open.
    fn adopt_peer(self: &Arc<Self>, user: String, s: TcpStream) -> Option<Tx> {
        let permit = self.conns.clone().try_acquire_owned().ok()?;
        let (mut r, mut w) = s.into_split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        self.peers.lock().insert(user.clone(), tx.clone());
        let writer = tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if !matches!(timeout(WRITE_WAIT, w.write_all(&m)).await, Ok(Ok(()))) {
                    break;
                }
            }
        });
        let me = self.clone();
        let mine = tx.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut stop = me.stop.subscribe();
            loop {
                let f = tokio::select! {
                    f = timeout(PEER_IDLE, read_frame(&mut r, PEER_MESSAGE_MOST)) => f,
                    _ = stop.changed() => break,
                };
                match f {
                    Ok(Ok(f)) => me.on_peer(&user, &f),
                    _ => break,
                }
            }
            {
                let mut peers = me.peers.lock();
                if peers.get(&user).is_some_and(|t| t.same_channel(&mine)) {
                    peers.remove(&user);
                }
            }
            // The connection closes with its reading: nothing is left waiting to be written.
            writer.abort();
        });
        Some(tx)
    }

    /// The server asks for a connection to a peer who could not reach this one.
    async fn pierce(self: Arc<Self>, user: String, kind: String, at: SocketAddr, token: u32) {
        let s = match timeout(Duration::from_secs(10), TcpStream::connect(at)).await {
            Ok(Ok(mut s)) => match s.write_all(&Out::new().u32(token).msg8(init::PIERCE_FIREWALL)).await {
                Ok(()) => Some(s),
                Err(_) => None,
            },
            _ => None,
        };
        let Some(s) = s else {
            self.to_server(Out::new().u32(token).str(&user).msg(server::CANT_CONNECT_TO_PEER));
            return;
        };
        let _ = s.set_nodelay(true);
        match kind.as_str() {
            "P" => {
                self.adopt_peer(user, s);
            }
            "F" => {
                if let Ok(_permit) = self.conns.clone().try_acquire_owned() {
                    self.receive_file(user, s).await;
                }
            }
            // No children are taken in the search network.
            _ => {}
        }
    }

    async fn listen_loop(self: Arc<Self>) {
        let l = match TcpListener::bind(("0.0.0.0", self.cfg.port)).await {
            Ok(l) => l,
            Err(e) => {
                *self.listen_problem.lock() = format!("Could not listen on port {}: {e}. Others can still be reached, but they cannot reach Spool.", self.cfg.port);
                return;
            }
        };
        self.listening.store(true, Ordering::Relaxed);
        let mut stop = self.stop.subscribe();
        loop {
            tokio::select! {
                a = l.accept() => if let Ok((s, _)) = a {
                    // Turned away when too many are open, before a byte is read.
                    if let Ok(permit) = self.conns.clone().try_acquire_owned() {
                        tokio::spawn(self.clone().incoming(s, permit));
                    }
                },
                _ = stop.changed() => break,
            }
        }
        self.listening.store(false, Ordering::Relaxed);
    }

    async fn incoming(self: Arc<Self>, mut s: TcpStream, permit: tokio::sync::OwnedSemaphorePermit) {
        let Ok(Ok(f)) = timeout(Duration::from_secs(30), read_frame(&mut s, 4096)).await else { return };
        let Ok((code, body)) = code8(&f) else { return };
        let mut i = In::new(body);
        match code {
            init::PIERCE_FIREWALL => {
                if let Ok(token) = i.u32() {
                    if let Some(w) = self.pierced.lock().remove(&token) {
                        let _ = w.send(s);
                    }
                }
            }
            init::PEER_INIT => {
                let (Ok(user), Ok(kind)) = (i.str(), i.str()) else { return };
                let _ = s.set_nodelay(true);
                match kind.as_str() {
                    "P" => {
                        // It holds a place of its own from here.
                        drop(permit);
                        self.adopt_peer(user, s);
                    }
                    "F" => self.receive_file(user, s).await,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // ---- What peers say.

    fn on_peer(self: &Arc<Self>, user: &str, frame: &[u8]) {
        let Ok((code, body)) = code32(frame) else { return };
        let mut i = In::new(body);
        let r: std::io::Result<()> = (|| {
            match code {
                peer::FILE_SEARCH_RESPONSE => {
                    let r = SearchReply::read(body)?;
                    if let Some(t) = self.searches.lock().get(&r.token) {
                        let _ = t.send(r);
                    }
                }
                peer::TRANSFER_REQUEST => {
                    let (direction, token, name) = (i.u32()?, i.u32()?, i.str()?);
                    if direction == 1 {
                        let size = i.u64()?;
                        let taken = self.jobs.lock().offered(user, &name, token, size);
                        let answer = if taken { Out::new().u32(token).bool(true) } else { Out::new().u32(token).bool(false).str("Cancelled") };
                        self.reply(user, answer.msg(peer::TRANSFER_RESPONSE));
                    } else {
                        // The old way of asking for a file: it is put in line like any other.
                        self.reply(user, Out::new().u32(token).bool(false).str("Queued").msg(peer::TRANSFER_RESPONSE));
                        self.queue_upload(user, name);
                    }
                }
                peer::TRANSFER_RESPONSE => {
                    let (token, allowed, reason) = msg::read_transfer_response(body)?;
                    if let Some(w) = self.answers.lock().remove(&(user.to_string(), token)) {
                        let _ = w.send((allowed, reason));
                    }
                }
                peer::QUEUE_UPLOAD => self.queue_upload(user, i.str()?),
                peer::PLACE_IN_QUEUE_REQUEST => {
                    let name = i.str()?;
                    let place = self.uploads.lock().waiting.iter().position(|(u, n)| u == user && *n == name);
                    if let Some(p) = place {
                        self.reply(user, Out::new().str(&name).u32(p as u32 + 1).msg(peer::PLACE_IN_QUEUE_RESPONSE));
                    }
                }
                peer::PLACE_IN_QUEUE_RESPONSE => {
                    let (name, place) = (i.str()?, i.u32()?);
                    if let Some((job, n)) = self.jobs.lock().file_mut(user, &name) {
                        job.files[n].place = Some(place);
                    }
                }
                peer::UPLOAD_DENIED => {
                    let (name, why) = (i.str()?, i.str()?);
                    tracing::info!(user, file = %name, why = %why, "a Soulseek download was refused");
                    self.jobs.lock().denied(user, &name, &why);
                }
                peer::UPLOAD_FAILED => self.jobs.lock().broke(user, &i.str()?),
                peer::SHARED_FILE_LIST_REQUEST => {
                    let Ok(permit) = self.browsing.clone().try_acquire_owned() else { return Ok(()) };
                    let shares = self.shares.lock().clone();
                    let me = self.clone();
                    let user = user.to_string();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Ok(m) = tokio::task::spawn_blocking(move || shares.list()).await {
                            let _ = me.send_peer(&user, m).await;
                        }
                    });
                }
                peer::FOLDER_CONTENTS_REQUEST => {
                    let (token, folder) = (i.u32()?, i.str()?);
                    let m = self.shares.lock().folder(token, &folder);
                    self.reply(user, m);
                }
                peer::USER_INFO_REQUEST => {
                    let u = self.uploads.lock();
                    let free = u.going.len() < self.cfg.upload_slots;
                    let m = Out::new().str("Spool").bool(false).u32(self.cfg.upload_slots as u32).u32(u.waiting.len() as u32).bool(free).u32(0).msg(peer::USER_INFO_RESPONSE);
                    drop(u);
                    self.reply(user, m);
                }
                _ => {}
            }
            Ok(())
        })();
        if let Err(e) = r {
            tracing::debug!(user, code, error = %e, "a message from a Soulseek peer could not be read");
        }
    }

    // ---- Downloads.

    /// A peer is sending a file of a download.
    async fn receive_file(self: Arc<Self>, user: String, mut s: TcpStream) {
        let Ok(Ok(token)) = timeout(Duration::from_secs(60), s.read_u32_le()).await else { return };
        let found = {
            let mut j = self.jobs.lock();
            j.list.iter_mut().filter(|job| job.user == user).find_map(|job| {
                let n = job.files.iter().position(|f| f.token == Some(token) && f.state == FileState::Coming)?;
                job.files[n].connected = true;
                let (to, part) = job.paths(&job.files[n]);
                Some((job.id, n, job.files[n].name.clone(), job.files[n].size, to, part))
            })
        };
        let Some((id, n, name, size, to, part)) = found else { return };
        match self.receive(id, n, token, size, &to, &part, &mut s).await {
            Ok(true) => {
                let mut j = self.jobs.lock();
                if let Some(f) = j.sending(id, n, token) {
                    f.state = FileState::Done;
                    f.got = size;
                    f.token = None;
                }
                j.dirty = true;
                tracing::debug!(user = %user, file = %name, "received a file from Soulseek");
            }
            // The download was removed, or the file asked for again, meanwhile.
            Ok(false) => {
                let _ = tokio::fs::remove_file(&part).await;
                if let Some(dir) = part.parent() {
                    let _ = tokio::fs::remove_dir(dir).await;
                }
            }
            Err(e) => {
                tracing::debug!(user = %user, file = %name, error = %e, "a file from Soulseek broke off");
                let mut j = self.jobs.lock();
                if j.sending(id, n, token).is_some() {
                    j.broke(&user, &name);
                }
            }
        }
        self.kick.notify_one();
    }

    /// Receive a file into its part, and put it in place when it is whole. False when it is
    /// no longer wanted.
    #[allow(clippy::too_many_arguments)]
    async fn receive(&self, id: u64, n: usize, token: u32, size: u64, to: &Path, part: &Path, s: &mut TcpStream) -> Result<bool> {
        let mut stop = self.stop.subscribe();
        if let Some(dir) = part.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let mut offset = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
        if offset > size {
            let _ = tokio::fs::remove_file(part).await;
            offset = 0;
        }
        let mut file = tokio::fs::OpenOptions::new().create(true).append(true).open(part).await?;
        s.write_u64_le(offset).await?;
        let mut got = offset;
        let mut noted = got;
        let mut buf = vec![0u8; 256 << 10];
        while got < size {
            let n_read = tokio::select! {
                r = timeout(Duration::from_secs(180), s.read(&mut buf)) => r.map_err(|_| anyhow!("nothing came for three minutes"))??,
                _ = stop.changed() => bail!("Spool is stopping Soulseek"),
            };
            if n_read == 0 {
                break;
            }
            let n_read = n_read.min((size - got) as usize);
            file.write_all(&buf[..n_read]).await?;
            got += n_read as u64;
            if got - noted >= 4 << 20 || got == size {
                noted = got;
                let mut j = self.jobs.lock();
                let Some(f) = j.sending(id, n, token) else { return Ok(false) };
                f.got = got;
                j.dirty = true;
            }
        }
        file.flush().await?;
        drop(file);
        if got < size {
            bail!("the connection closed with {got} of {size} bytes");
        }
        // Still wanted, at the last moment.
        if self.jobs.lock().sending(id, n, token).is_none() {
            return Ok(false);
        }
        tokio::fs::rename(part, to).await?;
        Ok(true)
    }

    /// Ask for what is still to come, and for places in queues, now and then.
    fn ask(self: &Arc<Self>) {
        let t = now();
        let mut asks: HashMap<String, Vec<(String, bool)>> = HashMap::new();
        {
            let mut j = self.jobs.lock();
            for job in &mut j.list {
                for f in &mut job.files {
                    match f.state {
                        FileState::Waiting if t - f.asked_at >= ASK_AGAIN => {
                            f.asked_at = t;
                            f.place_asked_at = t;
                            asks.entry(job.user.clone()).or_default().push((f.name.clone(), true));
                        }
                        FileState::Waiting if t - f.place_asked_at >= 300 => {
                            f.place_asked_at = t;
                            asks.entry(job.user.clone()).or_default().push((f.name.clone(), false));
                        }
                        // Offered, but its owner never connected to send it.
                        FileState::Coming if !f.connected && t - f.coming_at > 300 => {
                            f.state = FileState::Waiting;
                            f.token = None;
                            f.asked_at = 0;
                        }
                        _ => {}
                    }
                }
            }
        }
        for (user, names) in asks {
            let me = self.clone();
            tokio::spawn(async move {
                let tx = match me.peer(&user).await {
                    Ok(tx) => tx,
                    Err(e) => {
                        let mut j = me.jobs.lock();
                        for job in j.list.iter_mut().filter(|j| j.user == user) {
                            job.note = format!("Could not reach {user}: {e}");
                            for f in job.files.iter_mut().filter(|f| f.state == FileState::Waiting) {
                                // Tried again in two minutes rather than ten.
                                f.asked_at = f.asked_at.min(now() - ASK_AGAIN + 120);
                            }
                        }
                        return;
                    }
                };
                for (name, queue) in names {
                    let m = if queue { Out::new().str(&name).msg(peer::QUEUE_UPLOAD) } else { Out::new().str(&name).msg(peer::PLACE_IN_QUEUE_REQUEST) };
                    let _ = tx.send(m);
                }
                for job in me.jobs.lock().list.iter_mut().filter(|j| j.user == user) {
                    job.note.clear();
                }
            });
        }
    }

    // ---- Uploads.

    fn queue_upload(self: &Arc<Self>, user: &str, name: String) {
        let shared = self.shares.lock().get(&name).is_some();
        if !shared {
            self.reply(user, Out::new().str(&name).str("File not shared.").msg(peer::UPLOAD_DENIED));
            return;
        }
        let mut u = self.uploads.lock();
        if u.waiting.iter().any(|(w, n)| w == user && *n == name) || u.going.iter().any(|g| g.user == user && g.name == name) {
            return;
        }
        if u.waiting.iter().filter(|(w, _)| w == user).count() >= QUEUE_PER_USER {
            drop(u);
            self.reply(user, Out::new().str(&name).str("Too many files").msg(peer::UPLOAD_DENIED));
            return;
        }
        u.waiting.push_back((user.to_string(), name));
        drop(u);
        self.kick.notify_one();
    }

    /// Start the uploads there is room for: the longest waiting first, one at a time to each person.
    fn start_uploads(self: &Arc<Self>) {
        loop {
            let next = {
                let mut u = self.uploads.lock();
                if u.going.len() >= self.cfg.upload_slots {
                    return;
                }
                let Some(p) = u.waiting.iter().position(|(w, _)| !u.going.iter().any(|g| g.user == *w)) else { return };
                let (user, name) = u.waiting.remove(p).expect("a place in the queue");
                let Some(f) = self.shares.lock().get(&name).cloned() else { continue };
                let sent = Arc::new(AtomicU64::new(0));
                u.going.push(Going { user: user.clone(), name: name.clone(), size: f.size, sent: sent.clone() });
                (user, name, f.real, f.size, sent)
            };
            let me = self.clone();
            tokio::spawn(async move {
                let (user, name, real, size, sent) = next;
                let r = me.upload(&user, &name, &real, size, &sent).await;
                me.uploads.lock().going.retain(|g| !(g.user == user && g.name == name));
                if let Err(e) = r {
                    tracing::debug!(user = %user, file = %name, error = %e, "an upload did not finish");
                    let _ = me.send_peer(&user, Out::new().str(&name).msg(peer::UPLOAD_FAILED)).await;
                }
                me.kick.notify_one();
            });
        }
    }

    async fn upload(self: &Arc<Self>, user: &str, name: &str, real: &Path, size: u64, sent: &AtomicU64) -> Result<()> {
        let tx = self.peer(user).await?;
        let token = self.next_token();
        let (atx, arx) = oneshot::channel();
        self.answers.lock().insert((user.to_string(), token), atx);
        let _ = tx.send(Out::new().u32(1).u32(token).str(name).u64(size).msg(peer::TRANSFER_REQUEST));
        let answer = timeout(Duration::from_secs(60), arx).await;
        self.answers.lock().remove(&(user.to_string(), token));
        let (allowed, why) = answer.map_err(|_| anyhow!("{user} did not answer"))?.map_err(|_| anyhow!("no answer"))?;
        if !allowed {
            // They no longer want it: nothing failed.
            tracing::debug!(user, file = name, why = %why, "an upload was turned down");
            return Ok(());
        }
        let mut s = self.connect(user, "F").await?;
        s.write_u32_le(token).await?;
        let offset = timeout(Duration::from_secs(60), s.read_u64_le()).await.map_err(|_| anyhow!("{user} did not say where to start"))??;
        if offset > size {
            bail!("{user} asked to start past the end");
        }
        let mut file = tokio::fs::File::open(real).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut stop = self.stop.subscribe();
        let started = std::time::Instant::now();
        let mut left = size - offset;
        let mut buf = vec![0u8; 256 << 10];
        while left > 0 {
            let n = file.read(&mut buf[..(left.min(256 << 10)) as usize]).await?;
            if n == 0 {
                bail!("the file is shorter than it was");
            }
            tokio::select! {
                r = timeout(Duration::from_secs(180), s.write_all(&buf[..n])) => r.map_err(|_| anyhow!("{user} stopped taking it"))??,
                _ = stop.changed() => bail!("Spool is stopping Soulseek"),
            }
            left -= n as u64;
            sent.fetch_add(n as u64, Ordering::Relaxed);
        }
        // The one receiving closes the connection once it has it all.
        let _ = timeout(Duration::from_secs(60), s.read(&mut [0u8; 1])).await;
        let secs = started.elapsed().as_secs_f64().max(0.001);
        let speed = ((size - offset) as f64 / secs) as u32;
        {
            let mut u = self.uploads.lock();
            u.sent += size - offset;
            u.speed = speed;
        }
        self.to_server(Out::new().u32(speed).msg(server::SEND_UPLOAD_SPEED));
        Ok(())
    }

    // ---- Searches that reach this client.

    fn answer(self: &Arc<Self>, user: String, token: u32, query: String) {
        if user == self.cfg.username || query.len() > 300 {
            return;
        }
        let shares = self.shares.lock().clone();
        if shares.files() == 0 {
            return;
        }
        let excluded = self.excluded.lock().clone();
        let files = shares.search(&query, &excluded, 100);
        if files.is_empty() {
            return;
        }
        // Answers wait on a connection each; when many are waiting, more are let go.
        let Ok(permit) = self.answering.clone().try_acquire_owned() else { return };
        let (slot_free, speed, queue) = {
            let u = self.uploads.lock();
            (u.going.len() < self.cfg.upload_slots, u.speed, u.waiting.len() as u32)
        };
        let m = SearchReply { user: self.cfg.username.clone(), token, files, slot_free, speed, queue }.write();
        let me = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = me.send_peer(&user, m).await;
        });
    }

    /// Try the parents the server suggests; the first to pass a search on is kept.
    fn seek_parent(self: &Arc<Self>, list: Vec<(String, Ipv4Addr, u16)>) {
        if self.parent.lock().is_some() || self.seeking_parent.swap(true, Ordering::Relaxed) {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let tries: Vec<_> = list.into_iter().take(10).map(|(user, ip, port)| tokio::spawn(me.clone().parent_conn(user, SocketAddr::from((ip, port))))).collect();
            // Let the next list be tried once these have had their chance.
            tokio::time::sleep(Duration::from_secs(60)).await;
            me.seeking_parent.store(false, Ordering::Relaxed);
            drop(tries);
        });
    }

    /// Tell the server there is no parent, and this client is the root of its own branch.
    fn no_parent(&self) {
        self.to_server(Out::new().bool(true).msg(server::HAVE_NO_PARENT));
        self.to_server(Out::new().str(&self.cfg.username).msg(server::BRANCH_ROOT));
        self.to_server(Out::new().u32(0).msg(server::BRANCH_LEVEL));
    }

    async fn parent_conn(self: Arc<Self>, user: String, at: SocketAddr) {
        let Ok(_permit) = self.conns.clone().try_acquire_owned() else { return };
        let Ok(Ok(mut s)) = timeout(Duration::from_secs(10), TcpStream::connect(at)).await else { return };
        if s.write_all(&Out::new().str(&self.cfg.username).str("D").u32(0).msg8(init::PEER_INIT)).await.is_err() {
            return;
        }
        let mut stop = self.stop.subscribe();
        let (mut level, mut root) = (None::<i32>, None::<String>);
        let mut adopted = false;
        // The branch as told to the server, to tell it again when it changes.
        let mut told: Option<(i32, String)> = None;
        loop {
            let wait = Duration::from_secs(if adopted { 600 } else { 60 });
            let f = tokio::select! {
                f = timeout(wait, read_frame(&mut s, 1 << 20)) => f,
                _ = stop.changed() => break,
            };
            let Ok(Ok(f)) = f else { break };
            let Ok((code, body)) = code8(&f) else { break };
            let mut i = In::new(body);
            let search = match code {
                distrib::BRANCH_LEVEL => {
                    level = i.i32().ok();
                    None
                }
                distrib::BRANCH_ROOT => {
                    root = i.str().ok();
                    None
                }
                distrib::SEARCH => msg::read_distrib_search(body).ok(),
                distrib::EMBEDDED => match i.u8() {
                    Ok(distrib::SEARCH) => msg::read_distrib_search(i.rest()).ok(),
                    _ => None,
                },
                _ => None,
            };
            // Where this client stands in the branch: below its parent, under the same root.
            // A parent at the root is the root, whether it says so or not.
            let branch = match (level, &root) {
                (Some(0), _) => Some((1, user.clone())),
                (Some(l), Some(r)) if l > 0 => Some((l + 1, r.clone())),
                _ => None,
            };
            if !adopted {
                // A parent is taken once it has said where it stands and passed on a search.
                let (Some(_), Some(_)) = (&search, &branch) else { continue };
                let mut p = self.parent.lock();
                if p.is_some() {
                    break;
                }
                *p = Some(user.clone());
                drop(p);
                adopted = true;
                tracing::debug!(parent = %user, "joined the Soulseek search network");
                self.to_server(Out::new().bool(false).msg(server::HAVE_NO_PARENT));
            }
            if self.parent.lock().as_deref() != Some(user.as_str()) {
                break;
            }
            if let Some(b) = branch.filter(|b| told.as_ref() != Some(b)) {
                self.to_server(Out::new().u32(b.0 as u32).msg(server::BRANCH_LEVEL));
                self.to_server(Out::new().str(&b.1).msg(server::BRANCH_ROOT));
                told = Some(b);
            }
            if let Some(search) = search {
                self.answer(search.user, search.token, search.query);
            }
        }
        if adopted {
            let mut p = self.parent.lock();
            if p.as_deref() == Some(user.as_str()) {
                *p = None;
                drop(p);
                self.no_parent();
            }
        }
    }

    // ---- Keeping things moving.

    async fn drive_loop(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        loop {
            if self.online.load(Ordering::Relaxed) {
                self.ask();
                self.start_uploads();
            } else {
                // Nothing can come while signed out, and that is nobody's doing.
                let mut j = self.jobs.lock();
                for job in j.list.iter_mut().filter(|j| !j.done()) {
                    job.progress_at = now();
                }
            }
            let dirty = self.jobs.lock().dirty;
            if dirty {
                let path = self.cfg.state.clone();
                self.jobs.lock().save(&path);
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                _ = self.kick.notified() => {}
                _ = stop.changed() => return,
            }
        }
    }
}
