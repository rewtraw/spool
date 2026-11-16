//! Two clients and a stand-in for the server on this machine: one shares a folder, the other
//! finds it by searching and downloads it, by way of a direct connection and of one the server
//! asks for.

use spool_slsk::msg::server;
use spool_slsk::wire::{code32, read_frame, In, Out};
use spool_slsk::{Client, Config, Fetch, Shared, Shares};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};

#[derive(Default)]
struct World {
    /// Each signed-in user's channel and listening port.
    users: HashMap<String, (mpsc::UnboundedSender<Vec<u8>>, u16)>,
}

/// Enough of the server: signing in, addresses, asking a peer to connect, and passing
/// searches to everyone else. `unreachable` names users whose address is given wrongly, so
/// that only a connection from their end can work.
async fn server(unreachable: &'static [&'static str]) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let world = Arc::new(Mutex::new(World::default()));
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let world = world.clone();
            tokio::spawn(async move {
                let (mut r, mut w) = s.into_split();
                let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
                tokio::spawn(async move {
                    while let Some(m) = rx.recv().await {
                        if w.write_all(&m).await.is_err() {
                            break;
                        }
                    }
                });
                let mut me = String::new();
                while let Ok(f) = read_frame(&mut r, 1 << 20).await {
                    let (code, body) = code32(&f).unwrap();
                    let mut i = In::new(body);
                    let ip = u32::from(std::net::Ipv4Addr::LOCALHOST);
                    match code {
                        server::LOGIN => {
                            me = i.str().unwrap();
                            let _ = tx.send(Out::new().bool(true).str("hi").u32(ip).str("").bool(false).msg(server::LOGIN));
                            world.lock().await.users.insert(me.clone(), (tx.clone(), 0));
                        }
                        server::SET_WAIT_PORT => {
                            let p = i.u32().unwrap() as u16;
                            world.lock().await.users.get_mut(&me).unwrap().1 = p;
                        }
                        server::GET_PEER_ADDRESS => {
                            let user = i.str().unwrap();
                            let port = world.lock().await.users.get(&user).map(|u| u.1).unwrap_or(0);
                            // A port nobody listens on, for those who cannot be reached.
                            let port = if unreachable.contains(&user.as_str()) { 1 } else { port };
                            let _ = tx.send(Out::new().str(&user).u32(ip).u32(port as u32).u32(0).u16(0).msg(server::GET_PEER_ADDRESS));
                        }
                        server::CONNECT_TO_PEER => {
                            let (token, user, kind) = (i.u32().unwrap(), i.str().unwrap(), i.str().unwrap());
                            let w = world.lock().await;
                            let my_port = w.users.get(&me).map(|u| u.1).unwrap_or(0);
                            if let Some((them, _)) = w.users.get(&user) {
                                let _ = them.send(Out::new().str(&me).str(&kind).u32(ip).u32(my_port as u32).u32(token).bool(false).u32(0).u32(0).msg(server::CONNECT_TO_PEER));
                            }
                        }
                        server::FILE_SEARCH => {
                            let (token, query) = (i.u32().unwrap(), i.str().unwrap());
                            for (user, (them, _)) in world.lock().await.users.iter() {
                                if *user != me {
                                    let _ = them.send(Out::new().str(&me).u32(token).str(&query).msg(server::FILE_SEARCH));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            });
        }
    });
    port
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn client(name: &str, server: u16, dir: &std::path::Path) -> Client {
    Client::start(Config { username: name.into(), password: "pw".into(), server: format!("127.0.0.1:{server}"), port: free_port(), state: dir.join(format!("{name}.json")), upload_slots: 2 })
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..300 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

trait U16 {
    fn u16(self, v: u16) -> Out;
}
impl U16 for Out {
    fn u16(mut self, v: u16) -> Out {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
}

async fn share_and_fetch(unreachable: &'static [&'static str]) {
    let dir = tempfile::tempdir().unwrap();
    let port = server(unreachable).await;
    let music = dir.path().join("music/Can - Tago Mago");
    std::fs::create_dir_all(&music).unwrap();
    let track: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(music.join("01 Paperhouse.flac"), &track).unwrap();
    std::fs::write(music.join("02 Mushroom.flac"), b"short").unwrap();

    let sharer = client("sharer", port, dir.path());
    let fetcher = client("fetcher", port, dir.path());
    until("both to sign in", || sharer.online() && fetcher.online()).await;
    sharer.share(Shares::new(vec![
        Shared { name: "Music\\Can - Tago Mago\\01 Paperhouse.flac".into(), real: music.join("01 Paperhouse.flac"), size: track.len() as u64, lossless: true, seconds: 450, sample_rate: 44100, bit_depth: 16, ..Default::default() },
        Shared { name: "Music\\Can - Tago Mago\\02 Mushroom.flac".into(), real: music.join("02 Mushroom.flac"), size: 5, lossless: true, ..Default::default() },
    ]));

    let found = fetcher.search("can tago mago", Duration::from_secs(3)).await.unwrap();
    let folders = spool_slsk::folders(&found);
    assert_eq!(folders.len(), 1, "{found:?}");
    let f = &folders[0];
    assert_eq!((f.user.as_str(), f.path.as_str(), f.files.len()), ("sharer", "Music\\Can - Tago Mago", 2));
    assert_eq!(f.files[0].0.sample_rate, Some(44100));
    assert!(fetcher.search("kraftwerk", Duration::from_millis(500)).await.unwrap().is_empty());

    let dest = dir.path().join("got");
    let id = fetcher.download("sharer", &dest, f.files.iter().map(|(e, rel)| Fetch { name: e.path.clone(), size: e.size, rel: rel.clone() }).collect());
    until("the download", || fetcher.job(id).is_some_and(|j| j.done() || j.failed().is_some())).await;
    let job = fetcher.job(id).unwrap();
    assert_eq!(job.failed(), None);
    assert_eq!(std::fs::read(dest.join("01 Paperhouse.flac")).unwrap(), track);
    assert_eq!(std::fs::read(dest.join("02 Mushroom.flac")).unwrap(), b"short");
    until("the uploads to be counted", || sharer.status().uploaded == track.len() as u64 + 5).await;

    // A file not shared is refused, and the download says so.
    let id = fetcher.download("sharer", &dir.path().join("no"), vec![Fetch { name: "Music\\secret.flac".into(), size: 1, rel: "secret.flac".into() }]);
    until("the refusal", || fetcher.job(id).is_some_and(|j| j.failed().is_some())).await;
    assert_eq!(fetcher.job(id).unwrap().failed().as_deref(), Some("secret.flac: File not shared"));
    sharer.stop();
    fetcher.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_folder_is_found_and_fetched() {
    share_and_fetch(&[]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_that_cannot_be_reached_connects_from_its_end() {
    share_and_fetch(&["sharer", "fetcher"]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_password_is_said_plainly() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let _ = read_frame(&mut s, 1 << 20).await;
        s.write_all(&Out::new().bool(false).str("INVALIDPASS").msg(server::LOGIN)).await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let dir = tempfile::tempdir().unwrap();
    let c = client("someone", port, dir.path());
    until("the refusal", || !c.status().problem.is_empty()).await;
    assert_eq!(c.status().problem, "The password is wrong for that username.");
    assert!(!c.online());
    c.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_in_elsewhere_it_steps_back_and_stays_out() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let logins = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = logins.clone();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let _ = read_frame(&mut s, 1 << 20).await;
            s.write_all(&Out::new().bool(true).str("hi").u32(0).str("").bool(false).msg(server::LOGIN)).await.unwrap();
            s.write_all(&Out::new().msg(server::RELOGGED)).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let c = client("someone", port, dir.path());
    until("being signed out", || c.relogged()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!c.online());
    assert_eq!(logins.load(std::sync::atomic::Ordering::Relaxed), 1, "it does not sign in again by itself");
    assert!(c.status().problem.contains("signed in somewhere else"));
    c.stop();
}
