//! What the person signed in has watched: shown on the library and a title's page, marked and
//! unmarked from them, and what to carry on with. It is the same record a player keeps, so
//! what is watched in Infuse shows here and what is marked here shows there.

use super::*;
use crate::jellyfin::ids::Id;
use crate::jellyfin::items::{Library, Node};

/// The person a request is for, or the reason there is none.
fn person(who: Who) -> Result<i64, ApiError> {
    who.account.ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "what is watched is kept for a person; sign in as one".into()))
}

/// How far through it someone is: watched, or the share of it seen. Nothing when untouched.
fn standing(lib: &Library, n: &Node) -> Option<Value> {
    let s = lib.state(n);
    let part = match n {
        Node::Series(t) => {
            let all = lib.episodes_of(t.id).iter().filter(|e| e.season > 0).count() as f64;
            let left = s.unplayed.unwrap_or(0) as f64;
            if all > 0.0 { (all - left) / all } else { 0.0 }
        }
        _ => {
            let runtime = lib.runtime_secs(n);
            if runtime > 0.0 { (s.position / runtime).clamp(0.0, 1.0) } else { 0.0 }
        }
    };
    (s.played || part > 0.0).then(|| json!({"done": s.played, "part": part}))
}

/// Add to each title in a list where the person stands with it.
pub(super) fn mark_list(app: &App, who: Who, titles: &mut [Value]) -> anyhow::Result<()> {
    let Some(account) = who.account else { return Ok(()) };
    let lib = Library::shared(app, account, false)?;
    for v in titles {
        let Some(id) = v["id"].as_i64() else { continue };
        let node = match v["kind"].as_str() {
            Some("movie") => lib.node(Id::Movie(id)),
            _ => lib.node(Id::Series(id)),
        };
        if let Some(s) = node.and_then(|n| standing(&lib, &n)) {
            v["watched"] = s;
        }
    }
    Ok(())
}

/// Add to a title's page where the person stands with it and with each of its episodes.
pub(super) fn mark_title(app: &App, who: Who, t: &Title, v: &mut Value) -> anyhow::Result<()> {
    let Some(account) = who.account else { return Ok(()) };
    let lib = Library::shared(app, account, false)?;
    let place = |n: &Node| {
        let s = lib.state(n);
        json!({"done": s.played, "position": s.position, "runtime": lib.runtime_secs(n)})
    };
    match t.kind {
        Kind::Movie => {
            if let Some(n) = lib.node(Id::Movie(t.id)) {
                v["watched"] = place(&n);
            }
        }
        Kind::Series => {
            if let Some(n) = lib.node(Id::Series(t.id)) {
                v["watched"] = standing(&lib, &n).unwrap_or(json!({"done": false, "part": 0.0}));
            }
            let episodes: serde_json::Map<String, Value> = lib.episodes_of(t.id).iter().map(|e| (e.id.to_string(), place(&Node::Episode(t, e)))).collect();
            v["watched_episodes"] = json!(episodes);
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Mark {
    title_id: i64,
    /// One season of a series; with neither this nor an episode, the whole of it.
    season: Option<u32>,
    episode_id: Option<i64>,
    played: bool,
}

/// Mark a film, an episode, a season or a whole series watched or not. A season or a series
/// is the episodes of it there are to watch.
pub(super) async fn mark(State(app): State<App>, who: Option<axum::Extension<Who>>, Json(m): Json<Mark>) -> R<Value> {
    let account = person(Who::of(who))?;
    let t = app.db.title(m.title_id)?.ok_or_else(|| not_found("title"))?;
    let targets: Vec<(&str, i64)> = match t.kind {
        Kind::Movie => vec![("movie", t.id)],
        Kind::Series => app
            .db
            .episodes(t.id)?
            .into_iter()
            .filter(|e| match (m.episode_id, m.season) {
                (Some(id), _) => e.id == id,
                (None, Some(season)) => e.season == season && e.file_id.is_some(),
                (None, None) => e.season > 0 && e.file_id.is_some(),
            })
            .map(|e| ("episode", e.id))
            .collect(),
    };
    if targets.is_empty() {
        return Err(not_found("episode"));
    }
    for (kind, id) in &targets {
        crate::jellyfin::media::set(&app, account, kind, *id, m.played)?;
    }
    app.emit(Event::Title { id: t.id });
    Ok(Json(json!({"marked": targets.len()})))
}

/// What the person was part-way through and the next episode of what they have been
/// watching, the most recent first.
pub(super) async fn watching(State(app): State<App>, who: Option<axum::Extension<Who>>) -> R<Value> {
    let account = person(Who::of(who))?;
    let lib = Library::shared(&app, account, false)?;
    let item = |n: &Node, next: bool| {
        let s = lib.state(n);
        let (t, episode) = match n {
            Node::Movie(t) => (*t, None),
            Node::Episode(t, e) => (*t, Some(json!({"id": e.id, "season": e.season, "episode": e.episode, "title": e.title}))),
            _ => return None,
        };
        // A next episode is as recent as the watching of the series it follows on from.
        let last_played = if next { lib.state(&Node::Series(t)).last_played } else { s.last_played };
        Some(json!({
            "title_id": t.id, "kind": t.kind, "title": t.title, "year": t.year, "poster": t.poster, "tmdb_id": t.tmdb_id,
            "episode": episode, "position": s.position, "runtime": lib.runtime_secs(n), "last_played": last_played, "next": next,
        }))
    };
    let mut all: Vec<Value> = lib.resume(None, 20).iter().filter_map(|n| item(n, false)).collect();
    // A series carried on from part-way through one episode is not also offered its next.
    let started: HashSet<i64> = all.iter().filter_map(|v| v["title_id"].as_i64()).collect();
    all.extend(lib.next_up(None, 20).iter().filter_map(|n| item(n, true)).filter(|v| v["title_id"].as_i64().is_some_and(|id| !started.contains(&id))));
    all.sort_by_key(|v| std::cmp::Reverse(v["last_played"].as_i64().unwrap_or(0)));
    all.truncate(20);
    Ok(Json(json!(all)))
}
