//! Artwork and collections.

use super::*;

pub(super) async fn art(State(app): State<App>, Path((id, kind)): Path<(i64, String)>) -> Response {
    let image = match (kind.as_str(), crate::art::Art::parse(&kind)) {
        ("small", _) => app.art_small(id).await,
        (_, Some(kind)) => app.art(id, kind).await,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    picture(image)
}

/// The library's collections: the films of one series of films, or an anime's series, films
/// and specials, each with what it is to the story and what of the story is not here.
pub(super) async fn collections(State(app): State<App>, who: Option<axum::Extension<Who>>) -> R<Value> {
    let mut groups = app.collections_for(Who::of(who).account)?;
    for g in &mut groups {
        g.poster = g.poster.as_ref().map(|src| format!("/api/collections/{}/art?v={}", g.id, &crate::art::tag(src)[..10]));
        g.fanart = None;
        for m in &mut g.missing {
            m.poster = m.poster.as_ref().map(|src| match m.tmdb_id {
                Some(tmdb) => format!("/api/film-part/{tmdb}/art?v={}", &crate::art::tag(src)[..10]),
                None => format!("/api/anime/{}/art?v={}", m.anilist_id, &crate::art::tag(src)[..10]),
            });
        }
    }
    Ok(Json(json!(groups)))
}

/// Add a work a collection lacks.
pub(super) async fn collection_add(State(app): State<App>, Json(b): Json<Value>) -> R<Title> {
    if let Some(tmdb) = b["tmdb_id"].as_u64() {
        return Ok(Json(app.add_missing_film(tmdb as u32).await?));
    }
    let id = b["anilist_id"].as_u64().ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "which work?".into()))? as u32;
    Ok(Json(app.add_missing(id).await?))
}

/// A collection of the user's own making, empty until titles are put in it.
pub(super) async fn collection_new(State(app): State<App>, Json(b): Json<Value>) -> R<Value> {
    let name = b["name"].as_str().unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "give the collection a name".into()));
    }
    let mut id = 0;
    app.edit_collections(|e| {
        id = crate::anime::OWN_GROUP | (e.own.iter().map(|o| o.0 & !crate::anime::OWN_GROUP).max().unwrap_or(0) + 1);
        e.own.push((id, name.clone()));
    })?;
    Ok(Json(json!({"id": id, "name": name})))
}

/// Give a collection another name. An empty name gives back the one its source has.
pub(super) async fn collection_rename(State(app): State<App>, Path(id): Path<u32>, Json(b): Json<Value>) -> R<Value> {
    let name = b["name"].as_str().unwrap_or_default().trim().to_string();
    app.edit_collections(|e| {
        if let Some(own) = e.own.iter_mut().find(|o| o.0 == id) {
            if !name.is_empty() {
                own.1 = name.clone();
            }
        } else if name.is_empty() {
            e.names.remove(&id);
        } else {
            e.names.insert(id, name.clone());
        }
    })?;
    Ok(Json(json!({"ok": true})))
}

/// Put a collection's titles in the user's own order. An empty list gives back the story's.
pub(super) async fn collection_order(State(app): State<App>, Path(id): Path<u32>, Json(b): Json<Value>) -> R<Value> {
    let titles: Vec<i64> = b["titles"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default();
    app.edit_collections(|e| {
        if titles.is_empty() {
            e.order.remove(&id);
        } else {
            e.order.insert(id, titles);
        }
    })?;
    Ok(Json(json!({"ok": true})))
}

/// Undo a collection of the user's own. Its titles go back to where the sources put them.
pub(super) async fn collection_remove(State(app): State<App>, Path(id): Path<u32>) -> R<Value> {
    app.edit_collections(|e| {
        e.own.retain(|o| o.0 != id);
        e.placed.retain(|_, g| *g != id);
        e.names.remove(&id);
        e.order.remove(&id);
    })?;
    Ok(Json(json!({"ok": true})))
}

/// Say which collection a title is in, or what it is to its collection. `collection` is a
/// collection's id, 0 for none, or null to leave it to the sources; `role` likewise.
pub(super) async fn title_collection(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<Value>) -> R<Value> {
    app.edit_collections(|e| {
        if let Some(c) = b.get("collection") {
            match c.as_u64() {
                Some(g) => e.placed.insert(id, g as u32),
                None => e.placed.remove(&id),
            };
        }
        if let Some(r) = b.get("role") {
            match r.as_str() {
                Some(role) => e.roles.insert(id, role.trim().to_string()),
                None => e.roles.remove(&id),
            };
        }
    })?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn film_part_art(State(app): State<App>, Path(id): Path<u32>) -> Response {
    let Some(src) = app.db.film_sets().ok().and_then(|sets| sets.into_iter().flat_map(|s| s.parts).find(|p| p.tmdb_id == id)).and_then(|p| p.poster) else { return StatusCode::NOT_FOUND.into_response() };
    picture(app.art_any(&format!("part-{id}"), &src, 400).await)
}

/// An image, or word that there is none. The address carries a version tag, so the image
/// behind it never changes and may be kept for good.
pub(super) fn picture(image: anyhow::Result<Vec<u8>>) -> Response {
    match image {
        Ok(bytes) => ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], bytes).into_response(),
        Err(_) => ([(header::CACHE_CONTROL, "public, max-age=600")], StatusCode::NOT_FOUND).into_response(),
    }
}

pub(super) async fn collection_art(State(app): State<App>, Path(id): Path<u32>) -> Response {
    let Some(src) = app.collections().ok().and_then(|g| g.into_iter().find(|g| g.id == id)).and_then(|g| g.poster) else { return StatusCode::NOT_FOUND.into_response() };
    picture(app.art_any(&format!("set-{id}"), &src, 400).await)
}

pub(super) async fn anime_art(State(app): State<App>, Path(id): Path<u32>) -> Response {
    let Some(src) = app.db.anime(id).ok().flatten().and_then(|m| m.poster) else { return StatusCode::NOT_FOUND.into_response() };
    picture(app.art_any(&format!("anime-{id}"), &src, 400).await)
}
