//! The archive, Plex, disk space and the recycle folder.

use super::*;

/// The archive, grouped by film or series, whether or not each is still in the library.
pub(super) async fn archive_list(State(app): State<App>) -> R<Value> {
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
                Some(t) => crate::art::small_url(t),
                None => n.poster.as_deref().map(|src| crate::art::saved_small_url(n.id, src)),
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

pub(super) async fn archive_art(State(app): State<App>, Path(id): Path<i64>, Query(q): Query<HashMap<String, String>>) -> Response {
    let image = if q.get("size").is_some_and(|s| s == "small") { app.archive_art_small(id).await } else { app.archive_art(id).await };
    picture(image)
}

pub(super) async fn archive_restore(State(app): State<App>, Path(id): Path<i64>) -> R<Title> {
    Ok(Json(app.archive_restore(id).await?))
}

pub(super) async fn archive_delete(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.archive_delete(id)?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn archive_check(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let n = app.archive_check(id).await?;
    Ok(Json(json!({"available": n.available, "usable": n.usable(), "checked_at": n.checked_at})))
}

pub(super) async fn archive_grab(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    if !app.is_active() {
        return Err(ApiError(StatusCode::CONFLICT, "Spool is in shadow mode and does not download".into()));
    }
    let a = app.grab_saved(id).await?;
    Ok(Json(json!({"acquisition": a})))
}

pub(super) async fn plex_report(State(app): State<App>) -> R<crate::plex::PlexReport> {
    Ok(Json(app.plex_report()?))
}

pub(super) async fn plex_sync(State(app): State<App>) -> R<Value> {
    let message = app.run_task("plex").await;
    Ok(Json(json!({"message": message})))
}

#[derive(Deserialize)]
pub(super) struct PlexTrack {
    rating_key: String,
}

pub(super) async fn plex_track(State(app): State<App>, Json(b): Json<PlexTrack>) -> R<Title> {
    Ok(Json(app.plex_track(&b.rating_key).await?))
}

/// Where the disk went and what could be removed: the largest titles, whether each has been
/// watched in Plex, and whether it could be downloaded again from a saved release.
pub(super) async fn space(State(app): State<App>) -> R<Value> {
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
                "poster": crate::art::small_url(t),
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
        "locations": app.location_report(),
        "recycled": {"files": recycled_files, "bytes": recycled_bytes, "days": g.recycle_days},
        "waiting": {"count": waiting.len(), "bytes": waiting.iter().map(|j| j.total_bytes).sum::<u64>()},
        "titles": rows.into_iter().take(150).map(|r| r.1).collect::<Vec<_>>(),
    })))
}

/// Delete a title's files to make room and stop monitoring it, so Spool does not fetch it again.
/// The title and its saved releases stay.
pub(super) async fn free_title(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let mut t = app.db.title(id)?.ok_or_else(|| not_found("title"))?;
    for a in app.db.title_acquisitions(id)?.into_iter().filter(|a| a.state.is_active()) {
        app.cancel_acquisition(a.id, false).await?;
    }
    let files: Vec<MediaFile> = app.db.files(id)?.into_iter().filter(|f| f.base.is_none()).collect();
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

pub(super) async fn recycle_info(State(app): State<App>) -> Json<Value> {
    let (files, bytes) = app.recycle_usage();
    Json(json!({"files": files, "bytes": bytes, "days": app.settings.general().recycle_days}))
}

pub(super) async fn recycle_empty(State(app): State<App>) -> R<Value> {
    let app2 = app.clone();
    let freed = tokio::task::spawn_blocking(move || app2.clean_recycle(true)).await??;
    Ok(Json(json!({"freed": freed})))
}

pub(super) async fn check_paths(State(app): State<App>) -> R<Value> {
    let (same, differ) = app.check_paths()?;
    Ok(Json(json!({"matching": same, "differing": differ.into_iter().map(|(a, e)| json!({"actual": a, "expected": e})).collect::<Vec<_>>()})))
}
