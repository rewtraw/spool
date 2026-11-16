//! Settings, Usenet servers, profiles, indexers and tasks.

use super::*;

/// The settings' secrets, by where they are in the settings. They are sent to a browser
/// blank, with whether each is set; a change that leaves one blank or out keeps it, and null
/// clears it.
const SECRETS: &[&[&str]] = &[&["tmdb_api_key"], &["plex_token"], &["opensubtitles_api_key"], &["opensubtitles_password"], &["books", "annas_key"], &["books", "florilegium_token"], &["soulseek", "password"]];

fn at<'a>(v: &'a mut Value, path: &[&str]) -> Option<&'a mut Value> {
    path.iter().try_fold(v, |v, k| v.get_mut(*k))
}

/// How Soulseek stands: signed in or why not, what is shared, and what is being sent.
pub(super) async fn soulseek(State(app): State<App>) -> Json<Value> {
    Json(app.soulseek_status())
}

/// The settings as a browser sees them.
pub(super) async fn get_settings(State(app): State<App>) -> Json<Value> {
    // The main location first, there even before it has folders.
    let mut settings = app.settings.general();
    settings.main_mut();
    settings.locations.sort_by_key(|l| l.id != 0);
    let mut g = serde_json::to_value(settings).unwrap_or_default();
    let mut set = serde_json::Map::new();
    for path in SECRETS {
        if let Some(v) = at(&mut g, path) {
            set.insert(path.join("."), json!(v.as_str().is_some_and(|s| !s.is_empty())));
            *v = json!("");
        }
    }
    for l in g["links"].as_array_mut().into_iter().flatten() {
        l["key_set"] = json!(l["key"].as_str().is_some_and(|s| !s.is_empty()));
        l["key"] = json!("");
    }
    // Worked out by Spool, never set.
    g.as_object_mut().map(|o| o.remove("link_seq"));
    g["secrets"] = Value::Object(set);
    Json(g)
}

/// Refuse a location or a link with a field it does not have: a mistyped folder would
/// otherwise be dropped, and the folder it meant to set cleared.
fn known_fields(change: &Value) -> Result<(), String> {
    let fields = |v: Value| v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
    for (list, mut have) in [("locations", fields(serde_json::to_value(crate::settings::Location::default()).unwrap_or_default())), ("links", fields(serde_json::to_value(crate::settings::LinkConf::default()).unwrap_or_default()))] {
        have.push("key_set".into());
        for entry in change[list].as_array().into_iter().flatten() {
            if let Some(bad) = entry.as_object().and_then(|o| o.keys().find(|k| !have.contains(k))) {
                return Err(format!("a {} has no setting called {bad}", if list == "links" { "link" } else { "location" }));
            }
        }
    }
    Ok(())
}

/// Lay a change over what is there: objects field by field, anything else in its place.
fn merge(into: &mut Value, change: Value) -> Result<(), String> {
    match change {
        Value::Object(fields) => {
            for (k, v) in fields {
                let Some(slot) = into.get_mut(&k) else { return Err(format!("there is no setting called {k}")) };
                if slot.is_object() && v.is_object() {
                    merge(slot, v)?;
                } else {
                    *slot = v;
                }
            }
            Ok(())
        }
        _ => Err("a change to the settings is an object of the settings to change".into()),
    }
}

/// Change some settings: only those sent, laid over the settings as they are at that moment,
/// checked as a whole and written at once.
pub(super) async fn patch_settings(State(app): State<App>, Json(mut change): Json<Value>) -> Result<Response, ApiError> {
    let bad = |why: String| ApiError(StatusCode::BAD_REQUEST, why);
    // Saving the account again, unchanged, signs in again after being signed out.
    let soulseek_saved = change.get("soulseek").is_some();
    // What Spool works out for itself is not taken from a browser.
    if let Some(o) = change.as_object_mut() {
        for k in ["link_seq", "secrets"] {
            o.remove(k);
        }
    }
    // A secret sent blank is left as it is, and one sent as null is cleared.
    for path in SECRETS {
        let (last, parent) = path.split_last().expect("a path");
        if let Some(o) = at(&mut change, parent).and_then(Value::as_object_mut) {
            match o.get(*last) {
                Some(Value::String(s)) if s.is_empty() => {
                    o.remove(*last);
                }
                Some(Value::Null) => {
                    o.insert(last.to_string(), json!(""));
                }
                _ => {}
            }
        }
    }
    let watch = app.clone();
    let result = app.settings.update(|g| {
        let old = g.clone();
        let mut v = serde_json::to_value(&*g)?;
        known_fields(&change).map_err(|e| anyhow::anyhow!(e))?;
        merge(&mut v, change).map_err(|e| anyhow::anyhow!(e))?;
        let mut new: General = serde_json::from_value(v).map_err(|e| anyhow::anyhow!("{e}"))?;
        settle(&watch, &old, &mut new).map_err(|e| anyhow::anyhow!(e))?;
        *g = new;
        Ok(old)
    });
    let (g, old) = result.map_err(|e| bad(format!("{e:#}")))?;
    if g.paused != old.paused {
        app.apply_pause(g.paused);
    }
    if g.opensubtitles_username != old.opensubtitles_username || g.opensubtitles_password != old.opensubtitles_password || g.opensubtitles_api_key != old.opensubtitles_api_key {
        *app.subtitle_login.lock() = None;
    }
    if old.compact.auto && !g.compact.auto {
        app.drop_automatic_compacts();
    }
    let moved = g.locations != old.locations;
    if moved {
        // Look at what was just described, off to one side: a share that is slow to answer
        // must not hold up the reply.
        let again = app.clone();
        let _ = tokio::task::spawn_blocking(move || again.watch_locations()).await;
        app.location_changes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        app.emit(Event::Locations);
        // A music folder taken out of the locations is no longer shared, at once.
        app.soulseek_share();
    }
    if g.downloads_dir != old.downloads_dir {
        app.apply_engine_config();
    }
    if g.links != old.links {
        app.sync_remotes();
    }
    if g.soulseek != old.soulseek || soulseek_saved {
        app.sync_soulseek();
    }
    Ok(Json(json!({"ok": true})).into_response())
}

/// Check the settings as they would be, and put in what Spool works out for itself.
fn settle(app: &App, old: &General, g: &mut General) -> Result<(), String> {
    // Checked against each other as they would be, whichever changed.
    let list = std::mem::take(&mut g.locations);
    g.locations = app.tidy_locations(list)?;
    // A main folder given inside a folder that is there is made, so one folder chosen for the
    // whole library is enough; made first, so the disk it is on is found below. One whose
    // parent is not there is left: its disk may be away.
    if let (Some(main), was) = (g.locations.iter().find(|l| l.id == 0), old.main()) {
        for (new, before) in [(&main.movie_root, &was.movie_root), (&main.series_root, &was.series_root), (&main.music_root, &was.music_root), (&main.audiobook_root, &was.audiobook_root), (&main.video_root, &was.video_root)] {
            let dir = std::path::Path::new(new.as_str());
            if new != before && !new.is_empty() && !dir.exists() && dir.parent().is_some_and(|p| p.is_dir()) {
                if let Err(e) = std::fs::create_dir(dir) {
                    tracing::warn!(folder = %new, error = %e, "a library folder could not be made");
                }
            }
        }
    }
    // The disk each set of folders needs is Spool's to work out.
    for l in &mut g.locations {
        let known = old.locations.iter().find(|o| o.id == l.id).map(|o| o.volume.as_str()).unwrap_or("");
        l.volume = crate::app::disk_needed([&l.movie_root, &l.series_root, &l.music_root, &l.audiobook_root, &l.video_root], known);
    }
    for h in &mut g.links {
        if h.key.is_empty() {
            h.key = old.links.iter().find(|o| o.id == h.id && h.id != 0).map(|o| o.key.clone()).unwrap_or_default();
        }
    }
    g.link_seq = old.link_seq;
    if g.links != old.links {
        (g.links, g.link_seq) = crate::remote::tidy_links(std::mem::take(&mut g.links), &old.links, old.link_seq)?;
    }
    if g.new_titles_location != 0 && !g.locations.iter().any(|l| l.id == g.new_titles_location && l.manage) {
        g.new_titles_location = 0;
    }
    let profiles = app.db.profiles().map_err(|e| e.to_string())?;
    for (id, kind, what) in [(g.default_movie_profile, "movie", "new films"), (g.default_series_profile, "tv", "new series")] {
        if id != 0 && !profiles.iter().any(|p| p.id == id && p.kind == kind) {
            return Err(format!("the profile for {what} is not one of the {} profiles", if kind == "movie" { "film" } else { "series" }));
        }
    }
    if g.compact.location != 0 && !g.locations.iter().any(|l| l.id == g.compact.location && l.manage) {
        return Err("compact versions can only be kept on a location Spool may change".into());
    }
    Ok(())
}

/// A key for an AI session, and the command that sets one up with it, asked for when it is to
/// be used rather than sent with the settings. The read-only key is made, or made again, here.
pub(super) async fn mcp_access(State(app): State<App>, Json(b): Json<Value>) -> R<Value> {
    let mut a = app.settings.access();
    let key = match b["access"].as_str() {
        Some("read") => {
            if b["new"].as_bool() == Some(true) || a.mcp_read_key.is_empty() {
                a.mcp_read_key = crate::settings::new_key();
                app.settings.set_access(&a)?;
            }
            a.mcp_read_key
        }
        Some("none") => {
            a.mcp_read_key.clear();
            app.settings.set_access(&a)?;
            return Ok(Json(json!({"ok": true})));
        }
        _ => a.api_key,
    };
    Ok(Json(json!({"key": key})))
}

/// Whether the read-only key is on, without it.
pub(super) async fn mcp_status(State(app): State<App>) -> Json<Value> {
    Json(json!({"read_key": !app.settings.access().mcp_read_key.is_empty()}))
}

pub(super) async fn get_servers(State(app): State<App>) -> Json<Value> {
    let mut s = app.settings.servers();
    for x in &mut s {
        x.password = mask(&x.password);
    }
    Json(json!({"servers": s, "speed_limit": app.settings.speed_limit()}))
}

pub(super) fn merge_server_secrets(app: &App, servers: &mut [spool_nntp::ServerConfig]) {
    let old = app.settings.servers();
    for s in servers.iter_mut() {
        if s.id.is_empty() {
            s.id = format!("{}:{}", s.host, s.port);
        }
        if s.name.is_empty() {
            s.name = s.host.clone();
        }
        if s.password == MASK {
            s.password = old.iter().find(|o| o.id == s.id).map(|o| o.password.clone()).unwrap_or_default();
        }
    }
}

pub(super) async fn put_servers(State(app): State<App>, Json(b): Json<Value>) -> R<Value> {
    let mut servers: Vec<spool_nntp::ServerConfig> = serde_json::from_value(b["servers"].clone())?;
    merge_server_secrets(&app, &mut servers);
    app.settings.set_servers(&servers)?;
    if let Some(l) = b["speed_limit"].as_u64() {
        app.settings.set_speed_limit(l)?;
    }
    app.apply_engine_config();
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn test_server(State(app): State<App>, Json(s): Json<spool_nntp::ServerConfig>) -> R<Value> {
    let mut one = [s];
    merge_server_secrets(&app, &mut one);
    match spool_nntp::Engine::test_server(&one[0]).await {
        Ok(()) => Ok(Json(json!({"ok": true, "message": "Connected and signed in"}))),
        Err(e) => Ok(Json(json!({"ok": false, "message": e}))),
    }
}

pub(super) async fn list_profiles(State(app): State<App>) -> R<Vec<QualityProfile>> {
    Ok(Json(app.db.profiles()?))
}

pub(super) async fn save_profile(State(app): State<App>, Json(mut p): Json<QualityProfile>) -> R<QualityProfile> {
    if p.items.iter().all(|i| !i.allowed) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "a profile must allow at least one quality".into()));
    }
    app.db.save_profile(&mut p)?;
    Ok(Json(p))
}

pub(super) async fn delete_profile(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let users = app.db.titles(None)?.iter().filter(|t| t.profile_id == id).count();
    if users > 0 {
        return Err(ApiError(StatusCode::CONFLICT, format!("{users} titles use this profile")));
    }
    app.db.delete_profile(id)?;
    Ok(Json(json!({"ok": true})))
}

/// How each profile fits the files of the titles that use it, by location.
pub(super) async fn profile_fit(State(app): State<App>) -> R<Value> {
    Ok(Json(json!(app.profile_fit()?)))
}

pub(super) async fn qualities() -> Json<Vec<Value>> {
    Json(Quality::ALL.iter().filter(|q| **q != Quality::Unknown).map(|q| json!({"key": q.key(), "movie": q.name(Flavor::Movie), "tv": q.name(Flavor::Tv), "resolution": q.resolution(), "weight": q.default_weight()})).collect())
}

pub(super) async fn list_indexers(State(app): State<App>) -> R<Vec<Value>> {
    let budgets = app.indexer_budgets();
    let backoff = app.indexer_backoff.lock().iter().map(|(id, (_, until, why))| (*id, (until.saturating_duration_since(std::time::Instant::now()).as_secs() / 60 + 1, why.clone()))).collect::<HashMap<_, _>>();
    Ok(Json(
        app.db
            .indexers()?
            .into_iter()
            .map(|mut i| {
                i.api_key = mask(&i.api_key);
                let mut v = serde_json::to_value(&i).unwrap_or(Value::Null);
                v["usage"] = json!(budgets.get(&i.id));
                // An indexer that keeps failing is left alone for a while; say so and why.
                v["failing"] = json!(backoff.get(&i.id).map(|(minutes, why)| json!({"retry_in_minutes": minutes, "error": why})));
                v
            })
            .collect(),
    ))
}

pub(super) async fn save_indexer(State(app): State<App>, Json(mut i): Json<Indexer>) -> R<Indexer> {
    if i.api_key == MASK {
        i.api_key = app.db.indexers()?.into_iter().find(|o| o.id == i.id).map(|o| o.api_key).unwrap_or_default();
    }
    app.db.save_indexer(&mut i)?;
    i.api_key = mask(&i.api_key);
    Ok(Json(i))
}

pub(super) async fn delete_indexer(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    app.db.delete_indexer(id)?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn test_indexer(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    let ix = app.db.indexers()?.into_iter().find(|i| i.id == id).ok_or_else(|| not_found("indexer"))?;
    let q = if ix.movies { crate::newznab::Query::RecentMovies } else { crate::newznab::Query::RecentTv };
    Ok(Json(match crate::newznab::fetch(&app.http, &ix, &q).await {
        Ok((r, limits)) => {
            if let Some(l) = limits {
                app.note_indexer_limits(ix.id, l);
            }
            let part = |used: Option<u32>, max: Option<u32>, what: &str| match (used, max) {
                (Some(u), Some(m)) => format!("{u} of {m} {what}"),
                (Some(u), None) => format!("{u} {what} with no limit"),
                (None, Some(m)) => format!("a limit of {m} {what}"),
                (None, None) => String::new(),
            };
            let allowance = match limits {
                Some(l) => format!(". Today it reports {}", [part(l.api_current, l.api_max, "requests"), part(l.grab_current, l.grab_max, "downloads")].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" and ")),
                None => ". It does not report a daily allowance".into(),
            };
            json!({"ok": true, "message": format!("{} recent releases returned{allowance}", r.len())})
        }
        Err(e) => json!({"ok": false, "message": e.to_string()}),
    }))
}

pub(super) async fn run_task(State(app): State<App>, Path(name): Path<String>) -> R<Value> {
    if !["rss", "backlog", "refresh", "scan", "housekeeping", "plex", "smaller", "subtitles", "anime", "books", "book_index", "music", "music_wants"].contains(&name.as_str()) {
        return Err(not_found("task"));
    }
    let app2 = app.clone();
    tokio::spawn(async move {
        app2.run_task(&name).await;
    });
    Ok(Json(json!({"started": true})))
}

/// What Spool still needs before it can do its work, each with where to give it. Optional
/// ones are worth doing but stop nothing.
pub(super) async fn setup(State(app): State<App>) -> R<Value> {
    let g = app.settings.general();
    let places = app.placement().0;
    let with_folders = |l: &crate::settings::Location| !l.movie_root.is_empty() || !l.series_root.is_empty();
    let has_folders = places.iter().any(|(l, _)| with_folders(l));
    let reachable = places.iter().any(|(l, on)| *on && with_folders(l));
    let providers = app.settings.servers().iter().any(|s| s.enabled && !s.host.trim().is_empty());
    let indexers = app.db.indexers()?.iter().any(|i| !i.url.trim().is_empty() && (i.enable_rss || i.enable_search));
    let signs_in = app.db.accounts()?.iter().any(|a| a.enabled && !a.password.is_empty());
    let item = |id: &str, label: &str, done: bool, detail: &str, href: &str, optional: bool| json!({"id": id, "label": label, "done": done, "detail": detail, "href": href, "optional": optional});
    let items = vec![
        item("library", "Where the library is", has_folders && reachable, if !has_folders { "A folder for films and one for series, or one folder to hold both." } else if !reachable { "Its folders cannot be reached: is the disk there?" } else { "" }, "#/settings/storage", false),
        item("usenet", "A Usenet provider", providers, "Its address and sign-in. Without one nothing can be downloaded.", "#/settings/connections", false),
        item("indexer", "An indexer", indexers, "Its address and key. Without one no releases are found.", "#/settings/connections", false),
        item("tmdb", "A TMDB key", !g.tmdb_api_key.is_empty(), "Free from themoviedb.org. Films are looked up with it; series need none.", "#/settings/connections", false),
        item("person", "Someone to sign in as", signs_in, "A name and password, so nobody else who can reach Spool can change it.", "#/settings/people", true),
    ];
    let ready = items.iter().all(|i| i["done"] == true || i["optional"] == true);
    Ok(Json(json!({"ready": ready, "items": items})))
}
