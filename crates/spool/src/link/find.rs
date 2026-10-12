//! Finding Spools on the home network.
//!
//! A helper says that it is there, the way printers and players do, and a Spool listens
//! and lists what it hears so that an address need not be typed. Hearing of one links
//! nothing: anything on a network can say it is a Spool, so what is heard is only ever an
//! address to try, and the code or the key is still what links the two. An announcement
//! carries no code, no key, no folder and nothing of what is in the library.

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

/// What a Spool is known as to anything that looks for one.
const KIND: &str = "_spool._tcp.local.";
/// No more than this many are kept, however many say they are one.
const MOST: usize = 32;

/// A Spool heard of on the network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Found {
    pub name: String,
    /// Where it says it answers. Tried by whoever chooses it, and trusted for nothing.
    pub url: String,
    /// It says it is waiting to be linked with a code.
    pub open: bool,
}

/// A name as it may go in an announcement: short, and nothing but what a name is made of.
fn label(name: &str) -> String {
    let kept: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ')).take(40).collect();
    let kept = kept.trim();
    if kept.is_empty() { "spool".to_string() } else { kept.to_string() }
}

/// An address as it goes in a link's settings. The older kind of address is preferred, and
/// one that only works on a single wire is left out.
fn url_of(addrs: impl Iterator<Item = IpAddr>, port: u16) -> Option<String> {
    let mut addrs: Vec<IpAddr> = addrs.filter(|ip| !ip.is_loopback() && !matches!(ip, IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80)).collect();
    addrs.sort_by_key(|ip| !ip.is_ipv4());
    addrs.first().map(|ip| match ip {
        IpAddr::V4(v4) => format!("http://{v4}:{port}"),
        IpAddr::V6(v6) => format!("http://[{v6}]:{port}"),
    })
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

/// Saying that a Spool is here, for as long as this is kept.
pub struct Announced {
    daemon: ServiceDaemon,
    name: String,
    port: u16,
}

impl Announced {
    /// Begin saying so. `open` is whether it is waiting to be linked with a code.
    pub fn begin(name: &str, port: u16, open: bool) -> anyhow::Result<Announced> {
        let said = Announced { daemon: ServiceDaemon::new()?, name: label(name), port };
        said.say(open)?;
        Ok(said)
    }

    /// Say it again, with whether it is waiting to be linked as that stands now.
    pub fn say(&self, open: bool) -> anyhow::Result<()> {
        let host = format!("{}.local.", self.name.replace([' ', '_'], "-"));
        let info = ServiceInfo::new(KIND, &self.name, &host, "", self.port, &[("v", "1"), ("open", if open { "1" } else { "0" })][..])?.enable_addr_auto();
        self.daemon.register(info)?;
        Ok(())
    }
}

impl Drop for Announced {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// The Spools heard of, kept as they come and go.
#[derive(Clone)]
pub struct Heard {
    found: Arc<Mutex<HashMap<String, Found>>>,
    _daemon: Arc<ServiceDaemon>,
}

impl Heard {
    /// Begin listening. What is heard is kept until it says it is going, or is heard no more.
    pub fn listen() -> anyhow::Result<Heard> {
        let daemon = ServiceDaemon::new()?;
        let events = daemon.browse(KIND)?;
        let found: Arc<Mutex<HashMap<String, Found>>> = Default::default();
        let keep = found.clone();
        std::thread::Builder::new().name("spool-find".into()).spawn(move || {
            while let Ok(event) = events.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let Some(url) = url_of(info.get_addresses().iter().map(|a| a.to_ip_addr()), info.get_port()) else { continue };
                        let full = info.get_fullname().to_string();
                        let name = label(full.strip_suffix(KIND).unwrap_or(&full).trim_end_matches('.'));
                        let mut keep = keep.lock();
                        if keep.len() < MOST || keep.contains_key(&full) {
                            keep.insert(full, Found { name, url, open: info.get_property_val_str("open") == Some("1") });
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, full) => {
                        keep.lock().remove(&full);
                    }
                    _ => {}
                }
            }
        })?;
        Ok(Heard { found, _daemon: Arc::new(daemon) })
    }

    /// What is heard of now, by name, without those at an address that is linked already.
    pub fn found(&self, linked: &[String]) -> Vec<Found> {
        let at = |url: &str| url.trim().trim_end_matches('/').to_ascii_lowercase();
        let mut all: Vec<Found> = self.found.lock().values().filter(|f| !linked.iter().any(|l| at(l) == at(&f.url))).cloned().collect();
        all.sort_by(|a, b| (&a.name, &a.url).cmp(&(&b.name, &b.url)));
        all.dedup();
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_goes_in_an_announcement_is_plain() {
        assert_eq!(label("  turk  "), "turk");
        assert_eq!(label("Trey's NAS (main).local"), "Treys NAS mainlocal");
        assert_eq!(label("../../<script>"), "script");
        assert_eq!(label("!!!"), "spool");
        assert_eq!(label(&"a".repeat(200)).len(), 40);
        assert!(!host_name().is_empty() && !host_name().contains('.'));
    }

    #[test]
    fn an_address_heard_is_one_that_can_be_tried() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert_eq!(url_of([ip("fd00::7"), ip("192.168.69.97")].into_iter(), 7978).as_deref(), Some("http://192.168.69.97:7978"));
        assert_eq!(url_of([ip("fd00::7")].into_iter(), 7978).as_deref(), Some("http://[fd00::7]:7978"));
        assert_eq!(url_of([ip("127.0.0.1"), ip("fe80::1"), ip("::1")].into_iter(), 7978), None, "nothing that only this machine or this wire could use");
    }

    /// Run by hand: it speaks on the network this machine is on, which a machine that only
    /// builds and tests may not allow.
    #[test]
    #[ignore]
    fn a_spool_that_says_it_is_here_is_heard() {
        let heard = Heard::listen().unwrap();
        let said = Announced::begin("spool test helper", 47978, true).unwrap();
        let wait = |open: bool| {
            for _ in 0..80 {
                if heard.found(&[]).iter().any(|f| f.name == "spool test helper" && f.open == open) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            false
        };
        assert!(wait(true), "{:?}", heard.found(&[]));
        let found = heard.found(&[]).into_iter().find(|f| f.name == "spool test helper").unwrap();
        assert!(found.url.ends_with(":47978"), "{found:?}");
        assert!(heard.found(&[found.url.clone()]).iter().all(|f| f.name != "spool test helper"), "one linked already is not offered again");
        said.say(false).unwrap();
        assert!(wait(false), "it says so when it has been linked");
    }
}
