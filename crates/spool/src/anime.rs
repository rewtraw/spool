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
/// Raised when the way a title is matched to its entry changes, so the library is read again.
const RULES: u32 = 4;

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
    /// How far into that season the entry starts: its first episode is the season's
    /// episode `offset + 1`.
    pub offset: u32,
    pub film: bool,
    /// The list gives this entry several IMDb ids: it stands for a set of works, and which
    /// of them a film is has to be settled some other way.
    pub several: bool,
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
            let offset = e.get("episode_offset").and_then(|s| s.get("tvdb")).and_then(|x| x.as_u64()).map(|x| x as u32).unwrap_or(0);
            let film = e.get("type").and_then(|x| x.as_str()) == Some("MOVIE");
            for i in &imdb {
                ids.by_imdb.entry(i.clone()).or_default().push(anilist);
            }
            if let Some(t) = tvdb {
                ids.by_tvdb.entry(t).or_default().push((season, anilist));
            }
            ids.back.insert(anilist, Known { imdb: imdb.first().cloned(), tvdb, season, offset, film, several: imdb.len() > 1 });
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

const SEARCH: &str = "query($q:String){Page(perPage:6){media(search:$q,type:ANIME){id format episodes duration seasonYear startDate{year} title{romaji english} coverImage{large}}}}";

impl Metadata {
    /// Anime by name, as AniList finds it: under its English, romanised or native name.
    pub async fn anime_search(&self, query: &str) -> Result<Vec<Media>> {
        let r = self.http.post(&self.anilist_base).timeout(std::time::Duration::from_secs(10)).json(&json!({"query": SEARCH, "variables": {"q": query}})).send().await.map_err(|e| anyhow!("{}", e.without_url()))?;
        if !r.status().is_success() {
            bail!("AniList answered HTTP {}", r.status().as_u16());
        }
        let v: Value = r.json().await.map_err(|e| anyhow!("{}", e.without_url()))?;
        Ok(v["data"]["Page"]["media"].as_array().into_iter().flatten().map(|m| media(m, 0)).filter(|m| m.id != 0).collect())
    }

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

/// A set of titles that belong together: the films of one TMDB collection, an anime's
/// series, films and specials, or a set the user made.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Group {
    pub id: u32,
    pub name: String,
    pub poster: Option<String>,
    pub fanart: Option<String>,
    /// "anime", "films" or "own".
    pub kind: String,
    /// In order of release.
    pub members: Vec<Member>,
    /// Works of the same story, or films of the same collection, that are not in the library.
    pub missing: Vec<Missing>,
    /// The member to watch next, once one of them has been watched: the first in story
    /// order that has not been.
    pub next: Option<i64>,
    /// The user has put its titles in an order of their own, which `story` then follows.
    pub ordered: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Member {
    pub title_id: i64,
    pub year: u32,
    /// What it is to the story, when it is not simply part of it: "Recap", "Side story",
    /// "Alternative telling", "Spin-off".
    pub role: String,
    /// Its place when the story is followed in its own order, not the order of release.
    pub story: u32,
    pub watched: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Missing {
    /// Its AniList entry, for anime; 0 for a film of a film collection.
    pub anilist_id: u32,
    pub tmdb_id: Option<u32>,
    pub title: String,
    pub year: u32,
    pub format: String,
    pub episodes: u32,
    pub role: String,
    pub poster: Option<String>,
    pub story: u32,
    /// Whether Spool knows the ids to add it by.
    pub can_add: bool,
}

/// A film collection as TMDB lists it, whole.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilmSet {
    pub id: u32,
    pub name: String,
    pub parts: Vec<FilmPart>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilmPart {
    pub tmdb_id: u32,
    pub title: String,
    pub year: u32,
    pub poster: Option<String>,
}

/// What the user has said about collections, which stands over what the sources say.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Edits {
    /// A collection's name, by its id.
    pub names: HashMap<u32, String>,
    /// A title's role in its collection.
    pub roles: HashMap<i64, String>,
    /// The collection a title is put in; 0 keeps it out of any.
    pub placed: HashMap<i64, u32>,
    /// Collections the user made, by id and name.
    pub own: Vec<(u32, String)>,
    /// The order the user put a collection's titles in, by the collection's id.
    pub order: HashMap<u32, Vec<i64>>,
}

/// Ids of collections the user made carry this bit.
pub const OWN_GROUP: u32 = 0x2000_0000;

/// Everything collections are worked out from.
#[derive(Default)]
pub struct Sources<'a> {
    pub works: &'a [Media],
    pub ids: Option<&'a Ids>,
    pub sets: &'a [FilmSet],
    pub edits: Edits,
    /// Titles that have been watched through.
    pub watched: HashSet<i64>,
}

/// The ways out of each work of a story, relations followed both ways, since a work often
/// names its parent and the parent does not name it.
fn steps<'a>(story: &[&'a Media]) -> HashMap<u32, Vec<(u32, &'a str)>> {
    let mut steps: HashMap<u32, Vec<(u32, &str)>> = HashMap::new();
    for m in story {
        for e in m.edges.iter().filter(|e| STORY.contains(&e.kind.as_str())) {
            steps.entry(m.id).or_default().push((e.to, e.kind.as_str()));
            let back = match e.kind.as_str() {
                "PREQUEL" => "SEQUEL",
                "SEQUEL" => "PREQUEL",
                "PARENT" => "SIDE_STORY",
                "ALTERNATIVE" => "ALTERNATIVE",
                _ => "PARENT",
            };
            steps.entry(e.to).or_default().push((m.id, back));
        }
    }
    // What a work says of another outright counts before what is only implied by the reverse.
    for list in steps.values_mut() {
        list.sort_by_key(|(_, kind)| *kind == "SIDE_STORY");
    }
    steps
}

/// What each work of a story is to it, reckoned outward from where the story began. A sequel
/// continues whatever it follows: the second film of a retelling is still the retelling.
fn roles(first: u32, story: &[&Media]) -> HashMap<u32, &'static str> {
    let steps = steps(story);
    let mut role: HashMap<u32, &'static str> = HashMap::from([(first, "")]);
    let mut queue = std::collections::VecDeque::from([first]);
    while let Some(at) = queue.pop_front() {
        let here = role[&at];
        for (to, kind) in steps.get(&at).into_iter().flatten() {
            if role.contains_key(to) {
                continue;
            }
            let said = role_of(kind);
            role.insert(*to, if here.is_empty() { said } else { here });
            queue.push_back(*to);
        }
    }
    role
}

/// The order to follow a story in: from its earliest point in the telling, each work, then
/// what hangs off it (its recaps, side stories and spin-offs), then what follows it, and
/// other tellings of the same story last.
fn story_order(first: u32, story: &[&Media]) -> HashMap<u32, u32> {
    let steps = steps(story);
    let year: HashMap<u32, u32> = story.iter().map(|m| (m.id, m.year)).collect();
    // Back along prequels to where the telling starts.
    let mut start = first;
    let mut seen = HashSet::from([start]);
    while let Some((before, _)) = steps.get(&start).into_iter().flatten().find(|(to, kind)| *kind == "PREQUEL" && !seen.contains(to)) {
        start = *before;
        seen.insert(start);
    }
    let mut order: Vec<u32> = vec![];
    let mut placed: HashSet<u32> = HashSet::new();
    let mut stack = vec![start];
    while let Some(at) = stack.pop() {
        if !placed.insert(at) {
            continue;
        }
        order.push(at);
        let mut next: Vec<(u8, u32, u32)> = steps
            .get(&at)
            .into_iter()
            .flatten()
            .filter(|(to, _)| !placed.contains(to))
            .map(|(to, kind)| {
                let rank = match *kind {
                    "SUMMARY" | "COMPILATION" | "SIDE_STORY" | "SPIN_OFF" | "CONTAINS" => 0,
                    "SEQUEL" => 1,
                    "ALTERNATIVE" => 2,
                    _ => 3,
                };
                (rank, year.get(to).copied().unwrap_or(9999), *to)
            })
            .collect();
        // A stack gives back the last pushed first.
        next.sort();
        stack.extend(next.into_iter().rev().map(|n| n.2));
    }
    let mut rest: Vec<&&Media> = story.iter().filter(|m| !placed.contains(&m.id)).collect();
    rest.sort_by_key(|m| (m.year, m.id));
    order.extend(rest.into_iter().map(|m| m.id));
    order.into_iter().enumerate().map(|(i, id)| (id, i as u32)).collect()
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
/// before a film collection does, because it holds the series as well; and where the user
/// has put a title stands over both.
pub fn groups(titles: &[Title], from: &Sources) -> Vec<Group> {
    let (works, ids, edits) = (from.works, from.ids, &from.edits);
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
    let known: Vec<u32> = parent.keys().copied().collect();
    let story: HashMap<u32, u32> = known.into_iter().map(|i| (i, find(&mut parent, i))).collect();
    let story_of = |i: u32| story.get(&i).copied().unwrap_or(i);

    let member = |t: &Title, role: &str, story: u32| Member { title_id: t.id, year: t.year, role: edits.roles.get(&t.id).cloned().unwrap_or_else(|| role.to_string()), story, watched: from.watched.contains(&t.id) };
    let mut out: Vec<Group> = vec![];
    let mut taken: HashSet<i64> = HashSet::new();
    let mut by_root: BTreeMap<u32, Vec<&Title>> = BTreeMap::new();
    for t in titles.iter().filter(|t| !t.anilist.is_empty() && !edits.placed.contains_key(&t.id)) {
        by_root.entry(story_of(t.anilist[0])).or_default().push(t);
    }
    for (root, mut members) in by_root {
        members.sort_by_key(|t| (t.year, t.id));
        let in_story: Vec<&Media> = works.iter().filter(|m| story_of(m.id) == root).collect();
        // Named for where the story began: its earliest work, a series before a film of the same year.
        let rank = |m: &Media| ["TV", "OVA", "ONA", "TV_SHORT", "MOVIE"].iter().position(|f| *f == m.format).unwrap_or(9);
        let Some(first) = in_story.iter().filter(|m| m.year > 0).min_by_key(|m| (m.year, rank(m), m.id)).or(in_story.first()) else { continue };
        let role = roles(first.id, &in_story);
        let order = story_order(first.id, &in_story);
        let have: HashSet<u32> = members.iter().flat_map(|t| t.anilist.iter().copied()).collect();
        // Works the library's own are directly tied to, and does not hold under any id.
        let near: HashSet<u32> = have.iter().filter_map(|i| by_id.get(i)).flat_map(|m| m.edges.iter().filter(|e| STORY.contains(&e.kind.as_str())).map(|e| e.to)).collect();
        let held = |m: &Media| {
            let Some(k) = ids.and_then(|i| i.back.get(&m.id)) else { return false };
            titles.iter().any(|t| match t.kind {
                Kind::Movie => !k.several && k.imdb.is_some() && t.imdb_id == k.imdb,
                Kind::Series => k.tvdb.is_some() && t.tvdb_id == k.tvdb && k.season >= 1,
            })
        };
        let mut missing: Vec<Missing> = in_story
            .iter()
            .filter(|m| near.contains(&m.id) && !have.contains(&m.id) && LISTED.contains(&m.format.as_str()) && !held(m))
            .map(|m| Missing {
                anilist_id: m.id,
                tmdb_id: None,
                title: m.title.clone(),
                year: m.year,
                format: m.format.clone(),
                episodes: m.episodes,
                role: role.get(&m.id).copied().unwrap_or_default().to_string(),
                poster: m.poster.clone(),
                story: order.get(&m.id).copied().unwrap_or(u32::MAX),
                can_add: ids.and_then(|i| i.back.get(&m.id)).is_some_and(|k| !k.several && (k.imdb.is_some() || k.tvdb.is_some())),
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
            members: members.iter().map(|t| member(t, role.get(&t.anilist[0]).copied().unwrap_or_default(), order.get(&t.anilist[0]).copied().unwrap_or(u32::MAX))).collect(),
            missing,
            next: None,
            ordered: false,
        });
    }

    let mut films: BTreeMap<u32, Vec<&Title>> = BTreeMap::new();
    for t in titles.iter().filter(|t| !taken.contains(&t.id) && !edits.placed.contains_key(&t.id)) {
        if let Some(c) = &t.collection {
            films.entry(c.id).or_default().push(t);
        }
    }
    for (id, mut members) in films {
        members.sort_by_key(|t| (t.year, t.id));
        let c = members[0].collection.clone().unwrap_or_default();
        // The collection whole, when it has been read: what is not here is missing.
        let mut parts: Vec<&FilmPart> = from.sets.iter().find(|s| s.id == id).map(|s| s.parts.iter().collect()).unwrap_or_default();
        parts.sort_by_key(|p| (if p.year == 0 { u32::MAX } else { p.year }, p.tmdb_id));
        let place = |tmdb: Option<u32>, fallback: usize| parts.iter().position(|p| Some(p.tmdb_id) == tmdb).unwrap_or(parts.len() + fallback) as u32;
        let missing = parts
            .iter()
            .enumerate()
            .filter(|(_, p)| !titles.iter().any(|t| t.kind == Kind::Movie && t.tmdb_id == Some(p.tmdb_id)))
            .map(|(i, p)| Missing { anilist_id: 0, tmdb_id: Some(p.tmdb_id), title: p.title.clone(), year: p.year, format: "MOVIE".into(), episodes: 1, role: String::new(), poster: p.poster.clone(), story: i as u32, can_add: true })
            .collect();
        out.push(Group { id, name: c.name, poster: c.poster, fanart: c.fanart, kind: "films".into(), members: members.iter().enumerate().map(|(i, t)| member(t, "", place(t.tmdb_id, i))).collect(), missing, next: None, ordered: false });
    }

    // Where the user has put a title: into one of these, into a collection of their own, or out.
    for (id, name) in &edits.own {
        out.push(Group { id: *id, name: name.clone(), kind: "own".into(), ..Default::default() });
    }
    for t in titles {
        let Some(g) = edits.placed.get(&t.id).filter(|g| **g != 0).and_then(|g| out.iter_mut().find(|x| x.id == *g)) else { continue };
        let last = g.members.iter().map(|m| m.story + 1).max().unwrap_or(0);
        g.members.push(member(t, "", last));
        g.members.sort_by_key(|m| (m.year, m.title_id));
    }
    for g in &mut out {
        if let Some(name) = edits.names.get(&g.id) {
            g.name = name.clone();
        }
        if g.poster.is_none() {
            g.poster = g.members.iter().find_map(|m| titles.iter().find(|t| t.id == m.title_id).and_then(|t| t.poster.clone()));
        }
        // The user's own order stands in for the story's. A title they have not placed,
        // one added since, keeps its place after those they have.
        if let Some(order) = edits.order.get(&g.id).filter(|o| !o.is_empty()) {
            for m in &mut g.members {
                m.story = order.iter().position(|id| *id == m.title_id).map(|i| i as u32).unwrap_or(order.len() as u32 + m.story);
            }
            g.ordered = true;
        }
        // Once something of it has been watched, what comes next in the story.
        if g.members.iter().any(|m| m.watched) {
            let mut in_order: Vec<&Member> = g.members.iter().collect();
            in_order.sort_by_key(|m| (m.story, m.year, m.title_id));
            g.next = in_order.iter().find(|m| !m.watched).map(|m| m.title_id);
        }
    }
    // One title is not a collection, unless the user made it one.
    out.retain(|g| g.members.len() >= 2 || g.kind == "own");
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
        let mut entries = entries;
        // The id list is not always exact for a film: it may tie the film's id to a set of
        // specials that merely belongs to the same story. If a work beside the entry carries
        // the film's own name and the entry does not, that work is the film.
        let mut unsure = false;
        if let (Kind::Movie, Some(listed)) = (t.kind, entries.first().and_then(|i| self.db.anime(*i).ok().flatten())) {
            use crate::names::plain;
            // The title itself is asked first, then its other names: those may hold a name
            // taken from the listed entry on an earlier reading, which would settle nothing.
            let own = vec![plain(&t.title)];
            let all: Vec<String> = std::iter::once(&t.title).chain(&t.alt_titles).map(|n| plain(n)).filter(|n| !n.is_empty()).collect();
            let named = |m: &Media, ours: &[String]| m.names.iter().any(|n| ours.contains(&plain(n)));
            if !named(&listed, &own) {
                let near: Vec<Media> = listed.edges.iter().filter_map(|e| self.db.anime(e.to).ok().flatten()).collect();
                let beside = near.iter().find(|m| named(m, &own)).or_else(|| if named(&listed, &all) { None } else { near.iter().find(|m| named(m, &all)) });
                match beside {
                    Some(m) => {
                        entries = vec![m.id];
                        t.alt_titles.retain(|a| !listed.names.iter().any(|n| n.eq_ignore_ascii_case(a)));
                    }
                    None if named(&listed, &all) => {}
                    // A name is the better witness; without one, the year and the length
                    // have to agree, or the entry is some other work of the story.
                    None if listed.year > 0 && t.year > 0 && listed.year.abs_diff(t.year) > 1 => unsure = true,
                    None if listed.format == "MOVIE" && listed.minutes > 0 && t.runtime > 0 && (listed.minutes * 10 < t.runtime * 5 || listed.minutes * 10 > t.runtime * 16) => unsure = true,
                    // Nothing settles which of the set it is: it is of this story, and no more is taken from the entry.
                    None => unsure = self.anime_ids().is_some_and(|ids| ids.back.get(&listed.id).is_some_and(|k| k.several)),
                }
            }
        }
        t.anilist = entries;
        // The names of the first entry only: a later season's name on a release numbered
        // from one would be read as the first season's episodes.
        if let Some(m) = t.anilist.first().and_then(|i| self.db.anime(*i).ok().flatten()).filter(|_| !unsure) {
            for name in &m.names {
                if !name.eq_ignore_ascii_case(&t.title) && !t.alt_titles.iter().any(|a| a.eq_ignore_ascii_case(name)) {
                    t.alt_titles.push(name.clone());
                }
            }
            // A "film" that is an OVA or a series: it is posted as its episodes.
            if t.kind == Kind::Movie && EPISODIC.contains(&m.format.as_str()) && m.episodes >= 2 {
                t.in_episodes = true;
            }
            // And a film that AniList too calls a film, with no series listed for it
            // anywhere, is not: this undoes a mark made from a wrong entry.
            if t.kind == Kind::Movie && m.format == "MOVIE" && m.episodes <= 1 && t.as_series.is_none() {
                t.in_episodes = false;
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
        let again = self.db.get_setting::<u32>("anime_rules") != RULES;
        let due: Vec<Title> = self.db.titles(None)?.into_iter().filter(|t| again || now() - t.anime_checked_at > TITLE_KEPT).collect();
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
        if !paused {
            self.db.set_setting("anime_rules", &RULES)?;
        }
        // Film collections with two films here are read whole, so what is missing can be shown.
        let key = self.settings.general().tmdb_api_key;
        let mut held: HashMap<u32, u32> = HashMap::new();
        for c in self.db.titles(Some(Kind::Movie))?.iter().filter_map(|t| t.collection.as_ref()) {
            *held.entry(c.id).or_default() += 1;
        }
        let fresh: HashSet<u32> = self.db.film_sets_since(now() - TITLE_KEPT)?.into_iter().collect();
        let mut sets = 0;
        for id in held.into_iter().filter(|(id, n)| *n >= 2 && !fresh.contains(id)).map(|(id, _)| id) {
            if key.is_empty() {
                break;
            }
            match self.meta.film_set(&key, id).await {
                Ok(set) => {
                    self.db.film_set_put(&set)?;
                    sets += 1;
                }
                Err(e) => tracing::debug!(collection = id, error = %e, "a film collection could not be read"),
            }
        }
        if sets > 0 {
            self.emit(Event::Title { id: 0 });
        }
        Ok(format!("{found} anime titles read from AniList, {plain} others checked, {sets} film collections read{}", if paused { "; the rest next time" } else { "" }))
    }

    /// The library's collections.
    pub fn collections(&self) -> Result<Vec<Group>> {
        self.collections_among(&self.db.titles(None)?)
    }

    /// The collections among these titles: the whole library, or what of it can be played.
    pub fn collections_among(&self, titles: &[Title]) -> Result<Vec<Group>> {
        let works = if titles.iter().any(|t| !t.anilist.is_empty()) { self.db.anime_all()? } else { vec![] };
        let ids = self.anime_ids();
        // A film that was played through; a series whose every episode on disk was.
        let seen = self.db.watched_all()?;
        let played = |kind: &str, id: i64| seen.get(&(kind.to_string(), id)).is_some_and(|w| w.played);
        let mut episodes: HashMap<i64, (u32, u32)> = HashMap::new();
        for e in self.db.all_episodes()?.iter().filter(|e| e.season > 0 && e.file_id.is_some()) {
            let count = episodes.entry(e.title_id).or_default();
            count.0 += 1;
            count.1 += played("episode", e.id) as u32;
        }
        let watched = titles.iter().filter(|t| if t.kind == Kind::Movie { played("movie", t.id) } else { episodes.get(&t.id).is_some_and(|c| c.0 > 0 && c.0 == c.1) }).map(|t| t.id).collect();
        Ok(groups(titles, &Sources { works: &works, ids: ids.as_deref(), sets: &self.db.film_sets()?, edits: self.db.get_setting("collection_edits"), watched }))
    }

    /// Change what the user has said about collections.
    pub fn edit_collections(&self, change: impl FnOnce(&mut Edits)) -> Result<()> {
        let mut edits: Edits = self.db.get_setting("collection_edits");
        change(&mut edits);
        self.db.set_setting("collection_edits", &edits)?;
        self.emit(Event::Title { id: 0 });
        Ok(())
    }

    /// Add a film a film collection lacks.
    pub async fn add_missing_film(&self, tmdb_id: u32) -> Result<Title> {
        let g = self.settings.general();
        self.add_title(Kind::Movie, Some(tmdb_id), None, self.profile_for_new(Kind::Movie, g.new_titles_location), true, true).await
    }

    /// What a search for something to add finds, films and series together. The film and
    /// series sources are asked, and AniList as well: it finds anime under the names those
    /// sources do not know, and its entries lead back to theirs by id. An OVA the film
    /// source lists as a film is offered as the series it is posted as.
    pub async fn discover(&self, query: &str) -> Result<Value> {
        let key = self.settings.general().tmdb_api_key;
        let direct = ["tt", "tmdb:", "tvdb:"].iter().any(|p| query.trim().starts_with(p));
        let ids = if direct { None } else { self.anime_ids() };
        let anime = async {
            match &ids {
                Some(_) => self.meta.anime_search(query).await.unwrap_or_default(),
                None => vec![],
            }
        };
        let (movies, series, anime) = tokio::join!(self.meta.search_movies(&key, query), self.meta.search_series(query), anime);
        let movies_error = movies.as_ref().err().map(|e| e.to_string());
        let (mut movies, mut series) = (movies.unwrap_or_default(), series.unwrap_or_default());
        // What AniList says of a result, by the result's id; and films that are posted as a series.
        let mut said: HashMap<(Kind, u32), Value> = HashMap::new();
        let mut asked = 0;
        for m in anime.iter().take(5) {
            let Some(known) = ids.as_ref().and_then(|i| i.back.get(&m.id)) else { continue };
            let note = json!({"format": m.format, "episodes": m.episodes, "name": m.title});
            let as_series = known.tvdb.filter(|_| !known.film && known.season >= 1 && m.format != "MOVIE");
            if let Some(tvdb) = as_series {
                if !series.iter().any(|s| s.tvdb_id == Some(tvdb)) && asked < 6 {
                    asked += 1;
                    if let Ok(found) = self.meta.series_listing_by_tvdb(tvdb).await {
                        series.push(found);
                    }
                }
                said.entry((Kind::Series, tvdb)).or_insert(note.clone());
            }
            // Its film listing, when it has one of its own.
            if let (Some(imdb), false, false) = (&known.imdb, known.several, key.is_empty()) {
                if asked >= 6 {
                    continue;
                }
                asked += 1;
                let Ok(Some(tmdb)) = self.meta.movie_by_imdb(&key, imdb).await else { continue };
                if !movies.iter().any(|f| f.tmdb_id == Some(tmdb)) && as_series.is_none() {
                    if let Ok(found) = self.meta.movie(&key, tmdb).await {
                        movies.push(found);
                    }
                }
                let mut note = note;
                note["as_series"] = json!(as_series.is_some() && m.episodes >= 2);
                said.entry((Kind::Movie, tmdb)).or_insert(note);
            }
        }
        let listed = |found: Vec<Title>, kind: Kind| -> Result<Vec<Value>> {
            let mut out = vec![];
            let mut seen = HashSet::new();
            for t in found {
                let id = if kind == Kind::Movie { t.tmdb_id } else { t.tvdb_id.or(t.tvmaze_id) };
                if !seen.insert((t.tmdb_id, t.tvmaze_id)) {
                    continue;
                }
                let mut v = serde_json::to_value(&t)?;
                v["library_id"] = json!(self.db.find_title(kind, t.tmdb_id, t.tvdb_id)?.map(|e| e.id));
                if let Some(note) = id.and_then(|i| said.get(&(kind, i))) {
                    v["anime"] = note.clone();
                }
                out.push(v);
            }
            Ok(out)
        };
        Ok(json!({"movies": listed(movies, Kind::Movie)?, "series": listed(series, Kind::Series)?, "movies_error": movies_error}))
    }

    /// Add a work that a collection lacks, by the ids the list has for it.
    pub async fn add_missing(&self, anilist_id: u32) -> Result<Title> {
        let ids = self.anime_ids().ok_or_else(|| anyhow!("the anime id list is not available"))?;
        let known = ids.back.get(&anilist_id).ok_or_else(|| anyhow!("the id list does not know this work"))?;
        let g = self.settings.general();
        if let Some(tvdb) = known.tvdb.filter(|_| !known.film && known.season >= 1) {
            let (listed, _) = self.meta.series_by_tvdb(tvdb).await?;
            let tvmaze = listed.tvmaze_id.ok_or_else(|| anyhow!("the series source does not list it"))?;
            return self.add_title(Kind::Series, None, Some(tvmaze), self.profile_for_new(Kind::Series, g.new_titles_location), true, true).await;
        }
        let imdb = known.imdb.clone().ok_or_else(|| anyhow!("no film or series id is known for this work"))?;
        let tmdb = self.meta.movie_by_imdb(&g.tmdb_api_key, &imdb).await?.ok_or_else(|| anyhow!("the film source does not list it"))?;
        self.add_title(Kind::Movie, Some(tmdb), None, self.profile_for_new(Kind::Movie, g.new_titles_location), true, true).await
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
        let found = groups(&titles, &Sources { works: &works, ..Default::default() });
        assert_eq!(found.len(), 1, "{found:?}");
        let g = &found[0];
        assert_eq!((g.name.as_str(), g.kind.as_str(), g.id), ("Example Genesis", "anime", ANIME_GROUP | 30));
        assert_eq!(g.members.iter().map(|m| (m.title_id, m.role.as_str())).collect::<Vec<_>>(), [(1, ""), (2, "Recap"), (3, "Alternative telling")]);
        // The sequel film and the second retelling are tied to what is here, and are not here.
        assert_eq!(g.missing.iter().map(|m| m.anilist_id).collect::<Vec<_>>(), [32, 3784]);
    }

    #[test]
    fn a_story_is_followed_in_its_own_order_and_the_user_has_the_last_word() {
        // A prequel film made after the series it comes before, a recap, and a sequel.
        let works = [
            work(1, "TV", 1988, "Heroes", &[("PREQUEL", 2), ("SUMMARY", 3), ("SEQUEL", 4)]),
            work(2, "MOVIE", 1993, "Heroes: Overture", &[("SEQUEL", 1)]),
            work(3, "MOVIE", 1990, "Heroes: The Recap", &[("PARENT", 1)]),
            work(4, "TV", 1998, "Heroes: After", &[("PREQUEL", 1)]),
        ];
        let titles = [title(10, "series", "Heroes", 1988, &[1]), title(11, "movie", "Heroes: Overture", 1993, &[2]), title(12, "movie", "Heroes: The Recap", 1990, &[3]), title(13, "movie", "Heat", 1995, &[])];
        let story = |g: &Group| {
            let mut m: Vec<&Member> = g.members.iter().collect();
            m.sort_by_key(|m| m.story);
            m.iter().map(|m| m.title_id).collect::<Vec<_>>()
        };
        let found = groups(&titles, &Sources { works: &works, watched: HashSet::from([11]), ..Default::default() });
        let g = &found[0];
        assert_eq!(g.members.iter().map(|m| m.title_id).collect::<Vec<_>>(), [10, 12, 11], "by release");
        assert_eq!(story(g), [11, 10, 12], "the prequel first, then the series, then its recap");
        assert_eq!(g.next, Some(10), "the prequel was watched; the series is next");
        assert_eq!(g.missing.iter().map(|m| (m.anilist_id, m.story)).collect::<Vec<_>>(), [(4, 3)]);

        // The user renames it, calls the recap a side story, takes the prequel out and puts
        // an unrelated film in a collection of their own.
        let mut edits = Edits::default();
        edits.names.insert(g.id, "My Heroes".into());
        edits.roles.insert(12, "Side story".into());
        edits.placed.insert(11, 0);
        edits.own.push((OWN_GROUP | 1, "Rainy days".into()));
        edits.placed.insert(13, OWN_GROUP | 1);
        let found = groups(&titles, &Sources { works: &works, edits, ..Default::default() });
        let named: Vec<(&str, &str, Vec<i64>)> = found.iter().map(|g| (g.name.as_str(), g.kind.as_str(), g.members.iter().map(|m| m.title_id).collect())).collect();
        assert_eq!(named, [("My Heroes", "anime", vec![10, 12]), ("Rainy days", "own", vec![13])]);
        assert_eq!(found[0].members[1].role, "Side story");

        // And puts the titles in an order of their own, which is then the one followed.
        let mut edits = Edits::default();
        edits.order.insert(g.id, vec![12, 10]);
        let found = groups(&titles, &Sources { works: &works, edits, watched: HashSet::from([12]), ..Default::default() });
        assert!(found[0].ordered);
        assert_eq!(story(&found[0]), [12, 10, 11], "theirs first, then what they did not place");
        assert_eq!(found[0].next, Some(10));
    }

    #[test]
    fn a_film_collection_lists_the_films_that_are_not_here() {
        let film = |id: i64, tmdb: u32, name: &str, year: u32| {
            let mut t = title(id, "movie", name, year, &[]);
            t.tmdb_id = Some(tmdb);
            t.collection = Some(crate::models::Collection { id: 500, name: "Saga Collection".into(), poster: None, fanart: None });
            t
        };
        let titles = [film(1, 301, "Saga One", 2001), film(2, 303, "Saga Three", 2005)];
        let part = |tmdb_id: u32, name: &str, year: u32| FilmPart { tmdb_id, title: name.into(), year, poster: None };
        let sets = [FilmSet { id: 500, name: "Saga Collection".into(), parts: vec![part(303, "Saga Three", 2005), part(301, "Saga One", 2001), part(302, "Saga Two", 2003), part(304, "Saga Four", 0)] }];
        let found = groups(&titles, &Sources { sets: &sets, ..Default::default() });
        assert_eq!(found[0].missing.iter().map(|m| (m.tmdb_id, m.title.as_str(), m.can_add)).collect::<Vec<_>>(), [(Some(302), "Saga Two", true), (Some(304), "Saga Four", true)]);
        // Each in its place among all four.
        assert_eq!(found[0].members.iter().map(|m| m.story).collect::<Vec<_>>(), [0, 2]);
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
