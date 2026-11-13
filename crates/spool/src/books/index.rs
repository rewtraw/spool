//! The search index of the books Anna's Archive holds, built on this machine from the
//! metadata it publishes (see [`super::dump`]). Only EPUBs in the languages read here are
//! kept: a few hundred megabytes for every English one. A book is found by its ISBN or
//! Open Library edition when the file carries one, and otherwise by its title and author,
//! forgiving of typos, then re-ranked by how alike the names are.

use super::text::{author_alike, author_key, core_title, title_alike, words, STOP};
use super::Candidate;
use anyhow::{Context, Result};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Schema, Value, FAST, STORED, STRING, TEXT};
use tantivy::{Index, IndexReader, IndexWriter, TantivyDocument, Term};

#[derive(Clone, Copy)]
struct Fields {
    /// The title in full and without its subtitle, as plain words, so either finds it.
    title: Field,
    /// The first author, in the one form [`author_key`] gives.
    author: Field,
    isbn: Field,
    ol: Field,
    md5: Field,
    show_title: Field,
    show_author: Field,
    lang: Field,
    size: Field,
    year: Field,
}

fn schema() -> (Schema, Fields) {
    let mut sb = Schema::builder();
    let f = Fields {
        title: sb.add_text_field("title", TEXT),
        author: sb.add_text_field("author", TEXT),
        isbn: sb.add_text_field("isbn", STRING),
        ol: sb.add_text_field("ol", STRING),
        md5: sb.add_text_field("md5", STRING | STORED),
        show_title: sb.add_text_field("show_title", STORED),
        show_author: sb.add_text_field("show_author", STORED),
        lang: sb.add_text_field("lang", STRING | STORED),
        size: sb.add_u64_field("size", STORED | FAST),
        year: sb.add_u64_field("year", STORED),
    };
    (sb.build(), f)
}

/// One file of a book, as the metadata gives it.
#[derive(Debug, Default)]
pub struct Record {
    pub md5: String,
    pub title: String,
    pub author: String,
    pub lang: String,
    pub size: u64,
    pub year: u64,
    pub isbns: Vec<String>,
    pub ols: Vec<String>,
}

/// Writes a new index beside the one in use.
pub struct Builder {
    writer: IndexWriter,
    f: Fields,
    pub count: u64,
}

impl Builder {
    pub fn new(dir: &Path) -> Result<Builder> {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir)?;
        let (schema, f) = schema();
        let index = Index::create_in_dir(dir, schema)?;
        Ok(Builder { writer: index.writer(400_000_000)?, f, count: 0 })
    }

    pub fn add(&mut self, r: &Record) -> Result<()> {
        let f = self.f;
        let mut d = TantivyDocument::new();
        d.add_text(f.title, format!("{} {}", words(&r.title).join(" "), core_title(&r.title)));
        d.add_text(f.author, author_key(&r.author));
        for i in &r.isbns {
            d.add_text(f.isbn, i);
        }
        for o in &r.ols {
            d.add_text(f.ol, o);
        }
        d.add_text(f.md5, &r.md5);
        d.add_text(f.show_title, &r.title);
        d.add_text(f.show_author, &r.author);
        d.add_text(f.lang, &r.lang);
        d.add_u64(f.size, r.size);
        d.add_u64(f.year, r.year);
        self.writer.add_document(d)?;
        self.count += 1;
        Ok(())
    }

    pub fn finish(mut self) -> Result<u64> {
        self.writer.commit()?;
        self.writer.wait_merging_threads()?;
        Ok(self.count)
    }
}

struct Open {
    reader: IndexReader,
    f: Fields,
    docs: u64,
}

/// The index in use, if one has been built.
pub struct Shelf {
    dir: PathBuf,
    open: RwLock<Option<Open>>,
}

/// What a book is looked for by.
pub struct Ask<'a> {
    pub title: &'a str,
    pub author: &'a str,
    pub isbns: &'a [String],
    pub ols: &'a [String],
    /// Language codes a file may be in. A file whose language is not known may be in any.
    pub languages: &'a [String],
}

impl Shelf {
    pub fn new(books_dir: &Path) -> Shelf {
        let s = Shelf { dir: books_dir.join("index"), open: RwLock::new(None) };
        // A swap cut short by a crash left the index in use under its old name.
        let old = s.dir.with_file_name("index.old");
        if !s.dir.exists() && old.exists() {
            let _ = std::fs::rename(&old, &s.dir);
        }
        if let Err(e) = s.reopen() {
            tracing::debug!(error = %e, "no book index yet");
        }
        s
    }

    /// Where the next index is written before it takes the place of this one.
    pub fn next_dir(&self) -> PathBuf {
        self.dir.with_file_name("index.new")
    }

    /// Put the index just built in place of the one in use. If it will not open, the one in
    /// use stays.
    pub fn replace(&self) -> Result<()> {
        Index::open_in_dir(self.next_dir()).context("the index just built does not open")?;
        let old = self.dir.with_file_name("index.old");
        let _ = std::fs::remove_dir_all(&old);
        let mut open = self.open.write();
        *open = None;
        if self.dir.exists() {
            std::fs::rename(&self.dir, &old)?;
        }
        if let Err(e) = std::fs::rename(self.next_dir(), &self.dir) {
            let _ = std::fs::rename(&old, &self.dir);
            drop(open);
            let _ = self.reopen();
            return Err(e.into());
        }
        drop(open);
        self.reopen()?;
        let _ = std::fs::remove_dir_all(&old);
        Ok(())
    }

    fn reopen(&self) -> Result<()> {
        let index = Index::open_in_dir(&self.dir).with_context(|| format!("opening {}", self.dir.display()))?;
        let (_, f) = schema();
        let reader = index.reader()?;
        let docs = reader.searcher().num_docs();
        *self.open.write() = Some(Open { reader, f, docs });
        Ok(())
    }

    /// How many books are in the index; None when there is none.
    pub fn books(&self) -> Option<u64> {
        self.open.read().as_ref().map(|o| o.docs)
    }

    /// Files of a book, the likeliest first.
    pub fn find(&self, ask: &Ask) -> Result<Vec<Candidate>> {
        let open = self.open.read();
        let Some(o) = open.as_ref() else { anyhow::bail!("the book index has not been built yet") };
        let f = o.f;
        let searcher = o.reader.searcher();
        let mut found: HashMap<String, Candidate> = HashMap::new();

        // By ISBN or Open Library edition, which settle it when the names agree too.
        let mut ids: Vec<(Occur, Box<dyn Query>)> = vec![];
        for i in ask.isbns {
            ids.push((Occur::Should, Box::new(TermQuery::new(Term::from_field_text(f.isbn, i), IndexRecordOption::Basic))));
        }
        for i in ask.ols {
            ids.push((Occur::Should, Box::new(TermQuery::new(Term::from_field_text(f.ol, i), IndexRecordOption::Basic))));
        }
        let mut hits: Vec<(f32, tantivy::DocAddress, bool)> = vec![];
        if !ids.is_empty() {
            hits.extend(searcher.search(&BooleanQuery::new(ids), &TopDocs::with_limit(40))?.into_iter().map(|(s, a)| (s, a, true)));
        }
        // By title and author.
        let mut text: Vec<(Occur, Box<dyn Query>)> = vec![];
        clauses(f.title, ask.title, 2.0, &mut text);
        clauses(f.author, &author_key(ask.author), 1.5, &mut text);
        if !text.is_empty() {
            hits.extend(searcher.search(&BooleanQuery::new(text), &TopDocs::with_limit(60))?.into_iter().map(|(s, a)| (s, a, false)));
        }

        for (_, addr, by_id) in hits {
            let d: TantivyDocument = searcher.doc(addr)?;
            let text = |field: Field| d.get_first(field).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let num = |field: Field| d.get_first(field).and_then(|v| v.as_u64()).unwrap_or(0);
            let lang = text(f.lang);
            if !lang.is_empty() && !ask.languages.is_empty() && !ask.languages.contains(&lang) {
                continue;
            }
            let md5 = text(f.md5);
            let (title, author) = (text(f.show_title), text(f.show_author));
            let t = title_alike(ask.title, &title);
            let a = if ask.author.is_empty() { t } else { author_alike(ask.author, &author) };
            // An ISBN alone is not trusted: a file's list of them sometimes holds another
            // book's. It counts when the title agrees as well.
            let by_id = by_id && t >= 0.6;
            let score = (t * 0.65 + a * 0.35 + if by_id { 0.15 } else { 0.0 }).min(1.0);
            let c = Candidate { md5: md5.clone(), title, author, lang, size: num(f.size), year: num(f.year) as i32, score: (score * 1000.0).round() / 1000.0, by_id };
            match found.get(&md5) {
                Some(old) if old.score >= c.score => {}
                _ => {
                    found.insert(md5, c);
                }
            }
        }
        let mut out: Vec<Candidate> = found.into_values().collect();
        out.sort_by(|a, b| b.score.total_cmp(&a.score).then(b.by_id.cmp(&a.by_id)).then(b.size.cmp(&a.size)));
        out.truncate(15);
        Ok(out)
    }
}

/// Each word as itself, and forgiving of a typo or two once it is long enough to bear one.
fn clauses(field: Field, text: &str, exact: f32, out: &mut Vec<(Occur, Box<dyn Query>)>) {
    let ws = words(text);
    let only_stop = ws.iter().all(|w| STOP.contains(&w.as_str()));
    for w in &ws {
        if STOP.contains(&w.as_str()) && !only_stop {
            continue;
        }
        let term = Term::from_field_text(field, w);
        out.push((Occur::Should, Box::new(BoostQuery::new(Box::new(TermQuery::new(term.clone(), IndexRecordOption::WithFreqs)), exact))));
        let d = if w.len() >= 8 { 2 } else if w.len() >= 4 { 1 } else { 0 };
        if d > 0 {
            out.push((Occur::Should, Box::new(FuzzyTermQuery::new(term, d, true))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(md5: &str, title: &str, author: &str, isbn: &str) -> Record {
        Record { md5: md5.into(), title: title.into(), author: author.into(), lang: "en".into(), size: 500_000, year: 2000, isbns: if isbn.is_empty() { vec![] } else { vec![isbn.into()] }, ols: vec![] }
    }

    #[test]
    fn a_book_is_found_by_its_name_or_its_isbn() {
        let dir = tempfile::tempdir().unwrap();
        let shelf = Shelf::new(dir.path());
        let mut b = Builder::new(&shelf.next_dir()).unwrap();
        b.add(&rec("a1", "The Left Hand of Darkness", "Le Guin, Ursula K.", "9780441478125")).unwrap();
        b.add(&rec("b2", "The Dispossessed", "Ursula K. Le Guin", "")).unwrap();
        b.add(&rec("c3", "Goblin Slayer, Vol. 4", "Kumo Kagyu", "")).unwrap();
        b.add(&rec("d4", "Goblin Slayer, Vol. 11", "Kumo Kagyu", "")).unwrap();
        b.add(&rec("e5", "Darkness at Noon", "Arthur Koestler", "")).unwrap();
        assert_eq!(b.finish().unwrap(), 5);
        shelf.replace().unwrap();
        assert_eq!(shelf.books(), Some(5));
        let en = vec!["en".to_string()];
        let ask = |title, author, isbns: &[String]| shelf.find(&Ask { title, author, isbns, ols: &[], languages: &en }).unwrap();

        let got = ask("Left Hand of Darknes", "Ursula Le Guin", &[]);
        assert_eq!(got[0].md5, "a1");
        let got = ask("The Left Hand of Darkness", "", &["9780441478125".into()]);
        assert!(got[0].by_id && got[0].md5 == "a1");
        let got = ask("Goblin Slayer, Vol. 4", "Kagyu", &[]);
        assert_eq!(got[0].md5, "c3");
        assert!(got[0].score - got[1].score > 0.2);

        // A swap cut short after the index in use was moved aside is undone on the next start.
        drop(shelf);
        std::fs::rename(dir.path().join("index"), dir.path().join("index.old")).unwrap();
        assert_eq!(Shelf::new(dir.path()).books(), Some(5));
    }
}
