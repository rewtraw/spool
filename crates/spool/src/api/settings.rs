//! Settings, Usenet servers, profiles, indexers and tasks.

use super::*;

pub(super) async fn get_general(State(app): State<App>) -> Json<General> {
    let mut g = app.settings.general();
    g.tmdb_api_key = mask(&g.tmdb_api_key);
    g.plex_token = mask(&g.plex_token);
    g.password = mask(&g.password);
    g.opensubtitles_api_key = mask(&g.opensubtitles_api_key);
    g.opensubtitles_password = mask(&g.opensubtitles_password);
    g.players_password = mask(&g.players_password);
    for h in &mut g.links {
        h.key = mask(&h.key);
    }
    Json(g)
}

pub(super) async fn put_general(State(app): State<App>, headers: HeaderMap, Json(mut g): Json<General>) -> Result<Response, ApiError> {
    let old = app.settings.general();
    g.tmdb_api_key = unmask(g.tmdb_api_key, &old.tmdb_api_key);
    g.plex_token = unmask(g.plex_token, &old.plex_token);
    g.password = unmask(g.password, &old.password);
    g.opensubtitles_api_key = unmask(g.opensubtitles_api_key, &old.opensubtitles_api_key);
    g.opensubtitles_password = unmask(g.opensubtitles_password, &old.opensubtitles_password);
    g.players_password = unmask(g.players_password, &old.players_password);
    if g.players_password != old.players_password || g.players_username != old.players_username {
        // New sign-in details: every player has to sign in again.
        crate::jellyfin::sign_out_all(&app);
    }
    if g.opensubtitles_username != old.opensubtitles_username || g.opensubtitles_password != old.opensubtitles_password {
        *app.subtitle_login.lock() = None;
    }
    let mut cookie = None;
    if g.password != old.password {
        // A new password signs every other browser out; the one that set it stays in.
        let mine = session_cookie(&headers).filter(|c| app.sessions.lock().contains(c)).unwrap_or_else(|| {
            let token = crate::settings::new_key();
            cookie = Some(format!("spool_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000"));
            token
        });
        *app.sessions.lock() = HashSet::from([mine]);
        save_sessions(&app);
    }
    if g.mode == crate::settings::Mode::Active && old.mode != g.mode {
        if !app.volume_ok() {
            return Err(ApiError(StatusCode::CONFLICT, "the media volume is not mounted".into()));
        }
        tracing::warn!("Spool is now active: it will download and write to the library");
    }
    // The disk each pair of folders needs is Spool's to work out, whatever was sent.
    g.required_volume = crate::app::disk_needed([&g.movie_root, &g.series_root], &old.required_volume);
    for l in &mut g.locations {
        let known = old.locations.iter().find(|o| o.id == l.id && l.id != 0).map(|o| o.volume.as_str()).unwrap_or("");
        l.volume = crate::app::disk_needed([&l.movie_root, &l.series_root], known);
    }
    let moved = g.locations != old.locations || g.movie_root != old.movie_root || g.series_root != old.series_root || g.required_volume != old.required_volume;
    if g.locations != old.locations {
        g.locations = app.tidy_locations(std::mem::take(&mut g.locations)).map_err(|why| ApiError(StatusCode::BAD_REQUEST, why))?;
    }
    for h in &mut g.links {
        h.key = unmask(std::mem::take(&mut h.key), old.links.iter().find(|o| o.id == h.id && h.id != 0).map(|o| o.key.as_str()).unwrap_or(""));
    }
    // The count of links given out is Spool's own to keep, whatever was sent.
    g.link_seq = old.link_seq;
    if g.links != old.links {
        (g.links, g.link_seq) = crate::remote::tidy_links(std::mem::take(&mut g.links), &old.links, old.link_seq).map_err(|why| ApiError(StatusCode::BAD_REQUEST, why))?;
    }
    if g.new_titles_location != 0 && !g.locations.iter().any(|l| l.id == g.new_titles_location && l.manage) {
        g.new_titles_location = 0;
    }
    app.settings.set_general(&g)?;
    if moved {
        // Look at what was just described, off to one side: a share that is slow to answer
        // must not hold up the reply.
        let again = app.clone();
        let _ = tokio::task::spawn_blocking(move || again.watch_locations()).await;
        app.location_changes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        app.emit(Event::Locations);
    }
    app.apply_engine_config();
    app.sync_remotes();
    Ok(match cookie {
        Some(c) => ([(header::SET_COOKIE, c)], Json(json!({"ok": true}))).into_response(),
        None => Json(json!({"ok": true})).into_response(),
    })
}

pub(super) async fn get_naming(State(app): State<App>) -> Json<spool_core::naming::NamingConfig> {
    Json(app.settings.naming())
}

pub(super) async fn put_naming(State(app): State<App>, Json(n): Json<spool_core::naming::NamingConfig>) -> R<Value> {
    app.settings.set_naming(&n)?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn get_size_limits(State(app): State<App>) -> Json<crate::settings::SizeLimits> {
    Json(app.settings.size_limits())
}

pub(super) async fn put_size_limits(State(app): State<App>, Json(mut limits): Json<crate::settings::SizeLimits>) -> R<Value> {
    // An entry with no minimum and no maximum says nothing; drop it.
    limits.retain(|_, l| l.min > 0.0 || l.max.is_some_and(|m| m > 0.0));
    for l in limits.values_mut() {
        l.max = l.max.filter(|m| *m > 0.0);
    }
    app.settings.set_size_limits(&limits)?;
    Ok(Json(json!({"ok": true})))
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
    if !["rss", "backlog", "refresh", "scan", "housekeeping", "plex", "smaller", "subtitles", "anime"].contains(&name.as_str()) {
        return Err(not_found("task"));
    }
    let app2 = app.clone();
    tokio::spawn(async move {
        app2.run_task(&name).await;
    });
    Ok(Json(json!({"started": true})))
}
