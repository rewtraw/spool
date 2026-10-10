//! HTTP API and the embedded web app.

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
    if g.password.is_empty() || body["password"].as_str().is_some_and(|p| same(p, &g.password)) {
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

// ---------------------------------------------------------------- titles

fn summarize(t: &Title, files: &HashMap<i64, (u32, u64)>, eps: &HashMap<i64, (u32, u32)>, profiles: &[QualityProfile], active: &[Acquisition]) -> Value {
    let mut v = serde_json::to_value(t).unwrap_or(Value::Null);
    v["sort_title"] = json!(crate::store::sort_title(&t.title));
    v["poster"] = json!(crate::art::local_url(t, crate::art::Art::Poster));
    v["fanart"] = json!(crate::art::local_url(t, crate::art::Art::Fanart));
    let f = files.get(&t.id).copied().unwrap_or((0, 0));
    let e = eps.get(&t.id).copied().unwrap_or((0, 0));
    v["file_count"] = json!(f.0);
    v["size"] = json!(f.1);
    v["episodes_aired"] = json!(e.0);
    v["episodes_have"] = json!(e.1);
    v["profile"] = json!(profiles.iter().find(|p| p.id == t.profile_id).map(|p| p.name.clone()));
    v["active"] = json!(active.iter().find(|a| a.title_id == t.id).map(|a| a.state.as_str()));
    v["available"] = json!(t.kind == Kind::Series || t.movie_available(chrono::Local::now().date_naive()));
    v
}

async fn list_titles(State(app): State<App>) -> R<Vec<Value>> {
    let (files, eps, profiles, active) = (app.db.file_counts()?, app.db.episode_counts()?, app.db.profiles()?, app.db.active_acquisitions()?);
    // The list feeds grids and tables; long text stays on the title's own page.
    Ok(Json(
        app.db
            .titles(None)?
            .iter()
            .map(|t| {
                let mut v = summarize(t, &files, &eps, &profiles, &active);
                if let Some(o) = v.as_object_mut() {
                    for k in ["overview", "alt_titles", "alt_years", "genres", "seasons", "fanart", "path", "studio", "network", "last_search_at", "last_refresh_at"] {
                        o.remove(k);
                    }
                }
                v
            })
            .collect(),
    ))
}

async fn get_title(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let t = app.db.title(id)?.ok_or_else(|| not_found("title"))?;
    let (files, eps, profiles, active) = (app.db.file_counts()?, app.db.episode_counts()?, app.db.profiles()?, app.db.active_acquisitions()?);
    let mut v = summarize(&t, &files, &eps, &profiles, &active);
    v["episodes"] = json!(app.db.episodes(id)?);
    v["files"] = json!(app.db.files(id)?);
    v["decisions"] = json!(app.rejudge(&app.title_context(t.clone())?, app.db.decisions(id)?));
    v["history"] = json!(app.db.history(Some(id), 100)?);
    v["acquisitions"] = json!(app.db.title_acquisitions(id)?);
    v["saved"] = json!(app.archive_for(t.kind, t.tmdb_id, t.tvdb_id)?);
    if let Ok(idx) = app.plex_index() {
        if let Some(item) = idx.find(&t) {
            v["plex"] = json!({"rating_key": item.rating_key, "url": idx.url(item), "section": item.section});
        }
    }
    Ok(Json(v))
}

#[derive(Deserialize)]
struct AddTitle {
    kind: Kind,
    tmdb_id: Option<u32>,
    tvmaze_id: Option<u32>,
    profile_id: i64,
    #[serde(default = "yes")]
    monitored: bool,
    #[serde(default)]
    search: bool,
}
fn yes() -> bool {
    true
}

async fn add_title(State(app): State<App>, Json(b): Json<AddTitle>) -> R<Title> {
    Ok(Json(app.add_title(b.kind, b.tmdb_id, b.tvmaze_id, b.profile_id, b.monitored, b.search).await?))
}

async fn patch_title(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<Value>) -> R<Title> {
    let mut t = app.db.title(id)?.ok_or_else(|| not_found("title"))?;
    if let Some(m) = b["monitored"].as_bool() {
        t.monitored = m;
    }
    if let Some(p) = b["profile_id"].as_i64() {
        t.profile_id = p;
    }
    if let Some(a) = b["minimum_availability"].as_str() {
        t.minimum_availability = a.to_string();
    }
    if let Some(p) = b["path"].as_str() {
        t.path = p.to_string();
    }
    app.db.save_title(&mut t)?;
    app.emit(Event::Title { id });
    Ok(Json(t))
}

async fn delete_title(State(app): State<App>, Path(id): Path<i64>, Query(q): Query<HashMap<String, String>>) -> R<Value> {
    let t = app.db.title(id)?.ok_or_else(|| not_found("title"))?;
    for a in app.db.title_acquisitions(id)?.into_iter().filter(|a| a.state.is_active()) {
        app.cancel_acquisition(a.id, false).await?;
    }
    if q.get("delete_files").map(|v| v == "true").unwrap_or(false) {
        for f in app.db.files(id)? {
            app.delete_file(f.id)?;
        }
    }
    app.db.delete_title(id)?;
    app.emit(Event::Title { id });
    Ok(Json(json!({"deleted": t.title})))
}

/// One action over many titles. Quick changes happen before the reply; searches and refreshes
/// carry on in the background, one title at a time, and report through the "bulk" task.
async fn bulk_titles(State(app): State<App>, Json(b): Json<Value>) -> R<Value> {
    let ids: Vec<i64> = b["ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default();
    let action = b["action"].as_str().unwrap_or_default().to_string();
    if ids.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "no titles chosen".into()));
    }
    let mut done = 0;
    let mut bytes = 0u64;
    match action.as_str() {
        "monitor" | "unmonitor" | "profile" => {
            let profile = app.db.profiles()?.into_iter().find(|p| Some(p.id) == b["profile_id"].as_i64());
            if action == "profile" && profile.is_none() {
                return Err(ApiError(StatusCode::BAD_REQUEST, "no such profile".into()));
            }
            for id in &ids {
                let Some(mut t) = app.db.title(*id)? else { continue };
                match (action.as_str(), &profile) {
                    ("monitor", _) => t.monitored = true,
                    ("unmonitor", _) => t.monitored = false,
                    // A film profile means nothing to a series, and the other way round.
                    (_, Some(p)) if (p.kind == "movie") == (t.kind == Kind::Movie) => t.profile_id = p.id,
                    _ => continue,
                }
                app.db.save_title(&mut t)?;
                app.emit(Event::Title { id: *id });
                done += 1;
            }
        }
        "free" | "remove" => {
            let delete_files = action == "free" || b["delete_files"].as_bool().unwrap_or(false);
            for id in &ids {
                let Some(mut t) = app.db.title(*id)? else { continue };
                for a in app.db.title_acquisitions(*id)?.into_iter().filter(|a| a.state.is_active()) {
                    app.cancel_acquisition(a.id, false).await?;
                }
                if delete_files {
                    for f in app.db.files(*id)? {
                        bytes += f.size;
                        app.delete_file(f.id)?;
                    }
                }
                if action == "remove" {
                    app.db.delete_title(*id)?;
                } else {
                    // Left monitored, Spool would only fetch it again.
                    t.monitored = false;
                    app.db.save_title(&mut t)?;
                }
                app.emit(Event::Title { id: *id });
                done += 1;
            }
        }
        "search" | "compact" | "refresh" => {
            if action != "refresh" && !app.is_active() {
                return Err(ApiError(StatusCode::CONFLICT, "Spool is in shadow mode and does not download".into()));
            }
            let n = ids.len();
            let what = match action.as_str() {
                "search" => "Searching",
                "compact" => "Looking for smaller copies of",
                _ => "Refreshing",
            };
            if !app.task_start("bulk", &format!("{what} {n} titles")) {
                return Err(ApiError(StatusCode::CONFLICT, "an earlier bulk search or refresh is still running".into()));
            }
            let app2 = app.clone();
            tokio::spawn(async move {
                let (mut ok, mut started) = (0, 0);
                for id in ids {
                    let Ok(Some(t)) = app2.db.title(id) else { continue };
                    let r = match action.as_str() {
                        "refresh" => app2.refresh_title(id).await.map(|_| 0),
                        // Under the automatic rules for disk space, so a long list cannot queue
                        // more than the disk has room to wait for.
                        "compact" => app2.search_with(id, Scope::Compact, true, true, false).await.map(|o| o.grabbed.len()),
                        _ => app2.search_with(id, if t.kind == Kind::Movie { Scope::Movie } else { Scope::Missing }, true, true, false).await.map(|o| o.grabbed.len()),
                    };
                    match r {
                        Ok(g) => {
                            ok += 1;
                            started += g;
                        }
                        Err(e) => tracing::warn!(title = %t.title, error = %e, "bulk {action} failed"),
                    }
                }
                let message = if action == "refresh" { format!("refreshed {ok} of {n} titles") } else { format!("searched {ok} of {n} titles, {started} {} started", if started == 1 { "download" } else { "downloads" }) };
                app2.task_end("bulk", &message);
            });
            return Ok(Json(json!({"started": n})));
        }
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "unknown action".into())),
    }
    Ok(Json(json!({"done": done, "bytes": bytes})))
}

async fn patch_season(State(app): State<App>, Path((id, season)): Path<(i64, u32)>, Json(b): Json<Value>) -> R<Value> {
    let mut t = app.db.title(id)?.ok_or_else(|| not_found("title"))?;
    let monitored = b["monitored"].as_bool().unwrap_or(true);
    match t.seasons.iter_mut().find(|s| s.number == season) {
        Some(s) => s.monitored = monitored,
        None => t.seasons.push(SeasonInfo { number: season, monitored }),
    }
    app.db.save_title(&mut t)?;
    app.db.set_season_monitored(id, season, monitored)?;
    app.emit(Event::Title { id });
    Ok(Json(json!({"ok": true})))
}

async fn patch_episode(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<Value>) -> R<Value> {
    let e = app.db.episode(id)?.ok_or_else(|| not_found("episode"))?;
    app.db.set_episode_monitored(&[id], b["monitored"].as_bool().unwrap_or(true))?;
    app.emit(Event::Title { id: e.title_id });
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct SearchBody {
    #[serde(default)]
    season: Option<u32>,
    #[serde(default)]
    episode: Option<i64>,
    #[serde(default)]
    grab: bool,
    /// Look for smaller copies of files over the profile's size target.
    #[serde(default)]
    compact: bool,
}

async fn search_title(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<SearchBody>) -> R<crate::acquire::SearchOutcome> {
    let t = app.db.title(id)?.ok_or_else(|| not_found("title"))?;
    let scope = match (t.kind, b.episode, b.season) {
        _ if b.compact => Scope::Compact,
        (Kind::Movie, _, _) => Scope::Movie,
        (_, Some(e), _) => Scope::Episode(e),
        (_, _, Some(s)) => Scope::Season(s),
        _ => Scope::Missing,
    };
    Ok(Json(app.search(id, scope, true, b.grab).await?))
}

async fn grab_decision(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let d = app.db.decision(id)?.ok_or_else(|| not_found("release"))?;
    let t = app.db.title(d.title_id)?.ok_or_else(|| not_found("title"))?;
    if !app.is_active() {
        return Err(ApiError(StatusCode::CONFLICT, "Spool is in shadow mode and does not download".into()));
    }
    let a = app.grab(&d, &t, crate::acquire::BY_HAND_REASON, true).await?;
    Ok(Json(json!({"acquisition": a})))
}

async fn refresh_title(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.refresh_title(id).await?;
    Ok(Json(json!({"ok": true})))
}

async fn scan_title(State(app): State<App>, Path(id): Path<i64>) -> R<crate::catalog::ScanReport> {
    let mut r = crate::catalog::ScanReport::default();
    app.scan_title(id, &mut r).await?;
    app.emit(Event::Title { id });
    Ok(Json(r))
}

async fn delete_file(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.delete_file(id)?;
    Ok(Json(json!({"ok": true})))
}

async fn upload_nzb(State(app): State<App>, Path(id): Path<i64>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> R<Acquisition> {
    let t = app.db.title(id)?.ok_or_else(|| not_found("title"))?;
    let name = q.get("name").cloned().unwrap_or_else(|| format!("{}.nzb", t.title));
    let episodes: Vec<i64> = q.get("episodes").map(|e| e.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
    Ok(Json(app.add_manual_nzb(&t, episodes, &name, &body)?))
}

async fn lookup(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R<Vec<Value>> {
    let query = q.get("q").cloned().unwrap_or_default();
    if query.trim().is_empty() {
        return Ok(Json(vec![]));
    }
    let kind = q.get("kind").and_then(|k| Kind::parse(k)).unwrap_or(Kind::Movie);
    let found = match kind {
        Kind::Movie => app.meta.search_movies(&app.settings.general().tmdb_api_key, &query).await?,
        Kind::Series => app.meta.search_series(&query).await?,
    };
    let mut out = vec![];
    for t in found {
        let existing = app.db.find_title(kind, t.tmdb_id, t.tvdb_id)?.map(|e| e.id);
        let mut v = serde_json::to_value(&t)?;
        v["library_id"] = json!(existing);
        out.push(v);
    }
    Ok(Json(out))
}

// ---------------------------------------------------------------- activity

async fn activity(State(app): State<App>) -> R<Value> {
    let jobs = app.engine.jobs();
    let titles: HashMap<i64, Title> = app.db.titles(None)?.into_iter().map(|t| (t.id, t)).collect();
    // Everything still in progress, however long the queue, then the most recent finished ones.
    let mut recent = app.db.active_acquisitions()?;
    recent.sort_by_key(|a| a.id);
    // The current run reaches back to when the oldest download still going was added, so what
    // has finished since then keeps counting towards the queue's progress.
    let run_start = recent.iter().filter(|a| a.state == AcqState::Downloading).map(|a| a.created_at).min();
    let (finished, finished_bytes) = match run_start {
        Some(since) => app.db.imported_since(since)?,
        None => (0, 0),
    };
    for a in app.db.recent_acquisitions(60)? {
        if !recent.iter().any(|x| x.id == a.id) {
            recent.push(a);
        }
    }
    let rows: Vec<Value> = recent
        .iter()
        .map(|a| {
            let t = titles.get(&a.title_id);
            json!({
                "acquisition": a,
                "title": t.map(|t| t.title.clone()),
                "kind": t.map(|t| t.kind),
                "poster": t.and_then(|t| crate::art::local_url(t, crate::art::Art::Poster)),
                "job": a.job_id.as_ref().and_then(|id| jobs.iter().find(|j| j.id == *id)),
            })
        })
        .collect();
    let known: Vec<&str> = recent.iter().filter_map(|a| a.job_id.as_deref()).collect();
    let orphans: Vec<&spool_nntp::JobStatus> = jobs.iter().filter(|j| !known.contains(&j.id.as_str())).collect();
    Ok(Json(json!({"items": rows, "orphan_jobs": orphans, "paused": app.engine.is_paused(), "speed_limit": app.settings.speed_limit(), "finished": {"count": finished, "bytes": finished_bytes}})))
}

async fn activity_action(State(app): State<App>, Path((id, action)): Path<(i64, String)>, body: Option<Json<Value>>) -> R<Value> {
    let acq = app.db.acquisition(id)?.ok_or_else(|| not_found("download"))?;
    match action.as_str() {
        "cancel" => app.cancel_acquisition(id, false).await?,
        "cancel_blocklist" => {
            app.cancel_acquisition(id, true).await?;
            let scope = match (app.db.title(acq.title_id)?.map(|t| t.kind), acq.episode_ids.as_slice()) {
                (Some(Kind::Movie), _) => Scope::Movie,
                (_, [one]) => Scope::Episode(*one),
                _ => Scope::Missing,
            };
            let app2 = app.clone();
            tokio::spawn(async move {
                let _ = app2.search(acq.title_id, scope, false, true).await;
            });
        }
        "pause" => {
            if let Some(j) = &acq.job_id {
                app.waiting_for_space.lock().remove(j);
                app.engine.pause(j)?;
            }
        }
        "resume" => {
            if let Some(j) = &acq.job_id {
                // The user's word overrides the free-space hold for this job.
                app.waiting_for_space.lock().insert(j.clone());
                app.engine.resume(j)?;
            }
        }
        "retry_import" => {
            let mut a = acq.clone();
            if matches!(a.state, AcqState::ImportBlocked | AcqState::Importing) {
                a.state = AcqState::Importing;
                app.db.save_acquisition(&mut a)?;
            }
            let episodes: Option<Vec<i64>> = body.and_then(|b| b.0["episode_ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()));
            let force = episodes.clone().or_else(|| app.db.title(acq.title_id).ok().flatten().filter(|t| t.kind == Kind::Movie).map(|_| vec![]));
            app.import(id, if episodes.is_some() || force.is_some() { force } else { None }).await?;
        }
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "unknown action".into())),
    }
    Ok(Json(json!({"ok": true})))
}

async fn queue_action(State(app): State<App>, Path(action): Path<String>) -> R<Value> {
    match action.as_str() {
        "pause" => app.engine.pause_all(),
        "resume" => app.engine.resume_all(),
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "unknown action".into())),
    }
    Ok(Json(json!({"paused": app.engine.is_paused()})))
}

async fn job_remove(State(app): State<App>, Path(id): Path<String>) -> R<Value> {
    app.engine.remove(&id, true)?;
    Ok(Json(json!({"ok": true})))
}

async fn history(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R<Value> {
    let titles: HashMap<i64, String> = app.db.titles(None)?.into_iter().map(|t| (t.id, t.title)).collect();
    let rows = app.db.history(q.get("title_id").and_then(|t| t.parse().ok()), q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(200))?;
    Ok(Json(json!(rows.into_iter().map(|h| json!({"id": h.id, "ts": h.ts, "kind": h.kind, "title_id": h.title_id, "title": h.title_id.and_then(|i| titles.get(&i).cloned()), "data": h.data})).collect::<Vec<_>>())))
}

async fn attention(State(app): State<App>) -> R<Value> {
    let titles: HashMap<i64, Title> = app.db.titles(None)?.into_iter().map(|t| (t.id, t)).collect();
    let rows = app.db.attention(false)?;
    Ok(Json(json!(rows.into_iter().map(|a| {
        let t = a.title_id.and_then(|i| titles.get(&i));
        json!({"item": a, "title": t.map(|t| t.title.clone()), "kind": t.map(|t| t.kind), "poster": t.and_then(|t| crate::art::local_url(t, crate::art::Art::Poster))})
    }).collect::<Vec<_>>())))
}

async fn dismiss_attention(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.db.resolve_attention(id)?;
    app.emit(Event::Attention);
    Ok(Json(json!({"ok": true})))
}

async fn dismiss_all_attention(State(app): State<App>) -> R<Value> {
    let rows = app.db.attention(false)?;
    for a in &rows {
        app.db.resolve_attention(a.id)?;
    }
    app.emit(Event::Attention);
    Ok(Json(json!({"dismissed": rows.len()})))
}

async fn clear_blocklist(State(app): State<App>) -> R<Value> {
    let rows = app.db.blocklist()?;
    for id in rows.iter().filter_map(|b| b["id"].as_i64()) {
        app.db.delete_blocklist(id)?;
    }
    Ok(Json(json!({"cleared": rows.len()})))
}

async fn blocklist(State(app): State<App>) -> R<Vec<Value>> {
    Ok(Json(app.db.blocklist()?))
}

async fn delete_blocklist(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.db.delete_blocklist(id)?;
    Ok(Json(json!({"ok": true})))
}

async fn calendar(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R<Value> {
    let today = chrono::Local::now().date_naive();
    let from = q.get("from").cloned().unwrap_or_else(|| (today - chrono::Duration::days(7)).to_string());
    let to = q.get("to").cloned().unwrap_or_else(|| (today + chrono::Duration::days(28)).to_string());
    let titles: HashMap<i64, Title> = app.db.titles(None)?.into_iter().map(|t| (t.id, t)).collect();
    let mut items: Vec<Value> = app
        .db
        .episodes_between(&from, &to)?
        .into_iter()
        .filter_map(|e| {
            let t = titles.get(&e.title_id)?;
            Some(json!({"date": e.air_date, "kind": "episode", "title_id": t.id, "title": t.title, "poster": crate::art::local_url(t, crate::art::Art::Poster), "label": format!("S{:02}E{:02} · {}", e.season, e.episode, e.title),
                "have": e.file_id.is_some(), "monitored": e.monitored && t.monitored}))
        })
        .collect();
    for t in titles.values().filter(|t| t.kind == Kind::Movie) {
        for (d, label) in [(&t.in_cinemas, "In cinemas"), (&t.digital_release, "Digital release"), (&t.physical_release, "Physical release")] {
            if let Some(d) = d.as_deref().and_then(|d| d.get(..10)) {
                if d >= from.as_str() && d <= to.as_str() {
                    items.push(json!({"date": d, "kind": "movie", "title_id": t.id, "title": t.title, "poster": crate::art::local_url(t, crate::art::Art::Poster), "label": label, "have": false, "monitored": t.monitored}));
                }
            }
        }
    }
    items.sort_by(|a, b| a["date"].as_str().cmp(&b["date"].as_str()));
    Ok(Json(json!(items)))
}

// ---------------------------------------------------------------- settings

async fn get_general(State(app): State<App>) -> Json<General> {
    let mut g = app.settings.general();
    g.tmdb_api_key = mask(&g.tmdb_api_key);
    g.plex_token = mask(&g.plex_token);
    g.password = mask(&g.password);
    Json(g)
}

async fn put_general(State(app): State<App>, headers: HeaderMap, Json(mut g): Json<General>) -> Result<Response, ApiError> {
    let old = app.settings.general();
    g.tmdb_api_key = unmask(g.tmdb_api_key, &old.tmdb_api_key);
    g.plex_token = unmask(g.plex_token, &old.plex_token);
    g.password = unmask(g.password, &old.password);
    let mut cookie = None;
    if g.password != old.password {
        // A new password signs every other browser out; the one that set it stays in.
        let mine = session_cookie(&headers).filter(|c| app.sessions.lock().contains(c)).unwrap_or_else(|| {
            let token = crate::settings::new_key();
            cookie = Some(format!("spool_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000"));
            token
        });
        *app.sessions.lock() = HashSet::from([mine]);
        save_sessions(&app);
    }
    if g.mode == crate::settings::Mode::Active && old.mode != g.mode {
        if !app.volume_ok() {
            return Err(ApiError(StatusCode::CONFLICT, "the media volume is not mounted".into()));
        }
        tracing::warn!("Spool is now active: it will download and write to the library");
    }
    app.settings.set_general(&g)?;
    app.apply_engine_config();
    Ok(match cookie {
        Some(c) => ([(header::SET_COOKIE, c)], Json(json!({"ok": true}))).into_response(),
        None => Json(json!({"ok": true})).into_response(),
    })
}

async fn get_naming(State(app): State<App>) -> Json<spool_core::naming::NamingConfig> {
    Json(app.settings.naming())
}

async fn put_naming(State(app): State<App>, Json(n): Json<spool_core::naming::NamingConfig>) -> R<Value> {
    app.settings.set_naming(&n)?;
    Ok(Json(json!({"ok": true})))
}

async fn get_size_limits(State(app): State<App>) -> Json<crate::settings::SizeLimits> {
    Json(app.settings.size_limits())
}

async fn put_size_limits(State(app): State<App>, Json(mut limits): Json<crate::settings::SizeLimits>) -> R<Value> {
    // An entry with no minimum and no maximum says nothing; drop it.
    limits.retain(|_, l| l.min > 0.0 || l.max.is_some_and(|m| m > 0.0));
    for l in limits.values_mut() {
        l.max = l.max.filter(|m| *m > 0.0);
    }
    app.settings.set_size_limits(&limits)?;
    Ok(Json(json!({"ok": true})))
}

async fn get_servers(State(app): State<App>) -> Json<Value> {
    let mut s = app.settings.servers();
    for x in &mut s {
        x.password = mask(&x.password);
    }
    Json(json!({"servers": s, "speed_limit": app.settings.speed_limit()}))
}

fn merge_server_secrets(app: &App, servers: &mut [spool_nntp::ServerConfig]) {
    let old = app.settings.servers();
    for s in servers.iter_mut() {
        if s.id.is_empty() {
            s.id = format!("{}:{}", s.host, s.port);
        }
        if s.name.is_empty() {
            s.name = s.host.clone();
        }
        if s.password == MASK {
            s.password = old.iter().find(|o| o.id == s.id).map(|o| o.password.clone()).unwrap_or_default();
        }
    }
}

async fn put_servers(State(app): State<App>, Json(b): Json<Value>) -> R<Value> {
    let mut servers: Vec<spool_nntp::ServerConfig> = serde_json::from_value(b["servers"].clone())?;
    merge_server_secrets(&app, &mut servers);
    app.settings.set_servers(&servers)?;
    if let Some(l) = b["speed_limit"].as_u64() {
        app.settings.set_speed_limit(l)?;
    }
    app.apply_engine_config();
    Ok(Json(json!({"ok": true})))
}

async fn test_server(State(app): State<App>, Json(s): Json<spool_nntp::ServerConfig>) -> R<Value> {
    let mut one = [s];
    merge_server_secrets(&app, &mut one);
    match spool_nntp::Engine::test_server(&one[0]).await {
        Ok(()) => Ok(Json(json!({"ok": true, "message": "Connected and signed in"}))),
        Err(e) => Ok(Json(json!({"ok": false, "message": e}))),
    }
}

async fn list_profiles(State(app): State<App>) -> R<Vec<QualityProfile>> {
    Ok(Json(app.db.profiles()?))
}

async fn save_profile(State(app): State<App>, Json(mut p): Json<QualityProfile>) -> R<QualityProfile> {
    if p.items.iter().all(|i| !i.allowed) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "a profile must allow at least one quality".into()));
    }
    app.db.save_profile(&mut p)?;
    Ok(Json(p))
}

async fn delete_profile(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let users = app.db.titles(None)?.iter().filter(|t| t.profile_id == id).count();
    if users > 0 {
        return Err(ApiError(StatusCode::CONFLICT, format!("{users} titles use this profile")));
    }
    app.db.delete_profile(id)?;
    Ok(Json(json!({"ok": true})))
}

async fn qualities() -> Json<Vec<Value>> {
    Json(Quality::ALL.iter().filter(|q| **q != Quality::Unknown).map(|q| json!({"key": q.key(), "movie": q.name(Flavor::Movie), "tv": q.name(Flavor::Tv), "resolution": q.resolution(), "weight": q.default_weight()})).collect())
}

async fn list_indexers(State(app): State<App>) -> R<Vec<Value>> {
    let budgets = app.indexer_budgets();
    let backoff = app.indexer_backoff.lock().iter().map(|(id, (_, until, why))| (*id, (until.saturating_duration_since(std::time::Instant::now()).as_secs() / 60 + 1, why.clone()))).collect::<HashMap<_, _>>();
    Ok(Json(
        app.db
            .indexers()?
            .into_iter()
            .map(|mut i| {
                i.api_key = mask(&i.api_key);
                let mut v = serde_json::to_value(&i).unwrap_or(Value::Null);
                v["usage"] = json!(budgets.get(&i.id));
                // An indexer that keeps failing is left alone for a while; say so and why.
                v["failing"] = json!(backoff.get(&i.id).map(|(minutes, why)| json!({"retry_in_minutes": minutes, "error": why})));
                v
            })
            .collect(),
    ))
}

async fn save_indexer(State(app): State<App>, Json(mut i): Json<Indexer>) -> R<Indexer> {
    if i.api_key == MASK {
        i.api_key = app.db.indexers()?.into_iter().find(|o| o.id == i.id).map(|o| o.api_key).unwrap_or_default();
    }
    app.db.save_indexer(&mut i)?;
    i.api_key = mask(&i.api_key);
    Ok(Json(i))
}

async fn delete_indexer(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.db.delete_indexer(id)?;
    Ok(Json(json!({"ok": true})))
}

async fn test_indexer(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let ix = app.db.indexers()?.into_iter().find(|i| i.id == id).ok_or_else(|| not_found("indexer"))?;
    let q = if ix.movies { crate::newznab::Query::RecentMovies } else { crate::newznab::Query::RecentTv };
    Ok(Json(match crate::newznab::fetch(&app.http, &ix, &q).await {
        Ok((r, limits)) => {
            if let Some(l) = limits {
                app.note_indexer_limits(ix.id, l);
            }
            let part = |used: Option<u32>, max: Option<u32>, what: &str| match (used, max) {
                (Some(u), Some(m)) => format!("{u} of {m} {what}"),
                (Some(u), None) => format!("{u} {what} with no limit"),
                (None, Some(m)) => format!("a limit of {m} {what}"),
                (None, None) => String::new(),
            };
            let allowance = match limits {
                Some(l) => format!(". Today it reports {}", [part(l.api_current, l.api_max, "requests"), part(l.grab_current, l.grab_max, "downloads")].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" and ")),
                None => ". It does not report a daily allowance".into(),
            };
            json!({"ok": true, "message": format!("{} recent releases returned{allowance}", r.len())})
        }
        Err(e) => json!({"ok": false, "message": e.to_string()}),
    }))
}

async fn run_task(State(app): State<App>, Path(name): Path<String>) -> R<Value> {
    if !["rss", "backlog", "refresh", "scan", "housekeeping", "plex", "compact"].contains(&name.as_str()) {
        return Err(not_found("task"));
    }
    let app2 = app.clone();
    tokio::spawn(async move {
        app2.run_task(&name).await;
    });
    Ok(Json(json!({"started": true})))
}

/// The archive, grouped by film or series, whether or not each is still in the library.
async fn archive_list(State(app): State<App>) -> R<Value> {
    let saved = app.archive_all()?;
    let titles = app.db.titles(None)?;
    let bytes: u64 = saved.iter().map(|n| n.stored_bytes).sum();
    let count = saved.len();
    let mut groups: Vec<Value> = vec![];
    let mut index: HashMap<(Kind, u32, u32), usize> = HashMap::new();
    for n in &saved {
        let key = (n.kind, n.tmdb_id.unwrap_or(0), n.tvdb_id.unwrap_or(0));
        let at = *index.entry(key).or_insert_with(|| {
            let owner = titles.iter().find(|t| t.kind == n.kind && ((n.tmdb_id.is_some() && t.tmdb_id == n.tmdb_id) || (n.tvdb_id.is_some() && t.tvdb_id == n.tvdb_id)));
            let poster = match owner {
                Some(t) => crate::art::local_url(t, crate::art::Art::Poster),
                None => n.poster.as_deref().map(|src| crate::art::saved_url(n.id, src)),
            };
            groups.push(json!({"kind": n.kind, "name": owner.map(|t| t.title.clone()).unwrap_or_else(|| n.name.clone()), "year": n.year, "tmdb_id": n.tmdb_id, "tvdb_id": n.tvdb_id,
                "title_id": owner.map(|t| t.id), "poster": poster, "releases": [], "size": 0u64, "latest": 0i64}));
            groups.len() - 1
        });
        let mut v = serde_json::to_value(n).unwrap_or(Value::Null);
        v["usable"] = json!(n.usable());
        if let Some(o) = v.as_object_mut() {
            o.remove("poster");
        }
        if let Some(o) = v["release"].as_object_mut() {
            // Indexer addresses can carry the account's key and have no place in a response.
            o.remove("link");
            o.remove("guid");
            o.remove("info_url");
        }
        let g = &mut groups[at];
        g["size"] = json!(g["size"].as_u64().unwrap_or(0) + n.release.size);
        g["latest"] = json!(g["latest"].as_i64().unwrap_or(0).max(n.fetched_at));
        if let Some(a) = g["releases"].as_array_mut() {
            a.push(v);
        }
    }
    Ok(Json(json!({"groups": groups, "count": count, "stored_bytes": bytes})))
}

async fn archive_art(State(app): State<App>, Path(id): Path<i64>) -> Response {
    match app.archive_art(id).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], bytes).into_response(),
        Err(_) => ([(header::CACHE_CONTROL, "public, max-age=600")], StatusCode::NOT_FOUND).into_response(),
    }
}

async fn archive_restore(State(app): State<App>, Path(id): Path<i64>) -> R<Title> {
    Ok(Json(app.archive_restore(id).await?))
}

async fn archive_delete(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.archive_delete(id)?;
    Ok(Json(json!({"ok": true})))
}

async fn archive_check(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let n = app.archive_check(id).await?;
    Ok(Json(json!({"available": n.available, "usable": n.usable(), "checked_at": n.checked_at})))
}

async fn archive_grab(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    if !app.is_active() {
        return Err(ApiError(StatusCode::CONFLICT, "Spool is in shadow mode and does not download".into()));
    }
    let a = app.grab_saved(id).await?;
    Ok(Json(json!({"acquisition": a})))
}

async fn plex_report(State(app): State<App>) -> R<crate::plex::PlexReport> {
    Ok(Json(app.plex_report()?))
}

async fn plex_sync(State(app): State<App>) -> R<Value> {
    let message = app.run_task("plex").await;
    Ok(Json(json!({"message": message})))
}

#[derive(Deserialize)]
struct PlexTrack {
    rating_key: String,
}

async fn plex_track(State(app): State<App>, Json(b): Json<PlexTrack>) -> R<Title> {
    Ok(Json(app.plex_track(&b.rating_key).await?))
}

/// Where the disk went and what could be removed: the largest titles, whether each has been
/// watched in Plex, and whether it could be downloaded again from a saved release.
async fn space(State(app): State<App>) -> R<Value> {
    let g = app.settings.general();
    let titles = app.db.titles(None)?;
    let files = app.db.file_counts()?;
    let plex = app.plex_index().ok();
    let profiles = app.db.profiles()?;
    let saved = app.archive_all()?;
    let (recycled_files, recycled_bytes) = app.recycle_usage();
    let jobs = app.engine.jobs();
    let waiting: Vec<&spool_nntp::JobStatus> = jobs.iter().filter(|j| j.state == spool_nntp::JobState::Paused && j.message == "Waiting for free space").collect();
    let mut rows: Vec<(u64, Value)> = titles
        .iter()
        .filter_map(|t| {
            let (count, bytes) = files.get(&t.id).copied()?;
            if count == 0 {
                return None;
            }
            let item = plex.as_ref().and_then(|p| p.find(t));
            let ceiling = profiles.iter().find(|p| p.id == t.profile_id).and_then(|p| p.size_ceiling());
            let over: u64 = match ceiling {
                Some(c) => app.db.files(t.id).unwrap_or_default().iter().map(|f| f.size.saturating_sub(c)).sum(),
                None => 0,
            };
            let mine: Vec<&crate::archive::SavedNzb> = saved.iter().filter(|n| n.kind == t.kind && ((n.tmdb_id.is_some() && n.tmdb_id == t.tmdb_id) || (n.tvdb_id.is_some() && n.tvdb_id == t.tvdb_id))).collect();
            Some((bytes, json!({
                "title_id": t.id, "title": t.title, "year": t.year, "kind": t.kind, "monitored": t.monitored, "files": count, "size": bytes,
                "poster": crate::art::local_url(t, crate::art::Art::Poster),
                // Bytes above the profile's size target: roughly what a smaller copy would save.
                "over_target": over,
                "in_plex": item.is_some(),
                // Plex sometimes sees fewer episodes than are on disk; that is not "watched it all".
                "watched": item.map(|i| i.fully_watched() && (t.kind == Kind::Movie || i.items >= count)),
                "watched_count": item.map(|i| i.watched),
                "plex_items": item.map(|i| i.items),
                "last_viewed_at": item.map(|i| i.last_viewed_at).filter(|t| *t > 0),
                "saved_releases": mine.len(),
                // A saved release found gone at its last check is no way back.
                "can_download_again": mine.iter().any(|n| n.usable() != Some(false)),
            })))
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    Ok(Json(json!({
        "free": crate::app::free_space(&app.engine.config().incomplete_dir),
        "kept_free": g.min_free_gb as u64 * (1 << 30),
        "library": files.values().map(|f| f.1).sum::<u64>(),
        "recycled": {"files": recycled_files, "bytes": recycled_bytes, "days": g.recycle_days},
        "waiting": {"count": waiting.len(), "bytes": waiting.iter().map(|j| j.total_bytes).sum::<u64>()},
        "titles": rows.into_iter().take(150).map(|r| r.1).collect::<Vec<_>>(),
    })))
}

/// Delete a title's files to make room and stop monitoring it, so Spool does not fetch it again.
/// The title and its saved releases stay.
async fn free_title(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let mut t = app.db.title(id)?.ok_or_else(|| not_found("title"))?;
    for a in app.db.title_acquisitions(id)?.into_iter().filter(|a| a.state.is_active()) {
        app.cancel_acquisition(a.id, false).await?;
    }
    let files = app.db.files(id)?;
    let bytes: u64 = files.iter().map(|f| f.size).sum();
    for f in &files {
        app.delete_file(f.id)?;
    }
    t.monitored = false;
    app.db.save_title(&mut t)?;
    app.emit(Event::Title { id });
    let recycled = app.settings.general().recycle_days > 0;
    Ok(Json(json!({"title": t.title, "files": files.len(), "bytes": bytes, "recycled": recycled})))
}

async fn recycle_info(State(app): State<App>) -> Json<Value> {
    let (files, bytes) = app.recycle_usage();
    Json(json!({"files": files, "bytes": bytes, "days": app.settings.general().recycle_days}))
}

async fn recycle_empty(State(app): State<App>) -> R<Value> {
    let app2 = app.clone();
    let freed = tokio::task::spawn_blocking(move || app2.clean_recycle(true)).await??;
    Ok(Json(json!({"freed": freed})))
}

async fn check_paths(State(app): State<App>) -> R<Value> {
    let (same, differ) = app.check_paths()?;
    Ok(Json(json!({"matching": same, "differing": differ.into_iter().map(|(a, e)| json!({"actual": a, "expected": e})).collect::<Vec<_>>()})))
}

async fn art(State(app): State<App>, Path((id, kind)): Path<(i64, String)>) -> Response {
    let Some(kind) = crate::art::Art::parse(&kind) else { return StatusCode::NOT_FOUND.into_response() };
    match app.art(id, kind).await {
        // The address carries a version tag, so the image behind it never changes.
        Ok(bytes) => ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], bytes).into_response(),
        Err(_) => ([(header::CACHE_CONTROL, "public, max-age=600")], StatusCode::NOT_FOUND).into_response(),
    }
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
        .route("/api/titles", get(list_titles).post(add_title))
        .route("/api/titles/bulk", post(bulk_titles))
        .route("/api/blocklist/clear", post(clear_blocklist))
        .route("/api/attention/dismiss-all", post(dismiss_all_attention))
        .route("/api/titles/{id}", get(get_title).patch(patch_title).delete(delete_title))
        .route("/api/titles/{id}/search", post(search_title))
        .route("/api/titles/{id}/refresh", post(refresh_title))
        .route("/api/titles/{id}/scan", post(scan_title))
        .route("/api/titles/{id}/nzb", post(upload_nzb))
        .route("/api/titles/{id}/seasons/{season}", patch(patch_season))
        .route("/api/episodes/{id}", patch(patch_episode))
        .route("/api/decisions/{id}/grab", post(grab_decision))
        .route("/api/files/{id}", delete(delete_file))
        .route("/api/lookup", get(lookup))
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
        .route("/api/events", get(events))
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
