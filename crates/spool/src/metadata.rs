//! Title metadata: TMDB for movies; TVmaze for television, and TMDB for a series TVmaze
//! does not list.

use crate::models::{Collection, Episode, Kind, Person, SeasonInfo, SeriesTwin, Title};
use anyhow::{anyhow, bail, Result};
use serde_json::Value;

const TMDB: &str = "https://api.themoviedb.org/3";
const TVMAZE: &str = "https://api.tvmaze.com";
const TMDB_IMG: &str = "https://image.tmdb.org/t/p";

/// The ids a series may be known by. It is read from TVmaze when TVmaze lists it, and from
/// TMDB when it does not: old anime OVAs, say, that TVmaze has never listed.
#[derive(Clone, Debug, Default)]
pub struct SeriesIds {
    pub tvmaze: Option<u32>,
    pub tmdb: Option<u32>,
    pub tvdb: Option<u32>,
    pub imdb: Option<String>,
}

impl SeriesIds {
    pub fn of(t: &Title) -> SeriesIds {
        SeriesIds { tvmaze: t.tvmaze_id, tmdb: t.tmdb_id, tvdb: t.tvdb_id, imdb: t.imdb_id.clone() }
    }

    pub fn tvdb(id: u32) -> SeriesIds {
        SeriesIds { tvdb: Some(id), ..Default::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.tvmaze.is_none() && self.tmdb.is_none() && self.tvdb.is_none() && self.imdb.is_none()
    }
}

#[derive(Clone)]
pub struct Metadata {
    pub http: reqwest::Client,
    pub tmdb_base: String,
    pub tvmaze_base: String,
    /// AniList, and the list that ties its ids to the other sources'. See `anime.rs`.
    pub anilist_base: String,
    pub anime_ids_url: String,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn opt(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(str::to_string)
}
fn strip_html(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

impl Metadata {
    pub fn new(http: reqwest::Client) -> Metadata {
        Metadata { http, tmdb_base: TMDB.into(), tvmaze_base: TVMAZE.into(), anilist_base: crate::anime::ANILIST.into(), anime_ids_url: crate::anime::ANIME_IDS.into() }
    }

    async fn get(&self, url: &str) -> Result<Value> {
        let r = self.http.get(url).timeout(std::time::Duration::from_secs(30)).send().await.map_err(|e| anyhow!("{}", e.without_url()))?;
        if r.status().as_u16() == 404 {
            bail!("not found");
        }
        if r.status().as_u16() == 401 {
            bail!("the metadata provider rejected the API key");
        }
        if !r.status().is_success() {
            bail!("metadata provider answered HTTP {}", r.status().as_u16());
        }
        Ok(r.json().await.map_err(|e| anyhow!("{}", e.without_url()))?)
    }

    fn movie_from(&self, m: &Value) -> Title {
        let date = s(m, "release_date");
        let mut t = Title {
            id: 0,
            kind: Kind::Movie,
            title: s(m, "title"),
            sort_title: String::new(),
            year: date.get(..4).and_then(|y| y.parse().ok()).unwrap_or(0),
            overview: s(m, "overview"),
            status: match s(m, "status").as_str() {
                "Released" => "released".into(),
                "In Production" | "Post Production" | "Planned" | "Rumored" => "announced".into(),
                other => other.to_lowercase(),
            },
            runtime: m.get("runtime").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            tmdb_id: m.get("id").and_then(|x| x.as_u64()).map(|x| x as u32),
            tvdb_id: None,
            tvmaze_id: None,
            imdb_id: opt(m, "imdb_id").or_else(|| m.get("external_ids").and_then(|e| opt(e, "imdb_id"))),
            poster: opt(m, "poster_path").map(|p| format!("{TMDB_IMG}/w342{p}")),
            fanart: opt(m, "backdrop_path").map(|p| format!("{TMDB_IMG}/w1280{p}")),
            genres: m.get("genres").and_then(|g| g.as_array()).map(|g| g.iter().map(|x| s(x, "name")).collect()).unwrap_or_default(),
            alt_titles: vec![],
            alt_years: vec![],
            monitored: true,
            profile_id: 0,
            path: String::new(),
            added_at: 0,
            in_cinemas: (!date.is_empty()).then_some(date),
            digital_release: None,
            physical_release: None,
            minimum_availability: "released".into(),
            studio: m.get("production_companies").and_then(|c| c.as_array()).and_then(|c| c.first()).map(|c| s(c, "name")),
            network: None,
            seasons: vec![],
            season_folder: true,
            original_language: spool_core::parser::language_from_iso(&s(m, "original_language")).map(str::to_string),
            first_aired: None,
            last_search_at: 0,
            last_refresh_at: 0,
            tagline: s(m, "tagline"),
            rating: score(m.get("vote_average")),
            certification: None,
            people: movie_people(m),
            collection: m.get("belongs_to_collection").filter(|c| c.is_object()).map(|c| Collection {
                id: c.get("id").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                name: s(c, "name"),
                poster: opt(c, "poster_path").map(|p| format!("{TMDB_IMG}/w342{p}")),
                fanart: opt(c, "backdrop_path").map(|p| format!("{TMDB_IMG}/w1280{p}")),
            }),
            details_at: 0,
            ..Default::default()
        };
        let original = s(m, "original_title");
        if !original.is_empty() && original != t.title {
            t.alt_titles.push(original);
        }
        if let Some(alts) = m.get("alternative_titles").and_then(|a| a.get("titles")).and_then(|a| a.as_array()) {
            for a in alts {
                let name = s(a, "title");
                if matches!(s(a, "iso_3166_1").as_str(), "US" | "GB" | "CA" | "AU") && !name.is_empty() && !t.alt_titles.contains(&name) && name != t.title {
                    t.alt_titles.push(name);
                }
            }
        }
        // Release dates: type 3 theatrical, 4 digital, 5 physical. Earliest of each, preferring US.
        if let Some(countries) = m.get("release_dates").and_then(|r| r.get("results")).and_then(|r| r.as_array()) {
            let mut earliest: [Option<String>; 6] = Default::default();
            for country in countries {
                for rd in country.get("release_dates").and_then(|r| r.as_array()).into_iter().flatten() {
                    // The age rating is the American one: the theatrical release's, else any.
                    if s(country, "iso_3166_1") == "US" {
                        let cert = s(rd, "certification");
                        let theatrical = rd.get("type").and_then(|x| x.as_u64()) == Some(3);
                        if !cert.is_empty() && (t.certification.is_none() || theatrical) {
                            t.certification = Some(cert);
                        }
                    }
                    let ty = rd.get("type").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                    let d = s(rd, "release_date").get(..10).unwrap_or("").to_string();
                    if ty < 6 && !d.is_empty() && earliest[ty].as_ref().is_none_or(|e| d < *e) {
                        earliest[ty] = Some(d);
                    }
                }
            }
            if earliest[3].is_some() {
                t.in_cinemas = earliest[3].clone();
            }
            t.digital_release = earliest[4].clone();
            t.physical_release = earliest[5].clone();
        }
        t
    }

    pub async fn search_movies(&self, key: &str, query: &str) -> Result<Vec<Title>> {
        if key.is_empty() {
            bail!("add a TMDB API key in Settings to search for movies");
        }
        // "tmdb:603" and "tt0133093" look a title up directly.
        if let Some(id) = query.trim().strip_prefix("tmdb:").and_then(|i| i.trim().parse::<u32>().ok()) {
            return Ok(vec![self.movie(key, id).await?]);
        }
        if query.trim().starts_with("tt") && query.trim()[2..].chars().all(|c| c.is_ascii_digit()) {
            let v = self.get(&format!("{}/find/{}?external_source=imdb_id&api_key={key}", self.tmdb_base, query.trim())).await?;
            return Ok(v["movie_results"].as_array().map(|a| a.iter().map(|m| self.movie_from(m)).collect()).unwrap_or_default());
        }
        let (q, year) = split_year(query);
        let search = |q: String, year: Option<u32>| async move {
            let mut url = format!("{}/search/movie?api_key={key}&query={}", self.tmdb_base, urlencoding::encode(&q));
            if let Some(y) = year {
                url.push_str(&format!("&year={y}"));
            }
            let v = self.get(&url).await?;
            Ok::<Vec<Title>, anyhow::Error>(v["results"].as_array().map(|a| a.iter().take(20).map(|m| self.movie_from(m)).collect()).unwrap_or_default())
        };
        let mut found = search(q, year).await?;
        // A trailing number may be part of the title ("Blade Runner 2049"), so also try it whole.
        if year.is_some() {
            for t in search(query.trim().to_string(), None).await.unwrap_or_default() {
                if !found.iter().any(|f| f.tmdb_id == t.tmdb_id) {
                    found.push(t);
                }
            }
        }
        Ok(found)
    }

    /// A series as a search would list it, found by its TVDB id.
    pub async fn series_listing_by_tvdb(&self, key: &str, tvdb_id: u32) -> Result<Title> {
        if let Some(show) = self.tvmaze_show(&format!("thetvdb={tvdb_id}")).await? {
            return Ok(self.series_from(&show));
        }
        if key.is_empty() {
            bail!("not found");
        }
        let v = self.get(&format!("{}/find/{tvdb_id}?external_source=tvdb_id&api_key={key}", self.tmdb_base)).await?;
        let show = v.get("tv_results").and_then(|r| r.as_array()).and_then(|r| r.first()).ok_or_else(|| anyhow!("not found"))?;
        let mut t = self.tv_from(show);
        t.tvdb_id = Some(tvdb_id);
        Ok(t)
    }

    /// A film collection, whole: every film TMDB puts in it.
    pub async fn film_set(&self, key: &str, id: u32) -> Result<crate::anime::FilmSet> {
        let v = self.get(&format!("{}/collection/{id}?api_key={key}", self.tmdb_base)).await?;
        let parts = v
            .get("parts")
            .and_then(|p| p.as_array())
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some(crate::anime::FilmPart {
                    tmdb_id: p.get("id")?.as_u64()? as u32,
                    title: s(p, "title"),
                    year: s(p, "release_date").get(..4).and_then(|y| y.parse().ok()).unwrap_or(0),
                    poster: opt(p, "poster_path").map(|path| format!("{TMDB_IMG}/w500{path}")),
                })
            })
            .collect();
        Ok(crate::anime::FilmSet { id, name: s(&v, "name"), parts })
    }

    /// The film source's id for a film known by its IMDb id.
    pub async fn movie_by_imdb(&self, key: &str, imdb_id: &str) -> Result<Option<u32>> {
        if key.is_empty() {
            bail!("add a TMDB API key in Settings to fetch movie details");
        }
        let v = self.get(&format!("{}/find/{imdb_id}?external_source=imdb_id&api_key={key}", self.tmdb_base)).await?;
        Ok(v.get("movie_results").and_then(|r| r.as_array()).and_then(|r| r.first()).and_then(|m| m.get("id")).and_then(|x| x.as_u64()).map(|x| x as u32))
    }

    /// The film source's id for a series known by its TVDB or IMDb id. Players such as Infuse
    /// find a series in their library by it.
    pub async fn series_tmdb(&self, key: &str, tvdb_id: Option<u32>, imdb_id: Option<&str>) -> Result<Option<u32>> {
        if key.is_empty() {
            return Ok(None);
        }
        let asks = tvdb_id.map(|id| (id.to_string(), "tvdb_id")).into_iter().chain(imdb_id.map(|id| (id.to_string(), "imdb_id")));
        for (id, source) in asks {
            let v = self.get(&format!("{}/find/{id}?external_source={source}&api_key={key}", self.tmdb_base)).await?;
            if let Some(found) = v.get("tv_results").and_then(|r| r.as_array()).and_then(|r| r.first()).and_then(|m| m.get("id")).and_then(|x| x.as_u64()) {
                return Ok(Some(found as u32));
            }
        }
        Ok(None)
    }

    pub async fn movie(&self, key: &str, tmdb_id: u32) -> Result<Title> {
        if key.is_empty() {
            bail!("add a TMDB API key in Settings to fetch movie details");
        }
        let v = self.get(&format!("{}/movie/{tmdb_id}?api_key={key}&append_to_response=release_dates,alternative_titles,external_ids,credits", self.tmdb_base)).await?;
        let mut t = self.movie_from(&v);
        t.details_at = crate::db::now();
        Ok(t)
    }

    fn series_from(&self, show: &Value) -> Title {
        let premiered = opt(show, "premiered");
        Title {
            id: 0,
            kind: Kind::Series,
            title: s(show, "name"),
            sort_title: String::new(),
            year: premiered.as_deref().and_then(|d| d.get(..4)).and_then(|y| y.parse().ok()).unwrap_or(0),
            overview: strip_html(&s(show, "summary")),
            status: match s(show, "status").as_str() {
                "Ended" => "ended".into(),
                "Running" => "continuing".into(),
                _ => "upcoming".into(),
            },
            runtime: show.get("runtime").and_then(|x| x.as_u64()).or_else(|| show.get("averageRuntime").and_then(|x| x.as_u64())).unwrap_or(0) as u32,
            tmdb_id: None,
            tvdb_id: show.get("externals").and_then(|e| e.get("thetvdb")).and_then(|x| x.as_u64()).map(|x| x as u32),
            tvmaze_id: show.get("id").and_then(|x| x.as_u64()).map(|x| x as u32),
            imdb_id: show.get("externals").and_then(|e| opt(e, "imdb")),
            poster: show.get("image").and_then(|i| opt(i, "original").or_else(|| opt(i, "medium"))),
            fanart: None,
            genres: show.get("genres").and_then(|g| g.as_array()).map(|g| g.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
            alt_titles: vec![],
            alt_years: vec![],
            monitored: true,
            profile_id: 0,
            path: String::new(),
            added_at: 0,
            in_cinemas: None,
            digital_release: None,
            physical_release: None,
            minimum_availability: "released".into(),
            studio: None,
            network: show.get("network").and_then(|n| opt(n, "name")).or_else(|| show.get("webChannel").and_then(|n| opt(n, "name"))),
            seasons: vec![],
            season_folder: true,
            original_language: Some(s(show, "language").to_lowercase()).filter(|l| !l.is_empty()),
            first_aired: premiered,
            last_search_at: 0,
            last_refresh_at: 0,
            tagline: String::new(),
            rating: score(show.get("rating").and_then(|r| r.get("average"))),
            certification: None,
            people: series_people(show),
            collection: None,
            details_at: 0,
            ..Default::default()
        }
    }

    /// Series by name, from TVmaze and TMDB together. A show TVmaze lists comes from TVmaze;
    /// one only TMDB lists takes the place TMDB ranks it at.
    pub async fn search_series(&self, key: &str, query: &str) -> Result<Vec<Title>> {
        use crate::names::plain;
        if let Some(id) = query.trim().strip_prefix("tvdb:").and_then(|i| i.trim().parse::<u32>().ok()) {
            return Ok(vec![self.series(key, &SeriesIds::tvdb(id)).await?.0]);
        }
        let q = urlencoding::encode(query.trim());
        let tvmaze = async {
            let v = self.get(&format!("{}/search/shows?q={q}", self.tvmaze_base)).await?;
            Ok::<_, anyhow::Error>(v.as_array().map(|a| a.iter().take(20).filter_map(|r| r.get("show")).map(|sh| self.series_from(sh)).collect::<Vec<_>>()).unwrap_or_default())
        };
        let tmdb = async {
            if key.is_empty() {
                return Ok(vec![]);
            }
            let v = self.get(&format!("{}/search/tv?api_key={key}&query={q}", self.tmdb_base)).await?;
            Ok::<_, anyhow::Error>(v["results"].as_array().map(|a| a.iter().take(20).map(|sh| self.tv_from(sh)).collect::<Vec<_>>()).unwrap_or_default())
        };
        let (tvmaze, tmdb) = tokio::join!(tvmaze, tmdb);
        let tmdb = tmdb.unwrap_or_else(|e| {
            tracing::debug!(error = %e, "TMDB could not be searched for series");
            vec![]
        });
        let mut found = match tvmaze {
            Ok(found) => found,
            Err(_) if !tmdb.is_empty() => vec![],
            Err(e) => return Err(e),
        };
        let listed = |t: &Title, by: &[Title]| by.iter().any(|f| plain(&f.title) == plain(&t.title) && f.year.abs_diff(t.year) <= 1);
        let extra: Vec<(usize, Title)> = tmdb.into_iter().enumerate().filter(|(_, t)| !listed(t, &found)).collect();
        for (rank, t) in extra {
            found.insert(rank.min(found.len()), t);
        }
        Ok(found)
    }

    /// A series' details and every episode, specials included, from whichever source lists it:
    /// TVmaze when it does, TMDB otherwise. Only a source's saying it has no such show passes
    /// the question on; one that cannot be reached is an error.
    /// This is how a series is first read; once in the library it stays with the source it
    /// came from (`Title::from_tmdb`), since the two can number its episodes differently.
    pub async fn series(&self, key: &str, ids: &SeriesIds) -> Result<(Title, Vec<Episode>)> {
        let mut ids = ids.clone();
        // Known to TMDB alone so far: what TMDB knows it by elsewhere says whether TVmaze lists it too.
        if let (None, None, None, Some(tmdb), false) = (ids.tvmaze, ids.tvdb, &ids.imdb, ids.tmdb, key.is_empty()) {
            let v = self.get(&format!("{}/tv/{tmdb}/external_ids?api_key={key}", self.tmdb_base)).await?;
            ids.tvdb = v.get("tvdb_id").and_then(|x| x.as_u64()).map(|x| x as u32);
            ids.imdb = opt(&v, "imdb_id");
        }
        if let Some((mut title, episodes)) = self.tvmaze_listing(&ids).await? {
            // The same show, so known by the TMDB id it was asked for by as well.
            title.tmdb_id = title.tmdb_id.or(ids.tmdb);
            return Ok((title, episodes));
        }
        let tmdb = match ids.tmdb {
            Some(id) => Some(id),
            None => self.series_tmdb(key, ids.tvdb, ids.imdb.as_deref()).await?,
        };
        match (tmdb, key.is_empty()) {
            (Some(id), false) => self.tmdb_series(key, id).await,
            _ => bail!("not found"),
        }
    }

    /// A series as TVmaze lists it, by its TVmaze id or looked up by its TVDB or IMDb id;
    /// `None` when TVmaze lists no such show.
    pub async fn tvmaze_listing(&self, ids: &SeriesIds) -> Result<Option<(Title, Vec<Episode>)>> {
        if let Some(id) = ids.tvmaze {
            return Ok(Some(self.tvmaze_series(id).await?));
        }
        let asks = ids.tvdb.map(|id| format!("thetvdb={id}")).into_iter().chain(ids.imdb.as_ref().map(|id| format!("imdb={id}")));
        for ask in asks {
            if let Some(id) = self.tvmaze_show(&ask).await?.and_then(|show| show.get("id").and_then(|x| x.as_u64())) {
                return Ok(Some(self.tvmaze_series(id as u32).await?));
            }
        }
        Ok(None)
    }

    /// TVmaze's show for a lookup such as "thetvdb=81189"; `None` when it lists none.
    async fn tvmaze_show(&self, lookup: &str) -> Result<Option<Value>> {
        match self.get(&format!("{}/lookup/shows?{lookup}", self.tvmaze_base)).await {
            Ok(v) if v.get("id").is_some() => Ok(Some(v)),
            Ok(_) => Ok(None),
            Err(e) if e.to_string() == "not found" => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn tvmaze_series(&self, tvmaze_id: u32) -> Result<(Title, Vec<Episode>)> {
        let show = self.get(&format!("{}/shows/{tvmaze_id}?embed[]=cast&embed[]=images", self.tvmaze_base)).await?;
        let eps = self.get(&format!("{}/shows/{tvmaze_id}/episodes?specials=1", self.tvmaze_base)).await?;
        let aka = self.get(&format!("{}/shows/{tvmaze_id}/akas", self.tvmaze_base)).await.unwrap_or(Value::Null);
        let mut title = self.series_from(&show);
        title.details_at = crate::db::now();
        title.fanart = backdrop(&show);
        for a in aka.as_array().into_iter().flatten() {
            let english = a.get("country").is_none_or(|c| c.is_null() || matches!(s(c, "code").as_str(), "US" | "GB" | "CA" | "AU"));
            let name = s(a, "name");
            if english && !name.is_empty() && name != title.title && !title.alt_titles.contains(&name) {
                title.alt_titles.push(name);
            }
        }
        let mut episodes = Vec::new();
        let mut special_counter = 0;
        for e in eps.as_array().into_iter().flatten() {
            let regular = s(e, "type") == "regular";
            let (season, number) = if regular {
                (e.get("season").and_then(|x| x.as_u64()).unwrap_or(0) as u32, e.get("number").and_then(|x| x.as_u64()).unwrap_or(0) as u32)
            } else {
                special_counter += 1;
                (0, special_counter)
            };
            if regular && number == 0 {
                continue;
            }
            episodes.push(Episode {
                id: 0,
                title_id: 0,
                season,
                episode: number,
                absolute: None,
                title: s(e, "name"),
                overview: strip_html(&s(e, "summary")),
                air_date: opt(e, "airdate"),
                air_date_utc: opt(e, "airstamp"),
                runtime: e.get("runtime").and_then(|x| x.as_u64()).unwrap_or(title.runtime as u64) as u32,
                monitored: season > 0,
                file_id: None,
                still: e.get("image").and_then(|i| opt(i, "original").or_else(|| opt(i, "medium"))),
                rating: score(e.get("rating").and_then(|r| r.get("average"))),
            });
        }
        let mut seasons: Vec<u32> = episodes.iter().map(|e| e.season).collect();
        seasons.sort_unstable();
        seasons.dedup();
        title.seasons = seasons.into_iter().map(|n| SeasonInfo { number: n, monitored: n > 0 }).collect();
        Ok((title, episodes))
    }

    /// A film as the series source lists it, if it does: found by the film's IMDb id, or
    /// failing that by a show of exactly the same name that began in the same year and runs
    /// about as long in all. Only a listing of two or more episodes counts. `Ok(None)` means the source has no such show.
    pub async fn series_twin(&self, imdb_id: Option<&str>, title: &str, year: u32, runtime: u32) -> Result<Option<SeriesTwin>> {
        use crate::names::plain;
        let (mut show, mut by_name) = (None, false);
        if let Some(imdb) = imdb_id.filter(|i| !i.is_empty()) {
            match self.get(&format!("{}/lookup/shows?imdb={imdb}", self.tvmaze_base)).await {
                Ok(v) if v.get("id").is_some() => show = Some(v),
                Ok(_) => {}
                Err(e) if e.to_string() == "not found" => {}
                Err(e) => return Err(e),
            }
        }
        if show.is_none() && year > 0 {
            let found = self.get(&format!("{}/search/shows?q={}", self.tvmaze_base, urlencoding::encode(title.trim()))).await?;
            show = found.as_array().into_iter().flatten().filter_map(|r| r.get("show")).find(|s| plain(&self::s(s, "name")) == plain(title) && self::s(s, "premiered").starts_with(&year.to_string())).cloned();
            by_name = true;
        }
        let Some(show) = show else { return Ok(None) };
        let Some(id) = show.get("id").and_then(|x| x.as_u64()) else { return Ok(None) };
        let eps = self.get(&format!("{}/shows/{id}/episodes", self.tvmaze_base)).await?;
        let eps: Vec<&Value> = eps.as_array().into_iter().flatten().collect();
        if eps.len() < 2 {
            return Ok(None);
        }
        let minutes: u32 = eps.iter().map(|e| e.get("runtime").and_then(|x| x.as_u64()).unwrap_or(0) as u32).sum();
        // A name and a year can be shared by two different works. Without the id to tie them,
        // the episodes together have to run about as long as the film does.
        if by_name && (runtime == 0 || minutes == 0 || minutes * 10 < runtime * 6 || minutes * 10 > runtime * 14) {
            return Ok(None);
        }
        Ok(Some(SeriesTwin { tvmaze_id: id as u32, title: self::s(&show, "name"), episodes: eps.len() as u32, minutes }))
    }

    /// A series as TMDB lists it: its details, as a search result or in full.
    fn tv_from(&self, show: &Value) -> Title {
        let first = opt(show, "first_air_date");
        let mut t = Title {
            kind: Kind::Series,
            title: s(show, "name"),
            year: first.as_deref().and_then(|d| d.get(..4)).and_then(|y| y.parse().ok()).unwrap_or(0),
            overview: s(show, "overview"),
            status: match s(show, "status").as_str() {
                "Ended" | "Canceled" => "ended".into(),
                "Returning Series" => "continuing".into(),
                _ => "upcoming".into(),
            },
            runtime: show.get("episode_run_time").and_then(|r| r.as_array()).and_then(|r| r.first()).and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            tmdb_id: show.get("id").and_then(|x| x.as_u64()).map(|x| x as u32),
            tvdb_id: show.get("external_ids").and_then(|e| e.get("tvdb_id")).and_then(|x| x.as_u64()).map(|x| x as u32),
            imdb_id: show.get("external_ids").and_then(|e| opt(e, "imdb_id")),
            poster: opt(show, "poster_path").map(|p| format!("{TMDB_IMG}/w500{p}")),
            fanart: opt(show, "backdrop_path").map(|p| format!("{TMDB_IMG}/w1280{p}")),
            genres: show.get("genres").and_then(|g| g.as_array()).map(|g| g.iter().map(|x| s(x, "name")).collect()).unwrap_or_default(),
            monitored: true,
            minimum_availability: "released".into(),
            network: show.get("networks").and_then(|n| n.as_array()).and_then(|n| n.first()).and_then(|n| opt(n, "name")),
            season_folder: true,
            original_language: spool_core::parser::language_from_iso(&s(show, "original_language")).map(str::to_string),
            first_aired: first,
            rating: score(show.get("vote_average")),
            people: tv_people(show),
            from_tmdb: true,
            ..Default::default()
        };
        t.certification = show.get("content_ratings").and_then(|c| c.get("results")).and_then(|r| r.as_array()).and_then(|r| r.iter().find(|c| s(c, "iso_3166_1") == "US")).and_then(|c| opt(c, "rating"));
        // Releases carry an English name or, for anime, the romanised one.
        let original = s(show, "original_name");
        let alts = show.get("alternative_titles").and_then(|a| a.get("results")).and_then(|a| a.as_array());
        let names = std::iter::once(original).chain(alts.into_iter().flatten().filter(|a| matches!(s(a, "iso_3166_1").as_str(), "US" | "GB" | "CA" | "AU") || s(a, "type") == "Romanization").map(|a| s(a, "title")));
        for name in names {
            if !name.is_empty() && name != t.title && !t.alt_titles.contains(&name) {
                t.alt_titles.push(name);
            }
        }
        t
    }

    /// A series and its episodes as TMDB lists them, for one TVmaze does not.
    pub async fn tmdb_series(&self, key: &str, tmdb_id: u32) -> Result<(Title, Vec<Episode>)> {
        let base = format!("{}/tv/{tmdb_id}?api_key={key}", self.tmdb_base);
        let show = self.get(&format!("{base}&append_to_response=external_ids,alternative_titles,content_ratings,aggregate_credits")).await?;
        let mut title = self.tv_from(&show);
        title.details_at = crate::db::now();
        let numbers: Vec<u64> = show.get("seasons").and_then(|s| s.as_array()).into_iter().flatten().filter_map(|s| s.get("season_number").and_then(|x| x.as_u64())).collect();
        let mut episodes = Vec::new();
        // TMDB gives at most twenty seasons a call.
        for some in numbers.chunks(20) {
            let asked: Vec<String> = some.iter().map(|n| format!("season/{n}")).collect();
            let v = self.get(&format!("{base}&append_to_response={}", asked.join(","))).await?;
            for e in asked.iter().filter_map(|a| v.get(a)).filter_map(|season| season.get("episodes").and_then(|e| e.as_array())).flatten() {
                let (season, number) = (e.get("season_number").and_then(|x| x.as_u64()).unwrap_or(0) as u32, e.get("episode_number").and_then(|x| x.as_u64()).unwrap_or(0) as u32);
                if number == 0 {
                    continue;
                }
                episodes.push(Episode {
                    id: 0,
                    title_id: 0,
                    season,
                    episode: number,
                    absolute: None,
                    title: s(e, "name"),
                    overview: s(e, "overview"),
                    air_date: opt(e, "air_date"),
                    air_date_utc: None,
                    runtime: e.get("runtime").and_then(|x| x.as_u64()).unwrap_or(title.runtime as u64) as u32,
                    monitored: season > 0,
                    file_id: None,
                    still: opt(e, "still_path").map(|p| format!("{TMDB_IMG}/w780{p}")),
                    rating: score(e.get("vote_average")),
                });
            }
        }
        if title.runtime == 0 {
            title.runtime = episodes.iter().find(|e| e.season > 0 && e.runtime > 0).map(|e| e.runtime).unwrap_or(0);
        }
        let mut seasons: Vec<u32> = episodes.iter().map(|e| e.season).collect();
        seasons.sort_unstable();
        seasons.dedup();
        title.seasons = seasons.into_iter().map(|n| SeasonInfo { number: n, monitored: n > 0 }).collect();
        Ok((title, episodes))
    }
}

/// A score out of ten, when the source has one.
fn score(v: Option<&Value>) -> Option<f32> {
    v.and_then(|x| x.as_f64()).filter(|x| *x > 0.0).map(|x| (x * 10.0).round() as f32 / 10.0)
}

/// The leading cast, then the director and writers.
fn movie_people(m: &Value) -> Vec<Person> {
    let photo = |p: &Value| opt(p, "profile_path").map(|path| format!("{TMDB_IMG}/w185{path}"));
    let id = |p: &Value| p.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
    let mut out: Vec<Person> = vec![];
    let credits = m.get("credits");
    for p in credits.and_then(|c| c.get("cast")).and_then(|c| c.as_array()).into_iter().flatten().take(20) {
        out.push(Person { id: id(p), name: s(p, "name"), role: s(p, "character"), kind: "actor".into(), photo: photo(p) });
    }
    for p in credits.and_then(|c| c.get("crew")).and_then(|c| c.as_array()).into_iter().flatten() {
        let kind = match s(p, "job").as_str() {
            "Director" => "director",
            "Screenplay" | "Writer" | "Story" => "writer",
            _ => continue,
        };
        if !out.iter().any(|o| o.id == id(p) && o.kind == kind) {
            out.push(Person { id: id(p), name: s(p, "name"), role: s(p, "job"), kind: kind.into(), photo: photo(p) });
        }
    }
    out.retain(|p| !p.name.is_empty());
    out
}

/// A TMDB series' leading cast.
fn tv_people(show: &Value) -> Vec<Person> {
    let cast = show.get("aggregate_credits").and_then(|c| c.get("cast")).and_then(|c| c.as_array());
    let mut out: Vec<Person> = vec![];
    for p in cast.into_iter().flatten().take(20) {
        let role = p.get("roles").and_then(|r| r.as_array()).and_then(|r| r.first()).map(|r| s(r, "character")).unwrap_or_default();
        out.push(Person { id: p.get("id").and_then(|x| x.as_u64()).unwrap_or(0), name: s(p, "name"), role, kind: "actor".into(), photo: opt(p, "profile_path").map(|path| format!("{TMDB_IMG}/w185{path}")) });
    }
    out.retain(|p| !p.name.is_empty());
    out
}

fn series_people(show: &Value) -> Vec<Person> {
    let cast = show.get("_embedded").and_then(|e| e.get("cast")).and_then(|c| c.as_array());
    let mut out: Vec<Person> = vec![];
    for c in cast.into_iter().flatten().take(20) {
        let (Some(p), character) = (c.get("person"), c.get("character")) else { continue };
        let id = p.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
        if out.iter().any(|o| o.id == id) || s(p, "name").is_empty() {
            continue;
        }
        out.push(Person {
            id,
            name: s(p, "name"),
            role: character.map(|ch| s(ch, "name")).unwrap_or_default(),
            kind: "actor".into(),
            photo: p.get("image").and_then(|i| opt(i, "medium").or_else(|| opt(i, "original"))),
        });
    }
    out
}

/// The show's main wide image, when TVmaze has one.
fn backdrop(show: &Value) -> Option<String> {
    let images = show.get("_embedded").and_then(|e| e.get("images")).and_then(|i| i.as_array())?;
    let of = |i: &Value| i.get("resolutions").and_then(|r| r.get("original")).and_then(|o| opt(o, "url"));
    let wide = |i: &&Value| s(i, "type") == "background";
    images.iter().filter(wide).find(|i| i.get("main").and_then(|m| m.as_bool()) == Some(true)).or_else(|| images.iter().find(wide)).and_then(of)
}

/// "Dune 2021" -> ("Dune", Some(2021)).
fn split_year(q: &str) -> (String, Option<u32>) {
    let t = q.trim();
    if let Some((head, tail)) = t.rsplit_once(' ') {
        let tail = tail.trim_matches(['(', ')']);
        if tail.len() == 4 {
            if let Ok(y) = tail.parse::<u32>() {
                if (1880..=2100).contains(&y) && !head.trim().is_empty() {
                    return (head.trim().to_string(), Some(y));
                }
            }
        }
    }
    (t.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_year_from_query() {
        assert_eq!(split_year("Dune 2021"), ("Dune".into(), Some(2021)));
        assert_eq!(split_year("Dune (2021)"), ("Dune".into(), Some(2021)));
        assert_eq!(split_year("2001"), ("2001".into(), None));
        assert_eq!(split_year("Blade Runner 2049"), ("Blade Runner".into(), Some(2049)));
        assert_eq!(strip_html("<p>Hello <b>there</b></p>"), "Hello there");
    }
}
