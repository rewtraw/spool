//! Florilegium, where books are read and quoted, and kept in sync with Spool both ways.
//!
//! A book Spool gets is handed over: the EPUB goes onto its shelf and into its library at
//! once, and Florilegium marks any entry of its reading list that names the book as got.
//! The other way, every entry of the reading list not yet got is wanted here as an EPUB, and
//! a book already on the shelf is never fetched again. The shelf is read, not copied: Spool
//! keeps it in memory and reads it again every few minutes.

use crate::app::App;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// What Florilegium said when it took a book.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Receipt {
    pub at: i64,
    /// "shelved", or "exists" when it had these very bytes already.
    pub status: String,
    pub shelf_id: Option<i64>,
    pub book_id: Option<i64>,
}

/// Send an EPUB. An error says why it was not taken.
pub async fn send(http: &reqwest::Client, base: &str, token: &str, file: &Path, name: &str) -> Result<Receipt> {
    let bytes = tokio::fs::read(file).await?;
    let part = reqwest::multipart::Part::bytes(bytes).file_name(name.to_string()).mime_str("application/epub+zip")?;
    let form = reqwest::multipart::Form::new().text("library", "1").part("file", part);
    let mut req = http.post(format!("{}/shelf/upload", base.trim_end_matches('/'))).multipart(form).timeout(std::time::Duration::from_secs(300));
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.map_err(|e| anyhow::anyhow!("Florilegium could not be reached: {}", e.without_url()))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!("Florilegium turned the token down");
    }
    let body: serde_json::Value = resp.json().await.map_err(|_| anyhow::anyhow!("Florilegium answered {status} with something that is not its API"))?;
    if !status.is_success() {
        bail!("Florilegium said: {}", body["error"].as_str().unwrap_or(status.as_str()));
    }
    // It answers for each file it was sent, even when one failed.
    let r = &body["results"][0];
    match r["status"].as_str() {
        Some("error") => bail!("Florilegium could not take it: {}", r["error"].as_str().unwrap_or("no reason given")),
        Some(s @ ("shelved" | "exists")) => {
            let book = &r["book"];
            let receipt = Receipt { at: crate::db::now(), status: s.to_string(), shelf_id: book["id"].as_i64(), book_id: book["book_id"].as_i64() };
            if receipt.book_id.is_none() {
                bail!("Florilegium shelved it but did not add it to its library");
            }
            Ok(receipt)
        }
        _ => bail!("Florilegium's answer could not be read"),
    }
}

/// A book on Florilegium's shelf, as far as telling it from others goes.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Shelved {
    pub id: i64,
    pub title: String,
    pub author: Option<String>,
}

/// Florilegium's shelf as last read: from where, and when.
#[derive(Debug, Default)]
pub struct Shelf {
    pub books: Vec<Shelved>,
    pub url: String,
    pub at: i64,
}

impl Shelf {
    /// The book on the shelf that is this one. See [`same_book`].
    pub fn find(&self, title: &str, author: &str) -> Option<&Shelved> {
        self.books.iter().find(|s| same_book(title, author, &s.title, s.author.as_deref().unwrap_or("")))
    }
}

/// What is known of the shelf, and of reading it.
#[derive(Default)]
pub struct Cache {
    pub shelf: Arc<Shelf>,
    /// When it was last tried, so a Florilegium that does not answer is not asked again at once.
    pub tried: i64,
    /// Being read now.
    pub reading: bool,
}

/// Read again when older than this.
const FRESH: i64 = 600;
/// A read that failed is not tried again for this long.
const PAUSE: i64 = 60;

/// Lower-case words, accents and punctuation gone.
fn plain(s: &str) -> Vec<String> {
    deunicode::deunicode(s).to_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

/// A title split where its subtitle, or a series or edition note in brackets, begins.
fn split(t: &str) -> (&str, &str) {
    let cut = [":", " - ", " \u{2014} ", " \u{2013} ", "(", "["].iter().filter_map(|m| t.find(m)).min().unwrap_or(t.len());
    t.split_at(cut)
}

/// A title without its subtitle or note, and without "The".
fn title_key(t: &str) -> Vec<String> {
    let mut w = plain(split(t).0);
    if w.len() > 1 && matches!(w[0].as_str(), "the" | "a" | "an") {
        w.remove(0);
    }
    w
}

/// Whether two books, each by its title and author, are one. Both are acted on unasked, so
/// both must agree in full, as Florilegium itself matches its reading list: the titles the
/// same words, or the same before a subtitle only one of them has; numbers that differ make
/// two volumes. Every word of the shorter author's name, initials too, is in the longer's.
pub fn same_book(title: &str, author: &str, other_title: &str, other_author: &str) -> bool {
    same_author(author, other_author) && same_title(title, other_title)
}

fn same_title(a: &str, b: &str) -> bool {
    if plain(a) == plain(b) {
        return true;
    }
    let (ka, kb) = (title_key(a), title_key(b));
    if ka.is_empty() || ka != kb {
        return false;
    }
    // The same before the subtitle: then only one of them may have one, as both differing
    // (whole titles that were the same are taken above) can name two volumes.
    let subtitled = |t: &str| !plain(split(t).1).is_empty();
    let numbers = |t: &str| plain(t).into_iter().filter(|w| w.chars().all(|c| c.is_ascii_digit())).collect::<Vec<_>>();
    !(subtitled(a) && subtitled(b)) && numbers(a) == numbers(b)
}

fn same_author(a: &str, b: &str) -> bool {
    let names = |s: &str| {
        let mut w: Vec<String> = plain(s).into_iter().filter(|w| w != "and").collect();
        w.sort();
        w.dedup();
        w
    };
    let (wa, wb) = (names(a), names(b));
    let (short, long) = if wa.len() <= wb.len() { (wa, wb) } else { (wb, wa) };
    !short.is_empty() && short.iter().all(|w| long.contains(w))
}

/// An entry of Florilegium's reading list.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ReadingWant {
    pub id: i64,
    pub title: String,
    pub author: Option<String>,
    pub isbn: Option<String>,
    /// When it was got: on the shelf or in the library.
    pub acquired_at: Option<String>,
}

async fn get(http: &reqwest::Client, base: &str, token: &str, path: &str) -> Result<serde_json::Value> {
    let mut req = http.get(format!("{}{path}", base.trim_end_matches('/'))).timeout(std::time::Duration::from_secs(15));
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.map_err(|e| anyhow::anyhow!("Florilegium could not be reached: {}", e.without_url()))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!("Florilegium turned the token down");
    }
    if !status.is_success() {
        bail!("Florilegium answered {status}");
    }
    resp.json().await.map_err(|_| anyhow::anyhow!("Florilegium answered with something that is not its API"))
}

/// Everything on the shelf, a page at a time.
pub async fn shelf(http: &reqwest::Client, base: &str, token: &str) -> Result<Vec<Shelved>> {
    let mut books: Vec<Shelved> = vec![];
    loop {
        let page = get(http, base, token, &format!("/shelf?sort=title&limit=200&offset={}", books.len())).await?;
        let got: Vec<Shelved> = serde_json::from_value(page["books"].clone())?;
        let total = page["total"].as_u64().unwrap_or(0) as usize;
        let empty = got.is_empty();
        books.extend(got);
        if empty || books.len() >= total {
            return Ok(books);
        }
    }
}

/// The reading list.
pub async fn reading_list(http: &reqwest::Client, base: &str, token: &str) -> Result<Vec<ReadingWant>> {
    Ok(serde_json::from_value(get(http, base, token, "/wants").await?["wants"].clone())?)
}

/// How long to wait for the shelf to be read.
#[derive(Clone, Copy, PartialEq)]
pub enum Wait {
    /// Not at all: the shelf as last read, read again in the background when old.
    No,
    /// Only when it has never been read, as nothing can be told without it.
    IfNone,
    /// Read it now, unless a read just failed.
    Yes,
}

impl App {
    /// Florilegium's shelf. None when Florilegium is not set up, or it has not been read from
    /// where it now is.
    pub async fn florilegium_shelf(&self, wait: Wait) -> Option<Arc<Shelf>> {
        let g = self.settings.general();
        let url = g.books.florilegium_url.clone();
        if url.is_empty() {
            return None;
        }
        let now = crate::db::now();
        let (have, due) = {
            let mut c = self.books.florilegium.lock();
            let have = Some(c.shelf.clone()).filter(|s| s.url == url && s.at > 0);
            let old = have.as_ref().is_none_or(|s| now - s.at >= FRESH);
            let due = old && !c.reading && now - c.tried >= PAUSE;
            if due {
                c.reading = true;
                c.tried = now;
            }
            (have, due)
        };
        if !due {
            return have;
        }
        let app = self.clone();
        let read = async move {
            let got = shelf(&app.http, &url, &g.books.florilegium_token).await;
            let mut c = app.books.florilegium.lock();
            c.reading = false;
            match got {
                Ok(books) => {
                    c.shelf = Arc::new(Shelf { books, url, at: crate::db::now() });
                    Some(c.shelf.clone())
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "Florilegium's shelf could not be read");
                    None
                }
            }
        };
        if wait == Wait::Yes || (wait == Wait::IfNone && have.is_none()) {
            read.await.or(have)
        } else {
            tokio::spawn(read);
            have
        }
    }

    /// Take up Florilegium's reading list: every entry not yet got, and not already wanted
    /// here or let go of here, is wanted as an EPUB. Says what came of it, or nothing.
    pub async fn florilegium_sync(&self) -> Result<String> {
        let g = self.settings.general();
        if g.books.florilegium_url.is_empty() {
            return Ok(String::new());
        }
        let Some(shelf) = self.florilegium_shelf(Wait::Yes).await else {
            bail!("its shelf could not be read");
        };
        let list = reading_list(&self.http, &g.books.florilegium_url, &g.books.florilegium_token).await?;
        let books = self.db.books()?;
        let dropped: Vec<i64> = self.db.get_setting(DROPPED);
        let (mut wanted, mut unknown) = (0, vec![]);
        for w in list.iter().filter(|w| w.acquired_at.is_none() && !dropped.contains(&w.id)) {
            let author = w.author.clone().unwrap_or_default();
            if books.iter().any(|b| b.florilegium_want == Some(w.id)) || shelf.find(&w.title, &author).is_some() {
                continue;
            }
            // Florilegium names a book; Spool wants it by its Open Library work, when one is
            // that book by that author beyond doubt.
            let works = super::openlibrary::search(&self.http, &format!("{} {author}", w.title)).await.unwrap_or_default();
            let Some(work) = works.into_iter().find(|x| {
                let full = if x.subtitle.is_empty() { x.title.clone() } else { format!("{}: {}", x.title, x.subtitle) };
                same_book(&w.title, &author, &full, &x.authors.join(" & "))
            }) else {
                unknown.push(w.title.clone());
                continue;
            };
            match self.add_book_from(&work.ol_work, Some(w.id)).await {
                Ok(b) => {
                    tracing::info!(book = %b.title, "wanted a book from Florilegium's reading list");
                    wanted += 1;
                }
                Err(e) => tracing::warn!(book = %w.title, error = %format!("{e:#}"), "a book on Florilegium's reading list could not be wanted"),
            }
        }
        let mut said = vec![];
        if wanted > 0 {
            said.push(format!("{wanted} wanted from Florilegium's reading list"));
        }
        if !unknown.is_empty() {
            said.push(format!("not found in Open Library: {}", unknown.join("; ")));
        }
        Ok(said.join("; "))
    }

    /// A book wanted for the reading list was removed before it was got: its entry is not
    /// taken up again.
    pub(super) fn florilegium_let_go(&self, reading: i64) {
        let mut dropped: Vec<i64> = self.db.get_setting(DROPPED);
        if !dropped.contains(&reading) {
            dropped.push(reading);
            let _ = self.db.set_setting(DROPPED, &dropped);
        }
    }
}

/// Entries of the reading list let go of here.
const DROPPED: &str = "florilegium_dropped";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_book_on_the_shelf_is_known_by_its_title_and_author() {
        let shelf = Shelf {
            books: vec![
                Shelved { id: 1, title: "Forbidden Science 2: California Hermetica".into(), author: Some("Jacques Vallée".into()) },
                Shelved { id: 2, title: "Dune".into(), author: Some("Herbert, Frank".into()) },
                Shelved { id: 3, title: "Uri".into(), author: None },
                Shelved { id: 4, title: "Rubicon: The Last Years of the Roman Republic".into(), author: Some("Tom Holland".into()) },
                Shelved { id: 5, title: "The Return of the King: Being the Third Part of the Lord of the Rings".into(), author: Some("J. R. R. Tolkien".into()) },
            ],
            ..Default::default()
        };
        let id = |t: &str, a: &str| shelf.find(t, a).map(|s| s.id);
        assert_eq!(id("Dune", "Frank Herbert"), Some(2));
        assert_eq!(id("Forbidden Science 2", "Jacques Vallee"), Some(1));
        assert_eq!(id("Rubicon", "Tom Holland"), Some(4));
        assert_eq!(id("The Return of the King", "J.R.R. Tolkien"), Some(5));
        assert_eq!(id("Forbidden Science 3", "Jacques Vallée"), None, "another volume");
        assert_eq!(id("Dune Messiah", "Frank Herbert"), None);
        assert_eq!(id("Dune", "Brian Herbert"), None, "another author");
        assert_eq!(id("Dune", "F. Herbert"), None, "an initial is not a name");
        assert_eq!(id("Uri", "Andrija Puharich"), None, "no author to go by");
    }

    #[test]
    fn titles_must_agree_in_full() {
        assert!(same_title("Sinister Forces: The Nine", "Sinister Forces-The Nine"));
        assert!(same_title("Uri: A Journal of the Mystery of Uri Geller", "Uri"));
        assert!(same_title("The Only Planet of Choice", "The only planet of choice"));
        assert!(!same_title("Uri", "Uriel"));
        assert!(!same_title("Uri", "Uri / The Sacred Mushroom"), "an omnibus is not the one book");
        assert!(!same_title("Viridian Gate Online: Insurrection", "Viridian Gate Online: Vindication"));
        assert!(!same_author("Alice Smith & Bob Jones", "Alice Smith & Carol White"));
        assert!(same_author("Phyllis V. Schlemmer", "Phyllis V. Schlemmer & Palden Jenkins"));
    }
}
