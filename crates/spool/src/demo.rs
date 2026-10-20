//! A made-up library to look at.
//!
//! `spool demo DIR` lays out a few folders of films and series under DIR, with small stand-in
//! files, and records them: a main library, a second location Spool manages and a third it
//! only reads. Nothing in it needs a key or a network. `tools/demo.mjs` starts a Spool on it,
//! sets a slow move going and pictures every page, so a change to the web app can be seen
//! before it reaches a real library.

use crate::app::App;
use crate::models::*;
use anyhow::{bail, Result};
use serde_json::json;
use std::path::Path;

/// Films out of copyright: title, year, minutes, where it is kept, megabytes of stand-in file.
const FILMS: &[(&str, u32, u32, &str, u64)] = &[
    ("Battleship Potemkin", 1925, 75, "Movies", 1),
    ("The Cabinet of Dr. Caligari", 1920, 76, "Movies", 1),
    ("The General", 1926, 79, "Movies", 240),
    ("The Gold Rush", 1925, 95, "Movies", 1),
    ("The Kid", 1921, 68, "Movies", 1),
    ("Metropolis", 1927, 153, "Movies", 2),
    ("Nosferatu", 1922, 94, "Movies", 1),
    ("Safety Last!", 1923, 73, "Movies", 1),
    ("Sherlock Jr.", 1924, 45, "Shelf/Films", 1),
    ("His Girl Friday", 1940, 92, "Shelf/Films", 1),
    ("Detour", 1945, 68, "Vault/Films", 1),
    ("Charade", 1963, 113, "Vault/Films", 1),
    ("Night of the Living Dead", 1968, 96, "", 0),
];

/// Set a folder up as a demonstration library and record what is in it.
pub async fn seed(app: &App, root: &Path) -> Result<usize> {
    if !app.db.titles(None)?.is_empty() {
        bail!("this data folder already has a library; a demonstration needs an empty one of its own");
    }
    let at = |rel: &str| root.join(rel).to_string_lossy().to_string();
    for d in ["Movies", "Series", "Shelf/Films", "Shelf/Series", "Vault/Films", "Downloads"] {
        std::fs::create_dir_all(root.join(d))?;
    }
    let mut g = serde_json::to_value(app.settings.general())?;
    g["movie_root"] = at("Movies").into();
    g["series_root"] = at("Series").into();
    g["downloads_dir"] = at("Downloads").into();
    // Out of the way of a real Spool, or a Jellyfin, on the same machine.
    g["players_port"] = 18096.into();
    g["locations"] = json!([
        {"id": 1, "name": "Shelf", "movie_root": at("Shelf/Films"), "series_root": at("Shelf/Series"), "manage": true},
        {"id": 2, "name": "Vault", "movie_root": at("Vault/Films"), "manage": false},
    ]);
    app.settings.set_general(&serde_json::from_value(g)?)?;
    let profile = |kind: &str| app.db.profiles().ok().and_then(|p| p.into_iter().find(|p| p.kind == kind)).map(|p| p.id).unwrap_or(0);

    for (i, (title, year, minutes, place, megabytes)) in FILMS.iter().enumerate() {
        let name = format!("{} ({year})", title.replace('!', ""));
        let mut t: Title = serde_json::from_value(json!({
            "kind": "movie", "title": title, "year": year, "runtime": minutes, "imdb_id": format!("tt99{i:05}"), "minimum_availability": "announced",
            "monitored": *place != "Vault/Films", "overview": "A stand-in entry in a demonstration library.",
        }))?;
        t.profile_id = profile("movie");
        if !place.is_empty() {
            let folder = root.join(place).join(&name);
            std::fs::create_dir_all(&folder)?;
            // A file of the size wanted that takes no room until it is copied.
            std::fs::File::create(folder.join(format!("{name} Bluray-1080p.mkv")))?.set_len(megabytes << 20)?;
            std::fs::write(folder.join(format!("{name} Bluray-1080p.en.srt")), "1\n00:00:01,000 --> 00:00:02,000\nA stand-in.\n")?;
            t.path = folder.to_string_lossy().to_string();
        }
        app.db.save_title(&mut t)?;
    }

    let mut s: Title = serde_json::from_value(json!({
        "kind": "series", "title": "Signal Fires", "year": 2021, "tvdb_id": 9_900_001, "monitored": true,
        "seasons": [{"number": 1, "monitored": true}], "overview": "A stand-in series in a demonstration library.",
    }))?;
    s.profile_id = profile("tv");
    let folder = root.join("Series/Signal Fires (2021)");
    std::fs::create_dir_all(folder.join("Season 01"))?;
    s.path = folder.to_string_lossy().to_string();
    app.db.save_title(&mut s)?;
    for e in 1..=8u32 {
        let episode: Episode = serde_json::from_value(json!({"title_id": s.id, "season": 1, "episode": e, "title": format!("Part {e}"), "air_date": format!("2021-03-{:02}", e * 3), "runtime": 45, "monitored": true}))?;
        app.db.upsert_episode(&episode)?;
        // The last two are left missing.
        if e <= 6 {
            std::fs::File::create(folder.join(format!("Season 01/Signal Fires - S01E{e:02} - Part {e} WEBDL-1080p.mkv")))?.set_len(1 << 20)?;
        }
    }
    app.scan_all().await?;

    // Someone who has watched a little: two films, one of them part-way, and the start of the
    // series, stopped in its third episode.
    let me = app.db.add_account("You", "")?;
    let titles = app.db.titles(None)?;
    let id_of = |name: &str| titles.iter().find(|t| t.title == name).map(|t| t.id);
    let now = crate::db::now();
    let seen = |played: bool, position: f64, ago: i64| Watched { played, position, play_count: played as u32, last_played_at: now - ago, updated_at: now - ago, ..Default::default() };
    for (name, w) in [("Nosferatu", seen(true, 0.0, 86400 * 3)), ("Metropolis", seen(false, 41.0 * 60.0, 3600))] {
        if let Some(id) = id_of(name) {
            app.db.save_watched(me, "movie", id, &w)?;
        }
    }
    for e in app.db.episodes(s.id)? {
        let w = match e.episode {
            1 | 2 => seen(true, 0.0, 86400 * 2 - e.episode as i64),
            3 => seen(false, 17.0 * 60.0, 7200),
            _ => continue,
        };
        app.db.save_watched(me, "episode", e.id, &w)?;
    }
    Ok(app.db.titles(None)?.len())
}
