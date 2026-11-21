//! Shares: a video of a channel, or a clip, given a link that plays it for anyone on the
//! internet, without signing in.
//!
//! Shared videos are served from a door of their own, apart from the web app: a port open
//! only to this machine, which a tunnel (Cloudflare's, say) carries to a public address such
//! as `https://v.example.com`. That door answers for shared videos alone, so nothing else of
//! Spool can be reached through it, whatever the tunnel is told. Each link is a short random
//! name, found only by those who are sent it, and ends when the share is removed.
//!
//! A video of a channel is made into an MP4 that every browser plays (H.264 and AAC, no
//! larger than 1080p), copied when it is already that, and kept until the share ends, with a
//! still for the page's preview. A clip is already such an MP4, and is served as it is.

use crate::app::App;
use crate::db::Db;
use anyhow::{anyhow, bail, Result};
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Router;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Share {
    pub id: i64,
    /// What its link is known by: `<address>/<token>`.
    pub token: String,
    /// What is shared: a file of the library, or a clip.
    pub file_id: Option<i64>,
    pub clip_id: Option<i64>,
    pub title_id: Option<i64>,
    /// What it is called on its page.
    pub name: String,
    /// What it is from, in words: a channel's name, or what a clip was cut from.
    pub from: String,
    /// "making", "ready" or "failed".
    pub state: String,
    pub error: String,
    pub size: u64,
    pub created_at: i64,
    /// How many times its page has been opened.
    pub views: u64,
}

/// The port of the door for shared videos, open only to this machine: 7980, unless
/// `SPOOL_SHARE_PORT` names another.
pub fn port() -> u16 {
    std::env::var("SPOOL_SHARE_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(7980)
}

/// A link's name: ten letters and digits, too many to guess.
fn new_token() -> String {
    const LETTERS: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    crate::settings::new_key().as_bytes().chunks(2).take(10).map(|pair| LETTERS[usize::from_str_radix(std::str::from_utf8(pair).unwrap_or("0"), 16).unwrap_or(0) % LETTERS.len()] as char).collect()
}

impl Db {
    fn share_row(r: &rusqlite::Row) -> rusqlite::Result<Share> {
        let mut s: Share = serde_json::from_str(&r.get::<_, String>(2)?).map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e)))?;
        (s.id, s.token) = (r.get(0)?, r.get(1)?);
        Ok(s)
    }

    /// Every share, the newest first.
    pub fn shares(&self) -> Result<Vec<Share>> {
        self.with(|c| c.prepare("SELECT id, token, data FROM shares ORDER BY id DESC")?.query_map([], Self::share_row)?.collect())
    }

    pub fn share(&self, id: i64) -> Result<Option<Share>> {
        self.with(|c| c.query_row("SELECT id, token, data FROM shares WHERE id = ?1", [id], Self::share_row).optional())
    }

    pub fn share_by_token(&self, token: &str) -> Result<Option<Share>> {
        self.with(|c| c.query_row("SELECT id, token, data FROM shares WHERE token = ?1", [token], Self::share_row).optional())
    }

    /// Write down a share: a new one is given its id.
    pub fn save_share(&self, s: &mut Share) -> Result<()> {
        let data = serde_json::to_string(s)?;
        if s.id == 0 {
            s.id = self.with(|c| {
                c.execute("INSERT INTO shares(token, data) VALUES (?1, ?2)", params![s.token, data])?;
                Ok(c.last_insert_rowid())
            })?;
        } else {
            self.with(|c| c.execute("UPDATE shares SET data = ?2 WHERE id = ?1", params![s.id, data]))?;
        }
        Ok(())
    }

    pub fn delete_share(&self, id: i64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM shares WHERE id = ?1", [id]))?;
        Ok(())
    }

    /// A clip removed takes its shares with it.
    pub fn delete_shares_of_clip(&self, clip: i64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM shares WHERE json_extract(data, '$.clip_id') = ?1", [clip]))?;
        Ok(())
    }
}

impl App {
    fn shares_dir(&self) -> PathBuf {
        self.data_dir.join("shares")
    }

    /// The address a share's page is at, when sharing has an address.
    pub fn share_url(&self, s: &Share) -> Option<String> {
        let base = self.settings.general().share_address.trim().trim_end_matches('/').to_string();
        (!base.is_empty()).then(|| format!("{base}/{}", s.token))
    }

    /// The video a share plays, and its still.
    fn share_files(&self, s: &Share) -> (PathBuf, PathBuf) {
        match s.clip_id {
            Some(c) => (self.clip_video(c), self.clip_still(c)),
            None => (self.shares_dir().join(format!("{}.mp4", s.token)), self.shares_dir().join(format!("{}.jpg", s.token))),
        }
    }

    /// Share a video of a channel, or a clip. One already shared keeps its link.
    pub fn add_share(&self, file_id: Option<i64>, clip_id: Option<i64>) -> Result<Share> {
        if let Some(s) = self.db.shares()?.into_iter().find(|s| (file_id.is_some() && s.file_id == file_id) || (clip_id.is_some() && s.clip_id == clip_id)) {
            return Ok(s);
        }
        let mut s = Share { token: new_token(), created_at: crate::db::now(), ..Default::default() };
        match (file_id, clip_id) {
            (Some(f), None) => {
                let file = self.db.file(f)?.ok_or_else(|| anyhow!("there is no such file"))?;
                let title = self.db.title(file.title_id)?.ok_or_else(|| anyhow!("the file belongs to no title"))?;
                if title.channel.is_none() {
                    bail!("only the videos of channels, and clips, can be shared");
                }
                let episode = self.db.episodes(title.id)?.into_iter().find(|e| e.file_id == Some(f));
                (s.file_id, s.title_id, s.from) = (Some(f), Some(title.id), title.title.clone());
                s.name = episode.map(|e| e.title).filter(|t| !t.is_empty()).unwrap_or_else(|| title.title.clone());
                s.state = "making".into();
            }
            (None, Some(c)) => {
                let clip = self.db.clip(c)?.ok_or_else(|| anyhow!("there is no such clip"))?;
                if clip.state != "ready" {
                    bail!("the clip is not ready yet");
                }
                (s.clip_id, s.title_id, s.name, s.from, s.size) = (Some(c), clip.title_id, clip.name, clip.from, clip.size);
                s.state = "ready".into();
            }
            _ => bail!("share a file or a clip"),
        }
        self.db.save_share(&mut s)?;
        if s.state == "making" {
            self.make_share(s.id);
        }
        Ok(s)
    }

    /// Make a shared video's MP4 and still in the background.
    fn make_share(&self, id: i64) {
        let app = self.clone();
        tokio::spawn(async move {
            static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
            let _one = ONE.lock().await;
            let Ok(Some(mut s)) = app.db.share(id) else { return };
            match app.share_video(&s).await {
                Ok(size) => (s.state, s.size, s.error) = ("ready".into(), size, String::new()),
                Err(e) => {
                    tracing::warn!(share = id, error = %e, "a shared video could not be made");
                    (s.state, s.error) = ("failed".into(), format!("{e:#}"));
                }
            }
            // One stopped meanwhile stays stopped, and takes what was made with it.
            if app.db.share(id).ok().flatten().is_some() {
                let _ = app.db.save_share(&mut s);
            } else {
                let (video, still) = app.share_files(&s);
                let _ = std::fs::remove_file(video);
                let _ = std::fs::remove_file(still);
            }
        });
    }

    async fn share_video(&self, s: &Share) -> Result<u64> {
        let file = self.db.file(s.file_id.unwrap_or(0))?.ok_or_else(|| anyhow!("the file is gone"))?;
        let title = self.db.title(file.title_id)?.ok_or_else(|| anyhow!("the file belongs to no title"))?;
        let tool = self.ffmpeg_path().ok_or_else(|| anyhow!("ffmpeg is not installed"))?;
        let src = title.file_path(&file);
        let info = match file.media_info.clone().filter(|i| !i.streams.is_empty()) {
            Some(info) => info,
            None => self.probe(&src, "").await?.ok_or_else(|| anyhow!("the file could not be read"))?,
        };
        tokio::fs::create_dir_all(self.shares_dir()).await?;
        let (video, still) = self.share_files(s);
        let part = video.with_extension("mp4.part");
        let made = tokio::process::Command::new(&tool).args(crate::clips::save_arguments(&info, &src, &part, true)).kill_on_drop(true).output().await?;
        if !made.status.success() {
            let _ = tokio::fs::remove_file(&part).await;
            bail!("{}", String::from_utf8_lossy(&made.stderr).lines().last().unwrap_or("ffmpeg failed"));
        }
        tokio::fs::rename(&part, &video).await?;
        // A still from a tenth of the way in, for the page and the preview where the link is sent.
        let at = format!("{:.1}", info.runtime_secs / 10.0);
        let _ = tokio::process::Command::new(&tool)
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-ss", &at, "-i"])
            .arg(&video)
            .args(["-frames:v", "1", "-vf", "scale=1280:-2", "-q:v", "3"])
            .arg(&still)
            .kill_on_drop(true)
            .output()
            .await;
        Ok(tokio::fs::metadata(&video).await?.len())
    }

    /// Stop sharing: the link ends, and the MP4 made for it goes.
    pub fn remove_share(&self, id: i64) -> Result<()> {
        let Some(s) = self.db.share(id)? else { bail!("no such share") };
        self.db.delete_share(id)?;
        if s.clip_id.is_none() {
            let (video, still) = self.share_files(&s);
            let _ = std::fs::remove_file(video);
            let _ = std::fs::remove_file(still);
        }
        Ok(())
    }

    /// Make again the shares a stop cut short.
    pub fn resume_shares(&self) {
        for s in self.db.shares().unwrap_or_default().into_iter().filter(|s| s.state == "making") {
            self.make_share(s.id);
        }
    }
}

/// Open the door for shared videos, on this machine alone.
pub fn spawn(app: App) {
    tokio::spawn(async move {
        let port = port();
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => {
                tracing::info!(port, "serving shared videos");
                if let Err(e) = axum::serve(listener, router(app)).await {
                    tracing::warn!(error = %e, "the door for shared videos closed");
                }
            }
            Err(e) => tracing::warn!(port, error = %e, "the door for shared videos could not be opened"),
        }
    });
}

/// What the door answers: a shared video's page, its video and its still, and nothing else.
pub fn router(app: App) -> Router {
    Router::new().route("/{name}", axum::routing::get(serve)).fallback(|| async { StatusCode::NOT_FOUND }).with_state(app)
}

async fn serve(State(app): State<App>, UrlPath(name): UrlPath<String>, method: Method, headers: HeaderMap) -> Response {
    let (token, ext) = name.rsplit_once('.').map_or((name.as_str(), ""), |(t, e)| (t, e));
    if token.len() != 10 || !token.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(Some(mut s)) = app.db.share_by_token(token) else { return StatusCode::NOT_FOUND.into_response() };
    let (video, still) = app.share_files(&s);
    let head = method == Method::HEAD;
    match ext {
        "mp4" if s.state == "ready" => {
            let mut r = crate::serve::send_file(&video, &headers, head).await;
            r.headers_mut().insert(header::CONTENT_TYPE, header::HeaderValue::from_static("video/mp4"));
            r
        }
        "jpg" => crate::serve::send_file(&still, &headers, head).await,
        "" => {
            if !head {
                s.views += 1;
                let _ = app.db.save_share(&mut s);
            }
            page(&s).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A shared video's page: the video, its name, and what a messaging app shows of the link.
fn page(s: &Share) -> Html<String> {
    let (name, from, t) = (escape(&s.name), escape(&s.from), &s.token);
    let body = match s.state.as_str() {
        "ready" => format!(r#"<video src="{t}.mp4" poster="{t}.jpg" controls playsinline preload="metadata"></video>"#),
        "making" => r#"<p class="note">This video is being got ready. Try again in a minute or two.</p>"#.to_string(),
        _ => r#"<p class="note">This video could not be got ready.</p>"#.to_string(),
    };
    Html(format!(
        r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{name}</title>
<meta property="og:title" content="{name}"><meta property="og:description" content="{from}">
<meta property="og:type" content="video.other"><meta property="og:image" content="{t}.jpg">
<meta property="og:video" content="{t}.mp4"><meta property="og:video:type" content="video/mp4">
<meta name="twitter:card" content="summary_large_image"><meta name="robots" content="noindex">
<style>
:root{{color-scheme:dark}}
body{{margin:0;background:#0d0d0d;color:#e8e6e1;font:16px/1.5 -apple-system,system-ui,sans-serif;display:flex;justify-content:center}}
main{{width:min(1100px,100%);padding:24px 16px;box-sizing:border-box}}
video{{width:100%;max-height:80vh;background:#000;border-radius:6px;display:block}}
h1{{font-size:1.25rem;margin:16px 0 2px;font-weight:600}}
p{{margin:0;color:#9a978f}}
.note{{padding:48px 0;text-align:center}}
</style></head>
<body><main>{body}<h1>{name}</h1><p>{from}</p></main></body></html>"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_is_ten_plain_letters_and_a_page_says_nothing_it_should_not() {
        let t = new_token();
        assert_eq!(t.len(), 10);
        assert!(t.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()), "{t}");
        assert_ne!(t, new_token());
        let s = Share { token: t.clone(), name: "<script>x</script>".into(), state: "ready".into(), ..Default::default() };
        let html = page(&s).0;
        assert!(!html.contains("<script>x") && html.contains(&format!("{t}.mp4")));
    }

    #[test]
    fn shares_are_kept_and_found_by_their_link() {
        let db = Db::memory().unwrap();
        let mut s = Share { token: "abcdefghij".into(), clip_id: Some(3), state: "ready".into(), ..Default::default() };
        db.save_share(&mut s).unwrap();
        assert_eq!(db.share_by_token("abcdefghij").unwrap().unwrap().id, s.id);
        db.delete_shares_of_clip(3).unwrap();
        assert!(db.shares().unwrap().is_empty());
    }
}
