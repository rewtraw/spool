//! Downloading a book from Anna's Archive with a member's key: its API gives a link to a
//! file by the file's MD5, and the file that comes is checked against that MD5, so what
//! arrives is the file that was asked for, whole.

use super::dump::MIRRORS;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io::AsyncWriteExt;

/// What the membership allows, as the API last said.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Allowance {
    pub downloads_left: i64,
    pub downloads_per_day: i64,
    /// When this was said.
    pub at: i64,
}

#[derive(Deserialize)]
struct Answer {
    download_url: Option<String>,
    error: Option<String>,
    account_fast_download_info: Option<Allowance>,
}

/// Why a download could not be had.
#[derive(Debug)]
pub enum Refused {
    /// The key is wrong, or its membership has lapsed: nothing is tried again until it changes.
    Key(String),
    /// Today's downloads are used up.
    Spent,
    /// This file, from every server: another file of the book may do.
    File(String),
    /// Anna's Archive could not be reached: the same file is tried again later.
    Away(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Key(e) => write!(f, "Anna's Archive turned the key down: {e}"),
            Refused::Spent => write!(f, "today's downloads from Anna's Archive are used up"),
            Refused::File(e) => write!(f, "{e}"),
            Refused::Away(e) => write!(f, "Anna's Archive could not be reached: {e}"),
        }
    }
}

/// A link to the file, from the first of its servers that answers, with what the
/// membership has left. `server` picks among the download servers.
async fn link(http: &reqwest::Client, key: &str, md5: &str, server: u32) -> Result<(String, Option<Allowance>), Refused> {
    let mut away = String::new();
    for base in MIRRORS {
        let resp = http
            .get(format!("{base}/dyn/api/fast_download.json"))
            .query(&[("md5", md5), ("key", key), ("domain_index", &server.to_string())])
            .timeout(std::time::Duration::from_secs(40))
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                away = e.without_url().to_string();
                continue;
            }
        };
        let status = resp.status();
        let Ok(a) = resp.json::<Answer>().await else {
            away = format!("{base} answered {status} with something other than the API");
            continue;
        };
        let mut allowance = a.account_fast_download_info;
        if let Some(x) = allowance.as_mut() {
            x.at = crate::db::now();
        }
        if let Some(url) = a.download_url {
            return Ok((url, allowance));
        }
        let e = a.error.unwrap_or_else(|| status.to_string());
        let lower = e.to_lowercase();
        return Err(if lower.contains("key") || lower.contains("member") {
            Refused::Key(e)
        } else if lower.contains("no downloads left") || lower.contains("limit") || allowance.as_ref().is_some_and(|x| x.downloads_left <= 0) {
            Refused::Spent
        } else {
            Refused::File(format!("Anna's Archive has no download of it: {e}"))
        });
    }
    Err(Refused::Away(away))
}

/// Download the file with this MD5 to `dest`, carrying on from what an earlier try left
/// in `dest`'s ".part", trying each download server in turn, and check that what came is
/// that file. Gives what the membership has left. A server that fails for a while (a time
/// out, an error of its own) leaves the file to be tried again later; only a file that is
/// wrong, too large or not there is refused for good.
pub async fn fetch(http: &reqwest::Client, key: &str, md5: &str, dest: &Path, most: u64) -> Result<Option<Allowance>, Refused> {
    let part = dest.with_extension("part");
    let mut allowance = None;
    let mut wrong: Option<String> = None;
    let mut away: Option<String> = None;
    for server in 0..4u32 {
        let (url, said) = match link(http, key, md5, server).await {
            Ok(x) => x,
            Err(Refused::File(e)) => {
                wrong.get_or_insert(e);
                break;
            }
            Err(e) => return Err(e),
        };
        allowance = said.or(allowance);
        match download(http, &url, &part, most).await {
            Ok(()) => {}
            Err(Failed::TooLarge) => {
                let _ = tokio::fs::remove_file(&part).await;
                wrong = Some("the file is larger than a book should be".into());
                break;
            }
            Err(Failed::Now(e)) => {
                tracing::info!(md5, server, error = %e, "a download server failed; trying the next");
                away = Some(e);
                continue;
            }
        }
        let got = file_md5(&part).await.map_err(|e| Refused::Away(e.to_string()))?;
        if got != md5 {
            let _ = tokio::fs::remove_file(&part).await;
            wrong = Some("the file that came was not the one asked for (its MD5 differs)".into());
            continue;
        }
        tokio::fs::rename(&part, dest).await.map_err(|e| Refused::Away(e.to_string()))?;
        return Ok(allowance);
    }
    match (away, wrong) {
        (Some(e), _) => Err(Refused::Away(format!("the download servers failed: {e}"))),
        (None, Some(e)) => Err(Refused::File(e)),
        (None, None) => Err(Refused::Away("no download server answered".into())),
    }
}

enum Failed {
    /// The file is too large to be a book: refused for good.
    TooLarge,
    /// The server failed this time.
    Now(String),
}

/// Fetch a URL into `part`, from where it left off.
async fn download(http: &reqwest::Client, url: &str, part: &Path, most: u64) -> Result<(), Failed> {
    let now = |e: String| Failed::Now(e);
    let have = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
    let mut req = http.get(url).timeout(std::time::Duration::from_secs(600));
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req.send().await.map_err(|e| now(e.without_url().to_string()))?;
    let status = resp.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        // Already whole.
        return Ok(());
    }
    if !status.is_success() {
        return Err(now(format!("the download server answered {status}")));
    }
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
    if resp.content_length().is_some_and(|n| n + if resumed { have } else { 0 } > most) {
        return Err(Failed::TooLarge);
    }
    let mut f = tokio::fs::OpenOptions::new().create(true).write(true).append(resumed).truncate(!resumed).open(part).await.map_err(|e| now(e.to_string()))?;
    let mut written = if resumed { have } else { 0 };
    let mut body = resp;
    loop {
        let chunk = match tokio::time::timeout(std::time::Duration::from_secs(60), body.chunk()).await {
            Err(_) => return Err(now("the download stalled".into())),
            Ok(Err(e)) => return Err(now(e.without_url().to_string())),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(c))) => c,
        };
        written += chunk.len() as u64;
        if written > most {
            return Err(Failed::TooLarge);
        }
        f.write_all(&chunk).await.map_err(|e| now(e.to_string()))?;
    }
    f.flush().await.map_err(|e| now(e.to_string()))?;
    Ok(())
}

async fn file_md5(p: &Path) -> std::io::Result<String> {
    let p = p.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut f = std::fs::File::open(p)?;
        let mut h = Md5::new();
        std::io::copy(&mut f, &mut h)?;
        Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
    })
    .await
    .map_err(std::io::Error::other)?
}
