//! Finding Spools on the home network.
//!
//! A helper says that it is there, every few seconds, to whatever on its network is
//! listening for Spools, and a Spool listens and lists what it hears so that an address
//! need not be typed. Hearing of one links nothing: anything on a network can say it is a
//! Spool, so what is heard is only ever an address to try, and the code or the key is
//! still what links the two. What is said carries no code, no key, no folder and nothing
//! of what is in the library.
//!
//! It is said to a group address of Spool's own and not through the system's own way of
//! finding printers and players, which on a NAS belongs to a service that shares it with
//! nobody.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Where Spools say they are: a group address kept to one network, and a port.
const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 77, 79);
const PORT: u16 = 7977;
/// How often one says so, and how long one is kept after it was last heard.
const EVERY: Duration = Duration::from_secs(5);
const KEPT: Duration = Duration::from_secs(20);
/// No more than this many are kept, however many say they are one.
const MOST: usize = 32;

/// What one says of itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Said {
    /// The version of this way of saying it.
    spool: u32,
    name: String,
    port: u16,
    /// It is waiting to be linked with a code.
    open: bool,
}

/// A Spool heard of on the network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Found {
    pub name: String,
    /// Where it answers: the address it was heard from and the port it named. Tried by
    /// whoever chooses it, and trusted for nothing.
    pub url: String,
    /// It says it is waiting to be linked with a code.
    pub open: bool,
}

/// A name as it may be said: short, and nothing but what a name is made of.
fn label(name: &str) -> String {
    let kept: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ')).take(40).collect();
    let kept = kept.trim();
    if kept.is_empty() { "spool".to_string() } else { kept.to_string() }
}

/// What was heard, if it is one of these and names somewhere that could answer.
fn read(packet: &[u8], from: IpAddr) -> Option<Found> {
    let said: Said = serde_json::from_slice(packet).ok()?;
    (said.spool == 1 && said.port != 0 && !from.is_loopback()).then(|| Found { name: label(&said.name), url: format!("http://{from}:{}", said.port), open: said.open })
}

/// This machine's name, as it is known on the network.
pub fn host_name() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is ours and its length is given; the name is ended with a zero.
    let named = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len() - 1) } == 0;
    let end = buf.iter().position(|b| *b == 0).unwrap_or(0);
    let name = if named { String::from_utf8_lossy(&buf[..end]).to_string() } else { String::new() };
    label(name.split('.').next().unwrap_or_default())
}

/// The addresses this machine has on networks other machines are on.
fn here() -> Vec<Ipv4Addr> {
    if_addrs::get_if_addrs().into_iter().flatten().filter(|i| !i.is_loopback()).filter_map(|i| match i.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    }).collect()
}

/// Say that a Spool is here, under a name and at a port, for as long as the program runs.
/// `open` is asked each time: whether it is waiting to be linked with a code just then.
pub fn announce(name: &str, port: u16, open: impl Fn() -> bool + Send + 'static) -> std::io::Result<()> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_multicast_ttl_v4(1)?;
    let name = label(name);
    std::thread::Builder::new().name("spool-announce".into()).spawn(move || loop {
        let said = serde_json::to_vec(&Said { spool: 1, name: name.clone(), port, open: open() }).expect("it can always be written");
        // On every network it is on, since which of them a Spool is listening on is not known.
        for ip in here() {
            if socket.set_multicast_if_v4(&ip).is_ok() {
                let _ = socket.send_to(&said, &SocketAddr::V4(SocketAddrV4::new(GROUP, PORT)).into());
            }
        }
        std::thread::sleep(EVERY);
    })?;
    Ok(())
}

/// The Spools heard of, kept as they come and go.
#[derive(Clone, Default)]
pub struct Heard(Arc<Mutex<HashMap<String, (Instant, Found)>>>);

impl Heard {
    /// Begin listening, on every network this machine is on and any it joins later.
    pub fn listen() -> std::io::Result<Heard> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        #[cfg(unix)]
        socket.set_reuse_port(true)?;
        socket.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT)).into())?;
        socket.set_read_timeout(Some(EVERY))?;
        let socket: std::net::UdpSocket = socket.into();
        let heard = Heard::default();
        let keep = heard.clone();
        std::thread::Builder::new().name("spool-find".into()).spawn(move || {
            let (mut joined, mut packet) = (Vec::new(), [0u8; 512]);
            loop {
                for ip in here() {
                    if !joined.contains(&ip) && socket.join_multicast_v4(&GROUP, &ip).is_ok() {
                        joined.push(ip);
                    }
                }
                if let Ok((len, from)) = socket.recv_from(&mut packet) {
                    if let Some(found) = read(&packet[..len], from.ip()) {
                        keep.note(found);
                    }
                }
            }
        })?;
        Ok(heard)
    }

    fn note(&self, found: Found) {
        let mut kept = self.0.lock();
        kept.retain(|_, (when, _)| when.elapsed() < KEPT);
        if kept.len() < MOST || kept.contains_key(&found.url) {
            kept.insert(found.url.clone(), (Instant::now(), found));
        }
    }

    /// What is heard of now, by name, without those at an address that is linked already.
    pub fn found(&self, linked: &[String]) -> Vec<Found> {
        let at = |url: &str| url.trim().trim_end_matches('/').to_ascii_lowercase();
        let mut all: Vec<Found> = self.0.lock().values().filter(|(when, f)| when.elapsed() < KEPT && !linked.iter().any(|l| at(l) == at(&f.url))).map(|(_, f)| f.clone()).collect();
        all.sort_by(|a, b| (&a.name, &a.url).cmp(&(&b.name, &b.url)));
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_said_is_plain_and_what_is_heard_is_checked() {
        assert_eq!(label("  turk  "), "turk");
        assert_eq!(label("Trey's NAS (main).local"), "Treys NAS mainlocal");
        assert_eq!(label("../../<script>"), "script");
        assert_eq!(label("!!!"), "spool");
        assert_eq!(label(&"a".repeat(200)).len(), 40);
        assert!(!host_name().is_empty() && !host_name().contains('.'));

        let from = "192.168.69.97".parse::<IpAddr>().unwrap();
        let said = serde_json::to_vec(&Said { spool: 1, name: "<b>turk</b>".into(), port: 7978, open: true }).unwrap();
        assert_eq!(read(&said, from), Some(Found { name: "bturkb".into(), url: "http://192.168.69.97:7978".into(), open: true }), "the address is the one it was heard from");
        assert_eq!(read(&said, "127.0.0.1".parse().unwrap()), None);
        for not_one in [&b"hello"[..], br#"{"spool":2,"name":"x","port":1,"open":false}"#, br#"{"spool":1,"name":"x","port":0,"open":false}"#, br#"{"spool":1,"name":"x","port":1,"open":false,"url":"http://elsewhere"}"#, b""] {
            assert_eq!(read(not_one, from), None, "{}", String::from_utf8_lossy(not_one));
        }
    }

    #[test]
    fn what_is_heard_is_kept_a_while_and_one_linked_already_is_not_offered() {
        let heard = Heard::default();
        let turk = Found { name: "turk".into(), url: "http://192.168.69.97:7978".into(), open: false };
        heard.note(turk.clone());
        heard.note(Found { open: true, ..turk.clone() });
        assert_eq!(heard.found(&[]), [Found { open: true, ..turk.clone() }], "said again, it is the same one as it now is");
        assert!(heard.found(&["http://192.168.69.97:7978/".into()]).is_empty());
        heard.0.lock().values_mut().for_each(|(when, _)| *when = Instant::now() - KEPT);
        assert!(heard.found(&[]).is_empty(), "one that has stopped saying so is no longer listed");
        for n in 0..(MOST + 10) {
            heard.note(Found { name: "x".into(), url: format!("http://10.0.0.{n}:1"), open: false });
        }
        assert_eq!(heard.found(&[]).len(), MOST);
    }

    /// Run by hand: it speaks on the network this machine is on, which a machine that only
    /// builds and tests may not allow.
    #[test]
    #[ignore]
    fn a_spool_that_says_it_is_here_is_heard() {
        let heard = Heard::listen().unwrap();
        announce("spool test helper", 47978, || true).unwrap();
        for _ in 0..80 {
            if heard.found(&[]).iter().any(|f| f.name == "spool test helper" && f.open && f.url.ends_with(":47978")) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("not heard: {:?}", heard.found(&[]));
    }
}
