//! Compact versions: asking for them, calling them off, and how the library stands for them.

use super::*;

fn ids(b: &Value, key: &str) -> Vec<i64> {
    b[key].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default()
}

/// Put titles, or single files, in line for a compact version. One thing asked for that
/// cannot have one is an error with the reason; of several, those refused are named.
pub(super) async fn compact_make(State(app): State<App>, Json(b): Json<Value>) -> R<Value> {
    let (titles, files) = (ids(&b, "title_ids"), ids(&b, "file_ids"));
    let mut asked = app.compact_titles(&titles)?;
    let more = app.compact_files(&files, files.len() == 1);
    asked.queued += more.queued;
    asked.refused.extend(more.refused);
    if asked.queued == 0 && titles.len() + files.len() == 1 {
        let why = asked.refused.first().map(|r| r.why.clone()).unwrap_or_else(|| "there is nothing to make one from".into());
        return Err(ApiError(StatusCode::CONFLICT, why));
    }
    Ok(Json(json!({"queued": asked.queued, "refused": asked.refused})))
}

/// Call compact versions off: the files named, or with none named all that wait.
pub(super) async fn compact_cancel(State(app): State<App>, body: Option<Json<Value>>) -> R<Value> {
    let files = body.map(|b| ids(&b.0, "file_ids")).unwrap_or_default();
    let (cancelled, stopping) = app.cancel_compacts(&files);
    Ok(Json(json!({"cancelled": cancelled, "stopping": stopping})))
}

/// Begin no more compact versions, or go on: `{"hold": true}` lets those being made finish
/// and keeps the rest waiting, which is how Spool is restarted without throwing work away.
pub(super) async fn compact_hold(State(app): State<App>, Json(b): Json<Value>) -> R<Value> {
    let hold = b["hold"].as_bool().unwrap_or(true);
    app.hold_compacts(hold);
    Ok(Json(json!({"held": hold})))
}

/// What has a compact version, what would gain from one, and what is being made.
pub(super) async fn compact_state(State(app): State<App>) -> R<Value> {
    let mut report = tokio::task::spawn_blocking({
        let app = app.clone();
        move || app.compact_report()
    })
    .await??;
    report["making"] = app.compacts_shown();
    Ok(Json(report))
}

/// Make a compact version of everything in the library that would gain from one, or with
/// `{"count": n}` of the biggest `n` files that would.
pub(super) async fn compact_everything(State(app): State<App>, body: Option<Json<Value>>) -> R<Value> {
    let count = body.and_then(|b| b.0["count"].as_u64()).map(|n| n as usize);
    let asked = tokio::task::spawn_blocking(move || app.compact_library(count)).await??;
    Ok(Json(json!({"queued": asked.queued, "refused": asked.refused.len()})))
}

/// Keep a compact version and let its original go.
pub(super) async fn compact_keep(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.keep_only_compact(id).map_err(|e| ApiError(StatusCode::CONFLICT, format!("{e:#}")))?;
    Ok(Json(json!({"ok": true})))
}
