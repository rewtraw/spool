//! The OpenSubsonic API, for music players: Feishin and Supersonic on a Mac, Amperfy, play:Sub
//! and Narjo on a phone. Served at `/rest/` on Spool's own port.
//!
//! A player signs in as one of Spool's people, with that person's name and password. Subsonic
//! has two ways to send them: the password itself (or hex-encoded), which is checked as the
//! web app checks it, or a salted MD5 token, which needs the password kept readable. Spool
//! keeps only a hash, so a player that sends tokens is told to use its plain-password setting
//! (often called "legacy authentication"); a person with no password needs neither.
//!
//! Files are sent as they are unless the player asks for a lower bit rate or another format;
//! then ffmpeg makes MP3, Opus or AAC on the way out.

use super::{Album, Artist, Library, Mark, Playlist, Track};
use crate::app::App;
use crate::db::now;
use crate::models::Account;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

const API_VERSION: &str = "1.16.1";
/// How long a checked password is taken as right without checking again.
const SIGNED_FOR: std::time::Duration = std::time::Duration::from_secs(15 * 60);

pub fn routes(app: App) -> Router {
    Router::new().route("/rest/{method}", get(handle).post(handle)).with_state(app)
}

/// Query and form parameters by name; most may be given more than once.
#[derive(Default)]
pub struct Params(HashMap<String, Vec<String>>);

impl Params {
    fn add(&mut self, s: &str) {
        for pair in s.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |s: &str| urlencoding::decode(&s.replace('+', " ")).map(|c| c.into_owned()).unwrap_or_else(|_| s.to_string());
            self.0.entry(decode(k)).or_default().push(decode(v));
        }
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).and_then(|v| v.first()).map(|s| s.as_str())
    }
    fn all(&self, k: &str) -> Vec<&str> {
        self.0.get(k).map(|v| v.iter().map(|s| s.as_str()).collect()).unwrap_or_default()
    }
    fn num(&self, k: &str, default: i64) -> i64 {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(default)
    }
    fn flag(&self, k: &str, default: bool) -> bool {
        self.get(k).map(|v| v == "true" || v == "1").unwrap_or(default)
    }
}

pub enum Fail {
    Missing(&'static str),
    WrongPassword,
    TokensUnsupported,
    NotAllowed,
    NotFound(&'static str),
    Other(String),
}

impl Fail {
    fn code(&self) -> (u32, String) {
        match self {
            Fail::Missing(p) => (10, format!("Required parameter is missing: {p}")),
            Fail::WrongPassword => (40, "Wrong username or password".into()),
            Fail::TokensUnsupported => (41, "Spool does not keep passwords readable, so it cannot check a token. Turn on sending the plain password (often called legacy authentication) in the app.".into()),
            Fail::NotAllowed => (50, "This person may not do that".into()),
            Fail::NotFound(what) => (70, format!("The {what} was not found")),
            Fail::Other(m) => (0, m.clone()),
        }
    }
}

impl From<anyhow::Error> for Fail {
    fn from(e: anyhow::Error) -> Fail {
        Fail::Other(format!("{e:#}"))
    }
}

fn base() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("status".into(), json!("ok"));
    m.insert("version".into(), json!(API_VERSION));
    m.insert("type".into(), json!("spool"));
    m.insert("serverVersion".into(), json!(env!("CARGO_PKG_VERSION")));
    m.insert("openSubsonic".into(), json!(true));
    m
}

fn respond(p: &Params, body: Map<String, Value>) -> Response {
    let body = Value::Object(body);
    match p.get("f").unwrap_or("xml") {
        "json" => ([(header::CONTENT_TYPE, "application/json")], json!({"subsonic-response": body}).to_string()).into_response(),
        "jsonp" => {
            let cb: String = p.get("callback").unwrap_or("callback").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.').collect();
            ([(header::CONTENT_TYPE, "application/javascript")], format!("{cb}({});", json!({"subsonic-response": body}))).into_response()
        }
        _ => ([(header::CONTENT_TYPE, "application/xml; charset=utf-8")], super::xml::document(&body)).into_response(),
    }
}

fn ok(p: &Params, v: Value) -> Response {
    let mut m = base();
    if let Value::Object(extra) = v {
        m.extend(extra);
    }
    respond(p, m)
}

fn failed(p: &Params, f: Fail) -> Response {
    let (code, message) = f.code();
    let mut m = base();
    m.insert("status".into(), json!("failed"));
    m.insert("error".into(), json!({"code": code, "message": message}));
    respond(p, m)
}

async fn handle(State(app): State<App>, Path(method): Path<String>, req: Request) -> Response {
    let method = method.trim_end_matches(".view").to_string();
    let mut p = Params::default();
    p.add(req.uri().query().unwrap_or(""));
    let headers = req.headers().clone();
    let head = req.method() == Method::HEAD;
    let form = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"));
    if req.method() == Method::POST && form {
        if let Ok(bytes) = axum::body::to_bytes(req.into_body(), 1 << 20).await {
            p.add(&String::from_utf8_lossy(&bytes));
        }
    }
    let who = match sign_in(&app, &p).await {
        Ok(a) => a,
        Err(f) => return failed(&p, f),
    };
    match answer(&app, &method, &p, &who, &headers, head).await {
        Ok(r) => r,
        Err(f) => {
            if !matches!(f, Fail::NotFound(_)) {
                tracing::debug!(method, error = f.code().1, "a music player was refused");
            }
            failed(&p, f)
        }
    }
}

async fn sign_in(app: &App, p: &Params) -> Result<Account, Fail> {
    let name = p.get("u").ok_or(Fail::Missing("u"))?;
    let accounts = app.db.accounts()?;
    let who = accounts.into_iter().find(|a| a.enabled && a.name.eq_ignore_ascii_case(name));
    let Some(who) = who else {
        return Err(Fail::WrongPassword);
    };
    if who.password.is_empty() {
        return Ok(who);
    }
    let given = match (p.get("p"), p.get("t")) {
        (Some(pw), _) => match pw.strip_prefix("enc:") {
            Some(hex) => decode_hex(hex).ok_or(Fail::WrongPassword)?,
            None => pw.to_string(),
        },
        (None, Some(_)) => return Err(Fail::TokensUnsupported),
        (None, None) => return Err(Fail::Missing("p")),
    };
    let digest: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, given.as_bytes()).as_ref().try_into().unwrap_or([0; 32]);
    let key = (who.id, who.password.clone(), digest);
    if app.music.signed.lock().get(&key).is_some_and(|at| at.elapsed() < SIGNED_FOR) {
        return Ok(who);
    }
    let stored = who.password.clone();
    let right = tokio::task::spawn_blocking(move || crate::settings::verify_password(&stored, &given)).await.unwrap_or(false);
    if !right {
        return Err(Fail::WrongPassword);
    }
    let mut signed = app.music.signed.lock();
    signed.retain(|_, at| at.elapsed() < SIGNED_FOR);
    signed.insert(key, std::time::Instant::now());
    Ok(who)
}

fn decode_hex(s: &str) -> Option<String> {
    if s.len() % 2 != 0 {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect();
    String::from_utf8(bytes?).ok()
}

/// Ids as players see them: "ar-3", "al-12", "tr-140", "pl-2".
pub fn id(kind: &str, n: i64) -> String {
    format!("{kind}-{n}")
}

fn parse_id(s: &str) -> Option<(&str, i64)> {
    let (k, n) = s.split_once('-')?;
    Some((k, n.parse().ok()?))
}

fn iso(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// What one person has marked, looked up by item id.
struct Marks(HashMap<String, Mark>);

impl Marks {
    fn add(&self, item: &str, v: &mut Map<String, Value>) {
        if let Some(m) = self.0.get(item) {
            if m.starred_at > 0 {
                v.insert("starred".into(), json!(iso(m.starred_at)));
            }
            if m.rating > 0 {
                v.insert("userRating".into(), json!(m.rating));
            }
            if m.plays > 0 {
                v.insert("playCount".into(), json!(m.plays));
                v.insert("played".into(), json!(iso(m.played_at)));
            }
        }
    }
    fn get(&self, item: &str) -> Mark {
        self.0.get(item).cloned().unwrap_or_default()
    }
}

/// The part of a song's path players show: below the music folder.
fn shown_path(t: &Track, roots: &[String]) -> String {
    roots.iter().find_map(|r| std::path::Path::new(&t.path).strip_prefix(r).ok()).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| t.path.clone())
}

fn song(lib: &Library, t: &Track, marks: &Marks, roots: &[String]) -> Value {
    let album = lib.album(t.album_id);
    let mut v = Map::new();
    let artist_id = lib.artists.iter().find(|a| super::scan::artist_key(&a.name) == super::scan::artist_key(&t.artist)).map(|a| a.id);
    v.insert("id".into(), json!(id("tr", t.id)));
    v.insert("parent".into(), json!(id("al", t.album_id)));
    v.insert("isDir".into(), json!(false));
    v.insert("title".into(), json!(t.title));
    v.insert("album".into(), json!(album.map(|a| a.title.as_str()).unwrap_or("")));
    v.insert("albumId".into(), json!(id("al", t.album_id)));
    v.insert("artist".into(), json!(t.artist));
    v.insert("displayArtist".into(), json!(t.artist));
    if let Some(a) = artist_id {
        v.insert("artistId".into(), json!(id("ar", a)));
        v.insert("artists".into(), json!([{"id": id("ar", a), "name": t.artist}]));
    }
    if let Some(a) = album {
        v.insert("displayAlbumArtist".into(), json!(a.artist));
        v.insert("albumArtists".into(), json!([{"id": id("ar", a.artist_id), "name": a.artist}]));
    }
    if t.track > 0 {
        v.insert("track".into(), json!(t.track));
    }
    v.insert("discNumber".into(), json!(t.disc.max(1)));
    if t.year > 0 {
        v.insert("year".into(), json!(t.year));
    }
    if !t.genre.is_empty() {
        v.insert("genre".into(), json!(t.genre));
        v.insert("genres".into(), json!([{"name": t.genre}]));
    }
    if album.is_some_and(|a| !a.cover.is_empty()) {
        v.insert("coverArt".into(), json!(id("al", t.album_id)));
    }
    v.insert("size".into(), json!(t.size));
    v.insert("contentType".into(), json!(t.content_type()));
    v.insert("suffix".into(), json!(t.suffix));
    v.insert("duration".into(), json!(t.seconds.round() as i64));
    v.insert("bitRate".into(), json!(t.bitrate));
    if t.bit_depth > 0 {
        v.insert("bitDepth".into(), json!(t.bit_depth));
    }
    if t.sample_rate > 0 {
        v.insert("samplingRate".into(), json!(t.sample_rate));
    }
    if t.channels > 0 {
        v.insert("channelCount".into(), json!(t.channels));
    }
    v.insert("path".into(), json!(shown_path(t, roots)));
    v.insert("type".into(), json!("music"));
    v.insert("mediaType".into(), json!("song"));
    v.insert("created".into(), json!(iso(t.added_at)));
    marks.add(&id("tr", t.id), &mut v);
    Value::Object(v)
}

/// An album as ID3-style browsing shows it (getAlbum, getAlbumList2, search3).
fn album(lib: &Library, a: &Album, marks: &Marks) -> Map<String, Value> {
    let (count, seconds, _) = lib.album_totals(a.id);
    let mut v = Map::new();
    v.insert("id".into(), json!(id("al", a.id)));
    v.insert("name".into(), json!(a.title));
    v.insert("artist".into(), json!(a.artist));
    v.insert("artistId".into(), json!(id("ar", a.artist_id)));
    v.insert("displayArtist".into(), json!(a.artist));
    v.insert("artists".into(), json!([{"id": id("ar", a.artist_id), "name": a.artist}]));
    if !a.cover.is_empty() {
        v.insert("coverArt".into(), json!(id("al", a.id)));
    }
    v.insert("songCount".into(), json!(count));
    v.insert("duration".into(), json!(seconds.round() as i64));
    v.insert("created".into(), json!(iso(a.added_at)));
    if a.year > 0 {
        v.insert("year".into(), json!(a.year));
    }
    if !a.genre.is_empty() {
        v.insert("genre".into(), json!(a.genre));
        v.insert("genres".into(), json!([{"name": a.genre}]));
    }
    if !a.mbid.is_empty() {
        v.insert("musicBrainzId".into(), json!(a.mbid));
    }
    v.insert("isCompilation".into(), json!(a.artist == super::scan::VARIOUS));
    v.insert("mediaType".into(), json!("album"));
    marks.add(&id("al", a.id), &mut v);
    v
}

/// An album as folder browsing shows it (getMusicDirectory, getAlbumList, search2).
fn album_dir(lib: &Library, a: &Album, marks: &Marks) -> Value {
    let mut v = album(lib, a, marks);
    v.insert("parent".into(), json!(id("ar", a.artist_id)));
    v.insert("isDir".into(), json!(true));
    v.insert("title".into(), json!(a.title));
    v.insert("album".into(), json!(a.title));
    Value::Object(v)
}

fn artist(lib: &Library, a: &Artist, marks: &Marks) -> Map<String, Value> {
    let albums: Vec<&Album> = lib.albums_of(a.id).collect();
    let mut v = Map::new();
    v.insert("id".into(), json!(id("ar", a.id)));
    v.insert("name".into(), json!(a.name));
    v.insert("albumCount".into(), json!(albums.len()));
    if let Some(first) = albums.iter().rev().find(|a| !a.cover.is_empty()) {
        v.insert("coverArt".into(), json!(id("al", first.id)));
    }
    v.insert("sortName".into(), json!(sort_name(&a.name)));
    marks.add(&id("ar", a.id), &mut v);
    v
}

const ARTICLES: &[&str] = &["The ", "A ", "An ", "El ", "La ", "Los ", "Las ", "Le ", "Les "];

fn sort_name(name: &str) -> String {
    let n = ARTICLES.iter().find_map(|a| name.strip_prefix(a)).unwrap_or(name);
    deunicode::deunicode(n).to_lowercase()
}

/// Artists grouped by first letter, as getArtists and getIndexes give them.
fn index(lib: &Library, marks: &Marks, short: bool) -> Vec<Value> {
    let mut artists: Vec<&Artist> = lib.artists.iter().collect();
    artists.sort_by_key(|a| sort_name(&a.name));
    let mut out: Vec<(String, Vec<Value>)> = vec![];
    for a in artists {
        let letter = sort_name(&a.name).chars().next().map(|c| if c.is_ascii_alphabetic() { c.to_ascii_uppercase().to_string() } else { "#".into() }).unwrap_or("#".into());
        let entry = if short { json!({"id": id("ar", a.id), "name": a.name}) } else { Value::Object(artist(lib, a, marks)) };
        match out.last_mut() {
            Some((l, list)) if *l == letter => list.push(entry),
            _ => out.push((letter, vec![entry])),
        }
    }
    out.into_iter().map(|(name, artist)| json!({"name": name, "artist": artist})).collect()
}

/// Lower case, accents gone: what searches compare.
fn folded(s: &str) -> String {
    deunicode::deunicode(s).to_lowercase()
}

/// Every word of the query is somewhere in the text. An empty query (or `""`, as some
/// players send to fetch everything) matches all.
fn matches(words: &[String], text: &str) -> bool {
    let t = folded(text);
    words.iter().all(|w| t.contains(w.as_str()))
}

fn query_words(q: &str) -> Vec<String> {
    folded(q.trim().trim_matches('"').trim_end_matches('*')).split_whitespace().map(|w| w.to_string()).collect()
}

/// A small random number source; nothing here needs more.
fn shuffle<T>(v: &mut [T]) {
    let mut x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(7) | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

fn page<T: Clone>(v: &[T], offset: i64, size: i64) -> Vec<T> {
    v.iter().skip(offset.max(0) as usize).take(size.clamp(0, 500) as usize).cloned().collect()
}

/// Albums in the order getAlbumList asks for.
fn album_list<'a>(lib: &'a Library, p: &Params, marks: &Marks) -> Result<Vec<&'a Album>, Fail> {
    let mut list: Vec<&Album> = lib.albums.iter().collect();
    let kind = p.get("type").ok_or(Fail::Missing("type"))?;
    let mark = |a: &Album| marks.get(&id("al", a.id));
    match kind {
        "random" => shuffle(&mut list),
        "newest" => list.sort_by_key(|a| (std::cmp::Reverse(a.added_at), std::cmp::Reverse(a.id))),
        "recent" => {
            list.retain(|a| mark(a).played_at > 0);
            list.sort_by_key(|a| std::cmp::Reverse(mark(a).played_at));
        }
        "frequent" => {
            list.retain(|a| mark(a).plays > 0);
            list.sort_by_key(|a| std::cmp::Reverse(mark(a).plays));
        }
        "highest" => {
            list.retain(|a| mark(a).rating > 0);
            list.sort_by_key(|a| std::cmp::Reverse(mark(a).rating));
        }
        "starred" => {
            list.retain(|a| mark(a).starred_at > 0);
            list.sort_by_key(|a| std::cmp::Reverse(mark(a).starred_at));
        }
        "alphabeticalByName" => list.sort_by_key(|a| sort_name(&a.title)),
        "alphabeticalByArtist" => list.sort_by_key(|a| (sort_name(&a.artist), a.year, sort_name(&a.title))),
        "byYear" => {
            let (from, to) = (p.num("fromYear", 0), p.num("toYear", 9999));
            let (lo, hi) = (from.min(to), from.max(to));
            list.retain(|a| (lo..=hi).contains(&(a.year as i64)));
            list.sort_by_key(|a| a.year);
            if from > to {
                list.reverse();
            }
        }
        "byGenre" => {
            let g = p.get("genre").ok_or(Fail::Missing("genre"))?;
            list.retain(|a| a.genre.eq_ignore_ascii_case(g) || lib.tracks_of(a.id).any(|t| t.genre.eq_ignore_ascii_case(g)));
            list.sort_by_key(|a| sort_name(&a.title));
        }
        _ => return Err(Fail::Other(format!("unknown list type {kind}"))),
    }
    Ok(page(&list, p.num("offset", 0), p.num("size", 10)))
}

fn genres(lib: &Library) -> Vec<Value> {
    let mut counts: HashMap<&str, (usize, std::collections::HashSet<i64>)> = HashMap::new();
    for t in &lib.tracks {
        if !t.genre.is_empty() {
            let e = counts.entry(&t.genre).or_default();
            e.0 += 1;
            e.1.insert(t.album_id);
        }
    }
    let mut v: Vec<_> = counts.into_iter().map(|(g, (songs, albums))| (g, songs, albums.len())).collect();
    v.sort_by_key(|(g, _, _)| g.to_lowercase());
    v.into_iter().map(|(g, songs, albums)| json!({"value": g, "songCount": songs, "albumCount": albums})).collect()
}

fn playlist(lib: &Library, pl: &Playlist, owner: &str, with_songs: Option<(&Marks, &[String])>) -> Value {
    let tracks: Vec<&Track> = pl.tracks.iter().filter_map(|&t| lib.track(t)).collect();
    let mut v = json!({
        "id": id("pl", pl.id), "name": pl.name, "comment": pl.comment, "owner": owner, "public": pl.public,
        "songCount": tracks.len(), "duration": tracks.iter().map(|t| t.seconds).sum::<f64>().round() as i64,
        "created": iso(pl.created_at), "changed": iso(pl.changed_at),
    });
    if let Some(t) = tracks.first() {
        v["coverArt"] = json!(id("al", t.album_id));
    }
    if let Some((marks, roots)) = with_songs {
        v["entry"] = json!(tracks.iter().map(|t| song(lib, t, marks, roots)).collect::<Vec<_>>());
    }
    v
}

/// Who is listening to what, for getNowPlaying: by account, the track and when.
pub type NowPlaying = parking_lot::Mutex<HashMap<(i64, String), (i64, i64)>>;

async fn answer(app: &App, method: &str, p: &Params, who: &Account, headers: &HeaderMap, head: bool) -> Result<Response, Fail> {
    let lib = app.music.library();
    let marks = Marks(app.db.music_marks(who.id)?);
    let roots: Vec<String> = app.music_roots().into_iter().map(|(r, _)| r).collect();
    let need = |k: &'static str| p.get(k).ok_or(Fail::Missing(k));
    let want = |kind: &'static str, what: &'static str| -> Result<i64, Fail> {
        match parse_id(need("id")?) {
            Some((k, n)) if k == kind => Ok(n),
            _ => Err(Fail::NotFound(what)),
        }
    };
    let songs = |list: Vec<&Track>| list.into_iter().map(|t| song(&lib, t, &marks, &roots)).collect::<Vec<_>>();
    let v = match method {
        "ping" => json!({}),
        "getLicense" => json!({"license": {"valid": true}}),
        "getOpenSubsonicExtensions" => json!({"openSubsonicExtensions": [{"name": "formPost", "versions": [1]}, {"name": "transcodeOffset", "versions": [1]}]}),
        "getMusicFolders" => json!({"musicFolders": {"musicFolder": [{"id": 1, "name": "Music"}]}}),
        "getArtists" => json!({"artists": {"ignoredArticles": "The A An El La Los Las Le Les", "index": index(&lib, &marks, false)}}),
        "getIndexes" => json!({"indexes": {"ignoredArticles": "The A An El La Los Las Le Les", "lastModified": app.music.scan.lock().last_at * 1000, "index": index(&lib, &marks, true)}}),
        "getArtist" => {
            let a = lib.artist(want("ar", "artist")?).ok_or(Fail::NotFound("artist"))?;
            let mut v = artist(&lib, a, &marks);
            v.insert("album".into(), json!(lib.albums_of(a.id).map(|al| Value::Object(album(&lib, al, &marks))).collect::<Vec<_>>()));
            json!({"artist": v})
        }
        "getAlbum" => {
            let a = lib.album(want("al", "album")?).ok_or(Fail::NotFound("album"))?;
            let mut v = album(&lib, a, &marks);
            v.insert("song".into(), json!(songs(lib.tracks_of(a.id).collect())));
            json!({"album": v})
        }
        "getSong" => {
            let t = lib.track(want("tr", "song")?).ok_or(Fail::NotFound("song"))?;
            json!({"song": song(&lib, t, &marks, &roots)})
        }
        "getMusicDirectory" => match parse_id(need("id")?) {
            Some(("ar", n)) => {
                let a = lib.artist(n).ok_or(Fail::NotFound("artist"))?;
                json!({"directory": {"id": id("ar", n), "name": a.name, "child": lib.albums_of(n).map(|al| album_dir(&lib, al, &marks)).collect::<Vec<_>>()}})
            }
            Some(("al", n)) => {
                let a = lib.album(n).ok_or(Fail::NotFound("album"))?;
                json!({"directory": {"id": id("al", n), "parent": id("ar", a.artist_id), "name": a.title, "child": songs(lib.tracks_of(n).collect())}})
            }
            _ => return Err(Fail::NotFound("folder")),
        },
        "getArtistInfo" | "getArtistInfo2" => {
            let key = if method == "getArtistInfo" { "artistInfo" } else { "artistInfo2" };
            json!({key: {}})
        }
        "getAlbumInfo" | "getAlbumInfo2" => json!({"albumInfo": {}}),
        "getAlbumList" | "getAlbumList2" => {
            let list = album_list(&lib, p, &marks)?;
            if method == "getAlbumList" {
                json!({"albumList": {"album": list.into_iter().map(|a| album_dir(&lib, a, &marks)).collect::<Vec<_>>()}})
            } else {
                json!({"albumList2": {"album": list.into_iter().map(|a| Value::Object(album(&lib, a, &marks))).collect::<Vec<_>>()}})
            }
        }
        "getRandomSongs" => {
            let (from, to) = (p.num("fromYear", 0), p.num("toYear", 9999));
            let genre = p.get("genre");
            let mut list: Vec<&Track> = lib.tracks.iter().filter(|t| genre.is_none_or(|g| t.genre.eq_ignore_ascii_case(g)) && (t.year == 0 || (from..=to).contains(&(t.year as i64)))).collect();
            shuffle(&mut list);
            json!({"randomSongs": {"song": songs(page(&list, 0, p.num("size", 10)))}})
        }
        "getSongsByGenre" => {
            let g = need("genre")?;
            let list: Vec<&Track> = lib.tracks.iter().filter(|t| t.genre.eq_ignore_ascii_case(g)).collect();
            json!({"songsByGenre": {"song": songs(page(&list, p.num("offset", 0), p.num("count", 10)))}})
        }
        "getGenres" => json!({"genres": {"genre": genres(&lib)}}),
        "getTopSongs" => {
            let name = folded(need("artist")?);
            let mut list: Vec<&Track> = lib.tracks.iter().filter(|t| folded(&t.artist) == name).collect();
            list.sort_by_key(|t| std::cmp::Reverse(marks.get(&id("tr", t.id)).plays));
            json!({"topSongs": {"song": songs(page(&list, 0, p.num("count", 50)))}})
        }
        "getSimilarSongs" | "getSimilarSongs2" => {
            // Nothing knows what is similar; the same artist, then anything, in a new order.
            let artist = match parse_id(need("id")?) {
                Some(("ar", n)) => lib.artist(n).map(|a| a.name.clone()),
                Some(("al", n)) => lib.album(n).map(|a| a.artist.clone()),
                Some(("tr", n)) => lib.track(n).map(|t| t.artist.clone()),
                _ => None,
            }
            .unwrap_or_default();
            let mut same: Vec<&Track> = lib.tracks.iter().filter(|t| t.artist == artist).collect();
            shuffle(&mut same);
            let key = if method == "getSimilarSongs" { "similarSongs" } else { "similarSongs2" };
            json!({key: {"song": songs(page(&same, 0, p.num("count", 50)))}})
        }
        "search2" | "search3" => {
            let words = query_words(p.get("query").unwrap_or(""));
            let artists: Vec<&Artist> = lib.artists.iter().filter(|a| matches(&words, &a.name)).collect();
            let albums: Vec<&Album> = lib.albums.iter().filter(|a| matches(&words, &format!("{} {}", a.title, a.artist))).collect();
            let tracks: Vec<&Track> = lib.tracks.iter().filter(|t| matches(&words, &format!("{} {} {}", t.title, t.artist, lib.album(t.album_id).map(|a| a.title.as_str()).unwrap_or("")))).collect();
            let artists = page(&artists, p.num("artistOffset", 0), p.num("artistCount", 20));
            let albums = page(&albums, p.num("albumOffset", 0), p.num("albumCount", 20));
            let tracks = page(&tracks, p.num("songOffset", 0), p.num("songCount", 20));
            if method == "search2" {
                json!({"searchResult2": {"artist": artists.iter().map(|a| json!({"id": id("ar", a.id), "name": a.name})).collect::<Vec<_>>(), "album": albums.iter().map(|a| album_dir(&lib, a, &marks)).collect::<Vec<_>>(), "song": songs(tracks)}})
            } else {
                json!({"searchResult3": {"artist": artists.iter().map(|a| Value::Object(artist(&lib, a, &marks))).collect::<Vec<_>>(), "album": albums.iter().map(|a| Value::Object(album(&lib, a, &marks))).collect::<Vec<_>>(), "song": songs(tracks)}})
            }
        }
        "getStarred" | "getStarred2" => {
            let starred = |item: String| marks.get(&item).starred_at > 0;
            let artists: Vec<Value> = lib.artists.iter().filter(|a| starred(id("ar", a.id))).map(|a| Value::Object(artist(&lib, a, &marks))).collect();
            let albums: Vec<Value> = lib.albums.iter().filter(|a| starred(id("al", a.id))).map(|a| if method == "getStarred" { album_dir(&lib, a, &marks) } else { Value::Object(album(&lib, a, &marks)) }).collect();
            let tracks = songs(lib.tracks.iter().filter(|t| starred(id("tr", t.id))).collect());
            let key = if method == "getStarred" { "starred" } else { "starred2" };
            json!({key: {"artist": artists, "album": albums, "song": tracks}})
        }
        "star" | "unstar" => {
            let at = if method == "star" { now() } else { 0 };
            for item in p.all("id").into_iter().chain(p.all("albumId")).chain(p.all("artistId")) {
                if parse_id(item).is_some() {
                    app.db.music_mark(who.id, item, |m| m.starred_at = at)?;
                }
            }
            json!({})
        }
        "setRating" => {
            let item = need("id")?;
            let rating = p.num("rating", 0).clamp(0, 5) as u8;
            app.db.music_mark(who.id, item, |m| m.rating = rating)?;
            json!({})
        }
        "scrobble" => {
            let submission = p.flag("submission", true);
            for item in p.all("id") {
                let Some(("tr", n)) = parse_id(item) else { continue };
                let Some(t) = lib.track(n) else { continue };
                if submission {
                    let at = p.num("time", 0) / 1000;
                    let at = if at > 0 { at } else { now() };
                    for item in [id("tr", t.id), id("al", t.album_id)] {
                        app.db.music_mark(who.id, &item, |m| {
                            m.plays += 1;
                            m.played_at = at;
                        })?;
                    }
                } else {
                    let player = p.get("c").unwrap_or("").to_string();
                    app.music.now_playing.lock().insert((who.id, player), (t.id, now()));
                }
            }
            json!({})
        }
        "getNowPlaying" => {
            let accounts = app.db.accounts()?;
            let mut entries = vec![];
            let fresh: Vec<_> = app.music.now_playing.lock().iter().filter(|(_, (_, at))| now() - at < 30 * 60).map(|(k, v)| (k.clone(), *v)).collect();
            for ((account, player), (track, at)) in fresh {
                let Some(t) = lib.track(track) else { continue };
                let mut v = song(&lib, t, &marks, &roots);
                v["username"] = json!(accounts.iter().find(|a| a.id == account).map(|a| a.name.as_str()).unwrap_or(""));
                v["minutesAgo"] = json!((now() - at) / 60);
                v["playerId"] = json!(0);
                v["playerName"] = json!(player);
                entries.push(v);
            }
            json!({"nowPlaying": {"entry": entries}})
        }
        "getPlaylists" => {
            let accounts = app.db.accounts()?;
            let owner = |id: i64| accounts.iter().find(|a| a.id == id).map(|a| a.name.clone()).unwrap_or_default();
            let lists = app.db.music_playlists(who.id)?;
            json!({"playlists": {"playlist": lists.iter().map(|pl| playlist(&lib, pl, &owner(pl.account_id), None)).collect::<Vec<_>>()}})
        }
        "getPlaylist" => {
            let n = want("pl", "playlist")?;
            let pl = app.db.music_playlists(who.id)?.into_iter().find(|pl| pl.id == n).ok_or(Fail::NotFound("playlist"))?;
            let owner = if pl.account_id == who.id { who.name.clone() } else { app.db.accounts()?.into_iter().find(|a| a.id == pl.account_id).map(|a| a.name).unwrap_or_default() };
            json!({"playlist": playlist(&lib, &pl, &owner, Some((&marks, &roots)))})
        }
        "createPlaylist" | "updatePlaylist" => {
            let ids = |k: &str| -> Vec<i64> { p.all(k).into_iter().filter_map(|s| match parse_id(s) { Some(("tr", n)) => Some(n), _ => None }).collect() };
            let existing = match p.get("playlistId").and_then(parse_id) {
                Some(("pl", n)) => Some(app.db.music_playlists(who.id)?.into_iter().find(|pl| pl.id == n).ok_or(Fail::NotFound("playlist"))?),
                _ => None,
            };
            if existing.as_ref().is_some_and(|pl| pl.account_id != who.id) {
                return Err(Fail::NotAllowed);
            }
            let mut pl = match (method, existing) {
                ("createPlaylist", Some(mut pl)) => {
                    pl.tracks = ids("songId");
                    pl
                }
                ("createPlaylist", None) => Playlist { account_id: who.id, name: need("name")?.to_string(), tracks: ids("songId"), ..Default::default() },
                (_, Some(mut pl)) => {
                    if let Some(n) = p.get("name") {
                        pl.name = n.to_string();
                    }
                    if let Some(c) = p.get("comment") {
                        pl.comment = c.to_string();
                    }
                    if let Some(b) = p.get("public") {
                        pl.public = b == "true";
                    }
                    let mut remove: Vec<usize> = p.all("songIndexToRemove").into_iter().filter_map(|s| s.parse().ok()).collect();
                    remove.sort_unstable();
                    for i in remove.into_iter().rev() {
                        if i < pl.tracks.len() {
                            pl.tracks.remove(i);
                        }
                    }
                    pl.tracks.extend(ids("songIdToAdd"));
                    pl
                }
                (_, None) => return Err(Fail::Missing("playlistId")),
            };
            pl.id = app.db.save_music_playlist(&pl)?;
            if method == "createPlaylist" {
                let pl = app.db.music_playlists(who.id)?.into_iter().find(|x| x.id == pl.id).unwrap_or(pl);
                json!({"playlist": playlist(&lib, &pl, &who.name, Some((&marks, &roots)))})
            } else {
                json!({})
            }
        }
        "deletePlaylist" => {
            let n = want("pl", "playlist")?;
            let pl = app.db.music_playlists(who.id)?.into_iter().find(|pl| pl.id == n).ok_or(Fail::NotFound("playlist"))?;
            if pl.account_id != who.id {
                return Err(Fail::NotAllowed);
            }
            app.db.delete_music_playlist(n)?;
            json!({})
        }
        "getUser" => json!({"user": user(who)}),
        "getUsers" => json!({"users": {"user": [user(who)]}}),
        "getScanStatus" => {
            let s = app.music.scan.lock().clone();
            json!({"scanStatus": {"scanning": s.scanning, "count": if s.scanning { s.read } else { lib.tracks.len() }}})
        }
        "startScan" => {
            if !who.manages {
                return Err(Fail::NotAllowed);
            }
            let a = app.clone();
            tokio::spawn(async move {
                a.run_task("music").await;
            });
            json!({"scanStatus": {"scanning": true, "count": 0}})
        }
        "getPlayQueue" | "savePlayQueue" => json!({}),
        "getLyrics" => json!({"lyrics": {}}),
        "getLyricsBySongId" => json!({"lyricsList": {}}),
        "getInternetRadioStations" => json!({"internetRadioStations": {}}),
        "getPodcasts" => json!({"podcasts": {}}),
        "getNewestPodcasts" => json!({"newestPodcasts": {}}),
        "getBookmarks" => json!({"bookmarks": {}}),
        "getShares" => json!({"shares": {}}),
        "getChatMessages" => json!({"chatMessages": {}}),
        "getVideos" => json!({"videos": {}}),
        "stream" | "download" => {
            let t = lib.track(want("tr", "song")?).ok_or(Fail::NotFound("song"))?.clone();
            if method == "download" {
                return Ok(send_original(&t, headers, head, true).await);
            }
            let ask = Transcode::asked(p.get("format"), p.num("maxBitRate", 0), p.num("timeOffset", 0));
            return Ok(stream(app, &t, ask, headers, head).await);
        }
        "getCoverArt" => {
            let album_id = match parse_id(need("id")?) {
                Some(("al", n)) => Some(n),
                Some(("tr", n)) => lib.track(n).map(|t| t.album_id),
                Some(("ar", n)) => lib.albums_of(n).last().map(|a| a.id),
                Some(("pl", n)) => app.db.music_playlists(who.id)?.into_iter().find(|pl| pl.id == n).and_then(|pl| pl.tracks.first().and_then(|&t| lib.track(t)).map(|t| t.album_id)),
                _ => None,
            };
            let size = p.num("size", 0).clamp(0, 2000) as u32;
            return Ok(match album_id {
                Some(n) => cover(app, n, size).await,
                None => StatusCode::NOT_FOUND.into_response(),
            });
        }
        "getAvatar" => return Ok(StatusCode::NOT_FOUND.into_response()),
        _ => {
            tracing::info!(method, "a music player asked for something Spool does not offer");
            return Err(Fail::Other(format!("{method} is not offered")));
        }
    };
    Ok(ok(p, v))
}

fn user(who: &Account) -> Value {
    json!({
        "username": who.name, "scrobblingEnabled": true, "adminRole": who.manages, "settingsRole": false, "downloadRole": true,
        "uploadRole": false, "playlistRole": true, "coverArtRole": true, "commentRole": false, "podcastRole": false,
        "streamRole": true, "jukeboxRole": false, "shareRole": false, "videoConversionRole": false, "folder": [1],
    })
}

/// What a player asked to be sent instead of the file as it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transcode {
    pub format: Option<&'static str>,
    pub kbps: u32,
    pub offset: u32,
}

impl Transcode {
    pub fn asked(format: Option<&str>, max_kbps: i64, offset: i64) -> Transcode {
        let format = match format {
            Some("mp3") => Some("mp3"),
            Some("opus") | Some("ogg") => Some("opus"),
            Some("aac") | Some("m4a") => Some("aac"),
            _ => None,
        };
        Transcode { format, kbps: max_kbps.clamp(0, 320) as u32, offset: offset.max(0) as u32 }
    }

    /// The format and bit rate to make, or none to send the file as it is.
    pub fn plan(&self, t: &Track) -> Option<(&'static str, u32)> {
        let lower = self.kbps > 0 && (t.bitrate == 0 || self.kbps < t.bitrate);
        let other = self.format.is_some_and(|f| f != t.suffix.as_str());
        if !lower && !other && self.offset == 0 {
            return None;
        }
        let format = self.format.unwrap_or(if t.suffix == "opus" { "opus" } else { "mp3" });
        let kbps = if self.kbps > 0 { self.kbps } else if format == "opus" { 160 } else { 256 };
        Some((format, kbps.max(32)))
    }
}

async fn send_original(t: &Track, headers: &HeaderMap, head: bool, attachment: bool) -> Response {
    let mut r = crate::serve::send_file(std::path::Path::new(&t.path), headers, head).await;
    if let Ok(v) = t.content_type().parse() {
        r.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    if attachment {
        let name = std::path::Path::new(&t.path).file_name().and_then(|n| n.to_str()).unwrap_or("track");
        let ascii: String = name.chars().map(|c| if c.is_ascii() && c != '"' { c } else { '_' }).collect();
        if let Ok(v) = format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{}", urlencoding::encode(name)).parse() {
            r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
    }
    r
}

/// A track as it is, or made smaller by ffmpeg on the way out.
pub async fn stream(app: &App, t: &Track, ask: Transcode, headers: &HeaderMap, head: bool) -> Response {
    let Some((format, kbps)) = ask.plan(t) else {
        return send_original(t, headers, head, false).await;
    };
    let Some(tool) = app.ffmpeg_path() else {
        return send_original(t, headers, head, false).await;
    };
    let (codec, container, mime) = match format {
        "opus" => ("libopus", "ogg", "audio/ogg"),
        "aac" => ("aac", "adts", "audio/aac"),
        _ => ("libmp3lame", "mp3", "audio/mpeg"),
    };
    if head {
        return ([(header::CONTENT_TYPE, mime)], Body::empty()).into_response();
    }
    let mut cmd = tokio::process::Command::new(tool);
    cmd.args(["-v", "error", "-nostdin"]);
    if ask.offset > 0 {
        cmd.args(["-ss", &ask.offset.to_string()]);
    }
    cmd.arg("-i").arg(&t.path).args(["-map", "0:a:0", "-vn", "-c:a", codec, "-b:a", &format!("{kbps}k"), "-f", container, "pipe:1"]);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "ffmpeg would not start for a music stream");
            return send_original(t, headers, head, false).await;
        }
    };
    let Some(mut out) = child.stdout.take() else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    let (mut writer, reader) = tokio::io::duplex(256 << 10);
    // The player hanging up drops the reader; the copy then fails and ffmpeg is stopped.
    tokio::spawn(async move {
        let _ = tokio::io::copy(&mut out, &mut writer).await;
        let _ = child.kill().await;
    });
    let mut r = Body::from_stream(tokio_util::io::ReaderStream::new(reader)).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, mime.parse().unwrap());
    r
}

/// An album's picture: the image file beside it, or the one inside its first track; made
/// `size` pixels across when asked, and kept so it is made once.
pub async fn cover(app: &App, album_id: i64, size: u32) -> Response {
    let lib = app.music.library();
    let Some(album) = lib.album(album_id) else { return StatusCode::NOT_FOUND.into_response() };
    if album.cover.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let source = if album.cover == super::scan::EMBEDDED { String::new() } else { album.cover.clone() };
    let first = lib.tracks_of(album_id).map(|t| t.path.clone()).find(|p| source.is_empty() && has_picture(std::path::Path::new(p))).or_else(|| lib.tracks_of(album_id).next().map(|t| t.path.clone()));
    let cache = app.data_dir.join("music-art");
    let stamp = std::fs::metadata(if source.is_empty() { first.clone().unwrap_or_default() } else { source.clone() })
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let made = tokio::task::spawn_blocking(move || -> Option<(Vec<u8>, &'static str)> {
        let cached = cache.join(format!("{album_id}-{size}-{stamp}.jpg"));
        if size > 0 {
            if let Ok(b) = std::fs::read(&cached) {
                return Some((b, "image/jpeg"));
            }
        }
        let (bytes, mime) = if !source.is_empty() {
            let mime = match source.rsplit('.').next().map(|e| e.to_ascii_lowercase()).as_deref() {
                Some("png") => "image/png",
                Some("webp") => "image/webp",
                _ => "image/jpeg",
            };
            (std::fs::read(&source).ok()?, mime)
        } else {
            embedded_picture(std::path::Path::new(&first?))?
        };
        if size == 0 {
            return Some((bytes, mime));
        }
        let img = image::load_from_memory(&bytes).ok()?;
        let small = if img.width() > size || img.height() > size { img.thumbnail(size, size) } else { img };
        let mut out = std::io::Cursor::new(vec![]);
        small.to_rgb8().write_to(&mut out, image::ImageFormat::Jpeg).ok()?;
        let out = out.into_inner();
        let _ = std::fs::create_dir_all(&cache);
        // Older sizes of this album's picture are left for housekeeping; they are small.
        let _ = std::fs::write(&cached, &out);
        Some((out, "image/jpeg"))
    })
    .await
    .ok()
    .flatten();
    match made {
        Some((bytes, mime)) => ([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "private, max-age=86400")], bytes).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Whether a track carries a picture inside it.
pub fn has_picture(path: &std::path::Path) -> bool {
    use lofty::prelude::*;
    let Ok(f) = lofty::read_from_path(path) else { return false };
    f.tags().iter().any(|t| !t.pictures().is_empty())
}

fn embedded_picture(path: &std::path::Path) -> Option<(Vec<u8>, &'static str)> {
    use lofty::prelude::*;
    let f = lofty::read_from_path(path).ok()?;
    let tag = f.tags().iter().find(|t| !t.pictures().is_empty())?;
    let pics = tag.pictures();
    let pic = pics.iter().find(|p| p.pic_type() == lofty::picture::PictureType::CoverFront).or(pics.first())?;
    let mime = match pic.mime_type() {
        Some(lofty::picture::MimeType::Png) => "image/png",
        _ => "image/jpeg",
    };
    Some((pic.data().to_vec(), mime))
}

/// The library and one person's marks, for the web app.
pub fn web_album(lib: &Library, a: &Album) -> Value {
    let (count, seconds, bytes) = lib.album_totals(a.id);
    let first = lib.tracks_of(a.id).next();
    json!({
        "id": a.id, "title": a.title, "artist": a.artist, "artist_id": a.artist_id, "year": a.year, "genre": a.genre,
        "tracks": count, "seconds": seconds.round(), "bytes": bytes, "added_at": a.added_at,
        "has_cover": !a.cover.is_empty(), "format": first.map(|t| t.suffix.clone()), "bit_depth": first.map(|t| t.bit_depth), "sample_rate": first.map(|t| t.sample_rate),
    })
}

pub fn web_tracks(lib: &Library, album: i64) -> Vec<Value> {
    lib.tracks_of(album)
        .map(|t| json!({"id": t.id, "title": t.title, "artist": t.artist, "track": t.track, "disc": t.disc, "seconds": t.seconds, "suffix": t.suffix, "bitrate": t.bitrate, "bit_depth": t.bit_depth, "sample_rate": t.sample_rate, "size": t.size}))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(suffix: &str, kbps: u32) -> Track {
        Track { suffix: suffix.into(), bitrate: kbps, ..Default::default() }
    }

    #[test]
    fn files_are_sent_as_they_are_unless_asked_otherwise() {
        assert_eq!(Transcode::asked(None, 0, 0).plan(&track("flac", 900)), None);
        assert_eq!(Transcode::asked(Some("raw"), 0, 0).plan(&track("flac", 900)), None);
        assert_eq!(Transcode::asked(None, 320, 0).plan(&track("mp3", 256)), None);
        assert_eq!(Transcode::asked(None, 128, 0).plan(&track("flac", 900)), Some(("mp3", 128)));
        assert_eq!(Transcode::asked(Some("opus"), 0, 0).plan(&track("flac", 900)), Some(("opus", 160)));
        assert_eq!(Transcode::asked(Some("mp3"), 0, 0).plan(&track("mp3", 320)), None);
    }

    #[test]
    fn searches_match_every_word_without_accents() {
        let w = query_words("bjork vesp");
        assert!(matches(&w, "Vespertine Björk"));
        assert!(!matches(&w, "Vespertine"));
        assert!(matches(&query_words("\"\""), "anything"));
    }

    #[test]
    fn passwords_can_come_hex_encoded() {
        assert_eq!(decode_hex("736573616d65").as_deref(), Some("sesame"));
        assert_eq!(decode_hex("7"), None);
    }

    #[test]
    fn artists_are_indexed_by_letter_without_articles() {
        let lib = Library::new(vec![Artist { id: 1, name: "The Beatles".into() }, Artist { id: 2, name: "Björk".into() }, Artist { id: 3, name: "!!!".into() }], vec![], vec![]);
        let idx = index(&lib, &Marks(HashMap::new()), true);
        let letters: Vec<&str> = idx.iter().map(|i| i["name"].as_str().unwrap()).collect();
        assert_eq!(letters, vec!["#", "B"]);
        assert_eq!(idx[1]["artist"].as_array().unwrap().len(), 2);
    }
}
