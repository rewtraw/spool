//! Albums as MusicBrainz knows them. An album is wanted by its release group, which stands
//! for every edition of it; the editions say how many tracks it has.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

const BASE: &str = "https://musicbrainz.org/ws/2";

/// MusicBrainz asks that each program name itself and a way to reach whoever runs it, and
/// allows one request a second.
const AGENT: &str = concat!("Spool/", env!("CARGO_PKG_VERSION"), " ( https://github.com/rewtraw/spool )");

static LAST: parking_lot::Mutex<Option<std::time::Instant>> = parking_lot::Mutex::new(None);

/// What MusicBrainz said, kept an hour: asking again is a second's wait each time, and the
/// same things are asked for again and again while someone searches and then wants an album.
static KEPT: std::sync::LazyLock<parking_lot::Mutex<std::collections::HashMap<String, (std::time::Instant, String)>>> = std::sync::LazyLock::new(Default::default);
const KEEP: std::time::Duration = std::time::Duration::from_secs(3600);

async fn get<T: for<'de> Deserialize<'de>>(http: &reqwest::Client, url: &str) -> Result<T> {
    let kept = KEPT.lock().get(url).filter(|(at, _)| at.elapsed() < KEEP).map(|(_, body)| body.clone());
    let body = match kept {
        Some(b) => b,
        None => {
            let b = fetch(http, url).await?;
            let mut all = KEPT.lock();
            all.retain(|_, (at, _)| at.elapsed() < KEEP);
            if all.len() < 500 {
                all.insert(url.to_string(), (std::time::Instant::now(), b.clone()));
            }
            b
        }
    };
    serde_json::from_str(&body).map_err(|e| anyhow!("MusicBrainz sent something unexpected: {e}"))
}

async fn fetch(http: &reqwest::Client, url: &str) -> Result<String> {
    // It answers 503 when asked too often or when busy; a moment later usually does.
    for _ in 0..2 {
        match get_once(http, url).await {
            Err(Busy) => tokio::time::sleep(std::time::Duration::from_millis(1500)).await,
            Ok(r) => return r,
        }
    }
    get_once(http, url).await.unwrap_or_else(|Busy| Err(anyhow!("MusicBrainz is busy; try again in a moment")))
}

struct Busy;

async fn get_once(http: &reqwest::Client, url: &str) -> std::result::Result<Result<String>, Busy> {
    let wait = {
        let mut last = LAST.lock();
        let now = std::time::Instant::now();
        let at = last.map(|t| t + std::time::Duration::from_millis(1100)).filter(|t| *t > now).unwrap_or(now);
        *last = Some(at);
        at - now
    };
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
    let resp = match http.get(url).header(reqwest::header::USER_AGENT, AGENT).timeout(std::time::Duration::from_secs(20)).send().await {
        Ok(r) => r,
        Err(e) => return Ok(Err(anyhow!("MusicBrainz: {}", e.without_url()))),
    };
    if resp.status().as_u16() == 503 {
        return Err(Busy);
    }
    if !resp.status().is_success() {
        return Ok(Err(anyhow!("MusicBrainz answered HTTP {}", resp.status().as_u16())));
    }
    Ok(resp.text().await.map_err(|e| anyhow!("MusicBrainz: {}", e.without_url())))
}

/// What was typed, as plain words to MusicBrainz's search: its query language takes `!`, `/`,
/// `:`, quotes and brackets for operators, so "Neu!" or "Either/Or" would be refused.
fn escape(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    for c in q.chars() {
        if "+-&|!(){}[]^\"~*?:\\/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// An album, as a search finds it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Group {
    pub mbid: String,
    pub title: String,
    pub artist: String,
    pub artist_mbid: String,
    pub year: i32,
    /// "Album", "EP", "Single" and so on, with any second type after it: "Album · Live".
    pub kind: String,
}

impl Group {
    pub fn cover(&self) -> String {
        format!("https://coverartarchive.org/release-group/{}/front-250", self.mbid)
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Credit {
    name: String,
    joinphrase: String,
    artist: CreditArtist,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CreditArtist {
    id: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
struct RawGroup {
    id: String,
    title: String,
    primary_type: Option<String>,
    secondary_types: Vec<String>,
    first_release_date: Option<String>,
    artist_credit: Vec<Credit>,
}

impl From<RawGroup> for Group {
    fn from(g: RawGroup) -> Group {
        let artist = g.artist_credit.iter().map(|c| format!("{}{}", c.name, c.joinphrase)).collect::<String>().trim().to_string();
        let kind = g.primary_type.into_iter().chain(g.secondary_types).collect::<Vec<_>>().join(" · ");
        Group {
            mbid: g.id,
            title: g.title,
            artist,
            artist_mbid: g.artist_credit.first().map(|c| c.artist.id.clone()).unwrap_or_default(),
            year: g.first_release_date.as_deref().and_then(|d| d.get(..4)).and_then(|y| y.parse().ok()).unwrap_or(0),
            kind,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
struct Search {
    release_groups: Vec<RawGroup>,
}

/// Albums matching what was typed: an album, an artist, or both.
pub async fn search(http: &reqwest::Client, q: &str) -> Result<Vec<Group>> {
    let url = format!("{BASE}/release-group?fmt=json&limit=25&query={}", urlencoding::encode(&escape(q)));
    let s: Search = get(http, &url).await?;
    Ok(s.release_groups.into_iter().map(Group::from).collect())
}

/// An artist, as a search finds one.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Artist {
    pub mbid: String,
    pub name: String,
    /// What tells two of the same name apart: "UK rock band".
    pub disambiguation: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawArtist {
    id: String,
    name: String,
    score: u32,
    disambiguation: String,
    #[serde(rename = "sort-name")]
    sort_name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Artists {
    artists: Vec<RawArtist>,
}

/// Words as compared: lower case, without accents or punctuation, "&" as "and".
fn plain(s: &str) -> String {
    deunicode::deunicode(s).to_lowercase().replace('&', " and ").split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// The artist named by what was typed, when it names one outright: "boards of canada",
/// "Beatles". Typing more than a name ("radiohead ok computer") is a search for albums.
pub async fn artist(http: &reqwest::Client, q: &str) -> Result<Option<Artist>> {
    let url = format!("{BASE}/artist?fmt=json&limit=5&query={}", urlencoding::encode(&escape(q)));
    let a: Artists = get(http, &url).await?;
    Ok(named(&a.artists, q))
}

fn named(found: &[RawArtist], q: &str) -> Option<Artist> {
    let q = plain(q);
    let strip = |s: String| s.strip_prefix("the ").map(str::to_string).unwrap_or(s);
    // The best scored of those whose name is what was typed, "The" or not.
    found
        .iter()
        .filter(|a| a.score >= 80 && (strip(plain(&a.name)) == strip(q.clone()) || plain(&a.sort_name) == q))
        .max_by_key(|a| a.score)
        .map(|a| Artist { mbid: a.id.clone(), name: a.name.clone(), disambiguation: a.disambiguation.clone() })
}

/// An artist's own albums and EPs, oldest first: not live recordings, compilations,
/// remixes, demos or bootlegs, which have second types.
pub async fn discography(http: &reqwest::Client, artist_mbid: &str) -> Result<Vec<Group>> {
    let url = format!("{BASE}/release-group?fmt=json&limit=100&inc=artist-credits&type=album|ep&release-group-status=website-default&artist={}", urlencoding::encode(artist_mbid));
    let s: Search = get(http, &url).await?;
    let mut out: Vec<Group> = s.release_groups.into_iter().map(Group::from).filter(|g| !g.kind.contains(" · ")).collect();
    // Albums before EPs, and each oldest first; one with no date last.
    out.sort_by_key(|g| (g.kind != "Album", if g.year > 0 { g.year } else { i32::MAX }, g.title.to_lowercase()));
    Ok(out)
}

pub async fn group(http: &reqwest::Client, mbid: &str) -> Result<Group> {
    let url = format!("{BASE}/release-group/{}?fmt=json&inc=artist-credits", urlencoding::encode(mbid));
    Ok(get::<RawGroup>(http, &url).await?.into())
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
struct Releases {
    releases: Vec<Release>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
struct Release {
    status: Option<String>,
    media: Vec<Medium>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
struct Medium {
    track_count: u32,
}

/// How many tracks the album's editions have, fewest and most. Official editions only,
/// when there are any; bootlegs and promos run to anything.
pub async fn track_counts(http: &reqwest::Client, mbid: &str) -> Result<Option<(u32, u32)>> {
    let url = format!("{BASE}/release?fmt=json&limit=100&inc=media&release-group={}", urlencoding::encode(mbid));
    let r: Releases = get(http, &url).await?;
    Ok(counts(&r.releases))
}

fn counts(releases: &[Release]) -> Option<(u32, u32)> {
    let official: Vec<&Release> = releases.iter().filter(|r| r.status.as_deref() == Some("Official")).collect();
    let pick = if official.is_empty() { releases.iter().collect() } else { official };
    let n: Vec<u32> = pick.iter().map(|r| r.media.iter().map(|m| m.track_count).sum()).filter(|n| *n > 0).collect();
    Some((*n.iter().min()?, *n.iter().max()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_reads_its_artists_and_year() {
        let raw: RawGroup = serde_json::from_str(r#"{"id":"x","title":"Watch the Throne","primary-type":"Album","secondary-types":["Live"],"first-release-date":"2011-08-08","artist-credit":[{"name":"JAY-Z","joinphrase":" & ","artist":{"id":"a1"}},{"name":"Kanye West","joinphrase":"","artist":{"id":"a2"}}]}"#).unwrap();
        let g = Group::from(raw);
        assert_eq!((g.artist.as_str(), g.artist_mbid.as_str(), g.year, g.kind.as_str()), ("JAY-Z & Kanye West", "a1", 2011, "Album · Live"));
    }

    #[test]
    fn what_was_typed_is_searched_as_words() {
        assert_eq!(escape("Neu! 75"), "Neu\\! 75");
        assert_eq!(escape("Either/Or"), "Either\\/Or");
        assert_eq!(escape("AC/DC: Back in Black"), "AC\\/DC\\: Back in Black");
    }

    #[test]
    fn an_artist_is_taken_only_when_named_outright() {
        let a = |name: &str, score: u32| RawArtist { id: name.into(), name: name.into(), sort_name: name.into(), score, ..Default::default() };
        let found = [a("Boards of Canada", 100), a("Boards", 70)];
        assert_eq!(named(&found, "boards of canada").map(|x| x.name), Some("Boards of Canada".into()));
        assert_eq!(named(&[a("The Beatles", 100)], "beatles").map(|x| x.name), Some("The Beatles".into()));
        assert_eq!(named(&[a("Simon & Garfunkel", 100)], "simon and garfunkel").map(|x| x.name), Some("Simon & Garfunkel".into()));
        assert_eq!(named(&[a("Radiohead", 90)], "radiohead ok computer"), None);
    }

    #[test]
    fn track_counts_come_from_official_editions() {
        let r: Releases = serde_json::from_str(r#"{"releases":[{"status":"Official","media":[{"track-count":10},{"track-count":2}]},{"status":"Official","media":[{"track-count":11}]},{"status":"Bootleg","media":[{"track-count":40}]}]}"#).unwrap();
        assert_eq!(counts(&r.releases), Some((11, 12)));
        assert_eq!(counts(&[]), None);
    }
}
