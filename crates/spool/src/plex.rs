//! Mapping between Spool's library and Plex's: which Plex item each title is, what Plex has
//! that Spool does not track, and what Spool has that Plex has not picked up.

use crate::app::{App, Event};
use crate::db::{json, now};
use crate::models::*;
use anyhow::{anyhow, bail, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use spool_core::parser::{clean_movie_title, clean_series_title};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlexItem {
    pub rating_key: String,
    pub kind: Kind,
    pub title: String,
    pub year: u32,
    pub tmdb_id: Option<u32>,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub section: String,
    /// Movie files, or the series folder.
    pub paths: Vec<String>,
    /// Times a movie was played; for a series, episodes watched.
    #[serde(default)]
    pub watched: u32,
    /// Episodes Plex has for a series; 1 for a movie.
    #[serde(default)]
    pub items: u32,
    #[serde(default)]
    pub last_viewed_at: i64,
}

impl PlexItem {
    /// Watched in full: the movie played at least once, or every episode Plex has.
    pub fn fully_watched(&self) -> bool {
        self.watched > 0 && (self.kind == Kind::Movie || self.watched >= self.items)
    }
}

/// Lookup from Spool titles to Plex items.
pub struct PlexIndex {
    pub machine: String,
    pub items: Vec<PlexItem>,
    by_tmdb: HashMap<(Kind, u32), usize>,
    by_tvdb: HashMap<(Kind, u32), usize>,
    by_imdb: HashMap<String, usize>,
    by_name: HashMap<(Kind, String, u32), usize>,
}

fn name_key(kind: Kind, title: &str) -> String {
    match kind {
        Kind::Movie => clean_movie_title(title),
        Kind::Series => clean_series_title(title),
    }
}

impl PlexIndex {
    pub fn find(&self, t: &Title) -> Option<&PlexItem> {
        let i = t
            .tmdb_id
            .and_then(|id| self.by_tmdb.get(&(t.kind, id)))
            .or_else(|| t.tvdb_id.and_then(|id| self.by_tvdb.get(&(t.kind, id))))
            .or_else(|| t.imdb_id.as_ref().and_then(|id| self.by_imdb.get(id)))
            .or_else(|| self.by_name.get(&(t.kind, name_key(t.kind, &t.title), t.year)))
            // A year can differ by one between sources (festival and general release).
            .or_else(|| [t.year.saturating_sub(1), t.year + 1].iter().find_map(|y| self.by_name.get(&(t.kind, name_key(t.kind, &t.title), *y))))?;
        self.items.get(*i)
    }

    /// A link that opens the item in Plex's web app.
    pub fn url(&self, item: &PlexItem) -> Option<String> {
        if self.machine.is_empty() {
            return None;
        }
        Some(format!("https://app.plex.tv/desktop/#!/server/{}/details?key=%2Flibrary%2Fmetadata%2F{}", self.machine, item.rating_key))
    }
}

#[derive(Serialize)]
pub struct PlexReport {
    pub configured: bool,
    pub synced_at: i64,
    pub plex_items: usize,
    pub matched: usize,
    /// In Plex, not tracked by Spool.
    pub plex_only: Vec<Value>,
    /// Tracked by Spool with files on disk, but not found in Plex.
    pub spool_only: Vec<Value>,
}

impl App {
    async fn plex_get(&self, path: &str) -> Result<Value> {
        let g = self.settings.general();
        if g.plex_url.is_empty() || g.plex_token.is_empty() {
            bail!("Plex is not set up in Settings");
        }
        let url = format!("{}{}", g.plex_url.trim_end_matches('/'), path);
        let r = self
            .http
            .get(&url)
            .header("X-Plex-Token", &g.plex_token)
            .header("Accept", "application/json")
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| anyhow!("Plex did not answer: {}", e.without_url()))?;
        if r.status().as_u16() == 401 {
            bail!("Plex rejected the token");
        }
        if !r.status().is_success() {
            bail!("Plex answered HTTP {}", r.status().as_u16());
        }
        r.json().await.map_err(|e| anyhow!("Plex sent something unreadable: {}", e.without_url()))
    }

    /// Read every movie and show from Plex and replace Spool's copy of that list.
    pub async fn plex_sync(&self) -> Result<String> {
        let identity = self.plex_get("/identity").await?;
        let machine = identity["MediaContainer"]["machineIdentifier"].as_str().unwrap_or("").to_string();
        let sections = self.plex_get("/library/sections").await?;
        let mut items: Vec<PlexItem> = vec![];
        for sec in sections["MediaContainer"]["Directory"].as_array().into_iter().flatten() {
            let kind = match sec["type"].as_str() {
                Some("movie") => Kind::Movie,
                Some("show") => Kind::Series,
                _ => continue,
            };
            let key = sec["key"].as_str().unwrap_or_default();
            let all = self.plex_get(&format!("/library/sections/{key}/all?includeGuids=1")).await?;
            for m in all["MediaContainer"]["Metadata"].as_array().into_iter().flatten() {
                let guid = |prefix: &str| m["Guid"].as_array().into_iter().flatten().filter_map(|g| g["id"].as_str()).find_map(|g| g.strip_prefix(prefix).map(str::to_string));
                let paths: Vec<String> = match kind {
                    Kind::Movie => m["Media"].as_array().into_iter().flatten().flat_map(|me| me["Part"].as_array().into_iter().flatten()).filter_map(|p| p["file"].as_str().map(str::to_string)).collect(),
                    Kind::Series => m["Location"].as_array().into_iter().flatten().filter_map(|l| l["path"].as_str().map(str::to_string)).collect(),
                };
                items.push(PlexItem {
                    rating_key: m["ratingKey"].as_str().unwrap_or_default().to_string(),
                    kind,
                    title: m["title"].as_str().unwrap_or_default().to_string(),
                    year: m["year"].as_u64().unwrap_or(0) as u32,
                    tmdb_id: guid("tmdb://").and_then(|g| g.parse().ok()),
                    tvdb_id: guid("tvdb://").and_then(|g| g.parse().ok()),
                    imdb_id: guid("imdb://"),
                    section: sec["title"].as_str().unwrap_or_default().to_string(),
                    paths,
                    watched: m[if kind == Kind::Movie { "viewCount" } else { "viewedLeafCount" }].as_u64().unwrap_or(0) as u32,
                    items: if kind == Kind::Movie { 1 } else { m["leafCount"].as_u64().unwrap_or(0) as u32 },
                    last_viewed_at: m["lastViewedAt"].as_i64().unwrap_or(0),
                });
            }
        }
        // Series folders are not in the list response; they come one request per show.
        for it in items.iter_mut().filter(|i| i.kind == Kind::Series && i.paths.is_empty()) {
            if let Ok(detail) = self.plex_get(&format!("/library/metadata/{}", it.rating_key)).await {
                it.paths = detail["MediaContainer"]["Metadata"][0]["Location"].as_array().into_iter().flatten().filter_map(|l| l["path"].as_str().map(str::to_string)).collect();
            }
        }
        let count = items.len();
        self.db.tx(|tx| {
            tx.execute("DELETE FROM plex_items", [])?;
            for it in &items {
                tx.execute(
                    "INSERT OR REPLACE INTO plex_items(rating_key, kind, title, year, tmdb_id, tvdb_id, imdb_id, data) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![it.rating_key, it.kind.as_str(), it.title, it.year, it.tmdb_id, it.tvdb_id, it.imdb_id, serde_json::to_string(it)?],
                )?;
            }
            Ok(())
        })?;
        self.db.set_setting("plex_state", &serde_json::json!({"machine": machine, "synced_at": now()}))?;
        self.emit(Event::Title { id: 0 });

        // Plex misses files now and then. Where it has fewer than Spool does, ask it to look at
        // that folder again; the next reading shows whether it caught up.
        let idx = self.plex_index()?;
        let (files, episodes) = (self.db.file_counts()?, self.db.episode_counts()?);
        let mut nudged = 0;
        for t in self.db.titles(None)? {
            let have = match t.kind {
                Kind::Movie => files.get(&t.id).map(|f| f.0.min(1)).unwrap_or(0),
                Kind::Series => episodes.get(&t.id).map(|e| e.1).unwrap_or(0),
            };
            let in_plex = idx.find(&t).map(|i| i.items.max(if t.kind == Kind::Movie { 1 } else { 0 })).unwrap_or(0);
            if have > in_plex && !t.path.is_empty() && nudged < 20 {
                if self.plex_refresh(&t.path).await.is_ok() {
                    nudged += 1;
                }
            }
        }
        Ok(if nudged > 0 { format!("{count} items read from Plex; asked it to rescan {nudged} folders it was behind on") } else { format!("{count} items read from Plex") })
    }

    pub fn plex_index(&self) -> Result<PlexIndex> {
        let state: Value = self.db.get_setting("plex_state");
        let items: Vec<PlexItem> = self.db.with(|c| {
            let mut s = c.prepare("SELECT data FROM plex_items")?;
            let rows = s.query_map([], |r| json::<PlexItem>(r.get::<_, String>(0)?))?;
            rows.collect()
        })?;
        let mut idx = PlexIndex { machine: state["machine"].as_str().unwrap_or("").to_string(), items, by_tmdb: HashMap::new(), by_tvdb: HashMap::new(), by_imdb: HashMap::new(), by_name: HashMap::new() };
        for (i, it) in idx.items.iter().enumerate() {
            if let Some(id) = it.tmdb_id {
                idx.by_tmdb.entry((it.kind, id)).or_insert(i);
            }
            if let Some(id) = it.tvdb_id {
                idx.by_tvdb.entry((it.kind, id)).or_insert(i);
            }
            if let Some(id) = &it.imdb_id {
                idx.by_imdb.entry(id.clone()).or_insert(i);
            }
            idx.by_name.entry((it.kind, name_key(it.kind, &it.title), it.year)).or_insert(i);
        }
        Ok(idx)
    }

    /// Compare the two libraries.
    pub fn plex_report(&self) -> Result<PlexReport> {
        let g = self.settings.general();
        let state: Value = self.db.get_setting("plex_state");
        let idx = self.plex_index()?;
        let titles = self.db.titles(None)?;
        let files = self.db.file_counts()?;
        let episodes = self.db.episode_counts()?;
        let mut used: Vec<bool> = vec![false; idx.items.len()];
        let mut matched = 0;
        let mut spool_only = vec![];
        for t in &titles {
            let has_files = files.get(&t.id).is_some_and(|f| f.0 > 0) || episodes.get(&t.id).is_some_and(|e| e.1 > 0);
            match idx.find(t) {
                Some(item) => {
                    matched += 1;
                    if let Some(i) = idx.items.iter().position(|x| x.rating_key == item.rating_key) {
                        used[i] = true;
                    }
                }
                None if has_files => spool_only.push(serde_json::json!({"title_id": t.id, "title": t.title, "year": t.year, "kind": t.kind, "path": t.path})),
                None => {}
            }
        }
        let mut plex_only: Vec<Value> = idx
            .items
            .iter()
            .enumerate()
            .filter(|(i, _)| !used[*i])
            .map(|(_, it)| {
                serde_json::json!({"rating_key": it.rating_key, "title": it.title, "year": it.year, "kind": it.kind, "section": it.section, "path": it.paths.first(),
                    "can_track": it.tmdb_id.is_some() && it.kind == Kind::Movie || it.tvdb_id.is_some() && it.kind == Kind::Series, "url": idx.url(it)})
            })
            .collect();
        plex_only.sort_by(|a, b| a["title"].as_str().cmp(&b["title"].as_str()));
        Ok(PlexReport { configured: !g.plex_url.is_empty() && !g.plex_token.is_empty(), synced_at: state["synced_at"].as_i64().unwrap_or(0), plex_items: idx.items.len(), matched, plex_only, spool_only })
    }

    /// Start tracking something Plex already has. The title is added unmonitored, pointed at the
    /// folder Plex plays it from, and its files are read from disk. Nothing is moved or downloaded.
    pub async fn plex_track(&self, rating_key: &str) -> Result<Title> {
        let idx = self.plex_index()?;
        let item = idx.items.iter().find(|i| i.rating_key == rating_key).ok_or_else(|| anyhow!("that item is no longer in Spool's copy of the Plex library; sync and try again"))?.clone();
        let g = self.settings.general();
        let (mut title, episodes) = match item.kind {
            Kind::Movie => (self.meta.movie(&g.tmdb_api_key, item.tmdb_id.ok_or_else(|| anyhow!("Plex has no TMDB id for this item"))?).await?, vec![]),
            Kind::Series => self.meta.series_by_tvdb(item.tvdb_id.ok_or_else(|| anyhow!("Plex has no TVDB id for this item"))?).await?,
        };
        if title.kind == Kind::Series && title.tvdb_id.is_none() {
            title.tvdb_id = item.tvdb_id;
        }
        if self.db.find_title(item.kind, title.tmdb_id, title.tvdb_id)?.is_some() {
            bail!("{} is already in Spool", title.title);
        }
        let first = item.paths.first().ok_or_else(|| anyhow!("Plex did not say where the files are"))?;
        title.path = match item.kind {
            Kind::Movie => std::path::Path::new(first).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
            Kind::Series => first.clone(),
        };
        title.monitored = false;
        title.profile_id = if item.kind == Kind::Movie { g.default_movie_profile } else { g.default_series_profile };
        title.added_at = now();
        title.last_refresh_at = now();
        self.db.save_title(&mut title)?;
        for mut e in episodes {
            e.title_id = title.id;
            e.monitored = false;
            self.db.upsert_episode(&e)?;
        }
        let mut report = crate::catalog::ScanReport::default();
        let _ = self.scan_title(title.id, &mut report).await;
        self.emit(Event::Title { id: title.id });
        Ok(title)
    }
}
