//! Clips: asking for one, the list of them, and the clips themselves to watch, save or send.
//! Anyone signed in may make one and see them all; a clip is removed by whoever made it or
//! someone who runs Spool.

use super::*;
use crate::clips::{Clip, Wanted};

fn shown(app: &App, c: &Clip, who: Who, names: &HashMap<i64, String>) -> Value {
    let mut v = json!(c);
    v["progress"] = json!(app.clip_progress(c));
    v["by"] = json!(c.account_id.and_then(|a| names.get(&a)));
    v["yours"] = json!(who.manages || (c.account_id.is_some() && c.account_id == who.account));
    v
}

fn names(app: &App) -> HashMap<i64, String> {
    app.db.accounts().unwrap_or_default().into_iter().map(|a| (a.id, a.name)).collect()
}

pub(super) async fn list_clips(State(app): State<App>, who: Option<axum::Extension<Who>>) -> R<Value> {
    let who = Who::of(who);
    let names = names(&app);
    Ok(Json(json!(app.db.clips()?.iter().map(|c| shown(&app, c, who, &names)).collect::<Vec<_>>())))
}

pub(super) async fn get_clip(State(app): State<App>, who: Option<axum::Extension<Who>>, Path(id): Path<i64>) -> R<Value> {
    let c = app.db.clip(id)?.ok_or_else(|| not_found("clip"))?;
    Ok(Json(shown(&app, &c, Who::of(who), &names(&app))))
}

pub(super) async fn add_clip(State(app): State<App>, who: Option<axum::Extension<Who>>, Json(w): Json<Wanted>) -> R<Value> {
    let who = Who::of(who);
    let c = app.add_clip(&w, who.account).await?;
    Ok(Json(shown(&app, &c, who, &names(&app))))
}

pub(super) async fn delete_clip(State(app): State<App>, who: Option<axum::Extension<Who>>, Path(id): Path<i64>) -> R<Value> {
    let who = Who::of(who);
    let c = app.db.clip(id)?.ok_or_else(|| not_found("clip"))?;
    if !who.manages && (c.account_id.is_none() || c.account_id != who.account) {
        return Err(ApiError(StatusCode::FORBIDDEN, "only whoever made a clip, or someone who runs Spool, can remove it".into()));
    }
    app.delete_clip(id)?;
    Ok(Json(json!({"deleted": id})))
}

#[derive(Deserialize)]
pub(super) struct Look {
    q: String,
    limit: Option<usize>,
    title_id: Option<i64>,
}

/// Moments found by what is said in them, to choose clips from.
pub(super) async fn search_dialogue(State(app): State<App>, Query(look): Query<Look>) -> R<Value> {
    Ok(Json(json!(app.search_dialogue(&look.q, look.limit.unwrap_or(60), look.title_id).await?)))
}

/// How much of the library can be searched by what is said, and what is being done about the rest.
pub(super) async fn dialogue_coverage(State(app): State<App>) -> R<Value> {
    Ok(Json(json!(app.dialogue.coverage())))
}

/// A name a clip can be saved as: its own, without what a file name cannot hold.
fn file_name(c: &Clip) -> String {
    let clean: String = c.name.chars().map(|ch| if matches!(ch, '/' | '\\' | ':' | '"' | '\'' | '\n' | '\r') || ch.is_control() { ' ' } else { ch }).collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{}.mp4", if clean.is_empty() { format!("Clip {}", c.id) } else { clean })
}

/// The clip, to play; with `?download`, to save.
pub(super) async fn clip_video(State(app): State<App>, Path(id): Path<i64>, uri: Uri, req: axum::extract::Request) -> Response {
    match app.db.clip(id) {
        Ok(Some(c)) => send_clip(&app, c, uri, req).await,
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A clip by its link, to whoever was sent it: no sign-in, only the one clip.
pub(super) async fn shared_clip(State(app): State<App>, Path(file): Path<String>, uri: Uri, req: axum::extract::Request) -> Response {
    let link = file.strip_suffix(".mp4").unwrap_or(&file);
    let found = app.db.clips().unwrap_or_default().into_iter().find(|c| !c.link.is_empty() && crate::api::same(&c.link, link));
    match found {
        Some(c) => send_clip(&app, c, uri, req).await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn send_clip(app: &App, c: Clip, uri: Uri, req: axum::extract::Request) -> Response {
    let id = c.id;
    if c.state != "ready" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let head = req.method() == axum::http::Method::HEAD;
    let mut response = crate::serve::send_file(&app.clip_video(id), req.headers(), head).await;
    if uri.query().is_some_and(|q| q.split('&').any(|p| p == "download")) {
        // The name as plain letters for old browsers, and in full for the rest.
        let name = file_name(&c);
        let plain: String = name.chars().map(|ch| if ch.is_ascii() { ch } else { '_' }).collect();
        let full: String = name.bytes().map(|b| if b.is_ascii_alphanumeric() || b".-_".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
        if let Ok(v) = format!("attachment; filename=\"{plain}\"; filename*=UTF-8''{full}").parse() {
            response.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
    }
    response.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("private, max-age=86400"));
    response
}

pub(super) async fn clip_still(State(app): State<App>, Path(id): Path<i64>) -> Response {
    match tokio::fs::read(app.clip_still(id)).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "private, max-age=86400")], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
