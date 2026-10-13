//! The people who play: their accounts, the players each is signed in on, and what they
//! have played.

use super::*;

/// How far back "lately" goes when what someone has watched is added up, in seconds.
const LATELY: i64 = 7 * 24 * 3600;

/// A player's name to this page: something made from its token, which is never handed out.
fn device_name(token: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, token.as_bytes()).as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Every account, with the players signed in to it, what it is playing now and what it has
/// played in all; and the latest sittings, whoever's they were.
pub(super) async fn accounts(State(app): State<App>) -> R<Value> {
    let (now, playing) = (crate::db::now(), app.playing_now());
    let (devices, totals) = (app.db.devices()?, app.db.play_totals(now - LATELY)?);
    let accounts = app.db.accounts()?;
    let names: HashMap<i64, &str> = accounts.iter().map(|a| (a.id, a.name.as_str())).collect();
    let list: Vec<Value> = accounts
        .iter()
        .map(|a| {
            let on: Vec<Value> = devices.iter().filter(|d| d.account_id == a.id).map(|d| json!({"id": device_name(&d.token), "name": d.name, "client": d.client, "created_at": d.created_at, "seen_at": d.seen_at})).collect();
            json!({
                "id": a.id, "name": a.name, "enabled": a.enabled, "adds": a.adds, "compact_away": a.compact_away, "has_password": !a.password.is_empty(), "created_at": a.created_at,
                "devices": on, "playing": playing.iter().filter(|p| p.account.eq_ignore_ascii_case(&a.name)).collect::<Vec<_>>(),
                "totals": totals.get(&a.id).cloned().unwrap_or_default(),
            })
        })
        .collect();
    let recent: Vec<Value> = app.db.plays(None, 50)?.into_iter().map(|p| json!({"account": names.get(&p.account_id).copied().unwrap_or_default(), "play": p})).collect();
    // What an account held to compact versions would not see away from home: the films and
    // episodes that have none made.
    let (films, episodes) = app.db.without_compact()?;
    Ok(Json(json!({"accounts": list, "recent": recent, "without_compact": {"films": films, "episodes": episodes}})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountChange {
    name: Option<String>,
    /// Left out, the password stays as it is; empty, the account has none.
    password: Option<String>,
    enabled: Option<bool>,
    /// Whether what the account asks for is added without waiting.
    adds: Option<bool>,
    /// Whether, away from home, the account is given compact versions and nothing else.
    compact_away: Option<bool>,
}

fn named(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 40 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "an account needs a name, of forty letters or fewer".into()));
    }
    Ok(name.to_string())
}

fn hashed(password: &str) -> Result<String, ApiError> {
    Ok(if password.is_empty() { String::new() } else { crate::settings::hash_password(password)? })
}

/// Names are told apart without regard to capitals, as they are when signing in.
fn taken(app: &App, name: &str, but: i64) -> Result<(), ApiError> {
    match app.db.accounts()?.iter().any(|a| a.id != but && a.name.eq_ignore_ascii_case(name)) {
        true => Err(ApiError(StatusCode::CONFLICT, format!("there is already an account called {name}"))),
        false => Ok(()),
    }
}

pub(super) async fn account_add(State(app): State<App>, Json(b): Json<AccountChange>) -> R<Value> {
    let name = named(b.name.as_deref().unwrap_or_default())?;
    taken(&app, &name, 0)?;
    let id = app.db.add_account(&name, &hashed(b.password.as_deref().unwrap_or_default())?)?;
    tracing::info!(account = %name, "an account for a player was added");
    Ok(Json(json!({"id": id})))
}

/// Change an account. A new password, or an account that may no longer sign in, signs its
/// players out: what they hold was given under what no longer stands.
pub(super) async fn account_change(State(app): State<App>, Path(id): Path<i64>, Json(b): Json<AccountChange>) -> R<Value> {
    let mut a = app.db.account(id)?.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "no such account".into()))?;
    if let Some(name) = &b.name {
        a.name = named(name)?;
        taken(&app, &a.name, id)?;
    }
    let mut out = false;
    if let Some(password) = &b.password {
        a.password = hashed(password)?;
        out = true;
    }
    if let Some(enabled) = b.enabled {
        out |= a.enabled && !enabled;
        a.enabled = enabled;
    }
    if let Some(adds) = b.adds {
        a.adds = adds;
    }
    if let Some(compact) = b.compact_away {
        a.compact_away = compact;
    }
    app.db.save_account(&a)?;
    if out {
        app.db.remove_devices(None, Some(id))?;
    }
    app.reload_players();
    Ok(Json(json!({"ok": true})))
}

/// Remove an account, with what it watched and played.
pub(super) async fn account_remove(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.db.remove_account(id)?;
    app.reload_players();
    Ok(Json(json!({"ok": true})))
}

/// Sign one of an account's players out, or all of them with `all`.
pub(super) async fn device_remove(State(app): State<App>, Path((id, device)): Path<(i64, String)>) -> R<Value> {
    if device == "all" {
        app.db.remove_devices(None, Some(id))?;
    } else {
        let token = app.db.devices()?.into_iter().find(|d| d.account_id == id && device_name(&d.token) == device).map(|d| d.token).ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "no such player".into()))?;
        app.db.remove_devices(Some(&token), None)?;
    }
    app.reload_players();
    Ok(Json(json!({"ok": true})))
}

/// What one account has played, newest first.
pub(super) async fn account_plays(State(app): State<App>, Path(id): Path<i64>) -> R<Vec<Play>> {
    Ok(Json(app.db.plays(Some(id), 200)?))
}

/// Sittings and watched marks from Plex, for the account they are to be kept under.
pub(super) async fn account_from_plex(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.db.account(id)?.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "no such account".into()))?;
    Ok(Json(app.plex_watched(id).await?))
}

/// What has been asked for, by anyone, with how each stands.
pub(super) async fn asks(State(app): State<App>) -> R<Vec<Value>> {
    Ok(Json(app.asks_report(None)?))
}

/// Answer an ask: `add` it to the library, or `decline` it.
pub(super) async fn ask_answer(State(app): State<App>, Path((id, answer)): Path<(i64, String)>) -> R<Value> {
    match answer.as_str() {
        "add" => Ok(Json(json!({"title_id": app.ask_add(id).await?.id}))),
        "decline" => {
            app.ask_decline(id)?;
            Ok(Json(json!({"ok": true})))
        }
        _ => Err(ApiError(StatusCode::NOT_FOUND, format!("an ask is answered with add or decline, not {answer}"))),
    }
}

/// Clear an ask away. The title it became, if any, stays in the library.
pub(super) async fn ask_remove(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.db.remove_ask(id, None)?;
    app.emit(Event::Attention);
    Ok(Json(json!({"ok": true})))
}
