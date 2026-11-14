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

#[derive(Deserialize)]
pub(super) struct Find {
    #[serde(default)]
    q: String,
}

/// Albums in MusicBrainz matching what was typed, to want.
pub(super) async fn music_find(State(app): State<App>, Query(f): Query<Find>) -> R<Value> {
    if f.q.trim().len() < 2 {
        return Ok(Json(json!([])));
    }
    Ok(Json(json!(app.album_lookup(f.q.trim()).await?)))
}

/// The artist named by what was typed, and their own albums.
pub(super) async fn music_find_artist(State(app): State<App>, Query(f): Query<Find>) -> R<Value> {
    if f.q.trim().len() < 2 {
        return Ok(Json(json!({"artist": null, "albums": []})));
    }
    Ok(Json(app.artist_lookup(f.q.trim()).await?))
}

fn want_row(w: &crate::music::want::Want) -> Value {
    json!({"want": w, "cover": w.cover()})
}

pub(super) async fn music_wants(State(app): State<App>) -> R<Value> {
    Ok(Json(json!({"wants": app.db.music_wants()?.iter().map(want_row).collect::<Vec<_>>(), "counts": app.music_wants_status()})))
}

#[derive(Deserialize)]
pub(super) struct WantAlbum {
    mbid: String,
}

pub(super) async fn want_album(State(app): State<App>, who: Option<axum::Extension<Who>>, Json(a): Json<WantAlbum>) -> R<Value> {
    let mbid = a.mbid.trim();
    if mbid.len() != 36 || !mbid.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(ApiError(StatusCode::BAD_REQUEST, "a MusicBrainz release group is named by its 36-character id".into()));
    }
    Ok(Json(want_row(&app.want_album(mbid, Who::of(who).account).await?)))
}

pub(super) async fn search_want(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    Ok(Json(want_row(&app.search_want(id).await?)))
}

pub(super) async fn remove_want(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.remove_want(id).await?;
    Ok(Json(json!({"removed": id})))
}
