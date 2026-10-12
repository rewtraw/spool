//! The parts of the players' API that touch files and viewing: sending video, images and
//! subtitles, and recording what was watched.

use super::ids::Id;
use super::items::{Library, Node};
use crate::app::App;
use crate::models::*;
use crate::serve::send_file;
use anyhow::Result;
use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::path::{Path, PathBuf};

/// The video itself: a film, an episode, or one named version of a film. A player that can
/// reach the machine holding the file is sent there with a ticket for it; any other is sent
/// the file from here.
pub async fn stream(app: &App, id: &str, source: Option<&str>, headers: &HeaderMap, head: bool, player: Option<std::net::IpAddr>) -> Response {
    let Some(id) = Id::parse(id) else { return StatusCode::NOT_FOUND.into_response() };
    let path: Option<PathBuf> = Library::shared(app).ok().and_then(|lib| lib.extra_path(id).or_else(|| lib.file_for(id, source.and_then(Id::parse)).map(|(t, f)| t.file_path(f))));
    let Some(path) = path else { return StatusCode::NOT_FOUND.into_response() };
    // A player on Tailscale is placed by where it is really reached, which is what says
    // whether it can reach the other machine too.
    // It is known by both addresses: one named by its Tailscale address is sent wherever it is.
    let really = match player {
        Some(ip) => crate::link::tailnet::really(ip).await,
        None => None,
    };
    let known: Vec<std::net::IpAddr> = really.into_iter().chain(player).collect();
    let sent = app.play_direct(&path, &known);
    if let (Some(ip), false) = (player, head) {
        app.note_playing(ip, &path, sent.as_ref().map(|s| s.0.as_str()));
    }
    match sent {
        // Followed for this request only: the next one is asked of Spool again, and may be
        // answered differently.
        Some((_, there)) => (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, there.as_str()), (header::CACHE_CONTROL, "private, no-store"), (header::REFERRER_POLICY, "no-referrer")]).into_response(),
        None => send_file(&path, headers, head).await,
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
    fn subtitles_can_be_given_as_webvtt() {
        let vtt = to_vtt("1\n00:00:01,000 --> 00:00:02,500\nHello, there\n");
        assert!(vtt.starts_with("WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.500\nHello, there\n"));
    }
}
