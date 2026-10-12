//! Downloads, the queue, history and what needs attention.

use super::*;

pub(super) async fn activity(State(app): State<App>) -> R<Value> {
    let jobs = app.jobs();
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
    // A download that runs on a helper is marked with whose it is.
    let helpers: Vec<(String, String)> = app.remotes().iter().map(|r| (format!("@{}:", r.conf.id), r.conf.name.clone())).collect();
    let on = |job: Option<&String>| job.and_then(|id| helpers.iter().find(|h| id.starts_with(&h.0))).map(|h| h.1.clone());
    let rows: Vec<Value> = recent
        .iter()
        .map(|a| {
            let t = titles.get(&a.title_id);
            json!({
                "acquisition": a,
                "title": t.map(|t| t.title.clone()),
                "kind": t.map(|t| t.kind),
                "poster": t.and_then(crate::art::small_url),
                "job": a.job_id.as_ref().and_then(|id| jobs.iter().find(|j| j.id == *id)),
                "on": on(a.job_id.as_ref()),
            })
        })
        .collect();
    // Work Spool finished that was not a download: compact versions made and titles moved.
    let done: Vec<Value> = app
        .db
        .history(None, 400)?
        .into_iter()
        .filter(|h| matches!(h.kind.as_str(), "compact_made" | "compact_failed" | "moved"))
        .take(25)
        .map(|h| json!({"id": h.id, "ts": h.ts, "kind": h.kind, "title_id": h.title_id, "title": h.title_id.and_then(|id| titles.get(&id)).map(|t| t.title.clone()), "data": h.data}))
        .collect();
    let known: Vec<&str> = recent.iter().filter_map(|a| a.job_id.as_deref()).collect();
    let orphans: Vec<&spool_nntp::JobStatus> = jobs.iter().filter(|j| !known.contains(&j.id.as_str())).collect();
    Ok(Json(json!({"items": rows, "orphan_jobs": orphans, "paused": app.engine.is_paused(), "speed_limit": app.settings.speed_limit(), "finished": {"count": finished, "bytes": finished_bytes}, "moves": app.moves_shown(), "compact": app.compacts_shown(), "done": done})))
}

pub(super) async fn activity_action(State(app): State<App>, Path((id, action)): Path<(i64, String)>, body: Option<Json<Value>>) -> R<Value> {
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
                app.job_pause(j)?;
            }
        }
        "resume" => {
            if let Some(j) = &acq.job_id {
                // The user's word overrides the free-space hold for this job.
                app.waiting_for_space.lock().insert(j.clone());
                app.job_resume(j)?;
            }
        }
        "retry_import" => {
            let mut a = acq.clone();
            if matches!(a.state, AcqState::ImportBlocked | AcqState::Importing) {
                a.state = AcqState::Importing;
                app.db.save_acquisition(&mut a)?;
            }
            let episodes: Option<Vec<i64>> = body.and_then(|b| b.0["episode_ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()));
            // This is "Import anyway": the person has read why it was stopped, and is not asked
            // again. For a series the episodes the release was taken for stand.
            let force = episodes.clone().or_else(|| Some(if app.db.title(acq.title_id).ok().flatten().is_some_and(|t| t.kind == Kind::Movie) { vec![] } else { acq.episode_ids.clone() }));
            app.import(id, if episodes.is_some() || force.is_some() { force } else { None }).await?;
        }
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "unknown action".into())),
    }
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn queue_action(State(app): State<App>, Path(action): Path<String>) -> R<Value> {
    match action.as_str() {
        "pause" => app.queue_pause(true),
        "resume" => app.queue_pause(false),
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "unknown action".into())),
    }
    Ok(Json(json!({"paused": app.engine.is_paused()})))
}

pub(super) async fn job_remove(State(app): State<App>, Path(id): Path<String>) -> R<Value> {
    app.job_remove(&id, true)?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn history(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R<Value> {
    let titles: HashMap<i64, String> = app.db.titles(None)?.into_iter().map(|t| (t.id, t.title)).collect();
    let rows = app.db.history(q.get("title_id").and_then(|t| t.parse().ok()), q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(200))?;
    Ok(Json(json!(rows.into_iter().map(|h| json!({"id": h.id, "ts": h.ts, "kind": h.kind, "title_id": h.title_id, "title": h.title_id.and_then(|i| titles.get(&i).cloned()), "data": h.data})).collect::<Vec<_>>())))
}

pub(super) async fn attention(State(app): State<App>) -> R<Value> {
    let titles: HashMap<i64, Title> = app.db.titles(None)?.into_iter().map(|t| (t.id, t)).collect();
    let rows = app.db.attention(false)?;
    Ok(Json(json!(rows.into_iter().map(|a| {
        let t = a.title_id.and_then(|i| titles.get(&i));
        json!({"item": a, "title": t.map(|t| t.title.clone()), "kind": t.map(|t| t.kind), "poster": t.and_then(crate::art::small_url)})
    }).collect::<Vec<_>>())))
}

pub(super) async fn dismiss_attention(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.db.resolve_attention(id)?;
    app.emit(Event::Attention);
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn dismiss_all_attention(State(app): State<App>) -> R<Value> {
    let rows = app.db.attention(false)?;
    for a in &rows {
        app.db.resolve_attention(a.id)?;
    }
    app.emit(Event::Attention);
    Ok(Json(json!({"dismissed": rows.len()})))
}

pub(super) async fn clear_blocklist(State(app): State<App>) -> R<Value> {
    let rows = app.db.blocklist()?;
    for id in rows.iter().filter_map(|b| b["id"].as_i64()) {
        app.db.delete_blocklist(id)?;
    }
    Ok(Json(json!({"cleared": rows.len()})))
}

pub(super) async fn blocklist(State(app): State<App>) -> R<Vec<Value>> {
    Ok(Json(app.db.blocklist()?))
}

pub(super) async fn delete_blocklist(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.db.delete_blocklist(id)?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn calendar(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R<Value> {
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
            Some(json!({"date": e.air_date, "kind": "episode", "title_id": t.id, "title": t.title, "poster": crate::art::small_url(t), "label": format!("S{:02}E{:02} · {}", e.season, e.episode, e.title),
                "have": e.file_id.is_some(), "monitored": e.monitored && t.monitored}))
        })
        .collect();
    for t in titles.values().filter(|t| t.kind == Kind::Movie) {
        for (d, label) in [(&t.in_cinemas, "In cinemas"), (&t.digital_release, "Digital release"), (&t.physical_release, "Physical release")] {
            if let Some(d) = d.as_deref().and_then(|d| d.get(..10)) {
                if d >= from.as_str() && d <= to.as_str() {
                    items.push(json!({"date": d, "kind": "movie", "title_id": t.id, "title": t.title, "poster": crate::art::small_url(t), "label": label, "have": false, "monitored": t.monitored}));
                }
            }
        }
    }
    items.sort_by(|a, b| a["date"].as_str().cmp(&b["date"].as_str()));
    Ok(Json(json!(items)))
}
