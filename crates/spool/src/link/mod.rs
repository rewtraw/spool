//! What two linked Spools agree on.
//!
//! A Spool may be linked to another: a helper on a NAS today (see [`crate::helper`]), another
//! household's Spool later. What a link allows is the only difference between them, and it
//! is set by the side that gives it: [`Grants`]. This holds what both ends of a link share:
//! the grants, how each says what it speaks, the folders one shares with the other, and the
//! ticket a file is played on (see [`ticket`]).
//!
//! Each kind of thing a link can do is spoken in versions of its own, so that two Spools on
//! different releases go on doing whatever they both still understand.

pub mod tailnet;
pub mod ticket;

use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

/// The versions of each protocol this Spool speaks. A version is never changed once it has
/// been released: what it means stays, and anything that would change it is a new version.
pub const WORK: &[u32] = &[1];
pub const PLAY: &[u32] = &[1];
pub const SEE: &[u32] = &[1];

/// What one side of a link lets the other do. Nothing, unless it says so.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grants {
    /// List what is in the folders that are shared.
    pub see: bool,
    /// Fetch one file, on a ticket.
    pub play: bool,
    /// Download, file, move and convert on this machine's disks, as directed.
    pub work: bool,
}

impl Grants {
    pub const ALL: Grants = Grants { see: true, play: true, work: true };

    /// From a list such as `see,play`, which is how a container is told.
    pub fn parse(list: &str) -> Result<Grants, String> {
        let mut g = Grants::default();
        for word in list.split(',').map(str::trim).filter(|w| !w.is_empty()) {
            match word.to_ascii_lowercase().as_str() {
                "see" => g.see = true,
                "play" => g.play = true,
                "work" => g.work = true,
                other => return Err(format!("{other} is not something a link can allow; the three are see, play and work")),
            }
        }
        Ok(g)
    }

    pub fn words(&self) -> Vec<&'static str> {
        [(self.see, "see"), (self.play, "play"), (self.work, "work")].into_iter().filter(|g| g.0).map(|g| g.1).collect()
    }
}

/// The versions of each protocol one side speaks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Protocols {
    pub work: Vec<u32>,
    pub play: Vec<u32>,
    pub see: Vec<u32>,
}

impl Protocols {
    /// What this Spool speaks, of the things it allows the other side.
    pub fn allowed(g: Grants) -> Protocols {
        let of = |granted: bool, versions: &[u32]| if granted { versions.to_vec() } else { vec![] };
        Protocols { work: of(g.work, WORK), play: of(g.play, PLAY), see: of(g.see, SEE) }
    }
}

/// The newest version both sides speak, if there is one.
pub fn agreed(ours: &[u32], theirs: &[u32]) -> Option<u32> {
    ours.iter().filter(|v| theirs.contains(v)).max().copied()
}

/// A folder one side shares with the other. A file in it is named by the share and its
/// place inside, never by a full path, so neither side needs to know how the other's disks
/// are laid out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Share {
    pub id: String,
    pub path: PathBuf,
    pub writable: bool,
}

impl Share {
    /// A share is known by a short name made from its folder, which stays the same for as
    /// long as the folder does.
    pub fn new(path: &Path, writable: bool) -> Share {
        let digest = ring::digest::digest(&ring::digest::SHA256, path.to_string_lossy().as_bytes());
        let id = digest.as_ref()[..6].iter().map(|b| format!("{b:02x}")).collect();
        Share { id, path: path.to_path_buf(), writable }
    }
}

/// The share a path lies in, and its place inside.
pub fn place<'a>(shares: &'a [Share], path: &Path) -> Option<(&'a Share, String)> {
    shares.iter().filter_map(|s| Some((s, path.strip_prefix(&s.path).ok()?))).filter(|(_, rel)| !rel.as_os_str().is_empty()).map(|(s, rel)| (s, rel.to_string_lossy().to_string())).next()
}

/// A range of addresses, written `192.168.1.0/24`, or one address alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Net {
    addr: IpAddr,
    bits: u8,
}

/// An address as itself: one of the old kind that arrived dressed as the new kind is the old kind.
fn plain_addr(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

impl Net {
    pub fn parse(text: &str) -> Result<Net, String> {
        let bad = || format!("{text} is not a range of addresses; write one like 192.168.1.0/24, or a single address");
        let (addr, bits) = match text.trim().split_once('/') {
            Some((addr, bits)) => (addr, Some(bits.trim().parse::<u8>().map_err(|_| bad())?)),
            None => (text.trim(), None),
        };
        let addr = plain_addr(addr.trim().parse::<IpAddr>().map_err(|_| bad())?);
        let most = if addr.is_ipv4() { 32 } else { 128 };
        let bits = bits.unwrap_or(most);
        if bits > most {
            return Err(bad());
        }
        Ok(Net { addr, bits })
    }

    pub fn holds(&self, ip: IpAddr) -> bool {
        let same = |a: u128, b: u128, width: u32| self.bits == 0 || (a ^ b) >> (width - self.bits as u32) == 0;
        match (self.addr, plain_addr(ip)) {
            (IpAddr::V4(a), IpAddr::V4(b)) => same(u32::from(a) as u128, u32::from(b) as u128, 32),
            (IpAddr::V6(a), IpAddr::V6(b)) => same(u128::from(a), u128::from(b), 128),
            _ => false,
        }
    }
}

/// What a linked Spool says of itself when asked.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Hello {
    /// Different each time it starts.
    pub boot: String,
    /// The release it is, for a person to read. Nothing is decided by it.
    pub version: String,
    pub protocols: Protocols,
    /// What it allows whoever asked.
    pub grants: Grants,
    pub shares: Vec<Share>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_are_read_from_a_list_and_nothing_is_allowed_unasked() {
        assert_eq!(Grants::parse("see, Play").unwrap(), Grants { see: true, play: true, work: false });
        assert_eq!(Grants::parse("").unwrap(), Grants::default());
        assert!(Grants::parse("see,everything").is_err());
        assert_eq!(Grants::ALL.words(), ["see", "play", "work"]);
        assert_eq!(serde_json::from_str::<Grants>(r#"{"play":true,"later":true}"#).unwrap(), Grants { play: true, ..Default::default() });
    }

    #[test]
    fn the_newest_version_both_speak_is_the_one_used() {
        assert_eq!(agreed(&[1, 2, 3], &[2, 3, 4]), Some(3));
        assert_eq!(agreed(&[1], &[2]), None);
        assert_eq!(agreed(&[1], &[]), None);
    }

    #[test]
    fn a_range_holds_the_addresses_in_it_and_no_others() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let home = Net::parse(" 192.168.69.0/24 ").unwrap();
        assert!(home.holds(ip("192.168.69.76")) && home.holds(ip("192.168.69.255")));
        assert!(!home.holds(ip("192.168.70.1")) && !home.holds(ip("100.88.175.107")) && !home.holds(ip("fd00::1")));
        assert!(home.holds(ip("::ffff:192.168.69.76")), "an old address dressed as a new one is the same address");
        let one = Net::parse("192.168.69.76").unwrap();
        assert!(one.holds(ip("192.168.69.76")) && !one.holds(ip("192.168.69.77")));
        let v6 = Net::parse("fd7a:115c:a1e0::/48").unwrap();
        assert!(v6.holds(ip("fd7a:115c:a1e0::1")) && !v6.holds(ip("fd7a:115c:a1e1::1")) && !v6.holds(ip("10.0.0.1")));
        assert!(Net::parse("0.0.0.0/0").unwrap().holds(ip("8.8.8.8")));
        assert!(Net::parse("10.0.0.0/8").unwrap().holds(ip("10.255.0.1")));
        for bad in ["", "home", "192.168.1.0/33", "192.168.1/24", "fd00::/129", "192.168.1.0/x"] {
            assert!(Net::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_path_is_placed_in_the_share_that_holds_it() {
        let shares = [Share::new(Path::new("/nas/films"), true), Share::new(Path::new("/nas/vault"), false)];
        assert_ne!(shares[0].id, shares[1].id);
        assert_eq!(shares[0].id, Share::new(Path::new("/nas/films"), false).id, "a share keeps its name");
        let (share, rel) = place(&shares, Path::new("/nas/vault/Alpha (2001)/Alpha.mkv")).unwrap();
        assert_eq!((share.id.as_str(), rel.as_str()), (shares[1].id.as_str(), "Alpha (2001)/Alpha.mkv"));
        assert!(place(&shares, Path::new("/nas/films")).is_none(), "a share itself is not a file in it");
        assert!(place(&shares, Path::new("/nas/filmsier/x.mkv")).is_none());
        assert!(place(&shares, Path::new("/elsewhere/x.mkv")).is_none());
    }
}
