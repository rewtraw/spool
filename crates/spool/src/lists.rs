//! Lists on Letterboxd, followed: every film on one is added to the library, and so is every
//! film put on it later. A watchlist, a critic's list, a festival's, the Criterion Collection
//! or "1001 Movies You Must See": Letterboxd has a list for most things, and anyone's can be
//! followed without an account.
//!
//! Letterboxd has no API for this, so its pages are read as a browser reads them: each page of
//! the list for the films' names, then each film's own page, once, for the TMDB id Letterboxd
//! keeps there. What a film's page said is remembered for every list, so a long list costs a
//! few minutes the first time and a few page reads after that. Pages are read one at a time,
//! a second apart.
//!
//! A film added from a list and later removed from the library is not added again: the list
//! remembers what it brought.

use crate::app::{App, Event};
use crate::db::now;
use crate::models::Kind;
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Letterboxd turns away readers that do not say they are a browser.
const AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
/// Between one page and the next.
const PAUSE: Duration = Duration::from_millis(1000);
/// No list is longer than this many pages of a hundred films.
const MOST_PAGES: usize = 60;
/// One list is read at a time: two readers of a list would each forget what the other added,
/// and two lists adding the same film at once would add it twice.
static READING: std::sync::LazyLock<tokio::sync::Mutex<()>> = std::sync::LazyLock::new(Default::default);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct List {
    #[serde(skip_deserializing)]
    pub id: i64,
    /// The list's address, as https://letterboxd.com/{member}/list/{name}/ or
    /// https://letterboxd.com/{member}/watchlist/.
    pub url: String,
    pub name: String,
    /// The profile films added from it get; 0 for the usual one.
    pub profile_id: i64,
    /// How many films it had when it was last read.
    pub films: usize,
    /// TMDB ids of the films it added, which are not added again once removed.
    pub added: Vec<u32>,
    /// Films on it that are not films TMDB lists, or could not be read: their Letterboxd names.
    pub missed: Vec<String>,
    pub checked_at: i64,
    pub error: String,
    pub added_at: i64,
}

/// A list's address made plain, from what someone pasted.
pub fn list_url(pasted: &str) -> Result<String> {
    let s = pasted.trim();
    let rest = s.trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.");
    let Some(path) = rest.strip_prefix("letterboxd.com/") else { bail!("that is not a Letterboxd link: it should start with https://letterboxd.com/") };
    let parts: Vec<&str> = path.split(['?', '#']).next().unwrap_or("").split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [member, "watchlist", ..] => Ok(format!("https://letterboxd.com/{member}/watchlist/")),
        [member, "list", name, ..] => Ok(format!("https://letterboxd.com/{member}/list/{name}/")),
        [member, "films", ..] => Ok(format!("https://letterboxd.com/{member}/films/")),
        _ => bail!("that Letterboxd link is not a list: open the list and copy its address"),
    }
}

/// The films on one page of a list: each one's Letterboxd name ("the-godfather").
fn slugs(html: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for marker in ["data-item-slug=\"", "data-film-slug=\""] {
        for part in html.split(marker).skip(1) {
            if let Some(slug) = part.split('"').next().filter(|s| !s.is_empty()) {
                if !out.iter().any(|o| o == slug) {
                    out.push(slug.to_string());
                }
            }
        }
    }
    out
}

/// What a film's page says: its TMDB id and whether TMDB lists it as a film or a series.
fn tmdb_of(html: &str) -> Option<(u32, Kind)> {
    let id: u32 = html.split("data-tmdb-id=\"").nth(1)?.split('"').next()?.parse().ok()?;
    let kind = match html.split("data-tmdb-type=\"").nth(1).and_then(|r| r.split('"').next()) {
        Some("tv") => Kind::Series,
        _ => Kind::Movie,
    };
    Some((id, kind))
}

/// The list's name, from what its page gives for sharing.
fn name_of(html: &str) -> Option<String> {
    let og = html.split("property=\"og:title\" content=\"").nth(1)?.split('"').next()?;
    let name = og.split(", a list of films by").next().unwrap_or(og).trim_start_matches("&#8206;").trim_start_matches('\u{200e}').trim();
    Some(name.replace("&amp;", "&").replace("&#039;", "'").replace("&quot;", "\"")).filter(|n| !n.is_empty())
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<List> {
    let id: i64 = r.get(0)?;
    let data: String = r.get(1)?;
    let mut l: List = serde_json::from_str(&data).unwrap_or_default();
    l.id = id;
    Ok(l)
}

impl App {
    pub fn lists(&self) -> Result<Vec<List>> {
        self.db.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM lists ORDER BY id")?;
            let rows = s.query_map([], row)?;
            rows.collect()
        })
    }

    pub fn list(&self, id: i64) -> Result<Option<List>> {
        self.db.with(|c| c.query_row("SELECT id, data FROM lists WHERE id = ?1", [id], row).optional())
    }

    /// Save what a read of a list found, keeping what was changed meanwhile: its profile, and
    /// whether it is followed at all. Says whether it still is.
    fn save_read(&self, l: &mut List) -> Result<bool> {
        let Some(fresh) = self.list(l.id)? else { return Ok(false) };
        l.profile_id = fresh.profile_id;
        for id in fresh.added {
            if !l.added.contains(&id) {
                l.added.push(id);
            }
        }
        self.save_list(l)?;
        Ok(true)
    }

    fn save_list(&self, l: &mut List) -> Result<()> {
        let data = serde_json::to_string(l)?;
        if l.id == 0 {
            l.added_at = now();
            let data = serde_json::to_string(l)?;
            l.id = self.db.with(|c| {
                c.execute("INSERT INTO lists(url, added_at, data) VALUES (?1,?2,?3)", params![l.url, l.added_at, data])?;
                Ok(c.last_insert_rowid())
            })?;
        } else {
            self.db.with(|c| c.execute("UPDATE lists SET data = ?2 WHERE id = ?1", params![l.id, data]))?;
        }
        Ok(())
    }

    /// Follow a list. Its films are added in the background, a page at a time.
    pub async fn list_add(&self, pasted: &str, profile_id: i64) -> Result<List> {
        let url = list_url(pasted)?;
        if self.lists()?.iter().any(|l| l.url == url) {
            bail!("that list is followed already");
        }
        let first = self.letterboxd_page(&url).await?;
        if slugs(&first).is_empty() {
            bail!("that list has no films on it, or is private");
        }
        let mut l = List { url: url.clone(), name: name_of(&first).unwrap_or_else(|| url.trim_end_matches('/').rsplit('/').next().unwrap_or("").replace('-', " ")), profile_id, ..Default::default() };
        self.save_list(&mut l)?;
        let (app, id) = (self.clone(), l.id);
        tokio::spawn(async move {
            if let Err(e) = app.list_read(id).await {
                tracing::warn!(list = id, error = %format!("{e:#}"), "a Letterboxd list could not be read");
            }
        });
        Ok(l)
    }

    pub fn list_change(&self, id: i64, profile_id: i64) -> Result<List> {
        let mut l = self.list(id)?.ok_or_else(|| anyhow!("no such list"))?;
        l.profile_id = profile_id;
        self.save_list(&mut l)?;
        Ok(l)
    }

    /// Stop following a list. What it added stays.
    pub fn list_remove(&self, id: i64) -> Result<()> {
        self.db.with(|c| c.execute("DELETE FROM lists WHERE id = ?1", [id]))?;
        Ok(())
    }

    async fn letterboxd_page(&self, url: &str) -> Result<String> {
        let r = self.http.get(url).header("User-Agent", AGENT).header("Accept-Language", "en").timeout(Duration::from_secs(30)).send().await.with_context(|| format!("reading {url}"))?;
        match r.status().as_u16() {
            200 => Ok(r.text().await?),
            404 => bail!("Letterboxd has no page at {url}"),
            s => bail!("Letterboxd answered {s} for {url}"),
        }
    }

    /// A film's TMDB id, from what Letterboxd's page for it said, read once.
    async fn letterboxd_film(&self, slug: &str) -> Result<Option<(u32, Kind)>> {
        let known: Option<(Option<u32>, String)> = self.db.with(|c| c.query_row("SELECT tmdb_id, kind FROM letterboxd_films WHERE slug = ?1", [slug], |r| Ok((r.get(0)?, r.get(1)?))).optional())?;
        if let Some((id, kind)) = known {
            return Ok(id.map(|i| (i, if kind == "series" { Kind::Series } else { Kind::Movie })));
        }
        tokio::time::sleep(PAUSE).await;
        let html = self.letterboxd_page(&format!("https://letterboxd.com/film/{slug}/")).await?;
        let found = tmdb_of(&html);
        self.db.with(|c| c.execute("INSERT OR REPLACE INTO letterboxd_films(slug, tmdb_id, kind, checked_at) VALUES (?1,?2,?3,?4)", params![slug, found.map(|f| f.0), found.map(|f| f.1.as_str()).unwrap_or("movie"), now()]))?;
        Ok(found)
    }

    /// Read a list through and add what is on it that the library lacks.
    pub async fn list_read(&self, id: i64) -> Result<usize> {
        let _reading = READING.lock().await;
        let mut l = self.list(id)?.ok_or_else(|| anyhow!("no such list"))?;
        let mut films: Vec<String> = vec![];
        for page in 1..=MOST_PAGES {
            let url = if page == 1 { l.url.clone() } else { format!("{}page/{page}/", l.url) };
            if page > 1 {
                tokio::time::sleep(PAUSE).await;
            }
            let html = match self.letterboxd_page(&url).await {
                Ok(h) => h,
                // Past the last page there is no page.
                Err(e) if page > 1 && format!("{e:#}").contains("has no page") => break,
                // Anything else is not the end of the list: what is known of it stays as it was.
                Err(e) => {
                    l.error = format!("{e:#}");
                    l.checked_at = now();
                    self.save_read(&mut l)?;
                    return Err(e);
                }
            };
            let on_page = slugs(&html);
            let before = films.len();
            films.extend(on_page.into_iter().filter(|s| !films.contains(s)).collect::<Vec<_>>());
            if films.len() == before || !html.contains(&format!("/page/{}/", page + 1)) {
                break;
            }
        }
        let g = self.settings.general();
        let profile = if l.profile_id > 0 { l.profile_id } else { g.default_movie_profile };
        let mut added = 0;
        let mut missed = vec![];
        for slug in &films {
            // No longer followed: nothing more is added from it.
            if self.list(id)?.is_none() {
                return Ok(added);
            }
            let found = match self.letterboxd_film(slug).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::debug!(film = %slug, error = %format!("{e:#}"), "a film's page on Letterboxd could not be read");
                    missed.push(slug.clone());
                    continue;
                }
            };
            // Letterboxd lists some series too; only films are added from lists.
            let Some((tmdb, Kind::Movie)) = found else {
                missed.push(slug.clone());
                continue;
            };
            if l.added.contains(&tmdb) || self.list(id)?.is_some_and(|f| f.added.contains(&tmdb)) || self.db.find_title(Kind::Movie, Some(tmdb), None)?.is_some() {
                continue;
            }
            let profile = self.list(id)?.map(|f| f.profile_id).filter(|p| *p > 0).unwrap_or(profile);
            match self.add_title(Kind::Movie, Some(tmdb), None, profile, true, true).await {
                Ok(t) => {
                    added += 1;
                    l.added.push(tmdb);
                    let _ = self.db.add_history(Some(t.id), "list_added", serde_json::json!({"list": l.name, "url": l.url}));
                }
                Err(e) => {
                    tracing::warn!(film = %slug, error = %format!("{e:#}"), "a film on a list could not be added");
                    missed.push(slug.clone());
                }
            }
            // Saved as it goes: a long list is a long read, and what was added stays added.
            if added % 10 == 1 && !self.save_read(&mut l)? {
                return Ok(added);
            }
        }
        l.films = films.len();
        l.missed = missed;
        l.error.clear();
        l.checked_at = now();
        // Removed while it was being read: it is not brought back.
        self.save_read(&mut l)?;
        self.emit(Event::Title { id: 0 });
        Ok(added)
    }

    /// Read every list followed again for films put on it since.
    pub async fn lists_task(&self) -> Result<String> {
        let lists = self.lists()?;
        if lists.is_empty() {
            return Ok("no lists followed".into());
        }
        let (mut added, mut failed) = (0, 0);
        for l in &lists {
            match self.list_read(l.id).await {
                Ok(n) => added += n,
                Err(_) => failed += 1,
            }
        }
        Ok(format!("{} {} read, {added} {} added{}", lists.len(), if lists.len() == 1 { "list" } else { "lists" }, if added == 1 { "film" } else { "films" }, if failed > 0 { format!(", {failed} could not be read") } else { String::new() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_list_addresses() {
        assert_eq!(list_url("https://letterboxd.com/dave/list/official-top-250-narrative-feature-films/").unwrap(), "https://letterboxd.com/dave/list/official-top-250-narrative-feature-films/");
        assert_eq!(list_url("letterboxd.com/dave/list/imdb-top-250/page/3/?x=1").unwrap(), "https://letterboxd.com/dave/list/imdb-top-250/");
        assert_eq!(list_url("https://www.letterboxd.com/someone/watchlist").unwrap(), "https://letterboxd.com/someone/watchlist/");
        assert!(list_url("https://letterboxd.com/film/chinatown/").is_err());
        assert!(list_url("https://example.com/list/").is_err());
    }

    #[test]
    fn reads_films_and_ids_from_pages() {
        let page = r#"<li data-item-name="The Godfather (1972)" data-item-slug="the-godfather" data-item-link="/film/the-godfather/"></li><div data-item-slug="chinatown"></div><div data-film-slug="the-godfather"></div>"#;
        assert_eq!(slugs(page), ["the-godfather", "chinatown"]);
        assert_eq!(tmdb_of(r#"<body class="film backdropped" data-tmdb-type="movie" data-tmdb-id="829">"#), Some((829, Kind::Movie)));
        assert_eq!(tmdb_of(r#"<body data-tmdb-type="tv" data-tmdb-id="1399">"#), Some((1399, Kind::Series)));
        assert_eq!(tmdb_of("<body>"), None);
        assert_eq!(name_of(r#"<meta property="og:title" content="IMDb Top 250" />"#).as_deref(), Some("IMDb Top 250"));
        assert_eq!(name_of(r#"<meta property="og:title" content="&#8206;Films &amp; more, a list of films by dave" />"#).as_deref(), Some("Films & more"));
    }
}
