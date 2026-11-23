//! Books. A book is found in Open Library's catalogue and added by its work; Spool looks
//! for an EPUB of it in the index of Anna's Archive it keeps (see [`index`] and [`dump`]),
//! downloads it with a member's key, checks it is the book and can be read, keeps a copy,
//! and hands it to Florilegium, where books are read. A book with no EPUB is got as a PDF,
//! when one is clearly the book: Spool keeps the PDF, and Florilegium makes an EPUB of it,
//! reading a scan with its own OCR, and adds that without asking for a review.
//!
//! A book goes Wanted → Fetching → Sending → Have. When the index offers files that could
//! be the book but none clearly is, it waits at Choose for someone to pick one. A file that
//! turns out not to do is remembered and passed over, and the next likeliest tried.

pub mod annas;
pub mod audio;
pub mod dump;
pub mod epub;
pub mod fallback;
pub mod florilegium;
pub mod index;
pub mod openlibrary;
pub mod text;

use crate::app::App;
use crate::db::{json, now, Db};
use crate::hunt::HuntState;
use anyhow::{bail, Result};
use parking_lot::Mutex;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How books are got and where they go.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BookSettings {
    /// A member's secret key for Anna's Archive's download API.
    pub annas_key: String,
    /// Where Florilegium answers, such as `http://melchior:8790`; empty keeps books in Spool.
    pub florilegium_url: String,
    pub florilegium_token: String,
    /// Languages books are read in, as codes: "en", or "en, el, grc, la". The index holds
    /// these alone.
    pub languages: String,
}

impl Default for BookSettings {
    fn default() -> Self {
        BookSettings { annas_key: String::new(), florilegium_url: String::new(), florilegium_token: String::new(), languages: "en".into() }
    }
}

impl BookSettings {
    pub fn languages(&self) -> Vec<String> {
        // Two letters, or three for a language that has no two-letter code: "grc" is Ancient
        // Greek, "cop" Coptic, "syc" Syriac.
        let l: Vec<String> = self.languages.split([',', ' ']).map(|s| s.trim().to_lowercase()).filter(|s| (2..=3).contains(&s.len()) && s.chars().all(|c| c.is_ascii_lowercase())).collect();
        if l.is_empty() {
            vec!["en".into()]
        } else {
            l
        }
    }
}

/// A file of a book that Anna's Archive holds.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Candidate {
    pub md5: String,
    pub title: String,
    pub author: String,
    pub lang: String,
    pub size: u64,
    pub year: i32,
    /// How sure the match is, 0 to 1.
    pub score: f64,
    /// Matched by ISBN or Open Library edition as well as by name.
    pub by_id: bool,
    /// "epub" or "pdf".
    pub format: String,
    /// Chosen by a person, who is taken at their word: a translation's title need not match the
    /// original's.
    pub picked: bool,
    /// Who published it and which edition it is, as far as Anna's Archive knows.
    pub publisher: String,
    pub edition: String,
    /// What Anna's Archive found wrong with the file: it is broken, mislabelled, of poor
    /// quality, or a worse copy of another. Such a file is never taken without asking.
    pub problems: Vec<String>,
}

impl Candidate {
    pub fn is_pdf(&self) -> bool {
        self.format == "pdf"
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BookState {
    /// Looked for in the index.
    #[default]
    Wanted,
    /// Files that could be it were found, and someone is to say which.
    Choose,
    Fetching,
    /// Downloaded and checked, and being handed to Florilegium.
    Sending,
    Have,
}

impl BookState {
    pub fn as_str(self) -> &'static str {
        match self {
            BookState::Wanted => "wanted",
            BookState::Choose => "choose",
            BookState::Fetching => "fetching",
            BookState::Sending => "sending",
            BookState::Have => "have",
        }
    }
}

/// A file that would not do, and why.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TurnedDown {
    pub md5: String,
    pub why: String,
    pub at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Book {
    pub id: i64,
    /// Its work in Open Library's catalogue; empty for a book Open Library has no work of its
    /// own for, such as one volume of several catalogued as one, added from its file.
    pub ol_work: String,
    /// The file of Anna's Archive it was added from, by MD5, when it has no Open Library work.
    pub added_from: String,
    pub title: String,
    pub subtitle: String,
    pub authors: Vec<String>,
    pub year: i32,
    pub cover: String,
    pub description: String,
    /// The language it is wanted in.
    pub language: String,
    /// What its editions in that language are known by.
    pub isbns: Vec<String>,
    pub ol_editions: Vec<String>,
    pub state: BookState,
    /// What is happening to it, or what it waits for, in words; empty when nothing needs saying.
    pub note: String,
    pub candidates: Vec<Candidate>,
    pub chosen: Option<Candidate>,
    pub turned_down: Vec<TurnedDown>,
    /// The EPUB, or the PDF, kept in Spool's books folder, by its name there.
    pub file: String,
    /// What the EPUB says it is.
    pub opened_title: String,
    pub receipt: Option<florilegium::Receipt>,
    /// The job in which Florilegium is making an EPUB of the PDF sent it.
    pub conversion: Option<String>,
    /// How many times it was asked to be sent again: names each upload of a PDF, so an upload
    /// whose answer was lost is the same job when retried, and a resend is a new one.
    pub sends: u32,
    pub added_at: i64,
    pub updated_at: i64,
    pub searched_at: i64,
    pub account_id: Option<i64>,
    /// Counts the saves, so that work done on a book that was changed or removed meanwhile
    /// is not saved over the change.
    pub rev: i64,
    /// Wanted as an EPUB. A book may be wanted as an audiobook alone.
    #[serde(default = "yes")]
    pub epub: bool,
    /// Wanted as an audiobook, and where that stands. See [`audio`].
    pub audio: Option<audio::Audio>,
    /// The entry of Florilegium's reading list it was wanted for, if it was.
    pub florilegium_want: Option<i64>,
}

fn yes() -> bool {
    true
}

impl Book {
    /// What tells it from every other book: its Open Library work, or the file it was added from.
    pub fn key(&self) -> String {
        if self.ol_work.is_empty() {
            file_key(&self.added_from)
        } else {
            self.ol_work.clone()
        }
    }
}

fn file_key(md5: &str) -> String {
    format!("md5:{md5}")
}

impl Db {
    pub fn books(&self) -> Result<Vec<Book>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM books ORDER BY added_at DESC")?;
            let rows = s.query_map([], |r| {
                let mut b: Book = json(r.get(1)?)?;
                b.id = r.get(0)?;
                Ok(b)
            })?;
            rows.collect()
        })
    }

    pub fn book(&self, id: i64) -> Result<Option<Book>> {
        self.with(|c| {
            c.query_row("SELECT id, data FROM books WHERE id = ?1", [id], |r| {
                let mut b: Book = json(r.get(1)?)?;
                b.id = r.get(0)?;
                Ok(b)
            })
            .optional()
        })
    }

    /// The book with this key: an Open Library work, or md5:<hash> for one added from a file.
    pub fn book_by_key(&self, key: &str) -> Result<Option<i64>> {
        self.with(|c| c.query_row("SELECT id FROM books WHERE key = ?1", [key], |r| r.get(0)).optional())
    }

    pub fn save_book(&self, b: &mut Book) -> Result<()> {
        b.updated_at = now();
        b.rev += 1;
        let data = serde_json::to_string(b)?;
        if b.id == 0 {
            b.id = self.with(|c| {
                c.execute("INSERT INTO books(key, state, added_at, data) VALUES (?1, ?2, ?3, ?4)", params![b.key(), b.state.as_str(), b.added_at, data])?;
                Ok(c.last_insert_rowid())
            })?;
        } else {
            self.with(|c| c.execute("UPDATE books SET state = ?2, data = ?3 WHERE id = ?1", params![b.id, b.state.as_str(), data]))?;
        }
        Ok(())
    }

    /// Save a book only if it is still as it was at `rev`: not changed by anyone else, and
    /// not removed. Gives whether it was saved.
    pub fn save_book_if(&self, b: &mut Book, rev: i64) -> Result<bool> {
        b.updated_at = now();
        b.rev = rev + 1;
        let data = serde_json::to_string(b)?;
        let n = self.with(|c| c.execute("UPDATE books SET state = ?2, data = ?3 WHERE id = ?1 AND key = ?4 AND json_extract(data, '$.rev') = ?5", params![b.id, b.state.as_str(), data, b.key(), rev]))?;
        Ok(n > 0)
    }

    pub fn listening(&self, account: i64, book: i64) -> Result<serde_json::Value> {
        self.with(|c| {
            c.query_row("SELECT position, rate, updated_at FROM listening WHERE account_id = ?1 AND book_id = ?2", params![account, book], |r| Ok(serde_json::json!({"position": r.get::<_, f64>(0)?, "rate": r.get::<_, f64>(1)?, "at": r.get::<_, i64>(2)?})))
                .optional()
                .map(|v| v.unwrap_or_else(|| serde_json::json!({"position": 0.0, "rate": 1.0, "at": 0})))
        })
    }

    pub fn set_listening(&self, account: i64, book: i64, position: f64, rate: Option<f64>) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO listening(account_id, book_id, position, rate, updated_at) VALUES (?1, ?2, ?3, COALESCE(?4, 1), ?5) ON CONFLICT(account_id, book_id) DO UPDATE SET position = ?3, rate = COALESCE(?4, rate), updated_at = ?5",
                params![account, book, position, rate, now()],
            )
        })?;
        Ok(())
    }

    pub fn delete_book(&self, id: i64) -> Result<()> {
        self.with(|c| {
            c.execute("DELETE FROM listening WHERE book_id = ?1", [id])?;
            c.execute("DELETE FROM books WHERE id = ?1", [id])
        })?;
        Ok(())
    }
}

/// What Spool holds for books while it runs.
pub struct Books {
    pub shelf: index::Shelf,
    pub index_state: Mutex<dump::IndexState>,
    pub allowance: Mutex<Option<annas::Allowance>>,
    /// Held while a book is worked on, so that one book is moved along at a time.
    pub working: tokio::sync::Mutex<()>,
    pub dir: PathBuf,
    /// Set when Spool is stopping: a build of the index under way gives up at once rather
    /// than hold the shutdown up.
    pub stopping: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub stopped: tokio::sync::Notify,
    /// Florilegium's shelf as last read. See [`florilegium`].
    pub florilegium: Mutex<florilegium::Cache>,
}

impl Books {
    pub fn new(data_dir: &std::path::Path, db: &Db) -> Books {
        let dir = data_dir.join("books");
        let mut state: dump::IndexState = db.get_setting("book_index");
        // A build cut short by a restart is not running any more.
        state.building = false;
        Books { shelf: index::Shelf::new(&dir), index_state: Mutex::new(state), allowance: Mutex::new(db.get_setting("book_allowance")), working: Default::default(), dir, stopping: Default::default(), stopped: Default::default(), florilegium: Default::default() }
    }

    /// Spool is stopping.
    pub fn stop(&self) {
        self.stopping.store(true, std::sync::atomic::Ordering::SeqCst);
        self.stopped.notify_waiters();
    }

    pub fn files_dir(&self) -> PathBuf {
        self.dir.join("files")
    }
}

/// A name a file can have, from a book's author and title.
fn file_name(b: &Book, md5: &str, ext: &str) -> String {
    let clean = |s: &str| s.chars().map(|c| if "/\\:*?\"<>|".contains(c) || c.is_control() { ' ' } else { c }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    // Bounded in bytes: a file name may be at most 255 of them, and a title in another
    // script takes three to a letter.
    let bounded = |s: String, most: usize| {
        let mut out = String::new();
        for ch in s.chars() {
            if out.len() + ch.len_utf8() > most {
                break;
            }
            out.push(ch);
        }
        out.trim().to_string()
    };
    let author = bounded(b.authors.first().map(|a| clean(a)).unwrap_or_default(), 60);
    let title = bounded(clean(&b.title), 140);
    let short = &md5[..8.min(md5.len())];
    if author.is_empty() {
        format!("{title} [{short}].{ext}")
    } else {
        format!("{author} - {title} [{short}].{ext}")
    }
}

/// The file to take without asking: an EPUB that [`pick_of`] takes, or failing one, a PDF
/// it takes. A PDF that is clearly the book is better than EPUBs that may not be.
pub fn pick(c: &[Candidate]) -> Option<usize> {
    let of = |pdf: bool| {
        let idx: Vec<usize> = (0..c.len()).filter(|&i| c[i].is_pdf() == pdf).collect();
        let these: Vec<Candidate> = idx.iter().map(|&i| c[i].clone()).collect();
        pick_of(&these).map(|i| idx[i])
    };
    of(false).or_else(|| of(true))
}

/// The likeliest, when it is sure enough and nothing that is another book comes close
/// behind it.
fn pick_of(c: &[Candidate]) -> Option<usize> {
    let top = c.first()?;
    if !top.problems.is_empty() {
        return None;
    }
    if top.by_id && top.score >= 0.8 {
        return Some(0);
    }
    if top.score < 0.88 {
        return None;
    }
    // Another file of the same book has the same title in full; one whose subtitle differs
    // may be another volume or another book, and is asked about.
    let same = |x: &Candidate| text::words(&x.title) == text::words(&top.title) && text::author_key(&x.author) == text::author_key(&top.author);
    match c.iter().skip(1).find(|x| !same(x)) {
        Some(r) if top.score - r.score < 0.08 => None,
        _ => Some(0),
    }
}

/// The title a book is looked for by: with its subtitle, which may be what tells it from
/// another book of the series.
pub fn wanted_title(b: &Book) -> String {
    if b.subtitle.is_empty() {
        b.title.clone()
    } else {
        format!("{}: {}", b.title, b.subtitle)
    }
}

/// Whether what an EPUB says it is agrees with the book it was got for.
/// Whether a file plainly is this book: matched by ISBN, or with every word of the book's name
/// in its own. Sharing a few, as "Western Esotericism: A Guide for the Perplexed" does with
/// "Dictionary of Gnosis & Western Esotericism", is not enough.
fn names_this_book(b: &Book, c: &Candidate) -> bool {
    c.by_id || text::cover(&text::core_title(&b.title), &text::words(&c.title).join(" ")) >= 0.9
}

pub fn is_the_book(b: &Book, o: &epub::Opened) -> bool {
    if !o.isbns.is_empty() && o.isbns.iter().any(|i| b.isbns.contains(i)) {
        return true;
    }
    text::title_alike(&wanted_title(b), &o.title) >= 0.75
}

impl App {
    /// Works in Open Library's catalogue, each with the book it already is here, if any.
    pub async fn book_lookup(&self, q: &str) -> Result<Vec<serde_json::Value>> {
        let works = openlibrary::search(&self.http, q).await?;
        let shelf = self.florilegium_shelf(florilegium::Wait::No).await.unwrap_or_default();
        Ok(works
            .into_iter()
            .map(|w| {
                let have = self.db.book_by_key(&w.ol_work).ok().flatten().and_then(|id| self.db.book(id).ok().flatten());
                let mut v = serde_json::json!(w);
                v["book_id"] = serde_json::json!(have.as_ref().map(|b| b.id));
                // Which forms of it are already wanted.
                v["formats"] = serde_json::json!(have.map(|b| [b.epub.then_some("epub"), b.audio.is_some().then_some("audiobook")].into_iter().flatten().collect::<Vec<_>>()));
                let full = if w.subtitle.is_empty() { w.title.clone() } else { format!("{}: {}", w.title, w.subtitle) };
                v["on_shelf"] = serde_json::json!(shelf.find(&full, w.authors.first().map(String::as_str).unwrap_or("")).is_some());
                v
            })
            .collect())
    }

    /// Want a book: as an EPUB, an audiobook, or both. A book already added gains what it lacked.
    pub async fn add_book(&self, ol_work: &str, language: Option<&str>, account: Option<i64>, epub: bool, audiobook: bool) -> Result<Book> {
        self.add_book_as(ol_work, language, account, epub, audiobook, None).await
    }

    /// Want a book's EPUB for an entry of Florilegium's reading list.
    pub async fn add_book_from(&self, ol_work: &str, reading: Option<i64>) -> Result<Book> {
        self.add_book_as(ol_work, None, None, true, false, reading).await
    }

    async fn add_book_as(&self, ol_work: &str, language: Option<&str>, account: Option<i64>, epub: bool, audiobook: bool, reading: Option<i64>) -> Result<Book> {
        if let Some(id) = self.db.book_by_key(ol_work)? {
            let mut b = self.db.book(id)?.ok_or_else(|| anyhow::anyhow!("the book went"))?;
            let more = (epub && !b.epub) || (audiobook && b.audio.is_none()) || (reading.is_some() && b.florilegium_want.is_none());
            if more {
                b.florilegium_want = b.florilegium_want.or(reading);
                b.epub |= epub;
                if audiobook && b.audio.is_none() {
                    b.audio = Some(audio::Audio::default());
                }
                self.db.save_book(&mut b)?;
                self.nudge_book(id);
            }
            return Ok(b);
        }
        let language = language.map(str::to_string).unwrap_or_else(|| self.settings.general().books.languages()[0].clone());
        let d = openlibrary::detail(&self.http, ol_work, &language).await?;
        let mut b = Book {
            ol_work: ol_work.to_string(),
            title: d.work.title,
            subtitle: d.work.subtitle,
            authors: d.work.authors,
            year: d.work.year,
            cover: d.work.cover,
            description: d.description,
            language,
            isbns: d.isbns,
            ol_editions: d.ol_editions,
            added_at: now(),
            account_id: account,
            epub,
            audio: audiobook.then(audio::Audio::default),
            florilegium_want: reading,
            ..Default::default()
        };
        self.db.save_book(&mut b)?;
        tracing::info!(book = %b.title, "added a book");
        self.nudge_book(b.id);
        Ok(b)
    }

    /// Want a file of Anna's Archive as a book of its own, by its MD5: for a book Open Library
    /// has no work of its own for, such as one volume of a set it catalogues as one. The file
    /// is taken at its word, named as the index names it, and fetched at once.
    pub async fn add_book_file(&self, md5: &str, account: Option<i64>) -> Result<Book> {
        let md5 = md5.trim().to_lowercase();
        if md5.len() != 32 || !md5.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("an MD5 is 32 hexadecimal characters");
        }
        if let Some(id) = self.db.book_by_key(&file_key(&md5))? {
            return self.db.book(id)?.ok_or_else(|| anyhow::anyhow!("the book went"));
        }
        // A book that already has this file, or is fetching it, is this book.
        if let Some(b) = self.db.books()?.into_iter().find(|b| b.chosen.as_ref().is_some_and(|c| c.md5 == md5)) {
            return Ok(b);
        }
        let Some(c) = self.books.shelf.by_md5(&md5)? else { bail!("no file with that MD5 is in the index of Anna's Archive") };
        let language = Some(c.lang.clone()).filter(|l| !l.is_empty()).unwrap_or_else(|| self.settings.general().books.languages()[0].clone());
        let about = self.books.shelf.sources(&md5).ok().flatten().unwrap_or_default();
        let mut b = Book {
            cover: about.cover,
            description: about.description,
            added_from: md5,
            title: c.title.clone(),
            authors: Some(c.author.clone()).filter(|a| !a.is_empty()).into_iter().collect(),
            year: c.year,
            language,
            state: BookState::Fetching,
            chosen: Some(c),
            added_at: now(),
            account_id: account,
            epub: true,
            ..Default::default()
        };
        self.db.save_book(&mut b)?;
        tracing::info!(book = %b.title, "added a book from its file");
        self.nudge_book(b.id);
        Ok(b)
    }

    /// Move a book along in the background.
    pub fn nudge_book(&self, id: i64) {
        let app = self.clone();
        tokio::spawn(async move {
            if let Err(e) = app.advance_book(id).await {
                tracing::warn!(book = id, error = %e, "a book could not be moved along");
            }
        });
    }

    pub fn remove_book(&self, id: i64, delete_file: bool) -> Result<()> {
        let Some(b) = self.db.book(id)? else { bail!("no such book") };
        if delete_file && !b.file.is_empty() {
            let _ = std::fs::remove_file(self.books.files_dir().join(&b.file));
        }
        if let Some(c) = &b.chosen {
            self.drop_partial(&c.md5);
        }
        if let Some(job) = b.audio.as_ref().and_then(|a| a.hunt.download()) {
            let _ = self.job_remove(job, true);
        }
        if let (Some(reading), false) = (b.florilegium_want, b.state == BookState::Have) {
            self.florilegium_let_go(reading);
        }
        if b.state != BookState::Have {
            self.cancel_conversion(b.conversion.clone());
        }
        self.db.delete_book(id)
    }

    /// Take this file for the book: one of those offered, or any by its MD5.
    /// Download a file chosen by hand. A file that is another work, chosen from among those the
    /// index offered, is added as that book under its own name, and this one keeps waiting; the
    /// book it is fetched as is answered.
    pub async fn choose_book_file(&self, id: i64, md5: &str) -> Result<Book> {
        let md5 = md5.trim().to_lowercase();
        if md5.len() != 32 || !md5.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("an MD5 is 32 hexadecimal characters");
        }
        let Some(mut b) = self.db.book(id)? else { bail!("no such book") };
        let listed = b.candidates.iter().find(|c| c.md5 == md5).cloned();
        let c = Candidate { picked: true, ..listed.clone().unwrap_or(Candidate { md5: md5.clone(), title: b.title.clone(), author: b.authors.first().cloned().unwrap_or_default(), ..Default::default() }) };
        if let Some(listed) = &listed {
            if let Some(work) = self.other_work(&b, listed).await {
                let mut other = self.add_book_as(&work, Some(&b.language), b.account_id, true, false, None).await?;
                if other.state != BookState::Have {
                    other.turned_down.retain(|t| t.md5 != md5);
                    other.chosen = Some(c);
                    other.state = BookState::Fetching;
                    other.note.clear();
                    self.db.save_book(&mut other)?;
                    self.nudge_book(other.id);
                }
                return Ok(other);
            }
        }
        b.turned_down.retain(|t| t.md5 != md5);
        b.chosen = Some(c);
        b.state = BookState::Fetching;
        b.note.clear();
        self.db.save_book(&mut b)?;
        self.nudge_book(id);
        Ok(b)
    }

    /// The Open Library work a file is, when it is not this book: its name is unlike the book's,
    /// and looking it up by name and author finds another work. A translation, named in its own
    /// language, is found to be the same work.
    async fn other_work(&self, b: &Book, c: &Candidate) -> Option<String> {
        if names_this_book(b, c) {
            return None;
        }
        // "Title (Series Book 3)" is looked up as "Title".
        let title = c.title.split(['(', '[']).next().unwrap_or(&c.title).trim();
        let author = c.author.split([';', ',', '&']).next().unwrap_or("").trim();
        let works = openlibrary::search(&self.http, &format!("{title} {author}")).await.ok()?;
        let w = works.into_iter().find(|w| text::title_alike(title, &w.title) >= 0.75)?;
        (w.ol_work != b.ol_work).then_some(w.ol_work)
    }

    /// Look for the book again from the start.
    pub fn retry_book(&self, id: i64) -> Result<Book> {
        let Some(mut b) = self.db.book(id)? else { bail!("no such book") };
        if b.state != BookState::Have {
            b.state = BookState::Wanted;
        }
        b.note.clear();
        b.searched_at = 0;
        self.db.save_book(&mut b)?;
        self.nudge_book(id);
        Ok(b)
    }

    /// Hand a book that is here to Florilegium again.
    pub fn resend_book(&self, id: i64) -> Result<Book> {
        let Some(mut b) = self.db.book(id)? else { bail!("no such book") };
        if b.file.is_empty() {
            bail!("there is no file of it to send");
        }
        b.state = BookState::Sending;
        b.note.clear();
        // A PDF is converted again: Florilegium keeps what it read of the pages, so it is quick.
        if b.state != BookState::Have {
            self.cancel_conversion(b.conversion.take());
        }
        b.conversion = None;
        b.sends += 1;
        self.db.save_book(&mut b)?;
        self.nudge_book(id);
        Ok(b)
    }

    /// The "books" task: move along every book that is not yet here.
    pub async fn books_task(&self) -> Result<String> {
        // Florilegium's reading list first, so what it adds is moved along with the rest.
        let synced = self.florilegium_sync().await;
        let mut moved = 0;
        let mut failed = 0;
        let books = self.db.books()?;
        for b in &books {
            let audio_due = b.audio.as_ref().is_some_and(|a| a.hunt.state != HuntState::Have);
            if (b.epub && matches!(b.state, BookState::Wanted | BookState::Fetching | BookState::Sending)) || audio_due {
                // One book's trouble does not hold up the rest.
                match self.advance_book(b.id).await {
                    Ok(()) => moved += 1,
                    Err(e) => {
                        failed += 1;
                        tracing::warn!(book = %b.title, error = %e, "a book could not be moved along");
                    }
                }
            }
        }
        self.sweep_incoming(&books);
        let mut said = if failed > 0 { format!("{moved} books looked at, {failed} could not be") } else { format!("{moved} books looked at") };
        match synced {
            Ok(s) if !s.is_empty() => said = format!("{s}; {said}"),
            Ok(_) => {}
            Err(e) => said = format!("Florilegium's reading list could not be read: {e:#}; {said}"),
        }
        Ok(said)
    }

    /// Partial downloads of a file no book is fetching any more, left an hour.
    fn sweep_incoming(&self, books: &[Book]) {
        let fetching: Vec<&str> = books.iter().filter(|b| b.state == BookState::Fetching).filter_map(|b| b.chosen.as_ref().map(|c| c.md5.as_str())).collect();
        let Ok(dir) = std::fs::read_dir(self.books.dir.join("incoming")) else { return };
        for e in dir.flatten() {
            let p = e.path();
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|m| m.elapsed().ok()).is_some_and(|d| d.as_secs() > 3600);
            if old && !fetching.contains(&stem.as_str()) {
                let _ = std::fs::remove_file(&p);
            }
        }
    }

    /// Delete what was downloaded of a file, whole or in part.
    fn drop_partial(&self, md5: &str) {
        let incoming = self.books.dir.join("incoming");
        let _ = std::fs::remove_file(incoming.join(format!("{md5}.part")));
        let _ = std::fs::remove_file(incoming.join(format!("{md5}.epub")));
        let _ = std::fs::remove_file(incoming.join(format!("{md5}.pdf")));
    }

    /// Take a book as far as it can go now.
    pub async fn advance_book(&self, id: i64) -> Result<()> {
        let _held = self.books.working.lock().await;
        if self.db.book(id)?.is_some_and(|b| b.epub) {
            self.advance_epub(id).await?;
        }
        // The audiobook goes its own way beside the EPUB.
        for _ in 0..4 {
            let Some(mut b) = self.db.book(id)? else { return Ok(()) };
            let before = b.audio.clone();
            if before.is_none() {
                return Ok(());
            }
            let rev = b.rev;
            self.advance_audio(&mut b).await?;
            let after = b.audio.clone().unwrap_or_default();
            let job_before = before.as_ref().and_then(|a| a.hunt.job_id.clone());
            if b.audio == before {
                return Ok(());
            }
            if !self.db.save_book_if(&mut b, rev)? {
                // Changed meanwhile, or removed. A download this pass started would be followed by
                // nothing, so it goes; a filing is found again on the next pass, as the files are
                // already in place. Then the book is taken up again as it now is.
                if let Some(job) = after.hunt.download().filter(|j| job_before.as_deref() != Some(*j)) {
                    let _ = self.job_remove(job, true);
                }
                continue;
            }
            // Saved: the finished download it was filed from is cleared away.
            if let Some((job, out)) = &after.filed_from {
                let _ = self.job_remove(job, true);
                self.remove_download_folder(Some(job), out).await;
            }
            let moved = before.map(|a| (a.hunt.state, a.hunt.note)) != Some((after.hunt.state, after.hunt.note.clone()));
            if !moved || after.hunt.state != HuntState::Wanted {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Take a book's EPUB as far as it can go now. Called with the book held.
    async fn advance_epub(&self, id: i64) -> Result<()> {
        for _ in 0..8 {
            let Some(mut b) = self.db.book(id)? else { return Ok(()) };
            let mut rev = b.rev;
            let before = (b.state, b.note.clone());
            let file = b.file.clone();
            if b.state == BookState::Fetching {
                b.note = "Downloading.".into();
                if !self.db.save_book_if(&mut b, rev)? {
                    return Ok(());
                }
                rev = b.rev;
            }
            if b.state == BookState::Wanted {
                let author = b.authors.first().cloned().unwrap_or_default();
                if let Some(s) = self.florilegium_shelf(florilegium::Wait::IfNone).await.and_then(|shelf| shelf.find(&wanted_title(&b), &author).map(|s| s.title.clone())) {
                    tracing::info!(book = %b.title, shelved = %s, "a book is on Florilegium's shelf already");
                    b.state = BookState::Have;
                    b.note = "On Florilegium's shelf already.".into();
                    self.db.save_book_if(&mut b, rev)?;
                    return Ok(());
                }
            }
            match b.state {
                BookState::Wanted => self.look_for(&mut b)?,
                BookState::Fetching => self.fetch(&mut b).await?,
                BookState::Sending => self.send(&mut b).await?,
                BookState::Choose | BookState::Have => return Ok(()),
            }
            // Changed or removed while this was going on: what was done gives way, and a file
            // it brought in that nothing now names goes too.
            if !self.db.save_book_if(&mut b, rev)? {
                if b.file != file && !b.file.is_empty() {
                    let named = self.db.books().unwrap_or_default().iter().any(|x| x.file == b.file);
                    if !named {
                        let _ = std::fs::remove_file(self.books.files_dir().join(&b.file));
                    }
                }
                return Ok(());
            }
            if (b.state, b.note.clone()) == before || matches!(b.state, BookState::Choose | BookState::Have) {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Find files of a book in the index, and take one or ask which.
    fn look_for(&self, b: &mut Book) -> Result<()> {
        if self.books.shelf.books().is_none() {
            b.note = "Waiting for the book index to be built.".into();
            return Ok(());
        }
        let built = self.books.index_state.lock().built_at;
        // A book not found is looked for again only in a newer index.
        if b.searched_at > built && !b.note.is_empty() && b.candidates.is_empty() {
            return Ok(());
        }
        let author = b.authors.first().cloned().unwrap_or_default();
        let languages = vec![b.language.clone()];
        let title = wanted_title(b);
        let mut found = self.books.shelf.find(&index::Ask { title: &title, author: &author, isbns: &b.isbns, ols: &b.ol_editions, languages: &languages })?;
        found.retain(|c| !b.turned_down.iter().any(|t| t.md5 == c.md5) && c.score >= 0.5);
        b.searched_at = now();
        b.candidates = found;
        match pick(&b.candidates) {
            Some(i) => {
                b.chosen = Some(b.candidates[i].clone());
                b.state = BookState::Fetching;
                b.note.clear();
            }
            None if b.candidates.is_empty() => {
                b.note = if b.turned_down.is_empty() { "Not in Anna's Archive's index. It is looked for again when the index is next built.".into() } else { "None of the files found would do.".into() };
            }
            None => {
                b.state = BookState::Choose;
                b.note = "Several files could be this book. Choose one.".into();
            }
        }
        Ok(())
    }

    fn turn_down(&self, b: &mut Book, md5: &str, why: String) {
        tracing::info!(book = %b.title, md5, why = %why, "a file of a book would not do");
        let file = b.chosen.as_ref().map(|c| c.title.clone());
        let _ = self.db.add_history(None, "book_failed", serde_json::json!({"book_id": b.id, "book": b.title, "authors": b.authors, "release": file, "reason": why}));
        self.drop_partial(md5);
        b.turned_down.push(TurnedDown { md5: md5.to_string(), why: why.clone(), at: now() });
        b.candidates.retain(|c| c.md5 != md5);
        b.chosen = None;
        b.state = BookState::Wanted;
        b.searched_at = 0;
        b.note = format!("A file would not do: {why}. Trying the next.");
    }

    async fn fetch(&self, b: &mut Book) -> Result<()> {
        let Some(c) = b.chosen.clone() else {
            b.state = BookState::Wanted;
            return Ok(());
        };
        let g = self.settings.general();
        let incoming = self.books.dir.join("incoming");
        std::fs::create_dir_all(&incoming)?;
        let ext = if c.is_pdf() { "pdf" } else { "epub" };
        let dest = incoming.join(format!("{}.{ext}", c.md5));
        // Why Anna's Archive's servers did not give it, if they did not.
        let refused = if g.books.annas_key.is_empty() {
            Some((false, "Add an Anna's Archive key in Settings → Books to download it.".to_string()))
        } else if self.books.allowance.lock().clone().is_some_and(|a| a.downloads_left <= 0 && now() - a.at < 6 * 3600) {
            Some((false, "Today's downloads from Anna's Archive are used up; it is tried again later.".to_string()))
        } else {
            match annas::fetch(&self.http, &g.books.annas_key, &c.md5, &dest, 200_000_000).await {
                Ok(allowance) => {
                    if let Some(a) = allowance {
                        let _ = self.db.set_setting("book_allowance", &a);
                        *self.books.allowance.lock() = Some(a);
                    }
                    None
                }
                // A file the servers have not got, or have wrong, is turned down unless it can
                // be had elsewhere.
                Err(annas::Refused::File(why)) => Some((true, why)),
                Err(e @ annas::Refused::Spent) => {
                    let mut a = self.books.allowance.lock().clone().unwrap_or_default();
                    a.downloads_left = 0;
                    a.at = now();
                    *self.books.allowance.lock() = Some(a);
                    Some((false, format!("{e}; it is tried again later.")))
                }
                Err(e) => Some((false, format!("{e}. It is tried again later."))),
            }
        };
        if let Some((turn_down, why)) = refused {
            // Where else Anna's Archive says the file is: IPFS, or one of its torrents.
            let elsewhere = self.books.shelf.sources(&c.md5).ok().flatten().filter(|s| s.any());
            let got = match elsewhere {
                None => Err(anyhow::anyhow!("Anna's Archive knows of nowhere else it is")),
                Some(s) => {
                    let place = fallback::Place { books_dir: &self.books.dir, stopping: &self.books.stopping };
                    fallback::fetch(&self.http, &s, &dest, 200_000_000, &place).await
                }
            };
            match got {
                Ok(how) => tracing::info!(book = %b.title, md5 = %c.md5, how, "a book was had without Anna's Archive's servers"),
                Err(e) => {
                    tracing::info!(book = %b.title, md5 = %c.md5, error = %e, "a book could not be had elsewhere either");
                    if turn_down {
                        self.turn_down(b, &c.md5, why);
                    } else {
                        b.note = why;
                    }
                    return Ok(());
                }
            }
        }
        let path = dest.clone();
        // What it is, by what it begins with: a file chosen by its MD5 alone may be either.
        let head = {
            use tokio::io::AsyncReadExt;
            let mut buf = vec![0u8; 1024];
            let n = match tokio::fs::File::open(&dest).await {
                Ok(mut f) => f.read(&mut buf).await.unwrap_or(0),
                Err(_) => 0,
            };
            buf.truncate(n);
            buf
        };
        let pdf = memchr::memmem::find(&head, b"%PDF-").is_some();
        let ext = if pdf { "pdf" } else { "epub" };
        let opened = if pdf {
            // A PDF says little reliably of what it is: the index's match is relied on.
            Ok(epub::Opened { title: c.title.clone(), ..Default::default() })
        } else {
            tokio::task::spawn_blocking(move || epub::open(&path)).await?
        };
        let opened = match opened {
            Ok(o) => o,
            Err(e) => {
                let _ = std::fs::remove_file(&dest);
                self.turn_down(b, &c.md5, format!("{e:#}"));
                return Ok(());
            }
        };
        if !c.picked && !is_the_book(b, &opened) {
            let _ = std::fs::remove_file(&dest);
            let by = opened.authors.first().map(|a| format!(" by {a}")).unwrap_or_default();
            self.turn_down(b, &c.md5, format!("the EPUB says it is \u{201c}{}\u{201d}{by}", opened.title));
            return Ok(());
        }
        let files = self.books.files_dir();
        std::fs::create_dir_all(&files)?;
        let name = file_name(b, &c.md5, ext);
        std::fs::rename(&dest, files.join(&name))?;
        if !b.file.is_empty() && b.file != name {
            let _ = std::fs::remove_file(files.join(&b.file));
        }
        b.file = name;
        b.opened_title = opened.title;
        self.cancel_conversion(b.conversion.take());
        b.state = BookState::Sending;
        b.note.clear();
        tracing::info!(book = %b.title, md5 = %c.md5, "downloaded a book");
        Ok(())
    }

    async fn send(&self, b: &mut Book) -> Result<()> {
        let g = self.settings.general();
        if g.books.florilegium_url.is_empty() {
            let _ = self.db.add_history(None, "book_got", serde_json::json!({"book_id": b.id, "book": b.title, "authors": b.authors, "release": b.file, "kept": true}));
            b.state = BookState::Have;
            b.note = "Kept in Spool. Set up Florilegium in Settings → Books to send books there.".into();
            return Ok(());
        }
        let path = self.books.files_dir().join(&b.file);
        if b.file.ends_with(".pdf") {
            return self.send_pdf(b, &g.books.florilegium_url, &g.books.florilegium_token, &path).await;
        }
        match florilegium::send(&self.http, &g.books.florilegium_url, &g.books.florilegium_token, &path, &b.file).await {
            Ok(r) => {
                tracing::info!(book = %b.title, status = %r.status, "sent a book to Florilegium");
                let _ = self.db.add_history(None, "book_got", serde_json::json!({"book_id": b.id, "book": b.title, "authors": b.authors, "release": b.file, "florilegium": true}));
                b.receipt = Some(r);
                b.state = BookState::Have;
                b.note.clear();
            }
            Err(e) => b.note = format!("{e:#}. It is sent again later."),
        }
        Ok(())
    }

    /// Stop a conversion no longer wanted, so it does not add a book after all. One that has
    /// finished, or is being added, is left as it is.
    fn cancel_conversion(&self, job: Option<String>) {
        let Some(job) = job else { return };
        let g = self.settings.general();
        if g.books.florilegium_url.is_empty() {
            return;
        }
        let http = self.http.clone();
        tokio::spawn(async move {
            if let Err(e) = florilegium::cancel(&http, &g.books.florilegium_url, &g.books.florilegium_token, &job).await {
                tracing::info!(job = %job, error = %e, "a conversion in Florilegium was not stopped");
            }
        });
    }

    /// Hand a PDF to Florilegium to make an EPUB of and add, then follow the conversion: it
    /// takes minutes for a book with text, longer for a scan. The PDF stays here as it came.
    async fn send_pdf(&self, b: &mut Book, base: &str, token: &str, path: &std::path::Path) -> Result<()> {
        let Some(job) = b.conversion.clone() else {
            let author = b.authors.first().cloned().unwrap_or_default();
            let md5 = b.chosen.as_ref().map(|c| c.md5.as_str()).unwrap_or("");
            let client = format!("spool-book-{}-{md5}-{}", b.id, b.sends);
            match florilegium::send_pdf(&self.http, base, token, path, &b.file, &client, &wanted_title(b), &author).await {
                Ok(job) => {
                    tracing::info!(book = %b.title, job = %job, "sent a PDF to Florilegium to convert");
                    b.conversion = Some(job);
                    b.note = "Florilegium is making an EPUB of the PDF.".into();
                }
                Err(e) => b.note = format!("{e:#}. It is sent again later."),
            }
            return Ok(());
        };
        let c = match florilegium::conversion(&self.http, base, token, &job).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                // Florilegium has no such job any more: sent again.
                b.conversion = None;
                b.note = "Florilegium lost the conversion; the PDF is sent again.".into();
                return Ok(());
            }
            Err(e) => {
                b.note = format!("{e:#}. It is asked again later.");
                return Ok(());
            }
        };
        match c.state.as_str() {
            "completed" => {
                tracing::info!(book = %b.title, "Florilegium made an EPUB of a PDF and added it");
                let _ = self.db.add_history(None, "book_got", serde_json::json!({"book_id": b.id, "book": b.title, "authors": b.authors, "release": b.file, "florilegium": true, "converted": true}));
                b.receipt = Some(florilegium::Receipt { at: now(), status: "converted".into(), shelf_id: c.shelf_id, book_id: c.book_id });
                b.state = BookState::Have;
                b.note.clear();
            }
            "failed" | "cancelled" => b.note = format!("Florilegium could not make an EPUB of the PDF: {}. Send it again to try once more.", c.error.as_deref().unwrap_or(&c.state)),
            "review" => b.note = format!("Florilegium could not add the conversion by itself{}; it waits for a review there.", c.error.map(|e| format!(" ({e})")).unwrap_or_default()),
            _ if c.total > 0 => b.note = format!("Florilegium is making an EPUB of the PDF: {} of {} pages.", c.done, c.total),
            _ => b.note = "Florilegium is making an EPUB of the PDF.".into(),
        }
        Ok(())
    }

    /// What the books side has to say for itself: the index, the membership, the books.
    pub fn books_status(&self) -> serde_json::Value {
        let g = self.settings.general();
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        for b in self.db.books().unwrap_or_default() {
            *counts.entry(b.state.as_str()).or_default() += 1;
        }
        serde_json::json!({
            "index": *self.books.index_state.lock(),
            "index_books": self.books.shelf.books(),
            "allowance": *self.books.allowance.lock(),
            "has_key": !g.books.annas_key.is_empty(),
            "florilegium": !g.books.florilegium_url.is_empty(),
            "languages": g.books.languages(),
            "counts": counts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(md5: &str, title: &str, author: &str, score: f64, by_id: bool) -> Candidate {
        Candidate { md5: md5.into(), title: title.into(), author: author.into(), score, by_id, ..Default::default() }
    }

    #[test]
    fn a_file_names_the_book_only_with_all_its_words() {
        let book = |title: &str| Book { title: title.into(), ..Default::default() };
        let dictionary = book("Dictionary of Gnosis & Western Esotericism");
        assert!(!names_this_book(&dictionary, &cand("a", "Western Esotericism: a Guide for the Perplexed", "Hanegraaff", 0.9, false)));
        assert!(names_this_book(&book("Faust"), &cand("b", "Faust, Part One", "Goethe", 0.9, false)));
        assert!(names_this_book(&book("Uri"), &cand("c", "Uri (Bantam edition)", "Puharich", 0.9, false)));
        // A translation is not known by its name; Open Library is asked.
        assert!(!names_this_book(&book("Die Leiden des jungen Werthers"), &cand("d", "The Sorrows of Young Werther", "Goethe", 0.9, false)));
        assert!(names_this_book(&dictionary, &cand("e", "Anything", "", 0.5, true)));
    }

    #[test]
    fn a_clear_match_is_taken_and_a_close_call_is_asked() {
        assert_eq!(pick(&[cand("a", "Dune", "Frank Herbert", 0.83, true)]), Some(0));
        assert_eq!(pick(&[cand("a", "Dune", "Frank Herbert", 0.95, false), cand("b", "Dune", "Herbert, Frank", 0.94, false)]), Some(0), "two files of one book");
        assert_eq!(pick(&[cand("a", "Viridian Gate Online: Insurrection", "J A Hunter", 0.93, false), cand("b", "Viridian Gate Online: Vindication", "J A Hunter", 0.9, false)]), None, "another subtitle may be another book");
        assert_eq!(pick(&[cand("a", "Miss Marple", "Agatha Christie", 0.9, false), cand("b", "Miss Marple Omnibus", "Agatha Christie", 0.86, false)]), None);
        assert_eq!(pick(&[cand("a", "Dune", "Frank Herbert", 0.8, false)]), None);
    }

    #[test]
    fn a_pdf_is_taken_only_when_no_epub_will_do() {
        let pdf = |md5: &str, title: &str, score: f64, by_id: bool| Candidate { format: "pdf".into(), ..cand(md5, title, "Joscelyn Godwin", score, by_id) };
        assert_eq!(pick(&[pdf("p", "Arktos", 0.98, true), cand("e", "Arktos", "Joscelyn Godwin", 0.95, false)]), Some(1), "an EPUB of the book comes first");
        assert_eq!(pick(&[cand("e", "The Forbidden Book", "Joscelyn Godwin", 0.81, false), pdf("p", "Arktos", 0.98, true)]), Some(1), "a PDF that is the book beats EPUBs that may not be");
        assert_eq!(pick(&[pdf("p", "Arktos", 0.7, false)]), None, "a PDF is asked about as an EPUB would be");
    }

    #[test]
    fn a_book_is_kept_and_found_again() {
        let db = Db::memory().unwrap();
        let mut b = Book { ol_work: "OL1W".into(), title: "Dune".into(), added_at: 1, ..Default::default() };
        db.save_book(&mut b).unwrap();
        assert!(b.id > 0);
        assert_eq!(db.book_by_key("OL1W").unwrap(), Some(b.id));
        b.state = BookState::Choose;
        db.save_book(&mut b).unwrap();
        assert_eq!(db.book(b.id).unwrap().unwrap().state, BookState::Choose);
        // Two volumes Open Library catalogues as one work are two books, by their files.
        let mut two = Book { added_from: "98b5fd5c2f8b5451ff3c44ad0abdf209".into(), title: "The Zohar Vol 2".into(), added_at: 2, ..Default::default() };
        db.save_book(&mut two).unwrap();
        assert_eq!(db.book_by_key("md5:98b5fd5c2f8b5451ff3c44ad0abdf209").unwrap(), Some(two.id));
        assert!(db.save_book_if(&mut two.clone(), two.rev).unwrap());
        assert_eq!(db.books().unwrap().len(), 2);
        db.delete_book(b.id).unwrap();
        assert!(db.book(b.id).unwrap().is_none());
    }

    #[test]
    fn an_epub_is_the_book_by_isbn_or_by_name() {
        let b = Book { title: "The Left Hand of Darkness".into(), isbns: vec!["9780441478125".into()], ..Default::default() };
        let o = |t: &str, i: &[&str]| epub::Opened { title: t.into(), isbns: i.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        assert!(is_the_book(&b, &o("Left Hand of Darkness, The", &[])));
        assert!(is_the_book(&b, &o("LHoD", &["9780441478125"])));
        assert!(!is_the_book(&b, &o("The Dispossessed", &[])));
    }
}
