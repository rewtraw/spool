//! Playing on a streamer or amplifier on the home network, such as a WiiM, the way Spotify
//! plays to one: the device is told which file to play and fetches it from Spool itself, at
//! full quality, and whatever told it may go to sleep. Spool keeps the queue and moves the
//! device along it.
//!
//! Devices are found with UPnP (SSDP), as players that take a link and play it (a
//! MediaRenderer with an AVTransport service), and told what to do over SOAP. A device can
//! play only what it can fetch without signing in, so each track is given to it at a link
//! that names that track alone and stops working after a while ([`Cast::link`]).

use super::{subsonic, Track};
use crate::app::App;
use anyhow::{anyhow, bail, Result};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

const SSDP: (Ipv4Addr, u16) = (Ipv4Addr::new(239, 255, 255, 250), 1900);
const AVT: &str = "urn:schemas-upnp-org:service:AVTransport:1";
const RC: &str = "urn:schemas-upnp-org:service:RenderingControl:1";
/// How long a link given to a device works: longer than any album.
const LINK_LASTS: u64 = 12 * 3600;
/// The most a device's description or answer may run to.
const ANSWER_MOST: usize = 256 << 10;
/// The most players kept at once.
const MOST: usize = 32;

/// Devices are asked with a client of their own: no redirects and no proxy, so that what one
/// says cannot send Spool anywhere but back to it.
static DEVICES: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).no_proxy().connect_timeout(Duration::from_secs(3)).build().expect("http client")
});

/// Whether a link a device gave is to that device itself.
fn on_device(url: &reqwest::Url, ip: IpAddr) -> bool {
    url.scheme() == "http" && url.host_str().map(|h| h.trim_matches(['[', ']'])).and_then(|h| h.parse::<IpAddr>().ok()) == Some(ip)
}

async fn answer(resp: reqwest::Response) -> Result<String> {
    let body = crate::app::read_at_most(resp, ANSWER_MOST).await.map_err(|e| anyhow!("the player's answer {e}"))?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// A device found on the network that can be told to play.
#[derive(Clone, Debug, Serialize)]
pub struct Renderer {
    /// Spool's own short name for it, made from the device's UPnP id.
    pub id: String,
    pub name: String,
    pub model: String,
    #[serde(skip)]
    ip: IpAddr,
    #[serde(skip)]
    transport: String,
    #[serde(skip)]
    volume: Option<String>,
}

/// What is playing on one device.
#[derive(Clone, Debug, Serialize)]
pub struct Session {
    pub renderer: String,
    pub name: String,
    pub queue: Vec<i64>,
    pub at: usize,
    pub album_id: Option<i64>,
    /// "playing", "paused", "loading" or "stopped".
    pub state: String,
    pub position: f64,
    pub duration: f64,
    pub volume: Option<u32>,
    pub error: String,
    /// Spool told it to stop or pause, so its stopping is not the end of a track.
    #[serde(skip)]
    held: bool,
    /// When the current track was handed over; a device reports "stopped" for a moment
    /// while it starts, which is not the track ending.
    #[serde(skip)]
    loaded_at: Instant,
    /// The next track was handed over as well, for a device that plays on without a gap.
    #[serde(skip)]
    next_given: Option<i64>,
}

pub struct Cast {
    /// Held while a player is told something or asked how it is getting on, so that a Stop
    /// is never overtaken by a track being handed over.
    turns: Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    renderers: Mutex<HashMap<String, Renderer>>,
    sessions: Mutex<HashMap<String, Session>>,
    key: ring::hmac::Key,
    searched: Mutex<Option<Instant>>,
    following: std::sync::Once,
}

impl Default for Cast {
    fn default() -> Self {
        // Links are signed with a key made at start, so none outlives this Spool.
        let rng = ring::rand::SystemRandom::new();
        Cast {
            turns: Default::default(),
            renderers: Default::default(),
            sessions: Default::default(),
            key: ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &rng).expect("a random key"),
            searched: Default::default(),
            following: std::sync::Once::new(),
        }
    }
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

impl Cast {
    fn turn(&self, renderer: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        self.turns.lock().entry(renderer.to_string()).or_default().clone()
    }

    /// The path of a link to one track that a device may fetch without signing in, until
    /// `until`: /cast/{until}.{signature}/{track}.{suffix}.
    fn link(&self, t: &Track, until: u64) -> String {
        let sig = ring::hmac::sign(&self.key, format!("{}:{until}", t.id).as_bytes());
        format!("/cast/{until}.{}/{}.{}", b64(&sig.as_ref()[..16]), t.id, if t.suffix.is_empty() { "bin" } else { &t.suffix })
    }

    /// The track a link names, if it was signed here and has not run out.
    pub fn check(&self, token: &str, track: i64, now: u64) -> bool {
        let Some((until, sig)) = token.split_once('.') else { return false };
        let Ok(until) = until.parse::<u64>() else { return false };
        if until < now || until > now + LINK_LASTS + 60 {
            return false;
        }
        let want = ring::hmac::sign(&self.key, format!("{track}:{until}").as_bytes());
        // Compared in constant time.
        crate::api::same(&b64(&want.as_ref()[..16]), sig)
    }
}

/// Text between `<tag>` and `</tag>`, wherever the tag is and whatever its prefix.
fn tag(xml: &str, name: &str) -> Option<String> {
    let mut rest = xml;
    loop {
        let at = rest.find('<')?;
        rest = &rest[at + 1..];
        let end = rest.find('>')?;
        let head = &rest[..end];
        let local = head.split_whitespace().next().unwrap_or("").rsplit(':').next().unwrap_or("");
        if local == name && !head.starts_with('/') && !head.ends_with('/') {
            let body = &rest[end + 1..];
            let close = body.find("</")?;
            return Some(unescape(body[..close].trim()));
        }
    }
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Each `<service>` of a device description: its type and where it is controlled.
fn services(xml: &str) -> Vec<(String, String)> {
    xml.split("<service>").skip(1).filter_map(|s| Some((tag(s, "serviceType")?, tag(s, "controlURL")?))).collect()
}

fn hhmmss(s: f64) -> String {
    let s = s.max(0.0) as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn seconds(t: &str) -> f64 {
    let parts: Vec<f64> = t.split('.').next().unwrap_or("").split(':').filter_map(|p| p.parse().ok()).collect();
    parts.iter().fold(0.0, |acc, p| acc * 60.0 + p)
}

/// The address of this machine that a device at `ip` would reach it on.
fn own_address(ip: IpAddr) -> Option<IpAddr> {
    let s = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect((ip, 1900)).ok()?;
    s.local_addr().ok().map(|a| a.ip())
}

impl App {
    /// The devices on the network that can be told to play, looked for again when asked or
    /// when the last look is more than a few minutes old.
    pub async fn renderers(&self, look: bool) -> Vec<Renderer> {
        let stale = self.music.cast.searched.lock().is_none_or(|t| t.elapsed() > Duration::from_secs(300));
        if look || stale {
            match self.find_renderers().await {
                Ok(found) => {
                    *self.music.cast.searched.lock() = Some(Instant::now());
                    // What the last look found, and whatever is playing now though it did not answer.
                    let playing: Vec<String> = self.music.cast.sessions.lock().keys().cloned().collect();
                    let mut all = self.music.cast.renderers.lock();
                    all.retain(|id, _| playing.contains(id));
                    for r in found.into_iter().take(MOST) {
                        all.insert(r.id.clone(), r);
                    }
                }
                Err(e) => tracing::warn!(error = %e, "could not look for players on the network"),
            }
        }
        let mut out: Vec<Renderer> = self.music.cast.renderers.lock().values().cloned().collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out
    }

    async fn find_renderers(&self) -> Result<Vec<Renderer>> {
        let socket = tokio::net::UdpSocket::bind(("0.0.0.0", 0)).await?;
        let ask = format!("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {AVT}\r\n\r\n");
        for _ in 0..2 {
            socket.send_to(ask.as_bytes(), SocketAddr::from(SSDP)).await?;
        }
        // Who answered, and where each describes itself.
        let mut heard: HashMap<String, IpAddr> = HashMap::new();
        let until = Instant::now() + Duration::from_millis(2500);
        let mut buf = [0u8; 2048];
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            let Ok(Ok((n, from))) = tokio::time::timeout(left, socket.recv_from(&mut buf)).await else { break };
            // Only what is on this network is listened to, and only its own description.
            if crate::app::public(from.ip()) || heard.len() >= 32 {
                continue;
            }
            let text = String::from_utf8_lossy(&buf[..n]);
            let location = text.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("location")).map(|(_, v)| v.trim().to_string()));
            if let Some(loc) = location {
                let same_host = reqwest::Url::parse(&loc).ok().and_then(|u| u.host_str().and_then(|h| h.parse::<IpAddr>().ok())) == Some(from.ip());
                if same_host {
                    heard.insert(loc, from.ip());
                }
            }
        }
        let mut out = vec![];
        for (loc, ip) in heard {
            match self.describe(&loc, ip).await {
                Ok(r) => out.push(r),
                Err(e) => tracing::debug!(location = %loc, error = %e, "a player on the network could not be read"),
            }
        }
        Ok(out)
    }

    async fn describe(&self, location: &str, ip: IpAddr) -> Result<Renderer> {
        let loc = reqwest::Url::parse(location)?;
        if !on_device(&loc, ip) {
            bail!("it described itself somewhere else");
        }
        let xml = answer(DEVICES.get(loc.clone()).timeout(Duration::from_secs(4)).send().await?).await?;
        let base = tag(&xml, "URLBase").and_then(|b| reqwest::Url::parse(&b).ok()).filter(|b| on_device(b, ip)).unwrap_or(loc);
        let found = services(&xml);
        let control = |kind: &str| found.iter().find(|(t, _)| t.starts_with(kind.trim_end_matches('1'))).and_then(|(_, c)| base.join(c).ok()).filter(|u| on_device(u, ip)).map(|u| u.to_string());
        let transport = control(AVT).ok_or_else(|| anyhow!("it does not take links to play"))?;
        let udn = tag(&xml, "UDN").unwrap_or_else(|| location.to_string());
        let id = b64(&ring::digest::digest(&ring::digest::SHA256, udn.as_bytes()).as_ref()[..6]);
        Ok(Renderer { id, name: tag(&xml, "friendlyName").unwrap_or_else(|| ip.to_string()), model: tag(&xml, "modelName").unwrap_or_default(), ip, transport, volume: control(RC) })
    }

    async fn soap(&self, url: &str, service: &str, action: &str, args: &[(&str, String)]) -> Result<String> {
        let body: String = args.iter().map(|(k, v)| format!("<{k}>{}</{k}>", escape(v))).collect();
        let envelope = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{service}\">{body}</u:{action}></s:Body></s:Envelope>"
        );
        let resp = DEVICES
            .post(url)
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header("SOAPACTION", format!("\"{service}#{action}\""))
            .body(envelope)
            .timeout(Duration::from_secs(6))
            .send()
            .await?;
        let status = resp.status();
        let text = answer(resp).await?;
        if !status.is_success() {
            let why = tag(&text, "errorDescription").unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
            bail!("{action} was refused: {why}");
        }
        Ok(text)
    }

    fn renderer(&self, id: &str) -> Result<Renderer> {
        self.music.cast.renderers.lock().get(id).cloned().ok_or_else(|| anyhow!("that player is not on the network now"))
    }

    /// What one track is called, for the device's screen, and where it fetches it from.
    fn handed(&self, r: &Renderer, t: &Track) -> Result<(String, String)> {
        let host = own_address(r.ip).ok_or_else(|| anyhow!("no address of this machine reaches {}", r.name))?;
        let port = self.port.get().copied().unwrap_or(7979);
        let host = match host {
            IpAddr::V6(v6) => format!("[{v6}]"),
            v4 => v4.to_string(),
        };
        let now = crate::db::now() as u64;
        let url = format!("http://{host}:{port}{}", self.music.cast.link(t, now + LINK_LASTS));
        let lib = self.music.library();
        let album = lib.album(t.album_id);
        let art = album.filter(|a| !a.cover.is_empty()).map(|a| format!("<upnp:albumArtURI>http://{host}:{port}{}</upnp:albumArtURI>", self.music.cast.art_link(a.id, now + LINK_LASTS))).unwrap_or_default();
        let meta = format!(
            "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\"><item id=\"{id}\" parentID=\"0\" restricted=\"1\"><dc:title>{title}</dc:title><upnp:artist>{artist}</upnp:artist><upnp:album>{album}</upnp:album>{art}<upnp:class>object.item.audioItem.musicTrack</upnp:class><res protocolInfo=\"http-get:*:{mime}:*\" duration=\"{dur}.000\">{url}</res></item></DIDL-Lite>",
            id = t.id,
            title = escape(&t.title),
            artist = escape(&t.artist),
            album = escape(album.map(|a| a.title.as_str()).unwrap_or("")),
            mime = t.content_type(),
            dur = hhmmss(t.seconds),
            url = escape(&url),
        );
        Ok((url, meta))
    }

    /// Start playing `tracks` from `at` on a device.
    pub async fn cast(&self, renderer: &str, tracks: Vec<i64>, at: usize, album_id: Option<i64>) -> Result<Session> {
        if self.renderers(false).await.iter().all(|r| r.id != renderer) {
            self.renderers(true).await;
        }
        let r = self.renderer(renderer)?;
        let turn = self.music.cast.turn(&r.id);
        let _turn = turn.lock().await;
        let lib = self.music.library();
        let tracks: Vec<i64> = tracks.into_iter().filter(|id| lib.track(*id).is_some()).collect();
        if at >= tracks.len() {
            bail!("nothing to play");
        }
        let s = Session { renderer: r.id.clone(), name: r.name.clone(), queue: tracks, at, album_id, state: "loading".into(), position: 0.0, duration: 0.0, volume: None, error: String::new(), held: false, loaded_at: Instant::now(), next_given: None };
        self.music.cast.sessions.lock().insert(r.id.clone(), s);
        self.load(&r, at).await?;
        self.follow_casts();
        self.cast_state(renderer).ok_or_else(|| anyhow!("it stopped"))
    }

    /// Hand a device track `at` of its queue, and the one after for playing on without a gap.
    async fn load(&self, r: &Renderer, at: usize) -> Result<()> {
        let (track, next) = {
            let sessions = self.music.cast.sessions.lock();
            let s = sessions.get(&r.id).ok_or_else(|| anyhow!("nothing is playing there"))?;
            (s.queue.get(at).copied(), s.queue.get(at + 1).copied())
        };
        let lib = self.music.library();
        let t = track.and_then(|id| lib.track(id).cloned()).ok_or_else(|| anyhow!("that track is gone"))?;
        let (url, meta) = self.handed(r, &t)?;
        let result = async {
            self.soap(&r.transport, AVT, "SetAVTransportURI", &[("InstanceID", "0".into()), ("CurrentURI", url), ("CurrentURIMetaData", meta)]).await?;
            self.soap(&r.transport, AVT, "Play", &[("InstanceID", "0".into()), ("Speed", "1".into())]).await
        }
        .await;
        let mut next_given = None;
        let volume = match (&result, &r.volume) {
            (Ok(_), Some(url)) => self.soap(url, RC, "GetVolume", &[("InstanceID", "0".into()), ("Channel", "Master".into())]).await.ok().and_then(|x| tag(&x, "CurrentVolume")).and_then(|v| v.parse().ok()),
            _ => None,
        };
        if result.is_ok() {
            if let Some(n) = next.and_then(|id| lib.track(id).cloned()) {
                if let Ok((url, meta)) = self.handed(r, &n) {
                    // Not every device takes one; those that do not are moved along by hand.
                    if self.soap(&r.transport, AVT, "SetNextAVTransportURI", &[("InstanceID", "0".into()), ("NextURI", url), ("NextURIMetaData", meta)]).await.is_ok() {
                        next_given = Some(n.id);
                    }
                }
            }
        }
        if let Some(s) = self.music.cast.sessions.lock().get_mut(&r.id) {
            s.at = at;
            s.position = 0.0;
            s.duration = t.seconds;
            s.held = false;
            s.loaded_at = Instant::now();
            s.next_given = next_given;
            if volume.is_some() {
                s.volume = volume;
            }
            match &result {
                Ok(_) => {
                    s.state = "loading".into();
                    s.error.clear();
                }
                Err(e) => {
                    s.state = "stopped".into();
                    s.error = format!("{e:#}");
                }
            }
        }
        result.map(|_| ())
    }

    pub fn cast_state(&self, renderer: &str) -> Option<Session> {
        self.music.cast.sessions.lock().get(renderer).cloned()
    }

    pub fn casts(&self) -> Vec<Session> {
        self.music.cast.sessions.lock().values().cloned().collect()
    }

    /// What is playing on a device, with the track and its album, for showing.
    pub fn cast_json(&self, s: &Session) -> serde_json::Value {
        let lib = self.music.library();
        let track = s.queue.get(s.at).and_then(|id| lib.track(*id));
        let album = track.and_then(|t| lib.album(t.album_id));
        let mut v = serde_json::json!(s);
        v["track"] = serde_json::json!(track.map(|t| serde_json::json!({"id": t.id, "title": t.title, "artist": t.artist, "seconds": t.seconds, "suffix": t.suffix})));
        v["album"] = serde_json::json!(album.map(|a| serde_json::json!({"id": a.id, "title": a.title, "artist": a.artist, "has_cover": !a.cover.is_empty()})));
        v
    }

    /// Do something to what is playing on a device.
    pub async fn cast_act(&self, renderer: &str, action: &str, seconds: Option<f64>, volume: Option<u32>) -> Result<Option<Session>> {
        let r = self.renderer(renderer)?;
        let turn = self.music.cast.turn(&r.id);
        let _turn = turn.lock().await;
        let at = self.cast_state(renderer).map(|s| (s.at, s.queue.len())).ok_or_else(|| anyhow!("nothing is playing there"))?;
        let held = |app: &App, on: bool| {
            if let Some(s) = app.music.cast.sessions.lock().get_mut(renderer) {
                s.held = on;
            }
        };
        match action {
            "pause" => {
                held(self, true);
                self.soap(&r.transport, AVT, "Pause", &[("InstanceID", "0".into())]).await?;
            }
            "play" => {
                held(self, false);
                self.soap(&r.transport, AVT, "Play", &[("InstanceID", "0".into()), ("Speed", "1".into())]).await?;
            }
            "next" if at.0 + 1 < at.1 => self.load(&r, at.0 + 1).await?,
            "previous" => self.load(&r, at.0.saturating_sub(1)).await?,
            "next" => {}
            "seek" => {
                let to = seconds.ok_or_else(|| anyhow!("seek needs seconds"))?;
                self.soap(&r.transport, AVT, "Seek", &[("InstanceID", "0".into()), ("Unit", "REL_TIME".into()), ("Target", hhmmss(to))]).await?;
            }
            "volume" => {
                let v = volume.ok_or_else(|| anyhow!("volume needs a level"))?.min(100);
                let url = r.volume.clone().ok_or_else(|| anyhow!("{} does not let its volume be set", r.name))?;
                self.soap(&url, RC, "SetVolume", &[("InstanceID", "0".into()), ("Channel", "Master".into()), ("DesiredVolume", v.to_string())]).await?;
                if let Some(s) = self.music.cast.sessions.lock().get_mut(renderer) {
                    s.volume = Some(v);
                }
            }
            "stop" => {
                held(self, true);
                let _ = self.soap(&r.transport, AVT, "Stop", &[("InstanceID", "0".into())]).await;
                self.music.cast.sessions.lock().remove(renderer);
                return Ok(None);
            }
            other => bail!("unknown action {other}"),
        }
        Ok(self.cast_state(renderer))
    }

    /// Keep up with every device something is playing on: where it is in the track, and on
    /// to the next track when one ends.
    fn follow_casts(&self) {
        let app = self.clone();
        self.music.cast.following.call_once(move || {
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    let ids: Vec<String> = app.music.cast.sessions.lock().keys().cloned().collect();
                    for id in ids {
                        if let Err(e) = app.follow_cast(&id).await {
                            tracing::debug!(renderer = %id, error = %e, "could not ask a player how it is getting on");
                        }
                    }
                }
            });
        });
    }

    async fn follow_cast(&self, id: &str) -> Result<()> {
        let r = self.renderer(id)?;
        let turn = self.music.cast.turn(&r.id);
        let _turn = turn.lock().await;
        if self.cast_state(id).is_none() {
            return Ok(());
        }
        let info = self.soap(&r.transport, AVT, "GetTransportInfo", &[("InstanceID", "0".into())]).await?;
        let pos = self.soap(&r.transport, AVT, "GetPositionInfo", &[("InstanceID", "0".into())]).await?;
        let state = tag(&info, "CurrentTransportState").unwrap_or_default();
        let uri = tag(&pos, "TrackURI").unwrap_or_default();
        // Which of Spool's tracks it is playing, from the link it was given.
        let playing: Option<i64> = uri.contains("/cast/").then(|| uri.rsplit('/').next().and_then(|f| f.split('.').next()).and_then(|n| n.parse().ok())).flatten();
        let (advance, give_next) = {
            let mut sessions = self.music.cast.sessions.lock();
            let Some(s) = sessions.get_mut(id) else { return Ok(()) };
            s.position = seconds(&tag(&pos, "RelTime").unwrap_or_default());
            let d = seconds(&tag(&pos, "TrackDuration").unwrap_or_default());
            if d > 0.0 {
                s.duration = d;
            }
            // Played on to the next by itself: catch up, and hand it the one after. Only the next
            // is looked for, so a track that comes twice in the queue does not send it back.
            let mut give_next = false;
            if let Some(p) = playing {
                if s.queue.get(s.at) != Some(&p) && s.queue.get(s.at + 1) == Some(&p) {
                    s.at += 1;
                    s.next_given = None;
                    give_next = true;
                }
            }
            if playing.is_none() && !uri.is_empty() && state == "PLAYING" {
                // Something else is playing there now; Spool lets go.
                sessions.remove(id);
                return Ok(());
            }
            s.state = match state.as_str() {
                "PLAYING" => "playing",
                "PAUSED_PLAYBACK" => "paused",
                "TRANSITIONING" => "loading",
                _ if s.loaded_at.elapsed() < Duration::from_secs(8) => "loading",
                _ => "stopped",
            }
            .into();
            // Stopped on the track Spool thinks it is on; one just moved on to is not ended, as
            // the two answers may have been given either side of the change.
            let on_current = playing.is_none_or(|p| s.queue.get(s.at) == Some(&p));
            let ended = s.state == "stopped" && !s.held && !give_next && on_current;
            (ended.then_some(s.at + 1).filter(|n| *n < s.queue.len()), give_next.then_some(s.at))
        };
        if let Some(n) = advance {
            self.load(&r, n).await?;
        } else if let Some(at) = give_next {
            let next = self.cast_state(id).and_then(|s| s.queue.get(at + 1).copied());
            let lib = self.music.library();
            if let Some(t) = next.and_then(|n| lib.track(n).cloned()) {
                let (url, meta) = self.handed(&r, &t)?;
                if self.soap(&r.transport, AVT, "SetNextAVTransportURI", &[("InstanceID", "0".into()), ("NextURI", url), ("NextURIMetaData", meta)]).await.is_ok() {
                    if let Some(s) = self.music.cast.sessions.lock().get_mut(id) {
                        s.next_given = Some(t.id);
                    }
                }
            }
        }
        Ok(())
    }

    /// A track for a device that came with a link from [`Cast::link`].
    pub async fn cast_track(&self, token: &str, file: &str, headers: &axum::http::HeaderMap, head: bool) -> axum::response::Response {
        use axum::response::IntoResponse;
        let id: Option<i64> = file.split('.').next().and_then(|n| n.parse().ok());
        let now = crate::db::now() as u64;
        let Some(t) = id.filter(|id| self.music.cast.check(token, *id, now)).and_then(|id| self.music.library().track(id).cloned()) else {
            return axum::http::StatusCode::NOT_FOUND.into_response();
        };
        if !subsonic::music_file_ok(self, &t.path).await {
            return axum::http::StatusCode::NOT_FOUND.into_response();
        }
        subsonic::stream(self, &t, subsonic::Transcode::asked(Some("raw"), 0, 0), headers, head).await
    }

    /// An album's picture for a device's screen.
    pub async fn cast_art(&self, token: &str, file: &str) -> axum::response::Response {
        use axum::response::IntoResponse;
        let id: Option<i64> = file.split('.').next().and_then(|n| n.parse().ok());
        let now = crate::db::now() as u64;
        match id.filter(|id| *id > 0 && self.music.cast.check(token, -id, now)) {
            Some(id) => subsonic::cover(self, id, 600).await,
            None => axum::http::StatusCode::NOT_FOUND.into_response(),
        }
    }
}

impl Cast {
    /// A link to an album's picture; signed apart from tracks by its id being negative.
    fn art_link(&self, album: i64, until: u64) -> String {
        let sig = ring::hmac::sign(&self.key, format!("{}:{until}", -album).as_bytes());
        format!("/cast-art/{until}.{}/{album}.jpg", b64(&sig.as_ref()[..16]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_names_one_track_until_it_runs_out() {
        let c = Cast::default();
        let t = Track { id: 7, suffix: "flac".into(), ..Default::default() };
        let link = c.link(&t, 1_000);
        let token = link.split('/').nth(2).unwrap();
        assert!(link.ends_with("/7.flac"));
        assert!(c.check(token, 7, 900));
        assert!(!c.check(token, 8, 900), "another track");
        assert!(!c.check(token, 7, 1_001), "run out");
        assert!(!Cast::default().check(token, 7, 900), "signed by another Spool");
    }

    #[test]
    fn a_device_description_is_read() {
        let xml = r#"<root><URLBase>http://192.168.69.50:49152/</URLBase><device><friendlyName>Living Room &amp; Den</friendlyName><modelName>WiiM Amp</modelName><UDN>uuid:abc</UDN><serviceList><service><serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType><controlURL>/upnp/control/rendercontrol1</controlURL></service><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>/upnp/control/rendertransport1</controlURL></service></serviceList></device></root>"#;
        assert_eq!(tag(xml, "friendlyName").as_deref(), Some("Living Room & Den"));
        let s = services(xml);
        assert_eq!(s.len(), 2);
        assert_eq!(s[1], ("urn:schemas-upnp-org:service:AVTransport:1".into(), "/upnp/control/rendertransport1".into()));
        assert_eq!(tag(r#"<s:Envelope><s:Body><u:GetPositionInfoResponse><Track>1</Track><RelTime>0:03:07</RelTime></u:GetPositionInfoResponse></s:Body></s:Envelope>"#, "RelTime").as_deref(), Some("0:03:07"));
    }

    #[test]
    fn a_device_is_asked_at_its_own_address_alone() {
        let ip: IpAddr = "192.168.69.64".parse().unwrap();
        let u = |s: &str| reqwest::Url::parse(s).unwrap();
        assert!(on_device(&u("http://192.168.69.64:49152/upnp/control/rendertransport1"), ip));
        assert!(!on_device(&u("http://127.0.0.1:7979/api/settings"), ip), "this machine");
        assert!(!on_device(&u("http://192.168.69.97/"), ip), "another machine");
        assert!(!on_device(&u("http://localhost:49152/"), ip), "a name");
        assert!(!on_device(&u("file:///etc/passwd"), ip));
        let c = Cast::default();
        assert!(!c.check("1.x", i64::MIN, 0), "a crafted id is refused, not negated");
    }

    #[test]
    fn times_go_both_ways() {
        assert_eq!(hhmmss(3725.4), "1:02:05");
        assert_eq!(seconds("1:02:05"), 3725.0);
        assert_eq!(seconds("0:00:07.250"), 7.0);
        assert_eq!(seconds("NOT_IMPLEMENTED"), 0.0);
    }
}
