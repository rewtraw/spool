//! A file's subtitles.

use super::*;

/// What a file has: the languages inside it and the subtitle files beside it.
pub(super) async fn file_subtitles(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let app2 = app.clone();
    let beside = tokio::task::spawn_blocking(move || app2.find_sidecars(id)).await??;
    let f = app.db.file(id)?.ok_or_else(|| not_found("file"))?;
    let g = app.settings.general();
    Ok(Json(json!({
        "embedded": crate::subs::embedded_languages(&f),
        "beside": beside,
        "wanted": crate::subs::wanted_languages(&g.subtitle_languages),
        "can_search": !g.opensubtitles_api_key.is_empty(),
        "can_check": app.ffmpeg_path().is_some(),
    })))
}

pub(super) async fn search_file_subtitles(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<Value>) -> R<Value> {
    let language = b["language"].as_str().and_then(crate::subs::language_code).ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "which language?".into()))?;
    Ok(Json(json!(app.search_subtitles(id, language).await?)))
}

pub(super) async fn add_file_subtitle(State(app): State<App>, Path(id): Path<i64>, Json(c): Json<crate::subs::Candidate>) -> R<Value> {
    let (subtitle, report) = app.add_subtitle(id, &c, None).await?;
    Ok(Json(json!({"subtitle": subtitle, "report": report})))
}

/// Find and keep the first subtitles that prove to be in time, without choosing by hand.
pub(super) async fn auto_file_subtitle(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<Value>) -> R<Value> {
    let language = b["language"].as_str().and_then(crate::subs::language_code).ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "which language?".into()))?;
    Ok(Json(json!({"subtitle": app.auto_subtitle(id, language, 0).await?})))
}

pub(super) async fn check_file_subtitle(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<Value>) -> R<Value> {
    let rel = b["rel_path"].as_str().unwrap_or_default();
    Ok(Json(json!(app.check_sidecar(id, rel, b["correct"].as_bool().unwrap_or(false)).await?)))
}

pub(super) async fn delete_file_subtitle(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<Value>) -> R<Value> {
    app.delete_sidecar(id, b["rel_path"].as_str().unwrap_or_default())?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn patch_file(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<Value>) -> R<Value> {
    let mut f = app.db.file(id)?.ok_or_else(|| not_found("file"))?;
    if let Some(extra) = b["extra"].as_bool() {
        f.extra = extra;
    }
    app.db.with(|c| c.execute("UPDATE files SET data = ?2 WHERE id = ?1", rusqlite::params![id, serde_json::to_string(&f).unwrap_or_default()]))?;
    app.emit(Event::Title { id: f.title_id });
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn refresh_title(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.refresh_title(id).await?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn scan_title(State(app): State<App>, Path(id): Path<i64>) -> R<crate::catalog::ScanReport> {
    let mut r = crate::catalog::ScanReport::default();
    app.scan_title(id, &mut r).await?;
    app.emit(Event::Title { id });
    Ok(Json(r))
}

pub(super) async fn delete_file(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.delete_file(id)?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn upload_nzb(State(app): State<App>, Path(id): Path<i64>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> R<Acquisition> {
    let t = app.db.title(id)?.ok_or_else(|| not_found("title"))?;
    let name = q.get("name").cloned().unwrap_or_else(|| format!("{}.nzb", t.title));
    let episodes: Vec<i64> = q.get("episodes").map(|e| e.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
    Ok(Json(app.add_manual_nzb(&t, episodes, &name, &body)?))
}

/// Films and series by name, together.
pub(super) async fn lookup_all(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R<Value> {
    let query = q.get("q").cloned().unwrap_or_default();
    if query.trim().is_empty() {
        return Ok(Json(json!({"movies": [], "series": []})));
    }
    Ok(Json(app.discover(&query).await?))
}

pub(super) async fn lookup(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R<Vec<Value>> {
    let query = q.get("q").cloned().unwrap_or_default();
    if query.trim().is_empty() {
        return Ok(Json(vec![]));
    }
    let kind = q.get("kind").and_then(|k| Kind::parse(k)).unwrap_or(Kind::Movie);
    let found = match kind {
        Kind::Movie => app.meta.search_movies(&app.settings.general().tmdb_api_key, &query).await?,
        Kind::Series => app.meta.search_series(&app.settings.general().tmdb_api_key, &query).await?,
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
