//! Titles: listing them, what changed, one title and what is done to it.

use super::*;

pub(super) fn summarize(t: &Title, files: &HashMap<i64, (u32, u64)>, eps: &HashMap<i64, (u32, u32)>, profiles: &[QualityProfile], active: &[Acquisition], places: &crate::locations::Placement) -> Value {
    let mut v = serde_json::to_value(t).unwrap_or(Value::Null);
    v["sort_title"] = json!(crate::store::sort_title(&t.title));
    v["poster"] = json!(crate::art::local_url(t, crate::art::Art::Poster));
    v["fanart"] = json!(crate::art::local_url(t, crate::art::Art::Fanart));
    v["poster_small"] = json!(crate::art::small_url(t));
    let f = files.get(&t.id).copied().unwrap_or((0, 0));
    let e = eps.get(&t.id).copied().unwrap_or((0, 0));
    v["file_count"] = json!(f.0);
    v["size"] = json!(f.1);
    v["episodes_aired"] = json!(e.0);
    v["episodes_have"] = json!(e.1);
    v["profile"] = json!(profiles.iter().find(|p| p.id == t.profile_id).map(|p| p.name.clone()));
    v["active"] = json!(active.iter().find(|a| a.title_id == t.id).map(|a| a.state.as_str()));
    v["available"] = json!(t.kind == Kind::Series || t.movie_available(chrono::Local::now().date_naive()));
    // Where it is kept matters only once there is more than one place.
    if places.0.len() > 1 {
        v["location"] = json!(places.of(&t.path).map(|p| p.0.name.clone()));
        // And the other places that hold a copy of it.
        let mut also: Vec<String> = t.also.iter().filter_map(|f| places.of(f)).map(|p| p.0.name.clone()).collect();
        also.sort();
        also.dedup();
        v["also_on"] = json!(also);
    }
    // What the folder holds beside the film, in brief; the title's own page has it in full.
    v["trailers"] = json!(t.local.trailers().count());
    v["extras"] = json!(t.local.features().count());
    v["own_poster"] = json!(t.local.art(crate::local::ArtKind::Poster).is_some());
    v["offline"] = json!(!t.path.is_empty() && !places.online(t));
    v["read_only"] = json!(places.read_only(t));
    v
}

/// Titles as the grids and tables show them; long text stays on the title's own page.
/// `only` narrows the list to those ids.
pub(super) fn listed(app: &App, only: Option<&HashSet<i64>>) -> anyhow::Result<Vec<Value>> {
    let (files, eps, profiles, active) = (app.db.file_counts()?, app.db.episode_counts()?, app.db.profiles()?, app.db.active_acquisitions()?);
    let places = app.placement();
    let compact = app.db.compact_counts()?;
    let titles = match only {
        // A few are quicker fetched one by one than read with everything else.
        Some(ids) if ids.len() <= 20 => {
            let mut some = vec![];
            for id in ids {
                some.extend(app.db.title(*id)?);
            }
            some.sort_by_key(|t| t.id);
            some
        }
        _ => app.db.titles(None)?,
    };
    Ok(titles
        .iter()
        .filter(|t| only.is_none_or(|ids| ids.contains(&t.id)))
        .map(|t| {
            let mut v = summarize(t, &files, &eps, &profiles, &active, &places);
            if app.moving(t.id) {
                v["moving"] = json!(true);
            }
            if let Some((n, bytes)) = compact.get(&t.id) {
                v["compact_count"] = json!(n);
                v["compact_bytes"] = json!(bytes);
            }
            if let Some(o) = v.as_object_mut() {
                for k in ["overview", "alt_titles", "alt_years", "genres", "seasons", "fanart", "path", "studio", "network", "last_search_at", "last_refresh_at", "people", "tagline", "collection", "details_at", "local"] {
                    o.remove(k);
                }
            }
            v
        })
        .collect())
}

/// What a row in the title list is made from. Watched state is not part of it.
pub(super) const LISTED: &[&str] = &["title", "episode", "file", "activity"];

/// The title list. With `since`, the answer carries the library's change count, and when that
/// count is one this server handed out before, only the titles that changed after it and the
/// ids of those that went. `full` says the list is everything and replaces what the caller had.
pub(super) async fn list_titles(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R<Value> {
    let Some(since) = q.get("since").map(|s| s.parse::<i64>().unwrap_or(0)) else {
        return Ok(Json(json!(listed(&app, None)?)));
    };
    // Read the count first: a change that lands while the list is being put together is then
    // sent again next time, not missed.
    let revision = app.db.revision()?;
    if since <= 0 || since > revision || since < app.db.changes_floor()? {
        return Ok(Json(json!({"revision": revision, "full": true, "titles": listed(&app, None)?, "removed": []})));
    }
    let touched: HashSet<i64> = app.db.changes_since(since, LISTED, u32::MAX)?.iter().map(|c| c.title_id).collect();
    let titles = if touched.is_empty() { vec![] } else { listed(&app, Some(&touched))? };
    let still: HashSet<i64> = titles.iter().filter_map(|t| t["id"].as_i64()).collect();
    let mut removed: Vec<i64> = touched.difference(&still).copied().collect();
    removed.sort();
    Ok(Json(json!({"revision": revision, "full": false, "titles": titles, "removed": removed})))
}

/// What changed in the library after `since`, oldest first, for anything that keeps a copy.
pub(super) async fn changes(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R<Value> {
    let since = q.get("since").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let limit = q.get("limit").and_then(|s| s.parse::<u32>().ok()).unwrap_or(500).clamp(1, 5000);
    let kinds: Vec<&str> = q.get("kinds").map(|k| k.split(',').map(str::trim).filter(|k| !k.is_empty()).collect()).unwrap_or_default();
    let (revision, floor) = (app.db.revision()?, app.db.changes_floor()?);
    let list = app.db.changes_since(since, &kinds, limit)?;
    // Removals older than the floor have been forgotten, so a caller behind it cannot be told
    // what went and should read everything again.
    Ok(Json(json!({"revision": revision, "floor": floor, "complete": since >= floor && since <= revision, "more": list.len() as u32 == limit, "changes": list})))
}

pub(super) async fn get_title(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let t = app.db.title(id)?.ok_or_else(|| not_found("title"))?;
    let (files, eps, profiles, active) = (app.db.file_counts()?, app.db.episode_counts()?, app.db.profiles()?, app.db.active_acquisitions()?);
    let mut v = summarize(&t, &files, &eps, &profiles, &active, &app.placement());
    // Being moved to another location, or waiting its turn, with how far the move has got.
    if app.moving(id) {
        v["moving"] = json!(true);
        let moves = app.moves.lock();
        v["move"] = json!(moves.now.clone().filter(|m| m.title_id == id));
        v["move_waiting"] = json!(moves.queue.iter().any(|m| m.0 == id));
    }
    v["episodes"] = json!(app.db.episodes(id)?);
    // Each file with the place it is on, when it is a copy kept elsewhere than the title's own folder.
    let places = app.placement();
    v["files"] = json!(app
        .db
        .files(id)?
        .into_iter()
        .map(|f| {
            let place = f.base.as_deref().and_then(|b| places.of(b)).map(|p| json!({"name": p.0.name, "online": p.1}));
            let mut row = serde_json::to_value(&f).unwrap_or(Value::Null);
            row["copy_on"] = place.unwrap_or(Value::Null);
            if app.compacting(f.id) {
                row["compacting"] = json!(true);
            }
            // For a film, whether a compact version could be made of this file, and how big.
            if t.kind == Kind::Movie && f.compact_of.is_none() {
                row["compact"] = match app.compact_estimate(&t, &f) {
                    Ok(bytes) => json!({"can": true, "bytes": bytes}),
                    Err(why) => json!({"can": false, "why": why}),
                };
            }
            row
        })
        .collect::<Vec<_>>());
    v["decisions"] = json!(app.rejudge(&app.title_context(t.clone())?, app.db.decisions(id)?));
    v["history"] = json!(app.db.history(Some(id), 100)?);
    v["acquisitions"] = json!(app.db.title_acquisitions(id)?);
    v["saved"] = json!(app.archive_for(t.kind, t.tmdb_id, t.tvdb_id)?);
    if let Some(idx) = app.settings.general().plex_ready().then(|| app.plex_index().ok()).flatten() {
        if let Some(item) = idx.find(&t) {
            v["plex"] = json!({"rating_key": item.rating_key, "url": idx.url(item), "section": item.section});
        }
    }
    // A film can be handed to a player on the device the app is open on.
    if t.kind == Kind::Movie && v["offline"].as_bool() != Some(true) {
        if let Some(file) = app.compact_sources(&t)?.first() {
            v["play"] = json!({"port": app.settings.general().players_port, "path": app.play_path(file)});
        }
    }
    Ok(Json(v))
}

#[derive(Deserialize)]
pub(super) struct AddTitle {
    kind: Kind,
    tmdb_id: Option<u32>,
    tvmaze_id: Option<u32>,
    profile_id: i64,
    #[serde(default = "yes")]
    monitored: bool,
    #[serde(default)]
    search: bool,
    /// The location to keep it on; the one new titles go to when not given.
    #[serde(default)]
    location: Option<u32>,
}
pub(super) fn yes() -> bool {
    true
}

pub(super) async fn add_title(State(app): State<App>, Json(b): Json<AddTitle>) -> R<Title> {
    Ok(Json(app.add_title_on(b.kind, b.tmdb_id, b.tvmaze_id, b.profile_id, b.monitored, b.search, b.location).await?))
}

/// Move titles to another location: `{"ids": [...], "location": id}`. They are moved one at a
/// time in the background; the reply says how many were put in line.
pub(super) async fn move_titles(State(app): State<App>, Json(b): Json<Value>) -> R<Value> {
    let ids: Vec<i64> = b["ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default();
    let location = b["location"].as_u64().ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "which location?".into()))? as u32;
    let asked = app.move_titles(&ids, location).map_err(|e| ApiError(StatusCode::CONFLICT, format!("{e:#}")))?;
    for id in &ids {
        app.emit(Event::Title { id: *id });
    }
    Ok(Json(json!({"queued": asked.queued, "asked": ids.len(), "refused": asked.refused})))
}

/// Call moves off: the waiting titles named, or all that wait; and the move under way, when
/// its title is named.
pub(super) async fn cancel_moves(State(app): State<App>, body: Option<Json<Value>>) -> R<Value> {
    let ids: Vec<i64> = body.and_then(|b| b.0["ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect())).unwrap_or_default();
    let (cancelled, stopping) = app.cancel_moves(&ids);
    Ok(Json(json!({"cancelled": cancelled, "stopping": stopping})))
}

/// Turn a film that is posted in episodes into the series it is listed as.
pub(super) async fn title_to_series(State(app): State<App>, Path(id): Path<i64>, body: Option<Json<Value>>) -> R<Title> {
    let tvmaze = body.and_then(|b| b.0["tvmaze_id"].as_u64()).map(|n| n as u32);
    Ok(Json(app.film_to_series(id, tvmaze).await?))
}

pub(super) async fn patch_title(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<Value>) -> R<Title> {
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

pub(super) async fn delete_title(State(app): State<App>, Path(id): Path<i64>, Query(q): Query<HashMap<String, String>>) -> R<Value> {
    let t = app.db.title(id)?.ok_or_else(|| not_found("title"))?;
    for a in app.db.title_acquisitions(id)?.into_iter().filter(|a| a.state.is_active()) {
        app.cancel_acquisition(a.id, false).await?;
    }
    if q.get("delete_files").map(|v| v == "true").unwrap_or(false) {
        // Its own files. A copy kept on another location stays where it is.
        for f in app.db.files(id)?.into_iter().filter(|f| f.base.is_none()) {
            app.delete_file(f.id)?;
        }
    }
    app.db.delete_title(id)?;
    app.emit(Event::Title { id });
    Ok(Json(json!({"deleted": t.title})))
}

/// One action over many titles. Quick changes happen before the reply; searches and refreshes
/// carry on in the background, one title at a time, and report through the "bulk" task.
pub(super) async fn bulk_titles(State(app): State<App>, Json(b): Json<Value>) -> R<Value> {
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
                    for f in app.db.files(*id)?.into_iter().filter(|f| f.base.is_none()) {
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
        "search" | "smaller" | "refresh" => {
            if action != "refresh" && !app.is_active() {
                return Err(ApiError(StatusCode::CONFLICT, "Spool is in shadow mode and does not download".into()));
            }
            let n = ids.len();
            let what = match action.as_str() {
                "search" => "Searching",
                "smaller" => "Looking for smaller copies of",
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
                        "smaller" => app2.search_with(id, Scope::Smaller, true, true, false).await.map(|o| o.grabbed.len()),
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

pub(super) async fn patch_season(State(app): State<App>, Path((id, season)): Path<(i64, u32)>, Json(b): Json<Value>) -> R<Value> {
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

pub(super) async fn patch_episode(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<Value>) -> R<Value> {
    let e = app.db.episode(id)?.ok_or_else(|| not_found("episode"))?;
    app.db.set_episode_monitored(&[id], b["monitored"].as_bool().unwrap_or(true))?;
    app.emit(Event::Title { id: e.title_id });
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub(super) struct SearchBody {
    #[serde(default)]
    season: Option<u32>,
    #[serde(default)]
    episode: Option<i64>,
    #[serde(default)]
    grab: bool,
    /// Look for smaller copies of files over the profile's size target.
    #[serde(default)]
    smaller: bool,
}

pub(super) async fn search_title(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<SearchBody>) -> R<crate::acquire::SearchOutcome> {
    let t = app.db.title(id)?.ok_or_else(|| not_found("title"))?;
    let scope = match (t.kind, b.episode, b.season) {
        _ if b.smaller => Scope::Smaller,
        (Kind::Movie, _, _) => Scope::Movie,
        (_, Some(e), _) => Scope::Episode(e),
        (_, _, Some(s)) => Scope::Season(s),
        _ => Scope::Missing,
    };
    Ok(Json(app.search(id, scope, true, b.grab).await?))
}

pub(super) async fn grab_decision(State(app): State<App>, Path(id): Path<i64>, body: Option<Json<Value>>) -> R<Value> {
    let d = app.db.decision(id)?.ok_or_else(|| not_found("release"))?;
    let t = app.db.title(d.title_id)?.ok_or_else(|| not_found("title"))?;
    if !app.is_active() {
        return Err(ApiError(StatusCode::CONFLICT, "Spool is in shadow mode and does not download".into()));
    }
    // A film can keep a second version beside the one it has; a series cannot.
    let another = t.kind == Kind::Movie && body.is_some_and(|b| b.0["another_version"].as_bool().unwrap_or(false));
    let a = app.grab(&d, &t, if another { crate::acquire::ANOTHER_VERSION_REASON } else { crate::acquire::BY_HAND_REASON }, true).await?;
    Ok(Json(json!({"acquisition": a})))
}
