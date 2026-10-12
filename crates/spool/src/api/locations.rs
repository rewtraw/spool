//! The places the library lives.

use super::*;

/// Every place the library lives, with whether it can be reached and how much is on it.
pub(super) async fn locations(State(app): State<App>) -> Json<Value> {
    Json(json!(app.location_report()))
}

/// Each linked Spool: what it serves, what it allows, and how it was last found.
pub(super) async fn links(State(app): State<App>) -> Json<Value> {
    Json(json!(app.link_report()))
}

/// The Spools heard of on the network that this one is not linked to. Each is an address
/// to try and no more: nothing is linked without its code or its key.
pub(super) async fn links_found(State(app): State<App>) -> Json<Value> {
    Json(json!(app.spools_found()))
}

/// Link with the Spool at an address by the code it gave. The key the two agree is handed
/// back for the link's settings, which are saved as any others are.
pub(super) async fn link_pair(Json(b): Json<Value>) -> R<Value> {
    let (url, code) = (b["url"].as_str().unwrap_or_default(), b["code"].as_str().unwrap_or_default());
    match crate::remote::pair(url, code).await {
        Ok(key) => Ok(Json(json!({"key": key}))),
        Err(why) => Err(ApiError(StatusCode::BAD_REQUEST, why)),
    }
}

/// Start looking through a location for films and shows to bring in. Nothing is changed.
pub(super) async fn location_look(State(app): State<App>, Path(id): Path<u32>) -> R<Value> {
    if !app.placement().0.iter().any(|p| p.0.id == id && p.1) {
        return Err(ApiError(StatusCode::CONFLICT, "that location cannot be reached right now".into()));
    }
    if app.adoption.lock().as_ref().is_some_and(|a| a.running) {
        return Err(ApiError(StatusCode::CONFLICT, "a location is already being looked through".into()));
    }
    // Marked as under way before the reply, so whoever asks next sees this look and not the last.
    *app.adoption.lock() = Some(crate::adopt::Adoption { location: id, running: true, step: "looking".into(), ..Default::default() });
    tokio::spawn(async move {
        if let Err(e) = app.adopt_look(id).await {
            tracing::warn!(error = %e, "could not look through the location");
            app.adoption_failed(&format!("{e:#}"));
        }
    });
    Ok(Json(json!({"started": true})))
}

/// Try a location out: can its folders be listed, read and, where Spool is meant to, written.
pub(super) async fn location_check(State(app): State<App>, Path(id): Path<u32>) -> R<Value> {
    let place = app.locations().into_iter().find(|l| l.id == id).ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "no such location".into()))?;
    let lines = app.check_up(&place).await;
    Ok(Json(json!({"location": place.name, "ok": lines.iter().all(|l| l.ok), "checks": lines})))
}

/// What the last look found, or how far it has got.
pub(super) async fn location_found(State(app): State<App>, Path(id): Path<u32>) -> Json<Value> {
    Json(json!(app.adoption_of(id)))
}

#[derive(Deserialize)]
pub(super) struct LocationAdd {
    #[serde(default)]
    picks: Vec<crate::adopt::Pick>,
    #[serde(default)]
    monitor: bool,
}

/// Add what the last look was sure of, and whichever unclear ones the caller has settled.
pub(super) async fn location_add(State(app): State<App>, Path(id): Path<u32>, Json(b): Json<LocationAdd>) -> R<Value> {
    if app.adoption.lock().as_ref().is_some_and(|a| a.running) {
        return Err(ApiError(StatusCode::CONFLICT, "a location is already being worked on".into()));
    }
    // What was found here may have been kept from before another location was looked through.
    match app.adoption_of(id) {
        Some(kept) => *app.adoption.lock() = Some(kept),
        None => return Err(ApiError(StatusCode::CONFLICT, "look through the location first".into())),
    }
    if let Some(a) = app.adoption.lock().as_mut() {
        (a.running, a.done, a.total) = (true, 0, 0);
        a.step = "adding".into();
    }
    tokio::spawn(async move {
        if let Err(e) = app.adopt_add(id, b.picks, b.monitor).await {
            tracing::warn!(error = %e, "could not add titles from the location");
            app.adoption_failed(&format!("{e:#}"));
        }
    });
    Ok(Json(json!({"started": true})))
}
