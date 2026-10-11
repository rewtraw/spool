//! Spool's library in the shape Jellyfin's API gives items.

use super::ids::Id;
use super::Params;
use crate::app::App;
use crate::models::*;
use anyhow::Result;
use serde_json::{json, Map, Value};
use spool_core::mediainfo::{MediaInfo, Stream};
use std::collections::{HashMap, HashSet};

/// Jellyfin counts time in ticks of 100 nanoseconds.
pub const TICKS: f64 = 10_000_000.0;

/// Everything a player can be shown, read from the database in one go: the titles that have
/// files, their files, and what has been watched.
pub struct Library {
    pub server_id: String,
    pub titles: Vec<Title>,
    index: HashMap<i64, usize>,
    /// Per title, the main file first.
    files: HashMap<i64, Vec<MediaFile>>,
    /// Per series, the episodes that have a file, in order.
    episodes: HashMap<i64, Vec<Episode>>,
    episode_title: HashMap<i64, i64>,
    watched: HashMap<(String, i64), Watched>,
    /// Collections with more than one title here: a film series, or an anime's series and films.
    pub sets: Vec<Collection>,
    /// Per collection, its titles in the order they came out.
    set_members: HashMap<u32, Vec<i64>>,
}

#[derive(Clone, Copy)]
pub enum Node<'a> {
    View(Kind),
    Movie(&'a Title),
    Series(&'a Title),
    Season(&'a Title, u32),
    Episode(&'a Title, &'a Episode),
    Set(&'a Collection),
}

/// Where a title or an episode stands with whoever is watching.
#[derive(Default)]
pub struct State {
    pub played: bool,
    pub position: f64,
    pub play_count: u32,
    pub favorite: bool,
    pub last_played: i64,
    /// For a series or a season: how many of its episodes are still to watch.
    pub unplayed: Option<u32>,
}

/// Which of the costly parts of an item a request wants. A single item is always given whole;
/// a list carries them only when asked.
pub struct Fields {
    all: bool,
    named: HashSet<String>,
}

impl Fields {
    pub fn all() -> Fields {
        Fields { all: true, named: HashSet::new() }
    }
    pub fn from(q: &Params) -> Fields {
        Fields { all: false, named: q.list("fields").into_iter().collect() }
    }
    fn has(&self, name: &str) -> bool {
        self.all || self.named.contains(name)
    }
}

pub fn iso(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default().format("%Y-%m-%dT%H:%M:%S.0000000Z").to_string()
}

/// "2019-03-08" or a full timestamp, as Jellyfin writes dates.
fn iso_date(text: &str) -> Option<String> {
    let day = text.get(..10)?;
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    Some(format!("{day}T00:00:00.0000000Z"))
}

fn ticks(secs: f64) -> i64 {
    (secs * TICKS) as i64
}

fn tag_of(src: &Option<String>) -> Option<String> {
    src.as_deref().map(|s| crate::art::tag(s)[..10].to_string())
}

fn season_name(n: u32) -> String {
    if n == 0 {
        "Specials".into()
    } else {
        format!("Season {n}")
    }
}

fn container(f: &MediaFile) -> String {
    f.rel_path.rsplit_once('.').map(|(_, ext)| ext.to_lowercase()).unwrap_or_default()
}

impl Library {
    /// The library as it stands. Reading it takes a moment and a player asks many things at
    /// once, so one reading serves every request until the library or what has been watched changes,
    /// or a location comes or goes.
    pub fn shared(app: &App) -> Result<std::sync::Arc<Library>> {
        // Read the count before the library: a change that lands meanwhile then shows as a
        // newer count next time, and the library is read again.
        let revision = (app.db.revision()?, app.location_changes.load(std::sync::atomic::Ordering::Relaxed));
        if let Some((at, lib)) = app.player_library.lock().as_ref() {
            if *at == revision {
                return Ok(lib.clone());
            }
        }
        let lib = std::sync::Arc::new(Library::load(app)?);
        *app.player_library.lock() = Some((revision, lib.clone()));
        Ok(lib)
    }

    fn load(app: &App) -> Result<Library> {
        let mut files: HashMap<i64, Vec<MediaFile>> = HashMap::new();
        for f in app.db.all_files()? {
            files.entry(f.title_id).or_default().push(f);
        }
        for list in files.values_mut() {
            list.sort_by_key(|f| f.extra);
        }
        let mut episodes: HashMap<i64, Vec<Episode>> = HashMap::new();
        let mut episode_title = HashMap::new();
        for e in app.db.all_episodes()? {
            if e.file_id.is_some_and(|id| files.get(&e.title_id).is_some_and(|l| l.iter().any(|f| f.id == id))) {
                episode_title.insert(e.id, e.title_id);
                episodes.entry(e.title_id).or_default().push(e);
            }
        }
        // Only what can be played now: titles on a location that is away are left out until it is back.
        let places = app.placement();
        let titles: Vec<Title> = app
            .db
            .titles(None)?
            .into_iter()
            .filter(|t| places.online(t))
            .filter(|t| match t.kind {
                Kind::Movie => files.contains_key(&t.id),
                Kind::Series => episodes.contains_key(&t.id),
            })
            .collect();
        // The same collections the web app shows, among what can be played.
        let groups = app.collections_among(&titles)?;
        // In order of release, or in the user's own order where they have set one.
        let set_members = groups
            .iter()
            .map(|g| {
                let mut members: Vec<&crate::anime::Member> = g.members.iter().collect();
                if g.ordered {
                    members.sort_by_key(|m| m.story);
                }
                (g.id, members.into_iter().map(|m| m.title_id).collect())
            })
            .collect();
        let sets = groups.into_iter().map(|g| Collection { id: g.id, name: g.name, poster: g.poster, fanart: g.fanart }).collect();
        let index = titles.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        Ok(Library { server_id: super::server_id(app), titles, index, files, episodes, episode_title, watched: app.db.watched_all()?, sets, set_members })
    }

    /// The titles whose posters stand for a whole view, Movies or Shows: the five most lately
    /// added that have one. With them, a tag that changes when the choice does.
    pub fn view_posters(&self, kind: Kind) -> (Vec<i64>, String) {
        let mut with: Vec<&Title> = self.titles.iter().filter(|t| t.kind == kind && t.poster_source().is_some()).collect();
        with.sort_by_key(|t| std::cmp::Reverse((t.added_at, t.id)));
        with.truncate(5);
        let said: String = with.iter().map(|t| format!("{}:{}|", t.id, t.poster_source().unwrap_or_default())).collect();
        (with.iter().map(|t| t.id).collect(), crate::art::tag(&said)[..10].to_string())
    }

    fn title(&self, id: i64) -> Option<&Title> {
        self.index.get(&id).map(|i| &self.titles[*i])
    }

    pub fn files_of(&self, title_id: i64) -> &[MediaFile] {
        self.files.get(&title_id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn episodes_of(&self, title_id: i64) -> &[Episode] {
        self.episodes.get(&title_id).map(Vec::as_slice).unwrap_or(&[])
    }

    fn episode_file(&self, e: &Episode) -> Option<&MediaFile> {
        self.files_of(e.title_id).iter().find(|f| Some(f.id) == e.file_id)
    }

    fn movies(&self) -> impl Iterator<Item = Node<'_>> {
        self.titles.iter().filter(|t| t.kind == Kind::Movie).map(Node::Movie)
    }

    fn series(&self) -> impl Iterator<Item = Node<'_>> {
        self.titles.iter().filter(|t| t.kind == Kind::Series).map(Node::Series)
    }

    fn seasons<'a>(&'a self, t: &'a Title) -> Vec<Node<'a>> {
        let mut numbers: Vec<u32> = self.episodes_of(t.id).iter().map(|e| e.season).collect();
        numbers.dedup();
        // Specials sit after the seasons proper, as players expect.
        numbers.sort_by_key(|n| (*n == 0, *n));
        numbers.into_iter().map(|n| Node::Season(t, n)).collect()
    }

    fn season_episodes<'a>(&'a self, t: &'a Title, season: Option<u32>) -> Vec<Node<'a>> {
        let mut list: Vec<&Episode> = self.episodes_of(t.id).iter().filter(|e| season.is_none_or(|s| e.season == s)).collect();
        list.sort_by_key(|e| (e.season == 0, e.season, e.episode));
        list.into_iter().map(|e| Node::Episode(t, e)).collect()
    }

    pub fn node(&self, id: Id) -> Option<Node<'_>> {
        match id {
            Id::Movies => Some(Node::View(Kind::Movie)),
            Id::Shows => Some(Node::View(Kind::Series)),
            Id::Movie(i) => self.title(i).filter(|t| t.kind == Kind::Movie).map(Node::Movie),
            Id::Series(i) => self.title(i).filter(|t| t.kind == Kind::Series).map(Node::Series),
            Id::Season(i, n) => self.title(i).filter(|t| self.episodes_of(t.id).iter().any(|e| e.season == n)).map(|t| Node::Season(t, n)),
            Id::Episode(i) => {
                let t = self.title(*self.episode_title.get(&i)?)?;
                self.episodes_of(t.id).iter().find(|e| e.id == i).map(|e| Node::Episode(t, e))
            }
            Id::File(i) => {
                let (t, f) = self.titles.iter().find_map(|t| self.files_of(t.id).iter().find(|f| f.id == i).map(|f| (t, f)))?;
                match t.kind {
                    Kind::Movie => Some(Node::Movie(t)),
                    Kind::Series => self.episodes_of(t.id).iter().find(|e| e.file_id == Some(f.id)).map(|e| Node::Episode(t, e)),
                }
            }
            Id::Collection(i) => self.sets.iter().find(|s| s.id == i).map(Node::Set),
            _ => None,
        }
    }

    /// The file a player means by an item id and, for a film with several versions, the
    /// version it chose.
    pub fn file_for(&self, item: Id, source: Option<Id>) -> Option<(&Title, &MediaFile)> {
        for id in [source, Some(item)].into_iter().flatten() {
            if let Id::File(i) = id {
                if let Some(found) = self.titles.iter().find_map(|t| self.files_of(t.id).iter().find(|f| f.id == i).map(|f| (t, f))) {
                    return Some(found);
                }
            }
        }
        match self.node(item)? {
            Node::Movie(t) => self.files_of(t.id).first().map(|f| (t, f)),
            Node::Episode(t, e) => self.episode_file(e).map(|f| (t, f)),
            _ => None,
        }
    }

    // ------------------------------------------------------------ what an item is

    pub fn id(&self, n: &Node) -> Id {
        match n {
            Node::View(Kind::Movie) => Id::Movies,
            Node::View(Kind::Series) => Id::Shows,
            Node::Movie(t) => Id::Movie(t.id),
            Node::Series(t) => Id::Series(t.id),
            Node::Season(t, s) => Id::Season(t.id, *s),
            Node::Episode(_, e) => Id::Episode(e.id),
            Node::Set(c) => Id::Collection(c.id),
        }
    }

    fn kind_name(n: &Node) -> &'static str {
        match n {
            Node::View(_) => "collectionfolder",
            Node::Movie(_) => "movie",
            Node::Series(_) => "series",
            Node::Season(..) => "season",
            Node::Episode(..) => "episode",
            Node::Set(_) => "boxset",
        }
    }

    fn name(n: &Node) -> String {
        match n {
            Node::View(Kind::Movie) => "Movies".into(),
            Node::View(Kind::Series) => "Shows".into(),
            Node::Movie(t) | Node::Series(t) => t.title.clone(),
            Node::Season(_, s) => season_name(*s),
            Node::Episode(_, e) if e.title.is_empty() => format!("Episode {}", e.episode),
            Node::Episode(_, e) => e.title.clone(),
            Node::Set(c) => c.name.clone(),
        }
    }

    fn sort_name(n: &Node) -> String {
        match n {
            Node::Movie(t) | Node::Series(t) => t.sort_title.clone(),
            Node::Season(_, s) => format!("{s:04}"),
            Node::Episode(_, e) => format!("{:04}{:04}", e.season, e.episode),
            other => crate::store::sort_title(&Self::name(other)),
        }
    }

    fn runtime_secs(&self, n: &Node) -> f64 {
        let probed = |f: Option<&MediaFile>| f.and_then(|f| f.media_info.as_ref()).map(|m| m.runtime_secs).filter(|s| *s > 0.0);
        match n {
            Node::Movie(t) => probed(self.files_of(t.id).first()).unwrap_or(t.runtime as f64 * 60.0),
            Node::Series(t) => t.runtime as f64 * 60.0,
            Node::Episode(t, e) => probed(self.episode_file(e)).unwrap_or(if e.runtime > 0 { e.runtime } else { t.runtime } as f64 * 60.0),
            _ => 0.0,
        }
    }

    /// When the item arrived: for a film or an episode, when its file did.
    fn added(&self, n: &Node) -> i64 {
        let newest = |files: &[MediaFile]| files.iter().map(|f| f.added_at).max().unwrap_or(0);
        match n {
            Node::Movie(t) => newest(self.files_of(t.id)).max(t.added_at),
            Node::Series(t) => t.added_at,
            Node::Season(t, s) => self.episodes_of(t.id).iter().filter(|e| e.season == *s).filter_map(|e| self.episode_file(e)).map(|f| f.added_at).min().unwrap_or(t.added_at),
            Node::Episode(t, e) => self.episode_file(e).map(|f| f.added_at).unwrap_or(t.added_at),
            _ => 0,
        }
    }

    fn premiere(&self, n: &Node) -> Option<String> {
        match n {
            Node::Movie(t) => [&t.in_cinemas, &t.digital_release, &t.physical_release].into_iter().flatten().min().cloned(),
            Node::Series(t) => t.first_aired.clone(),
            Node::Season(t, s) => self.episodes_of(t.id).iter().filter(|e| e.season == *s).filter_map(|e| e.air_date.clone()).min(),
            Node::Episode(_, e) => e.air_date.clone(),
            _ => None,
        }
    }

    fn year(&self, n: &Node) -> u32 {
        match n {
            Node::Movie(t) | Node::Series(t) => t.year,
            other => self.premiere(other).and_then(|d| d.get(..4).and_then(|y| y.parse().ok())).unwrap_or(0),
        }
    }

    fn rating(n: &Node) -> Option<f32> {
        match n {
            Node::Movie(t) | Node::Series(t) => t.rating,
            Node::Episode(_, e) => e.rating,
            _ => None,
        }
    }

    fn own(&self, kind: &str, id: i64) -> Watched {
        self.watched.get(&(kind.to_string(), id)).cloned().unwrap_or_default()
    }

    pub fn state(&self, n: &Node) -> State {
        let of = |w: Watched| State { played: w.played, position: w.position, play_count: w.play_count, favorite: w.favorite, last_played: w.last_played_at, unplayed: None };
        let over = |episodes: Vec<&Episode>, favorite: bool| {
            let seen: Vec<Watched> = episodes.iter().map(|e| self.own("episode", e.id)).collect();
            let unplayed = seen.iter().filter(|w| !w.played).count() as u32;
            State { played: unplayed == 0 && !seen.is_empty(), position: 0.0, play_count: 0, favorite, last_played: seen.iter().map(|w| w.last_played_at).max().unwrap_or(0), unplayed: Some(unplayed) }
        };
        match n {
            Node::Movie(t) => of(self.own("movie", t.id)),
            Node::Episode(_, e) => of(self.own("episode", e.id)),
            Node::Series(t) => over(self.episodes_of(t.id).iter().filter(|e| e.season > 0).collect(), self.own("series", t.id).favorite),
            Node::Season(t, s) => over(self.episodes_of(t.id).iter().filter(|e| e.season == *s).collect(), false),
            _ => State::default(),
        }
    }

    pub fn user_data(&self, n: &Node) -> Value {
        let s = self.state(n);
        let id = self.id(n).to_string();
        let mut u = json!({"PlaybackPositionTicks": ticks(s.position), "PlayCount": s.play_count, "IsFavorite": s.favorite, "Played": s.played, "Key": id, "ItemId": id});
        if s.last_played > 0 {
            u["LastPlayedDate"] = json!(iso(s.last_played));
        }
        let runtime = self.runtime_secs(n);
        if s.position > 0.0 && runtime > 0.0 {
            u["PlayedPercentage"] = json!((s.position / runtime * 100.0).min(100.0));
        }
        if let Some(left) = s.unplayed {
            u["UnplayedItemCount"] = json!(left);
        }
        u
    }

    fn genres<'a>(n: &Node<'a>) -> &'a [String] {
        match n {
            Node::Movie(t) | Node::Series(t) | Node::Season(t, _) | Node::Episode(t, _) => &t.genres,
            _ => &[],
        }
    }

    fn studio<'a>(n: &Node<'a>) -> Option<&'a str> {
        match n {
            Node::Movie(t) => t.studio.as_deref(),
            Node::Series(t) | Node::Season(t, _) | Node::Episode(t, _) => t.network.as_deref(),
            _ => None,
        }
    }

    // ------------------------------------------------------------ items as JSON

    pub fn json(&self, n: &Node, fields: &Fields) -> Value {
        let id = self.id(n);
        let mut o = Map::new();
        macro_rules! put {
            ($k:expr, $v:expr $(,)?) => {
                o.insert($k.to_string(), $v);
            };
        }
        put!("Name", json!(Self::name(n)));
        put!("ServerId", json!(self.server_id));
        put!("Id", json!(id.to_string()));
        put!("SortName", json!(Self::sort_name(n)));
        put!("UserData", self.user_data(n));
        put!("ImageBlurHashes", json!({}));
        put!("LocationType", json!("FileSystem"));
        put!("CanDelete", json!(false));
        let added = self.added(n);
        if added > 0 {
            put!("DateCreated", json!(iso(added)));
            put!("Etag", json!(format!("{added:x}")));
        }
        if let Some(d) = self.premiere(n).as_deref().and_then(iso_date) {
            put!("PremiereDate", json!(d));
        }
        if self.year(n) > 0 {
            put!("ProductionYear", json!(self.year(n)));
        }
        if let Some(r) = Self::rating(n) {
            put!("CommunityRating", json!(r));
        }
        let runtime = self.runtime_secs(n);
        if runtime > 0.0 {
            put!("RunTimeTicks", json!(ticks(runtime)));
        }
        if fields.has("genres") {
            put!("Genres", json!(Self::genres(n)));
            put!("GenreItems", json!(Self::genres(n).iter().map(|g| json!({"Name": g, "Id": Id::Genre(Id::named(g)).to_string()})).collect::<Vec<_>>()));
        }
        if fields.has("studios") {
            put!("Studios", json!(Self::studio(n).map(|s| json!({"Name": s, "Id": Id::Studio(Id::named(s)).to_string()})).into_iter().collect::<Vec<_>>()));
        }
        match n {
            Node::View(kind) => {
                put!("Type", json!("CollectionFolder"));
                put!("CollectionType", json!(if *kind == Kind::Movie { "movies" } else { "tvshows" }));
                put!("IsFolder", json!(true));
                put!("ParentId", json!(Id::Root.to_string()));
                put!("ChildCount", json!(self.titles.iter().filter(|t| t.kind == *kind).count()));
                put!("DisplayPreferencesId", json!(id.to_string()));
                put!("PlayAccess", json!("Full"));
                // A picture made of the posters most lately added to it. See `view_posters`.
                let picture = self.view_posters(*kind);
                put!("ImageTags", if picture.0.is_empty() { json!({}) } else { json!({"Primary": picture.1}) });
                put!("PrimaryImageAspectRatio", json!(1.7778));
                put!("BackdropImageTags", json!([]));
                put!("MediaType", json!("Unknown"));
                put!("CanDownload", json!(false));
            }
            Node::Movie(t) | Node::Series(t) => {
                let movie = t.kind == Kind::Movie;
                put!("Type", json!(if movie { "Movie" } else { "Series" }));
                put!("IsFolder", json!(!movie));
                put!("ParentId", json!(if movie { Id::Movies } else { Id::Shows }.to_string()));
                put!("ImageTags", tag_of(&t.poster_source()).map(|p| json!({"Primary": p})).unwrap_or(json!({})));
                put!("BackdropImageTags", json!(tag_of(&t.fanart_source()).into_iter().collect::<Vec<_>>()));
                put!("LocalTrailerCount", json!(t.local.trailers().count()));
                put!("SpecialFeatureCount", json!(t.local.features().count()));
                put!("PrimaryImageAspectRatio", json!(0.6667));
                put!("CanDownload", json!(movie));
                if let Some(c) = &t.certification {
                    put!("OfficialRating", json!(c));
                }
                if fields.has("overview") {
                    put!("Overview", json!(t.overview));
                }
                if fields.has("taglines") {
                    put!("Taglines", json!(if t.tagline.is_empty() { vec![] } else { vec![t.tagline.clone()] }));
                }
                if fields.has("providerids") {
                    let mut ids = Map::new();
                    let mut add = |k: &str, v: Option<String>| {
                        if let Some(v) = v {
                            ids.insert(k.to_string(), json!(v));
                        }
                    };
                    add("Tmdb", t.tmdb_id.map(|i| i.to_string()));
                    add("Imdb", t.imdb_id.clone());
                    add("Tvdb", t.tvdb_id.map(|i| i.to_string()));
                    add("TvMaze", t.tvmaze_id.map(|i| i.to_string()));
                    put!("ProviderIds", Value::Object(ids));
                }
                if fields.has("people") {
                    let source = if movie { 1 } else { 2 };
                    put!(
                        "People",
                        json!(t
                            .people
                            .iter()
                            .map(|p| {
                                let mut person = json!({"Name": p.name, "Id": Id::Person(source, p.id).to_string(), "Role": p.role, "Type": match p.kind.as_str() { "director" => "Director", "writer" => "Writer", _ => "Actor" }});
                                if let Some(tag) = tag_of(&p.photo) {
                                    person["PrimaryImageTag"] = json!(tag);
                                }
                                person
                            })
                            .collect::<Vec<_>>()),
                    );
                }
                if fields.has("path") {
                    put!("Path", json!(match self.files_of(t.id).first().filter(|_| movie) {
                        Some(f) => format!("{}/{}", t.path, f.rel_path),
                        None => t.path.clone(),
                    }));
                }
                if movie {
                    put!("MediaType", json!("Video"));
                    put!("VideoType", json!("VideoFile"));
                    self.put_file_facts(&mut o, id, t, self.files_of(t.id), runtime, fields);
                } else {
                    let episodes = self.episodes_of(t.id);
                    put!("MediaType", json!("Unknown"));
                    put!("Status", json!(match t.status.as_str() { "ended" => "Ended", "upcoming" => "Unreleased", _ => "Continuing" }));
                    put!("ChildCount", json!(self.seasons(t).len()));
                    put!("RecursiveItemCount", json!(episodes.len()));
                    put!("AirDays", json!([]));
                    if let Some(last) = episodes.iter().filter_map(|e| self.episode_file(e)).map(|f| f.added_at).max() {
                        put!("DateLastMediaAdded", json!(iso(last)));
                    }
                }
            }
            Node::Season(t, s) => {
                put!("Type", json!("Season"));
                put!("IsFolder", json!(true));
                put!("IndexNumber", json!(s));
                put!("ParentId", json!(Id::Series(t.id).to_string()));
                put!("SeriesId", json!(Id::Series(t.id).to_string()));
                put!("SeriesName", json!(t.title));
                put!("ChildCount", json!(self.episodes_of(t.id).iter().filter(|e| e.season == *s).count()));
                put!("ImageTags", tag_of(&self.season_poster(t, *s).or_else(|| t.poster_source())).map(|p| json!({"Primary": p})).unwrap_or(json!({})));
                put!("BackdropImageTags", json!([]));
                put!("PrimaryImageAspectRatio", json!(0.6667));
                put!("MediaType", json!("Unknown"));
                put!("CanDownload", json!(false));
                self.put_series_art(&mut o, t);
            }
            Node::Episode(t, e) => {
                put!("Type", json!("Episode"));
                put!("IsFolder", json!(false));
                put!("IndexNumber", json!(e.episode));
                put!("ParentIndexNumber", json!(e.season));
                put!("ParentId", json!(Id::Season(t.id, e.season).to_string()));
                put!("SeasonId", json!(Id::Season(t.id, e.season).to_string()));
                put!("SeasonName", json!(season_name(e.season)));
                put!("SeriesId", json!(Id::Series(t.id).to_string()));
                put!("SeriesName", json!(t.title));
                // An episode with no frame of its own borrows the show's wide image.
                put!("ImageTags", tag_of(&self.episode_still(t, e)).or_else(|| tag_of(&t.fanart_source())).map(|p| json!({"Primary": p})).unwrap_or(json!({})));
                put!("BackdropImageTags", json!([]));
                put!("PrimaryImageAspectRatio", json!(1.7778));
                put!("MediaType", json!("Video"));
                put!("VideoType", json!("VideoFile"));
                put!("CanDownload", json!(true));
                if let Some(c) = &t.certification {
                    put!("OfficialRating", json!(c));
                }
                if fields.has("overview") {
                    put!("Overview", json!(e.overview));
                }
                if fields.has("path") {
                    if let Some(f) = self.episode_file(e) {
                        put!("Path", json!(format!("{}/{}", t.path, f.rel_path)));
                    }
                }
                self.put_series_art(&mut o, t);
                self.put_file_facts(&mut o, id, t, self.episode_file(e).map(std::slice::from_ref).unwrap_or(&[]), runtime, fields);
            }
            Node::Set(c) => {
                put!("Type", json!("BoxSet"));
                put!("IsFolder", json!(true));
                put!("ParentId", json!(Id::Movies.to_string()));
                put!("ChildCount", json!(self.set_members.get(&c.id).map(|m| m.len()).unwrap_or(0)));
                put!("ImageTags", tag_of(&c.poster).map(|p| json!({"Primary": p})).unwrap_or(json!({})));
                put!("BackdropImageTags", json!(tag_of(&c.fanart).into_iter().collect::<Vec<_>>()));
                put!("PrimaryImageAspectRatio", json!(0.6667));
                put!("MediaType", json!("Unknown"));
                put!("CanDownload", json!(false));
            }
        }
        Value::Object(o)
    }

    fn put_series_art(&self, o: &mut Map<String, Value>, t: &Title) {
        if let Some(p) = tag_of(&t.poster_source()) {
            o.insert("SeriesPrimaryImageTag".into(), json!(p));
        }
        if let Some(b) = tag_of(&t.fanart_source()) {
            o.insert("ParentBackdropItemId".into(), json!(Id::Series(t.id).to_string()));
            o.insert("ParentBackdropImageTags".into(), json!([b]));
        }
    }

    /// The parts of a film or an episode that come from its file.
    fn put_file_facts(&self, o: &mut Map<String, Value>, item: Id, t: &Title, files: &[MediaFile], runtime: f64, fields: &Fields) {
        let Some(main) = files.first() else { return };
        o.insert("Container".into(), json!(container(main)));
        o.insert("HasSubtitles".into(), json!(!main.subtitles.is_empty() || main.media_info.as_ref().is_some_and(|m| !m.subtitles.is_empty() || m.streams.iter().any(|s| s.kind == "subtitle"))));
        if let Some(m) = &main.media_info {
            if m.width > 0 {
                o.insert("Width".into(), json!(m.width));
                o.insert("Height".into(), json!(m.height));
                o.insert("IsHD".into(), json!(m.width >= 1200 || m.height >= 700));
            }
        }
        if fields.has("mediasources") || fields.has("mediastreams") {
            let sources = self.sources(item, t, files, runtime);
            if fields.has("mediastreams") {
                o.insert("MediaStreams".into(), sources[0]["MediaStreams"].clone());
            }
            if fields.has("mediasources") {
                o.insert("MediaSources".into(), json!(sources));
            }
        }
        if fields.has("chapters") {
            o.insert("Chapters".into(), json!([]));
        }
    }

    /// What a player can choose to play for an item: one entry per file. The first carries the
    /// item's own id, as Jellyfin's does; further versions of a film carry their file's.
    pub fn sources(&self, item: Id, t: &Title, files: &[MediaFile], runtime: f64) -> Vec<Value> {
        files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let id = if i == 0 { item } else { Id::File(f.id) };
                let info = f.media_info.clone().unwrap_or_default();
                let mut streams = media_streams(&info);
                let embedded = streams.len();
                for (n, sub) in f.subtitles.iter().enumerate() {
                    let index = embedded + n;
                    let language = crate::subs::language_name(&sub.language);
                    let mut title = language.clone();
                    if sub.forced {
                        title.push_str(" - Forced");
                    }
                    if sub.hearing_impaired {
                        title.push_str(" - SDH");
                    }
                    streams.push(json!({
                        "Index": index, "Type": "Subtitle", "Codec": "subrip", "Language": three_letter(&sub.language),
                        "DisplayTitle": format!("{title} - External"), "Title": title, "IsDefault": false, "IsForced": sub.forced, "IsHearingImpaired": sub.hearing_impaired,
                        "IsExternal": true, "IsTextSubtitleStream": true, "SupportsExternalStream": true, "IsInterlaced": false,
                        "DeliveryMethod": "External", "DeliveryUrl": format!("/Videos/{item}/{id}/Subtitles/{index}/0/Stream.srt"),
                    }));
                }
                let audio = streams.iter().position(|s| s["Type"] == "Audio" && s["IsDefault"] == true).or_else(|| streams.iter().position(|s| s["Type"] == "Audio"));
                let secs = if info.runtime_secs > 0.0 { info.runtime_secs } else { runtime };
                let mut source = json!({
                    "Protocol": "File", "Id": id.to_string(), "Path": format!("{}/{}", t.path, f.rel_path), "Type": "Default", "Container": container(f),
                    "Size": f.size, "Name": version_name(f), "IsRemote": false, "ETag": format!("{:x}{:x}", f.id, f.size), "RunTimeTicks": ticks(secs),
                    "ReadAtNativeFramerate": false, "IgnoreDts": false, "IgnoreIndex": false, "GenPtsInput": false,
                    "SupportsTranscoding": false, "SupportsDirectStream": true, "SupportsDirectPlay": true, "IsInfiniteStream": false,
                    "RequiresOpening": false, "RequiresClosing": false, "RequiresLooping": false, "SupportsProbing": true, "HasSegments": false,
                    "VideoType": "VideoFile", "MediaStreams": streams, "MediaAttachments": [], "Formats": [], "RequiredHttpHeaders": {},
                    "DefaultSubtitleStreamIndex": -1,
                });
                let rate = if info.bit_rate > 0 { info.bit_rate } else if secs > 0.0 { (f.size as f64 * 8.0 / secs) as u64 } else { 0 };
                if rate > 0 {
                    source["Bitrate"] = json!(rate);
                }
                if let Some(a) = audio {
                    source["DefaultAudioStreamIndex"] = json!(a);
                }
                source
            })
            .collect()
    }

    // ------------------------------------------------------------ lists

    /// Answer Jellyfin's item query: what is under a parent, narrowed, ordered and paged.
    /// Returns the page and how many there were before paging.
    pub fn list(&self, q: &Params) -> (Vec<Node<'_>>, usize) {
        let types = q.list("includeitemtypes");
        let wants = |name: &str| types.iter().any(|t| t == name);
        let recursive = q.flag("recursive") == Some(true);
        let parent = q.id("parentid");
        let ids = q.list("ids");
        let mut natural = !ids.is_empty();
        let mut nodes: Vec<Node> = if !ids.is_empty() {
            ids.iter().filter_map(|i| Id::parse(i)).filter_map(|i| self.node(i)).collect()
        } else {
            match parent {
                None | Some(Id::Root) if recursive => {
                    let mut all: Vec<Node> = self.movies().collect();
                    for t in self.titles.iter().filter(|t| t.kind == Kind::Series) {
                        all.push(Node::Series(t));
                        all.extend(self.seasons(t));
                        all.extend(self.season_episodes(t, None));
                    }
                    if wants("boxset") {
                        all.extend(self.sets.iter().map(Node::Set));
                    }
                    all
                }
                None | Some(Id::Root) => vec![Node::View(Kind::Movie), Node::View(Kind::Series)],
                Some(Id::Movies) => {
                    let mut all: Vec<Node> = self.movies().collect();
                    if wants("boxset") {
                        all.extend(self.sets.iter().map(Node::Set));
                    }
                    all
                }
                Some(Id::Shows) if recursive => {
                    let mut all = vec![];
                    for t in self.titles.iter().filter(|t| t.kind == Kind::Series) {
                        all.push(Node::Series(t));
                        all.extend(self.seasons(t));
                        all.extend(self.season_episodes(t, None));
                    }
                    all
                }
                Some(Id::Shows) => self.series().collect(),
                Some(Id::Series(i)) => match self.title(i) {
                    Some(t) => {
                        natural = true;
                        let mut all = self.seasons(t);
                        if recursive {
                            all.extend(self.season_episodes(t, None));
                        }
                        all
                    }
                    None => vec![],
                },
                Some(Id::Season(i, s)) => {
                    natural = true;
                    self.title(i).map(|t| self.season_episodes(t, Some(s))).unwrap_or_default()
                }
                Some(Id::Collection(c)) => {
                    // In order of release or the user's own order, films and series together.
                    natural = true;
                    self.set_members.get(&c).into_iter().flatten().filter_map(|id| self.title(*id)).map(|t| if t.kind == Kind::Movie { Node::Movie(t) } else { Node::Series(t) }).collect()
                }
                _ => vec![],
            }
        };

        // A recursive query with no type named means titles, not every season and episode.
        if types.is_empty() && recursive && ids.is_empty() && matches!(parent, None | Some(Id::Root) | Some(Id::Shows)) {
            nodes.retain(|n| matches!(n, Node::Movie(_) | Node::Series(_)));
        }
        if !types.is_empty() {
            nodes.retain(|n| wants(Self::kind_name(n)));
        }
        let excluded = q.list("excludeitemtypes");
        nodes.retain(|n| !excluded.iter().any(|t| t == Self::kind_name(n)));
        if q.list("mediatypes").iter().any(|m| m == "video") {
            nodes.retain(|n| matches!(n, Node::Movie(_) | Node::Episode(..)));
        }

        let filters = q.list("filters");
        let filter = |name: &str| filters.iter().any(|f| f == name);
        let played = q.flag("isplayed").or(if filter("isplayed") { Some(true) } else if filter("isunplayed") { Some(false) } else { None });
        let favorite = q.flag("isfavorite").or((filter("isfavorite") || filter("isfavoriteorlikes")).then_some(true));
        if let Some(want) = played {
            nodes.retain(|n| self.state(n).played == want);
        }
        if let Some(want) = favorite {
            nodes.retain(|n| self.state(n).favorite == want);
        }
        if filter("isresumable") {
            nodes.retain(|n| self.state(n).position > 0.0);
        }
        if let Some(term) = q.get("searchterm").map(str::to_lowercase).filter(|t| !t.is_empty()) {
            nodes.retain(|n| Self::name(n).to_lowercase().contains(&term) || matches!(n, Node::Movie(t) | Node::Series(t) if t.alt_titles.iter().any(|a| a.to_lowercase().contains(&term))));
        }
        if let Some(prefix) = q.get("namestartswith").map(str::to_lowercase).filter(|t| !t.is_empty()) {
            nodes.retain(|n| Self::sort_name(n).starts_with(&prefix));
        }
        let split = |key: &str| -> Vec<String> { q.all(key).iter().flat_map(|v| v.split(['|', ','])).map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect() };
        let genres = split("genres");
        if !genres.is_empty() {
            nodes.retain(|n| Self::genres(n).iter().any(|g| genres.contains(&g.to_lowercase())));
        }
        let genre_ids: Vec<Id> = split("genreids").iter().filter_map(|i| Id::parse(i)).collect();
        if !genre_ids.is_empty() {
            nodes.retain(|n| Self::genres(n).iter().any(|g| genre_ids.contains(&Id::Genre(Id::named(g)))));
        }
        let studio_ids: Vec<Id> = split("studioids").iter().filter_map(|i| Id::parse(i)).collect();
        if !studio_ids.is_empty() {
            nodes.retain(|n| Self::studio(n).is_some_and(|s| studio_ids.contains(&Id::Studio(Id::named(s)))));
        }
        let people: Vec<Id> = split("personids").iter().filter_map(|i| Id::parse(i)).collect();
        if !people.is_empty() {
            nodes.retain(|n| match n {
                Node::Movie(t) | Node::Series(t) => t.people.iter().any(|p| people.contains(&Id::Person(if t.kind == Kind::Movie { 1 } else { 2 }, p.id))),
                _ => false,
            });
        }
        let years: Vec<u32> = split("years").iter().filter_map(|y| y.parse().ok()).collect();
        if !years.is_empty() {
            nodes.retain(|n| years.contains(&self.year(n)));
        }

        let keys = q.list("sortby");
        let descending: Vec<bool> = q.list("sortorder").iter().map(|o| o.starts_with("desc")).collect();
        if !keys.is_empty() || !natural {
            let keys = if keys.is_empty() { vec!["sortname".to_string()] } else { keys };
            let seed = chrono::Utc::now().timestamp_subsec_nanos() as u64;
            let mut keyed: Vec<(Vec<SortKey>, Node)> = nodes.into_iter().map(|n| (keys.iter().map(|k| self.sort_key(&n, k, seed)).collect(), n)).collect();
            keyed.sort_by(|(a, _), (b, _)| {
                for (i, (x, y)) in a.iter().zip(b).enumerate() {
                    let order = x.cmp(y);
                    if order != std::cmp::Ordering::Equal {
                        // One direction given applies to every key.
                        return if descending.get(i).or(descending.first()).copied().unwrap_or(false) { order.reverse() } else { order };
                    }
                }
                std::cmp::Ordering::Equal
            });
            nodes = keyed.into_iter().map(|(_, n)| n).collect();
        }

        let total = nodes.len();
        let start = q.num("startindex").unwrap_or(0) as usize;
        let limit = q.num("limit").map(|l| l as usize).filter(|l| *l > 0).unwrap_or(usize::MAX);
        (nodes.into_iter().skip(start).take(limit).collect(), total)
    }

    fn sort_key(&self, n: &Node, key: &str, seed: u64) -> SortKey {
        let text = |s: String| SortKey::Text(s);
        let number = |x: f64| SortKey::Number(x);
        match key {
            "datecreated" | "dateadded" => number(self.added(n) as f64),
            "datelastcontentadded" => number(match n {
                Node::Series(t) => self.episodes_of(t.id).iter().filter_map(|e| self.episode_file(e)).map(|f| f.added_at).max().unwrap_or(0) as f64,
                other => self.added(other) as f64,
            }),
            "premieredate" | "productionyear" => text(self.premiere(n).unwrap_or_else(|| format!("{:04}", self.year(n)))),
            "communityrating" | "criticrating" => number(Self::rating(n).unwrap_or(0.0) as f64),
            "dateplayed" | "seriesdateplayed" => number(self.state(n).last_played as f64),
            "playcount" => number(self.state(n).play_count as f64),
            "runtime" => number(self.runtime_secs(n)),
            "random" => number((Id::named(&self.id(n).to_string()) ^ seed).wrapping_mul(0x9e3779b97f4a7c15) as f64),
            "indexnumber" => number(match n {
                Node::Season(_, s) => *s as f64,
                Node::Episode(_, e) => e.episode as f64,
                _ => 0.0,
            }),
            "parentindexnumber" => number(match n {
                Node::Episode(_, e) => e.season as f64,
                _ => 0.0,
            }),
            "seriessortname" => text(match n {
                Node::Season(t, _) | Node::Episode(t, _) => format!("{} {}", t.sort_title, Self::sort_name(n)),
                other => Self::sort_name(other),
            }),
            "officialrating" => text(match n {
                Node::Movie(t) | Node::Series(t) => t.certification.clone().unwrap_or_default(),
                _ => String::new(),
            }),
            _ => text(Self::sort_name(n)),
        }
    }

    pub fn seasons_of(&self, series: Id) -> Vec<Node<'_>> {
        match series {
            Id::Series(i) => self.title(i).map(|t| self.seasons(t)).unwrap_or_default(),
            _ => vec![],
        }
    }

    pub fn episodes_in(&self, series: Id, season: Option<u32>) -> Vec<Node<'_>> {
        match series {
            Id::Series(i) => self.title(i).map(|t| self.season_episodes(t, season)).unwrap_or_default(),
            _ => vec![],
        }
    }

    /// The newest arrivals: films as themselves, series by their latest episode.
    pub fn latest(&self, parent: Option<Id>, limit: usize) -> Vec<Node<'_>> {
        let mut nodes: Vec<Node> = vec![];
        if !matches!(parent, Some(Id::Shows)) {
            nodes.extend(self.movies());
        }
        if !matches!(parent, Some(Id::Movies)) {
            for t in self.titles.iter().filter(|t| t.kind == Kind::Series) {
                if let Some(e) = self.episodes_of(t.id).iter().max_by_key(|e| self.episode_file(e).map(|f| f.added_at).unwrap_or(0)) {
                    nodes.push(Node::Episode(t, e));
                }
            }
        }
        nodes.sort_by_key(|n| std::cmp::Reverse(self.added(n)));
        nodes.truncate(limit);
        nodes
    }

    /// Films and episodes stopped part-way, the most recent first.
    pub fn resume(&self, parent: Option<Id>, limit: usize) -> Vec<Node<'_>> {
        let mut nodes: Vec<Node> = vec![];
        if !matches!(parent, Some(Id::Shows)) {
            nodes.extend(self.movies());
        }
        if !matches!(parent, Some(Id::Movies)) {
            for t in self.titles.iter().filter(|t| t.kind == Kind::Series) {
                nodes.extend(self.season_episodes(t, None));
            }
        }
        nodes.retain(|n| {
            let s = self.state(n);
            s.position > 0.0 && !s.played
        });
        nodes.sort_by_key(|n| std::cmp::Reverse(self.state(n).last_played));
        nodes.truncate(limit);
        nodes
    }

    /// For each series someone has started, the episode after the last one they watched.
    pub fn next_up(&self, series: Option<Id>, limit: usize) -> Vec<Node<'_>> {
        let mut found: Vec<(i64, Node)> = vec![];
        for t in self.titles.iter().filter(|t| t.kind == Kind::Series) {
            if series.is_some_and(|s| s != Id::Series(t.id)) {
                continue;
            }
            let episodes: Vec<&Episode> = self.episodes_of(t.id).iter().filter(|e| e.season > 0).collect();
            let Some(last) = episodes.iter().rposition(|e| self.own("episode", e.id).played) else { continue };
            let when = self.own("episode", episodes[last].id).last_played_at;
            if let Some(next) = episodes[last + 1..].iter().find(|e| !self.own("episode", e.id).played) {
                found.push((when, Node::Episode(t, next)));
            }
        }
        found.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
        found.into_iter().take(limit).map(|(_, n)| n).collect()
    }

    /// Genres or studios in use under a view, by name.
    pub fn names(&self, parent: Option<Id>, studios: bool) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for t in &self.titles {
            if matches!((parent, t.kind), (Some(Id::Movies), Kind::Series) | (Some(Id::Shows), Kind::Movie)) {
                continue;
            }
            let n = if t.kind == Kind::Movie { Node::Movie(t) } else { Node::Series(t) };
            let names: Vec<String> = if studios { Self::studio(&n).map(str::to_string).into_iter().collect() } else { t.genres.clone() };
            for name in names {
                if !out.iter().any(|o| o.eq_ignore_ascii_case(&name)) {
                    out.push(name);
                }
            }
        }
        out.sort_by_key(|n| n.to_lowercase());
        out
    }

    /// Everyone credited on anything in the library, once each.
    pub fn people(&self) -> Vec<(Id, &Person)> {
        let mut seen = HashSet::new();
        let mut out = vec![];
        for t in &self.titles {
            let source = if t.kind == Kind::Movie { 1 } else { 2 };
            for p in &t.people {
                let id = Id::Person(source, p.id);
                if seen.insert(id) {
                    out.push((id, p));
                }
            }
        }
        out
    }

    pub fn person_json(&self, id: Id, p: &Person) -> Value {
        let id = id.to_string();
        json!({
            "Name": p.name, "ServerId": self.server_id, "Id": id, "Type": "Person", "IsFolder": false, "LocationType": "FileSystem", "MediaType": "Unknown",
            "ImageTags": tag_of(&p.photo).map(|t| json!({"Primary": t})).unwrap_or(json!({})), "BackdropImageTags": [], "ImageBlurHashes": {},
            "UserData": {"PlaybackPositionTicks": 0, "PlayCount": 0, "IsFavorite": false, "Played": false, "Key": id, "ItemId": id},
        })
    }

    pub fn counts(&self) -> Value {
        let movies = self.titles.iter().filter(|t| t.kind == Kind::Movie).count();
        json!({
            "MovieCount": movies, "SeriesCount": self.titles.len() - movies, "EpisodeCount": self.episodes.values().map(Vec::len).sum::<usize>(),
            "ArtistCount": 0, "ProgramCount": 0, "TrailerCount": 0, "SongCount": 0, "AlbumCount": 0, "MusicVideoCount": 0, "BoxSetCount": self.sets.len(), "BookCount": 0,
            "ItemCount": self.titles.len(),
        })
    }

    /// Where an image for an item comes from, a name to keep it under, and how wide to keep it.
    // ------------------------------------------------------------ extras

    /// A trailer or another extra by its id, with the title it belongs to.
    pub fn extra(&self, id: Id) -> Option<(&Title, &crate::local::Extra)> {
        match id {
            Id::Extra(title, n) => self.title(title).and_then(|t| t.local.extras.get(n as usize).map(|e| (t, e))),
            _ => None,
        }
    }

    pub fn extra_path(&self, id: Id) -> Option<std::path::PathBuf> {
        self.extra(id).map(|(t, e)| std::path::Path::new(&t.path).join(&e.rel_path))
    }

    /// An extra as a player lists and plays it. Its file is sent as it is, like any other.
    pub fn extra_json(&self, id: Id) -> Option<Value> {
        let (t, e) = self.extra(id)?;
        let trailer = e.kind == crate::local::ExtraKind::Trailer;
        let path = format!("{}/{}", t.path, e.rel_path);
        let container = e.rel_path.rsplit_once('.').map(|(_, x)| x.to_lowercase()).unwrap_or_default();
        let owner = match t.kind {
            Kind::Movie => Id::Movie(t.id),
            Kind::Series => Id::Series(t.id),
        };
        let name = if e.name.is_empty() { "Trailer".to_string() } else { e.name.clone() };
        let source = json!({
            "Protocol": "File", "Id": id.to_string(), "Path": path, "Type": "Default", "Container": container, "Size": e.size, "Name": name, "IsRemote": false,
            "ETag": format!("{:x}{:x}", t.id, e.size), "ReadAtNativeFramerate": false, "IgnoreDts": false, "IgnoreIndex": false, "GenPtsInput": false,
            "SupportsTranscoding": false, "SupportsDirectStream": true, "SupportsDirectPlay": true, "IsInfiniteStream": false,
            "RequiresOpening": false, "RequiresClosing": false, "RequiresLooping": false, "SupportsProbing": true, "HasSegments": false,
            "VideoType": "VideoFile", "MediaStreams": [], "MediaAttachments": [], "Formats": [], "RequiredHttpHeaders": {},
        });
        Some(json!({
            "Name": name, "SortName": name.to_lowercase(), "ServerId": self.server_id, "Id": id.to_string(), "Type": if trailer { "Trailer" } else { "Video" },
            "ExtraType": e.kind.jellyfin(), "MediaType": "Video", "VideoType": "VideoFile", "IsFolder": false, "LocationType": "FileSystem",
            "Path": path, "Container": container, "CanDownload": false, "ParentId": owner.to_string(),
            "ImageTags": {}, "BackdropImageTags": [], "ImageBlurHashes": {},
            "UserData": {"PlaybackPositionTicks": 0, "PlayCount": 0, "IsFavorite": false, "Played": false, "Key": id.to_string(), "ItemId": id.to_string()},
            "MediaSources": [source],
        }))
    }

    /// The trailers of a film, a show or a season, or its other extras. A season has those
    /// kept inside its own folder.
    pub fn extras_of(&self, owner: Id, trailers: bool) -> Vec<Value> {
        let (title, season) = match owner {
            Id::Movie(i) | Id::Series(i) => (i, None),
            Id::Season(i, n) => (i, Some(n)),
            _ => return vec![],
        };
        let Some(t) = self.title(title) else { return vec![] };
        let in_season = |rel: &str| std::path::Path::new(rel).components().next().and_then(|c| crate::local::season_folder(&c.as_os_str().to_string_lossy()));
        let wanted: Vec<usize> = if trailers { t.local.trailers().map(|(i, _)| i).collect() } else { t.local.features().map(|(i, _)| i).collect() };
        wanted
            .into_iter()
            .filter(|i| match season {
                Some(n) => in_season(&t.local.extras[*i].rel_path) == Some(n),
                None => true,
            })
            .filter_map(|i| self.extra_json(Id::Extra(t.id, i as u32)))
            .collect()
    }

    /// A season's poster kept in the show's folder.
    fn season_poster(&self, t: &Title, season: u32) -> Option<String> {
        t.local.season_art(crate::local::ArtKind::Poster, season).map(|a| crate::local::art_address(&t.path, a))
    }

    /// An episode's frame: a picture kept beside its file, before the metadata source's.
    fn episode_still(&self, t: &Title, e: &Episode) -> Option<String> {
        self.episode_file(e).and_then(|f| t.local.art_of(&f.rel_path)).map(|a| crate::local::art_address(&t.path, a)).or_else(|| e.still.clone())
    }

    pub fn image(&self, id: Id, kind: &str) -> Option<(String, String, u32)> {
        let wide = matches!(kind, "backdrop" | "thumb" | "art" | "banner");
        if !wide && kind != "primary" {
            return None;
        }
        let title_art = |t: &Title| match wide {
            true => t.fanart_source().map(|src| (src, format!("jf-fanart-{}", t.id), 1920)),
            false => t.poster_source().map(|src| (src, format!("jf-poster-{}", t.id), 780)),
        };
        if let Id::Person(..) = id {
            return self.people().into_iter().find(|(i, _)| *i == id).and_then(|(_, p)| p.photo.clone()).map(|src| (src, format!("jf-person-{id}"), 185));
        }
        match self.node(id)? {
            Node::Movie(t) | Node::Series(t) => title_art(t),
            // A season's own poster, where the folder has one.
            Node::Season(t, n) if !wide => self.season_poster(t, n).map(|src| (src, format!("jf-season-{}-{n}", t.id), 780)).or_else(|| title_art(t)),
            Node::Season(t, _) => title_art(t),
            Node::Episode(t, e) if wide => self.episode_still(t, e).map(|src| (src, format!("jf-still-{}", e.id), 960)).or_else(|| title_art(t)),
            Node::Episode(t, e) => self.episode_still(t, e).map(|src| (src, format!("jf-still-{}", e.id), 960)).or_else(|| t.fanart_source().map(|src| (src, format!("jf-fanart-{}", t.id), 1920))),
            Node::Set(c) if wide => c.fanart.clone().map(|src| (src, format!("jf-setwide-{}", c.id), 1920)),
            Node::Set(c) => c.poster.clone().map(|src| (src, format!("jf-set-{}", c.id), 780)),
            Node::View(_) => None,
        }
    }
}

#[derive(PartialEq, PartialOrd)]
enum SortKey {
    Text(String),
    Number(f64),
}

impl SortKey {
    fn cmp(&self, other: &SortKey) -> std::cmp::Ordering {
        self.partial_cmp(other).unwrap_or(std::cmp::Ordering::Equal)
    }
}

/// What to call one version of a film among several: its cut if it has one, else its size.
fn version_name(f: &MediaFile) -> String {
    // A film in several files: each is offered by its place in the order.
    let stem = std::path::Path::new(&f.rel_path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    if let Some(n) = crate::local::part_number(&stem).filter(|_| !f.extra) {
        return format!("Part {n}");
    }
    let height = f.media_info.as_ref().map(|m| (m.width, m.height)).unwrap_or((0, 0));
    let size = match height {
        (w, h) if w >= 3000 || h >= 2000 => "4K",
        (w, h) if w >= 1800 || h >= 1000 => "1080p",
        (w, h) if w >= 1200 || h >= 700 => "720p",
        (0, 0) => "",
        _ => "SD",
    };
    match (f.edition.is_empty(), size.is_empty()) {
        (true, true) => "Default".into(),
        (true, false) => size.into(),
        (false, true) => f.edition.clone(),
        (false, false) => format!("{} - {size}", f.edition),
    }
}

/// Jellyfin gives languages as three letters.
fn three_letter(code: &str) -> String {
    let c = code.trim().to_lowercase();
    if c.len() == 3 || c.is_empty() {
        return if c.is_empty() { "und".into() } else { c };
    }
    match c.as_str() {
        "en" => "eng", "es" => "spa", "fr" => "fre", "de" => "ger", "it" => "ita", "pt" => "por", "nl" => "dut", "ja" => "jpn", "ko" => "kor", "zh" => "chi",
        "ru" => "rus", "sv" => "swe", "no" => "nor", "da" => "dan", "fi" => "fin", "pl" => "pol", "tr" => "tur", "ar" => "ara", "he" => "heb", "hi" => "hin",
        "el" => "gre", "cs" => "cze", "hu" => "hun", "ro" => "rum", "th" => "tha", "vi" => "vie", "id" => "ind", "uk" => "ukr",
        _ => return c,
    }
    .into()
}

fn language_label(code: &str) -> String {
    match crate::subs::language_code(code) {
        Some(two) => crate::subs::language_name(two),
        None if code.is_empty() || code == "und" => "Unknown".into(),
        None => code.to_string(),
    }
}

fn audio_label(codec: &str, profile: &str) -> String {
    match codec {
        "eac3" if profile.contains("Atmos") => "Dolby Digital+ Atmos".into(),
        "eac3" => "Dolby Digital+".into(),
        "ac3" => "Dolby Digital".into(),
        "truehd" if profile.contains("Atmos") => "Dolby TrueHD Atmos".into(),
        "truehd" => "Dolby TrueHD".into(),
        "dts" if !profile.is_empty() => profile.to_string(),
        other => other.to_uppercase(),
    }
}

fn channel_label(channels: u32, layout: &str) -> String {
    match channels {
        0 => String::new(),
        1 => "Mono".into(),
        2 => "Stereo".into(),
        6 => "5.1".into(),
        8 => "7.1".into(),
        n if layout.contains("LFE") || layout.contains(".1") => format!("{}.1", n - 1),
        n => format!("{n} ch"),
    }
}

/// Spool's one-word summary of a file's dynamic range, as Jellyfin's pair of fields.
fn video_range(summary: &str) -> (&'static str, &'static str) {
    match summary {
        "" => ("SDR", "SDR"),
        "HDR10" | "PQ" => ("HDR", "HDR10"),
        "HDR10Plus" => ("HDR", "HDR10Plus"),
        "HLG" => ("HDR", "HLG"),
        "DV HDR10" | "DV HDR10Plus" => ("HDR", "DOVIWithHDR10"),
        "DV HLG" => ("HDR", "DOVIWithHLG"),
        "DV SDR" => ("SDR", "DOVIWithSDR"),
        _ => ("HDR", "DOVI"),
    }
}

fn subtitle_codec(codec: &str) -> &str {
    match codec {
        "hdmv_pgs_subtitle" => "PGSSUB",
        "dvd_subtitle" => "DVDSUB",
        "dvb_subtitle" => "DVBSUB",
        other => other,
    }
}

/// The streams of a file as Jellyfin lists them. A file probed before streams were recorded
/// gets one video and one audio entry made from the summary Spool has always kept.
pub fn media_streams(info: &MediaInfo) -> Vec<Value> {
    let summary;
    let streams: &[Stream] = if info.streams.is_empty() {
        summary = summary_streams(info);
        &summary
    } else {
        &info.streams
    };
    let first_audio = streams.iter().position(|s| s.kind == "audio");
    let any_default_audio = streams.iter().any(|s| s.kind == "audio" && s.default);
    streams
        .iter()
        .enumerate()
        .map(|(index, s)| {
            let language = language_label(&s.language);
            let mut o = json!({
                "Index": index, "Codec": s.codec, "IsDefault": s.default, "IsForced": s.forced, "IsExternal": false, "IsInterlaced": false,
                "IsTextSubtitleStream": false, "SupportsExternalStream": false, "IsHearingImpaired": false,
            });
            if !s.language.is_empty() {
                o["Language"] = json!(three_letter(&s.language));
            }
            if !s.title.is_empty() {
                o["Title"] = json!(s.title);
            }
            if !s.profile.is_empty() {
                o["Profile"] = json!(s.profile);
            }
            if s.bit_rate > 0 {
                o["BitRate"] = json!(s.bit_rate);
            }
            match s.kind.as_str() {
                "video" => {
                    let (range, range_type) = video_range(&info.dynamic_range_type);
                    let size = match (s.width, s.height) {
                        (w, h) if w >= 3000 || h >= 2000 => "4K".to_string(),
                        (w, h) if w >= 1800 || h >= 1000 => "1080p".to_string(),
                        (w, h) if w >= 1200 || h >= 700 => "720p".to_string(),
                        (_, h) => format!("{h}p"),
                    };
                    let mut title = format!("{size} {}", s.codec.to_uppercase());
                    if range_type != "SDR" {
                        title.push_str(if range_type.starts_with("DOVI") { " Dolby Vision" } else { " HDR" });
                    }
                    o["Type"] = json!("Video");
                    o["DisplayTitle"] = json!(title);
                    o["Width"] = json!(s.width);
                    o["Height"] = json!(s.height);
                    o["VideoRange"] = json!(range);
                    o["VideoRangeType"] = json!(range_type);
                    o["IsAVC"] = json!(s.codec == "h264");
                    o["IsDefault"] = json!(true);
                    if s.bit_depth > 0 {
                        o["BitDepth"] = json!(s.bit_depth);
                    }
                    if s.frame_rate > 0.0 {
                        o["AverageFrameRate"] = json!(s.frame_rate);
                        o["RealFrameRate"] = json!(s.frame_rate);
                    }
                    if !s.pix_fmt.is_empty() {
                        o["PixelFormat"] = json!(s.pix_fmt);
                    }
                    if !s.color_transfer.is_empty() {
                        o["ColorTransfer"] = json!(s.color_transfer);
                    }
                    if s.width > 0 && s.height > 0 {
                        let ratio = s.width as f64 / s.height as f64;
                        o["AspectRatio"] = json!(if (ratio - 16.0 / 9.0).abs() < 0.03 { "16:9".to_string() } else if (ratio - 4.0 / 3.0).abs() < 0.03 { "4:3".to_string() } else { format!("{ratio:.2}:1") });
                    }
                }
                "audio" => {
                    let default = s.default || (!any_default_audio && Some(index) == first_audio);
                    let mut parts = vec![];
                    if !s.language.is_empty() {
                        parts.push(language);
                    }
                    parts.push(audio_label(&s.codec, &s.profile));
                    let channels = channel_label(s.channels, &s.channel_layout);
                    if !channels.is_empty() {
                        parts.push(channels);
                    }
                    if default {
                        parts.push("Default".into());
                    }
                    o["Type"] = json!("Audio");
                    o["IsDefault"] = json!(default);
                    o["DisplayTitle"] = json!(parts.join(" - "));
                    o["Channels"] = json!(s.channels);
                    if !s.channel_layout.is_empty() {
                        o["ChannelLayout"] = json!(s.channel_layout);
                    }
                    if s.sample_rate > 0 {
                        o["SampleRate"] = json!(s.sample_rate);
                    }
                }
                _ => {
                    let text = matches!(s.codec.as_str(), "subrip" | "srt" | "ass" | "ssa" | "mov_text" | "webvtt" | "text");
                    let mut parts = vec![language];
                    if s.default {
                        parts.push("Default".into());
                    }
                    if s.forced {
                        parts.push("Forced".into());
                    }
                    parts.push(subtitle_codec(&s.codec).to_uppercase());
                    o["Type"] = json!("Subtitle");
                    o["Codec"] = json!(subtitle_codec(&s.codec));
                    o["DisplayTitle"] = json!(parts.join(" - "));
                    o["IsTextSubtitleStream"] = json!(text);
                    o["SupportsExternalStream"] = json!(text);
                }
            }
            o
        })
        .collect()
}

fn summary_streams(info: &MediaInfo) -> Vec<Stream> {
    if info.video_codec.is_empty() && info.width == 0 {
        return vec![];
    }
    let video = match info.video_codec.to_lowercase().as_str() {
        "x265" | "hevc" | "h265" => "hevc",
        "x264" | "avc" | "h264" => "h264",
        "xvid" | "divx" => "mpeg4",
        "mpeg2" => "mpeg2video",
        other => return summary_with(info, other.to_string()),
    };
    summary_with(info, video.to_string())
}

fn summary_with(info: &MediaInfo, video: String) -> Vec<Stream> {
    let mut out = vec![Stream { index: 0, kind: "video".into(), codec: video, width: info.width, height: info.height, bit_depth: info.bit_depth, default: true, ..Default::default() }];
    if !info.audio_codec.is_empty() {
        let name = info.audio_codec.to_lowercase();
        let codec = match name.split([' ', '-']).next().unwrap_or("") {
            "truehd" => "truehd",
            "eac3" => "eac3",
            "ac3" => "ac3",
            "dts" => "dts",
            "he" | "aac" => "aac",
            other => other,
        };
        let channels = (info.audio_channels.floor() + (info.audio_channels.fract() * 10.0).round()) as u32;
        out.push(Stream {
            index: 1,
            kind: "audio".into(),
            codec: codec.to_string(),
            profile: if name.contains("atmos") { "Dolby Atmos".into() } else if codec == "dts" { info.audio_codec.clone() } else { String::new() },
            language: info.audio_languages.first().cloned().unwrap_or_default(),
            channels,
            default: true,
            ..Default::default()
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_probed_long_ago_still_lists_its_tracks() {
        let info = MediaInfo { video_codec: "x265".into(), audio_codec: "EAC3 Atmos".into(), audio_channels: 5.1, width: 3840, height: 1600, dynamic_range_type: "DV HDR10".into(), audio_languages: vec!["eng".into()], ..Default::default() };
        let streams = media_streams(&info);
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0]["Codec"], "hevc");
        assert_eq!(streams[0]["VideoRangeType"], "DOVIWithHDR10");
        assert_eq!(streams[0]["DisplayTitle"], "4K HEVC Dolby Vision");
        assert_eq!(streams[1]["Codec"], "eac3");
        assert_eq!(streams[1]["Channels"], 6);
        assert_eq!(streams[1]["DisplayTitle"], "English - Dolby Digital+ Atmos - 5.1 - Default");
    }

    #[test]
    fn dates_are_written_the_way_jellyfin_writes_them() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.0000000Z");
        assert_eq!(iso_date("2019-03-08").as_deref(), Some("2019-03-08T00:00:00.0000000Z"));
        assert_eq!(iso_date("soon"), None);
        assert_eq!(three_letter("en"), "eng");
        assert_eq!(three_letter("jpn"), "jpn");
    }
}
