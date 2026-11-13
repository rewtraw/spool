//! Albums as MusicBrainz knows them. An album is wanted by its release group, which stands
//! for every edition of it; the editions say how many tracks it has.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

const BASE: &str = "https://musicbrainz.org/ws/2";

/// MusicBrainz asks that each program name itself and a way to reach whoever runs it, and
/// allows one request a second.
const AGENT: &str = concat!("Spool/", env!("CARGO_PKG_VERSION"), " ( https://github.com/rewtraw/spool )");

static LAST: parking_lot::Mutex<Option<std::time::Instant>> = parking_lot::Mutex::new(None);

async fn get<T: for<'de> Deserialize<'de>>(http: &reqwest::Client, url: &str) -> Result<T> {
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
    let resp = http.get(url).header(reqwest::header::USER_AGENT, AGENT).timeout(std::time::Duration::from_secs(20)).send().await.map_err(|e| anyhow!("MusicBrainz: {}", e.without_url()))?;
    if !resp.status().is_success() {
        return Err(anyhow!("MusicBrainz answered HTTP {}", resp.status().as_u16()));
    }
    resp.json().await.map_err(|e| anyhow!("MusicBrainz sent something unexpected: {}", e.without_url()))
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
    let url = format!("{BASE}/release-group?fmt=json&limit=25&query={}", urlencoding::encode(q));
    let s: Search = get(http, &url).await?;
    Ok(s.release_groups.into_iter().map(Group::from).collect())
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
    fn track_counts_come_from_official_editions() {
        let r: Releases = serde_json::from_str(r#"{"releases":[{"status":"Official","media":[{"track-count":10},{"track-count":2}]},{"status":"Official","media":[{"track-count":11}]},{"status":"Bootleg","media":[{"track-count":40}]}]}"#).unwrap();
        assert_eq!(counts(&r.releases), Some((11, 12)));
        assert_eq!(counts(&[]), None);
    }
}
