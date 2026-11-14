//! What Spool asks of yt-dlp, which knows how to read YouTube, X, Instagram and well over a
//! thousand other sites, and keeps up with them as they change. Spool only runs it: once to
//! read what a link is, and once to fetch a video with its captions and picture.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Where yt-dlp is installed: where SPOOL_YT_DLP says, or where package managers put it.
pub fn find() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SPOOL_YT_DLP").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let fixed = ["/opt/homebrew/bin/yt-dlp".to_string(), "/usr/local/bin/yt-dlp".into(), "/usr/bin/yt-dlp".into(), format!("{home}/.local/bin/yt-dlp")];
    fixed.iter().map(PathBuf::from).find(|p| p.is_file()).or_else(|| std::env::var("PATH").ok().and_then(|p| p.split(':').map(|d| Path::new(d).join("yt-dlp")).find(|p| p.is_file())))
}

/// The search path yt-dlp is run with. It needs ffmpeg to join a video's picture and sound,
/// and a JavaScript runtime (deno or node) to read YouTube, and finds both on the path; a
/// service started by the system is given one without where package managers put them.
fn search_path(tool: &Path) -> String {
    let mut dirs: Vec<String> = tool.parent().map(|d| d.to_string_lossy().to_string()).into_iter().collect();
    let home = std::env::var("HOME").unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin".to_string(), "/usr/local/bin".into(), format!("{home}/.deno/bin"), format!("{home}/.local/bin")]);
    dirs.extend(std::env::var("PATH").unwrap_or_default().split(':').filter(|d| !d.is_empty()).map(str::to_string));
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs.join(":")
}

/// What yt-dlp says of one video. Sites fill in different fields: a YouTube video has a
/// channel, a post on X or Instagram only the account that posted it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Info {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    /// yt-dlp's name for the site: "Youtube", "Twitter", "Instagram".
    pub extractor_key: String,
    pub webpage_url: String,
    pub channel: Option<String>,
    pub channel_id: Option<String>,
    pub channel_url: Option<String>,
    pub uploader: Option<String>,
    pub uploader_id: Option<String>,
    pub uploader_url: Option<String>,
    /// YYYYMMDD.
    pub upload_date: Option<String>,
    pub timestamp: Option<f64>,
    /// Seconds.
    pub duration: Option<f64>,
    pub height: Option<u32>,
    /// Captions people wrote, by language.
    pub subtitles: serde_json::Map<String, serde_json::Value>,
    pub thumbnail: Option<String>,
    pub is_live: Option<bool>,
    pub live_status: Option<String>,
}

impl Info {
    /// The channel or account it was posted by: (its id on the site, its name, its address).
    pub fn poster(&self) -> (String, String, String) {
        let id = self.channel_id.clone().or_else(|| self.uploader_id.clone()).or_else(|| self.uploader.clone()).unwrap_or_else(|| "unknown".into());
        let name = self.channel.clone().or_else(|| self.uploader.clone()).or_else(|| self.uploader_id.clone()).unwrap_or_else(|| id.clone());
        let url = self.channel_url.clone().or_else(|| self.uploader_url.clone()).unwrap_or_default();
        (id, name, url)
    }

    /// The day it was posted, YYYY-MM-DD.
    pub fn day(&self) -> Option<String> {
        if let Some(d) = self.upload_date.as_deref().filter(|d| d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit())) {
            return Some(format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..]));
        }
        self.timestamp.and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0)).map(|t| t.format("%Y-%m-%d").to_string())
    }
}

/// One entry of a channel or a playlist, as listed without reading each video.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub id: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub timestamp: Option<f64>,
    pub upload_date: Option<String>,
    pub live_status: Option<String>,
}

/// A channel, an account or a playlist and what is in it, newest first as the site lists it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Listing {
    pub id: String,
    pub title: Option<String>,
    pub extractor_key: String,
    pub webpage_url: String,
    pub channel: Option<String>,
    pub channel_id: Option<String>,
    pub channel_url: Option<String>,
    pub uploader: Option<String>,
    pub uploader_id: Option<String>,
    pub uploader_url: Option<String>,
    pub entries: Vec<serde_json::Value>,
    pub thumbnails: Vec<Thumbnail>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Thumbnail {
    pub id: Option<String>,
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Listing {
    /// Its videos. A channel on YouTube lists its tabs (videos, shorts, live), each a listing of
    /// its own; their videos are taken together.
    pub fn videos(&self) -> Vec<Entry> {
        let mut out = vec![];
        for e in &self.entries {
            if e.get("_type").and_then(|t| t.as_str()) == Some("playlist") || e.get("entries").is_some() {
                if let Ok(inner) = serde_json::from_value::<Listing>(e.clone()) {
                    out.extend(inner.videos());
                }
                continue;
            }
            if let Ok(entry) = serde_json::from_value::<Entry>(e.clone()) {
                if !entry.id.is_empty() && entry.live_status.as_deref() != Some("is_upcoming") {
                    out.push(entry);
                }
            }
        }
        out
    }

    /// The channel's own picture: its avatar on YouTube, square.
    pub fn avatar(&self) -> Option<String> {
        let square = |t: &&Thumbnail| t.width.is_some() && t.width == t.height;
        self.thumbnails.iter().find(|t| t.id.as_deref() == Some("avatar_uncropped")).or_else(|| self.thumbnails.iter().filter(square).max_by_key(|t| t.width)).map(|t| t.url.clone())
    }

    /// The channel or account it belongs to: (its id on the site, its name, its address).
    pub fn poster(&self) -> (String, String, String) {
        let id = self.channel_id.clone().or_else(|| self.uploader_id.clone()).unwrap_or_else(|| self.id.clone());
        let name = self.channel.clone().or_else(|| self.uploader.clone()).or_else(|| self.title.clone()).unwrap_or_else(|| id.clone());
        let url = self.channel_url.clone().or_else(|| self.uploader_url.clone()).unwrap_or_else(|| self.webpage_url.clone());
        (id, name, url)
    }
}

/// What a link turned out to be.
pub enum Found {
    Video(Box<Info>),
    List(Box<Listing>),
}

/// Arguments every run gets: no settings file of the person's own, quiet, and signed in to the
/// sites as the cookies setting says.
fn base(cookies: &str) -> Vec<String> {
    let mut a: Vec<String> = ["--ignore-config", "--no-warnings", "--no-progress", "--encoding", "utf-8"].map(String::from).to_vec();
    let c = cookies.trim();
    if c.starts_with('/') {
        a.extend(["--cookies".into(), c.to_string()]);
    } else if !c.is_empty() {
        a.extend(["--cookies-from-browser".into(), c.to_string()]);
    }
    a
}

async fn run_json(tool: &Path, args: Vec<String>, wait: std::time::Duration) -> Result<serde_json::Value> {
    let mut cmd = tokio::process::Command::new(tool);
    cmd.args(&args).env("PATH", search_path(tool)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let out = tokio::time::timeout(wait, cmd.output()).await.map_err(|_| anyhow!("yt-dlp took longer than {} seconds to answer", wait.as_secs()))??;
    if !out.status.success() {
        bail!("{}", said(&String::from_utf8_lossy(&out.stderr)));
    }
    serde_json::from_slice(&out.stdout).context("reading what yt-dlp said")
}

/// The part of what yt-dlp printed that says what went wrong, without its prefixes.
pub fn said(stderr: &str) -> String {
    let line = stderr.lines().rev().find(|l| l.starts_with("ERROR:")).or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty())).unwrap_or("yt-dlp stopped without saying why");
    let line = line.trim_start_matches("ERROR:").trim();
    // "[youtube] abc123: Video unavailable" names the site and the id, which the person knows.
    let line = match line.strip_prefix('[').and_then(|r| r.split_once("] ")) {
        Some((_, rest)) => rest.split_once(": ").map(|(_, why)| why).unwrap_or(rest),
        None => line,
    };
    line.chars().take(300).collect()
}

/// What a link is: one video, or a channel, account or playlist with a list of what is in it.
/// A listing names at most `most` of its newest videos.
pub async fn look(tool: &Path, cookies: &str, url: &str, most: usize) -> Result<Found> {
    let mut args = base(cookies);
    args.extend(["--dump-single-json".into(), "--flat-playlist".into(), "--playlist-end".into(), most.to_string(), "--".into(), url.to_string()]);
    let v = run_json(tool, args, std::time::Duration::from_secs(120)).await?;
    if v.get("_type").and_then(|t| t.as_str()) == Some("playlist") {
        return Ok(Found::List(Box::new(serde_json::from_value(v)?)));
    }
    Ok(Found::Video(Box::new(serde_json::from_value(v)?)))
}

/// How far a fetch has got.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Progress {
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// Bytes a second.
    pub speed: u64,
    /// Seconds to go, as yt-dlp reckons it.
    pub eta: Option<u64>,
    /// Which part is coming: a video and its sound are fetched one after the other.
    pub part: u32,
}

const MARK: &str = "SPOOL-PROGRESS";

/// Read one line of the progress yt-dlp was asked to print.
fn progress_line(line: &str) -> Option<(u64, u64, u64, Option<u64>)> {
    let rest = line.trim().strip_prefix(MARK)?.trim();
    let f: Vec<&str> = rest.split_whitespace().collect();
    let num = |i: usize| f.get(i).and_then(|s| s.parse::<f64>().ok()).filter(|n| n.is_finite() && *n >= 0.0).map(|n| n as u64);
    let done = num(0)?;
    let total = num(1).or(num(2)).unwrap_or(0);
    Some((done, total, num(3).unwrap_or(0), num(4)))
}

/// Fetch one video into `dir` as `video.<ext>`, with its captions in `languages` as written
/// (`video.<lang>.vtt`), its picture (`video.jpg`) and what yt-dlp knows of it
/// (`video.info.json`). `progress` is told how it goes; `stop` ends it early.
pub async fn fetch(tool: &Path, cookies: &str, url: &str, dir: &Path, languages: &[String], progress: impl Fn(Progress), stop: &tokio::sync::Notify) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut args = base(cookies);
    args.retain(|a| a != "--no-progress");
    // Captions in each language wanted, both those people wrote and the site's own, with
    // their regional forms ("en-GB") and the original of the site's own ("en-orig"); which to
    // keep is chosen once they are here. Not the site's translations ("en-de", from German),
    // of which YouTube offers dozens and turns away a reader who asks for them all.
    let langs: Vec<String> = languages.iter().flat_map(|l| [l.clone(), format!("{l}-orig"), format!("{l}-[A-Z][A-Za-z]+")]).chain(["-live_chat".to_string()]).collect();
    args.extend([
        "--no-playlist".into(),
        // Captions that cannot be had do not stop the video: whether it came is seen below.
        "--ignore-errors".into(),
        "--newline".into(),
        "--progress-template".into(),
        format!("download:{MARK} %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(progress.speed)s %(progress.eta)s"),
        // The best there is, kept as it was made: Matroska holds any picture, sound and captions.
        "--format".into(),
        "bestvideo*+bestaudio/best".into(),
        "--merge-output-format".into(),
        "mkv".into(),
        "--write-subs".into(),
        "--write-auto-subs".into(),
        "--sub-format".into(),
        "vtt/best".into(),
        "--sub-langs".into(),
        langs.join(","),
        "--write-thumbnail".into(),
        "--convert-thumbnails".into(),
        "jpg".into(),
        "--write-info-json".into(),
        "--no-write-playlist-metafiles".into(),
        "--no-part".into(),
        "--paths".into(),
        dir.to_string_lossy().to_string(),
        "--output".into(),
        "video.%(ext)s".into(),
        "--output".into(),
        "thumbnail:video.%(ext)s".into(),
        "--output".into(),
        "subtitle:video.%(ext)s".into(),
        "--output".into(),
        "infojson:video".into(),
        "--".into(),
        url.to_string(),
    ]);
    let mut child = tokio::process::Command::new(tool).args(&args).env("PATH", search_path(tool)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn().context("starting yt-dlp")?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no output from yt-dlp"))?;
    let mut stderr = child.stderr.take().ok_or_else(|| anyhow!("no output from yt-dlp"))?;
    let errors = tokio::spawn(async move {
        let mut s = String::new();
        let _ = tokio::io::AsyncReadExt::read_to_string(&mut stderr, &mut s).await;
        s
    });
    let reading = async {
        let mut lines = BufReader::new(stdout).lines();
        let (mut part, mut last_done) = (1u32, 0u64);
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some((done, total, speed, eta)) = progress_line(&line) {
                // A count that starts again is the next part: the sound after the picture.
                if done < last_done / 2 {
                    part += 1;
                }
                last_done = done;
                progress(Progress { done_bytes: done, total_bytes: total, speed, eta, part });
            }
        }
        child.wait().await
    };
    let status = tokio::select! {
        s = reading => s?,
        _ = stop.notified() => bail!("stopped"),
    };
    let stderr = errors.await.unwrap_or_default();
    let came = std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
        let p = e.path();
        p.file_stem().is_some_and(|s| s == "video") && p.extension().is_some_and(|x| matches!(x.to_str(), Some("mkv" | "mp4" | "webm" | "mov" | "m4v")))
    });
    if !came || !dir.join("video.info.json").is_file() {
        bail!("{}", said(&stderr));
    }
    if !status.success() {
        tracing::debug!(url, said = %said(&stderr), "yt-dlp fetched the video but not everything with it");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_progress_and_what_went_wrong() {
        assert_eq!(progress_line("SPOOL-PROGRESS 1024 4096 NA 512.5 6"), Some((1024, 4096, 512, Some(6))));
        assert_eq!(progress_line("SPOOL-PROGRESS 1024 NA 8000.0 NA NA"), Some((1024, 8000, 0, None)));
        assert_eq!(progress_line("[download] Destination: video.mkv"), None);
        assert_eq!(said("WARNING: x\nERROR: [youtube] dQw4w9WgXcQ: Video unavailable. This video is private\n"), "Video unavailable. This video is private");
        assert_eq!(said("ERROR: Unsupported URL: https://example.com/\n"), "Unsupported URL: https://example.com/");
    }

    #[test]
    fn names_the_poster_and_the_day() {
        let i: Info = serde_json::from_value(serde_json::json!({"id": "1", "title": "t", "extractor_key": "Twitter", "uploader": "Jacques Vallée", "uploader_id": "jvallee", "timestamp": 1763078400.0})).unwrap();
        assert_eq!(i.poster(), ("jvallee".into(), "Jacques Vallée".into(), String::new()));
        assert_eq!(i.day().as_deref(), Some("2025-11-14"));
        let y: Info = serde_json::from_value(serde_json::json!({"id": "2", "extractor_key": "Youtube", "channel": "Ch", "channel_id": "UC1", "uploader_id": "@ch", "upload_date": "20240102"})).unwrap();
        assert_eq!(y.poster().0, "UC1");
        assert_eq!(y.day().as_deref(), Some("2024-01-02"));
    }

    #[test]
    fn a_channel_lists_the_videos_of_its_tabs() {
        let l: Listing = serde_json::from_value(serde_json::json!({
            "id": "UC1", "_type": "playlist", "extractor_key": "YoutubeTab", "channel": "Ch",
            "entries": [
                {"_type": "playlist", "id": "UC1-videos", "entries": [{"id": "a", "url": "https://www.youtube.com/watch?v=a"}, {"id": "b", "live_status": "is_upcoming"}]},
                {"_type": "playlist", "id": "UC1-shorts", "entries": [{"id": "c"}]}
            ],
            "thumbnails": [{"id": "banner", "url": "b", "width": 2000, "height": 300}, {"id": "avatar_uncropped", "url": "a"}]
        })).unwrap();
        assert_eq!(l.videos().iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(l.avatar().as_deref(), Some("a"));
    }
}
