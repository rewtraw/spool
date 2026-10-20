//! HTTP API and the embedded web app.
//!
//! This file holds what every route shares: signing in, the answer an error gives, status,
//! the event stream and the list of routes. The routes themselves are in the files beside it,
//! by what they are about.

use crate::acquire::Scope;
use crate::app::{App, Event};
use crate::models::*;
use crate::settings::General;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use spool_core::profile::QualityProfile;
use spool_core::{Flavor, Quality};
use std::collections::{HashMap, HashSet};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

mod titles;
mod watched;
mod subtitles;
mod activity;
mod settings;
mod upkeep;
mod collections;
mod locations;
mod compact;
mod players;
use titles::*;
use subtitles::*;
use activity::*;
use players::*;
use settings::*;
use upkeep::*;
use collections::*;
use locations::*;
use compact::*;

#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/dist"]
struct Assets;

pub struct ApiError(StatusCode, String);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(StatusCode::BAD_REQUEST, format!("{:#}", e.into()))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

type R<T> = Result<Json<T>, ApiError>;

fn not_found(what: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("{what} not found"))
}

const MASK: &str = "********";

fn mask(s: &str) -> String {
    if s.is_empty() {
        String::new()
    } else {
        MASK.into()
    }
}

/// Keep the stored secret when the client sends the mask back.
fn unmask(new: String, old: &str) -> String {
    if new == MASK {
        old.to_string()
    } else {
        new
    }
}

// ---------------------------------------------------------------- auth

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers.get(header::COOKIE)?.to_str().ok()?.split(';').find_map(|c| c.trim().strip_prefix("spool_session=").map(str::to_string))
}

/// Who a request comes from: a person signed in to the web app, or a program with the key.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Who {
    /// The person, when there is one. A program with the key is nobody in particular.
    pub account: Option<i64>,
    /// Whether they may change what Spool does, and not only watch and ask.
    pub manages: bool,
}

impl Who {
    /// A request that came in without passing the sign-in, as the assistants' endpoint's do
    /// once it has checked its own key.
    fn of(who: Option<axum::Extension<Who>>) -> Who {
        who.map(|w| w.0).unwrap_or(Who { account: None, manages: true })
    }
}

/// Whether the web app asks who you are: once anyone has a password. Until then it is open to
/// whoever can reach it, as the first person who runs Spool.
fn sign_in_required(accounts: &[Account]) -> bool {
    accounts.iter().any(|a| !a.password.is_empty())
}

fn who(app: &App, headers: &HeaderMap) -> Option<Who> {
    let g = app.settings.general();
    if let Some(key) = headers.get("x-api-key").and_then(|k| k.to_str().ok()) {
        if !g.api_key.is_empty() && same(key, &g.api_key) {
            return Some(Who { account: None, manages: true });
        }
    }
    let accounts = app.db.accounts().ok()?;
    if let Some(id) = session_cookie(headers).and_then(|c| app.sessions.lock().get(&c).copied()) {
        if let Some(a) = accounts.iter().find(|a| a.id == id && a.enabled) {
            return Some(Who { account: Some(a.id), manages: a.manages });
        }
    }
    (!sign_in_required(&accounts)).then(|| Who { account: accounts.iter().find(|a| a.enabled && a.manages).map(|a| a.id), manages: true })
}

/// What someone who does not run Spool may do: read the library, keep what they have
/// watched, and sign out. Asking for more is done where players ask.
fn a_viewer_may(method: &axum::http::Method, path: &str) -> bool {
    let read = matches!(*method, axum::http::Method::GET | axum::http::Method::HEAD);
    let parts: Vec<&str> = path.trim_start_matches("/api/").split('/').collect();
    matches!(
        (read, parts.as_slice()),
        (true, ["session" | "status" | "titles" | "collections" | "qualities" | "profiles" | "events" | "watching"])
            | (true, ["titles", _] | ["art", ..] | ["collections", _, "art"] | ["film-part", _, "art"] | ["anime", _, "art"])
            | (false, ["logout" | "watched"])
    )
}

/// Browsers signed in as someone, kept so that signing in is not needed again after every
/// upgrade.
fn save_sessions(app: &App) {
    let all = app.sessions.lock().clone();
    let _ = app.db.set_setting("sessions", &all);
}

/// Sign someone out of every browser, but the one named.
pub(crate) fn sign_out_everywhere(app: &App, account: i64, but: Option<&str>) {
    app.sessions.lock().retain(|token, whose| *whose != account || Some(token.as_str()) == but);
    save_sessions(app);
}

/// Compare two secrets without the time taken giving away how much of a guess was right.
pub(crate) fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// Whether a name the app was reached by is one only this network can give it: an address,
/// a name without a dot, or one under a suffix no one outside can hand out. A web page could
/// otherwise point a name of its own at this machine and use the app as if it were its own.
pub(crate) fn local_name(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(v6) => return v6.split(']').next().is_some_and(|a| a.parse::<std::net::Ipv6Addr>().is_ok()),
        None => host.rsplit_once(':').filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit())).map_or(host, |(name, _)| name),
    };
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    name.parse::<std::net::IpAddr>().is_ok() || (!name.is_empty() && !name.contains('.')) || [".local", ".localhost", ".ts.net", ".lan", ".home.arpa", ".internal"].iter().any(|s| name.ends_with(s))
}

/// A change sent from a page somewhere else, which a browser marks with where it came from.
fn from_elsewhere(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) else { return false };
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    origin.split_once("://").is_none_or(|(_, at)| !at.eq_ignore_ascii_case(host))
}

async fn guard(State(app): State<App>, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    // A request without a name comes from a program, not a page in a browser, which always gives one.
    if !host.is_empty() && !local_name(host) {
        return ApiError(StatusCode::MISDIRECTED_REQUEST, format!("Spool answers to its address or its name on this network, not {host}")).into_response();
    }
    let path = req.uri().path();
    let changes = !matches!(*req.method(), axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS);
    if changes && path.starts_with("/api/") && from_elsewhere(req.headers()) {
        return ApiError(StatusCode::FORBIDDEN, "a change can only be made from Spool's own pages".into()).into_response();
    }
    if !path.starts_with("/api/") || path == "/api/login" || path == "/api/session" {
        return next.run(req).await;
    }
    match who(&app, req.headers()) {
        Some(w) if w.manages || a_viewer_may(req.method(), path) => {
            let mut req = req;
            req.extensions_mut().insert(w);
            next.run(req).await
        }
        Some(_) => ApiError(StatusCode::FORBIDDEN, "only someone who runs Spool can do that".into()).into_response(),
        None => ApiError(StatusCode::UNAUTHORIZED, "sign in required".into()).into_response(),
    }
}

async fn login(State(app): State<App>, from: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>, Json(body): Json<Value>) -> Response {
    let from = from.map(|c| c.0 .0.ip());
    // A run of wrong guesses from one place shuts the door to it for a minute, which makes guessing hopeless.
    if !app.login_failures.take(from) {
        return ApiError(StatusCode::TOO_MANY_REQUESTS, "too many wrong passwords; wait a minute and try again".into()).into_response();
    }
    let (name, given) = (body["name"].as_str().unwrap_or_default().trim(), body["password"].as_str().unwrap_or_default());
    let person = app.db.accounts().ok().and_then(|all| all.into_iter().find(|a| a.enabled && a.name.eq_ignore_ascii_case(name)));
    // One with no password signs in by name, as a player does.
    match person.filter(|a| a.password.is_empty() || crate::settings::verify_password(&a.password, given)) {
        Some(a) => {
            app.login_failures.right(from);
            tracing::info!(account = %a.name, "signed in to the web app");
            ([(header::SET_COOKIE, new_session(&app, a.id))], Json(json!({"ok": true}))).into_response()
        }
        None => {
            tracing::warn!(from = ?from, "wrong name or password at sign-in");
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            ApiError(StatusCode::UNAUTHORIZED, "wrong name or password".into()).into_response()
        }
    }
}

async fn logout(State(app): State<App>, headers: HeaderMap) -> Response {
    if let Some(c) = session_cookie(&headers) {
        app.sessions.lock().remove(&c);
        save_sessions(&app);
    }
    ([(header::SET_COOKIE, "spool_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0".to_string())], Json(json!({"ok": true}))).into_response()
}

/// Start a browser's session as someone, and the cookie that carries it.
fn new_session(app: &App, account: i64) -> String {
    let token = crate::settings::new_key();
    {
        let mut s = app.sessions.lock();
        // A household has a handful of browsers each; past that, the oldest are not worth keeping.
        if s.len() >= 200 {
            s.clear();
        }
        s.insert(token.clone(), account);
    }
    save_sessions(app);
    format!("spool_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000")
}

/// Whether the app asks who you are, and who you are to it. While it is open, a browser is
/// signed in as the first person who runs Spool, so that it stays theirs once passwords are
/// given.
async fn session(State(app): State<App>, headers: HeaderMap) -> Response {
    let accounts = app.db.accounts().unwrap_or_default();
    let w = who(&app, &headers);
    let mut cookie = None;
    if let (Some(Who { account: Some(id), .. }), false) = (w, session_cookie(&headers).is_some_and(|c| app.sessions.lock().contains_key(&c))) {
        if !sign_in_required(&accounts) && headers.get("x-api-key").is_none() {
            cookie = Some(new_session(&app, id));
        }
    }
    let me = w.and_then(|w| w.account).and_then(|id| accounts.iter().find(|a| a.id == id)).map(|a| json!({"id": a.id, "name": a.name}));
    let g = app.settings.general();
    let body = Json(json!({
        "required": sign_in_required(&accounts),
        "signed_in": w.is_some(),
        "me": me,
        "manages": w.is_some_and(|w| w.manages),
        "players": g.players_enabled.then_some(g.players_port),
    }));
    match cookie {
        Some(c) => ([(header::SET_COOKIE, c)], body).into_response(),
        None => body.into_response(),
    }
}

// ---------------------------------------------------------------- status

async fn status(State(app): State<App>) -> R<Value> {
    let g = app.settings.general();
    let titles = app.db.titles(None)?;
    let files = app.db.file_counts()?;
    let free = |p: &str| -> Option<u64> { (!p.is_empty()).then(|| crate::app::free_space(std::path::Path::new(p))).flatten() };
    let mut warnings: Vec<String> = vec![];
    if let Some(problem) = app.volume_problem() {
        warnings.push(problem);
    }
    if app.volume_ok() {
        if let Some(free) = crate::app::free_space(&app.engine.config().incomplete_dir) {
            if free < g.min_free_gb as u64 * (1 << 30) {
                warnings.push(format!("Only {:.0} GB is free on the downloads disk. Spool will not start new downloads until more than {} GB is free.", free as f64 / (1u64 << 30) as f64, g.min_free_gb));
            }
        }
    }
    warnings.extend(app.location_problems());
    if !sign_in_required(&app.db.accounts()?) {
        warnings.push("Nobody has a password, so anyone on this network can use Spool. Give yourself one under Settings → Players → People.".into());
    }
    if app.settings.servers().iter().all(|s| !s.enabled || s.host.is_empty()) {
        warnings.push("No usenet server is configured.".into());
    }
    if app.db.indexers()?.is_empty() {
        warnings.push("No indexer is configured.".into());
    }
    if g.tmdb_api_key.is_empty() {
        warnings.push("No TMDB key is set, so new movies cannot be looked up.".into());
    }
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "mode": g.mode,
        "default_movie_profile": g.default_movie_profile,
        "default_series_profile": g.default_series_profile,
        "volume_ok": app.volume_ok(),
        "queue_paused": app.engine.is_paused(),
        "movies": titles.iter().filter(|t| t.kind == Kind::Movie).count(),
        "series": titles.iter().filter(|t| t.kind == Kind::Series).count(),
        "files": files.values().map(|f| f.0 as u64).sum::<u64>(),
        "library_bytes": files.values().map(|f| f.1).sum::<u64>(),
        "free_bytes": free(if g.movie_root.is_empty() { &g.series_root } else { &g.movie_root }),
        "attention": app.db.attention(false)?.len(),
        "asks": app.asks_waiting(),
        "warnings": warnings,
        "tasks": app.tasks.lock().clone(),
    })))
}

/// How the players' door stands, and what players have been asking for lately.
async fn players(State(app): State<App>) -> Json<Value> {
    let g = app.settings.general();
    Json(json!({
        "enabled": g.players_enabled,
        "port": g.players_port,
        "status": *app.player_status.lock(),
        "signed_in": app.players.lock().len(),
        "playing": app.playing_now(),
        "requests": app.player_requests.lock().iter().cloned().collect::<Vec<_>>(),
    }))
}

async fn events(State(app): State<App>, who: Option<axum::Extension<Who>>) -> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let manages = Who::of(who).manages;
    let stream = BroadcastStream::new(app.events.subscribe()).filter_map(move |e| match e {
        // Someone who only watches hears that a title or a location changed, and nothing of
        // what is downloaded, decided or wrong.
        Ok(Event::Job { .. } | Event::Acquisition { .. } | Event::Decisions { .. } | Event::Attention | Event::Task { .. }) if !manages => None,
        Ok(ev) => serde_json::to_string(&ev).ok().map(|s| Ok(SseEvent::default().data(s))),
        // A slow client missed events: tell it to reload its state.
        Err(_) => Some(Ok(SseEvent::default().data(r#"{"type":"resync"}"#))),
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
}

async fn assets(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    // An address under the API that nothing answers is not a page of the app.
    if path == "api" || path.starts_with("api/") {
        return ApiError(StatusCode::NOT_FOUND, format!("there is nothing at /{path}")).into_response();
    }
    match Assets::get(path).or_else(|| if path.contains('.') { None } else { Assets::get("index.html") }) {
        Some(file) => {
            let served = if Assets::get(path).is_some() { path } else { "index.html" };
            let mime = mime_guess::from_path(served).first_or_octet_stream();
            let cache = if served.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
            ([(header::CONTENT_TYPE, mime.as_ref().to_string()), (header::CACHE_CONTROL, cache.to_string())], file.data.into_owned()).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Every API route, without the sign-in layer.
fn api_routes() -> Router<App> {
    Router::new()
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/session", get(session))
        .route("/api/status", get(status))
        .route("/api/busy", get(busy))
        .route("/api/titles", get(list_titles).post(add_title))
        .route("/api/titles/bulk", post(bulk_titles))
        .route("/api/accounts", get(accounts).post(account_add))
        .route("/api/accounts/{id}", put(account_change).delete(account_remove))
        .route("/api/accounts/{id}/devices/{device}", delete(device_remove))
        .route("/api/accounts/{id}/plays", get(account_plays))
        .route("/api/accounts/{id}/plex", post(account_from_plex))
        .route("/api/asks", get(asks))
        .route("/api/asks/{id}", delete(ask_remove))
        .route("/api/asks/{id}/{answer}", post(ask_answer))
        .route("/api/links", get(links))
        .route("/api/links/pair", post(link_pair))
        .route("/api/links/found", get(links_found))
        .route("/api/links/{id}/find", post(link_find))
        .route("/api/titles/move", post(move_titles))
        .route("/api/titles/move/cancel", post(cancel_moves))
        .route("/api/blocklist/clear", post(clear_blocklist))
        .route("/api/attention/dismiss-all", post(dismiss_all_attention))
        .route("/api/titles/{id}", get(get_title).patch(patch_title).delete(delete_title))
        .route("/api/watched", post(watched::mark))
        .route("/api/watching", get(watched::watching))
        .route("/api/titles/{id}/search", post(search_title))
        .route("/api/titles/{id}/refresh", post(refresh_title))
        .route("/api/titles/{id}/scan", post(scan_title))
        .route("/api/titles/{id}/to-series", post(title_to_series))
        .route("/api/titles/{id}/nzb", post(upload_nzb))
        .route("/api/titles/{id}/seasons/{season}", patch(patch_season))
        .route("/api/episodes/{id}", patch(patch_episode))
        .route("/api/decisions/{id}/grab", post(grab_decision))
        .route("/api/files/{id}", delete(delete_file).patch(patch_file))
        .route("/api/files/{id}/keep-compact", post(compact_keep))
        .route("/api/compact", get(compact_state).post(compact_make))
        .route("/api/compact/cancel", post(compact_cancel))
        .route("/api/compact/hold", post(compact_hold))
        .route("/api/compact/everything", post(compact_everything))
        .route("/api/files/{id}/subtitles", get(file_subtitles).delete(delete_file_subtitle))
        .route("/api/files/{id}/subtitles/search", post(search_file_subtitles))
        .route("/api/files/{id}/subtitles/add", post(add_file_subtitle))
        .route("/api/files/{id}/subtitles/auto", post(auto_file_subtitle))
        .route("/api/files/{id}/subtitles/check", post(check_file_subtitle))
        .route("/api/lookup", get(lookup))
        .route("/api/lookup/all", get(lookup_all))
        .route("/api/activity", get(activity))
        .route("/api/activity/{id}/{action}", post(activity_action))
        .route("/api/queue/{action}", post(queue_action))
        .route("/api/jobs/{id}", delete(job_remove))
        .route("/api/history", get(history))
        .route("/api/attention", get(attention))
        .route("/api/attention/{id}/dismiss", post(dismiss_attention))
        .route("/api/blocklist", get(blocklist))
        .route("/api/blocklist/{id}", delete(delete_blocklist))
        .route("/api/calendar", get(calendar))
        .route("/api/settings/general", get(get_general).put(put_general))
        .route("/api/settings/naming", get(get_naming).put(put_naming))
        .route("/api/settings/servers", get(get_servers).put(put_servers))
        .route("/api/settings/size-limits", get(get_size_limits).put(put_size_limits))
        .route("/api/servers/test", post(test_server))
        .route("/api/profiles", get(list_profiles).post(save_profile))
        .route("/api/profiles/{id}", put(save_profile).delete(delete_profile))
        .route("/api/qualities", get(qualities))
        .route("/api/profiles/fit", get(profile_fit))
        .route("/api/indexers", get(list_indexers).post(save_indexer))
        .route("/api/indexers/{id}", put(save_indexer).delete(delete_indexer))
        .route("/api/indexers/{id}/test", post(test_indexer))
        .route("/api/tasks/{name}", post(run_task))
        .route("/api/check-paths", get(check_paths))
        .route("/api/space", get(space))
        .route("/api/titles/{id}/free", post(free_title))
        .route("/api/recycle", get(recycle_info))
        .route("/api/recycle/empty", post(recycle_empty))
        .route("/api/archive", get(archive_list))
        .route("/api/archive/{id}", delete(archive_delete))
        .route("/api/archive/{id}/check", post(archive_check))
        .route("/api/archive/{id}/grab", post(archive_grab))
        .route("/api/archive/{id}/restore", post(archive_restore))
        .route("/api/archive/{id}/art", get(archive_art))
        .route("/api/plex", get(plex_report))
        .route("/api/plex/sync", post(plex_sync))
        .route("/api/plex/track", post(plex_track))
        .route("/api/art/{id}/{kind}", get(art))
        .route("/api/players", get(players))
        .route("/api/changes", get(changes))
        .route("/api/collections", get(collections).post(collection_new))
        .route("/api/collections/add", post(collection_add))
        .route("/api/collections/{id}", put(collection_rename).delete(collection_remove))
        .route("/api/collections/{id}/order", put(collection_order))
        .route("/api/collections/{id}/art", get(collection_art))
        .route("/api/film-part/{id}/art", get(film_part_art))
        .route("/api/titles/{id}/collection", put(title_collection))
        .route("/api/anime/{id}/art", get(anime_art))
        .route("/api/locations", get(locations))
        .route("/api/locations/{id}/look", post(location_look))
        .route("/api/locations/{id}/check", post(location_check))
        .route("/api/locations/{id}/found", get(location_found))
        .route("/api/locations/{id}/add", post(location_add))
        .route("/api/events", get(events))
}

/// Whether Spool can be restarted now without interrupting anything.
async fn busy(State(app): State<App>) -> Json<Value> {
    Json(json!(app.busy()))
}

/// The one file a linked machine has been given to make a compact version of, to whoever
/// holds the ticket made out for it. The ticket is the whole of the request.
async fn link_source(State(app): State<App>, Path(link): Path<u32>, req: axum::extract::Request) -> Response {
    let mut given = req.uri().query().unwrap_or("").split('&').filter_map(|pair| pair.strip_prefix("ticket="));
    let (Some(token), None) = (given.next(), given.next()) else { return StatusCode::BAD_REQUEST.into_response() };
    let Some(file_id) = app.remotes().iter().find(|r| r.conf.id == link).and_then(|r| r.source_of(token)) else { return StatusCode::UNAUTHORIZED.into_response() };
    // Only while that machine is making it: a ticket outlives the making, and one linked
    // machine has no business with what another was given.
    if !app.encodes.lock().now.iter().any(|n| n.file_id == file_id && n.link == link) {
        return StatusCode::GONE.into_response();
    }
    let path = app.db.file(file_id).ok().flatten().and_then(|f| Some(app.db.title(f.title_id).ok()??.file_path(&f)));
    let Some(path) = path else { return StatusCode::NOT_FOUND.into_response() };
    let head = req.method() == axum::http::Method::HEAD;
    ([(header::CACHE_CONTROL, "private, no-store")], crate::serve::send_file(&path, req.headers(), head).await).into_response()
}

pub fn router(app: App) -> Router {
    // The MCP endpoint checks its own key, then calls the API in-process.
    let mcp = crate::mcp::routes(app.clone(), api_routes().with_state(app.clone()));
    api_routes()
        .fallback(assets)
        .layer(axum::extract::DefaultBodyLimit::max(64 << 20))
        .layer(axum::middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app.clone())
        .merge(mcp)
        .layer(tower_http::compression::CompressionLayer::new())
        // A linked machine comes with a ticket, not the key to the app, and a film is sent as it is.
        .merge(Router::new().route("/link/source/{link}", get(link_source)).with_state(app))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_names_this_network_gives_reach_the_app() {
        for host in ["casper", "casper:7979", "CASPER.local:7979", "casper.tail1234.ts.net", "192.168.69.96:7979", "100.97.6.66", "[::1]:7979", "localhost:5173", "nas.home.arpa", "casper."] {
            assert!(local_name(host), "{host}");
        }
        for host in ["evil.example.com", "evil.example.com:7979", "casper.evil.com", "local.evil.com", "[nonsense]", ""] {
            assert!(!local_name(host), "{host}");
        }
    }

    #[test]
    fn a_change_sent_from_another_page_is_told_apart() {
        let h = |origin: Option<&str>| {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, "casper:7979".parse().unwrap());
            if let Some(o) = origin {
                h.insert(header::ORIGIN, o.parse().unwrap());
            }
            h
        };
        assert!(!from_elsewhere(&h(None)), "programs send no origin");
        assert!(!from_elsewhere(&h(Some("http://casper:7979"))));
        assert!(!from_elsewhere(&h(Some("http://CASPER:7979"))));
        assert!(from_elsewhere(&h(Some("https://evil.example.com"))));
        assert!(from_elsewhere(&h(Some("http://casper:8088"))), "another port on the same machine is another page");
        assert!(from_elsewhere(&h(Some("null"))));
        // What the web app's dev server passes on: the page's own name, as it was asked by.
        let mut dev = HeaderMap::new();
        dev.insert(header::HOST, "localhost:5173".parse().unwrap());
        dev.insert(header::ORIGIN, "http://localhost:5173".parse().unwrap());
        assert!(!from_elsewhere(&dev));
    }
}
