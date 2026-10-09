//! Shadow comparison: does Spool read releases and reach decisions the way Radarr and Sonarr do?
//!
//! Uses their grab history as ground truth. For every release they grabbed, Spool parses the same
//! name and judges it against its own copy of the library. Read-only on both sides.

use crate::app::App;
use crate::models::*;
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use spool_core::matching;
use spool_core::parser::{parse_episode_title, parse_movie_title};
use spool_core::Quality;

#[derive(Debug, Default, Serialize)]
pub struct ShadowReport {
    pub movies: Section,
    pub tv: Section,
    /// Releases Spool would have grabbed from the feeds while shadowing.
    pub would_grab: Vec<Value>,
}

#[derive(Debug, Default, Serialize)]
pub struct Section {
    /// Grabs in the other app's history that were examined.
    pub examined: usize,
    /// Spool mapped the release to the same title (and episodes).
    pub same_title: usize,
    /// Spool read the same quality from the name.
    pub same_quality: usize,
    /// Mapped to the same title only because the indexer supplied its id, as the other app did.
    pub matched_by_id: usize,
    /// Grabs the other app made on its own (feed or automatic search), where profiles apply.
    pub automatic: usize,
    /// Of those, how many Spool's profile also allows.
    pub quality_allowed: usize,
    /// Grabs picked by hand in the other app; profile rules were overridden, so they are not compared.
    pub manual: usize,
    pub disagreements: Vec<Disagreement>,
}

#[derive(Debug, Serialize)]
pub struct Disagreement {
    pub release: String,
    pub kind: String,
    pub theirs: String,
    pub ours: String,
}

impl App {
    async fn arr_get(&self, base: &(String, String), path: &str) -> Result<Value> {
        let url = format!("{}/api/v3/{path}", base.0.trim_end_matches('/'));
        Ok(self.http.get(&url).header("X-Api-Key", &base.1).timeout(std::time::Duration::from_secs(60)).send().await.map_err(|e| anyhow::anyhow!("{}", e.without_url()))?.error_for_status().map_err(|e| anyhow::anyhow!("{}", e.without_url()))?.json().await?)
    }

    pub async fn shadow_report(&self, radarr: Option<(String, String)>, sonarr: Option<(String, String)>) -> Result<ShadowReport> {
        let mut report = ShadowReport::default();
        let profiles = self.db.profiles()?;

        if let Some(r) = &radarr {
            let movies = self.db.titles(Some(Kind::Movie))?;
            let keys: Vec<matching::TitleKey> = movies.iter().map(|m| m.key()).collect();
            let theirs = self.arr_get(r, "movie").await?;
            let tmdb_of = |id: i64| theirs.as_array().into_iter().flatten().find(|m| m["id"].as_i64() == Some(id)).and_then(|m| m["tmdbId"].as_u64()).map(|t| t as u32);
            let hist = self.arr_get(r, "history?page=1&pageSize=1000&eventType=1&sortKey=date&sortDirection=descending").await?;
            for h in hist["records"].as_array().into_iter().flatten() {
                let name = h["sourceTitle"].as_str().unwrap_or("").to_string();
                let their_quality = h["quality"]["quality"]["name"].as_str().unwrap_or("").to_string();
                let their_tmdb = tmdb_of(h["movieId"].as_i64().unwrap_or(0));
                report.movies.examined += 1;
                let Some(p) = parse_movie_title(&name, false) else {
                    report.movies.disagreements.push(Disagreement { release: name, kind: "unparseable".into(), theirs: their_quality, ours: "could not parse".into() });
                    continue;
                };
                let ours = matching::match_movie(&p, &keys).and_then(|k| movies.iter().find(|m| m.id == k.id));
                match (ours, their_tmdb) {
                    (Some(m), Some(t)) if m.tmdb_id == Some(t) => report.movies.same_title += 1,
                    (None, Some(tm)) if h["data"]["movieMatchType"] == "Id" && movies.iter().any(|m| m.tmdb_id == Some(tm) && (p.year == 0 || p.year.abs_diff(m.year) <= 1)) => report.movies.matched_by_id += 1,
                    (ours, t) => report.movies.disagreements.push(Disagreement {
                        release: name.clone(),
                        kind: "title".into(),
                        theirs: format!("tmdb {}", t.map(|t| t.to_string()).unwrap_or_else(|| "?".into())),
                        ours: ours.map(|m| format!("{} ({})", m.title, m.year)).unwrap_or_else(|| format!("no match for \"{}\" ({})", p.title(), p.year)),
                    }),
                }
                let our_quality = p.quality.quality.movie_name();
                if Quality::from_name(&their_quality) == Some(p.quality.quality) {
                    report.movies.same_quality += 1;
                } else {
                    report.movies.disagreements.push(Disagreement { release: name.clone(), kind: "quality".into(), theirs: their_quality.clone(), ours: our_quality.into() });
                }
                let automatic = matches!(h["data"]["releaseSource"].as_str(), Some("Rss") | Some("Search"));
                let ours = ours.or_else(|| their_tmdb.and_then(|t| movies.iter().find(|m| m.tmdb_id == Some(t))));
                if !automatic {
                    report.movies.manual += 1;
                } else if let Some(m) = ours {
                    report.movies.automatic += 1;
                    if profiles.iter().find(|pr| pr.id == m.profile_id).is_some_and(|pr| pr.allows(p.quality.quality)) {
                        report.movies.quality_allowed += 1;
                    } else {
                        report.movies.disagreements.push(Disagreement { release: name, kind: "profile".into(), theirs: "grabbed".into(), ours: format!("{our_quality} is not allowed by this movie's profile") });
                    }
                }
            }
        }

        if let Some(s) = &sonarr {
            let series = self.db.titles(Some(Kind::Series))?;
            let keys: Vec<matching::TitleKey> = series.iter().map(|m| m.key()).collect();
            let theirs = self.arr_get(s, "series").await?;
            let tvdb_of = |id: i64| theirs.as_array().into_iter().flatten().find(|m| m["id"].as_i64() == Some(id)).and_then(|m| m["tvdbId"].as_u64()).map(|t| t as u32);
            let hist = self.arr_get(s, "history?page=1&pageSize=1000&eventType=1&sortKey=date&sortDirection=descending&includeEpisode=true").await?;
            for h in hist["records"].as_array().into_iter().flatten() {
                let name = h["sourceTitle"].as_str().unwrap_or("").to_string();
                let their_quality = h["quality"]["quality"]["name"].as_str().unwrap_or("").to_string();
                let their_tvdb = tvdb_of(h["seriesId"].as_i64().unwrap_or(0));
                let (their_season, their_ep) = (h["episode"]["seasonNumber"].as_u64().unwrap_or(0) as u32, h["episode"]["episodeNumber"].as_u64().unwrap_or(0) as u32);
                report.tv.examined += 1;
                let Some(p) = parse_episode_title(&name) else {
                    report.tv.disagreements.push(Disagreement { release: name, kind: "unparseable".into(), theirs: their_quality, ours: "could not parse".into() });
                    continue;
                };
                let ours = matching::match_series(&p, &keys).and_then(|k| series.iter().find(|m| m.id == k.id));
                let covers = p.season_number() == their_season && (p.full_season || p.episode_numbers.contains(&their_ep));
                match (ours, their_tvdb) {
                    (Some(m), Some(t)) if m.tvdb_id == Some(t) && covers => report.tv.same_title += 1,
                    (None, Some(tv)) if covers && h["data"]["seriesMatchType"] == "Id" && series.iter().any(|m| m.tvdb_id == Some(tv)) => report.tv.matched_by_id += 1,
                    (ours, _) => report.tv.disagreements.push(Disagreement {
                        release: name.clone(),
                        kind: "title".into(),
                        theirs: format!("S{their_season:02}E{their_ep:02}"),
                        ours: format!("{} S{:02} {:?}", ours.map(|m| m.title.as_str()).unwrap_or("no series match"), p.season_number(), p.episode_numbers),
                    }),
                }
                let q = p.quality.map(|q| q.quality).unwrap_or(Quality::Unknown);
                if Quality::from_name(&their_quality) == Some(q) {
                    report.tv.same_quality += 1;
                } else {
                    report.tv.disagreements.push(Disagreement { release: name.clone(), kind: "quality".into(), theirs: their_quality.clone(), ours: q.tv_name().into() });
                }
                let automatic = matches!(h["data"]["releaseSource"].as_str(), Some("Rss") | Some("Search"));
                let ours = ours.or_else(|| their_tvdb.and_then(|t| series.iter().find(|m| m.tvdb_id == Some(t))));
                if !automatic {
                    report.tv.manual += 1;
                } else if let Some(m) = ours {
                    report.tv.automatic += 1;
                    if profiles.iter().find(|pr| pr.id == m.profile_id).is_some_and(|pr| pr.allows(q)) {
                        report.tv.quality_allowed += 1;
                    } else {
                        report.tv.disagreements.push(Disagreement { release: name, kind: "profile".into(), theirs: "grabbed".into(), ours: format!("{} is not allowed by this series' profile", q.tv_name()) });
                    }
                }
            }
        }

        report.would_grab = self.db.history(None, 2000)?.into_iter().filter(|h| h.kind == "would_grab").map(|h| serde_json::json!({"ts": h.ts, "title_id": h.title_id, "release": h.data["release"], "covers": h.data["covers"], "indexer": h.data["indexer"]})).collect();
        Ok(report)
    }
}
