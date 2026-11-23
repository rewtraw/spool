//! Getting a book when Anna's Archive cannot give it: its API is down, the day's downloads are
//! used up, there is no key, or its servers have no copy. The same file is had from where
//! the archive's own metadata says it also is (see [`super::index::Sources`]):
//!
//! 1. an IPFS gateway, by the file's IPFS address, when it has one;
//! 2. one of Anna's Archive's torrents, which hold most of its books as loose files named by
//!    their MD5 (Library Genesis), their Z-Library number or their AACID (its own
//!    collections). Only that one file is downloaded, out of the torrent's tens or hundreds
//!    of gigabytes. The torrent is found by its infohash, from the list of torrents kept at
//!    each look for a new snapshot, so Anna's Archive need not answer: its `.torrent` is
//!    fetched from the archive when it can be, and otherwise from the torrent's own peers,
//!    and kept for next time.
//!
//! Whatever comes is checked against the MD5 asked for. A file packed into a larger archive
//! inside its torrent (some of the newer collections are) is not fetched: that would mean
//! downloading the whole archive for one book.

use super::annas::{download, file_md5, Failed};
use super::dump::{files, MIRRORS, TORRENTS_PATH};
use super::index::Sources;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How a torrent of Anna's Archive is found without it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Listed {
    pub btih: String,
    pub magnet: String,
}

pub fn save_list(path: &Path, list: &HashMap<&str, Listed>) -> Result<()> {
    let part = path.with_extension("json.part");
    std::fs::write(&part, serde_json::to_vec(list)?)?;
    std::fs::rename(&part, path)?;
    Ok(())
}

fn load_list(path: &Path) -> HashMap<String, Listed> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Gateways to IPFS that serve a file by its address.
const GATEWAYS: &[&str] = &["https://ipfs.io/ipfs/", "https://dweb.link/ipfs/", "https://4everland.io/ipfs/"];

/// How long a torrent may give nothing before it is given up on, and how long it may take
/// at most: a book is small, and another way may be quicker.
const STALL: Duration = Duration::from_secs(5 * 60);
const LONGEST: Duration = Duration::from_secs(45 * 60);

/// Where the fallback works: torrents' `.torrent` files kept, and files being fetched.
pub struct Place<'a> {
    pub books_dir: &'a Path,
    pub stopping: &'a std::sync::atomic::AtomicBool,
}

/// Fetch a file into `dest` by another way than Anna's Archive's servers, and check it.
/// Gives how it came, or why none of the ways gave it.
pub async fn fetch(http: &reqwest::Client, s: &Sources, dest: &Path, most: u64, place: &Place<'_>) -> Result<String> {
    let mut why: Vec<String> = vec![];
    for cid in &s.ipfs {
        for gateway in GATEWAYS {
            match by_url(http, &format!("{gateway}{cid}"), &s.md5, dest, most).await {
                Ok(()) => return Ok(format!("from IPFS, through {}", gateway.trim_start_matches("https://").trim_end_matches("/ipfs/"))),
                Err(e) => why.push(format!("IPFS through {gateway}: {e}")),
            }
        }
    }
    let list = load_list(&place.books_dir.join("torrents.json"));
    for path in &s.torrents {
        let Some(listed) = list.get(path) else {
            why.push(format!("{path}: not in the list of Anna's Archive's torrents kept here"));
            continue;
        };
        match by_torrent(http, path, listed, s, dest, most, place).await {
            Ok(()) => return Ok(format!("from Anna's Archive's torrent {}", path.rsplit('/').next().unwrap_or(path))),
            Err(e) => why.push(format!("{path}: {e:#}")),
        }
    }
    if why.is_empty() {
        bail!("Anna's Archive knows of nowhere else it is");
    }
    bail!("{}", why.join("; "))
}

async fn by_url(http: &reqwest::Client, url: &str, md5: &str, dest: &Path, most: u64) -> Result<()> {
    let part = dest.with_extension("part");
    let _ = tokio::fs::remove_file(&part).await;
    match download(http, url, &part, most).await {
        Ok(()) => {}
        Err(Failed::TooLarge) => bail!("larger than a book should be"),
        Err(Failed::Now(e)) => bail!("{e}"),
    }
    settle(&part, md5, dest).await
}

/// A file come whole: kept as `dest` when it is the one asked for.
async fn settle(got: &Path, md5: &str, dest: &Path) -> Result<()> {
    if file_md5(got).await? != md5 {
        let _ = tokio::fs::remove_file(got).await;
        bail!("what came was not the file asked for (its MD5 differs)");
    }
    tokio::fs::rename(got, dest).await?;
    Ok(())
}

/// The `.torrent` of one of Anna's Archive's torrents: kept from before, from the archive,
/// or from the torrent's peers.
async fn torrent_file(http: &reqwest::Client, session: &std::sync::Arc<librqbit::Session>, path: &str, listed: &Listed, kept: &Path) -> Result<axum::body::Bytes> {
    if let Ok(b) = tokio::fs::read(kept).await {
        return Ok(b.into());
    }
    let mut got = None;
    for base in MIRRORS {
        let resp = http.get(format!("{base}{TORRENTS_PATH}{path}")).timeout(Duration::from_secs(30)).send().await.and_then(|r| r.error_for_status());
        if let Ok(b) = match resp {
            Ok(r) => r.bytes().await,
            Err(e) => Err(e),
        } {
            got = Some(b);
            break;
        }
    }
    let bytes = match got {
        Some(b) => b,
        None => {
            let link = if listed.magnet.is_empty() { format!("magnet:?xt=urn:btih:{}", listed.btih) } else { listed.magnet.clone() };
            let opts = librqbit::AddTorrentOptions { list_only: true, ..Default::default() };
            let asked = session.add_torrent(librqbit::AddTorrent::from_url(link), Some(opts));
            match tokio::time::timeout(Duration::from_secs(180), asked).await {
                Err(_) => bail!("its peers gave no list of its files in three minutes"),
                Ok(r) => match r? {
                    librqbit::AddTorrentResponse::ListOnly(l) => l.torrent_bytes,
                    _ => bail!("the torrent was added when only its list of files was asked for"),
                },
            }
        }
    };
    if let Some(dir) = kept.parent() {
        let _ = tokio::fs::create_dir_all(dir).await;
    }
    let _ = tokio::fs::write(kept, &bytes).await;
    Ok(bytes)
}

/// Which of a torrent's files is the book: one named by its MD5 (with or without its
/// extension), its Z-Library number or its AACID, or failing those the one file of exactly its
/// size. An archive of many files is not it.
pub fn find_file(listing: &[(usize, String, u64)], s: &Sources) -> Option<(usize, String, u64)> {
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_lowercase();
    let wanted: Vec<String> = [s.md5.clone(), format!("{}.{}", s.md5, s.format)].into_iter().chain(s.names.iter().map(|n| n.to_lowercase())).collect();
    if let Some(f) = listing.iter().find(|(_, p, _)| wanted.contains(&base(p))) {
        return Some(f.clone());
    }
    let packed = |p: &str| [".tar", ".zip", ".7z", ".rar", ".gz", ".zst"].iter().any(|e| p.to_lowercase().ends_with(e));
    let same: Vec<_> = listing.iter().filter(|(_, p, len)| s.size > 0 && *len == s.size && !packed(p)).collect();
    match same.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

async fn by_torrent(http: &reqwest::Client, path: &str, listed: &Listed, s: &Sources, dest: &Path, most: u64, place: &Place<'_>) -> Result<()> {
    let work: PathBuf = place.books_dir.join("torrent-work").join(&s.md5);
    let _ = tokio::fs::remove_dir_all(&work).await;
    tokio::fs::create_dir_all(&work).await?;
    let session = librqbit::Session::new_with_opts(work.clone(), librqbit::SessionOptions { disable_local_service_discovery: true, ..Default::default() }).await?;
    let result = async {
        let kept = place.books_dir.join("torrents").join(format!("{}.torrent", listed.btih.to_lowercase()));
        let bytes = torrent_file(http, &session, path, listed, &kept).await?;
        let listing = files(&bytes)?;
        let (idx, inside, len) = find_file(&listing, s).ok_or_else(|| anyhow!("the book is not a file of its own in it"))?;
        if len > most {
            bail!("its file there is larger than a book should be");
        }
        let opts = librqbit::AddTorrentOptions { only_files: Some(vec![idx]), overwrite: true, output_folder: Some(work.to_string_lossy().to_string()), ..Default::default() };
        let handle = session.add_torrent(librqbit::AddTorrent::TorrentFileBytes(bytes.clone()), Some(opts)).await?.into_handle().context("the torrent was not taken")?;
        let (started, mut since, mut last) = (Instant::now(), Instant::now(), 0u64);
        loop {
            if place.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                bail!("Spool is stopping");
            }
            let st = handle.stats();
            if let Some(e) = st.error {
                bail!("the torrent failed: {e}");
            }
            if st.finished {
                break;
            }
            let have = st.file_progress.get(idx).copied().unwrap_or(0);
            if have > last {
                (last, since) = (have, Instant::now());
            } else if since.elapsed() > STALL {
                bail!("no peer gave any of it for {} minutes", STALL.as_secs() / 60);
            }
            if started.elapsed() > LONGEST {
                bail!("it was not had in {} minutes", LONGEST.as_secs() / 60);
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        let got = work.join(&inside);
        settle(&got, &s.md5, dest).await
    }
    .await;
    session.stop().await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_book_is_found_among_a_torrents_files() {
        let s = Sources { md5: "3a332ea0e31df1116b87b365e4233267".into(), size: 900, format: "epub".into(), names: vec!["aacid__zlib3_files__2024".into()], ..Default::default() };
        let listing = |names: &[(&str, u64)]| names.iter().enumerate().map(|(i, (n, l))| (i, n.to_string(), *l)).collect::<Vec<_>>();
        // Library Genesis: by MD5, with or without the extension.
        assert_eq!(find_file(&listing(&[("r_1000/aaaa", 900), ("r_1000/3a332ea0e31df1116b87b365e4233267", 900)]), &s).map(|f| f.0), Some(1));
        assert_eq!(find_file(&listing(&[("f_1000/3a332ea0e31df1116b87b365e4233267.epub", 900)]), &s).map(|f| f.0), Some(0));
        // Anna's own collections: by AACID.
        assert_eq!(find_file(&listing(&[("x/aacid__zlib3_files__2024", 5)]), &s).map(|f| f.0), Some(0));
        // By size, when only one file is that size.
        assert_eq!(find_file(&listing(&[("a", 10), ("b", 900)]), &s).map(|f| f.0), Some(1));
        assert_eq!(find_file(&listing(&[("a", 900), ("b", 900)]), &s), None, "two of its size: neither is taken");
        // Packed with others: not fetched.
        assert_eq!(find_file(&listing(&[("pilimi-zlib-0-119999.tar", 900)]), &s), None);
    }

    /// One book out of a real torrent of Anna's Archive, as if the archive were down: the
    /// torrent's path is one it has not got, so its list of files has to come from its peers.
    /// Run by hand: `cargo test -p spool --lib fallback::tests::live -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_a_book_out_of_a_torrent_without_annas_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = "test/not-on-annas-archive.torrent";
        let btih = "990e68b9059692c5d23e34017afbe0cbd32bf59e";
        let mut list = HashMap::new();
        list.insert(path, Listed { btih: btih.into(), magnet: format!("magnet:?xt=urn:btih:{btih}&tr=udp://tracker.opentrackr.org:1337/announce") });
        save_list(&dir.path().join("torrents.json"), &list).unwrap();
        let s = Sources { md5: "fbfd73e286a96143328eea7195861343".into(), size: 23981, format: "epub".into(), torrents: vec![path.into()], ..Default::default() };
        let stopping = std::sync::atomic::AtomicBool::new(false);
        let dest = dir.path().join("book.epub");
        let how = fetch(&reqwest::Client::new(), &s, &dest, 200_000_000, &Place { books_dir: dir.path(), stopping: &stopping }).await.unwrap();
        assert_eq!(file_md5(&dest).await.unwrap(), s.md5, "{how}");
        assert!(dir.path().join("torrents").join(format!("{btih}.torrent")).is_file(), "the .torrent is kept for next time");
        eprintln!("{how}");
    }

    #[test]
    fn the_list_of_torrents_is_kept_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("torrents.json");
        let mut list = HashMap::new();
        list.insert("external/libgen_rs_fic/f_1000.torrent", Listed { btih: "abc".into(), magnet: "magnet:?xt=urn:btih:abc".into() });
        save_list(&path, &list).unwrap();
        assert_eq!(load_list(&path)["external/libgen_rs_fic/f_1000.torrent"].btih, "abc");
    }
}
