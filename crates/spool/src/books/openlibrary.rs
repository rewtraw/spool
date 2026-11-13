//! Books as Open Library knows them: a work, with the editions it was published in. A book
//! is found and added by its work; its editions give the ISBNs its files are matched by.

use anyhow::Result;
use serde::{Deserialize, Serialize};

const BASE: &str = "https://openlibrary.org";

/// A work, as a search finds it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Work {
    /// Such as "OL45804W".
    pub ol_work: String,
    pub title: String,
    pub subtitle: String,
    pub authors: Vec<String>,
    pub year: i32,
    pub cover: String,
    pub editions: u32,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Search {
    docs: Vec<Doc>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Doc {
    key: String,
    title: String,
    subtitle: Option<String>,
    author_name: Vec<String>,
    first_publish_year: Option<i32>,
    cover_i: Option<i64>,
    edition_count: Option<u32>,
}

fn cover(id: Option<i64>) -> String {
    id.filter(|i| *i > 0).map(|i| format!("https://covers.openlibrary.org/b/id/{i}-M.jpg")).unwrap_or_default()
}

fn work_id(key: &str) -> String {
    key.trim_start_matches("/works/").to_string()
}

/// Works matching what was typed: a title, an author, or both.
pub async fn search(http: &reqwest::Client, q: &str) -> Result<Vec<Work>> {
    let s: Search = http
        .get(format!("{BASE}/search.json"))
        .query(&[("q", q), ("fields", "key,title,subtitle,author_name,first_publish_year,cover_i,edition_count"), ("limit", "24")])
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(s.docs
        .into_iter()
        .filter(|d| d.key.starts_with("/works/"))
        .map(|d| Work { ol_work: work_id(&d.key), title: d.title, subtitle: d.subtitle.unwrap_or_default(), authors: d.author_name, year: d.first_publish_year.unwrap_or(0), cover: cover(d.cover_i), editions: d.edition_count.unwrap_or(0) })
        .collect())
}

/// What a work is, with what its editions in a language say: their ISBNs and Open Library
/// edition ids.
#[derive(Clone, Debug, Default)]
pub struct Detail {
    pub work: Work,
    pub description: String,
    pub isbns: Vec<String>,
    pub ol_editions: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WorkJson {
    title: String,
    subtitle: Option<String>,
    description: Option<serde_json::Value>,
    covers: Vec<i64>,
    authors: Vec<AuthorRef>,
    first_publish_date: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AuthorRef {
    author: Option<Key>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Key {
    key: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Named {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Editions {
    entries: Vec<Edition>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Edition {
    key: String,
    isbn_13: Vec<String>,
    isbn_10: Vec<String>,
    languages: Vec<Key>,
    publish_date: Option<String>,
}

/// Open Library names languages by three letters, as "/languages/eng"; Spool and Anna's
/// Archive by two.
pub fn two_letter(code: &str) -> String {
    let c = code.trim_start_matches("/languages/");
    match c {
        "eng" => "en",
        "fre" | "fra" => "fr",
        "ger" | "deu" => "de",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "dut" | "nld" => "nl",
        "jpn" => "ja",
        "chi" | "zho" => "zh",
        "rus" => "ru",
        "pol" => "pl",
        "swe" => "sv",
        "dan" => "da",
        "nor" => "no",
        "fin" => "fi",
        "gre" | "ell" => "el",
        "kor" => "ko",
        "ara" => "ar",
        "heb" => "he",
        "tur" => "tr",
        "cze" | "ces" => "cs",
        "hun" => "hu",
        other => other,
    }
    .to_string()
}

fn year_of(s: &str) -> i32 {
    s.split(|c: char| !c.is_ascii_digit()).find(|w| w.len() == 4).and_then(|w| w.parse().ok()).unwrap_or(0)
}

/// A work in full, with the ISBNs of its editions in `language` (and of those whose
/// language is not given).
pub async fn detail(http: &reqwest::Client, ol_work: &str, language: &str) -> Result<Detail> {
    let get = |path: String| {
        let http = http.clone();
        async move { http.get(format!("{BASE}{path}")).timeout(std::time::Duration::from_secs(30)).send().await?.error_for_status() }
    };
    let w: WorkJson = get(format!("/works/{ol_work}.json")).await?.json().await?;
    let mut authors = vec![];
    for a in w.authors.iter().filter_map(|a| a.author.as_ref()).take(4) {
        if let Ok(r) = get(format!("{}.json", a.key)).await {
            if let Ok(n) = r.json::<Named>().await {
                if !n.name.is_empty() {
                    authors.push(n.name);
                }
            }
        }
    }
    let eds: Editions = match get(format!("/works/{ol_work}/editions.json?limit=200")).await {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(_) => Editions::default(),
    };
    let mut isbns = vec![];
    let mut ol_editions = vec![];
    let mut earliest = w.first_publish_date.as_deref().map(year_of).unwrap_or(0);
    for e in &eds.entries {
        let langs: Vec<String> = e.languages.iter().map(|l| two_letter(&l.key)).collect();
        if !langs.is_empty() && !langs.iter().any(|l| l == language) {
            continue;
        }
        let y = e.publish_date.as_deref().map(year_of).unwrap_or(0);
        if y > 0 && (earliest == 0 || y < earliest) {
            earliest = y;
        }
        isbns.extend(e.isbn_13.iter().chain(&e.isbn_10).filter_map(|i| super::text::isbn13(i)));
        ol_editions.push(e.key.trim_start_matches("/books/").to_string());
    }
    isbns.sort();
    isbns.dedup();
    let description = match w.description {
        Some(serde_json::Value::String(s)) => s,
        Some(serde_json::Value::Object(o)) => o.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        _ => String::new(),
    };
    Ok(Detail {
        work: Work { ol_work: ol_work.to_string(), title: w.title, subtitle: w.subtitle.unwrap_or_default(), authors, year: earliest, cover: cover(w.covers.first().copied()), editions: eds.entries.len() as u32 },
        description,
        isbns,
        ol_editions,
    })
}
