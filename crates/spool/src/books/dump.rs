//! The book index, built from the metadata Anna's Archive publishes. Its search pages sit
//! behind a check for a browser, so books are not looked up there: every month or two it
//! shares a snapshot of its own search index as a torrent, and Spool reads the part of it
//! that describes files (about 170 GB across a dozen compressed files) one file at a time,
//! keeping only the EPUBs, and the PDFs of books, in the languages read here, and deleting
//! each file once read. A PDF is taken only when a book has no EPUB: Florilegium makes one
//! of it.

use super::index::{Builder, Record};
use super::text::isbn13;
use crate::app::App;
use anyhow::{bail, Context, Result};
use axum::body::Bytes;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Anna's Archive's own addresses, as its FAQ gives them. Others that look like it are not
/// to be trusted.
pub const MIRRORS: &[&str] = &["https://annas-archive.gl", "https://annas-archive.pk", "https://annas-archive.gd"];

/// The collection of its metadata that holds its search index.
const GROUP: &str = "aa_derived_mirror_metadata";

/// How the index came to be, or is coming to be.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexState {
    /// The snapshot the index in use was built from, and when.
    pub snapshot: String,
    pub built_at: i64,
    /// While building: which file of how many, and what has been read so far.
    pub building: bool,
    pub snapshot_next: String,
    pub part: u32,
    pub parts: u32,
    pub read_bytes: u64,
    pub total_bytes: u64,
    pub records: u64,
    pub kept: u64,
    pub started_at: i64,
    /// "downloading" or "reading" the part it is on.
    pub phase: String,
    /// Why the last build stopped short, if it did.
    pub error: String,
}

#[derive(Deserialize)]
struct Row {
    #[serde(default)]
    group_name: String,
    #[serde(default)]
    btih: String,
    #[serde(default)]
    magnet_link: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    added_to_torrents_list_at: String,
    #[serde(default)]
    obsolete: bool,
}

/// Where Anna's Archive keeps its torrents, by their path in its list: what follows this in
/// a torrent's address.
pub const TORRENTS_PATH: &str = "/dyn/small_file/torrents/";

/// The newest snapshot: its name and its torrent file. Anna's Archive's list of torrents is
/// kept beside the books, as each torrent's path and how to find it without Anna's Archive
/// (its infohash, and a magnet link with a tracker), for [`super::fallback`].
pub async fn latest(http: &reqwest::Client, keep_list: &std::path::Path) -> Result<(String, Bytes)> {
    let mut last = anyhow::anyhow!("no address of Anna's Archive answered");
    for base in MIRRORS {
        let rows: Vec<Row> = match http.get(format!("{base}/dyn/torrents.json")).timeout(std::time::Duration::from_secs(120)).send().await.and_then(|r| r.error_for_status()) {
            Ok(r) => match r.json().await {
                Ok(rows) => rows,
                Err(e) => {
                    last = anyhow::anyhow!("{base} sent a list of torrents that could not be read: {e}");
                    continue;
                }
            },
            Err(e) => {
                last = anyhow::anyhow!("{base}: {e}");
                continue;
            }
        };
        let list: std::collections::HashMap<&str, super::fallback::Listed> = rows
            .iter()
            .filter(|r| !r.btih.is_empty())
            .filter_map(|r| Some((r.url.split_once(TORRENTS_PATH)?.1, super::fallback::Listed { btih: r.btih.clone(), magnet: r.magnet_link.clone() })))
            .collect();
        if !list.is_empty() {
            if let Err(e) = super::fallback::save_list(keep_list, &list) {
                tracing::warn!(error = %e, "the list of Anna's Archive's torrents could not be kept");
            }
        }
        let Some(row) = rows.into_iter().filter(|r| r.group_name == GROUP && !r.obsolete && !r.url.is_empty()).max_by(|a, b| a.added_to_torrents_list_at.cmp(&b.added_to_torrents_list_at)) else {
            bail!("Anna's Archive lists no snapshot of its metadata");
        };
        let url = if row.url.starts_with("http") { row.url.clone() } else { format!("{base}{}", row.url) };
        let torrent = http.get(&url).timeout(std::time::Duration::from_secs(120)).send().await?.error_for_status()?.bytes().await?;
        let name = row.display_name.trim_end_matches(".torrent").to_string();
        return Ok((name, torrent));
    }
    Err(last)
}

/// A little of bencoding, enough to read the files a torrent holds.
enum Ben<'a> {
    Int,
    Bytes(&'a [u8]),
    List(Vec<Ben<'a>>),
    Dict(Vec<(&'a [u8], Ben<'a>)>),
}

fn ben<'a>(b: &'a [u8], i: &mut usize) -> Result<Ben<'a>> {
    let c = *b.get(*i).context("the torrent file ends early")?;
    match c {
        b'i' => {
            let end = *i + b[*i..].iter().position(|&x| x == b'e').context("bad number")?;
            *i = end + 1;
            Ok(Ben::Int)
        }
        b'l' => {
            *i += 1;
            let mut out = vec![];
            while b.get(*i) != Some(&b'e') {
                out.push(ben(b, i)?);
            }
            *i += 1;
            Ok(Ben::List(out))
        }
        b'd' => {
            *i += 1;
            let mut out = vec![];
            while b.get(*i) != Some(&b'e') {
                let Ben::Bytes(k) = ben(b, i)? else { bail!("a key that is not text") };
                out.push((k, ben(b, i)?));
            }
            *i += 1;
            Ok(Ben::Dict(out))
        }
        b'0'..=b'9' => {
            let colon = *i + b[*i..].iter().position(|&x| x == b':').context("bad length")?;
            let n: usize = std::str::from_utf8(&b[*i..colon])?.parse()?;
            let start = colon + 1;
            *i = start + n;
            Ok(Ben::Bytes(b.get(start..start + n).context("the torrent file ends early")?))
        }
        _ => bail!("the torrent file could not be read"),
    }
}

fn get<'a, 'b>(d: &'b Ben<'a>, key: &str) -> Option<&'b Ben<'a>> {
    match d {
        Ben::Dict(kv) => kv.iter().find(|(k, _)| *k == key.as_bytes()).map(|(_, v)| v),
        _ => None,
    }
}

/// Each file in a torrent: its number, its path and its length.
pub fn files(torrent: &[u8]) -> Result<Vec<(usize, String, u64)>> {
    let mut i = 0;
    let top = ben(torrent, &mut i)?;
    let info = get(&top, "info").context("the torrent has no info")?;
    let Some(Ben::List(list)) = get(info, "files") else { bail!("the torrent holds one file, not a folder") };
    // Lengths are read from the raw text, which the little reader above passes over.
    let mut out = vec![];
    for (n, f) in list.iter().enumerate() {
        let Some(Ben::List(parts)) = get(f, "path") else { continue };
        let path: Vec<String> = parts.iter().filter_map(|p| if let Ben::Bytes(b) = p { Some(String::from_utf8_lossy(b).to_string()) } else { None }).collect();
        out.push((n, path.join("/"), 0));
    }
    // Lengths in order of appearance.
    let mut lengths = vec![];
    let mut at = 0;
    while let Some(p) = memchr::memmem::find(&torrent[at..], b"6:lengthi") {
        let start = at + p + 9;
        let end = start + torrent[start..].iter().position(|&x| x == b'e').context("bad length")?;
        lengths.push(std::str::from_utf8(&torrent[start..end])?.parse::<u64>()?);
        at = end;
    }
    if lengths.len() == out.len() {
        for (f, l) in out.iter_mut().zip(lengths) {
            f.2 = l;
        }
    }
    Ok(out)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Line {
    #[serde(rename = "_source")]
    source: Source,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Source {
    file_unified_data: Unified,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Unified {
    title_best: Option<String>,
    author_best: Option<String>,
    extension_best: Option<String>,
    content_type_best: Option<String>,
    filesize_best: Option<serde_json::Value>,
    year_best: Option<serde_json::Value>,
    most_likely_language_codes: Vec<String>,
    language_codes: Vec<String>,
    identifiers_unified: Ids,
    classifications_unified: Classes,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Classes {
    /// The torrents of Anna's Archive that hold the file, by their path in its list.
    torrent: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Ids {
    md5: Vec<String>,
    isbn13: Vec<String>,
    ol: Vec<String>,
    ipfs_cid: Vec<String>,
    /// What the file is named in a torrent of Z-Library's, or of Anna's own collections.
    zlib: Vec<String>,
    aacid: Vec<String>,
}

fn number(v: &Option<serde_json::Value>) -> u64 {
    match v {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(serde_json::Value::String(s)) => s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0),
        _ => 0,
    }
}

/// What is not a book, though it may be a PDF: papers, magazines, standards, scores, comics.
const NOT_BOOKS: &[&str] = &["journal_article", "magazine", "standards_document", "musical_score", "book_comic"];

/// The EPUBs and the PDFs of books in one of the snapshot's files, read line by line: one
/// JSON document a line. Gives how many lines were read.
pub fn read_records(input: impl Read, languages: &[String], mut each: impl FnMut(Record) -> Result<()>) -> Result<u64> {
    let mut r = std::io::BufReader::with_capacity(1 << 20, input);
    let mut line = Vec::with_capacity(1 << 16);
    let mut n = 0;
    let epub = memchr::memmem::Finder::new(b"\"epub\"");
    let pdf = memchr::memmem::Finder::new(b"\"pdf\"");
    loop {
        line.clear();
        if r.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        n += 1;
        // Most files are neither; those are passed over before being read.
        if epub.find(&line).is_none() && pdf.find(&line).is_none() {
            continue;
        }
        let parsed: Line = match serde_json::from_slice(&line) {
            Ok(l) => l,
            Err(_) => match serde_json::from_str(&String::from_utf8_lossy(&line)) {
                Ok(l) => l,
                Err(_) => continue,
            },
        };
        let u = parsed.source.file_unified_data;
        let format = match u.extension_best.as_deref() {
            Some("epub") => "epub",
            Some("pdf") if !u.content_type_best.as_deref().is_some_and(|t| NOT_BOOKS.contains(&t)) => "pdf",
            _ => continue,
        };
        let Some(md5) = u.identifiers_unified.md5.first().filter(|m| m.len() == 32).cloned() else { continue };
        let lang = u.most_likely_language_codes.first().or(u.language_codes.first()).cloned().unwrap_or_default();
        if !lang.is_empty() && !languages.is_empty() && !languages.contains(&lang) {
            continue;
        }
        let title = u.title_best.unwrap_or_default();
        if title.trim().is_empty() {
            continue;
        }
        let mut isbns: Vec<String> = u.identifiers_unified.isbn13.iter().filter_map(|i| isbn13(i)).collect();
        isbns.sort();
        isbns.dedup();
        let ids = u.identifiers_unified;
        let names = ids.zlib.into_iter().chain(ids.aacid).collect();
        each(Record { md5, title, author: u.author_best.unwrap_or_default(), lang, size: number(&u.filesize_best), year: number(&u.year_best), isbns, ols: ids.ol, format: format.into(), torrents: u.classifications_unified.torrent, ipfs: ids.ipfs_cid, names })?;
    }
    Ok(n)
}

/// A part on the disk, read through: counts what has been read, and stops when Spool does.
struct Counting<R> {
    inner: R,
    n: Arc<AtomicU64>,
    stopping: Arc<std::sync::atomic::AtomicBool>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.stopping.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("Spool is stopping"));
        }
        let got = self.inner.read(buf)?;
        self.n.fetch_add(got as u64, Ordering::Relaxed);
        Ok(got)
    }
}

/// How long the torrent may give nothing before the build gives up.
const STALL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

impl App {
    fn index_state(&self, f: impl FnOnce(&mut IndexState)) {
        let mut s = self.books.index_state.lock();
        f(&mut s);
        let _ = self.db.set_setting("book_index", &*s);
    }

    /// The "book_index" task: build the index again when Anna's Archive has published a
    /// newer snapshot than the one it was built from.
    pub async fn book_index_task(&self, force: bool) -> Result<String> {
        let g = self.settings.general();
        if g.books.annas_key.is_empty() && !force {
            return Ok("skipped: no Anna's Archive key".into());
        }
        // The first build reads 170 GB, so it is begun by someone, from the Books page; after
        // that the index is kept up to date by itself.
        if self.books.shelf.books().is_none() && !force {
            return Ok("skipped: the book index is first built from the Books page".into());
        }
        let (name, torrent) = latest(&self.http, &self.books.dir.join("torrents.json")).await?;
        let state = self.books.index_state.lock().clone();
        if !force && state.snapshot == name && self.books.shelf.books().is_some() {
            return Ok(format!("up to date with {name}"));
        }
        match self.build_book_index(&name, torrent).await {
            Ok(m) => {
                // Books wanted while there was no index, or not in the old one, are looked for
                // straight away rather than at the next books pass.
                Box::pin(self.run_task("books")).await;
                Ok(m)
            }
            Err(e) => {
                let why = format!("{e:#}");
                self.index_state(|s| {
                    s.building = false;
                    s.error = why.clone();
                });
                Err(e)
            }
        }
    }

    async fn build_book_index(&self, name: &str, torrent: Bytes) -> Result<String> {
        let g = self.settings.general();
        let languages = g.books.languages();
        let parts: Vec<(usize, String, u64)> = files(&torrent)?.into_iter().filter(|(_, p, len)| p.starts_with("elasticsearch/aarecords__") && p.ends_with(".json.gz") && *len > 1_000_000).collect();
        if parts.is_empty() {
            bail!("the snapshot {name} holds no search index");
        }
        let total: u64 = parts.iter().map(|p| p.2).sum();
        let largest = parts.iter().map(|p| p.2).max().unwrap_or(0);
        // Each file is downloaded whole before it is deleted, so the largest must fit, with
        // room to spare for what else the disk is doing.
        let work = if g.downloads_dir.is_empty() { self.data_dir.join("books").join("snapshot") } else { std::path::Path::new(&g.downloads_dir).join(".spool-book-index") };
        // What an earlier build cut short left is not carried on from: it goes before the
        // room is counted.
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        let free = crate::app::free_space(&work).unwrap_or(0);
        let need = largest + g.min_free_gb as u64 * 1_000_000_000;
        if free < need {
            bail!("building the book index needs {:.0} GB free beside the downloads, and there are {:.0}", need as f64 / 1e9, free as f64 / 1e9);
        }
        self.index_state(|s| {
            *s = IndexState { snapshot: s.snapshot.clone(), built_at: s.built_at, building: true, snapshot_next: name.into(), parts: parts.len() as u32, total_bytes: total, started_at: crate::db::now(), ..Default::default() };
        });
        tracing::info!(snapshot = name, parts = parts.len(), gb = total / 1_000_000_000, "building the book index");

        let session = librqbit::Session::new_with_opts(work.clone(), librqbit::SessionOptions { disable_local_service_discovery: true, ..Default::default() }).await?;
        let builder = Builder::new(&self.books.shelf.next_dir())?;
        let mut done_bytes = 0u64;
        let result: Result<Builder> = async {
            let mut builder = builder;
            for (n, (idx, path, len)) in parts.iter().enumerate() {
                self.index_state(|s| s.part = n as u32 + 1);
                let opts = librqbit::AddTorrentOptions { only_files: Some(vec![*idx]), overwrite: true, output_folder: Some(work.to_string_lossy().to_string()), ..Default::default() };
                let handle = session.add_torrent(librqbit::AddTorrent::TorrentFileBytes(torrent.clone()), Some(opts)).await?.into_handle().context("the torrent was not taken")?;
                // The part is downloaded whole, then read from the disk: reading it as it
                // arrives goes at a fraction of the speed.
                self.index_state(|s| s.phase = "downloading".into());
                let base = done_bytes;
                let mut last = 0u64;
                let mut since = std::time::Instant::now();
                loop {
                    if self.books.stopping.load(Ordering::SeqCst) {
                        bail!("Spool is stopping");
                    }
                    let st = handle.stats();
                    if let Some(e) = st.error {
                        bail!("the torrent failed: {e}");
                    }
                    if st.finished {
                        break;
                    }
                    // What has come of this part: the torrent's own count takes in pieces it
                    // shares with files not asked for.
                    let have = st.file_progress.get(*idx).copied().unwrap_or(0);
                    if have > last {
                        last = have;
                        since = std::time::Instant::now();
                        // Half the bar for downloading a part, half for reading it.
                        self.books.index_state.lock().read_bytes = base + have / 2;
                    } else if since.elapsed() > STALL {
                        bail!("the torrent gave nothing for {} minutes", STALL.as_secs() / 60);
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                        _ = self.books.stopped.notified() => bail!("Spool is stopping"),
                    }
                }
                self.index_state(|s| s.phase = "reading".into());
                let file = work.join(path);
                let read = Arc::new(AtomicU64::new(0));
                let counted = read.clone();
                let stopping = self.books.stopping.clone();
                let langs = languages.clone();
                let app = self.clone();
                let half = len / 2;
                let job = tokio::task::spawn_blocking(move || -> Result<(Builder, u64)> {
                    let raw = Counting { inner: std::fs::File::open(&file).with_context(|| format!("{} was not downloaded", file.display()))?, n: counted, stopping };
                    let gz = flate2::read::MultiGzDecoder::new(std::io::BufReader::with_capacity(1 << 20, raw));
                    let mut last = std::time::Instant::now();
                    let lines = read_records(gz, &langs, |r| {
                        builder.add(&r)?;
                        if last.elapsed().as_secs() >= 5 {
                            last = std::time::Instant::now();
                            let mut s = app.books.index_state.lock();
                            s.kept = builder.count;
                            s.read_bytes = base + half + read.load(Ordering::Relaxed) / 2;
                        }
                        Ok(())
                    })?;
                    Ok((builder, lines))
                });
                let (b, lines) = job.await??;
                builder = b;
                done_bytes += len;
                self.index_state(|s| {
                    s.records += lines;
                    s.kept = builder.count;
                    s.read_bytes = done_bytes;
                });
                tracing::info!(file = path.as_str(), lines, kept = builder.count, "read a part of the book metadata");
                session.delete(librqbit::api::TorrentIdOrHash::Id(handle.id()), true).await?;
            }
            Ok(builder)
        }
        .await;
        session.stop().await;
        let _ = std::fs::remove_dir_all(&work);
        let builder = result?;
        let kept = builder.finish()?;
        self.books.shelf.replace()?;
        self.index_state(|s| {
            s.snapshot = name.into();
            s.built_at = crate::db::now();
            s.building = false;
            s.error.clear();
        });
        Ok(format!("built from {name}: {kept} books"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epubs_and_book_pdfs_in_the_languages_read_are_kept() {
        let lines = [
            r#"{"_id":"md5:1","_source":{"file_unified_data":{"title_best":"The Left Hand of Darkness","author_best":"Le Guin, Ursula K.","extension_best":"epub","filesize_best":523000,"year_best":"1969","most_likely_language_codes":["en"],"identifiers_unified":{"md5":["0123456789abcdef0123456789abcdef"],"isbn13":["9780441478125"],"ol":["OL24215271M"],"ipfs_cid":["bafk1"],"zlib":["1234"]},"classifications_unified":{"torrent":["external/libgen_rs_fic/f_1000.torrent"]}}}}"#,
            r#"{"_id":"md5:2","_source":{"file_unified_data":{"title_best":"A paper","extension_best":"pdf","content_type_best":"journal_article","identifiers_unified":{"md5":["11111111111111111111111111111111"]}}}}"#,
            r#"{"_id":"md5:3","_source":{"file_unified_data":{"title_best":"Le Petit Prince","extension_best":"epub","most_likely_language_codes":["fr"],"identifiers_unified":{"md5":["22222222222222222222222222222222"]}}}}"#,
            r#"{"_id":"md5:4","_source":{"file_unified_data":{"title_best":"Arktos","author_best":"Godwin, Joscelyn","extension_best":"pdf","content_type_best":"book_nonfiction","most_likely_language_codes":["en"],"identifiers_unified":{"md5":["33333333333333333333333333333333"]}}}}"#,
            r#"{"_id":"md5:5","_source":{"file_unified_data":{"title_best":"A scan","extension_best":"djvu","identifiers_unified":{"md5":["44444444444444444444444444444444"]}}}}"#,
            "not json at all \"epub\"",
        ]
        .join("\n");
        let mut got = vec![];
        let n = read_records(lines.as_bytes(), &["en".to_string()], |r| {
            got.push(r);
            Ok(())
        })
        .unwrap();
        assert_eq!(n, 6);
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].format.as_str(), got[1].format.as_str(), got[1].title.as_str()), ("epub", "pdf", "Arktos"));
        assert_eq!(got[0].isbns, vec!["9780441478125"]);
        assert_eq!(got[0].size, 523000);
        assert_eq!(got[0].year, 1969);
        assert_eq!((got[0].torrents.as_slice(), got[0].ipfs.as_slice(), got[0].names.as_slice()), (&["external/libgen_rs_fic/f_1000.torrent".to_string()][..], &["bafk1".to_string()][..], &["1234".to_string()][..]));
    }

    #[test]
    fn a_torrent_lists_its_files() {
        let t = b"d4:infod5:filesld6:lengthi5e4:pathl13:elasticsearch6:a.jsoneed6:lengthi7e4:pathl1:beee4:name1:x12:piece lengthi16eee";
        let f = files(t).unwrap();
        assert_eq!(f, vec![(0, "elasticsearch/a.json".to_string(), 5), (1, "b".to_string(), 7)]);
    }
}
