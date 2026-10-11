//! The parts of the players' API that touch files and viewing: sending video, images and
//! subtitles, and recording what was watched.

use super::ids::Id;
use super::items::{Library, Node};
use crate::app::App;
use crate::models::*;
use anyhow::Result;
use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// What part of a file a request wants: first and last byte, and whether it asked for a part.
/// An error means the range cannot be met.
fn byte_range(header: Option<&str>, len: u64) -> Result<(u64, u64, bool), ()> {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else { return Ok((0, len.saturating_sub(1), false)) };
    // Only the first of several ranges is served.
    let (from, to) = spec.split(',').next().unwrap_or("").trim().split_once('-').ok_or(())?;
    let (start, end) = match (from.trim(), to.trim()) {
        ("", "") => return Err(()),
        ("", last) => {
            let n: u64 = last.parse().map_err(|_| ())?;
            (len.saturating_sub(n), len.saturating_sub(1))
        }
        (first, "") => (first.parse().map_err(|_| ())?, len.saturating_sub(1)),
        (first, last) => (first.parse().map_err(|_| ())?, last.parse::<u64>().map_err(|_| ())?.min(len.saturating_sub(1))),
    };
    if len == 0 || start >= len || start > end {
        return Err(());
    }
    Ok((start, end, true))
}

/// Send a file, or the part of it asked for, without holding more than a piece in memory.
async fn send_file(path: &Path, headers: &HeaderMap, head: bool) -> Response {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "a player asked for a file that could not be opened");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let Ok((start, end, partial)) = byte_range(range, len) else {
        return (StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, format!("bytes */{len}"))]).into_response();
    };
    let count = if len == 0 { 0 } else { end - start + 1 };
    let mime = mime_guess::from_path(path).first().filter(|m| m.type_() == "video").map(|m| m.to_string()).unwrap_or_else(|| "video/x-matroska".into());
    let mut response = Response::builder()
        .status(if partial { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, count);
    if partial {
        response = response.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    if head || count == 0 {
        return response.body(Body::empty()).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    if start > 0 && file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    const PIECE: u64 = 512 * 1024;
    let pieces = futures::stream::unfold((file, count), |(mut file, left)| async move {
        if left == 0 {
            return None;
        }
        let mut buf = vec![0u8; left.min(PIECE) as usize];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<Bytes, std::io::Error>(Bytes::from(buf)), (file, left - n as u64)))
            }
            Err(e) => Some((Err(e), (file, 0))),
        }
    });
    response.body(Body::from_stream(pieces)).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// The video itself: a film, an episode, or one named version of a film.
pub async fn stream(app: &App, id: &str, source: Option<&str>, headers: &HeaderMap, head: bool) -> Response {
    let Some(id) = Id::parse(id) else { return StatusCode::NOT_FOUND.into_response() };
    let path: Option<PathBuf> = Library::shared(app).ok().and_then(|lib| lib.extra_path(id).or_else(|| lib.file_for(id, source.and_then(Id::parse)).map(|(t, f)| Path::new(&t.path).join(&f.rel_path))));
    match path {
        Some(p) => send_file(&p, headers, head).await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A poster, a backdrop, an episode's frame or someone's photograph.
pub async fn image(app: &App, id: &str, kind: &str, versioned: bool, head: bool) -> Response {
    let id = Id::parse(id);
    // Movies and Shows have no picture of their own; one is made from what is in them.
    let view = match id {
        Some(Id::Movies) => Some(Kind::Movie),
        Some(Id::Shows) => Some(Kind::Series),
        _ => None,
    };
    let made = match view.filter(|_| kind.eq_ignore_ascii_case("primary")) {
        Some(of) => {
            let Some((titles, tag)) = Library::shared(app).ok().map(|lib| lib.view_posters(of)).filter(|p| !p.0.is_empty()) else { return StatusCode::NOT_FOUND.into_response() };
            app.art_view(of.as_str(), &titles, &tag).await
        }
        None => {
            let found = id.and_then(|id| Library::shared(app).ok().and_then(|lib| lib.image(id, kind)));
            let Some((src, name, width)) = found else { return StatusCode::NOT_FOUND.into_response() };
            app.art_any(&name, &src, width).await
        }
    };
    match made {
        Ok(bytes) => {
            // An address that names a version never shows a different picture.
            let cache = if versioned { "public, max-age=31536000, immutable" } else { "public, max-age=3600" };
            let headers = [(header::CONTENT_TYPE, "image/jpeg".to_string()), (header::CACHE_CONTROL, cache.to_string()), (header::CONTENT_LENGTH, bytes.len().to_string())];
            if head {
                (headers, Body::empty()).into_response()
            } else {
                (headers, bytes).into_response()
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, "no image for a player");
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

/// SubRip text as WebVTT, for players that ask for it that way.
fn to_vtt(srt: &str) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for line in srt.trim_start_matches('\u{feff}').lines() {
        if line.contains(" --> ") {
            out.push_str(&line.replace(',', "."));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// A subtitle file kept beside the video. Tracks inside the video are the player's to read.
pub async fn subtitle(app: &App, id: &str, source: Option<&str>, index: &str, file: &str) -> Response {
    let missing = || StatusCode::NOT_FOUND.into_response();
    let (Some(id), Ok(index)) = (Id::parse(id), index.parse::<usize>()) else { return missing() };
    let path = Library::shared(app).ok().and_then(|lib| {
        let (t, f) = lib.file_for(id, source.and_then(Id::parse))?;
        let inside = super::items::media_streams(&f.media_info.clone().unwrap_or_default()).len();
        let sub = f.subtitles.get(index.checked_sub(inside)?)?;
        Some(Path::new(&t.path).join(&sub.rel_path))
    });
    let Some(path) = path else { return missing() };
    let Ok(bytes) = tokio::fs::read(&path).await else { return missing() };
    let text = String::from_utf8_lossy(&bytes).to_string();
    if file.ends_with(".vtt") {
        ([(header::CONTENT_TYPE, "text/vtt; charset=utf-8")], to_vtt(&text)).into_response()
    } else {
        ([(header::CONTENT_TYPE, "application/x-subrip; charset=utf-8")], text).into_response()
    }
}

/// What a report about an item is recorded against: the film or the episode, and how long it runs.
fn subject(lib: &Library, id: Id) -> Option<(&'static str, i64, f64)> {
    let node = lib.node(id)?;
    let runtime = lib.json(&node, &super::items::Fields::from(&super::Params::parse(None)))["RunTimeTicks"].as_f64().unwrap_or(0.0) / super::items::TICKS;
    match node {
        Node::Movie(t) => Some(("movie", t.id, runtime)),
        Node::Episode(_, e) => Some(("episode", e.id, runtime)),
        _ => None,
    }
}

/// Record how far a player has got. Stopping near the end counts as having watched it; stopping
/// almost at once leaves nothing to resume.
pub fn report(app: &App, lib: &Library, id: Id, position: f64, stopped: bool) -> Result<()> {
    let Some((kind, ref_id, runtime)) = subject(lib, id) else { return Ok(()) };
    let mut w = app.db.watched(kind, ref_id)?;
    let now = crate::db::now();
    w.last_played_at = now;
    let finished = runtime > 0.0 && position >= runtime * 0.9;
    if stopped && finished {
        if !w.played {
            w.play_count += 1;
        }
        w.played = true;
        w.position = 0.0;
    } else if stopped && (runtime > 0.0 && position < runtime * 0.02 || position < 30.0) {
        w.position = 0.0;
    } else {
        w.position = position.max(0.0);
    }
    app.db.save_watched(kind, ref_id, &w)
}

/// Mark something watched or not. A series or a season carries its episodes with it.
pub fn mark(app: &App, lib: &Library, id: Id, played: bool) -> Result<()> {
    let targets: Vec<(&str, i64)> = match lib.node(id) {
        Some(Node::Movie(t)) => vec![("movie", t.id)],
        Some(Node::Episode(_, e)) => vec![("episode", e.id)],
        Some(Node::Series(t)) => lib.episodes_of(t.id).iter().map(|e| ("episode", e.id)).collect(),
        Some(Node::Season(t, s)) => lib.episodes_of(t.id).iter().filter(|e| e.season == s).map(|e| ("episode", e.id)).collect(),
        _ => vec![],
    };
    let now = crate::db::now();
    for (kind, ref_id) in targets {
        let mut w = app.db.watched(kind, ref_id)?;
        w.played = played;
        w.position = 0.0;
        if played {
            w.play_count = w.play_count.max(1);
            w.last_played_at = now;
        } else {
            w.play_count = 0;
        }
        app.db.save_watched(kind, ref_id, &w)?;
    }
    Ok(())
}

pub fn favorite(app: &App, lib: &Library, id: Id, on: bool) -> Result<()> {
    let (kind, ref_id) = match lib.node(id) {
        Some(Node::Movie(t)) => ("movie", t.id),
        Some(Node::Series(t)) => ("series", t.id),
        Some(Node::Episode(_, e)) => ("episode", e.id),
        _ => return Ok(()),
    };
    let mut w = app.db.watched(kind, ref_id)?;
    w.favorite = on;
    app.db.save_watched(kind, ref_id, &w)
}

/// Read a file's streams if they were never recorded, and keep them. Leaves alone the summary
/// that names are built from. Returns whether anything was learned.
async fn learn_streams(app: &App, title: &Title, file: &MediaFile) -> bool {
    if file.media_info.as_ref().is_some_and(|m| !m.streams.is_empty()) {
        return false;
    }
    let path = title.file_path(&file);
    let name = file.scene_name.clone().unwrap_or_else(|| file.rel_path.clone());
    let Ok(Some(probed)) = app.probe(&path, &name).await else { return false };
    if probed.streams.is_empty() {
        return false;
    }
    let mut file = file.clone();
    file.media_info = Some(match file.media_info.take() {
        Some(mut kept) => {
            kept.streams = probed.streams;
            kept.bit_rate = probed.bit_rate;
            if kept.runtime_secs <= 0.0 {
                kept.runtime_secs = probed.runtime_secs;
            }
            kept
        }
        None => probed,
    });
    app.db.save_file(&file).is_ok()
}

/// The library, with the streams of the files behind one item read if they had not been.
pub async fn with_streams(app: &App, lib: std::sync::Arc<Library>, id: Id) -> Result<std::sync::Arc<Library>, Response> {
    let files: Vec<(Title, MediaFile)> = match lib.node(id) {
        Some(Node::Movie(t)) => lib.files_of(t.id).iter().map(|f| (t.clone(), f.clone())).collect(),
        Some(Node::Episode(..)) => lib.file_for(id, None).map(|(t, f)| (t.clone(), f.clone())).into_iter().collect(),
        _ => vec![],
    };
    let mut learned = false;
    for (t, f) in &files {
        learned |= learn_streams(app, t, f).await;
    }
    if !learned {
        return Ok(lib);
    }
    Library::shared(app).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Bring older titles and files up to what players show: cast, scores and artwork for titles
/// added before those were kept, and the stream list of files probed before it was recorded.
/// Works through them slowly, so it is never in the way.
pub async fn fill_in(app: App) {
    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    let titles = app.db.titles(None).unwrap_or_default();
    let mut refreshed = 0;
    for t in titles.iter().filter(|t| t.details_at == 0) {
        if app.refresh_title(t.id).await.is_ok() {
            refreshed += 1;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
    let mut probed = 0;
    for t in &titles {
        for f in app.db.files(t.id).unwrap_or_default() {
            if !app.title_online(t) {
                break;
            }
            if learn_streams(&app, t, &f).await {
                probed += 1;
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
    }
    if refreshed + probed > 0 {
        tracing::info!(refreshed, probed, "filled in details for players");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ranges_are_read_as_players_send_them() {
        assert_eq!(byte_range(None, 1000), Ok((0, 999, false)));
        assert_eq!(byte_range(Some("bytes=0-"), 1000), Ok((0, 999, true)));
        assert_eq!(byte_range(Some("bytes=200-299"), 1000), Ok((200, 299, true)));
        assert_eq!(byte_range(Some("bytes=900-5000"), 1000), Ok((900, 999, true)));
        assert_eq!(byte_range(Some("bytes=-100"), 1000), Ok((900, 999, true)));
        assert_eq!(byte_range(Some("bytes=1000-"), 1000), Err(()));
        assert_eq!(byte_range(Some("bytes=5-2"), 1000), Err(()));
        assert_eq!(byte_range(Some("bytes=abc"), 1000), Err(()));
    }

    #[test]
    fn subtitles_can_be_given_as_webvtt() {
        let vtt = to_vtt("1\n00:00:01,000 --> 00:00:02,500\nHello, there\n");
        assert!(vtt.starts_with("WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.500\nHello, there\n"));
    }
}
