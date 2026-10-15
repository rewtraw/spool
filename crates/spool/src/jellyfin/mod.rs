//! A second front door for players: the part of Jellyfin's API that an app such as Infuse uses
//! to browse a library, play from it and report what was watched.
//!
//! It listens on its own port, apart from Spool's web app, and has its own sign-in: what a
//! player is given opens the library and nothing else. Films and episodes are played as they
//! are; nothing is transcoded.

mod discovery;
pub mod ids;
mod asking;
pub(crate) mod items;
mod media;

use crate::app::App;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use ids::Id;
use items::{Fields, Library, Node};
use serde_json::{json, Value};
use std::collections::HashMap;

/// The Jellyfin release whose API this follows. Players choose how to talk from it.
const VERSION: &str = "10.10.7";
const SERVER_NAME: &str = "Spool";

/// Query parameters, with names matched whatever their capitals: players disagree on them.
pub struct Params(HashMap<String, Vec<String>>);

impl Params {
    pub fn parse(query: Option<&str>) -> Params {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for pair in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |s: &str| urlencoding::decode(&s.replace('+', " ")).map(|c| c.into_owned()).unwrap_or_else(|_| s.to_string());
            map.entry(decode(k).to_lowercase()).or_default().push(decode(v));
        }
        Params(map)
    }
    pub fn all(&self, key: &str) -> &[String] {
        self.0.get(key).map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.all(key).first().map(String::as_str)
    }
    /// A comma-separated list, lower-cased.
    pub fn list(&self, key: &str) -> Vec<String> {
        self.all(key).iter().flat_map(|v| v.split(',')).map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect()
    }
    pub fn flag(&self, key: &str) -> Option<bool> {
        match self.get(key)?.to_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        }
    }
    pub fn num(&self, key: &str) -> Option<i64> {
        self.get(key)?.trim().parse().ok()
    }
    pub fn id(&self, key: &str) -> Option<Id> {
        Id::parse(self.get(key)?)
    }
}

/// The id players know this server by, made once and kept.
pub fn server_id(app: &App) -> String {
    let id: String = app.db.get_setting("player_server_id");
    if id.len() == 32 {
        return id;
    }
    let id = crate::settings::new_key()[..32].to_string();
    let _ = app.db.set_setting("player_server_id", &id);
    id
}

fn page(items: Vec<Value>, total: usize, start: i64) -> Response {
    Json(json!({"Items": items, "TotalRecordCount": total, "StartIndex": start})).into_response()
}

fn empty_page() -> Response {
    page(vec![], 0, 0)
}

fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

fn missing() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

/// One value from a `MediaBrowser Client="Infuse", Token="..."` header.
fn auth_field(header: &str, name: &str) -> Option<String> {
    let rest = header.split_once(' ').map(|(_, r)| r).unwrap_or(header);
    let mut parts = vec![];
    let (mut current, mut quoted) = (String::new(), false);
    for c in rest.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => parts.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    parts.push(current);
    parts.iter().find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string()).filter(|v| !v.is_empty())
    })
}

/// A field of a request body, whatever capitals the player gave its name.
fn field<'a>(body: &'a Value, name: &str) -> Option<&'a Value> {
    let o = body.as_object()?;
    o.get(name).or_else(|| o.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v)).filter(|v| !v.is_null())
}

struct Call {
    app: App,
    method: Method,
    path: Vec<String>,
    q: Params,
    headers: HeaderMap,
    body: Value,
    /// The address the request came from, when it is known.
    peer: Option<std::net::IpAddr>,
    /// Whether whoever is asking is held to compact versions just now: their account is,
    /// and they are away from home. Settled once they are known to be signed in.
    compact: std::sync::atomic::AtomicBool,
}

impl Call {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    fn client(&self, name: &str) -> String {
        ["authorization", "x-emby-authorization"].iter().find_map(|h| auth_field(self.header(h)?, name)).unwrap_or_default()
    }

    fn token(&self) -> Option<String> {
        for h in ["authorization", "x-emby-authorization"] {
            if let Some(v) = self.header(h) {
                if let Some(t) = auth_field(v, "Token") {
                    return Some(t);
                }
                if let Some(t) = v.strip_prefix("Bearer ") {
                    return Some(t.trim().to_string());
                }
            }
        }
        ["x-emby-token", "x-mediabrowser-token"].iter().find_map(|h| self.header(h)).or_else(|| self.q.get("api_key")).or_else(|| self.q.get("apikey")).map(str::to_string)
    }

    /// Who is asking: the account the player's token is signed in to. The token is found
    /// by what it hashes to, so how long that takes says nothing of how nearly a guess was
    /// right. That it was seen is written down once in a few minutes. One that signed in
    /// before players' names were kept is given the name it goes by now.
    fn signed(&self) -> Option<crate::app::Signed> {
        let token = self.token()?;
        let mut players = self.app.players.lock();
        let who = players.get_mut(&token)?;
        let now = crate::db::now();
        if now - who.seen > 300 {
            who.seen = now;
            let _ = self.app.db.device_seen(&token);
        }
        if who.device.is_empty() {
            let name = self.client("Device");
            if !name.is_empty() && self.app.db.name_device(&token, &name, &self.client("Client")).is_ok() {
                who.device = name;
            }
        }
        Some(who.clone())
    }

    /// A value a player may put in the query or in the body.
    fn given(&self, name: &str) -> Option<String> {
        self.q.get(&name.to_lowercase()).map(str::to_string).or_else(|| {
            field(&self.body, name).map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
        })
    }

    fn library(&self) -> Result<Library, Response> {
        Library::shared(&self.app, self.signed().map(|w| w.account).unwrap_or(0), self.compact()).map_err(|e| {
            tracing::warn!(error = %e, "could not read the library for a player");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })
    }

    fn compact(&self) -> bool {
        self.compact.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn user(&self, who: &crate::models::Account) -> Value {
        let now = items::iso(crate::db::now());
        // Heard and read in the household's language: subtitles come on when what is heard is another.
        let language = crate::subs::language_code(&self.app.settings.general().language).map(items::three_letter).unwrap_or_default();
        let configuration = json!({
            "PlayDefaultAudioTrack": true, "AudioLanguagePreference": language, "SubtitleLanguagePreference": language, "DisplayMissingEpisodes": false, "GroupedFolders": [], "SubtitleMode": "Smart",
            "DisplayCollectionsView": false, "EnableLocalPassword": false, "OrderedViews": [], "LatestItemsExcludes": [], "MyMediaExcludes": [],
            "HidePlayedInLatest": false, "RememberAudioSelections": true, "RememberSubtitleSelections": true, "EnableNextEpisodeAutoPlay": true, "CastReceiverId": "",
        });
        let mut policy = json!({
            "IsAdministrator": true, "IsHidden": false, "IsDisabled": false, "BlockedTags": [], "AllowedTags": [], "EnableUserPreferenceAccess": true,
            "AccessSchedules": [], "BlockUnratedItems": [], "EnableRemoteControlOfOtherUsers": false, "EnableSharedDeviceControl": false, "EnableRemoteAccess": true,
            "EnableLiveTvManagement": false, "EnableLiveTvAccess": false, "EnableMediaPlayback": true, "EnableAudioPlaybackTranscoding": false,
            "EnableVideoPlaybackTranscoding": false, "EnablePlaybackRemuxing": false, "ForceRemoteSourceTranscoding": false, "EnableContentDeletion": false,
        });
        let more = json!({
            "EnableContentDeletionFromFolders": [], "EnableContentDownloading": true, "EnableSyncTranscoding": false, "EnableMediaConversion": false,
            "EnabledDevices": [], "EnableAllDevices": true, "EnabledChannels": [], "EnableAllChannels": true, "EnabledFolders": [], "EnableAllFolders": true,
            "InvalidLoginAttemptCount": 0, "LoginAttemptsBeforeLockout": -1, "MaxActiveSessions": 0, "EnablePublicSharing": false, "BlockedMediaFolders": [],
            "BlockedChannels": [], "RemoteClientBitrateLimit": 0, "SyncPlayAccess": "None", "EnableCollectionManagement": false, "EnableSubtitleManagement": false,
            "EnableLyricManagement": false,
            "AuthenticationProviderId": "Jellyfin.Server.Implementations.Users.DefaultAuthenticationProvider",
            "PasswordResetProviderId": "Jellyfin.Server.Implementations.Users.DefaultPasswordResetProvider",
        });
        if let (Some(policy), Value::Object(more)) = (policy.as_object_mut(), more) {
            policy.extend(more);
        }
        json!({
            "Name": who.name, "ServerId": server_id(&self.app), "Id": Id::User(who.id).to_string(),
            "HasPassword": !who.password.is_empty(), "HasConfiguredPassword": !who.password.is_empty(), "HasConfiguredEasyPassword": false,
            "EnableAutoLogin": false, "LastLoginDate": now, "LastActivityDate": now, "Configuration": configuration, "Policy": policy,
        })
    }

    /// The address the player reached this server at, which it is told to keep using.
    fn address(&self) -> String {
        match self.header("host") {
            Some(host) => format!("{}://{host}", self.header("x-forwarded-proto").unwrap_or("http")),
            None => format!("http://localhost:{}", self.app.settings.general().players_port),
        }
    }

    fn system_info(&self, full: bool) -> Value {
        let mut info = json!({
            "LocalAddress": self.address(), "ServerName": SERVER_NAME, "Version": VERSION, "ProductName": "Jellyfin Server",
            "OperatingSystem": if cfg!(target_os = "macos") { "macOS" } else { "Linux" }, "Id": server_id(&self.app), "StartupWizardCompleted": true,
        });
        if full {
            for (k, v) in [
                ("OperatingSystemDisplayName", json!("")), ("HasPendingRestart", json!(false)), ("IsShuttingDown", json!(false)), ("SupportsLibraryMonitor", json!(false)),
                ("WebSocketPortNumber", json!(self.app.settings.general().players_port)), ("CompletedInstallations", json!([])), ("CanSelfRestart", json!(false)),
                ("CanLaunchWebBrowser", json!(false)), ("HasUpdateAvailable", json!(false)), ("EncoderLocation", json!("System")), ("SystemArchitecture", json!(std::env::consts::ARCH)),
                ("PackageName", json!("spool")), ("CastReceiverApplications", json!([])),
            ] {
                info[k] = v;
            }
        }
        info
    }

    async fn sign_in(&self) -> Response {
        const TRIES: u32 = 5;
        const LOCKED_FOR: i64 = 60;
        let now = crate::db::now();
        {
            let mut f = self.app.player_failures.lock();
            if now - f.1 >= LOCKED_FOR {
                f.0 = 0;
            }
            if f.0 >= TRIES {
                return (StatusCode::TOO_MANY_REQUESTS, "Too many wrong passwords; wait a minute and try again.").into_response();
            }
        }
        let name = self.given("Username").unwrap_or_default();
        let password = self.given("Pw").or_else(|| self.given("Password")).unwrap_or_default();
        // An account that may not sign in is answered as one that is not there.
        let who = self.app.db.accounts().unwrap_or_default().into_iter().find(|a| a.enabled && a.name.trim().eq_ignore_ascii_case(name.trim()));
        let right = who.as_ref().is_some_and(|a| a.password.is_empty() || crate::settings::verify_password(&a.password, &password));
        let Some(who) = who.filter(|_| right) else {
            {
                let mut f = self.app.player_failures.lock();
                *f = (f.0 + 1, now);
            }
            tracing::warn!(client = %self.client("Client"), device = %self.client("Device"), "a player gave the wrong name or password");
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            return (StatusCode::UNAUTHORIZED, "Invalid username or password entered.").into_response();
        };
        *self.app.player_failures.lock() = (0, 0);
        let token = crate::settings::new_key()[..32].to_string();
        let device = crate::models::Device { token: token.clone(), account_id: who.id, name: self.client("Device"), client: self.client("Client"), created_at: now, seen_at: now };
        if self.app.db.add_device(&device).is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        // Its password was right when it was checked. If the account has been changed since,
        // what was checked no longer stands, and neither does this sign-in.
        if self.app.db.account(who.id).ok().flatten().is_none_or(|a| !a.enabled || a.password != who.password) {
            let _ = self.app.db.remove_devices(Some(&token), None);
            self.app.reload_players();
            return (StatusCode::UNAUTHORIZED, "Invalid username or password entered.").into_response();
        }
        self.app.reload_players();
        tracing::info!(account = %who.name, client = %self.client("Client"), device = %self.client("Device"), "a player signed in");
        let server = server_id(&self.app);
        Json(json!({
            "User": self.user(&who),
            "SessionInfo": {
                "Id": crate::settings::new_key()[..32], "UserId": Id::User(who.id).to_string(), "UserName": who.name, "Client": self.client("Client"),
                "DeviceName": self.client("Device"), "DeviceId": self.client("DeviceId"), "ApplicationVersion": self.client("Version"),
                "RemoteEndPoint": "", "IsActive": true, "SupportsMediaControl": false, "SupportsRemoteControl": false, "HasCustomDeviceName": false,
                "LastActivityDate": items::iso(now), "LastPlaybackCheckIn": "0001-01-01T00:00:00.0000000Z",
                "PlayState": {"CanSeek": false, "IsPaused": false, "IsMuted": false, "RepeatMode": "RepeatNone", "PlaybackOrder": "Default"},
                "Capabilities": {"PlayableMediaTypes": [], "SupportedCommands": [], "SupportsMediaControl": false, "SupportsPersistentIdentifier": true},
                "PlayableMediaTypes": [], "AdditionalUsers": [], "NowPlayingQueue": [], "SupportedCommands": [], "ServerId": server,
            },
            "AccessToken": token,
            "ServerId": server,
        }))
        .into_response()
    }

    fn views(&self, lib: &Library) -> Vec<Value> {
        [Kind::Movie, Kind::Series].into_iter().map(|k| lib.json(&Node::View(k), &Fields::all())).collect()
    }

    fn nodes(&self, lib: &Library, nodes: &[Node], fields: &Fields) -> Vec<Value> {
        nodes.iter().map(|n| lib.json(n, fields)).collect()
    }

    /// The full description of one item.
    async fn detail(&self, id: &str) -> Response {
        let Some(id) = Id::parse(id) else { return missing() };
        let lib = match self.library() {
            Ok(l) => l,
            Err(r) => return r,
        };
        if let Id::Extra(..) = id {
            return lib.extra_json(id).filter(|_| !lib.compact).map(|v| Json(v).into_response()).unwrap_or_else(missing);
        }
        if let Id::Person(..) = id {
            return match lib.people().into_iter().find(|(i, _)| *i == id) {
                Some((i, p)) => Json(lib.person_json(i, p)).into_response(),
                None => missing(),
            };
        }
        let lib = match media::with_streams(&self.app, lib, id).await {
            Ok(l) => l,
            Err(r) => return r,
        };
        match lib.node(id) {
            Some(n) => Json(lib.json(&n, &Fields::all())).into_response(),
            None => missing(),
        }
    }

    async fn playback_info(&self, id: &str) -> Response {
        let Some(id) = Id::parse(id) else { return missing() };
        let lib = match self.library() {
            Ok(l) => l,
            Err(r) => return r,
        };
        if let Id::Extra(..) = id {
            let Some(item) = lib.extra_json(id).filter(|_| !lib.compact) else { return missing() };
            return Json(json!({"MediaSources": item["MediaSources"], "PlaySessionId": crate::settings::new_key()[..32]})).into_response();
        }
        let lib = match media::with_streams(&self.app, lib, id).await {
            Ok(l) => l,
            Err(r) => return r,
        };
        let Some(node) = lib.node(id) else { return missing() };
        let item = lib.json(&node, &Fields::all());
        Json(json!({"MediaSources": item.get("MediaSources").cloned().unwrap_or(json!([])), "PlaySessionId": crate::settings::new_key()[..32]})).into_response()
    }

    async fn route(&self) -> Response {
        let path: Vec<&str> = self.path.iter().map(String::as_str).collect();
        let reading = matches!(self.method, Method::GET | Method::HEAD);
        let head = self.method == Method::HEAD;

        // What a player may ask before it has signed in.
        match (reading, path.as_slice()) {
            // In a browser this address is where the people who play ask for something new.
            (true, []) => return ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], include_str!("ask.html")).into_response(),
            (_, ["system", "ping"]) => return Json(json!("Jellyfin Server")).into_response(),
            (true, ["system", "info", "public"]) => return Json(self.system_info(false)).into_response(),
            // Who may sign in, so that a player can offer the names to choose from.
            (true, ["users", "public"]) => return Json(self.app.db.accounts().unwrap_or_default().iter().filter(|a| a.enabled).map(|a| self.user(a)).collect::<Vec<_>>()).into_response(),
            (true, ["branding", "configuration"]) => return Json(json!({"LoginDisclaimer": "", "CustomCss": "", "SplashscreenEnabled": false})).into_response(),
            (true, ["branding", "css" | "css.css"]) => return ([(header::CONTENT_TYPE, "text/css")], "").into_response(),
            (true, ["quickconnect", "enabled"]) => return Json(json!(false)).into_response(),
            (false, ["users", "authenticatebyname"]) => return self.sign_in().await,
            // One file, to whoever holds a ticket the app made out for it.
            (true, ["spool", "play", _]) => {
                let file = self.q.get("ticket").and_then(|t| self.app.played_on(t)).and_then(|id| self.app.db.file(id).ok().flatten());
                let Some(path) = file.and_then(|f| Some(self.app.db.title(f.title_id).ok()??.file_path(&f))) else { return (StatusCode::UNAUTHORIZED, "The link has run out. Open the film's page again.").into_response() };
                return crate::serve::send_file(&path, &self.headers, head).await;
            }
            (true, ["items", id, "images", kind, ..]) => return media::image(&self.app, id, kind, self.q.get("tag").is_some(), head).await,
            (true, ["users", _, "images", ..] | ["userimage"] | ["favicon.ico"] | ["branding", "splashscreen"]) => return missing(),
            _ => {}
        }
        let Some(who) = self.signed() else { return (StatusCode::UNAUTHORIZED, "Sign in first.").into_response() };
        let Some(me) = self.app.db.account(who.account).ok().flatten().filter(|a| a.enabled) else { return (StatusCode::UNAUTHORIZED, "Sign in first.").into_response() };
        if me.compact_away && !self.app.at_home(self.peer).await {
            self.compact.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        match (reading, path.as_slice()) {
            (true, ["system", "info"]) => Json(self.system_info(true)).into_response(),
            (true, ["system", "endpoint"]) => Json(json!({"IsLocal": true, "IsInNetwork": true})).into_response(),
            (true, ["users"]) => Json(json!([self.user(&me)])).into_response(),
            (true, ["users", "me"] | ["users", _]) => Json(self.user(&me)).into_response(),
            (false, ["sessions", "logout"]) => {
                if let Some(token) = self.token() {
                    let _ = self.app.db.remove_devices(Some(&token), None);
                    self.app.reload_players();
                }
                no_content()
            }

            // Asking for something the library has not got: not part of Jellyfin's API, and
            // used by the page this door gives a browser.
            (true, ["spool", "find"]) => asking::answer(self.app.asks_find(me.id, self.q.get("q").unwrap_or("")).await.map(|found| json!(found))),
            (true, ["spool", "asks"]) => asking::answer(self.app.asks_report(Some(me.id)).map(|mine| json!(mine))),
            (false, ["spool", "asks"]) if self.method == Method::POST => asking::ask(&self.app, &me, &self.body).await,
            (false, ["spool", "asks", id]) if self.method == Method::DELETE => asking::withdraw(&self.app, &me, id),

            (true, ["userviews"] | ["users", _, "views"] | ["library", "mediafolders"]) => match self.library() {
                Ok(lib) => page(self.views(&lib), 2, 0),
                Err(r) => r,
            },
            (true, ["library", "virtualfolders"]) => {
                let g = self.app.settings.general();
                Json(json!([
                    {"Name": "Movies", "Locations": [g.movie_root], "CollectionType": "movies", "ItemId": Id::Movies.to_string(), "RefreshStatus": "Idle", "LibraryOptions": {}},
                    {"Name": "Shows", "Locations": [g.series_root], "CollectionType": "tvshows", "ItemId": Id::Shows.to_string(), "RefreshStatus": "Idle", "LibraryOptions": {}},
                ]))
                .into_response()
            }
            (true, ["items"] | ["users", _, "items"]) => match self.library() {
                Ok(lib) => {
                    let (nodes, total) = lib.list(&self.q);
                    page(self.nodes(&lib, &nodes, &Fields::from(&self.q)), total, self.q.num("startindex").unwrap_or(0))
                }
                Err(r) => r,
            },
            (true, ["items", "latest"] | ["users", _, "items", "latest"]) => match self.library() {
                Ok(lib) => {
                    let nodes = lib.latest(self.q.id("parentid"), self.q.num("limit").unwrap_or(20).clamp(1, 200) as usize);
                    Json(self.nodes(&lib, &nodes, &Fields::from(&self.q))).into_response()
                }
                Err(r) => r,
            },
            (true, ["useritems", "resume"] | ["items", "resume"] | ["users", _, "items", "resume"]) => match self.library() {
                Ok(lib) => {
                    let nodes = lib.resume(self.q.id("parentid"), self.q.num("limit").unwrap_or(50).clamp(1, 200) as usize);
                    page(self.nodes(&lib, &nodes, &Fields::from(&self.q)), nodes.len(), 0)
                }
                Err(r) => r,
            },
            (true, ["shows", "nextup"]) => match self.library() {
                Ok(lib) => {
                    let nodes = lib.next_up(self.q.id("seriesid").or(self.q.id("parentid").filter(|i| matches!(i, Id::Series(_)))), self.q.num("limit").unwrap_or(50).clamp(1, 200) as usize);
                    page(self.nodes(&lib, &nodes, &Fields::from(&self.q)), nodes.len(), 0)
                }
                Err(r) => r,
            },
            (true, ["shows", id, "seasons"]) => match (self.library(), Id::parse(id)) {
                (Ok(lib), Some(id)) => {
                    let nodes = lib.seasons_of(id);
                    page(self.nodes(&lib, &nodes, &Fields::from(&self.q)), nodes.len(), 0)
                }
                (Err(r), _) => r,
                _ => missing(),
            },
            (true, ["shows", id, "episodes"]) => match (self.library(), Id::parse(id)) {
                (Ok(lib), Some(id)) => {
                    let season = match self.q.id("seasonid") {
                        Some(Id::Season(_, n)) => Some(n),
                        _ => self.q.num("season").map(|n| n as u32),
                    };
                    let nodes = lib.episodes_in(id, season);
                    let total = nodes.len();
                    let start = self.q.num("startindex").unwrap_or(0).max(0);
                    let limit = self.q.num("limit").filter(|l| *l > 0).map(|l| l as usize).unwrap_or(usize::MAX);
                    let shown: Vec<Node> = nodes.into_iter().skip(start as usize).take(limit).collect();
                    page(self.nodes(&lib, &shown, &Fields::from(&self.q)), total, start)
                }
                (Err(r), _) => r,
                _ => missing(),
            },
            (true, ["items", "counts"]) => match self.library() {
                Ok(lib) => Json(lib.counts()).into_response(),
                Err(r) => r,
            },
            (true, ["items", "filters"]) => match self.library() {
                Ok(lib) => Json(json!({"Genres": lib.names(self.q.id("parentid"), false), "Tags": [], "OfficialRatings": [], "Years": []})).into_response(),
                Err(r) => r,
            },
            (true, ["items", "filters2"]) => match self.library() {
                Ok(lib) => Json(json!({"Genres": lib.names(self.q.id("parentid"), false).iter().map(|g| json!({"Name": g, "Id": Id::Genre(Id::named(g)).to_string()})).collect::<Vec<_>>(), "Tags": []})).into_response(),
                Err(r) => r,
            },
            (true, ["genres" | "studios"]) => match self.library() {
                Ok(lib) => {
                    let studios = path[0] == "studios";
                    let items: Vec<Value> = lib
                        .names(self.q.id("parentid"), studios)
                        .iter()
                        .map(|name| {
                            let id = if studios { Id::Studio(Id::named(name)) } else { Id::Genre(Id::named(name)) }.to_string();
                            json!({"Name": name, "ServerId": lib.server_id, "Id": id, "Type": if studios { "Studio" } else { "Genre" }, "IsFolder": false, "ImageTags": {}, "BackdropImageTags": [], "LocationType": "FileSystem", "MediaType": "Unknown"})
                        })
                        .collect();
                    let total = items.len();
                    page(items, total, 0)
                }
                Err(r) => r,
            },
            (true, ["persons"]) => match self.library() {
                Ok(lib) => {
                    let term = self.q.get("searchterm").map(str::to_lowercase).unwrap_or_default();
                    let items: Vec<Value> = lib.people().into_iter().filter(|(_, p)| p.name.to_lowercase().contains(&term)).take(self.q.num("limit").filter(|l| *l > 0).unwrap_or(200) as usize).map(|(i, p)| lib.person_json(i, p)).collect();
                    let total = items.len();
                    page(items, total, 0)
                }
                Err(r) => r,
            },
            (true, ["search", "hints"]) => match self.library() {
                Ok(lib) => {
                    let q = Params::parse(Some(&format!("recursive=true&searchterm={}&limit={}", urlencoding::encode(self.q.get("searchterm").unwrap_or("")), self.q.num("limit").unwrap_or(20))));
                    let (nodes, total) = lib.list(&q);
                    let hints: Vec<Value> = nodes
                        .iter()
                        .map(|n| {
                            let item = lib.json(n, &Fields::from(&q));
                            json!({"ItemId": item["Id"], "Id": item["Id"], "Name": item["Name"], "Type": item["Type"], "ProductionYear": item["ProductionYear"], "RunTimeTicks": item["RunTimeTicks"], "MediaType": item["MediaType"], "PrimaryImageTag": item["ImageTags"]["Primary"], "IsFolder": item["IsFolder"]})
                        })
                        .collect();
                    Json(json!({"SearchHints": hints, "TotalRecordCount": total})).into_response()
                }
                Err(r) => r,
            },

            (_, ["items", id, "playbackinfo"]) => self.playback_info(id).await,
            (true, ["items", id, "download" | "file"] | ["videos", id, "stream" | "original"]) => media::stream(&self.app, id, self.q.get("mediasourceid"), &self.headers, head, self.peer, self.signed().as_ref(), self.compact(), me.compact_away).await,
            (true, ["videos", id, file]) if file.starts_with("stream.") || file.starts_with("original.") => media::stream(&self.app, id, self.q.get("mediasourceid"), &self.headers, head, self.peer, self.signed().as_ref(), self.compact(), me.compact_away).await,
            (true, ["videos", id, source, "subtitles", index, .., file]) if file.starts_with("stream.") => media::subtitle(&self.app, id, Some(source), index, file, self.compact()).await,
            (true, ["videos", id, "subtitles", index, .., file]) if file.starts_with("stream.") => media::subtitle(&self.app, id, None, index, file, self.compact()).await,

            (false, ["sessions", "playing"]) => self.progress(false),
            (false, ["sessions", "playing", "progress"]) => self.progress(false),
            (false, ["sessions", "playing", "stopped"]) => self.progress(true),
            (false, ["users", _, "playingitems", id]) => self.progress_for(id, self.method == Method::DELETE),
            (false, ["users", _, "playingitems", id, "progress"]) => self.progress_for(id, false),
            (false, ["users", _, "playeditems", id] | ["userplayeditems", id]) => self.change(id, |app, lib, id| media::mark(app, lib, id, self.method != Method::DELETE)),
            (false, ["users", _, "favoriteitems", id] | ["userfavoriteitems", id]) => self.change(id, |app, lib, id| media::favorite(app, lib, id, self.method != Method::DELETE)),
            (true, ["useritems", id, "userdata"] | ["users", _, "items", id, "userdata"]) => self.change(id, |_, _, _| Ok(())),
            (false, ["useritems", id, "userdata"] | ["users", _, "items", id, "userdata"]) => self.change(id, |app, lib, id| {
                if let Some(played) = field(&self.body, "Played").and_then(Value::as_bool) {
                    media::mark(app, lib, id, played)?;
                }
                if let Some(on) = field(&self.body, "IsFavorite").and_then(Value::as_bool) {
                    media::favorite(app, lib, id, on)?;
                }
                Ok(())
            }),
            (false, ["users", _, "items", _, "rating"] | ["useritems", _, "rating"]) => Json(json!({})).into_response(),

            (true, ["displaypreferences", id]) => Json(json!({
                "Id": id, "SortBy": "SortName", "SortOrder": "Ascending", "RememberIndexing": false, "RememberSorting": false, "PrimaryImageHeight": 250, "PrimaryImageWidth": 250,
                "CustomPrefs": {}, "ScrollDirection": "Horizontal", "ShowBackdrop": true, "ShowSidebar": false, "Client": self.q.get("client").unwrap_or("emby"),
            }))
            .into_response(),
            (true, ["playback", "bitratetest"]) => {
                let size = self.q.num("size").unwrap_or(102_400).clamp(1, 10_000_000) as usize;
                ([(header::CONTENT_TYPE, "application/octet-stream")], vec![0u8; size]).into_response()
            }
            (true, ["items", id, "ancestors"] | ["users", _, "items", id, "ancestors"]) => match (self.library(), Id::parse(id)) {
                (Ok(lib), Some(id)) => {
                    let view = match lib.node(id) {
                        Some(Node::Movie(_) | Node::Set(_)) => Some(Kind::Movie),
                        Some(Node::Series(_) | Node::Season(..) | Node::Episode(..)) => Some(Kind::Series),
                        _ => None,
                    };
                    Json(view.map(|k| lib.json(&Node::View(k), &Fields::all())).into_iter().collect::<Vec<_>>()).into_response()
                }
                (Err(r), _) => r,
                _ => Json(json!([])).into_response(),
            },
            (true, ["items", _, "thememedia"] | ["users", _, "items", _, "thememedia"]) => {
                let none = json!({"Items": [], "TotalRecordCount": 0, "StartIndex": 0, "OwnerId": "00000000000000000000000000000000"});
                Json(json!({"ThemeVideosResult": none, "ThemeSongsResult": none, "SoundtrackSongsResult": none})).into_response()
            }
            // Trailers and other extras kept in the title's folder.
            (true, ["items", id, which @ ("specialfeatures" | "localtrailers")] | ["users", _, "items", id, which @ ("specialfeatures" | "localtrailers")]) => match (Id::parse(id), self.library()) {
                (Some(id), Ok(lib)) if !lib.compact => Json(json!(lib.extras_of(id, *which == "localtrailers"))).into_response(),
                (_, Err(r)) => r,
                _ => Json(json!([])).into_response(),
            },
            (true, ["items", _, "remotetrailers" | "criticreviews" | "externalidinfos" | "images"]) => Json(json!([])).into_response(),
            (true, ["movies", "recommendations"] | ["plugins"] | ["scheduledtasks"] | ["sessions"] | ["users", _, "groupingoptions"] | ["userviews", "groupingoptions"] | ["localization", ..] | ["devices"] | ["channels"]) => Json(json!([])).into_response(),
            // Last, so the named routes under /Items above are tried first.
            (true, ["items", id] | ["users", _, "items", id]) => self.detail(id).await,

            // Anything else under a name Jellyfin has is answered with nothing rather than an
            // error: players ask for plenty that a library can do without.
            (_, [first, ..]) if KNOWN.contains(first) => {
                self.note_unknown();
                if reading {
                    empty_page()
                } else {
                    no_content()
                }
            }
            _ => {
                self.note_unknown();
                missing()
            }
        }
    }

    fn note_unknown(&self) {
        tracing::info!(method = %self.method, path = %self.path.join("/"), client = %self.client("Client"), "a player asked for something Spool does not offer");
    }

    /// A playback report: where a player has got to in what it is playing.
    fn progress(&self, stopped: bool) -> Response {
        match self.given("ItemId") {
            Some(id) => self.progress_for(&id, stopped),
            None => no_content(),
        }
    }

    fn progress_for(&self, id: &str, stopped: bool) -> Response {
        // Remembered so that Spool can say whether a restart would interrupt someone. A
        // player that has stopped is no longer one.
        self.app.last_playing.store(if stopped { 0 } else { crate::db::now() }, std::sync::atomic::Ordering::Relaxed);
        let position = self.given("PositionTicks").and_then(|t| t.parse::<f64>().ok()).unwrap_or(0.0) / items::TICKS;
        if let (Some(id), Ok(lib), Some(who)) = (Id::parse(id), self.library(), self.signed()) {
            let finished = media::report(&self.app, &lib, id, position, stopped).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "could not record a player's progress");
                false
            });
            // The sitting is written down against what was played: a film, or an episode
            // under its series' name.
            let subject = match lib.node(id) {
                Some(Node::Movie(t)) => Some(("movie", t.id, t.id, t.title.clone())),
                Some(Node::Episode(t, e)) => Some(("episode", e.id, t.id, format!("{} {}x{:02}", t.title, e.season, e.episode))),
                _ => None,
            };
            if let Some((kind, ref_id, title_id, title)) = subject {
                let paused = self.given("IsPaused").is_some_and(|p| p == "true");
                self.app.note_sitting(&who, &crate::app::Sitting { kind, ref_id, title_id, title: &title, position, paused, finished, stopped });
            }
        }
        no_content()
    }

    /// Change what is recorded about an item for the person watching, and answer with how it
    /// stands afterwards.
    fn change(&self, id: &str, apply: impl FnOnce(&App, &Library, Id) -> anyhow::Result<()>) -> Response {
        let Some(id) = Id::parse(id) else { return missing() };
        let outcome = self.library().and_then(|lib| {
            apply(&self.app, &lib, id).map_err(|e| {
                tracing::warn!(error = %e, "could not record a change from a player");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            })?;
            self.library()
        });
        match outcome {
            Ok(lib) => match lib.node(id) {
                Some(n) => Json(lib.user_data(&n)).into_response(),
                None => missing(),
            },
            Err(r) => r,
        }
    }
}

use crate::models::Kind;

/// The first word of every address Jellyfin's API has.
const KNOWN: &[&str] = &[
    "items", "users", "useritems", "userviews", "userplayeditems", "userfavoriteitems", "shows", "movies", "videos", "audio", "sessions", "system", "library", "libraries",
    "livetv", "syncplay", "playlists", "artists", "albums", "songs", "musicgenres", "genres", "studios", "persons", "years", "trailers", "channels", "collections",
    "search", "displaypreferences", "mediasegments", "playback", "livestreams", "localization", "branding", "quickconnect", "devices", "auth", "clientlog", "providers",
];

fn redact(query: &str) -> String {
    query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((k, _)) if ["api_key", "apikey", "ticket"].iter().any(|secret| k.eq_ignore_ascii_case(secret)) => format!("{k}=..."),
            _ => pair.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

async fn handle(State(app): State<App>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    // The address the connection came from, and never one a request claims for itself.
    let peer = parts.extensions.get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    let mut path: Vec<String> = parts.uri.path().split('/').filter(|s| !s.is_empty()).map(str::to_lowercase).collect();
    // Older players put the API under a prefix.
    if matches!(path.first().map(String::as_str), Some("emby" | "jellyfin")) {
        path.remove(0);
    }
    let body = match parts.method {
        Method::GET | Method::HEAD => Value::Null,
        _ => axum::body::to_bytes(body, 1 << 20).await.ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null),
    };
    let call = Call { app, method: parts.method, path, q: Params::parse(parts.uri.query()), headers: parts.headers, body, peer, compact: Default::default() };
    let response = call.route().await;
    let line = format!(
        "{} {} {}{} -> {}  [{}]",
        chrono::Local::now().format("%H:%M:%S"),
        call.method,
        parts.uri.path(),
        parts.uri.query().map(|q| format!("?{}", redact(q))).unwrap_or_default(),
        response.status().as_u16(),
        call.header("user-agent").unwrap_or("")
    );
    tracing::debug!("player: {line}");
    let mut log = call.app.player_requests.lock();
    if log.len() >= 400 {
        log.pop_front();
    }
    log.push_back(line);
    response
}

/// Everything a player can ask for. Video is sent as it is; the rest is compressed.
pub fn router(app: App) -> Router {
    use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
    let text_only = DefaultPredicate::new().and(NotForContentType::new("video/")).and(NotForContentType::new("application/octet-stream"));
    Router::new().fallback(handle).with_state(app).layer(tower_http::compression::CompressionLayer::new().compress_when(text_only))
}

/// Keep the players' door in step with the settings: open it, move it or close it as they change.
pub fn spawn(app: App, host: String) {
    tokio::spawn(async move {
        let mut open: Option<(u32, Vec<tokio::task::JoinHandle<()>>)> = None;
        let mut refused: Option<u32> = None;
        loop {
            let g = app.settings.general();
            let want = g.players_enabled.then_some(g.players_port);
            if want != open.as_ref().map(|o| o.0) {
                if let Some((_, tasks)) = open.take() {
                    tasks.iter().for_each(|t| t.abort());
                    tracing::info!("no longer serving players");
                }
                match want {
                    None => {
                        refused = None;
                        *app.player_status.lock() = String::new();
                    }
                    Some(port) => match tokio::net::TcpListener::bind((host.as_str(), port as u16)).await {
                        Ok(listener) => {
                            refused = None;
                            *app.player_status.lock() = format!("Listening on port {port}.");
                            tracing::info!(port, "serving the library to players");
                            let router = router(app.clone());
                            let serve = tokio::spawn(async move {
                                if let Err(e) = axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>()).await {
                                    tracing::warn!(error = %e, "the players' server stopped");
                                }
                            });
                            open = Some((port, vec![serve, tokio::spawn(discovery::answer(app.clone(), port)), tokio::spawn(media::fill_in(app.clone()))]));
                        }
                        Err(e) => {
                            *app.player_status.lock() = format!("Port {port} could not be opened: {e}.");
                            if refused != Some(port) {
                                tracing::warn!(port, error = %e, "could not open the port for players");
                                refused = Some(port);
                            }
                        }
                    },
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_sign_in_header_players_send() {
        let h = r#"MediaBrowser Client="Infuse-Direct", Device="Living Room, Apple TV", DeviceId="abc", Version="8.5", Token="t0k3n""#;
        assert_eq!(auth_field(h, "Token").as_deref(), Some("t0k3n"));
        assert_eq!(auth_field(h, "Device").as_deref(), Some("Living Room, Apple TV"));
        assert_eq!(auth_field(h, "client").as_deref(), Some("Infuse-Direct"));
        assert_eq!(auth_field("MediaBrowser Client=\"x\"", "Token"), None);
        assert_eq!(auth_field("Emby Token=bare", "Token").as_deref(), Some("bare"));
    }

    #[test]
    fn query_names_match_in_any_capitals() {
        let q = Params::parse(Some("parentId=10000000000000000000000000000001&IncludeItemTypes=Movie,Series&Recursive=true&limit=5&SearchTerm=the+thing&api_key=secret"));
        assert_eq!(q.id("parentid"), Some(Id::Movie(1)));
        assert_eq!(q.list("includeitemtypes"), vec!["movie", "series"]);
        assert_eq!(q.flag("recursive"), Some(true));
        assert_eq!(q.num("limit"), Some(5));
        assert_eq!(q.get("searchterm"), Some("the thing"));
        assert_eq!(redact("a=1&api_key=secret&ApiKey=x"), "a=1&api_key=...&ApiKey=...");
    }
}
