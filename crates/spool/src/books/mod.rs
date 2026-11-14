//! Books. A book is found in Open Library's catalogue and added by its work; Spool looks
//! for an EPUB of it in the index of Anna's Archive it keeps (see [`index`] and [`dump`]),
//! downloads it with a member's key, checks it is the book and can be read, keeps a copy,
//! and hands it to Florilegium, where books are read.
//!
//! A book goes Wanted → Fetching → Sending → Have. When the index offers files that could
//! be the book but none clearly is, it waits at Choose for someone to pick one. A file that
//! turns out not to do is remembered and passed over, and the next likeliest tried.

pub mod annas;
pub mod audio;
pub mod dump;
pub mod epub;
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
    /// Languages books are read in, as two-letter codes: "en", or "en, fr". The index holds
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
        let l: Vec<String> = self.languages.split([',', ' ']).map(|s| s.trim().to_lowercase()).filter(|s| s.len() == 2).collect();
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
    pub ol_work: String,
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
    /// The EPUB kept in Spool's books folder, by its name there.
    pub file: String,
    /// What the EPUB says it is.
    pub opened_title: String,
    pub receipt: Option<florilegium::Receipt>,
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
}

fn yes() -> bool {
    true
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

    pub fn book_by_work(&self, ol_work: &str) -> Result<Option<i64>> {
        self.with(|c| c.query_row("SELECT id FROM books WHERE ol_work = ?1", [ol_work], |r| r.get(0)).optional())
    }

    pub fn save_book(&self, b: &mut Book) -> Result<()> {
        b.updated_at = now();
        b.rev += 1;
        let data = serde_json::to_string(b)?;
        if b.id == 0 {
            b.id = self.with(|c| {
                c.execute("INSERT INTO books(ol_work, state, added_at, data) VALUES (?1, ?2, ?3, ?4)", params![b.ol_work, b.state.as_str(), b.added_at, data])?;
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
        let n = self.with(|c| c.execute("UPDATE books SET state = ?2, data = ?3 WHERE id = ?1 AND ol_work = ?4 AND json_extract(data, '$.rev') = ?5", params![b.id, b.state.as_str(), data, b.ol_work, rev]))?;
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
}

impl Books {
    pub fn new(data_dir: &std::path::Path, db: &Db) -> Books {
        let dir = data_dir.join("books");
        let mut state: dump::IndexState = db.get_setting("book_index");
        // A build cut short by a restart is not running any more.
        state.building = false;
        Books { shelf: index::Shelf::new(&dir), index_state: Mutex::new(state), allowance: Mutex::new(db.get_setting("book_allowance")), working: Default::default(), dir, stopping: Default::default(), stopped: Default::default() }
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
fn file_name(b: &Book, md5: &str) -> String {
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
        format!("{title} [{short}].epub")
    } else {
        format!("{author} - {title} [{short}].epub")
    }
}

/// The file to take without asking: the likeliest, when it is sure enough and nothing that
/// is another book comes close behind it.
pub fn pick(c: &[Candidate]) -> Option<usize> {
    let top = c.first()?;
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
        Ok(works
            .into_iter()
            .map(|w| {
                let have = self.db.book_by_work(&w.ol_work).ok().flatten().and_then(|id| self.db.book(id).ok().flatten());
                let mut v = serde_json::json!(w);
                v["book_id"] = serde_json::json!(have.as_ref().map(|b| b.id));
                // Which forms of it are already wanted.
                v["formats"] = serde_json::json!(have.map(|b| [b.epub.then_some("epub"), b.audio.is_some().then_some("audiobook")].into_iter().flatten().collect::<Vec<_>>()));
                v
            })
            .collect())
    }

    /// Want a book: as an EPUB, an audiobook, or both. A book already added gains what it lacked.
    pub async fn add_book(&self, ol_work: &str, language: Option<&str>, account: Option<i64>, epub: bool, audiobook: bool) -> Result<Book> {
        if let Some(id) = self.db.book_by_work(ol_work)? {
            let mut b = self.db.book(id)?.ok_or_else(|| anyhow::anyhow!("the book went"))?;
            let more = (epub && !b.epub) || (audiobook && b.audio.is_none());
            if more {
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
            ..Default::default()
        };
        self.db.save_book(&mut b)?;
        tracing::info!(book = %b.title, "added a book");
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
        self.db.delete_book(id)
    }

    /// Take this file for the book: one of those offered, or any by its MD5.
    pub fn choose_book_file(&self, id: i64, md5: &str) -> Result<Book> {
        let md5 = md5.trim().to_lowercase();
        if md5.len() != 32 || !md5.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("an MD5 is 32 hexadecimal characters");
        }
        let Some(mut b) = self.db.book(id)? else { bail!("no such book") };
        let c = b.candidates.iter().find(|c| c.md5 == md5).cloned().unwrap_or(Candidate { md5: md5.clone(), title: b.title.clone(), author: b.authors.first().cloned().unwrap_or_default(), ..Default::default() });
        b.turned_down.retain(|t| t.md5 != md5);
        b.chosen = Some(c);
        b.state = BookState::Fetching;
        b.note.clear();
        self.db.save_book(&mut b)?;
        self.nudge_book(id);
        Ok(b)
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
        self.db.save_book(&mut b)?;
        self.nudge_book(id);
        Ok(b)
    }

    /// The "books" task: move along every book that is not yet here.
    pub async fn books_task(&self) -> Result<String> {
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
        Ok(if failed > 0 { format!("{moved} books looked at, {failed} could not be") } else { format!("{moved} books looked at") })
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
        if g.books.annas_key.is_empty() {
            b.note = "Add an Anna's Archive key in Settings → Books to download it.".into();
            return Ok(());
        }
        if let Some(a) = self.books.allowance.lock().clone() {
            if a.downloads_left <= 0 && now() - a.at < 6 * 3600 {
                b.note = "Today's downloads from Anna's Archive are used up; it is tried again later.".into();
                return Ok(());
            }
        }
        let incoming = self.books.dir.join("incoming");
        std::fs::create_dir_all(&incoming)?;
        let dest = incoming.join(format!("{}.epub", c.md5));
        match annas::fetch(&self.http, &g.books.annas_key, &c.md5, &dest, 200_000_000).await {
            Ok(allowance) => {
                if let Some(a) = allowance {
                    let _ = self.db.set_setting("book_allowance", &a);
                    *self.books.allowance.lock() = Some(a);
                }
            }
            Err(annas::Refused::File(why)) => {
                self.turn_down(b, &c.md5, why);
                return Ok(());
            }
            Err(e @ annas::Refused::Spent) => {
                let mut a = self.books.allowance.lock().clone().unwrap_or_default();
                a.downloads_left = 0;
                a.at = now();
                *self.books.allowance.lock() = Some(a);
                b.note = format!("{e}; it is tried again later.");
                return Ok(());
            }
            Err(e) => {
                b.note = format!("{e}. It is tried again later.");
                return Ok(());
            }
        }
        let path = dest.clone();
        let opened = tokio::task::spawn_blocking(move || epub::open(&path)).await?;
        let opened = match opened {
            Ok(o) => o,
            Err(e) => {
                let _ = std::fs::remove_file(&dest);
                self.turn_down(b, &c.md5, format!("{e:#}"));
                return Ok(());
            }
        };
        if !is_the_book(b, &opened) {
            let _ = std::fs::remove_file(&dest);
            let by = opened.authors.first().map(|a| format!(" by {a}")).unwrap_or_default();
            self.turn_down(b, &c.md5, format!("the EPUB says it is \u{201c}{}\u{201d}{by}", opened.title));
            return Ok(());
        }
        let files = self.books.files_dir();
        std::fs::create_dir_all(&files)?;
        let name = file_name(b, &c.md5);
        std::fs::rename(&dest, files.join(&name))?;
        if !b.file.is_empty() && b.file != name {
            let _ = std::fs::remove_file(files.join(&b.file));
        }
        b.file = name;
        b.opened_title = opened.title;
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
    fn a_clear_match_is_taken_and_a_close_call_is_asked() {
        assert_eq!(pick(&[cand("a", "Dune", "Frank Herbert", 0.83, true)]), Some(0));
        assert_eq!(pick(&[cand("a", "Dune", "Frank Herbert", 0.95, false), cand("b", "Dune", "Herbert, Frank", 0.94, false)]), Some(0), "two files of one book");
        assert_eq!(pick(&[cand("a", "Viridian Gate Online: Insurrection", "J A Hunter", 0.93, false), cand("b", "Viridian Gate Online: Vindication", "J A Hunter", 0.9, false)]), None, "another subtitle may be another book");
        assert_eq!(pick(&[cand("a", "Miss Marple", "Agatha Christie", 0.9, false), cand("b", "Miss Marple Omnibus", "Agatha Christie", 0.86, false)]), None);
        assert_eq!(pick(&[cand("a", "Dune", "Frank Herbert", 0.8, false)]), None);
    }

    #[test]
    fn a_book_is_kept_and_found_again() {
        let db = Db::memory().unwrap();
        let mut b = Book { ol_work: "OL1W".into(), title: "Dune".into(), added_at: 1, ..Default::default() };
        db.save_book(&mut b).unwrap();
        assert!(b.id > 0);
        assert_eq!(db.book_by_work("OL1W").unwrap(), Some(b.id));
        b.state = BookState::Choose;
        db.save_book(&mut b).unwrap();
        assert_eq!(db.book(b.id).unwrap().unwrap().state, BookState::Choose);
        assert_eq!(db.books().unwrap().len(), 1);
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
