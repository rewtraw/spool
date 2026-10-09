//! Title metadata: TMDB for movies, TVmaze for television.

use crate::models::{Episode, Kind, SeasonInfo, Title};
use anyhow::{anyhow, bail, Result};
use serde_json::Value;

const TMDB: &str = "https://api.themoviedb.org/3";
const TVMAZE: &str = "https://api.tvmaze.com";
const TMDB_IMG: &str = "https://image.tmdb.org/t/p";

#[derive(Clone)]
pub struct Metadata {
    pub http: reqwest::Client,
    pub tmdb_base: String,
    pub tvmaze_base: String,
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
        Metadata { http, tmdb_base: TMDB.into(), tvmaze_base: TVMAZE.into() }
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

    pub async fn movie(&self, key: &str, tmdb_id: u32) -> Result<Title> {
        if key.is_empty() {
            bail!("add a TMDB API key in Settings to fetch movie details");
        }
        let v = self.get(&format!("{}/movie/{tmdb_id}?api_key={key}&append_to_response=release_dates,alternative_titles,external_ids", self.tmdb_base)).await?;
        Ok(self.movie_from(&v))
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
        }
    }

    pub async fn search_series(&self, query: &str) -> Result<Vec<Title>> {
        if let Some(id) = query.trim().strip_prefix("tvdb:").and_then(|i| i.trim().parse::<u32>().ok()) {
            return Ok(vec![self.series_by_tvdb(id).await?.0]);
        }
        let v = self.get(&format!("{}/search/shows?q={}", self.tvmaze_base, urlencoding::encode(query.trim()))).await?;
        Ok(v.as_array().map(|a| a.iter().take(20).filter_map(|r| r.get("show")).map(|sh| self.series_from(sh)).collect()).unwrap_or_default())
    }

    /// Series details and every episode, specials included.
    pub async fn series(&self, tvmaze_id: u32) -> Result<(Title, Vec<Episode>)> {
        let show = self.get(&format!("{}/shows/{tvmaze_id}", self.tvmaze_base)).await?;
        let eps = self.get(&format!("{}/shows/{tvmaze_id}/episodes?specials=1", self.tvmaze_base)).await?;
        let aka = self.get(&format!("{}/shows/{tvmaze_id}/akas", self.tvmaze_base)).await.unwrap_or(Value::Null);
        let mut title = self.series_from(&show);
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
            });
        }
        let mut seasons: Vec<u32> = episodes.iter().map(|e| e.season).collect();
        seasons.sort_unstable();
        seasons.dedup();
        title.seasons = seasons.into_iter().map(|n| SeasonInfo { number: n, monitored: n > 0 }).collect();
        Ok((title, episodes))
    }

    pub async fn series_by_tvdb(&self, tvdb_id: u32) -> Result<(Title, Vec<Episode>)> {
        let show = self.get(&format!("{}/lookup/shows?thetvdb={tvdb_id}", self.tvmaze_base)).await?;
        let id = show.get("id").and_then(|x| x.as_u64()).ok_or_else(|| anyhow!("not found"))? as u32;
        self.series(id).await
    }
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
