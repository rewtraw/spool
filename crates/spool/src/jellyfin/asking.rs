//! Asking for something the library has not got, from the page the players' door gives a
//! browser. Whoever asks is signed in with their own account, as on a player.

use crate::app::App;
use crate::models::{Account, Kind};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

pub fn answer(found: anyhow::Result<Value>) -> Response {
    match found {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"error": format!("{e:#}")}))).into_response(),
    }
}

pub async fn ask(app: &App, me: &Account, body: &Value) -> Response {
    let id = |name: &str| body[name].as_u64().and_then(|n| u32::try_from(n).ok());
    let Some(kind) = body["kind"].as_str().and_then(Kind::parse) else { return answer(Err(anyhow::anyhow!("a film or a series is asked for"))) };
    answer(app.ask_for(me, kind, id("tmdb_id"), id("tvmaze_id")).await)
}

/// Take back an ask of one's own. One already added stays in the library; only the note
/// of having asked goes.
pub fn withdraw(app: &App, me: &Account, id: &str) -> Response {
    // Removed only if it is theirs, in the one step that removes it.
    match id.parse::<i64>().ok().map(|id| app.db.remove_ask(id, Some(me.id))) {
        Some(Ok(true)) => {
            app.emit(crate::app::Event::Attention);
            Json(json!({"ok": true})).into_response()
        }
        Some(Err(e)) => answer(Err(e)),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
