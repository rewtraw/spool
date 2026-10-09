//! One-way import of settings and catalog from Radarr, Sonarr and SABnzbd.
//!
//! The catalog comes from the Radarr and Sonarr APIs. Those APIs mask secrets, so indexer keys and
//! the Plex token are read from copies of their databases, and usenet servers from `sabnzbd.ini`.
//! Running it twice updates what is there instead of duplicating it. Nothing on the media volume
//! is touched.

use crate::app::App;
use crate::db::now;
use crate::models::*;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use spool_core::decision::ProperPolicy;
use spool_core::mediainfo::MediaInfo;
use spool_core::naming::ColonReplacement;
use spool_core::profile::{ProfileItem, QualityProfile, SizeLimit};
use spool_core::{Quality, QualityModel, Revision};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub radarr: Option<(String, String)>,
    pub sonarr: Option<(String, String)>,
    pub radarr_db: Option<PathBuf>,
    pub sonarr_db: Option<PathBuf>,
    pub sab_ini: Option<PathBuf>,
    /// Container path prefix to host path prefix, e.g. ("/data", "/Volumes/Media").
    pub path_map: Vec<(String, String)>,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Report {
    pub profiles: usize,
    pub indexers: usize,
    pub servers: usize,
    pub movies: usize,
    pub series: usize,
    pub episodes: usize,
    pub files: usize,
    /// Settings Spool has no equivalent for. Each is something to review before cutover.
    pub unsupported: Vec<String>,
    pub notes: Vec<String>,
}

fn st(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn opt(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}
fn num(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0)
}
fn date(v: &Value, k: &str) -> Option<String> {
    opt(v, k).and_then(|d| d.get(..10).map(str::to_string))
}

fn snake(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_uppercase() {
            out.push('_');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn quality_model(v: &Value) -> QualityModel {
    let name = v["quality"]["name"].as_str().unwrap_or("Unknown");
    QualityModel {
        quality: Quality::from_name(name).unwrap_or(Quality::Unknown),
        revision: Revision {
            version: v["revision"]["version"].as_u64().unwrap_or(1) as u32,
            real: v["revision"]["real"].as_u64().unwrap_or(0) as u32,
            is_repack: v["revision"]["isRepack"].as_bool().unwrap_or(false),
        },
    }
}

fn media_info(v: &Value) -> Option<MediaInfo> {
    if !v.is_object() {
        return None;
    }
    let runtime = st(v, "runTime").split(':').fold(0.0, |acc, part| acc * 60.0 + part.parse::<f64>().unwrap_or(0.0));
    let (w, h) = st(v, "resolution").split_once('x').map(|(a, b)| (a.parse().unwrap_or(0), b.parse().unwrap_or(0))).unwrap_or((0, 0));
    let list = |k: &str| st(v, k).split('/').filter(|s| !s.is_empty()).map(str::to_string).collect::<Vec<_>>();
    Some(MediaInfo {
        video_codec: st(v, "videoCodec"),
        audio_codec: st(v, "audioCodec"),
        audio_channels: v.get("audioChannels").and_then(|x| x.as_f64()).unwrap_or(0.0),
        dynamic_range_type: st(v, "videoDynamicRangeType"),
        bit_depth: num(v, "videoBitDepth") as u32,
        width: w,
        height: h,
        runtime_secs: runtime,
        audio_languages: list("audioLanguages"),
        subtitles: list("subtitles"),
        is_3d: false,
    })
}

fn image(v: &Value, kind: &str) -> Option<String> {
    v["images"].as_array()?.iter().find(|i| i["coverType"] == kind).and_then(|i| opt(i, "remoteUrl").or_else(|| opt(i, "url"))).filter(|u| u.starts_with("http"))
}

/// Convert an *arr quality profile. Returns None when it lists no quality Spool knows.
fn profile(v: &Value, kind: &str, report: &mut Report) -> Option<QualityProfile> {
    let name = st(v, "name");
    let cutoff_id = v["cutoff"].as_i64().unwrap_or(-1);
    let mut items = vec![];
    let mut cutoff = None;
    for it in v["items"].as_array().into_iter().flatten() {
        let children = it["items"].as_array().filter(|a| !a.is_empty());
        let (names, group_name, id): (Vec<String>, Option<String>, i64) = match children {
            Some(ch) => (ch.iter().map(|c| st(&c["quality"], "name")).collect(), opt(it, "name"), it["id"].as_i64().unwrap_or(-1)),
            None => (vec![st(&it["quality"], "name")], None, it["quality"]["id"].as_i64().unwrap_or(-1)),
        };
        let qualities: Vec<Quality> = names.iter().filter_map(|n| Quality::from_name(n)).collect();
        if qualities.is_empty() {
            if it["allowed"].as_bool().unwrap_or(false) {
                report.unsupported.push(format!("{kind} profile \"{name}\" allows {}, which Spool does not know", names.join(", ")));
            }
            continue;
        }
        if id == cutoff_id {
            cutoff = Some(qualities[0]);
        }
        items.push(ProfileItem { name: group_name, qualities, allowed: it["allowed"].as_bool().unwrap_or(false) });
    }
    if items.is_empty() {
        return None;
    }
    if v["formatItems"].as_array().is_some_and(|f| f.iter().any(|x| x["score"].as_i64().unwrap_or(0) != 0)) || v["minFormatScore"].as_i64().unwrap_or(0) != 0 {
        report.unsupported.push(format!("{kind} profile \"{name}\" uses custom format scores"));
    }
    let lang = st(&v["language"], "name");
    if !lang.is_empty() && !matches!(lang.as_str(), "English" | "Original" | "Any") {
        report.unsupported.push(format!("{kind} profile \"{name}\" wants {lang}; Spool has one language setting for everything"));
    }
    let cutoff = cutoff.or_else(|| items.iter().rev().find(|i| i.allowed).map(|i| i.qualities[0])).unwrap_or(items[items.len() - 1].qualities[0]);
    Some(QualityProfile { id: 0, name, kind: kind.into(), upgrade_allowed: v["upgradeAllowed"].as_bool().unwrap_or(false), cutoff, items, target_size_gb: None, prefer_efficient_codec: false, prefer_direct_play: false })
}

/// Read `Name -> Settings JSON` from an *arr database table.
fn arr_settings(db: &PathBuf, table: &str) -> Result<HashMap<String, Value>> {
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).with_context(|| format!("opening {}", db.display()))?;
    let mut s = conn.prepare(&format!("SELECT Name, Settings FROM {table}"))?;
    let rows = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows.flatten() {
        if let Ok(v) = serde_json::from_str(&row.1) {
            out.insert(row.0, v);
        }
    }
    Ok(out)
}

/// Usenet servers from sabnzbd.ini.
pub fn parse_sab_servers(ini: &str) -> Vec<spool_nntp::ServerConfig> {
    let mut out = vec![];
    let mut in_servers = false;
    let mut cur: Option<HashMap<String, String>> = None;
    let flush = |cur: &mut Option<HashMap<String, String>>, out: &mut Vec<spool_nntp::ServerConfig>| {
        if let Some(m) = cur.take() {
            let get = |k: &str| m.get(k).cloned().unwrap_or_default();
            let host = get("host");
            if host.is_empty() {
                return;
            }
            out.push(spool_nntp::ServerConfig {
                id: host.clone(),
                name: if get("displayname").is_empty() { host.clone() } else { get("displayname") },
                host,
                port: get("port").parse().unwrap_or(563),
                tls: get("ssl") != "0",
                tls_verify: get("ssl_verify") != "0",
                username: get("username"),
                password: get("password"),
                connections: get("connections").parse().unwrap_or(8),
                priority: get("priority").parse().unwrap_or(0),
                enabled: get("enable") != "0",
                pipeline: 2,
            });
        }
    };
    for line in ini.lines() {
        let t = line.trim();
        if t.starts_with("[[") && t.ends_with("]]") {
            flush(&mut cur, &mut out);
            if in_servers {
                cur = Some(HashMap::new());
            }
        } else if t.starts_with('[') && t.ends_with(']') {
            flush(&mut cur, &mut out);
            in_servers = t == "[servers]";
        } else if let (Some(m), Some((k, v))) = (cur.as_mut(), t.split_once('=')) {
            m.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    flush(&mut cur, &mut out);
    out
}

impl App {
    async fn arr(&self, base: &(String, String), path: &str) -> Result<Value> {
        let url = format!("{}/api/v3/{path}", base.0.trim_end_matches('/'));
        let r = self.http.get(&url).header("X-Api-Key", &base.1).timeout(std::time::Duration::from_secs(120)).send().await.map_err(|e| anyhow!("{}", e.without_url()))?;
        if !r.status().is_success() {
            return Err(anyhow!("{} answered HTTP {} for {path}", base.0, r.status().as_u16()));
        }
        r.json().await.map_err(|e| anyhow!("{}", e.without_url()))
    }

    fn save_profile_by_name(&self, mut p: QualityProfile) -> Result<i64> {
        if let Some(existing) = self.db.profiles()?.into_iter().find(|e| e.kind == p.kind && e.name == p.name) {
            p.id = existing.id;
        }
        self.db.save_profile(&mut p)
    }

    fn merge_indexer(&self, v: &Value, secrets: &HashMap<String, Value>, tv: bool, report: &mut Report) -> Result<()> {
        if st(v, "protocol") != "usenet" {
            report.unsupported.push(format!("indexer \"{}\" is not a usenet indexer", st(v, "name")));
            return Ok(());
        }
        let field = |name: &str| v["fields"].as_array().into_iter().flatten().find(|f| f["name"] == name).map(|f| f["value"].clone()).unwrap_or(Value::Null);
        let name = st(v, "name");
        let url = format!("{}{}", field("baseUrl").as_str().unwrap_or("").trim_end_matches('/'), field("apiPath").as_str().unwrap_or("/api"));
        let key = secrets.get(&name).map(|s| st(s, "apiKey")).unwrap_or_default();
        let cats: Vec<u32> = field("categories").as_array().map(|a| a.iter().filter_map(|c| c.as_u64().map(|c| c as u32)).collect()).unwrap_or_default();
        let host = |u: &str| u.split("//").nth(1).unwrap_or(u).split('/').next().unwrap_or("").to_lowercase();
        let mut ix = self.db.indexers()?.into_iter().find(|i| host(&i.url) == host(&url)).unwrap_or_else(|| Indexer {
            id: 0,
            name: name.clone(),
            url: url.clone(),
            api_key: String::new(),
            movie_categories: vec![2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060],
            tv_categories: vec![5030, 5040],
            enable_rss: true,
            enable_search: true,
            priority: 25,
            movies: false,
            tv: false,
            daily_requests: 0,
            daily_grabs: 0,
        });
        ix.url = url;
        if !key.is_empty() {
            ix.api_key = key;
        }
        ix.enable_rss = v["enableRss"].as_bool().unwrap_or(true);
        ix.enable_search = v["enableAutomaticSearch"].as_bool().unwrap_or(true);
        ix.priority = v["priority"].as_i64().unwrap_or(25) as i32;
        if tv {
            ix.tv = true;
            if !cats.is_empty() {
                ix.tv_categories = cats;
            }
        } else {
            ix.movies = true;
            if !cats.is_empty() {
                ix.movie_categories = cats;
            }
        }
        if ix.api_key.is_empty() {
            report.notes.push(format!("indexer \"{name}\" was imported without its API key; add it in Settings"));
        }
        self.db.save_indexer(&mut ix)?;
        report.indexers = self.db.indexers()?.len();
        Ok(())
    }

    pub async fn migrate(&self, o: &Options) -> Result<Report> {
        let mut report = Report::default();
        let map_path = |p: &str| -> String {
            for (from, to) in &o.path_map {
                if let Some(rest) = p.strip_prefix(from.as_str()) {
                    return format!("{}{}", to.trim_end_matches('/'), rest);
                }
            }
            p.to_string()
        };
        let mut general = self.settings.general();
        let mut naming = self.settings.naming();
        let mut limits = self.settings.size_limits();
        let colon = |v: &Value| match v {
            Value::String(s) => match s.as_str() {
                "delete" => ColonReplacement::Delete,
                "dash" => ColonReplacement::Dash,
                "spaceDash" => ColonReplacement::SpaceDash,
                "spaceDashSpace" => ColonReplacement::SpaceDashSpace,
                _ => ColonReplacement::Smart,
            },
            Value::Number(n) => match n.as_i64().unwrap_or(4) {
                0 => ColonReplacement::Delete,
                1 => ColonReplacement::Dash,
                2 => ColonReplacement::SpaceDash,
                3 => ColonReplacement::SpaceDashSpace,
                _ => ColonReplacement::Smart,
            },
            _ => ColonReplacement::Smart,
        };

        // ------------------------------------------------------------ Radarr
        if let Some(radarr) = &o.radarr {
            let secrets = o.radarr_db.as_ref().map(|d| arr_settings(d, "Indexers")).transpose()?.unwrap_or_default();
            let mut profile_map: HashMap<i64, i64> = HashMap::new();
            for p in self.arr(radarr, "qualityprofile").await?.as_array().into_iter().flatten() {
                if let Some(conv) = profile(p, "movie", &mut report) {
                    profile_map.insert(p["id"].as_i64().unwrap_or(0), self.save_profile_by_name(conv)?);
                    report.profiles += 1;
                }
            }
            for d in self.arr(radarr, "qualitydefinition").await?.as_array().into_iter().flatten() {
                if let Some(q) = Quality::from_name(&st(&d["quality"], "name")) {
                    limits.insert(format!("movie:{}", q.key()), SizeLimit { min: d["minSize"].as_f64().unwrap_or(0.0), max: d["maxSize"].as_f64(), preferred: d["preferredSize"].as_f64() });
                }
            }
            let n = self.arr(radarr, "config/naming").await?;
            naming.rename = n["renameMovies"].as_bool().unwrap_or(true);
            naming.replace_illegal_characters = n["replaceIllegalCharacters"].as_bool().unwrap_or(true);
            naming.movie_colon_replacement = colon(&n["colonReplacementFormat"]);
            if let Some(f) = opt(&n, "standardMovieFormat") {
                naming.movie_file_format = f;
            }
            if let Some(f) = opt(&n, "movieFolderFormat") {
                naming.movie_folder_format = f;
            }
            let mm = self.arr(radarr, "config/mediamanagement").await?;
            general.proper_policy = match st(&mm, "downloadPropersAndRepacks").as_str() {
                "doNotUpgrade" => ProperPolicy::DoNotUpgrade,
                "doNotPrefer" => ProperPolicy::DoNotPrefer,
                _ => ProperPolicy::PreferAndUpgrade,
            };
            if mm["importExtraFiles"].as_bool().unwrap_or(false) {
                report.unsupported.push("Radarr imports extra files (subtitles); Spool imports video only".into());
            }
            if let Ok(ic) = self.arr(radarr, "config/indexer").await {
                general.retention_days = num(&ic, "retention") as u32;
            }
            if let Some(root) = self.arr(radarr, "rootfolder").await?.as_array().and_then(|a| a.first()).map(|r| st(r, "path")) {
                general.movie_root = map_path(&root);
            }
            for ix in self.arr(radarr, "indexer").await?.as_array().into_iter().flatten() {
                self.merge_indexer(ix, &secrets, false, &mut report)?;
            }
            for n in self.arr(radarr, "notification").await?.as_array().into_iter().flatten() {
                if st(n, "implementation") == "PlexServer" {
                    let field = |name: &str| n["fields"].as_array().into_iter().flatten().find(|f| f["name"] == name).map(|f| f["value"].clone()).unwrap_or(Value::Null);
                    let mut host = field("host").as_str().unwrap_or("127.0.0.1").to_string();
                    if host == "host.docker.internal" {
                        host = "127.0.0.1".into();
                    }
                    let scheme = if field("useSsl").as_bool().unwrap_or(false) { "https" } else { "http" };
                    general.plex_url = format!("{scheme}://{host}:{}", field("port").as_u64().unwrap_or(32400));
                    if let Some(db) = &o.radarr_db {
                        if let Some(token) = arr_settings(db, "Notifications")?.get(&st(n, "name")).map(|s| st(s, "authToken")).filter(|t| !t.is_empty()) {
                            general.plex_token = token;
                        }
                    }
                    if general.plex_token.is_empty() {
                        report.notes.push("the Plex connection was imported without its token; add it in Settings".into());
                    }
                } else {
                    report.unsupported.push(format!("Radarr notification \"{}\" ({})", st(n, "name"), st(n, "implementation")));
                }
            }
            if self.arr(radarr, "customformat").await.ok().and_then(|v| v.as_array().map(|a| a.len())).unwrap_or(0) > 0 {
                report.unsupported.push("Radarr has custom formats".into());
            }
            if self.arr(radarr, "importlist").await.ok().and_then(|v| v.as_array().map(|a| a.len())).unwrap_or(0) > 0 {
                report.unsupported.push("Radarr has import lists".into());
            }

            for m in self.arr(radarr, "movie").await?.as_array().into_iter().flatten() {
                let tmdb = num(m, "tmdbId") as u32;
                let existing = self.db.find_title(Kind::Movie, Some(tmdb), None)?;
                let mut t = Title {
                    id: existing.as_ref().map(|e| e.id).unwrap_or(0),
                    kind: Kind::Movie,
                    title: st(m, "title"),
                    sort_title: String::new(),
                    year: num(m, "year") as u32,
                    overview: st(m, "overview"),
                    status: snake(&st(m, "status")),
                    runtime: num(m, "runtime") as u32,
                    tmdb_id: (tmdb > 0).then_some(tmdb),
                    tvdb_id: None,
                    tvmaze_id: None,
                    imdb_id: opt(m, "imdbId"),
                    poster: image(m, "poster"),
                    fanart: image(m, "fanart"),
                    genres: m["genres"].as_array().map(|g| g.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
                    alt_titles: {
                        let mut alts: Vec<String> = m["alternateTitles"].as_array().map(|a| a.iter().map(|x| st(x, "title")).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
                        if let Some(o) = opt(m, "originalTitle") {
                            alts.push(o);
                        }
                        alts.dedup();
                        alts
                    },
                    alt_years: [num(m, "secondaryYear") as u32].into_iter().filter(|y| *y > 0).collect(),
                    monitored: m["monitored"].as_bool().unwrap_or(false),
                    profile_id: profile_map.get(&m["qualityProfileId"].as_i64().unwrap_or(0)).copied().unwrap_or(0),
                    path: map_path(&st(m, "path")),
                    added_at: existing.as_ref().map(|e| e.added_at).unwrap_or_else(|| chrono::DateTime::parse_from_rfc3339(&st(m, "added")).map(|d| d.timestamp()).unwrap_or_else(|_| now())),
                    in_cinemas: date(m, "inCinemas"),
                    digital_release: date(m, "digitalRelease"),
                    physical_release: date(m, "physicalRelease"),
                    minimum_availability: snake(&st(m, "minimumAvailability")),
                    studio: opt(m, "studio"),
                    network: None,
                    seasons: vec![],
                    season_folder: true,
                    original_language: None,
                    first_aired: None,
                    last_search_at: existing.as_ref().map(|e| e.last_search_at).unwrap_or(0),
                    last_refresh_at: now(),
                };
                self.db.save_title(&mut t)?;
                report.movies += 1;
                if let Some(f) = m.get("movieFile").filter(|f| f.is_object()) {
                    let rel = st(f, "relativePath");
                    let file = MediaFile {
                        id: 0,
                        title_id: t.id,
                        rel_path: rel.clone(),
                        size: num(f, "size"),
                        quality: quality_model(&f["quality"]),
                        release_group: opt(f, "releaseGroup"),
                        edition: st(f, "edition"),
                        media_info: media_info(&f["mediaInfo"]),
                        scene_name: opt(f, "sceneName"),
                        languages: f["languages"].as_array().map(|l| l.iter().map(|x| st(x, "name").to_lowercase()).collect()).unwrap_or_default(),
                        added_at: chrono::DateTime::parse_from_rfc3339(&st(f, "dateAdded")).map(|d| d.timestamp()).unwrap_or_else(|_| now()),
                    };
                    self.db.tx(|tx| {
                        tx.execute("DELETE FROM files WHERE title_id = ?1", [t.id])?;
                        tx.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1,?2,?3,?4)", rusqlite::params![t.id, rel, file.size as i64, serde_json::to_string(&file)?])?;
                        Ok(())
                    })?;
                    report.files += 1;
                }
            }
        }

        // ------------------------------------------------------------ Sonarr
        if let Some(sonarr) = &o.sonarr {
            let secrets = o.sonarr_db.as_ref().map(|d| arr_settings(d, "Indexers")).transpose()?.unwrap_or_default();
            let mut profile_map: HashMap<i64, i64> = HashMap::new();
            for p in self.arr(sonarr, "qualityprofile").await?.as_array().into_iter().flatten() {
                if let Some(conv) = profile(p, "tv", &mut report) {
                    profile_map.insert(p["id"].as_i64().unwrap_or(0), self.save_profile_by_name(conv)?);
                    report.profiles += 1;
                }
            }
            for d in self.arr(sonarr, "qualitydefinition").await?.as_array().into_iter().flatten() {
                if let Some(q) = Quality::from_name(&st(&d["quality"], "name")) {
                    limits.insert(format!("tv:{}", q.key()), SizeLimit { min: d["minSize"].as_f64().unwrap_or(0.0), max: d["maxSize"].as_f64(), preferred: d["preferredSize"].as_f64() });
                }
            }
            let n = self.arr(sonarr, "config/naming").await?;
            naming.series_colon_replacement = colon(&n["colonReplacementFormat"]);
            for (key, target) in [("standardEpisodeFormat", &mut naming.episode_file_format), ("seriesFolderFormat", &mut naming.series_folder_format), ("seasonFolderFormat", &mut naming.season_folder_format), ("specialsFolderFormat", &mut naming.specials_folder_format)] {
                if let Some(f) = opt(&n, key) {
                    *target = f;
                }
            }
            if n["multiEpisodeStyle"].as_i64().unwrap_or(0) != 0 {
                report.unsupported.push("Sonarr uses a multi-episode naming style other than \"Extend\"".into());
            }
            if let Some(root) = self.arr(sonarr, "rootfolder").await?.as_array().and_then(|a| a.first()).map(|r| st(r, "path")) {
                general.series_root = map_path(&root);
            }
            for ix in self.arr(sonarr, "indexer").await?.as_array().into_iter().flatten() {
                self.merge_indexer(ix, &secrets, true, &mut report)?;
            }
            if general.plex_token.is_empty() {
                if let Some(db) = &o.sonarr_db {
                    if let Some(token) = arr_settings(db, "Notifications")?.values().map(|s| st(s, "authToken")).find(|t| !t.is_empty()) {
                        general.plex_token = token;
                        report.notes.retain(|n| !n.contains("Plex connection"));
                    }
                }
            }
            if self.arr(sonarr, "releaseprofile").await.ok().and_then(|v| v.as_array().map(|a| a.len())).unwrap_or(0) > 0 {
                report.unsupported.push("Sonarr has release profiles".into());
            }
            if self.arr(sonarr, "customformat").await.ok().and_then(|v| v.as_array().map(|a| a.len())).unwrap_or(0) > 0 {
                report.unsupported.push("Sonarr has custom formats".into());
            }

            for sr in self.arr(sonarr, "series").await?.as_array().into_iter().flatten() {
                let tvdb = num(sr, "tvdbId") as u32;
                let existing = self.db.find_title(Kind::Series, None, Some(tvdb))?;
                if st(sr, "seriesType") != "standard" {
                    report.unsupported.push(format!("series \"{}\" is type {}; Spool treats every series as standard", st(sr, "title"), st(sr, "seriesType")));
                }
                let mut t = Title {
                    id: existing.as_ref().map(|e| e.id).unwrap_or(0),
                    kind: Kind::Series,
                    title: st(sr, "title"),
                    sort_title: String::new(),
                    year: num(sr, "year") as u32,
                    overview: st(sr, "overview"),
                    status: st(sr, "status"),
                    runtime: num(sr, "runtime") as u32,
                    tmdb_id: None,
                    tvdb_id: (tvdb > 0).then_some(tvdb),
                    tvmaze_id: Some(num(sr, "tvMazeId") as u32).filter(|i| *i > 0),
                    imdb_id: opt(sr, "imdbId"),
                    poster: image(sr, "poster"),
                    fanart: image(sr, "fanart"),
                    genres: sr["genres"].as_array().map(|g| g.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
                    alt_titles: sr["alternateTitles"].as_array().map(|a| a.iter().map(|x| st(x, "title")).filter(|s| !s.is_empty()).collect()).unwrap_or_default(),
                    alt_years: vec![],
                    monitored: sr["monitored"].as_bool().unwrap_or(false),
                    profile_id: profile_map.get(&sr["qualityProfileId"].as_i64().unwrap_or(0)).copied().unwrap_or(0),
                    path: map_path(&st(sr, "path")),
                    added_at: existing.as_ref().map(|e| e.added_at).unwrap_or_else(|| chrono::DateTime::parse_from_rfc3339(&st(sr, "added")).map(|d| d.timestamp()).unwrap_or_else(|_| now())),
                    in_cinemas: None,
                    digital_release: None,
                    physical_release: None,
                    minimum_availability: "released".into(),
                    studio: None,
                    network: opt(sr, "network"),
                    seasons: sr["seasons"].as_array().map(|a| a.iter().map(|s| SeasonInfo { number: num(s, "seasonNumber") as u32, monitored: s["monitored"].as_bool().unwrap_or(false) }).collect()).unwrap_or_default(),
                    season_folder: sr["seasonFolder"].as_bool().unwrap_or(true),
                    original_language: None,
                    first_aired: date(sr, "firstAired"),
                    last_search_at: existing.as_ref().map(|e| e.last_search_at).unwrap_or(0),
                    last_refresh_at: now(),
                };
                self.db.save_title(&mut t)?;
                report.series += 1;
                let sid = sr["id"].as_i64().unwrap_or(0);

                // Files first, so episodes can point at them.
                let mut file_map: HashMap<i64, i64> = HashMap::new();
                self.db.with(|c| c.execute("DELETE FROM files WHERE title_id = ?1", [t.id]))?;
                for f in self.arr(sonarr, &format!("episodefile?seriesId={sid}")).await?.as_array().into_iter().flatten() {
                    let file = MediaFile {
                        id: 0,
                        title_id: t.id,
                        rel_path: st(f, "relativePath"),
                        size: num(f, "size"),
                        quality: quality_model(&f["quality"]),
                        release_group: opt(f, "releaseGroup"),
                        edition: String::new(),
                        media_info: media_info(&f["mediaInfo"]),
                        scene_name: opt(f, "sceneName"),
                        languages: f["languages"].as_array().map(|l| l.iter().map(|x| st(x, "name").to_lowercase()).collect()).unwrap_or_default(),
                        added_at: chrono::DateTime::parse_from_rfc3339(&st(f, "dateAdded")).map(|d| d.timestamp()).unwrap_or_else(|_| now()),
                    };
                    let id = self.db.with(|c| {
                        c.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1,?2,?3,?4)", rusqlite::params![t.id, file.rel_path, file.size as i64, serde_json::to_string(&file).unwrap_or_default()])?;
                        Ok(c.last_insert_rowid())
                    })?;
                    file_map.insert(f["id"].as_i64().unwrap_or(0), id);
                    report.files += 1;
                }
                for e in self.arr(sonarr, &format!("episode?seriesId={sid}")).await?.as_array().into_iter().flatten() {
                    let file_id = file_map.get(&e["episodeFileId"].as_i64().unwrap_or(0)).copied();
                    let ep = Episode {
                        id: 0,
                        title_id: t.id,
                        season: num(e, "seasonNumber") as u32,
                        episode: num(e, "episodeNumber") as u32,
                        absolute: e["absoluteEpisodeNumber"].as_u64().map(|n| n as u32),
                        title: st(e, "title"),
                        overview: st(e, "overview"),
                        air_date: opt(e, "airDate"),
                        air_date_utc: opt(e, "airDateUtc"),
                        runtime: num(e, "runtime") as u32,
                        monitored: e["monitored"].as_bool().unwrap_or(false),
                        file_id,
                    };
                    let id = self.db.upsert_episode(&ep)?;
                    self.db.with(|c| c.execute("UPDATE episodes SET monitored = ?2, file_id = ?3 WHERE id = ?1", rusqlite::params![id, ep.monitored, file_id]))?;
                    report.episodes += 1;
                }
            }
        }

        // ------------------------------------------------------------ SABnzbd
        if let Some(ini) = &o.sab_ini {
            let text = std::fs::read_to_string(ini).with_context(|| format!("reading {}", ini.display()))?;
            let servers = parse_sab_servers(&text);
            report.servers = servers.len();
            if !servers.is_empty() {
                self.settings.set_servers(&servers)?;
            }
            let complete = text.lines().find_map(|l| l.trim().strip_prefix("complete_dir = ").map(str::to_string));
            if let Some(dir) = complete.map(|d| map_path(&d)) {
                if let Some(parent) = std::path::Path::new(&dir).parent() {
                    general.downloads_dir = parent.to_string_lossy().to_string();
                }
            }
        }

        // The volume everything lives on must be mounted before Spool touches media.
        if general.required_volume.is_empty() {
            if let Some(rest) = general.movie_root.strip_prefix("/Volumes/").or_else(|| general.series_root.strip_prefix("/Volumes/")) {
                general.required_volume = format!("/Volumes/{}", rest.split('/').next().unwrap_or(""));
            }
        }
        self.settings.set_general(&general)?;
        self.settings.set_naming(&naming)?;
        self.settings.set_size_limits(&limits)?;
        self.apply_engine_config();
        report.unsupported.sort();
        report.unsupported.dedup();
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sab_servers() {
        let ini = r#"
[misc]
complete_dir = /data/downloads/complete
[servers]
[[news.example.com]]
name = news.example.com
displayname = Example
host = news.example.com
port = 563
username = user
password = "p@ss=word"
connections = 50
ssl = 1
ssl_verify = 3
priority = 0
enable = 1
[[backup.example.net]]
host = backup.example.net
port = 119
ssl = 0
connections = 20
priority = 1
enable = 0
[categories]
[[movies]]
name = movies
"#;
        let s = parse_sab_servers(ini);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].name.as_str(), s[0].port, s[0].tls, s[0].connections, s[0].priority, s[0].enabled), ("Example", 563, true, 50, 0, true));
        assert_eq!(s[0].password, "p@ss=word");
        assert_eq!((s[1].host.as_str(), s[1].tls, s[1].enabled, s[1].priority), ("backup.example.net", false, false, 1));
    }

    #[test]
    fn converts_profiles() {
        let v: Value = serde_json::json!({
            "name": "HD - 720p/1080p", "upgradeAllowed": true, "cutoff": 1001,
            "items": [
                {"quality": {"id": 1, "name": "SDTV"}, "items": [], "allowed": false},
                {"quality": {"id": 4, "name": "HDTV-720p"}, "items": [], "allowed": true},
                {"name": "WEB 1080p", "id": 1001, "allowed": true, "items": [{"quality": {"id": 3, "name": "WEBDL-1080p"}, "allowed": true}, {"quality": {"id": 15, "name": "WEBRip-1080p"}, "allowed": true}]},
                {"quality": {"id": 20, "name": "Bluray-1080p Remux"}, "items": [], "allowed": false}
            ],
            "formatItems": [{"score": 0}]
        });
        let mut r = Report::default();
        let p = profile(&v, "tv", &mut r).unwrap();
        assert_eq!(p.items.len(), 4);
        assert_eq!(p.cutoff, Quality::Webdl1080p);
        assert!(p.allows(Quality::Webrip1080p) && p.allows(Quality::Hdtv720p) && !p.allows(Quality::Remux1080p));
        assert_eq!(p.items[3].qualities, vec![Quality::Remux1080p]);
        assert!(r.unsupported.is_empty());
        assert_eq!(snake("inCinemas"), "in_cinemas");
    }
}
