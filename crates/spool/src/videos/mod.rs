//! Videos from YouTube, X, Instagram and the many other sites yt-dlp can read.
//!
//! A video is asked for by its link. It is fetched with its captions and its picture, and filed
//! under the channel or account that posted it, which Spool keeps as a series: a folder for the
//! channel, a season for each year and an episode for each video, named by the day it was
//! posted. So players, subtitles, clips and the search of what is said treat these videos as
//! they treat television, with nothing of their own to learn.
//!
//! A channel can be followed: it is looked at every hour, and what it has posted since is
//! fetched. The link to a channel can also bring its newest videos, or all of them.
//!
//! The fetching is yt-dlp's (see [`ytdlp`]): no other program keeps up with so many sites as
//! they change. Spool runs it, two videos at a time, into a folder beside its downloads, and
//! files what it fetched through the import journal, as it files a film.

pub mod captions;
pub mod ytdlp;

use crate::app::App;
use crate::models::{Episode, Kind, MediaFile, SeasonInfo, Title};
use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How many videos are fetched at once. They are small, and a site sends each slowly.
const AT_ONCE: usize = 2;
/// How many of a channel's newest videos are looked at when following it.
const LOOK_AT: usize = 30;
/// How many of a channel's newest videos "the newest" brings.
pub const NEWEST: usize = 10;
/// How many videos "all of them" brings at most.
const ALL: usize = 5000;

/// A channel or an account, kept on the title that holds its videos.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Channel {
    /// The site, as [`site_of`] names it: "youtube", "x", "instagram".
    pub site: String,
    /// The site's id for it.
    pub id: String,
    /// Where its videos are listed.
    pub url: String,
    /// Whether what it posts is fetched as it is posted.
    pub follow: bool,
    /// When it was last looked at for new videos.
    pub checked_at: i64,
    /// Videos it had when it was first followed, which were not asked for.
    pub skip: Vec<String>,
}

/// One video on its site, kept on the episode that is it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoRef {
    pub site: String,
    pub id: String,
    pub url: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Queued,
    Fetching,
    Filing,
    Done,
    Failed,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Fetching => "fetching",
            State::Filing => "filing",
            State::Done => "done",
            State::Failed => "failed",
        }
    }
}

/// A video asked for, from the moment it is asked for until it is filed.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Video {
    #[serde(skip_deserializing)]
    pub id: i64,
    pub url: String,
    pub state: State,
    pub site: String,
    pub video_id: String,
    pub title: String,
    /// The channel or account's name.
    pub channel: String,
    pub day: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
    pub title_id: Option<i64>,
    pub episode_id: Option<i64>,
    /// Which of the videos at `url` it is, counting from 1, when the link holds several: a post
    /// on X with more than one video.
    pub item: Option<u32>,
    pub error: String,
    pub added_at: i64,
    pub updated_at: i64,
}

impl Video {
    fn key(&self) -> String {
        format!("{}:{}", self.site, self.video_id)
    }
}

/// What is being fetched now, and how to stop it.
#[derive(Default)]
pub struct Videos {
    progress: Mutex<HashMap<i64, ytdlp::Progress>>,
    /// The videos being fetched, each with how to stop it. One worker a video, never two.
    stops: Mutex<HashMap<i64, Arc<tokio::sync::Notify>>>,
    wake: tokio::sync::Notify,
    /// Held while a video is filed, or a channel's title made: two videos of a channel not
    /// yet in the library would otherwise make it twice, or take the same number.
    filing: tokio::sync::Mutex<()>,
}

/// The site a video is on, from yt-dlp's name for what read it: "YoutubeTab" and "Youtube"
/// are both YouTube.
pub fn site_of(extractor: &str) -> String {
    let e = extractor.to_ascii_lowercase();
    for (prefix, site) in [("youtube", "youtube"), ("twitter", "x"), ("instagram", "instagram"), ("tiktok", "tiktok"), ("vimeo", "vimeo"), ("twitch", "twitch"), ("rumble", "rumble"), ("bilibili", "bilibili"), ("dailymotion", "dailymotion"), ("facebook", "facebook"), ("reddit", "reddit")] {
        if e.starts_with(prefix) {
            return site.into();
        }
    }
    e.trim_end_matches("tab").trim_end_matches("user").trim_end_matches("playlist").to_string()
}

/// A site's name as people write it.
pub fn site_name(site: &str) -> String {
    match site {
        "youtube" => "YouTube".into(),
        "x" => "X".into(),
        "instagram" => "Instagram".into(),
        "tiktok" => "TikTok".into(),
        "bilibili" => "Bilibili".into(),
        other => {
            let mut c = other.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        }
    }
}

/// A name that can be a file's: no characters a disk refuses, and not so long that a path
/// built from it is refused.
pub fn file_safe(name: &str, most: usize) -> String {
    let mut s: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { ' ' } else { c }).collect();
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s = s.trim_matches(['.', ' ']).to_string();
    if s.len() > most {
        let mut end = most;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s = s.trim_end_matches(['.', ' ']).to_string();
    }
    if s.is_empty() {
        "Untitled".into()
    } else {
        s
    }
}

/// The episode number a video gets: its month and day, then which of that day's videos it is.
/// "2026-11-14", the second that day, is 111402: in order, and the same whenever it is filed.
pub fn number_for(day: &str, nth: u32) -> u32 {
    let month: u32 = day.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(1);
    let date: u32 = day.get(8..10).and_then(|d| d.parse().ok()).unwrap_or(1);
    (month * 100 + date) * 100 + nth.clamp(1, 99)
}

/// Where a channel's video is kept, in its folder: "Season 2026/Channel - 2026-11-14 - What it
/// is called [id].mkv". The id at the end ties the file to its video when the folder is read.
pub fn rel_path(cfg: &spool_core::naming::NamingConfig, title: &Title, e: &Episode, ext: &str) -> String {
    let day = e.air_date.clone().unwrap_or_default();
    let id = e.video.as_ref().map(|v| v.id.as_str()).unwrap_or("");
    let stem = file_safe(&format!("{} - {day} - {} [{}]", file_safe(&title.title, 60), file_safe(&e.title, 120), file_safe(id, 40)), 220);
    format!("{}/{stem}.{ext}", spool_core::naming::season_folder_name(cfg, e.season))
}

/// What a link brought.
#[derive(Debug, Default, Serialize)]
pub struct Added {
    /// Videos newly asked for.
    pub added: usize,
    /// Videos already asked for or already here.
    pub already: usize,
    /// The channel, when the link was one, or when it is followed.
    pub title_id: Option<i64>,
    /// What the link was: "video", or "channel" for a channel, account or playlist.
    pub what: String,
    pub name: String,
    /// How many videos the channel listed, when it was one.
    pub listed: usize,
}

/// What to bring from a link to a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Take {
    /// Only what it posts from now on.
    New,
    /// Its ten newest.
    Newest,
    /// Everything it has.
    All,
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Video> {
    let id: i64 = r.get(0)?;
    let data: String = r.get(1)?;
    let mut v: Video = serde_json::from_str(&data).unwrap_or_default();
    v.id = id;
    Ok(v)
}

impl App {
    fn tool(&self) -> Result<PathBuf> {
        ytdlp::find().ok_or_else(|| anyhow!("yt-dlp is not installed on this machine. Install it (brew install yt-dlp, or pip install yt-dlp) and try again."))
    }

    fn cookies(&self) -> String {
        self.settings.general().video_cookies
    }

    // ------------------------------------------------------------ the record of what was asked for

    pub fn videos_all(&self) -> Result<Vec<Video>> {
        self.db.with(|c| {
            let mut s = c.prepare("SELECT id, data FROM videos ORDER BY id DESC LIMIT 500")?;
            let rows = s.query_map([], row)?;
            rows.collect()
        })
    }

    pub fn video(&self, id: i64) -> Result<Option<Video>> {
        use rusqlite::OptionalExtension;
        self.db.with(|c| c.query_row("SELECT id, data FROM videos WHERE id = ?1", [id], row).optional())
    }

    fn video_by_key(&self, key: &str) -> Result<Option<Video>> {
        use rusqlite::OptionalExtension;
        self.db.with(|c| c.query_row("SELECT id, data FROM videos WHERE key = ?1", [key], row).optional())
    }

    fn save_video(&self, v: &mut Video) -> Result<()> {
        v.updated_at = crate::db::now();
        let data = serde_json::to_string(v)?;
        if v.id == 0 {
            v.added_at = v.updated_at;
            let data = serde_json::to_string(v)?;
            let id = self.db.with(|c| {
                c.execute("INSERT INTO videos(key, state, added_at, data) VALUES (?1,?2,?3,?4)", params![v.key(), v.state.as_str(), v.added_at, data])?;
                Ok(c.last_insert_rowid())
            })?;
            v.id = id;
        } else {
            self.db.with(|c| c.execute("UPDATE videos SET state = ?2, data = ?3 WHERE id = ?1", params![v.id, v.state.as_str(), data]))?;
        }
        Ok(())
    }

    /// How far each video being fetched has got.
    pub fn video_progress(&self) -> HashMap<i64, ytdlp::Progress> {
        self.videos.progress.lock().clone()
    }

    /// Where videos are filed: the first videos folder Spool may write to that can be reached.
    pub fn video_root(&self) -> Option<String> {
        self.placement().0.into_iter().find(|(l, online)| *online && l.manage && !l.video_root.is_empty()).map(|(l, _)| l.video_root)
    }

    /// Whether this video is here or asked for already: by the queue, or by an episode.
    fn known(&self, site: &str, id: &str) -> Result<bool> {
        if self.video_by_key(&format!("{site}:{id}"))?.is_some_and(|v| v.state != State::Failed) {
            return Ok(true);
        }
        // An episode left without its file, by a filing cut short or a file deleted, can be fetched again.
        Ok(self.channel_episode(site, id)?.is_some_and(|e| e.file_id.is_some()))
    }

    fn channel_episode(&self, site: &str, id: &str) -> Result<Option<Episode>> {
        let like = format!("%\"id\":{}%", serde_json::to_string(id)?);
        let found: Vec<Episode> = self.db.with(|c| {
            let mut s = c.prepare("SELECT id FROM episodes WHERE data LIKE ?1")?;
            let ids: Vec<i64> = s.query_map([like], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            Ok(ids)
        })?
        .into_iter()
        .filter_map(|i| self.db.episode(i).ok().flatten())
        .collect();
        Ok(found.into_iter().find(|e| e.video.as_ref().is_some_and(|v| v.site == site && v.id == id)))
    }

    /// Put a video in line to be fetched, unless it is here or in line already. Says whether it was.
    fn queue(&self, draft: Video) -> Result<bool> {
        if self.known(&draft.site, &draft.video_id)? {
            return Ok(false);
        }
        let existing = self.video_by_key(&draft.key())?;
        let mut v = existing.clone().unwrap_or_default();
        v.url = draft.url;
        v.state = State::Queued;
        v.site = draft.site;
        v.video_id = draft.video_id;
        v.title = draft.title;
        v.channel = draft.channel;
        v.day = draft.day.or(v.day);
        v.duration = draft.duration.or(v.duration);
        v.error.clear();
        v.item = draft.item;
        match existing {
            // One that failed is asked for again only if nothing else has asked for it since.
            Some(_) => {
                if !self.save_video_if(&mut v, State::Failed)? {
                    return Ok(false);
                }
            }
            None => self.save_video(&mut v)?,
        }
        self.videos.wake.notify_one();
        Ok(true)
    }

    /// Save a video only if it still stands as `was`: two hands at it, the second finds it moved
    /// on and leaves it. Says whether it was saved.
    fn save_video_if(&self, v: &mut Video, was: State) -> Result<bool> {
        v.updated_at = crate::db::now();
        let data = serde_json::to_string(v)?;
        let n = self.db.with(|c| c.execute("UPDATE videos SET state = ?2, data = ?3 WHERE id = ?1 AND state = ?4", params![v.id, v.state.as_str(), data, was.as_str()]))?;
        Ok(n == 1)
    }

    /// Ask for what a link is: one video, or a channel, an account or a playlist, from which
    /// `take` says what to bring. `follow` keeps bringing what a channel posts.
    pub async fn videos_add(&self, url: &str, take: Take, follow: bool) -> Result<Added> {
        let url = url.trim();
        // A link out on the internet: yt-dlp is not sent to machines in this network.
        let parsed = reqwest::Url::parse(url).map_err(|_| anyhow!("that is not a link: it should start with https://"))?;
        if !crate::app::public_link(&parsed) || parsed.host_str().is_some_and(crate::api::local_name) {
            bail!("that link is not to a site on the internet");
        }
        if self.video_root().is_none() {
            bail!("there is no videos folder to file them in. Add one to a location in Settings → Library.");
        }
        let tool = self.tool()?;
        let most = match take {
            Take::All => ALL,
            _ => LOOK_AT,
        };
        match ytdlp::look(&tool, &self.cookies(), url, most).await? {
            ytdlp::Found::Video(info) => {
                let site = site_of(&info.extractor_key);
                let (_, channel, _) = info.poster();
                let link = if info.webpage_url.is_empty() { url.to_string() } else { info.webpage_url.clone() };
                let fresh = self.queue(Video { url: link, site: site.clone(), video_id: info.id.clone(), title: info.title.clone(), channel, day: info.day(), duration: info.duration, ..Default::default() })?;
                let mut out = Added { what: "video".into(), name: info.title.clone(), added: fresh as usize, already: !fresh as usize, ..Default::default() };
                if follow {
                    let _filing = self.videos.filing.lock().await;
                    out.title_id = Some(self.channel_title(&site, &info.poster(), None, true, &[])?);
                }
                Ok(out)
            }
            ytdlp::Found::List(list) => {
                let site = site_of(&list.extractor_key);
                let poster = list.poster();
                let videos = list.videos();
                let mut out = Added { what: "channel".into(), name: poster.1.clone(), listed: videos.len(), ..Default::default() };
                let chosen: Vec<&ytdlp::Entry> = match take {
                    Take::New => vec![],
                    Take::Newest => videos.iter().take(NEWEST).collect(),
                    Take::All => videos.iter().collect(),
                };
                let listing_url = if list.webpage_url.is_empty() { url.to_string() } else { list.webpage_url.clone() };
                for e in &chosen {
                    let (link, item) = entry_link(&site, &listing_url, e);
                    let day = e.upload_date.as_deref().filter(|d| d.len() == 8).map(|d| format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..]));
                    if self.queue(Video { url: link, item, site: site.clone(), video_id: e.id.clone(), title: e.title.clone().unwrap_or_default(), channel: poster.1.clone(), day, ..Default::default() })? {
                        out.added += 1;
                    } else {
                        out.already += 1;
                    }
                }
                // What was there when it was followed and not asked for is never fetched by following.
                let skip: Vec<String> = videos.iter().filter(|v| !chosen.iter().any(|c| c.id == v.id)).map(|v| v.id.clone()).collect();
                if follow || !chosen.is_empty() {
                    let _filing = self.videos.filing.lock().await;
                    let id = self.channel_title(&site, &(poster.0.clone(), poster.1.clone(), listing_url), list.avatar(), follow, &skip)?;
                    out.title_id = Some(id);
                }
                Ok(out)
            }
        }
    }

    /// The title that holds a channel's videos, made if there is none yet. `follow` turns
    /// following on; it is never turned off here.
    fn channel_title(&self, site: &str, poster: &(String, String, String), avatar: Option<String>, follow: bool, skip: &[String]) -> Result<i64> {
        let (id, name, url) = poster;
        if let Some(mut t) = self.channel_of(site, id)? {
            let c = t.channel.as_mut().expect("a channel's title");
            let mut changed = false;
            if follow && !c.follow {
                c.follow = true;
                c.skip = skip.to_vec();
                // With nothing to leave out yet, the first look notes what is there and fetches none of it.
                if skip.is_empty() {
                    c.checked_at = 0;
                }
                changed = true;
            }
            if c.url.is_empty() && !url.is_empty() {
                c.url = url.clone();
                changed = true;
            }
            if t.poster.is_none() && avatar.is_some() {
                t.poster = avatar;
                changed = true;
            }
            if changed {
                self.db.save_title(&mut t)?;
                self.emit(crate::app::Event::Title { id: t.id });
            }
            return Ok(t.id);
        }
        let root = self.video_root().ok_or_else(|| anyhow!("there is no videos folder to file them in"))?;
        let mut folder = Path::new(&root).join(file_safe(name, 120));
        let taken = |p: &Path| self.db.titles(Some(Kind::Series)).unwrap_or_default().iter().any(|t| Path::new(&t.path) == p);
        if taken(&folder) {
            folder = Path::new(&root).join(file_safe(&format!("{name} ({})", site_name(site)), 140));
        }
        let mut t = Title {
            kind: Kind::Series,
            title: name.clone(),
            year: chrono::Local::now().format("%Y").to_string().parse().unwrap_or(0),
            status: "continuing".into(),
            // Channels are never searched for on the indexers.
            monitored: false,
            profile_id: self.settings.general().default_series_profile,
            path: folder.to_string_lossy().to_string(),
            added_at: crate::db::now(),
            network: Some(site_name(site)),
            poster: avatar,
            season_folder: true,
            channel: Some(Channel { site: site.into(), id: id.clone(), url: url.clone(), follow, checked_at: if follow && skip.is_empty() { 0 } else { crate::db::now() }, skip: if follow { skip.to_vec() } else { vec![] } }),
            ..Default::default()
        };
        let tid = self.db.save_title(&mut t)?;
        self.emit(crate::app::Event::Title { id: tid });
        Ok(tid)
    }

    fn channel_of(&self, site: &str, id: &str) -> Result<Option<Title>> {
        Ok(self.db.titles(Some(Kind::Series))?.into_iter().find(|t| t.channel.as_ref().is_some_and(|c| c.site == site && c.id == id)))
    }

    /// Follow a channel, or stop.
    pub fn channel_follow(&self, title_id: i64, follow: bool) -> Result<()> {
        let mut t = self.db.title(title_id)?.ok_or_else(|| anyhow!("no such channel"))?;
        let c = t.channel.as_mut().ok_or_else(|| anyhow!("{} is not a channel", t.title))?;
        if c.follow == follow {
            return Ok(());
        }
        c.follow = follow;
        if follow {
            // What it has now was not asked for; what it posts next is.
            c.skip.clear();
            c.checked_at = 0;
        }
        self.db.save_title(&mut t)?;
        self.emit(crate::app::Event::Title { id: title_id });
        Ok(())
    }

    /// Try a video that failed again.
    pub fn video_retry(&self, id: i64) -> Result<()> {
        let mut v = self.video(id)?.ok_or_else(|| anyhow!("no such video"))?;
        if v.state != State::Failed {
            return Ok(());
        }
        v.state = State::Queued;
        v.error.clear();
        if self.save_video_if(&mut v, State::Failed)? {
            self.videos.wake.notify_one();
        }
        Ok(())
    }

    /// Forget a video asked for, stopping it if it is being fetched. What is already filed
    /// stays: it is an episode of its channel, and removed as one.
    pub fn video_remove(&self, id: i64) -> Result<()> {
        self.db.with(|c| c.execute("DELETE FROM videos WHERE id = ?1", [id]))?;
        // One being fetched is stopped, and its worker clears up after it once yt-dlp has gone.
        match self.videos.stops.lock().get(&id) {
            Some(stop) => stop.notify_one(),
            None => self.clear_staging(id),
        }
        Ok(())
    }

    /// Forget the videos that were filed or failed.
    pub fn videos_clear(&self) -> Result<usize> {
        self.db.with(|c| c.execute("DELETE FROM videos WHERE state IN ('done', 'failed')", []))
    }

    fn video_staging(&self, id: i64) -> PathBuf {
        self.staging_root().join(id.to_string())
    }

    fn staging_root(&self) -> PathBuf {
        let incomplete = self.engine.config().incomplete_dir;
        incomplete.parent().map(|p| p.to_path_buf()).unwrap_or(incomplete).join("spool-videos")
    }

    /// Whether the folder videos are fetched into holds part of the library, or is held by it:
    /// a videos folder set inside the downloads folder, say. Nothing is fetched or cleared then.
    fn staging_overlaps(&self) -> Option<String> {
        let root = self.staging_root();
        if std::fs::symlink_metadata(&root).is_ok_and(|m| m.file_type().is_symlink()) {
            return Some(format!("{} is a link to somewhere else", root.display()));
        }
        let places = self.placement();
        let folders = places.0.iter().flat_map(|(l, _)| [&l.movie_root, &l.series_root, &l.music_root, &l.audiobook_root, &l.video_root]).filter(|f| !f.is_empty()).cloned();
        let titles = self.db.titles(None).unwrap_or_default().into_iter().map(|t| t.path).filter(|p| !p.is_empty());
        for f in folders.chain(titles) {
            let f = Path::new(&f);
            if f.starts_with(&root) || root.starts_with(f) {
                return Some(format!("{} is inside the library folder {}, or holds it", root.display(), f.display()));
            }
        }
        None
    }

    /// Clear the folder one video was fetched into, when it is safely apart from the library.
    fn clear_staging(&self, id: i64) {
        let dir = self.video_staging(id);
        if let Some(why) = self.staging_overlaps() {
            tracing::warn!(dir = %dir.display(), why, "a video's download folder was left as it is");
            return;
        }
        if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    // ------------------------------------------------------------ fetching

    /// Fetch what is asked for, a few at a time, for as long as Spool runs.
    pub fn spawn_videos(&self) {
        let app = self.clone();
        tokio::spawn(async move {
            // What was being fetched when Spool stopped is fetched again from the start.
            let _ = app.db.with(|c| c.execute("UPDATE videos SET state = 'queued', data = json_set(data, '$.state', 'queued') WHERE state IN ('fetching', 'filing')", []));
            let running = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            loop {
                while running.load(std::sync::atomic::Ordering::SeqCst) < AT_ONCE && app.is_active() {
                    let Some((v, stop)) = app.claim_video() else { break };
                    running.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let (app2, running2) = (app.clone(), running.clone());
                    tokio::spawn(async move {
                        app2.fetch_video(v, stop).await;
                        running2.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                        app2.videos.wake.notify_one();
                    });
                }
                let _ = tokio::time::timeout(std::time::Duration::from_secs(60), app.videos.wake.notified()).await;
            }
        });
    }

    /// Take the next video in line: marked as being fetched in the same step as it is taken, so
    /// that nothing else takes it, and with its way of being stopped in place before that, so a
    /// removal from then on reaches it.
    fn claim_video(&self) -> Option<(Video, Arc<tokio::sync::Notify>)> {
        for _ in 0..20 {
            let mut v = self.db.with(|c| {
                use rusqlite::OptionalExtension;
                c.query_row("SELECT id, data FROM videos WHERE state = 'queued' ORDER BY id LIMIT 1", [], row).optional()
            }).ok().flatten()?;
            let stop = Arc::new(tokio::sync::Notify::new());
            if self.videos.stops.lock().insert(v.id, stop.clone()).is_some() {
                // Still being stopped from an earlier turn: its worker has yet to finish.
                return None;
            }
            v.state = State::Fetching;
            if self.save_video_if(&mut v, State::Queued).unwrap_or(false) {
                return Some((v, stop));
            }
            self.videos.stops.lock().remove(&v.id);
        }
        None
    }

    async fn fetch_video(&self, mut v: Video, stop: Arc<tokio::sync::Notify>) {
        let id = v.id;
        let result = self.fetch_and_file(&mut v, &stop).await;
        self.videos.progress.lock().remove(&id);
        // Removed while it was being fetched: nothing is left of it.
        if self.video(id).ok().flatten().is_none() {
            self.clear_staging(id);
            self.videos.stops.lock().remove(&id);
            return;
        }
        match result {
            Ok(()) => {
                v.state = State::Done;
                v.error.clear();
                let _ = self.db.add_history(v.title_id, "video_filed", serde_json::json!({"video": v.title, "channel": v.channel, "url": v.url}));
                self.clear_staging(id);
            }
            Err(e) => {
                v.state = State::Failed;
                v.error = format!("{e:#}");
                tracing::warn!(video = %v.url, error = %v.error, "a video could not be fetched");
                let _ = self.db.add_history(v.title_id, "video_failed", serde_json::json!({"video": if v.title.is_empty() { &v.url } else { &v.title }, "error": v.error}));
            }
        }
        let _ = self.save_video(&mut v);
        self.videos.stops.lock().remove(&id);
        if let Some(t) = v.title_id {
            self.emit(crate::app::Event::Title { id: t });
        }
    }

    async fn fetch_and_file(&self, v: &mut Video, stop: &tokio::sync::Notify) -> Result<()> {
        let tool = self.tool()?;
        if let Some(why) = self.staging_overlaps() {
            bail!("videos are not fetched while {why}");
        }
        let dir = self.video_staging(v.id);
        self.clear_staging(v.id);
        let languages = self.caption_languages();
        let id = v.id;
        let progress = |p: ytdlp::Progress| {
            self.videos.progress.lock().insert(id, p);
        };
        ytdlp::fetch(&tool, &self.cookies(), &v.url, v.item, &dir, &languages, progress, stop).await?;
        // Removed while it came: it is not filed.
        if self.video(v.id)?.is_none() {
            bail!("removed");
        }
        let info: ytdlp::Info = serde_json::from_slice(&std::fs::read(dir.join("video.info.json")).context("yt-dlp left no notes on the video")?)?;
        v.state = State::Filing;
        v.title = info.title.clone();
        v.channel = info.poster().1;
        v.day = info.day();
        v.duration = info.duration;
        self.save_video(v)?;
        let (title_id, episode_id) = self.file_video(&info, &dir, &languages).await?;
        v.title_id = Some(title_id);
        v.episode_id = Some(episode_id);
        Ok(())
    }

    /// The languages captions are kept in: those subtitles are wanted in, or the household's.
    fn caption_languages(&self) -> Vec<String> {
        let g = self.settings.general();
        let mut langs = crate::subs::wanted_languages(&g.subtitle_languages);
        if langs.is_empty() {
            langs.extend(crate::subs::language_code(&g.language).map(str::to_string));
        }
        if langs.is_empty() {
            langs.push("en".into());
        }
        langs
    }

    // ------------------------------------------------------------ filing

    /// File a fetched video under its channel: the video through the import journal, its
    /// captions and picture beside it. Gives the channel's title and the video's episode.
    async fn file_video(&self, info: &ytdlp::Info, dir: &Path, languages: &[String]) -> Result<(i64, i64)> {
        let site = site_of(&info.extractor_key);
        let poster = info.poster();
        let video = std::fs::read_dir(dir)?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.file_stem().is_some_and(|s| s == "video") && p.extension().is_some_and(|e| matches!(e.to_str(), Some("mkv" | "mp4" | "webm" | "mov" | "m4v"))))
            .ok_or_else(|| anyhow!("yt-dlp finished but left no video"))?;
        let ext = video.extension().and_then(|e| e.to_str()).unwrap_or("mkv").to_string();
        // One filing at a time, and none into a channel being moved to another location.
        let _filing = self.videos.filing.lock().await;
        let title_id = self.channel_title(&site, &poster, None, false, &[])?;
        let turn = self.title_imports.lock().entry(title_id).or_default().clone();
        let _turn = turn.lock().await;
        let mut title = self.db.title(title_id)?.ok_or_else(|| anyhow!("the channel's title went"))?;
        if let Some(why) = self.title_locked(&title) {
            bail!("{why}");
        }

        // The episode it is: the one it was before, if it was fetched before.
        let day = info.day().unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
        let season: u32 = day[..4].parse().unwrap_or(0);
        let episodes = self.db.episodes(title_id)?;
        let mut ep = match episodes.iter().find(|e| e.video.as_ref().is_some_and(|r| r.site == site && r.id == info.id)) {
            Some(e) => e.clone(),
            None => {
                let mut nth = 1;
                while episodes.iter().any(|e| e.season == season && e.episode == number_for(&day, nth)) && nth < 99 {
                    nth += 1;
                }
                let blank: Episode = serde_json::from_value(serde_json::json!({"title_id": title_id, "season": season, "episode": number_for(&day, nth)}))?;
                blank
            }
        };
        ep.title = if info.title.trim().is_empty() { format!("{} {day}", poster.1) } else { info.title.trim().to_string() };
        ep.overview = info.description.clone().unwrap_or_default();
        ep.air_date = Some(day.clone());
        ep.runtime = info.duration.map(|d| (d / 60.0).ceil() as u32).unwrap_or(0);
        ep.video = Some(VideoRef { site: site.clone(), id: info.id.clone(), url: info.webpage_url.clone() });
        let episode_id = self.db.upsert_episode(&ep)?;
        if !title.seasons.iter().any(|s| s.number == season) {
            title.seasons.push(SeasonInfo { number: season, monitored: true });
            title.seasons.sort_by_key(|s| s.number);
        }
        if title.first_aired.as_deref().is_none_or(|f| f > day.as_str()) {
            title.first_aired = Some(day.clone());
            title.year = season;
        }
        self.db.save_title(&mut title)?;

        let rel = rel_path(&self.settings.naming(), &title, &ep, &ext);
        let stem = Path::new(&rel).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let dest_dir = Path::new(&title.path).join(Path::new(&rel).parent().unwrap_or(Path::new("")));

        // Captions and the picture go first, so the video is found with them.
        for (lang, text) in captions::choose(dir, info, languages) {
            let p = dir.join(format!("{stem}.{lang}.srt"));
            std::fs::write(&p, text)?;
            self.file_move(&p, &dest_dir.join(format!("{stem}.{lang}.srt"))).or_else(|e| if dest_dir.join(format!("{stem}.{lang}.srt")).exists() { Ok(()) } else { Err(e) })?;
        }
        // A copy, as the picture may be the channel's too.
        let thumb = dir.join("video.jpg");
        let to = dest_dir.join(format!("{stem}-thumb.jpg"));
        if thumb.is_file() && !to.exists() {
            let copy = dir.join("thumb.jpg");
            std::fs::copy(&thumb, &copy)?;
            self.file_move(&copy, &to)?;
        }

        let size = std::fs::metadata(&video)?.len();
        // A video that does not play is not filed: a fetch cut short leaves one.
        let media_info = self.probe(&video, &stem).await.context("what was fetched does not play as a video")?;
        let height = media_info.as_ref().map(|m| m.height).filter(|h| *h > 0).or(info.height).unwrap_or(0);
        let file = MediaFile {
            id: 0,
            title_id,
            rel_path: rel.clone(),
            size,
            quality: spool_core::quality::parse_quality(&format!("{stem} {height}p WEB-DL"), spool_core::quality::Flavor::Tv),
            release_group: None,
            edition: String::new(),
            media_info,
            scene_name: Some(stem.clone()),
            languages: vec![],
            added_at: crate::db::now(),
            extra: false,
            subtitles: vec![],
            base: None,
            compact_of: None,
        };
        // Fetched again: the copy filed before is replaced, as an upgrade replaces a file.
        let replaces: Vec<(i64, String, String)> = ep
            .file_id
            .and_then(|fid| self.db.file(fid).ok().flatten())
            .map(|f| {
                let old = Path::new(&title.path).join(&f.rel_path);
                (f.id, old.to_string_lossy().to_string(), self.recycle_path(&title, &old).to_string_lossy().to_string())
            })
            .into_iter()
            .collect();
        let op = crate::import::Operation {
            acquisition_id: None,
            title_id,
            episode_ids: vec![episode_id],
            src: video.to_string_lossy().to_string(),
            dst: Path::new(&title.path).join(&rel).to_string_lossy().to_string(),
            replaces,
            file,
        };
        self.file_operation(op).await?;
        if let Some(f) = self.db.files(title_id)?.into_iter().find(|f| f.rel_path == rel) {
            self.compact_arrivals(title_id, &[f.id]);
            if let Ok(found) = self.find_sidecars(f.id) {
                if let Some(mut f) = self.db.file(f.id)? {
                    f.subtitles = found.into_iter().map(|s| crate::subs::Sidecar { source: "download".into(), release: info.webpage_url.clone(), ..s }).collect();
                    self.db.save_file(&f)?;
                }
            }
        }
        self.channel_poster(&title, dir).await;
        let _ = self.survey_title(title_id).await;
        self.emit(crate::app::Event::Title { id: title_id });
        Ok((title_id, episode_id))
    }

    /// Give a channel a picture of its own in its folder, once: its avatar where the site has
    /// one, or else the picture of the video just filed.
    async fn channel_poster(&self, title: &Title, dir: &Path) {
        let folder = Path::new(&title.path);
        if title.local.art.iter().any(|a| a.of.is_none() && a.kind == crate::local::ArtKind::Poster) || folder.join("poster.jpg").exists() {
            return;
        }
        let mut avatar = title.poster.clone();
        if avatar.is_none() {
            if let (Ok(tool), Some(c)) = (self.tool(), title.channel.as_ref().filter(|c| !c.url.is_empty())) {
                if let Ok(ytdlp::Found::List(l)) = ytdlp::look(&tool, &self.cookies(), &c.url, 1).await {
                    avatar = l.avatar();
                }
            }
        }
        if let Some(url) = avatar {
            if let Ok(r) = self.public_http.get(&url).timeout(std::time::Duration::from_secs(30)).send().await.and_then(|r| r.error_for_status()) {
                if let Ok(bytes) = r.bytes().await {
                    let tmp = dir.join("poster.jpg");
                    if std::fs::write(&tmp, &bytes).is_ok() && self.file_move(&tmp, &folder.join("poster.jpg")).is_ok() {
                        return;
                    }
                }
            }
        }
        // No picture of the channel's own: the first video's serves.
        let thumb = dir.join("video.jpg");
        if thumb.is_file() {
            let tmp = dir.join("poster-from-video.jpg");
            if std::fs::copy(&thumb, &tmp).is_ok() {
                let _ = self.file_move(&tmp, &folder.join("poster.jpg"));
            }
        }
    }

    // ------------------------------------------------------------ following

    /// Look at each followed channel for what it has posted since, and ask for it.
    pub async fn videos_task(&self) -> Result<String> {
        let Ok(tool) = self.tool() else { return Ok("yt-dlp is not installed".into()) };
        let channels: Vec<Title> = self.db.titles(Some(Kind::Series))?.into_iter().filter(|t| t.channel.as_ref().is_some_and(|c| c.follow && !c.url.is_empty())).collect();
        let (mut looked, mut added, mut failed) = (0, 0, 0);
        for t in channels {
            let c = t.channel.clone().expect("a channel");
            looked += 1;
            match ytdlp::look(&tool, &self.cookies(), &c.url, LOOK_AT).await {
                Ok(ytdlp::Found::List(list)) => {
                    let first_look = c.checked_at == 0 && c.skip.is_empty();
                    let videos = list.videos();
                    let mut skip = c.skip.clone();
                    for e in &videos {
                        if c.skip.contains(&e.id) {
                            continue;
                        }
                        // Followed by hand from its title: what was there then is not fetched.
                        if first_look {
                            skip.push(e.id.clone());
                            continue;
                        }
                        let (link, item) = entry_link(&c.site, &c.url, e);
                        if self.queue(Video { url: link, item, site: c.site.clone(), video_id: e.id.clone(), title: e.title.clone().unwrap_or_default(), channel: t.title.clone(), ..Default::default() })? {
                            added += 1;
                        }
                    }
                    // Only what is still listed is worth remembering.
                    skip.retain(|s| videos.iter().any(|v| &v.id == s) || c.skip.len() < 200);
                    if let Some(mut fresh) = self.db.title(t.id)? {
                        if let Some(fc) = fresh.channel.as_mut() {
                            fc.checked_at = crate::db::now();
                            fc.skip = skip;
                        }
                        self.db.save_title(&mut fresh)?;
                    }
                }
                Ok(ytdlp::Found::Video(_)) => {}
                Err(e) => {
                    failed += 1;
                    tracing::warn!(channel = %t.title, error = %format!("{e:#}"), "a followed channel could not be looked at");
                }
            }
        }
        Ok(format!("{looked} followed {}, {added} new {}{}", if looked == 1 { "channel" } else { "channels" }, if added == 1 { "video" } else { "videos" }, if failed > 0 { format!(", {failed} could not be looked at") } else { String::new() }))
    }
}

/// Where to fetch one entry of a listing from: its own page when the listing points to it, or
/// else the listing itself and which of its videos it is. A post on X with several videos lists
/// them already read, each with the address of the file itself, which says nothing of whose
/// it is; fetched from the post, it is known.
fn entry_link(site: &str, listing: &str, e: &ytdlp::Entry) -> (String, Option<u32>) {
    match (e.kind.as_deref(), e.url.as_deref()) {
        (Some("url" | "url_transparent"), Some(u)) if u.starts_with("http") => (u.to_string(), None),
        _ if e.index > 0 && !listing.is_empty() => (listing.to_string(), Some(e.index)),
        _ => (watch_url(site, &e.id), None),
    }
}

/// A video's own page, for a listing that names only its id.
fn watch_url(site: &str, id: &str) -> String {
    match site {
        "youtube" => format!("https://www.youtube.com/watch?v={id}"),
        _ => id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_names() {
        assert_eq!(number_for("2026-11-14", 1), 111401);
        assert_eq!(number_for("2026-01-02", 3), 10203);
        assert!(number_for("2026-12-31", 1) > number_for("2026-02-01", 9));
        assert_eq!(file_safe("What: is \"this\"? / a test...", 100), "What is this a test");
        assert_eq!(file_safe("ééééé", 5), "éé");
        assert_eq!(file_safe("...", 10), "Untitled");
        assert_eq!(site_of("YoutubeTab"), "youtube");
        assert_eq!(site_of("Twitter"), "x");
        assert_eq!(site_name("x"), "X");
        assert_eq!(site_name("peertube"), "Peertube");
    }
}
