//! Newznab indexer client: search and recent-release feeds.

use crate::models::{Indexer, Release};
use anyhow::{anyhow, bail, Result};
use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Clone, Debug)]
pub enum Query {
    /// Latest releases in the indexer's movie categories.
    RecentMovies,
    RecentTv,
    Movie { imdb_id: Option<String>, title: String, year: u32 },
    Episode { tvdb_id: Option<u32>, title: String, season: u32, episode: u32 },
    Season { tvdb_id: Option<u32>, title: String, season: u32 },
    /// Plain words against release names, for what an indexer has not tagged with an id.
    TvText { text: String },
    /// The same for a film. Anime films are usually filed under television, so that category
    /// is searched along with the movie ones.
    MovieText { text: String },
}

fn cats(v: &[u32]) -> String {
    v.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",")
}

pub fn build_url(ix: &Indexer, q: &Query) -> String {
    let base = ix.url.trim_end_matches('/');
    let base = if base.ends_with("/api") { base.to_string() } else { format!("{base}/api") };
    let enc = |s: &str| urlencoding::encode(s).into_owned();
    let params = match q {
        Query::RecentMovies => format!("t=movie&cat={}", cats(&ix.movie_categories)),
        Query::RecentTv => format!("t=tvsearch&cat={}", cats(&ix.tv_categories)),
        Query::Movie { imdb_id: Some(id), .. } if id.starts_with("tt") => format!("t=movie&cat={}&imdbid={}", cats(&ix.movie_categories), &id[2..]),
        Query::Movie { title, year, .. } => {
            let q = if *year > 0 { format!("{title} {year}") } else { title.clone() };
            format!("t=search&cat={}&q={}", cats(&ix.movie_categories), enc(&q))
        }
        Query::Episode { tvdb_id: Some(id), season, episode, .. } => format!("t=tvsearch&cat={}&tvdbid={id}&season={season}&ep={episode}", cats(&ix.tv_categories)),
        Query::Episode { title, season, episode, .. } => format!("t=tvsearch&cat={}&q={}&season={season}&ep={episode}", cats(&ix.tv_categories), enc(title)),
        Query::Season { tvdb_id: Some(id), season, .. } => format!("t=tvsearch&cat={}&tvdbid={id}&season={season}", cats(&ix.tv_categories)),
        Query::Season { title, season, .. } => format!("t=tvsearch&cat={}&q={}&season={season}", cats(&ix.tv_categories), enc(title)),
        Query::MovieText { text } => format!("t=search&cat={},5070&q={}", cats(&ix.movie_categories), enc(text)),
        // Whole-category numbers (5000) so releases filed under anime or foreign are included.
        Query::TvText { text } => format!("t=search&cat={}&q={}", {
            let mut c: Vec<u32> = ix.tv_categories.iter().map(|c| c / 1000 * 1000).collect();
            c.sort_unstable();
            c.dedup();
            cats(&c)
        }, enc(text)),
    };
    format!("{base}?{params}&extended=1&limit=100&apikey={}", enc(&ix.api_key))
}

/// Parse a Newznab RSS response.
pub fn parse_feed(xml: &str, ix: &Indexer) -> Result<Vec<Release>> {
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut cur: Option<Release> = None;
    let mut tag = String::new();
    let mut text = String::new();
    let mut saw_rss = false;

    let attr = |e: &quick_xml::events::BytesStart, name: &[u8]| -> Option<String> {
        e.attributes().flatten().find(|a| a.key.as_ref() == name).and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
    };
    let handle_empty = |e: &quick_xml::events::BytesStart, cur: &mut Option<Release>| -> Result<()> {
        match e.name().as_ref() {
            b"error" => bail!("{} says: {}", ix.name, attr(e, b"description").unwrap_or_else(|| "unknown error".into())),
            b"enclosure" => {
                if let Some(r) = cur.as_mut() {
                    if let Some(u) = attr(e, b"url") {
                        r.link = u;
                    }
                    if r.size == 0 {
                        r.size = attr(e, b"length").and_then(|l| l.parse().ok()).unwrap_or(0);
                    }
                }
            }
            n if n.ends_with(b":attr") || n == b"attr" => {
                if let (Some(r), Some(name), Some(value)) = (cur.as_mut(), attr(e, b"name"), attr(e, b"value")) {
                    match name.as_str() {
                        "size" => r.size = value.parse().unwrap_or(r.size),
                        "imdb" | "imdbid" => {
                            let digits = value.trim_start_matches("tt");
                            if digits.chars().all(|c| c.is_ascii_digit()) && digits.trim_start_matches('0') != "" {
                                r.imdb_id = Some(format!("tt{digits:0>7}"));
                            }
                        }
                        "tvdbid" => r.tvdb_id = value.parse().ok().filter(|v| *v > 0),
                        "usenetdate" => {
                            if let Ok(d) = chrono::DateTime::parse_from_rfc2822(&value) {
                                r.published = d.timestamp();
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        Ok(())
    };

    loop {
        match reader.read_event() {
            Err(e) => return Err(anyhow!("{} returned invalid XML: {e}", ix.name)),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "rss" || name == "channel" {
                    saw_rss = true;
                }
                if name == "item" {
                    cur = Some(Release { indexer_id: ix.id, indexer: ix.name.clone(), indexer_priority: ix.priority, ..Default::default() });
                }
                handle_empty(&e, &mut cur)?;
                tag = name;
                text.clear();
            }
            Ok(Event::Empty(e)) => handle_empty(&e, &mut cur)?,
            Ok(Event::Text(t)) => {
                if let Ok(s) = t.decode() {
                    text.push_str(&quick_xml::escape::unescape(&s).map(|c| c.into_owned()).unwrap_or_else(|_| s.into_owned()));
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if let Ok(name) = r.decode() {
                    text.push_str(match name.as_ref() {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => "",
                    });
                }
            }
            Ok(Event::CData(t)) => text.push_str(&String::from_utf8_lossy(&t)),
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "item" {
                    if let Some(mut r) = cur.take() {
                        if r.guid.is_empty() {
                            r.guid = r.link.clone();
                        }
                        if !r.title.is_empty() && !r.link.is_empty() {
                            out.push(r);
                        }
                    }
                } else if let (Some(r), true) = (cur.as_mut(), name == tag) {
                    let v = text.trim();
                    match name.as_str() {
                        "title" => r.title = v.to_string(),
                        "guid" => r.guid = v.to_string(),
                        "link" if r.link.is_empty() => r.link = v.to_string(),
                        "comments" => r.info_url = v.to_string(),
                        "pubDate" if r.published == 0 => {
                            if let Ok(d) = chrono::DateTime::parse_from_rfc2822(v) {
                                r.published = d.timestamp();
                            }
                        }
                        _ => {}
                    }
                }
                text.clear();
            }
            _ => {}
        }
    }
    if !saw_rss {
        bail!("{} did not return a feed", ix.name);
    }
    Ok(out)
}

/// What an indexer says about the account's allowance, sent along with results by many of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Limits {
    pub api_current: Option<u32>,
    pub api_max: Option<u32>,
    pub grab_current: Option<u32>,
    pub grab_max: Option<u32>,
}

/// Read `<newznab:apilimits apicurrent=".." apimax=".." grabcurrent=".." grabmax=".."/>`.
pub fn parse_limits(xml: &str) -> Option<Limits> {
    let at = xml.find("apilimits")?;
    // Indexers disagree on capitals (apicurrent, apiCurrent).
    let tag = xml[at..at + xml[at..].find('>')?].to_lowercase();
    let num = |name: &str| -> Option<u32> {
        let i = tag.find(&format!("{name}=\""))? + name.len() + 2;
        tag[i..].split('"').next()?.trim().parse().ok()
    };
    let l = Limits { api_current: num("apicurrent"), api_max: num("apimax"), grab_current: num("grabcurrent"), grab_max: num("grabmax") };
    (l != Limits::default()).then_some(l)
}

pub async fn fetch(http: &reqwest::Client, ix: &Indexer, q: &Query) -> Result<(Vec<Release>, Option<Limits>)> {
    let url = build_url(ix, q);
    let resp = http.get(&url).timeout(std::time::Duration::from_secs(45)).send().await.map_err(|e| anyhow!("{}: {}", ix.name, e.without_url()))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| anyhow!("{}: {}", ix.name, e.without_url()))?;
    if !status.is_success() && !body.contains("<error") {
        bail!("{} answered HTTP {}", ix.name, status.as_u16());
    }
    Ok((parse_feed(&body, ix)?, parse_limits(&body)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ix() -> Indexer {
        serde_json::from_value(serde_json::json!({"id": 3, "name": "Test", "url": "https://indexer.test/", "api_key": "k&y"})).unwrap()
    }

    #[test]
    fn builds_queries() {
        let i = ix();
        assert_eq!(
            build_url(&i, &Query::Movie { imdb_id: Some("tt0127536".into()), title: "Elizabeth".into(), year: 1998 }),
            "https://indexer.test/api?t=movie&cat=2000,2010,2020,2030,2040,2045,2050,2060&imdbid=0127536&extended=1&limit=100&apikey=k%26y"
        );
        assert!(build_url(&i, &Query::Movie { imdb_id: None, title: "A & B".into(), year: 2001 }).contains("t=search&cat=2000,2010,2020,2030,2040,2045,2050,2060&q=A%20%26%20B%202001"));
        assert!(build_url(&i, &Query::Episode { tvdb_id: Some(296762), title: "Westworld".into(), season: 1, episode: 2 }).contains("t=tvsearch&cat=5030,5040&tvdbid=296762&season=1&ep=2"));
        assert!(build_url(&i, &Query::Season { tvdb_id: None, title: "Westworld".into(), season: 2 }).contains("q=Westworld&season=2"));
    }

    #[test]
    fn parses_items_and_errors() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel>
<item><title>Elizabeth.1998.1080p.BluRay.x265-GRP &amp; Co</title><guid isPermaLink="true">https://indexer.test/details/abc</guid>
<link>https://indexer.test/getnzb/abc.nzb&amp;i=1</link><comments>https://indexer.test/details/abc#comments</comments>
<pubDate>Mon, 05 Oct 2026 10:00:00 +0000</pubDate>
<enclosure url="https://indexer.test/getnzb/abc.nzb&amp;i=1&amp;r=key" length="123" type="application/x-nzb" />
<newznab:attr name="size" value="9876543210" /><newznab:attr name="imdb" value="127536" /><newznab:attr name="usenetdate" value="Sun, 04 Oct 2026 10:00:00 +0000" /></item>
<item><title>No link</title></item>
</channel></rss>"#;
        let r = parse_feed(xml, &ix()).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "Elizabeth.1998.1080p.BluRay.x265-GRP & Co");
        assert_eq!(r[0].link, "https://indexer.test/getnzb/abc.nzb&i=1&r=key");
        assert_eq!(r[0].size, 9876543210);
        assert_eq!(r[0].imdb_id.as_deref(), Some("tt0127536"));
        assert_eq!(r[0].guid, "https://indexer.test/details/abc");
        assert_eq!(r[0].published, chrono::DateTime::parse_from_rfc2822("Sun, 04 Oct 2026 10:00:00 +0000").unwrap().timestamp());
        assert_eq!((r[0].indexer_id, r[0].indexer.as_str()), (3, "Test"));

        let err = parse_feed(r#"<?xml version="1.0"?><error code="100" description="Incorrect user credentials"/>"#, &ix()).unwrap_err();
        assert!(err.to_string().contains("Incorrect user credentials"));
        assert!(parse_feed("<html>login</html>", &ix()).is_err());
    }

    #[test]
    fn reads_the_allowance_an_indexer_reports() {
        let xml = r#"<rss><channel><newznab:response offset="0" total="3"/><newznab:apilimits apicurrent="12" apimax="100" grabcurrent="3" grabmax="25" apioldesttime="x"/></channel></rss>"#;
        assert_eq!(parse_limits(xml), Some(Limits { api_current: Some(12), api_max: Some(100), grab_current: Some(3), grab_max: Some(25) }));
        assert_eq!(parse_limits("<rss><channel></channel></rss>"), None);
        // An account without a cap reports what it has used and no maximum.
        assert_eq!(parse_limits(r#"<newznab:apilimits apiCurrent="92" grabCurrent="103"/>"#), Some(Limits { api_current: Some(92), api_max: None, grab_current: Some(103), grab_max: None }));
    }
}
