//! Music for the web app: the albums, one album's tracks, its picture, a track to play, and
//! reading the music folders again. Players have their own door; see [`crate::music::subsonic`].

use super::*;
use crate::music::subsonic::{self, Transcode};

pub(super) async fn music(State(app): State<App>) -> R<Value> {
    let lib = app.music.library();
    let mut albums: Vec<_> = lib.albums.iter().collect();
    albums.sort_by_key(|a| (a.artist.to_lowercase(), a.year, a.title.to_lowercase()));
    Ok(Json(json!({"status": app.music_status(), "albums": albums.into_iter().map(|a| subsonic::web_album(&lib, a)).collect::<Vec<_>>()})))
}

pub(super) async fn music_album(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let lib = app.music.library();
    let a = lib.album(id).ok_or_else(|| not_found("album"))?;
    Ok(Json(json!({"album": subsonic::web_album(&lib, a), "tracks": subsonic::web_tracks(&lib, id), "folder": a.folder})))
}

#[derive(Deserialize)]
pub(super) struct ArtSize {
    #[serde(default)]
    size: u32,
}

pub(super) async fn music_art(State(app): State<App>, Path(id): Path<i64>, Query(s): Query<ArtSize>) -> Response {
    subsonic::cover(&app, id, s.size.min(2000)).await
}

#[derive(Deserialize)]
pub(super) struct PlayAsk {
    format: Option<String>,
    #[serde(default)]
    kbps: i64,
}

pub(super) async fn music_stream(State(app): State<App>, Path(id): Path<i64>, Query(p): Query<PlayAsk>, req: axum::extract::Request) -> Response {
    let Some(t) = app.music.library().track(id).cloned() else { return StatusCode::NOT_FOUND.into_response() };
    let head = req.method() == axum::http::Method::HEAD;
    subsonic::stream(&app, &t, Transcode::asked(p.format.as_deref(), p.kbps, 0), req.headers(), head).await
}

pub(super) async fn music_scan(State(app): State<App>) -> R<Value> {
    if app.music.scan.lock().scanning {
        return Ok(Json(json!({"started": false, "message": "already reading the music folders"})));
    }
    let a = app.clone();
    tokio::spawn(async move {
        a.run_task("music").await;
    });
    Ok(Json(json!({"started": true})))
}
