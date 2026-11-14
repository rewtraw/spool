//! Videos from sites by their links, the channels they are filed under, and Letterboxd lists
//! followed. See [`crate::videos`] and [`crate::lists`].

use super::*;
use crate::videos::{Take, Video};

fn video_row(v: &Video, progress: Option<&crate::videos::ytdlp::Progress>) -> Value {
    let mut row = serde_json::to_value(v).unwrap_or_default();
    row["id"] = json!(v.id);
    row["site_name"] = json!(crate::videos::site_name(&v.site));
    row["progress"] = progress.map(|p| json!(p)).unwrap_or(Value::Null);
    row
}

/// What has been asked for, newest first, with how far each fetch has got; the channels
/// videos are filed under; and whether videos can be fetched at all here.
pub(super) async fn videos(State(app): State<App>) -> R<Value> {
    let progress = app.video_progress();
    let rows: Vec<Value> = app.videos_all()?.iter().map(|v| video_row(v, progress.get(&v.id))).collect();
    let counts = app.db.episode_counts()?;
    let channels: Vec<Value> = app
        .db
        .titles(Some(Kind::Series))?
        .into_iter()
        .filter_map(|t| {
            let c = t.channel.clone()?;
            let have = counts.get(&t.id).map(|c| c.0).unwrap_or(0);
            Some(json!({
                "title_id": t.id, "title": t.title, "site": c.site, "site_name": crate::videos::site_name(&c.site), "url": c.url,
                "follow": c.follow, "checked_at": c.checked_at, "videos": have, "poster": crate::art::small_url(&t),
            }))
        })
        .collect();
    let g = app.settings.general();
    Ok(Json(json!({
        "videos": rows,
        "channels": channels,
        "ready": {"yt_dlp": crate::videos::ytdlp::find().is_some(), "folder": app.video_root(), "cookies": g.video_cookies},
    })))
}

#[derive(Deserialize)]
pub(super) struct AddVideo {
    url: String,
    #[serde(default = "newest")]
    take: Take,
    #[serde(default)]
    follow: bool,
}

fn newest() -> Take {
    Take::Newest
}

/// Ask for a link: one video, or a channel, account or playlist.
pub(super) async fn add_video(State(app): State<App>, Json(b): Json<AddVideo>) -> R<Value> {
    let added = app.videos_add(&b.url, b.take, b.follow).await.map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok(Json(json!(added)))
}

pub(super) async fn retry_video(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.video_retry(id)?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn remove_video(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.video_remove(id)?;
    Ok(Json(json!({"ok": true})))
}

/// Forget what was filed or failed.
pub(super) async fn clear_videos(State(app): State<App>) -> R<Value> {
    Ok(Json(json!({"cleared": app.videos_clear()?})))
}

/// Follow a channel or stop: `{"follow": true}`.
pub(super) async fn follow_channel(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<Value>) -> R<Value> {
    let follow = b["follow"].as_bool().ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "follow, true or false?".into()))?;
    app.channel_follow(id, follow).map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    if follow {
        let app = app.clone();
        tokio::spawn(async move { app.run_task("videos").await });
    }
    Ok(Json(json!({"ok": true})))
}

// ---------------------------------------------------------------- lists

pub(super) async fn lists(State(app): State<App>) -> R<Value> {
    Ok(Json(json!({"lists": app.lists()?.iter().map(list_row).collect::<Vec<_>>()})))
}

fn list_row(l: &crate::lists::List) -> Value {
    json!({
        "id": l.id, "url": l.url, "name": l.name, "profile_id": l.profile_id, "films": l.films, "added": l.added.len(),
        "missed": l.missed.len(), "checked_at": l.checked_at, "error": l.error, "added_at": l.added_at,
    })
}

#[derive(Deserialize)]
pub(super) struct AddList {
    url: String,
    #[serde(default)]
    profile_id: i64,
}

/// Follow a Letterboxd list. Its films are added in the background.
pub(super) async fn add_list(State(app): State<App>, Json(b): Json<AddList>) -> R<Value> {
    let l = app.list_add(&b.url, b.profile_id).await.map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok(Json(list_row(&l)))
}

pub(super) async fn change_list(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<Value>) -> R<Value> {
    let profile = b["profile_id"].as_i64().unwrap_or(0);
    Ok(Json(list_row(&app.list_change(id, profile)?)))
}

pub(super) async fn remove_list(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.list_remove(id)?;
    Ok(Json(json!({"ok": true})))
}

/// Read a list again now, for what was put on it since.
pub(super) async fn read_list(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.list(id)?.ok_or_else(|| not_found("list"))?;
    let app = app.clone();
    tokio::spawn(async move {
        if let Err(e) = app.list_read(id).await {
            tracing::warn!(list = id, error = %format!("{e:#}"), "a Letterboxd list could not be read");
        }
    });
    Ok(Json(json!({"ok": true})))
}
