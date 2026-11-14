//! Audiobooks. A book can be wanted as an audiobook as well as, or instead of, an EPUB. Spool
//! looks for it in the indexers' audiobook category, an M4B first and MP3s when there is none,
//! downloads it, checks it is the book and long enough to be one, and files it in a location's
//! audiobooks folder as "Author/Title (Year)", reading its chapters for the player.
//!
//! The audiobook goes Wanted → Downloading → Have beside the EPUB's own way, by the steps
//! every wanted thing takes; see [`crate::hunt`].

use super::{Book, BookState};
use crate::app::App;
use crate::hunt::{copy_files, download_files, same_files, Filing, Followed, Hunt, HuntState, Search};
use crate::music::want::{a_piece, folder_safe, words};
use crate::newznab::Query;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How good a copy a release would give: one file with chapters, or many.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Files,
    M4b,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AudioFile {
    pub path: String,
    pub seconds: f64,
}

/// A place in the book to go to, `start` seconds from its beginning.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Chapter {
    pub title: String,
    pub start: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Audio {
    #[serde(flatten)]
    pub hunt: Hunt,
    pub kind: Kind,
    /// The folder it was filed in, and what is in it, in order.
    pub folder: String,
    pub files: Vec<AudioFile>,
    /// Tidied as they are read too, so books filed before a better tidy read the same.
    #[serde(deserialize_with = "tidied")]
    pub chapters: Vec<Chapter>,
    pub seconds: f64,
    /// A finished download to clear away once the filing is saved, so that losing the save
    /// to a change made meanwhile never loses what was filed.
    #[serde(skip)]
    pub filed_from: Option<(String, PathBuf)>,
}

/// Whether `path`, with every link on the way followed, is inside `root`, with its own followed.
pub(crate) fn inside(path: &Path, root: &Path) -> bool {
    match (std::fs::canonicalize(path), std::fs::canonicalize(root)) {
        (Ok(p), Ok(r)) => p.starts_with(r),
        _ => false,
    }
}

/// Audio files an audiobook can be in.
const AUDIO: &[&str] = &["m4b", "m4a", "mp3", "aac", "flac", "ogg", "opus", "mp4"];

fn is_audio(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| AUDIO.contains(&e.to_ascii_lowercase().as_str()))
}

/// For each word of a name (as [`words`] splits it), whether a bracket, dash or the like stands
/// between it and the word before. A dotted scene name is read with its dots as spaces.
pub(crate) fn separated(name: &str) -> Vec<bool> {
    let dotted = !name.contains(' ');
    let mut out = vec![];
    let mut sep = false;
    let mut in_word = false;
    for c in deunicode::deunicode(name).replace('&', " and ").replace('\'', "").chars() {
        if c.is_ascii_alphanumeric() {
            if !in_word {
                out.push(sep);
                sep = false;
                in_word = true;
            }
        } else {
            in_word = false;
            if matches!(c, '(' | ')' | '[' | ']' | '{' | '}' | '-' | ':' | ',' | '/' | '|') || (!dotted && c == '.') {
                sep = true;
            }
        }
    }
    out
}

/// Words a release names itself with after the title, which do not make it another book.
const TAGS: &[&str] = &["unabridged", "abridged", "m4b", "mp3", "aac", "flac", "ogg", "opus", "audiobook", "audiobooks", "audio", "book", "read", "narrated", "by", "retail", "chapterized", "chapters", "full", "cast", "edition", "anniversary", "audible", "web", "cbr", "vbr", "kbps"];

/// The name a book is known by in a release: before any subtitle, without "The" or "A".
fn short_title(title: &str) -> Vec<String> {
    let main = title.split([':', '(']).next().unwrap_or(title);
    let w = words(main);
    match w.first().map(String::as_str) {
        Some("the" | "a" | "an") if w.len() > 1 => w[1..].to_vec(),
        _ => w,
    }
}

/// What a release would give, or None when it is not the book or not an audiobook.
pub fn grade(name: &str, size: u64, author: &str, title: &str) -> Option<Kind> {
    if a_piece(name) || !(20 << 20..=10u64 << 30).contains(&size) {
        return None;
    }
    let n = words(name);
    let t = short_title(title);
    let surname = words(author).pop()?;
    // The title as it is written, in one run, and then nothing that would make it another book:
    // "Dune (Unabridged) M4B" is Dune, "Dune Messiah" is not. What may follow is a year, a
    // bitrate, words releases describe themselves with, the author, or the rest of the title.
    let (author_words, full_title) = (words(author), words(title));
    let fine = |w: &String| {
        let digits = w.trim_end_matches("kbps").trim_end_matches('k');
        (w.len() == 4 && (w.starts_with("19") || w.starts_with("20")) && w.chars().all(|c| c.is_ascii_digit()))
            || (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) && digits.len() < w.len())
            || TAGS.contains(&w.as_str())
            || author_words.contains(w)
            || full_title.contains(w)
    };
    // A word set off from the title by a bracket or a dash is something else, such as who reads
    // it ("[Andy Serkis]", "- Rob Inglis"); one that runs straight on is part of another title.
    let set_off = separated(name);
    let alone = (0..n.len().saturating_sub(t.len() - 1)).filter(|&i| n[i..].starts_with(&t)).any(|i| n.get(i + t.len()).is_none_or(|w| fine(w) || set_off.get(i + t.len()).copied().unwrap_or(false)));
    if t.is_empty() || !alone {
        return None;
    }
    if !n.contains(&surname) {
        return None;
    }
    let has = |w: &str| n.iter().any(|x| x == w);
    // The ebook, or a whole series, is not this audiobook.
    if ["epub", "pdf", "mobi", "azw3", "ebook", "ebooks"].iter().any(|w| has(w)) {
        return None;
    }
    Some(if has("m4b") { Kind::M4b } else { Kind::Files })
}

/// How a release judged to be the audiobook ranks: one file with chapters over many, then
/// unabridged over not said. An abridged one never does.
fn rank(name: &str, kind: Kind) -> Option<(Kind, bool)> {
    let w = words(name);
    let says = |x: &str| w.iter().any(|y| y == x);
    (!says("abridged")).then(|| (kind, says("unabridged")))
}

pub fn folder_name(b: &Book) -> PathBuf {
    let author = folder_safe(b.authors.first().map(String::as_str).unwrap_or("Unknown"));
    let year = if b.year > 0 { format!(" ({})", b.year) } else { String::new() };
    PathBuf::from(author).join(folder_safe(&format!("{}{year}", b.title)))
}

/// Chapters inside one file, as ffprobe reads them, each with its start in seconds.
fn file_chapters(ffprobe: &Path, file: &Path) -> Vec<Chapter> {
    let out = std::process::Command::new(ffprobe).args(["-v", "error", "-print_format", "json", "-show_chapters"]).arg(file).stdin(std::process::Stdio::null()).output();
    let Ok(out) = out else { return vec![] };
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    v["chapters"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, c)| Chapter { title: c["tags"]["title"].as_str().map(str::to_string).unwrap_or_else(|| format!("Chapter {}", i + 1)), start: c["start_time"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0) })
        .collect()
}

/// Read what was filed: each file's length in order, and the chapters, from inside the files
/// or, failing that, one for each file.
pub fn read_book(folder: &Path, ffprobe: Option<&Path>) -> (Vec<AudioFile>, Vec<Chapter>, f64) {
    let mut paths = vec![];
    let mut dirs = vec![folder.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            match std::fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => dirs.push(p),
                Ok(m) if m.is_file() && is_audio(&p) => paths.push(p),
                _ => {}
            }
        }
    }
    // "Part 2" before "Part 10".
    paths.sort_by_key(|p| natural(&p.to_string_lossy()));
    let mut files = vec![];
    let mut chapters = vec![];
    let mut at = 0.0;
    for p in &paths {
        let seconds = lofty::read_from_path(p).map(|f| lofty::file::AudioFile::properties(&f).duration().as_secs_f64()).unwrap_or(0.0);
        let inside = ffprobe.map(|f| file_chapters(f, p)).unwrap_or_default();
        if inside.len() > 1 {
            chapters.extend(inside.into_iter().map(|c| Chapter { title: c.title, start: at + c.start }));
        } else {
            let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("Part").to_string();
            chapters.push(Chapter { title: name, start: at });
        }
        files.push(AudioFile { path: p.to_string_lossy().into_owned(), seconds });
        at += seconds;
    }
    tidy(&mut chapters);
    (files, chapters, at)
}

/// Chapters named after their files often all begin "Author - Title - "; what they share up to
/// its last " - " goes, so each reads as what it is ("001 of 108"). "Chapter 1" and "1-01 …"
/// share nothing set off that way and are left as they are.
fn tidied<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Chapter>, D::Error> {
    let mut chapters = Vec::<Chapter>::deserialize(d)?;
    tidy(&mut chapters);
    Ok(chapters)
}

fn tidy(chapters: &mut [Chapter]) {
    if chapters.len() < 2 {
        return;
    }
    cut_shared(chapters);
    // Left with only a number ("001 of 108", "07"), a file is a part, not a named chapter.
    let numbered = |t: &str| {
        let t = t.split(" of ").next().unwrap_or(t);
        !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
    };
    if chapters.iter().all(|c| numbered(&c.title) || c.title.starts_with("Part ")) {
        for c in chapters.iter_mut() {
            if numbered(&c.title) {
                let n: u64 = c.title.split(" of ").next().and_then(|n| n.parse().ok()).unwrap_or(0);
                c.title = format!("Part {n}");
            }
        }
    }
}

fn cut_shared(chapters: &mut [Chapter]) {
    let first = chapters[0].title.clone();
    let shared = chapters.iter().skip(1).map(|c| first.chars().zip(c.title.chars()).take_while(|(a, b)| a == b).count()).min().unwrap_or(0);
    let common: String = first.chars().take(shared).collect();
    let Some(at) = common.rfind(" - ") else { return };
    let cut = common[..at + 3].chars().count();
    for c in chapters.iter_mut() {
        let rest: String = c.title.chars().skip(cut).collect::<String>().trim_matches([' ', '-', '_', '.']).to_string();
        if !rest.is_empty() {
            c.title = rest;
        }
    }
}

/// A name sorted with its numbers as numbers.
fn natural(s: &str) -> Vec<(String, u64)> {
    let mut out = vec![];
    let mut text = String::new();
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            if !num.is_empty() {
                out.push((std::mem::take(&mut text), num.parse().unwrap_or(0)));
                num.clear();
            }
            text.extend(c.to_lowercase());
        }
    }
    out.push((text, num.parse().unwrap_or(0)));
    out
}

impl App {
    /// The audiobooks folders of every location, with whether each can be reached now.
    pub fn audiobook_roots(&self) -> Vec<(String, bool)> {
        let placement = self.placement();
        placement.0.iter().filter(|(l, _)| !l.audiobook_root.is_empty()).map(|(l, online)| (l.audiobook_root.clone(), *online)).collect()
    }

    fn audiobook_roots_to_file(&self) -> Vec<String> {
        let placement = self.placement();
        placement.0.iter().filter(|(l, online)| *online && l.manage && !l.audiobook_root.is_empty()).map(|(l, _)| l.audiobook_root.clone()).collect()
    }

    /// Whether a file is part of an audiobook Spool keeps: in an audiobooks folder once every
    /// link on the way is followed, and a file itself.
    pub fn audiobook_file_ok(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file()) && self.audiobook_roots().iter().any(|(r, _)| inside(path, Path::new(r)))
    }

    /// Want a book as an audiobook too.
    pub fn want_audio(&self, id: i64) -> Result<Book> {
        let mut b = self.db.book(id)?.ok_or_else(|| anyhow!("no such book"))?;
        if b.audio.is_none() {
            b.audio = Some(Audio::default());
            self.db.save_book(&mut b)?;
        }
        self.nudge_book(id);
        Ok(b)
    }

    /// Stop wanting the audiobook, and stop its download. What was filed stays.
    pub fn drop_audio(&self, id: i64) -> Result<Book> {
        let mut b = self.db.book(id)?.ok_or_else(|| anyhow!("no such book"))?;
        if let Some(a) = &b.audio {
            if let Some(job) = a.hunt.download() {
                let _ = self.job_remove(job, true);
            }
        }
        b.audio = None;
        self.db.save_book(&mut b)?;
        Ok(b)
    }

    /// Look for the audiobook again now, giving releases turned down another chance.
    pub fn search_audio(&self, id: i64) -> Result<Book> {
        let mut b = self.db.book(id)?.ok_or_else(|| anyhow!("no such book"))?;
        if b.audio.as_mut().is_some_and(|a| a.hunt.search_again()) {
            self.db.save_book(&mut b)?;
            self.nudge_book(id);
        }
        Ok(b)
    }

    /// A download finished or failed: if it was an audiobook's, take it from there.
    pub fn book_job_done(&self, job: &str) -> bool {
        let Ok(books) = self.db.books() else { return false };
        match books.into_iter().find(|b| b.audio.as_ref().is_some_and(|a| a.hunt.download() == Some(job))) {
            Some(b) => {
                self.nudge_book(b.id);
                true
            }
            None => false,
        }
    }

    /// Move the audiobook along as far as it goes now. Called with the book held.
    pub(super) async fn advance_audio(&self, b: &mut Book) -> Result<()> {
        let Some(a) = b.audio.clone() else { return Ok(()) };
        let mut a = a;
        match a.hunt.state {
            HuntState::Wanted => self.look_for_audio(b, &mut a).await?,
            HuntState::Downloading => self.follow_audio(b, &mut a).await,
            HuntState::Have => {}
        }
        b.audio = Some(a);
        Ok(())
    }

    async fn look_for_audio(&self, b: &Book, a: &mut Audio) -> Result<()> {
        let author = b.authors.first().cloned().unwrap_or_default();
        let surname = words(&author).pop().unwrap_or_default();
        let search = Search {
            what: "audiobook",
            name: &b.title,
            no_folder: self.audiobook_roots_to_file().is_empty().then_some("No audiobooks folder Spool manages can be reached. Add one to a location in Settings → Library."),
            queries: vec![Query::Audiobook { text: format!("{} {surname}", short_title(&b.title).join(" ")) }],
            grade: &|r, _| grade(&r.title, r.size, &author, &b.title).and_then(|k| rank(&r.title, k)),
            none: "No audiobook of it found.",
        };
        if let Some((k, _)) = self.hunt_search(&mut a.hunt, search).await? {
            a.kind = k;
        }
        Ok(())
    }

    fn audio_failed(&self, b: &Book, a: &mut Audio, why: &str, release_at_fault: bool) {
        tracing::info!(book = %b.title, why, "an audiobook's download did not do");
        let r = a.hunt.release.as_ref();
        let _ = self.db.add_history(None, "audiobook_failed", serde_json::json!({"book_id": b.id, "book": b.title, "authors": b.authors, "release": r.map(|r| &r.title), "reason": why}));
        a.hunt.failed(why, release_at_fault);
    }

    async fn follow_audio(&self, b: &Book, a: &mut Audio) {
        match self.hunt_follow(&mut a.hunt) {
            Followed::Going => {}
            Followed::Failed(why, at_fault) => self.audio_failed(b, a, &why, at_fault),
            Followed::Finished(job, out) => match self.file_audio(b, &out).await {
                Ok((folder, files, chapters, seconds)) => {
                    // Cleared away once the filing is saved; see `filed_from`.
                    a.filed_from = Some((job, out));
                    tracing::info!(book = %b.title, folder = %folder, "filed an audiobook");
                    let r = a.hunt.release.as_ref();
                    let _ = self.db.add_history(None, "audiobook_filed", serde_json::json!({"book_id": b.id, "book": b.title, "authors": b.authors, "release": r.map(|r| &r.title), "indexer": r.map(|r| &r.indexer), "size": r.map(|r| r.size), "folder": folder, "seconds": seconds}));
                    (a.folder, a.files, a.chapters, a.seconds) = (folder, files, chapters, seconds);
                    a.hunt.filed();
                }
                Err(Filing::Wrong(why)) => {
                    self.hunt_clear(&job, &out).await;
                    self.audio_failed(b, a, &why, true);
                }
                Err(Filing::Later(why)) => a.hunt.not_filed_yet(&why),
            },
        }
    }

    /// Check a download is the book and long enough to be one, and copy it into the folder.
    async fn file_audio(&self, b: &Book, out: &Path) -> std::result::Result<(String, Vec<AudioFile>, Vec<Chapter>, f64), Filing> {
        let root = self.audiobook_roots_to_file().into_iter().next().map(PathBuf::from).ok_or_else(|| Filing::Later("no audiobooks folder Spool manages can be reached".into()))?;
        let dest = root.join(folder_name(b));
        let (out, ffprobe) = (out.to_path_buf(), self.ffprobe_path());
        tokio::task::spawn_blocking(move || {
            let files = download_files(&out).map_err(|e| Filing::Wrong(format!("{e:#}")))?;
            if !files.iter().any(|(p, _)| is_audio(p)) {
                return Err(Filing::Wrong("the download held no audio".into()));
            }
            // An audiobook runs to hours; something of minutes is a sample or a chapter.
            let (_, _, seconds) = read_book(&out, None);
            if seconds > 0.0 && seconds < 20.0 * 60.0 {
                return Err(Filing::Wrong(format!("it is only {} minutes long", (seconds / 60.0).round())));
            }
            if dest.exists() && !same_files(&files, &dest) {
                return Err(Filing::Later(format!("{} is already there", dest.display())));
            }
            if !dest.exists() {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| Filing::Later(e.to_string()))?;
                    // An author's folder that is a link to somewhere else is not written through.
                    if !inside(parent, &root) {
                        return Err(Filing::Later(format!("{} leads out of the audiobooks folder", parent.display())));
                    }
                }
                copy_files(&files, &dest).map_err(|e| Filing::Later(format!("{e:#}")))?;
            }
            let (files, chapters, seconds) = read_book(&dest, ffprobe.as_deref());
            Ok((dest.to_string_lossy().into_owned(), files, chapters, seconds))
        })
        .await
        .map_err(|e| Filing::Later(e.to_string()))?
    }

    /// One file of a book's audiobook, for the player.
    pub fn audio_file(&self, b: &Book, n: usize) -> Option<PathBuf> {
        let f = b.audio.as_ref().filter(|a| a.hunt.state == HuntState::Have)?.files.get(n)?;
        let p = PathBuf::from(&f.path);
        self.audiobook_file_ok(&p).then_some(p)
    }
}

/// Whether a book is here in some form: its EPUB got, or its audiobook filed.
pub fn have(b: &Book) -> bool {
    (b.epub && b.state == BookState::Have) || b.audio.as_ref().is_some_and(|a| a.hunt.state == HuntState::Have)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audiobook_releases_are_graded_by_name() {
        let mb = 1 << 20;
        let g = |n: &str| grade(n, 400 * mb, "Frank Herbert", "Dune");
        assert_eq!(g("Frank Herbert - Dune (Unabridged) [M4B]"), Some(Kind::M4b));
        assert_eq!(g("Herbert, Frank - Dune - Read by Scott Brick 64k MP3"), Some(Kind::Files));
        assert_eq!(g("Frank Herbert - Dune Messiah [M4B]"), None, "another book that starts the same");
        assert_eq!(g("Frank.Herbert-Dune.Messiah.M4B"), None, "dotted, the same");
        assert_eq!(g("Frank Herbert - Dune [Scott Brick] M4B"), Some(Kind::M4b), "who reads it, set off");
        assert_eq!(grade("J.R.R. Tolkien - The Fellowship of the Ring - Rob Inglis", 400 << 20, "J.R.R. Tolkien", "The Fellowship of the Ring"), Some(Kind::Files));
        assert_eq!(g("Dune - Frank Herbert - 2007 - 64kbps"), Some(Kind::Files), "the author after the title");
        assert_eq!(g("Frank Herbert - Dune - Book 1 of the Dune Chronicles M4B"), Some(Kind::M4b));
        assert_eq!(g("Frank Herbert - Dune [EPUB]"), None, "the ebook");
        assert_eq!(g("Brian Herbert - House Atreides"), None);
        assert_eq!(grade("Frank Herbert - Dune [M4B]", mb, "Frank Herbert", "Dune"), None, "too small");
        assert_eq!(grade("Andy Weir - The Martian (2014) M4B", 400 * mb, "Andy Weir", "The Martian"), Some(Kind::M4b));
        assert_eq!(grade("Andy Weir - Project Hail Mary M4B", 400 * mb, "Andy Weir", "Project Hail Mary: A Novel"), Some(Kind::M4b), "subtitle left off");
    }

    #[test]
    fn one_file_with_chapters_and_unabridged_are_taken_first() {
        let r = |t: &str| crate::models::Release { guid: t.into(), title: t.into(), ..Default::default() };
        let found: Vec<_> = [("Dune MP3", Kind::Files), ("Dune Abridged M4B", Kind::M4b), ("Dune M4B", Kind::M4b), ("Dune Unabridged MP3", Kind::Files)].into_iter().filter_map(|(t, k)| Some((r(t), rank(t, k)?))).collect();
        assert_eq!(crate::hunt::best(&found, &[]).unwrap().0.title, "Dune M4B");
        let found: Vec<_> = [("Dune MP3", Kind::Files), ("Dune Unabridged MP3", Kind::Files)].into_iter().filter_map(|(t, k)| Some((r(t), rank(t, k)?))).collect();
        assert_eq!(crate::hunt::best(&found, &[]).unwrap().0.title, "Dune Unabridged MP3");
    }

    #[test]
    fn chapter_names_lose_what_they_all_share() {
        let mut ch: Vec<Chapter> = ["Alexandre Dumas - The Count of Monte Cristo - 001 of 108", "Alexandre Dumas - The Count of Monte Cristo - 002 of 108"].iter().map(|t| Chapter { title: t.to_string(), start: 0.0 }).collect();
        tidy(&mut ch);
        assert_eq!(ch[0].title, "Part 1");
        tidy(&mut ch);
        assert_eq!(ch[1].title, "Part 2", "tidying twice changes nothing");
        let mut ch: Vec<Chapter> = ["Chapter 1", "Chapter 2"].iter().map(|t| Chapter { title: t.to_string(), start: 0.0 }).collect();
        tidy(&mut ch);
        assert_eq!(ch[1].title, "Chapter 2", "nothing set off is shared");
        let mut ch: Vec<Chapter> = ["1-01 Disc One Intro", "1-02 Recap"].iter().map(|t| Chapter { title: t.to_string(), start: 0.0 }).collect();
        tidy(&mut ch);
        assert_eq!(ch[0].title, "1-01 Disc One Intro");
    }

    #[test]
    fn parts_sort_by_their_numbers() {
        let mut v = vec!["Part 10.mp3", "Part 2.mp3", "Part 1.mp3"];
        v.sort_by_key(|s| natural(s));
        assert_eq!(v, ["Part 1.mp3", "Part 2.mp3", "Part 10.mp3"]);
    }

    #[test]
    fn a_book_is_filed_under_its_author() {
        let b = Book { title: "Dune".into(), authors: vec!["Frank Herbert".into()], year: 1965, ..Default::default() };
        assert_eq!(folder_name(&b), PathBuf::from("Frank Herbert/Dune (1965)"));
    }
}
