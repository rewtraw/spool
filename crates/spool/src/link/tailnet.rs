//! Where a player on Tailscale really is.
//!
//! A player that reaches Spool over Tailscale is seen at its Tailscale address, at home or
//! away. Tailscale on this machine knows which address it is really talking to that player
//! at, when the two talk directly. That address is what says whether the player is on the
//! same network as a linked machine. A player reached through a relay, or one Tailscale
//! says nothing of, is taken to be away.

use parking_lot::Mutex;
use serde_json::Value;
use std::net::{IpAddr, SocketAddr};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// How long what Tailscale said is kept. A player asks for a film in many parts.
const KEPT: Duration = Duration::from_secs(30);

static SAID: LazyLock<Mutex<Option<(Instant, Value)>>> = LazyLock::new(Default::default);

/// An address Tailscale gives out: 100.64.0.0/10, or one under fd7a:115c:a1e0::/48.
pub fn ours(ip: IpAddr) -> bool {
    ["100.64.0.0/10", "fd7a:115c:a1e0::/48"].iter().filter_map(|n| super::Net::parse(n).ok()).any(|n| n.holds(ip))
}

fn addresses(peer: &Value) -> impl Iterator<Item = IpAddr> + '_ {
    peer["TailscaleIPs"].as_array().into_iter().flatten().filter_map(|i| i.as_str()?.parse::<IpAddr>().ok())
}

/// In what Tailscale says of the devices it knows, the address this machine reaches the
/// one at `ip` at directly.
fn direct(status: &Value, ip: IpAddr) -> Option<IpAddr> {
    let peer = status["Peer"].as_object()?.values().find(|p| addresses(p).any(|i| i == ip))?;
    peer["CurAddr"].as_str()?.parse::<SocketAddr>().ok().map(|at| at.ip())
}

/// The Tailscale addresses of the devices that go by a name, the older kind of address first.
fn named(status: &Value, name: &str) -> Vec<IpAddr> {
    let mut found: Vec<IpAddr> = status["Peer"].as_object().into_iter().flat_map(|peers| peers.values()).filter(|p| p["HostName"].as_str().is_some_and(|h| h.eq_ignore_ascii_case(name.trim()))).flat_map(addresses).collect();
    found.sort_by_key(|ip| !ip.is_ipv4());
    found
}

fn tool() -> Option<&'static str> {
    ["/usr/local/bin/tailscale", "/opt/homebrew/bin/tailscale", "/usr/bin/tailscale", "/usr/sbin/tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"].into_iter().find(|p| std::path::Path::new(p).is_file())
}

/// What Tailscale on this machine says of the devices it knows; nothing where there is no
/// Tailscale, or it does not answer in a moment.
async fn said() -> Value {
    if let Some((when, said)) = SAID.lock().clone() {
        if when.elapsed() < KEPT {
            return said;
        }
    }
    let asked = async {
        let out = tokio::process::Command::new(tool()?).args(["status", "--json"]).kill_on_drop(true).output().await.ok()?;
        serde_json::from_slice::<Value>(&out.stdout).ok()
    };
    let said = tokio::time::timeout(Duration::from_secs(2), asked).await.ok().flatten().unwrap_or(Value::Null);
    *SAID.lock() = Some((Instant::now(), said.clone()));
    said
}

/// The address a player is really at: for one on Tailscale that this machine talks to
/// directly, that address; for one on Tailscale otherwise, none; for any other, its own.
pub async fn really(player: IpAddr) -> Option<IpAddr> {
    if !ours(player) {
        return Some(player);
    }
    direct(&said().await, player)
}

/// The Tailscale addresses of the devices on this machine's Tailscale that go by a name.
pub async fn addresses_of(name: &str) -> Vec<IpAddr> {
    named(&said().await, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_player_on_tailscale_is_placed_by_the_address_it_is_reached_at() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(ours(ip("100.105.199.49")) && ours(ip("fd7a:115c:a1e0::f401:c731")));
        assert!(!ours(ip("192.168.69.53")) && !ours(ip("100.128.0.1")));
        let status = serde_json::json!({"Peer": {
            "a": {"TailscaleIPs": ["100.105.199.49", "fd7a:115c:a1e0::f401:c731"], "CurAddr": "192.168.69.53:41641"},
            "b": {"TailscaleIPs": ["100.88.175.107"], "CurAddr": "", "Relay": "iad"},
            "c": {"TailscaleIPs": ["100.89.63.59"], "CurAddr": "[2001:db8::7]:41641"},
        }});
        assert_eq!(direct(&status, ip("100.105.199.49")), Some(ip("192.168.69.53")));
        assert_eq!(direct(&status, ip("fd7a:115c:a1e0::f401:c731")), Some(ip("192.168.69.53")));
        assert_eq!(direct(&status, ip("100.88.175.107")), None, "reached through a relay, so it may be anywhere");
        assert_eq!(direct(&status, ip("100.89.63.59")), Some(ip("2001:db8::7")));
        assert_eq!(direct(&status, ip("100.1.2.3")), None);
        assert_eq!(direct(&serde_json::json!({}), ip("100.105.199.49")), None);
        let status = serde_json::json!({"Peer": {
            "a": {"HostName": "Turk", "TailscaleIPs": ["fd7a:115c:a1e0::1", "100.97.136.99"]},
            "b": {"HostName": "turkey", "TailscaleIPs": ["100.1.1.1"]},
        }});
        assert_eq!(named(&status, " turk "), [ip("100.97.136.99"), ip("fd7a:115c:a1e0::1")]);
        assert!(named(&status, "nas").is_empty() && named(&serde_json::Value::Null, "turk").is_empty());
    }
}
