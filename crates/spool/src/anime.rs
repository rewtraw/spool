//! Anime as the people who post it see it.
//!
//! The film and series sources file anime the way they file everything else, and the fit is
//! poor: a six-episode OVA is one film to TMDB, a recap film is a "special" of its series to
//! TVDB, and neither says that a series, its recap and its sequel films belong together.
//! AniList keeps each of those as a work of its own, with its format, its episode count and
//! what it is to the works around it. A community list maps its ids to the ones Spool
//! already holds, so a title is found by its IMDb or TVDB id and never by guessing at names.
//!
//! What Spool takes from it: whether a "film" is really posted as episodes, the name a
//! release is likely to carry, and the relations that collections are built from.

use crate::app::{App, Event};
use crate::db::now;
use crate::metadata::Metadata;
use crate::models::{Kind, Title};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

pub const ANILIST: &str = "https://graphql.anilist.co";
pub const ANIME_IDS: &str = "https://raw.githubusercontent.com/Fribb/anime-lists/master/anime-list-mini.json";

/// How long the id list is kept before it is fetched again, and a title before it is asked
/// about again.
const IDS_KEPT: i64 = 7 * 86400;
const TITLE_KEPT: i64 = 30 * 86400;
/// AniList allows thirty requests a minute. One request covers ten titles.
const PER_REQUEST: usize = 10;
const BETWEEN_REQUESTS: std::time::Duration = std::time::Duration::from_millis(2500);

/// Group ids for players and the web app share one space with TMDB's collection ids, which
/// are small. An anime group is its first work's AniList id with this bit set.
const ANIME_GROUP: u32 = 0x4000_0000;

/// The relations that make two works part of one story. A shared character or a cross-over
/// advertisement does not.
const STORY: [&str; 9] = ["PREQUEL", "SEQUEL", "PARENT", "SIDE_STORY", "SUMMARY", "ALTERNATIVE", "SPIN_OFF", "COMPILATION", "CONTAINS"];
/// Formats that come as more than one video.
const EPISODIC: [&str; 5] = ["TV", "TV_SHORT", "OVA", "ONA", "SPECIAL"];
/// Formats worth listing as missing from a collection.
const LISTED: [&str; 5] = ["TV", "TV_SHORT", "MOVIE", "OVA", "ONA"];

/// What the id list says about one AniList entry.
#[derive(Clone, Debug, Default)]
pub struct Known {
    pub imdb: Option<String>,
    pub tvdb: Option<u32>,
    /// The season of the TVDB series this entry is; 0 for a special.
    pub season: u32,
    pub film: bool,
}

/// The community id list, read for the ids Spool can use.
#[derive(Default)]
pub struct Ids {
    by_imdb: HashMap<String, Vec<u32>>,
    by_tvdb: HashMap<u32, Vec<(u32, u32)>>,
    pub back: HashMap<u32, Known>,
}

impl Ids {
    pub fn parse(bytes: &[u8]) -> Result<Ids> {
        let list: Vec<Value> = serde_json::from_slice(bytes)?;
        let mut ids = Ids::default();
        for e in &list {
            let Some(anilist) = e.get("anilist_id").and_then(|x| x.as_u64()).map(|x| x as u32) else { continue };
            let imdb: Vec<String> = match e.get("imdb_id") {
                Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
                Some(Value::String(s)) => vec![s.clone()],
                _ => vec![],
            };
            let tvdb = e.get("tvdb_id").and_then(|x| x.as_u64()).map(|x| x as u32);
            let season = e.get("season").and_then(|s| s.get("tvdb")).and_then(|x| x.as_u64()).map(|x| x as u32).unwrap_or(1);
            let film = e.get("type").and_then(|x| x.as_str()) == Some("MOVIE");
            for i in &imdb {
                ids.by_imdb.entry(i.clone()).or_default().push(anilist);
            }
            if let Some(t) = tvdb {
                ids.by_tvdb.entry(t).or_default().push((season, anilist));
            }
            ids.back.insert(anilist, Known { imdb: imdb.first().cloned(), tvdb, season, film });
        }
        if ids.back.is_empty() {
            bail!("the anime id list is empty");
        }
        Ok(ids)
    }

    /// The AniList entries a title is: one for a film, one a season for a series, first
    /// season first. A series' specials are other works and are left out.
    pub fn of(&self, t: &Title) -> Vec<u32> {
        match t.kind {
            Kind::Movie => t.imdb_id.as_ref().and_then(|i| self.by_imdb.get(i)).map(|v| v.iter().take(1).copied().collect()).unwrap_or_default(),
            Kind::Series => {
                let mut seasons: Vec<(u32, u32)> = t.tvdb_id.and_then(|i| self.by_tvdb.get(&i)).map(|v| v.iter().filter(|s| s.0 >= 1).copied().collect()).unwrap_or_default();
                seasons.sort();
                seasons.dedup_by_key(|s| s.1);
                seasons.into_iter().map(|s| s.1).take(24).collect()
            }
        }
    }
}

/// One relation: what `to` is to the work that holds the edge.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub kind: String,
    pub to: u32,
}

/// One work as AniList lists it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Media {
    pub id: u32,
    pub format: String,
    pub episodes: u32,
    /// How long one episode runs, in minutes.
    pub minutes: u32,
    pub year: u32,
    pub title: String,
    /// English and romaji names, the ones releases carry.
    pub names: Vec<String>,
    pub poster: Option<String>,
    pub banner: Option<String>,
    pub edges: Vec<Edge>,
    /// How much is known: 2 asked about directly, 1 seen beside one that was (its relations
    /// are known), 0 only named by one of those.
    pub depth: u8,
}

const QUERY: &str = "query($ids:[Int]){Page(perPage:50){media(id_in:$ids,type:ANIME){id format episodes duration seasonYear startDate{year} title{romaji english} coverImage{large} bannerImage relations{edges{relationType node{id type format episodes duration seasonYear startDate{year} title{romaji english} coverImage{large} relations{edges{relationType node{id type format seasonYear startDate{year} title{romaji english}}}}}}}}}}";

fn media(v: &Value, depth: u8) -> Media {
    let text = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(str::to_string);
    let number = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let title = v.get("title").cloned().unwrap_or(Value::Null);
    let names: Vec<String> = [text(&title, "english"), text(&title, "romaji")].into_iter().flatten().collect();
    let year = match number("seasonYear") {
        0 => v.get("startDate").and_then(|d| d.get("year")).and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        y => y,
    };
    let edges = v
        .get("relations")
        .and_then(|r| r.get("edges"))
        .and_then(|e| e.as_array())
        .into_iter()
        .flatten()
        .filter(|e| e["node"]["type"] == "ANIME")
        .filter_map(|e| Some(Edge { kind: e["relationType"].as_str()?.to_string(), to: e["node"]["id"].as_u64()? as u32 }))
        .collect();
    Media {
        id: number("id"),
        format: text(v, "format").unwrap_or_default(),
        episodes: number("episodes"),
        minutes: number("duration"),
        year,
        title: names.first().cloned().unwrap_or_default(),
        names,
        poster: v.get("coverImage").and_then(|c| text(c, "large")),
        banner: text(v, "bannerImage"),
        edges,
        depth,
    }
}

impl Metadata {
    /// The works with these ids, the works beside them and the ones beside those, in one
    /// request. Those asked for come back at depth 2.
    pub async fn anime(&self, ids: &[u32]) -> Result<Vec<Media>> {
        let r = self.http.post(&self.anilist_base).timeout(std::time::Duration::from_secs(30)).json(&json!({"query": QUERY, "variables": {"ids": ids}})).send().await.map_err(|e| anyhow!("{}", e.without_url()))?;
        if r.status().as_u16() == 429 {
            bail!("AniList asked for a pause");
        }
        if !r.status().is_success() {
            bail!("AniList answered HTTP {}", r.status().as_u16());
        }
        let v: Value = r.json().await.map_err(|e| anyhow!("{}", e.without_url()))?;
        let mut out: BTreeMap<u32, Media> = BTreeMap::new();
        let mut keep = |m: Media| {
            if m.id != 0 && out.get(&m.id).is_none_or(|have| have.depth < m.depth) {
                out.insert(m.id, m);
            }
        };
        for m in v["data"]["Page"]["media"].as_array().into_iter().flatten() {
            keep(media(m, 2));
            for e in m["relations"]["edges"].as_array().into_iter().flatten().filter(|e| e["node"]["type"] == "ANIME") {
                keep(media(&e["node"], 1));
                for far in e["node"]["relations"]["edges"].as_array().into_iter().flatten().filter(|e| e["node"]["type"] == "ANIME") {
                    keep(media(&far["node"], 0));
                }
            }
        }
        Ok(out.into_values().collect())
    }
}

/// A set of titles that belong together: the films of one TMDB collection, or the series,
/// films and specials of one anime.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Group {
    pub id: u32,
    pub name: String,
    pub poster: Option<String>,
    pub fanart: Option<String>,
    /// "anime" or "films".
    pub kind: String,
    /// In the order they came out.
    pub members: Vec<Member>,
    /// Works of the same story that are not in the library.
    pub missing: Vec<Missing>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Member {
    pub title_id: i64,
    pub year: u32,
    /// What it is to the story, when it is not simply part of it: "Recap", "Side story",
    /// "Alternative telling", "Spin-off".
    pub role: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Missing {
    pub anilist_id: u32,
    pub title: String,
    pub year: u32,
    pub format: String,
    pub episodes: u32,
    pub role: String,
    pub poster: Option<String>,
    /// Whether Spool knows the ids to add it by.
    pub can_add: bool,
}

fn role_of(kind: &str) -> &'static str {
    match kind {
        "SUMMARY" | "COMPILATION" => "Recap",
        "ALTERNATIVE" => "Alternative telling",
        "SPIN_OFF" => "Spin-off",
        "SIDE_STORY" => "Side story",
        _ => "",
    }
}

/// Collections among these titles. A title is in at most one: an anime's group takes it
/// before a film collection does, because it holds the series as well.
pub fn groups(titles: &[Title], works: &[Media], ids: Option<&Ids>) -> Vec<Group> {
    let by_id: HashMap<u32, &Media> = works.iter().map(|m| (m.id, m)).collect();
    // Join works that a story relation ties, in either direction.
    let mut parent: HashMap<u32, u32> = HashMap::new();
    fn find(parent: &mut HashMap<u32, u32>, x: u32) -> u32 {
        let p = *parent.entry(x).or_insert(x);
        if p == x {
            return x;
        }
        let root = find(parent, p);
        parent.insert(x, root);
        root
    }
    for m in works {
        for e in m.edges.iter().filter(|e| STORY.contains(&e.kind.as_str())) {
            let (a, b) = (find(&mut parent, m.id), find(&mut parent, e.to));
            if a != b {
                parent.insert(a, b);
            }
        }
    }
    // What each work is to the story: what another work calls it, or failing that the fact
    // that it names a parent.
    let mut role: HashMap<u32, &'static str> = HashMap::new();
    for m in works {
        for e in &m.edges {
            let r = role_of(&e.kind);
            if !r.is_empty() && (role.get(&e.to).is_none_or(|have| *have == "Side story")) {
                role.insert(e.to, r);
            }
        }
    }
    for m in works {
        if m.edges.iter().any(|e| e.kind == "PARENT") && !role.contains_key(&m.id) {
            role.insert(m.id, "Side story");
        }
    }

    let known: Vec<u32> = parent.keys().copied().collect();
    let story: HashMap<u32, u32> = known.into_iter().map(|i| (i, find(&mut parent, i))).collect();
    let story_of = |i: u32| story.get(&i).copied().unwrap_or(i);

    let mut out: Vec<Group> = vec![];
    let mut taken: HashSet<i64> = HashSet::new();
    let mut by_root: BTreeMap<u32, Vec<&Title>> = BTreeMap::new();
    for t in titles.iter().filter(|t| !t.anilist.is_empty()) {
        by_root.entry(story_of(t.anilist[0])).or_default().push(t);
    }
    for (root, mut members) in by_root {
        if members.len() < 2 {
            continue;
        }
        members.sort_by_key(|t| (t.year, t.id));
        let in_story: Vec<&Media> = works.iter().filter(|m| story_of(m.id) == root).collect();
        // Named for where the story began: its first series, or its first work of any kind.
        let Some(first) = in_story.iter().filter(|m| m.year > 0).min_by_key(|m| (m.format != "TV", m.year, m.id)).or(in_story.first()) else { continue };
        let have: HashSet<u32> = members.iter().flat_map(|t| t.anilist.iter().copied()).collect();
        // Works the library's own are directly tied to, and does not hold under any id.
        let near: HashSet<u32> = have.iter().filter_map(|i| by_id.get(i)).flat_map(|m| m.edges.iter().filter(|e| STORY.contains(&e.kind.as_str())).map(|e| e.to)).collect();
        let held = |m: &Media| {
            let Some(k) = ids.and_then(|i| i.back.get(&m.id)) else { return false };
            titles.iter().any(|t| match t.kind {
                Kind::Movie => k.imdb.is_some() && t.imdb_id == k.imdb,
                Kind::Series => k.tvdb.is_some() && t.tvdb_id == k.tvdb && k.season >= 1,
            })
        };
        let mut missing: Vec<Missing> = in_story
            .iter()
            .filter(|m| near.contains(&m.id) && !have.contains(&m.id) && LISTED.contains(&m.format.as_str()) && !held(m))
            .map(|m| Missing {
                anilist_id: m.id,
                title: m.title.clone(),
                year: m.year,
                format: m.format.clone(),
                episodes: m.episodes,
                role: role.get(&m.id).copied().unwrap_or_default().to_string(),
                poster: m.poster.clone(),
                can_add: ids.and_then(|i| i.back.get(&m.id)).is_some_and(|k| k.imdb.is_some() || k.tvdb.is_some()),
            })
            .collect();
        missing.sort_by_key(|m| (m.year, m.anilist_id));
        taken.extend(members.iter().map(|t| t.id));
        out.push(Group {
            id: ANIME_GROUP | first.id,
            name: first.title.clone(),
            poster: first.poster.clone().or_else(|| members.iter().find_map(|t| t.poster.clone())),
            fanart: first.banner.clone().or_else(|| members.iter().find_map(|t| t.fanart.clone())),
            kind: "anime".into(),
            members: members.iter().map(|t| Member { title_id: t.id, year: t.year, role: role.get(&t.anilist[0]).copied().unwrap_or_default().to_string() }).collect(),
            missing,
        });
    }

    let mut films: BTreeMap<u32, Vec<&Title>> = BTreeMap::new();
    for t in titles.iter().filter(|t| !taken.contains(&t.id)) {
        if let Some(c) = &t.collection {
            films.entry(c.id).or_default().push(t);
        }
    }
    for (id, mut members) in films {
        if members.len() < 2 {
            continue;
        }
        members.sort_by_key(|t| (t.year, t.id));
        let c = members[0].collection.clone().unwrap_or_default();
        out.push(Group { id, name: c.name, poster: c.poster, fanart: c.fanart, kind: "films".into(), members: members.iter().map(|t| Member { title_id: t.id, year: t.year, role: String::new() }).collect(), missing: vec![] });
    }
    out.sort_by_key(|g| g.name.to_lowercase());
    out
}

impl App {
    /// The id list, from memory or from the copy kept on disk.
    pub fn anime_ids(&self) -> Option<Arc<Ids>> {
        if let Some(ids) = self.anime_list.lock().clone() {
            return Some(ids);
        }
        let ids = Arc::new(Ids::parse(&std::fs::read(self.data_dir.join("anime-ids.json")).ok()?).ok()?);
        *self.anime_list.lock() = Some(ids.clone());
        Some(ids)
    }

    /// The id list, fetched if there is no copy or the copy is old. An old copy serves when
    /// the fetch fails.
    pub async fn fresh_anime_ids(&self) -> Option<Arc<Ids>> {
        let path = self.data_dir.join("anime-ids.json");
        let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|m| m.elapsed().ok()).map(|d| d.as_secs() as i64);
        if age.is_none_or(|a| a > IDS_KEPT) {
            let fetched = async {
                let r = self.http.get(&self.meta.anime_ids_url).timeout(std::time::Duration::from_secs(60)).send().await.map_err(|e| anyhow!("{}", e.without_url()))?;
                if !r.status().is_success() {
                    bail!("HTTP {}", r.status().as_u16());
                }
                let bytes = r.bytes().await.map_err(|e| anyhow!("{}", e.without_url()))?;
                let ids = Ids::parse(&bytes)?;
                let tmp = path.with_extension("tmp");
                std::fs::write(&tmp, &bytes)?;
                std::fs::rename(&tmp, &path)?;
                Ok(ids)
            };
            match fetched.await {
                Ok(ids) => *self.anime_list.lock() = Some(Arc::new(ids)),
                Err(e) => tracing::info!(error = %e, "the anime id list could not be fetched"),
            }
        }
        self.anime_ids()
    }

    /// Take from what is known of a title's anime entries what Spool uses on the title
    /// itself. Returns whether anything changed.
    fn apply_anime(&self, t: &mut Title, entries: Vec<u32>) -> Result<bool> {
        let before = (t.anilist.clone(), t.alt_titles.clone(), t.in_episodes);
        t.anilist = entries;
        // The names of the first entry only: a later season's name on a release numbered
        // from one would be read as the first season's episodes.
        if let Some(m) = t.anilist.first().and_then(|i| self.db.anime(*i).ok().flatten()) {
            for name in &m.names {
                if !name.eq_ignore_ascii_case(&t.title) && !t.alt_titles.iter().any(|a| a.eq_ignore_ascii_case(name)) {
                    t.alt_titles.push(name.clone());
                }
            }
            // A "film" that is an OVA or a series: it is posted as its episodes.
            if t.kind == Kind::Movie && EPISODIC.contains(&m.format.as_str()) && m.episodes >= 2 {
                t.in_episodes = true;
            }
        }
        Ok(before != (t.anilist.clone(), t.alt_titles.clone(), t.in_episodes))
    }

    /// Find out whether a title is anime and, if so, what AniList says of it. For one title,
    /// when it is added; the sweep does the library. Failing to reach either source leaves
    /// the title as it was, to be asked about again.
    pub async fn learn_anime(&self, t: &mut Title) -> bool {
        let Some(ids) = self.fresh_anime_ids().await else { return false };
        let entries = ids.of(t);
        if !entries.is_empty() {
            match self.meta.anime(&entries[..entries.len().min(PER_REQUEST)]).await {
                Ok(works) => {
                    if let Err(e) = self.db.anime_put(&works) {
                        tracing::warn!(error = %e, "could not keep what AniList said");
                    }
                }
                Err(e) => {
                    tracing::debug!(title = %t.title, error = %e, "AniList could not be asked");
                    return false;
                }
            }
        }
        t.anime_checked_at = now();
        let anime = !entries.is_empty();
        let _ = self.apply_anime(t, entries);
        anime
    }

    /// The "anime" task: every title not yet asked about, or asked about a month ago.
    pub async fn anime_sweep(&self) -> Result<String> {
        let Some(ids) = self.fresh_anime_ids().await else { bail!("the anime id list is not available") };
        let due: Vec<Title> = self.db.titles(None)?.into_iter().filter(|t| now() - t.anime_checked_at > TITLE_KEPT).collect();
        let mut wanted: Vec<(i64, Vec<u32>)> = vec![];
        let mut plain = 0;
        for t in &due {
            let entries = ids.of(t);
            if entries.is_empty() {
                // Not anime, as far as the list knows. No request is spent on it.
                if let Some(mut t) = self.db.title(t.id)? {
                    t.anime_checked_at = now();
                    self.db.save_title(&mut t)?;
                }
                plain += 1;
            } else {
                wanted.push((t.id, entries));
            }
        }
        let all: Vec<u32> = wanted.iter().flat_map(|w| w.1.iter().copied()).collect::<HashSet<_>>().into_iter().collect();
        let mut asked: HashSet<u32> = HashSet::new();
        let mut paused = false;
        for chunk in all.chunks(PER_REQUEST) {
            match self.meta.anime(chunk).await {
                Ok(works) => {
                    self.db.anime_put(&works)?;
                    asked.extend(chunk);
                }
                Err(e) => {
                    tracing::info!(error = %e, "stopping the anime sweep until next time");
                    paused = true;
                    break;
                }
            }
            tokio::time::sleep(BETWEEN_REQUESTS).await;
        }
        let mut found = 0;
        for (id, entries) in wanted {
            if !entries.iter().all(|e| asked.contains(e)) {
                continue;
            }
            let Some(mut t) = self.db.title(id)? else { continue };
            t.anime_checked_at = now();
            self.apply_anime(&mut t, entries)?;
            self.db.save_title(&mut t)?;
            self.emit(Event::Title { id });
            found += 1;
        }
        Ok(format!("{found} anime titles read from AniList, {plain} others checked{}", if paused { "; the rest next time" } else { "" }))
    }

    /// The library's collections.
    pub fn collections(&self) -> Result<Vec<Group>> {
        let titles = self.db.titles(None)?;
        let works = if titles.iter().any(|t| !t.anilist.is_empty()) { self.db.anime_all()? } else { vec![] };
        Ok(groups(&titles, &works, self.anime_ids().as_deref()))
    }

    /// Add a work that a collection lacks, by the ids the list has for it.
    pub async fn add_missing(&self, anilist_id: u32) -> Result<Title> {
        let ids = self.anime_ids().ok_or_else(|| anyhow!("the anime id list is not available"))?;
        let known = ids.back.get(&anilist_id).ok_or_else(|| anyhow!("the id list does not know this work"))?;
        let g = self.settings.general();
        if let Some(tvdb) = known.tvdb.filter(|_| !known.film && known.season >= 1) {
            let (listed, _) = self.meta.series_by_tvdb(tvdb).await?;
            let tvmaze = listed.tvmaze_id.ok_or_else(|| anyhow!("the series source does not list it"))?;
            return self.add_title(Kind::Series, None, Some(tvmaze), g.default_series_profile, true, true).await;
        }
        let imdb = known.imdb.clone().ok_or_else(|| anyhow!("no film or series id is known for this work"))?;
        let tmdb = self.meta.movie_by_imdb(&g.tmdb_api_key, &imdb).await?.ok_or_else(|| anyhow!("the film source does not list it"))?;
        self.add_title(Kind::Movie, Some(tmdb), None, g.default_movie_profile, true, true).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(id: u32, format: &str, year: u32, title: &str, edges: &[(&str, u32)]) -> Media {
        Media { id, format: format.into(), year, title: title.into(), names: vec![title.into()], edges: edges.iter().map(|(k, to)| Edge { kind: k.to_string(), to: *to }).collect(), depth: 2, ..Default::default() }
    }
    fn title(id: i64, kind: &str, name: &str, year: u32, anilist: &[u32]) -> Title {
        let mut t: Title = serde_json::from_value(json!({"kind": kind, "title": name, "year": year})).unwrap();
        (t.id, t.anilist) = (id, anilist.to_vec());
        t
    }

    #[test]
    fn a_series_its_recap_and_its_films_make_one_collection() {
        // The shape of a real one: the series, a recap film, a sequel film, and a retelling
        // tied to the series only through the sequel.
        let works = [
            work(30, "TV", 1995, "Example Genesis", &[("SEQUEL", 32), ("SUMMARY", 31), ("CHARACTER", 900)]),
            work(31, "MOVIE", 1997, "Example Genesis: Recap", &[("PARENT", 30)]),
            work(32, "MOVIE", 1997, "The End of Example", &[("PREQUEL", 30), ("ALTERNATIVE", 2759)]),
            work(2759, "MOVIE", 2007, "Example 1.0", &[("ALTERNATIVE", 32), ("SEQUEL", 3784)]),
            work(3784, "MOVIE", 2009, "Example 2.0", &[("PREQUEL", 2759)]),
            // Shares a character and nothing else.
            work(900, "TV", 2018, "Train Robots", &[("CHARACTER", 30)]),
        ];
        let titles = [title(1, "series", "Example Genesis", 1995, &[30]), title(2, "movie", "Example Genesis: Recap", 1997, &[31]), title(3, "movie", "Example 1.0", 2007, &[2759]), title(4, "series", "Train Robots", 2018, &[900]), title(5, "movie", "Heat", 1995, &[])];
        let found = groups(&titles, &works, None);
        assert_eq!(found.len(), 1, "{found:?}");
        let g = &found[0];
        assert_eq!((g.name.as_str(), g.kind.as_str(), g.id), ("Example Genesis", "anime", ANIME_GROUP | 30));
        assert_eq!(g.members.iter().map(|m| (m.title_id, m.role.as_str())).collect::<Vec<_>>(), [(1, ""), (2, "Recap"), (3, "Alternative telling")]);
        // The sequel film and the second retelling are tied to what is here, and are not here.
        assert_eq!(g.missing.iter().map(|m| m.anilist_id).collect::<Vec<_>>(), [32, 3784]);
    }

    #[test]
    fn the_id_list_gives_a_film_its_entry_and_a_series_its_seasons() {
        let list = json!([
            {"type": "OVA", "anilist_id": 82, "imdb_id": ["tt0097661"], "tvdb_id": 80785, "season": {"tvdb": 1}},
            {"type": "TV", "anilist_id": 30, "imdb_id": ["tt0112159"], "tvdb_id": 70350, "season": {"tvdb": 1}},
            {"type": "MOVIE", "anilist_id": 32, "imdb_id": ["tt0169858"], "tvdb_id": 70350, "season": {"tvdb": 0}},
            {"type": "TV", "anilist_id": 31, "tvdb_id": 70350, "season": {"tvdb": 2}},
            {"type": "TV", "anidb_id": 5}
        ]);
        let ids = Ids::parse(list.to_string().as_bytes()).unwrap();
        let mut film = title(1, "movie", "A Film", 1989, &[]);
        film.imdb_id = Some("tt0097661".into());
        assert_eq!(ids.of(&film), [82]);
        let mut show = title(2, "series", "A Show", 1995, &[]);
        show.tvdb_id = Some(70350);
        assert_eq!(ids.of(&show), [30, 31], "seasons in order, without the special");
        assert!(ids.back[&32].film);
    }
}
