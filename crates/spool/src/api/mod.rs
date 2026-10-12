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
mod subtitles;
mod activity;
mod settings;
mod upkeep;
mod collections;
mod locations;
mod compact;
use titles::*;
use subtitles::*;
use activity::*;
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

fn authorized(app: &App, headers: &HeaderMap) -> bool {
    let g = app.settings.general();
    if g.password.is_empty() {
        return true;
    }
    if let Some(key) = headers.get("x-api-key").and_then(|k| k.to_str().ok()) {
        if !g.api_key.is_empty() && same(key, &g.api_key) {
            return true;
        }
    }
    session_cookie(headers).is_some_and(|c| app.sessions.lock().contains(&c))
}

/// Compare two secrets without the time taken giving away how much of a guess was right.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// Sessions outlive a restart, so signing in is not needed again after every upgrade.
fn save_sessions(app: &App) {
    let all: Vec<String> = app.sessions.lock().iter().cloned().collect();
    let _ = app.db.set_setting("sessions", &all);
}

async fn guard(State(app): State<App>, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let path = req.uri().path();
    let open = !path.starts_with("/api/") || path == "/api/login" || path == "/api/session";
    if open || authorized(&app, req.headers()) {
        next.run(req).await
    } else {
        ApiError(StatusCode::UNAUTHORIZED, "sign in required".into()).into_response()
    }
}

async fn login(State(app): State<App>, Json(body): Json<Value>) -> Response {
    const TRIES: u32 = 5;
    const LOCKED_FOR: i64 = 60;
    let g = app.settings.general();
    let now = crate::db::now();
    {
        // A run of wrong guesses shuts the door for a minute, which makes guessing hopeless.
        let mut f = app.login_failures.lock();
        if now - f.1 >= LOCKED_FOR {
            f.0 = 0;
        }
        if f.0 >= TRIES {
            return ApiError(StatusCode::TOO_MANY_REQUESTS, "too many wrong passwords; wait a minute and try again".into()).into_response();
        }
    }
    if g.password.is_empty() || body["password"].as_str().is_some_and(|p| crate::settings::verify_password(&g.password, p)) {
        *app.login_failures.lock() = (0, 0);
        let token = crate::settings::new_key();
        {
            let mut s = app.sessions.lock();
            // A household has a handful of browsers; past that, the oldest are not worth keeping.
            if s.len() >= 40 {
                s.clear();
            }
            s.insert(token.clone());
        }
        save_sessions(&app);
        ([(header::SET_COOKIE, format!("spool_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000"))], Json(json!({"ok": true}))).into_response()
    } else {
        {
            let mut f = app.login_failures.lock();
            *f = (f.0 + 1, now);
        }
        tracing::warn!("wrong password at sign-in");
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        ApiError(StatusCode::UNAUTHORIZED, "wrong password".into()).into_response()
    }
}

async fn logout(State(app): State<App>, headers: HeaderMap) -> Response {
    if let Some(c) = session_cookie(&headers) {
        app.sessions.lock().remove(&c);
        save_sessions(&app);
    }
    ([(header::SET_COOKIE, "spool_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0".to_string())], Json(json!({"ok": true}))).into_response()
}

async fn session(State(app): State<App>, headers: HeaderMap) -> Json<Value> {
    Json(json!({"required": !app.settings.general().password.is_empty(), "signed_in": authorized(&app, &headers)}))
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
        "signed_in": app.player_tokens.lock().len(),
        "playing": app.playing_now(),
        "requests": app.player_requests.lock().iter().cloned().collect::<Vec<_>>(),
    }))
}

async fn events(State(app): State<App>) -> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let stream = BroadcastStream::new(app.events.subscribe()).filter_map(|e| match e {
        Ok(ev) => serde_json::to_string(&ev).ok().map(|s| Ok(SseEvent::default().data(s))),
        // A slow client missed events: tell it to reload its state.
        Err(_) => Some(Ok(SseEvent::default().data(r#"{"type":"resync"}"#))),
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
}

async fn assets(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
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
        .route("/api/links", get(links))
        .route("/api/links/pair", post(link_pair))
        .route("/api/titles/move", post(move_titles))
        .route("/api/titles/move/cancel", post(cancel_moves))
        .route("/api/blocklist/clear", post(clear_blocklist))
        .route("/api/attention/dismiss-all", post(dismiss_all_attention))
        .route("/api/titles/{id}", get(get_title).patch(patch_title).delete(delete_title))
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

pub fn router(app: App) -> Router {
    // The MCP endpoint checks its own key, then calls the API in-process.
    let mcp = crate::mcp::routes(app.clone(), api_routes().with_state(app.clone()));
    api_routes()
        .fallback(assets)
        .layer(axum::extract::DefaultBodyLimit::max(64 << 20))
        .layer(axum::middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
        .merge(mcp)
        .layer(tower_http::compression::CompressionLayer::new())
}
