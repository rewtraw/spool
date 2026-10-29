//! What is said in the library, to find a moment by its words: "wake up", or anything said
//! like it.
//!
//! Every film and episode a clip can be cut from is read for its subtitles in the household's
//! language, text inside the file or beside it. Each line is kept with its time, to be found
//! by its words, and every few lines said together are kept as a point placed by what they
//! mean (see [`crate::embed`]), to be found when it is put another way: "don't go to sleep"
//! finds "whatever you do, don't fall asleep". A search weighs both, so the words as written
//! come first and what means much the same follows.
//!
//! A file with no subtitles to read, only pictures of them as on a disc or none at all, gets
//! some from OpenSubtitles when an account is set, checked against the film's own speech
//! before they are kept. They are put beside the video, where players find them too. A few of
//! the day's allowance are always left for looking by hand.
//!
//! The work is done in the background, a file at a time, and waits while anything is being
//! played. It is kept in a database of its own beside Spool's, as it is large and can always
//! be made again.

use crate::app::App;
use crate::captions::Source;
use crate::db::now;
use crate::embed::{Embedder, DIMENSIONS, MODEL};
use crate::models::{Episode, Kind, MediaFile, Title};
use crate::subs::Cue;
use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The shape of the database. When it changes the database is made again from the start.
const SHAPE: i64 = 1;
const SCHEMA: &str = r#"
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

    -- Each file read, by its id in Spool's own database, with what it was read from and the
    -- file as it was then: it is read again when that changes.
    CREATE TABLE files (
        file_id INTEGER PRIMARY KEY,
        title_id INTEGER NOT NULL,
        episode_id INTEGER,
        mark TEXT NOT NULL,
        lines INTEGER NOT NULL DEFAULT 0,
        read_at INTEGER NOT NULL,
        error TEXT NOT NULL DEFAULT ''
    );

    CREATE TABLE lines (id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL, start REAL NOT NULL, end REAL NOT NULL, text TEXT NOT NULL);
    CREATE INDEX lines_by_file ON lines(file_id);

    -- A few lines said together, from line `first` to line `last`, what they say, and the
    -- point their meaning is placed at once it has been worked out.
    CREATE TABLE passages (id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL, first INTEGER NOT NULL, last INTEGER NOT NULL, text TEXT NOT NULL, point BLOB);
    -- The passages by their words, each word by its stem: "awakening" finds "awakened".
    CREATE VIRTUAL TABLE words USING fts5(text, content='passages', content_rowid='id', tokenize='porter unicode61 remove_diacritics 2');
    CREATE INDEX passages_by_file ON passages(file_id);
    CREATE INDEX passages_to_place ON passages(id) WHERE point IS NULL;

    -- Files OpenSubtitles was asked about, so each is asked again only once in a while.
    -- found: 1 when subtitles that fit were fetched, 0 when it had none, -1 when asking failed.
    CREATE TABLE asked (file_id INTEGER PRIMARY KEY, at INTEGER NOT NULL, found INTEGER NOT NULL);
"#;

/// Subtitles fetched are left alone at this many a day, for looking by hand.
const KEEP_FOR_HAND: i64 = 10;
/// How long before OpenSubtitles is asked again about a file it had nothing for.
const ASK_AGAIN: i64 = 30 * 86400;
/// How long before a file is asked about again when asking about it failed.
const TRY_AGAIN: i64 = 86400;

/// What came of asking OpenSubtitles about a file.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Asked {
    Found = 1,
    None = 0,
    Failed = -1,
}
/// How alike in meaning a passage must be to what is looked for to be found by that alone.
const ALIKE: f32 = 0.45;

/// What the index covers, and what it is doing.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Coverage {
    /// Films and episodes there are to search.
    pub files: usize,
    /// Of those, the ones whose lines have been read.
    pub read: usize,
    pub lines: usize,
    /// Passages placed by their meaning, of all there are.
    pub placed: usize,
    pub passages: usize,
    /// Files with no subtitles that can be read: only pictures of them, or none.
    pub unreadable: usize,
    /// Of those, ones OpenSubtitles had nothing in time with.
    pub not_found: usize,
    /// Files on locations that cannot be reached right now.
    pub away: usize,
    /// Whether subtitles can be fetched for the files without: an OpenSubtitles key is set.
    pub can_fetch: bool,
    /// How many more subtitles OpenSubtitles will hand over today, as it last said.
    pub fetches_left: Option<i64>,
    /// What it is doing, in words; empty when it is idle.
    pub doing: String,
    /// What is in the way, if anything: of searching by meaning, of fetching subtitles.
    pub problems: Vec<String>,
}

/// A moment found, with the lines around it.
#[derive(Clone, Debug, Serialize)]
pub struct Moment {
    pub title_id: i64,
    pub episode_id: Option<i64>,
    /// What it is in, in words: "Dark City (1998)", "Ergo Proxy S01E01 · Awakening".
    pub from: String,
    pub file_id: i64,
    /// From the start of the first line found to the end of the last.
    pub start: f64,
    pub end: f64,
    pub lines: Vec<Line>,
    /// A couple of lines either side, for what it is said in answer to.
    pub before: Vec<Line>,
    pub after: Vec<Line>,
    /// How well it fits, higher the better; and whether the words looked for are said in it.
    pub score: f32,
    pub words: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Line {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// What a search found.
#[derive(Clone, Debug, Serialize)]
pub struct Found {
    pub moments: Vec<Moment>,
    /// Whether what is meant was looked for, and not only the words.
    pub by_meaning: bool,
    pub coverage: Coverage,
}

/// A passage found, with the lines either side, the file it is in and what that is of.
struct Passage {
    file: i64,
    before: Vec<Line>,
    lines: Vec<Line>,
    after: Vec<Line>,
    title: i64,
    episode: Option<i64>,
}

/// The passages placed by meaning, held in memory as which side of the middle each number
/// is on: a rough likeness that narrows thousands to the few worth a proper look.
#[derive(Clone, Default)]
struct Signs {
    ids: Vec<i64>,
    files: Vec<i64>,
    bits: Vec<[u64; DIMENSIONS / 64]>,
}

pub struct Index {
    conn: Mutex<Connection>,
    /// Where the model is fetched from.
    source: Mutex<String>,
    model: tokio::sync::Mutex<Option<Arc<Embedder>>>,
    /// When the model last could not be had, and why: it is tried again after a while.
    model_failed: Mutex<Option<(i64, String)>>,
    signs: Mutex<Option<Arc<Signs>>>,
    coverage: Mutex<Coverage>,
    /// When OpenSubtitles should next be asked after it last failed, and why it did.
    /// With the account it failed with.
    fetch_paused: Mutex<Option<(i64, String, String)>>,
    /// When each file without subtitles to read was last looked beside for some put there.
    looked_beside: Mutex<HashMap<i64, i64>>,
    /// Set to have the work looked at again now.
    pub wake: tokio::sync::Notify,
}

impl Index {
    pub fn open(path: &Path) -> Result<Index> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let shape: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if shape != SHAPE {
            // Made again from the start: everything in it can be read again.
            conn.execute_batch("DROP TABLE IF EXISTS words; DROP TABLE IF EXISTS lines; DROP TABLE IF EXISTS passages; DROP TABLE IF EXISTS files; DROP TABLE IF EXISTS asked; DROP TABLE IF EXISTS meta;")?;
            conn.execute_batch(&format!("BEGIN; {SCHEMA} PRAGMA user_version = {SHAPE}; COMMIT;"))?;
        }
        // Points worked out by another model mean nothing to this one.
        let model: Option<String> = conn.query_row("SELECT value FROM meta WHERE key = 'model'", [], |r| r.get(0)).optional()?;
        if model.as_deref() != Some(MODEL) {
            conn.execute("UPDATE passages SET point = NULL", [])?;
            conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('model', ?)", [MODEL])?;
        }
        Ok(Index {
            conn: Mutex::new(conn),
            source: Mutex::new(crate::embed::SOURCE.to_string()),
            looked_beside: Mutex::new(HashMap::new()),
            model: Default::default(),
            model_failed: Mutex::new(None),
            signs: Mutex::new(None),
            coverage: Mutex::new(Coverage::default()),
            fetch_paused: Mutex::new(None),
            wake: tokio::sync::Notify::new(),
        })
    }

    /// Fetch the model from elsewhere: for tests, a folder of its files, or nowhere.
    pub fn take_model_from(&self, source: &str) {
        *self.source.lock() = source.to_string();
    }

    pub fn coverage(&self) -> Coverage {
        self.coverage.lock().clone()
    }

    fn doing(&self, what: &str) {
        self.coverage.lock().doing = what.to_string();
    }

    /// The files read, each with what it was read from and whether that failed.
    fn marks(&self) -> Result<HashMap<i64, (String, String, i64)>> {
        let conn = self.conn.lock();
        let mut q = conn.prepare("SELECT file_id, mark, error, read_at FROM files")?;
        let rows = q.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn asked(&self) -> Result<HashMap<i64, (i64, Asked)>> {
        let conn = self.conn.lock();
        let mut q = conn.prepare("SELECT file_id, at, found FROM asked")?;
        let rows = q.query_map([], |r| {
            let found = match r.get::<_, i64>(2)? {
                1 => Asked::Found,
                0 => Asked::None,
                _ => Asked::Failed,
            };
            Ok((r.get(0)?, (r.get(1)?, found)))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn note_asked(&self, file_id: i64, found: Asked) -> Result<()> {
        self.conn.lock().execute("INSERT OR REPLACE INTO asked (file_id, at, found) VALUES (?, ?, ?)", params![file_id, now(), found as i64])?;
        Ok(())
    }

    /// Forget asking about these files, as what went wrong was nothing to do with them.
    fn unask(&self, files: &[i64]) -> Result<()> {
        let conn = self.conn.lock();
        for f in files {
            conn.execute("DELETE FROM asked WHERE file_id = ?", [f])?;
        }
        Ok(())
    }

    fn remove_rows(conn: &Connection, file_id: i64) -> rusqlite::Result<()> {
        conn.execute("INSERT INTO words (words, rowid, text) SELECT 'delete', id, text FROM passages WHERE file_id = ?", [file_id])?;
        conn.execute("DELETE FROM lines WHERE file_id = ?", [file_id])?;
        conn.execute("DELETE FROM passages WHERE file_id = ?", [file_id])?;
        Ok(())
    }

    /// Leave out the signs of files whose passages have gone. The signs are held the while the
    /// database is changed, and while they are read from it, so the two always agree.
    fn drop_signs_in(mut held: parking_lot::MutexGuard<'_, Option<Arc<Signs>>>, files: &HashSet<i64>) {
        if let Some(signs) = held.as_ref() {
            if signs.files.iter().any(|f| files.contains(f)) {
                let mut kept = Signs::default();
                for i in (0..signs.ids.len()).filter(|&i| !files.contains(&signs.files[i])) {
                    kept.ids.push(signs.ids[i]);
                    kept.files.push(signs.files[i]);
                    kept.bits.push(signs.bits[i]);
                }
                *held = Some(Arc::new(kept));
            }
        }
    }

    /// Forget files that are no longer there to be searched.
    fn forget_all_but(&self, keep: &HashSet<i64>) -> Result<usize> {
        let gone: HashSet<i64> = self.marks()?.into_keys().filter(|id| !keep.contains(id)).collect();
        if gone.is_empty() {
            return Ok(0);
        }
        let held = self.signs.lock();
        {
            let mut conn = self.conn.lock();
            let tx = conn.transaction()?;
            for &id in &gone {
                Self::remove_rows(&tx, id)?;
                tx.execute("DELETE FROM files WHERE file_id = ?", [id])?;
            }
            tx.commit()?;
        }
        Self::drop_signs_in(held, &gone);
        Ok(gone.len())
    }

    /// Keep a file's lines, in place of what was kept of it before.
    fn put(&self, want: &Want, cues: &[Cue]) -> Result<()> {
        let held = self.signs.lock();
        {
            let mut conn = self.conn.lock();
            let tx = conn.transaction()?;
            Self::remove_rows(&tx, want.file_id)?;
            let mut ids = Vec::with_capacity(cues.len());
            {
                let mut line = tx.prepare("INSERT INTO lines (file_id, start, end, text) VALUES (?, ?, ?, ?)")?;
                for c in cues {
                    line.execute(params![want.file_id, c.start, c.end, c.text])?;
                    ids.push(tx.last_insert_rowid());
                }
                let mut passage = tx.prepare("INSERT INTO passages (file_id, first, last, text) VALUES (?, ?, ?, ?)")?;
                let mut word = tx.prepare("INSERT INTO words (rowid, text) VALUES (?, ?)")?;
                for (a, b) in passages(cues) {
                    let text = cues[a..=b].iter().map(|c| c.text.replace('\n', " ")).collect::<Vec<_>>().join(" ");
                    passage.execute(params![want.file_id, ids[a], ids[b], text])?;
                    word.execute(params![tx.last_insert_rowid(), text])?;
                }
            }
            tx.execute(
                "INSERT OR REPLACE INTO files (file_id, title_id, episode_id, mark, lines, read_at, error) VALUES (?, ?, ?, ?, ?, ?, '')",
                params![want.file_id, want.title_id, want.episode_id, want.mark, cues.len(), now()],
            )?;
            tx.commit()?;
        }
        Self::drop_signs_in(held, &HashSet::from([want.file_id]));
        Ok(())
    }

    /// Note that a file could not be read, so that it is not tried again until it changes or a
    /// day has gone by.
    fn put_failure(&self, want: &Want, error: &str) -> Result<()> {
        let held = self.signs.lock();
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        Self::remove_rows(&tx, want.file_id)?;
        tx.execute(
            "INSERT OR REPLACE INTO files (file_id, title_id, episode_id, mark, lines, read_at, error) VALUES (?, ?, ?, ?, 0, ?, ?)",
            params![want.file_id, want.title_id, want.episode_id, want.mark, now(), error],
        )?;
        tx.commit()?;
        drop(conn);
        Self::drop_signs_in(held, &HashSet::from([want.file_id]));
        Ok(())
    }

    /// Passages not yet placed by meaning, with what they say.
    fn unplaced(&self, n: usize) -> Result<Vec<(i64, i64, String)>> {
        let conn = self.conn.lock();
        let mut q = conn.prepare("SELECT id, file_id, text FROM passages WHERE point IS NULL LIMIT ?")?;
        let rows = q.query_map([n as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn place(&self, placed: &[(i64, i64, Vec<f32>)]) -> Result<()> {
        let mut held = self.signs.lock();
        // A passage of a file read again while it was being worked out is there no longer.
        let mut still = vec![];
        {
            let mut conn = self.conn.lock();
            let tx = conn.transaction()?;
            {
                let mut q = tx.prepare("UPDATE passages SET point = ? WHERE id = ?")?;
                for (id, _, point) in placed {
                    still.push(q.execute(params![pack(point), id])? > 0);
                }
            }
            tx.commit()?;
        }
        if let Some(signs) = held.as_mut() {
            let s = Arc::make_mut(signs);
            for ((id, file, point), _) in placed.iter().zip(&still).filter(|(_, there)| **there) {
                s.ids.push(*id);
                s.files.push(*file);
                s.bits.push(signs_of(point));
            }
        }
        Ok(())
    }

    /// The passages' signs, read from the database the first time they are wanted.
    fn signs(&self) -> Result<Arc<Signs>> {
        let mut held = self.signs.lock();
        if let Some(s) = held.as_ref() {
            return Ok(s.clone());
        }
        let mut signs = Signs::default();
        {
            let conn = self.conn.lock();
            let mut q = conn.prepare("SELECT id, file_id, point FROM passages WHERE point IS NOT NULL")?;
            let mut rows = q.query([])?;
            while let Some(r) = rows.next()? {
                let point: Vec<u8> = r.get(2)?;
                let Some(v) = unpack(&point) else { continue };
                signs.ids.push(r.get(0)?);
                signs.files.push(r.get(1)?);
                signs.bits.push(signs_of(&v));
            }
        }
        let signs = Arc::new(signs);
        *held = Some(signs.clone());
        Ok(signs)
    }

    fn points(&self, ids: &[i64]) -> Result<HashMap<i64, Vec<f32>>> {
        let conn = self.conn.lock();
        let mut out = HashMap::new();
        for chunk in ids.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(",");
            let mut q = conn.prepare(&format!("SELECT id, point FROM passages WHERE point IS NOT NULL AND id IN ({marks})"))?;
            let mut rows = q.query(rusqlite::params_from_iter(chunk))?;
            while let Some(r) = rows.next()? {
                if let Some(v) = unpack(&r.get::<_, Vec<u8>>(1)?) {
                    out.insert(r.get(0)?, v);
                }
            }
        }
        Ok(out)
    }

    /// Passages whose lines have the words, all of them together as written (true) or each
    /// somewhere in it (false).
    fn with_words(&self, query: &str, only: Option<&HashSet<i64>>) -> Result<HashMap<i64, bool>> {
        let words: Vec<String> = query.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
        if words.is_empty() {
            return Ok(HashMap::new());
        }
        let together = format!("\"{}\"", words.join(" "));
        let each = words.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join(" AND ");
        // Kept to the files asked for before the best are taken, so others cannot crowd them out.
        let files = only.map(|o| serde_json::to_string(&o.iter().collect::<Vec<_>>()).unwrap_or_default());
        let conn = self.conn.lock();
        let mut out: HashMap<i64, bool> = HashMap::new();
        for (wanted, exact) in [(together, true), (each, false)] {
            let mut q = conn.prepare(
                "SELECT words.rowid FROM words JOIN passages p ON p.id = words.rowid
                 WHERE words MATCH ?1 AND (?2 IS NULL OR p.file_id IN (SELECT value FROM json_each(?2)))
                 ORDER BY rank LIMIT 2000",
            )?;
            let mut rows = q.query(params![wanted, files])?;
            while let Some(r) = rows.next()? {
                let was = out.entry(r.get(0)?).or_insert(exact);
                *was |= exact;
            }
        }
        Ok(out)
    }

    /// A passage with the lines either side, and the file it is in.
    fn passage(&self, id: i64) -> Result<Option<Passage>> {
        const AROUND: i64 = 2;
        let conn = self.conn.lock();
        let Some((file, first, last, title, episode)) = conn
            .query_row("SELECT p.file_id, p.first, p.last, f.title_id, f.episode_id FROM passages p JOIN files f ON f.file_id = p.file_id WHERE p.id = ?", [id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, Option<i64>>(4)?)))
            .optional()?
        else {
            return Ok(None);
        };
        let mut q = conn.prepare("SELECT id, start, end, text FROM lines WHERE file_id = ? AND id BETWEEN ? AND ? ORDER BY id")?;
        let rows = q.query_map(params![file, first - AROUND, last + AROUND], |r| Ok((r.get::<_, i64>(0)?, Line { start: r.get(1)?, end: r.get(2)?, text: r.get(3)? })))?;
        let (mut before, mut lines, mut after) = (vec![], vec![], vec![]);
        for row in rows {
            let (lid, line) = row?;
            if lid < first {
                before.push(line);
            } else if lid > last {
                after.push(line);
            } else {
                lines.push(line);
            }
        }
        Ok(Some(Passage { file, before, lines, after, title, episode }))
    }

    /// Whether a file's words have been read into the index.
    pub fn has_read(&self, file_id: i64) -> Result<bool> {
        let conn = self.conn.lock();
        Ok(conn.query_row("SELECT lines > 0 FROM files WHERE file_id = ?", [file_id], |r| r.get(0)).optional()?.unwrap_or(false))
    }

    /// The lines of a file shown between two times, in order.
    pub fn lines_between(&self, file_id: i64, from: f64, to: f64) -> Result<Vec<Line>> {
        let conn = self.conn.lock();
        let mut q = conn.prepare("SELECT start, end, text FROM lines WHERE file_id = ? AND end > ? AND start < ? ORDER BY start LIMIT 500")?;
        let rows = q.query_map(params![file_id, from, to], |r| Ok(Line { start: r.get(0)?, end: r.get(1)?, text: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Everything said in a film or an episode, in order: from the file of it read for the most
    /// lines, when there are several. None when none of it has been read.
    pub fn transcript(&self, title_id: i64, episode_id: Option<i64>) -> Result<Option<(i64, Vec<Line>)>> {
        let conn = self.conn.lock();
        let file: Option<i64> = conn
            .query_row("SELECT file_id FROM files WHERE title_id = ? AND episode_id IS ? AND lines > 0 ORDER BY lines DESC LIMIT 1", params![title_id, episode_id], |r| r.get(0))
            .optional()?;
        let Some(file) = file else { return Ok(None) };
        let mut q = conn.prepare("SELECT start, end, text FROM lines WHERE file_id = ? ORDER BY start")?;
        let rows = q.query_map([file], |r| Ok(Line { start: r.get(0)?, end: r.get(1)?, text: r.get(2)? }))?;
        Ok(Some((file, rows.collect::<rusqlite::Result<_>>()?)))
    }

    fn files_of_title(&self, title_id: i64) -> Result<HashSet<i64>> {
        let conn = self.conn.lock();
        let mut q = conn.prepare("SELECT file_id FROM files WHERE title_id = ?")?;
        let rows = q.query_map([title_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn counts(&self) -> Result<(usize, usize, usize)> {
        let conn = self.conn.lock();
        let lines: i64 = conn.query_row("SELECT coalesce(sum(lines), 0) FROM files", [], |r| r.get(0))?;
        let passages: i64 = conn.query_row("SELECT count(*) FROM passages", [], |r| r.get(0))?;
        let unplaced: i64 = conn.query_row("SELECT count(*) FROM passages WHERE point IS NULL", [], |r| r.get(0))?;
        Ok((lines as usize, passages as usize, (passages - unplaced) as usize))
    }
}

/// A point as it is kept: how large its largest number is, then each number as a whole
/// number from -127 to 127 in proportion to that.
fn pack(point: &[f32]) -> Vec<u8> {
    let scale = point.iter().fold(0f32, |m, v| m.max(v.abs())).max(f32::MIN_POSITIVE);
    let mut out = scale.to_le_bytes().to_vec();
    out.extend(point.iter().map(|v| ((v / scale * 127.0).round().clamp(-127.0, 127.0) as i8) as u8));
    out
}

fn unpack(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() != 4 + DIMENSIONS {
        return None;
    }
    let scale = f32::from_le_bytes(bytes[..4].try_into().ok()?) / 127.0;
    Some(bytes[4..].iter().map(|b| (*b as i8) as f32 * scale).collect())
}

fn signs_of(point: &[f32]) -> [u64; DIMENSIONS / 64] {
    let mut out = [0u64; DIMENSIONS / 64];
    for (i, v) in point.iter().enumerate() {
        if *v > 0.0 {
            out[i / 64] |= 1 << (i % 64);
        }
    }
    out
}

fn alike(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// A file's lines made ready to keep: in order, and without the same words twice at once,
/// as subtitles drawn in layers can have.
fn tidy(mut cues: Vec<Cue>) -> Vec<Cue> {
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Cue> = vec![];
    for c in cues {
        let words = c.text.trim_matches(|ch: char| ch == '♪' || ch == '#' || ch.is_whitespace());
        if words.is_empty() {
            continue;
        }
        if let Some(last) = out.iter_mut().rev().take(4).find(|l| l.text == c.text && c.start <= l.end + 0.05) {
            last.end = last.end.max(c.end);
            continue;
        }
        out.push(c);
    }
    out
}

/// Lines said together, as ranges of positions: up to three lines in a row with little
/// pause between them.
fn passages(cues: &[Cue]) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut start = 0;
    for i in 0..cues.len() {
        let length: usize = cues[start..=i].iter().map(|c| c.text.len()).sum();
        let next_apart = cues.get(i + 1).is_none_or(|n| n.start - cues[i].end > 2.5);
        if next_apart || i + 1 - start >= 3 || length > 160 {
            out.push((start, i));
            start = i + 1;
        }
    }
    out
}

/// A film or an episode to search, and where its words are to be read from.
#[derive(Clone, Debug)]
struct Want {
    file_id: i64,
    title_id: i64,
    episode_id: Option<i64>,
    path: PathBuf,
    /// Its subtitles in the household's language, if any can be read.
    source: Option<Source>,
    /// What it is read from and the file as it is: read again when this changes.
    mark: String,
    /// The file's streams, to read a track inside it by.
    streams: Vec<(u32, String, String)>,
    away: bool,
    /// Whether subtitles may be put beside it.
    can_take: bool,
}

/// The subtitles of a file that can be read, in a language: text inside it, full before those
/// for the hard of hearing, then a file of them beside it. Never only the lines always shown.
fn readable(title: &Title, file: &MediaFile, language: &str) -> Option<(Source, String)> {
    let ours = |code: &str| crate::subs::language_code(code) == Some(language);
    if let Some(info) = &file.media_info {
        let mut text: Vec<_> = info.streams.iter().filter(|s| s.kind == "subtitle" && crate::captions::is_text(&s.codec) && !s.forced && !crate::captions::only_signs(s) && ours(&s.language)).collect();
        text.sort_by_key(|s| crate::captions::for_the_hard_of_hearing(s));
        if let Some(s) = text.first() {
            return Some((Source::Text(s.index), format!("track {} {}", s.index, s.codec)));
        }
    }
    if file.base.is_some() {
        return None;
    }
    let mut beside: Vec<_> = file.subtitles.iter().filter(|s| !s.forced && ours(&s.language)).collect();
    beside.sort_by_key(|s| s.hearing_impaired);
    beside.first().map(|s| {
        let path = Path::new(&title.path).join(&s.rel_path);
        // A file of subtitles edited or put right is read again.
        let changed = std::fs::metadata(&path).map(|m| format!("{} {}", m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0))).unwrap_or_default();
        (Source::Beside(path), format!("beside {} {changed}", s.rel_path))
    })
}

impl App {
    /// The films and episodes there are to search.
    fn dialogue_wants(&self, language: &str) -> Result<Vec<Want>> {
        let places = self.placement();
        let mut out = vec![];
        for title in self.db.titles(None)? {
            let can_take = places.locked(&title).is_none();
            let files: Vec<(MediaFile, Option<i64>)> = match title.kind {
                Kind::Movie => self.film_file(&title)?.into_iter().map(|f| (f, None)).collect(),
                Kind::Series => {
                    let mut by_id: HashMap<i64, MediaFile> = self.db.files(title.id)?.into_iter().map(|f| (f.id, f)).collect();
                    let mut episodes: Vec<Episode> = self.db.episodes(title.id)?;
                    episodes.sort_by_key(|e| (e.season, e.episode));
                    // A file holding two episodes is read once, as the first.
                    episodes.into_iter().filter_map(|e| Some((by_id.remove(&e.file_id?)?, Some(e.id)))).collect()
                }
            };
            for (mut file, episode_id) in files {
                let mut source = readable(&title, &file, language);
                // Subtitles put beside a file by hand are noticed within a day.
                if source.is_none() && file.base.is_none() && places.online_at(&title.path) {
                    let due = self.dialogue.looked_beside.lock().get(&file.id).is_none_or(|at| now() - at > 86400);
                    if due {
                        self.dialogue.looked_beside.lock().insert(file.id, now());
                        if let Ok(found) = self.find_sidecars(file.id) {
                            file.subtitles = found;
                            source = readable(&title, &file, language);
                        }
                    }
                }
                let mark = format!("{} {} {}", file.rel_path, file.size, source.as_ref().map(|s| s.1.as_str()).unwrap_or("none"));
                let streams = file.media_info.as_ref().map(crate::captions::streams_of).unwrap_or_default();
                out.push(Want {
                    file_id: file.id,
                    title_id: title.id,
                    episode_id,
                    path: title.file_path(&file),
                    source: source.map(|s| s.0),
                    mark,
                    streams,
                    away: !places.online_at(title.folder_of(&file)),
                    can_take: can_take && file.base.is_none(),
                });
            }
        }
        Ok(out)
    }

    /// The model, fetched and read the first time it is wanted.
    async fn dialogue_model(&self) -> Result<Arc<Embedder>> {
        let mut held = self.dialogue.model.lock().await;
        if let Some(m) = held.as_ref() {
            return Ok(m.clone());
        }
        if let Some((at, why)) = self.dialogue.model_failed.lock().clone() {
            if now() - at < 3600 {
                return Err(anyhow!(why));
            }
        }
        let dir = crate::embed::model_dir(&self.data_dir);
        let loaded: Result<Arc<Embedder>> = async {
            let source = self.dialogue.source.lock().clone();
            tokio::time::timeout(Duration::from_secs(600), crate::embed::fetch(&self.public_http, &dir, &source)).await.map_err(|_| anyhow!("fetching it took too long"))??;
            Ok(Arc::new(tokio::task::spawn_blocking(move || Embedder::load(&dir)).await??))
        }
        .await;
        match loaded {
            Ok(m) => {
                *self.dialogue.model_failed.lock() = None;
                *held = Some(m.clone());
                Ok(m)
            }
            Err(e) => {
                let why = format!("the model for finding what is meant could not be had: {e:#}");
                tracing::warn!("{why}");
                *self.dialogue.model_failed.lock() = Some((now(), why.clone()));
                Err(anyhow!(why))
            }
        }
    }

    /// Start keeping the index, in the background, for as long as Spool runs.
    pub fn spawn_dialogue(&self) {
        let app = self.clone();
        tokio::spawn(async move {
            // Out of the way of everything else that starts.
            tokio::time::sleep(Duration::from_secs(60)).await;
            loop {
                let pause = match app.dialogue_round(true).await {
                    Ok(true) => Duration::from_secs(2),
                    Ok(false) => Duration::from_secs(15 * 60),
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "the index of what is said could not be kept");
                        Duration::from_secs(15 * 60)
                    }
                };
                app.dialogue.doing("");
                tokio::select! {
                    _ = tokio::time::sleep(pause) => {}
                    _ = app.dialogue.wake.notified() => {}
                }
            }
        });
    }

    /// Do some of the work: read what has changed, place what has not been placed, fetch
    /// subtitles where they are missing (when `fetch`). Says whether there is more to do now.
    pub async fn dialogue_round(&self, fetch: bool) -> Result<bool> {
        let household = crate::subs::language_code(&self.settings.general().language).unwrap_or_default().to_string();
        let mut problems = vec![];
        if household.is_empty() {
            problems.push("no household language is set, so there are no subtitles to read".to_string());
        }
        // Reading whole files is put off while anyone is watching or a clip is being made, so
        // it does not get in the way.
        if !self.playing_now().is_empty() || crate::clips::any_making() {
            self.dialogue.doing(if crate::clips::any_making() { "Waiting while clips are made" } else { "Waiting while something is played" });
            tokio::time::sleep(Duration::from_secs(60)).await;
            return Ok(true);
        }
        let app = self.clone();
        let language = household.clone();
        let wants = tokio::task::spawn_blocking(move || app.dialogue_wants(&language)).await??;
        // What is gone from the library, or no longer has subtitles to read, is forgotten.
        self.dialogue.forget_all_but(&wants.iter().filter(|w| w.source.is_some()).map(|w| w.file_id).collect())?;
        let marks = self.dialogue.marks()?;
        let asked = self.dialogue.asked()?;
        let to_read: Vec<&Want> = wants
            .iter()
            .filter(|w| w.source.is_some() && !w.away)
            // One that could not be read is tried again when it changes, or the next day.
            .filter(|w| marks.get(&w.file_id).is_none_or(|(mark, error, at)| *mark != w.mark || (!error.is_empty() && now() - at > 86400)))
            .collect();
        let g = self.settings.general();
        let can_fetch = !g.opensubtitles_api_key.is_empty() && !household.is_empty();
        let to_fetch: Vec<&Want> = {
            let mut list: Vec<&Want> = wants.iter().filter(|w| w.source.is_none() && !w.away && w.can_take && asked.get(&w.file_id).is_none_or(|(at, found)| now() - at > if *found == Asked::Failed { TRY_AGAIN } else { ASK_AGAIN })).collect();
            // The newest first, as what was just added is likeliest to be looked for.
            list.sort_by_key(|w| std::cmp::Reverse(w.file_id));
            list
        };
        // A pause after failing is lifted when what OpenSubtitles is reached with changes.
        let account = format!("{}\n{}\n{}\n{}", g.opensubtitles_username, g.opensubtitles_password, g.opensubtitles_api_key, g.opensubtitles_url);
        let paused = {
            let mut p = self.dialogue.fetch_paused.lock();
            if p.as_ref().is_some_and(|(until, _, was)| now() >= *until || *was != account) {
                *p = None;
            }
            p.as_ref().map(|(_, why, _)| why.clone())
        };
        if can_fetch {
            problems.extend(paused.clone());
        }
        self.dialogue_tell(&wants, can_fetch, &problems)?;
        let mut more = false;

        // Read what has changed, for a few minutes at most before the rest gets a turn.
        let began = Instant::now();
        if let Some(tool) = self.ffmpeg_path() {
            for want in &to_read {
                if began.elapsed() > Duration::from_secs(300) || !self.playing_now().is_empty() || crate::clips::any_making() {
                    more = true;
                    break;
                }
                let Some(source) = &want.source else { continue };
                self.dialogue.doing(&format!("Reading what is said in {}", self.dialogue_label(want)));
                // A file being read whole is let go of the moment a clip is asked for or
                // something is played, and read again later.
                let stop = Arc::new(AtomicBool::new(false));
                let watch = {
                    let (app, stop) = (self.clone(), stop.clone());
                    tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            if crate::clips::any_making() || !app.playing_now().is_empty() {
                                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                                return;
                            }
                        }
                    })
                };
                // A share that stops answering does not hold everything up.
                let read = tokio::time::timeout(Duration::from_secs(1200), crate::captions::every_line(&tool, &want.path, source, &want.streams, &stop)).await.unwrap_or_else(|_| Err(anyhow!("reading it took too long")));
                watch.abort();
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    more = true;
                    break;
                }
                match read {
                    Ok(cues) => self.dialogue.put(want, &tidy(cues))?,
                    Err(e) => {
                        tracing::info!(file = want.file_id, error = %format!("{e:#}"), "subtitles could not be read for the index");
                        self.dialogue.put_failure(want, &format!("{e:#}"))?;
                    }
                }
            }
        } else if !to_read.is_empty() {
            problems.push("ffmpeg is not installed, so subtitles inside files cannot be read".into());
        }

        // Place what has not been placed by meaning, a few hundred passages at a time.
        let began = Instant::now();
        loop {
            let todo = self.dialogue.unplaced(512)?;
            if todo.is_empty() {
                break;
            }
            if began.elapsed() > Duration::from_secs(120) {
                more = true;
                break;
            }
            let model = match self.dialogue_model().await {
                Ok(m) => m,
                Err(e) => {
                    problems.push(format!("{e:#}"));
                    break;
                }
            };
            let (_, passages, placed) = self.dialogue.counts().unwrap_or_default();
            self.dialogue.doing(&format!("Working out what is meant: {placed} of {passages} passages"));
            let texts: Vec<String> = todo.iter().map(|t| t.2.clone()).collect();
            let points = tokio::task::spawn_blocking(move || model.embed(&texts.iter().map(String::as_str).collect::<Vec<_>>())).await??;
            let placed: Vec<(i64, i64, Vec<f32>)> = todo.into_iter().zip(points).map(|((id, file, _), p)| (id, file, p)).collect();
            self.dialogue.place(&placed)?;
        }

        // Fetch subtitles for what has none to read.
        if fetch && can_fetch && paused.is_none() && !to_fetch.is_empty() {
            let spent = (*self.subtitle_allowance.lock()).filter(|(left, resets)| *left <= KEEP_FOR_HAND && now() < *resets);
            if let Some((_, resets)) = spent {
                problems.push(format!("today's subtitles from OpenSubtitles are spent; more can be fetched from {}", chrono::DateTime::from_timestamp(resets, 0).map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default()));
            } else {
                let began = Instant::now();
                let mut failed = vec![];
                for want in to_fetch {
                    if began.elapsed() > Duration::from_secs(600) || !self.playing_now().is_empty() || crate::clips::any_making() {
                        more = true;
                        break;
                    }
                    if (*self.subtitle_allowance.lock()).is_some_and(|(left, resets)| left <= KEEP_FOR_HAND && now() < resets) {
                        break;
                    }
                    self.dialogue.doing(&format!("Fetching subtitles for {}", self.dialogue_label(want)));
                    // Some may have been put beside it since it was last looked at.
                    let (app, id) = (self.clone(), want.file_id);
                    let beside = tokio::task::spawn_blocking(move || app.find_sidecars(id)).await?.unwrap_or_default();
                    if beside.iter().any(|s| !s.forced && s.language == household) {
                        more = true;
                        continue;
                    }
                    match self.auto_subtitle(want.file_id, &household, KEEP_FOR_HAND).await {
                        Ok(found) => {
                            failed.clear();
                            self.dialogue.note_asked(want.file_id, if found.is_some() { Asked::Found } else { Asked::None })?;
                            more |= found.is_some();
                        }
                        Err(e) => {
                            tracing::info!(file = want.file_id, error = %format!("{e:#}"), "subtitles could not be fetched for the index");
                            self.dialogue.note_asked(want.file_id, Asked::Failed)?;
                            // A file that cannot be read, or whose soundtrack is slow to reach, is
                            // that file's own trouble and holds up none of the rest.
                            if !e.chain().any(|c| c.is::<crate::subs::AtOpenSubtitles>()) {
                                continue;
                            }
                            failed.push(want.file_id);
                            // Several in a row from OpenSubtitles is something in the way of every
                            // file alike, such as signing in: looked at again in a while, or when the account
                            // changes, with those files asked about again then.
                            if failed.len() >= 3 {
                                self.dialogue.unask(&failed)?;
                                let why = format!("subtitles could not be fetched from OpenSubtitles: {e:#}");
                                tracing::warn!("{why}");
                                *self.dialogue.fetch_paused.lock() = Some((now() + 3600, why.clone(), account.clone()));
                                problems.push(why);
                                break;
                            }
                        }
                    }
                }
            }
        }
        self.dialogue_tell(&wants, can_fetch, &problems)?;
        Ok(more)
    }

    /// Count what the index covers, for anyone who asks.
    fn dialogue_tell(&self, wants: &[Want], can_fetch: bool, problems: &[String]) -> Result<()> {
        let marks = self.dialogue.marks()?;
        let asked = self.dialogue.asked()?;
        let (lines, passages, placed) = self.dialogue.counts()?;
        let unreadable: Vec<&Want> = wants.iter().filter(|w| w.source.is_none()).collect();
        *self.dialogue.coverage.lock() = Coverage {
            files: wants.len(),
            read: wants.iter().filter(|w| marks.get(&w.file_id).is_some_and(|(mark, error, _)| *mark == w.mark && error.is_empty())).count(),
            lines,
            placed,
            passages,
            unreadable: unreadable.len(),
            not_found: unreadable.iter().filter(|w| asked.get(&w.file_id).is_some_and(|(_, found)| *found == Asked::None)).count(),
            away: wants.iter().filter(|w| w.away).count(),
            can_fetch,
            fetches_left: None,
            doing: String::new(),
            problems: problems.to_vec(),
        };
        Ok(())
    }

    /// What the index covers, with how many subtitles may still be fetched today.
    pub fn dialogue_coverage(&self) -> Coverage {
        let mut c = self.dialogue.coverage();
        c.fetches_left = (*self.subtitle_allowance.lock()).filter(|(_, resets)| now() < *resets).map(|(left, _)| left);
        c
    }

    fn dialogue_label(&self, want: &Want) -> String {
        let Ok(Some(title)) = self.db.title(want.title_id) else { return "a file".into() };
        let episode = want.episode_id.and_then(|e| self.db.episode(e).ok().flatten());
        crate::clips::cut_from(&title, episode.as_ref())
    }

    /// Find moments by what is said in them: the words, or what they mean. Several things can
    /// be looked for at once, put apart by ";" or "|", or on lines of their own.
    pub async fn search_dialogue(&self, query: &str, limit: usize, title_id: Option<i64>) -> Result<Found> {
        let asks: Vec<String> = query.split([';', '|', '\n']).map(str::trim).filter(|q| !q.is_empty()).map(str::to_string).collect();
        if asks.is_empty() {
            return Err(anyhow!("say what to look for"));
        }
        let limit = limit.clamp(1, 200);
        let only: Option<HashSet<i64>> = match title_id {
            Some(t) => Some(self.dialogue.files_of_title(t)?),
            None => None,
        };
        // Searching by meaning needs the model; while it is still being fetched, the words do.
        // It is fetched apart from the search, so a search that gives up does not stop it.
        let fetching = {
            let app = self.clone();
            tokio::spawn(async move { app.dialogue_model().await })
        };
        let model = match tokio::time::timeout(Duration::from_secs(20), fetching).await {
            Ok(Ok(Ok(m))) => Some(m),
            _ => None,
        };
        let mut scores: HashMap<i64, (f32, bool)> = HashMap::new();
        let by_meaning = model.is_some();
        for ask in &asks {
            let with_words = self.dialogue.with_words(ask, only.as_ref())?;
            let mut candidates: HashMap<i64, f32> = HashMap::new();
            if let Some(model) = &model {
                let (m, text) = (model.clone(), ask.clone());
                let wanted = tokio::task::spawn_blocking(move || m.embed(&[text.as_str()])).await??.pop().unwrap_or_default();
                let signs = {
                    let index = self.clone();
                    tokio::task::spawn_blocking(move || index.dialogue.signs()).await??
                };
                // The few thousand nearest by sign, then each looked at properly.
                let near = nearest(&signs, &signs_of(&wanted), 4000, only.as_ref());
                let look: Vec<i64> = near.into_iter().chain(with_words.keys().copied()).collect();
                for (id, point) in self.dialogue.points(&look)? {
                    candidates.insert(id, alike(&wanted, &point));
                }
            }
            for (id, exact) in &with_words {
                let meaning = candidates.get(id).copied().unwrap_or(0.0);
                // Words as written count for more than words found apart.
                let bonus = if *exact { 0.3 } else { 0.12 };
                candidates.insert(*id, meaning.max(if by_meaning { 0.0 } else { 0.5 }) + bonus);
            }
            for (id, score) in candidates {
                let words = with_words.contains_key(&id);
                if !words && score < ALIKE {
                    continue;
                }
                let best = scores.entry(id).or_insert((score, words));
                if score > best.0 {
                    *best = (score, words || best.1);
                }
            }
        }
        let mut ranked: Vec<(i64, f32, bool)> = scores.into_iter().map(|(id, (s, w))| (id, s, w)).collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut moments: Vec<Moment> = vec![];
        let mut titles: HashMap<i64, Option<Title>> = HashMap::new();
        for (id, score, words) in ranked {
            if moments.len() >= limit {
                break;
            }
            let Some(Passage { file: file_id, before, lines, after, title: title_id, episode: episode_id }) = self.dialogue.passage(id)? else { continue };
            let (Some(first), Some(last)) = (lines.first(), lines.last()) else { continue };
            let (start, end) = (first.start, last.end);
            // One moment for each stretch of a file: a better one close by stands for it.
            if moments.iter().any(|m| m.file_id == file_id && m.start - 20.0 < end && start < m.end + 20.0) {
                continue;
            }
            let title = titles.entry(title_id).or_insert_with(|| self.db.title(title_id).ok().flatten());
            let Some(title) = title.as_ref() else { continue };
            let episode = episode_id.and_then(|e| self.db.episode(e).ok().flatten());
            moments.push(Moment { title_id, episode_id, from: crate::clips::cut_from(title, episode.as_ref()), file_id, start, end, lines, before, after, score: (score * 100.0).round() / 100.0, words });
        }
        Ok(Found { moments, by_meaning, coverage: self.dialogue_coverage() })
    }
}

/// The passages whose signs differ least from the ones given.
fn nearest(signs: &Signs, wanted: &[u64; DIMENSIONS / 64], n: usize, only: Option<&HashSet<i64>>) -> Vec<i64> {
    let mut apart: Vec<(u32, usize)> = signs
        .bits
        .iter()
        .enumerate()
        .filter(|(i, _)| only.is_none_or(|o| o.contains(&signs.files[*i])))
        .map(|(i, b)| (b.iter().zip(wanted).map(|(x, y)| (x ^ y).count_ones()).sum::<u32>(), i))
        .collect();
    if apart.len() > n {
        apart.select_nth_unstable(n);
        apart.truncate(n);
    }
    apart.into_iter().map(|(_, i)| signs.ids[i]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start: f64, end: f64, text: &str) -> Cue {
        Cue { start, end, text: text.into() }
    }

    #[test]
    fn lines_are_kept_once_in_order_and_put_together_as_they_are_said() {
        let cues = tidy(vec![cue(5.0, 6.0, "Wake up, Neo."), cue(1.0, 2.0, "♪ ♪"), cue(5.0, 6.0, "Wake up, Neo."), cue(6.0, 7.0, "Wake up, Neo."), cue(6.5, 8.0, "The Matrix has you.")]);
        assert_eq!(cues.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["Wake up, Neo.", "The Matrix has you."]);
        assert_eq!(cues[0].end, 7.0);
        let said = [cue(0.0, 1.0, "a"), cue(1.5, 2.0, "b"), cue(2.5, 3.0, "c"), cue(3.5, 4.0, "d"), cue(20.0, 21.0, "e")];
        assert_eq!(passages(&said), [(0, 2), (3, 3), (4, 4)]);
    }

    #[test]
    fn a_point_is_kept_small_and_comes_back_near_enough_the_same() {
        let point: Vec<f32> = (0..DIMENSIONS).map(|i| ((i as f32) * 0.37).sin() * 0.1).collect();
        let back = unpack(&pack(&point)).unwrap();
        assert!(point.iter().zip(&back).all(|(a, b)| (a - b).abs() < 0.001));
        assert_eq!(signs_of(&point), signs_of(&back));
        let signs = Signs { ids: vec![10, 11, 12], files: vec![1, 1, 2], bits: vec![[0; 6], [u64::MAX; 6], [1; 6]] };
        assert_eq!(nearest(&signs, &[0; 6], 2, None), [10, 12]);
        assert_eq!(nearest(&signs, &[0; 6], 5, Some(&HashSet::from([1]))).len(), 2);
    }
}
