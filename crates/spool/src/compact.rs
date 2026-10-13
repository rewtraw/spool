//! Compact versions: a smaller copy of a film or an episode, made here and kept beside it.
//!
//! The original is never touched. A compact version is one more file of the title, marked
//! with the file it was made from; players are offered it as a version, and upgrades,
//! searches and the library's sums go on treating the original as the title's file.
//!
//! One is made at a time. The video is re-encoded to HEVC at a size and rate the settings
//! give, on the machine's video hardware where it has any; sound that is already small is
//! copied and the rest becomes Dolby Digital Plus; subtitles, chapters and fonts are carried
//! over as they are. The work is done under a hidden name and only given its own once it
//! has been read back and found to run as long as the original.
//!
//! The line of files waiting is written down, so a restart costs the one being made and no
//! more.

use crate::app::{App, Event};
use crate::models::*;
use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use spool_core::mediainfo::MediaInfo;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// How compact versions are made. Part of the general settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Compact {
    /// The picture is made to fit a screen of this height: 1080 fits within 1920 by 1080.
    pub height: u32,
    /// Kilobits a second for the picture.
    pub video_kbps: u32,
    /// Kilobits a second for surround sound that has to be converted.
    pub audio_kbps: u32,
    /// Where compact versions of titles on a location Spool only reads are put; zero for
    /// nowhere, in which case those titles get none.
    pub location: u32,
    /// Make one for whatever arrives, when it would be worth it.
    pub auto: bool,
    /// Use the processor when the video hardware cannot do a file, or there is none.
    pub software: bool,
}

impl Default for Compact {
    fn default() -> Self {
        Compact { height: 1080, video_kbps: 8000, audio_kbps: 640, location: 0, auto: false, software: true }
    }
}

/// The files waiting for a compact version, and the one being made.
#[derive(Default)]
pub struct Encodes {
    pub queue: VecDeque<i64>,
    pub now: Option<Encoding>,
    pub working: bool,
}

#[derive(Clone)]
pub struct Encoding {
    pub file_id: i64,
    pub title_id: i64,
    /// How much of the running time is done, and the whole of it, in thousandths of a second.
    pub done_ms: Arc<AtomicU64>,
    pub total_ms: u64,
    /// How many times faster than it plays, times a hundred.
    pub speed: Arc<AtomicU64>,
    pub stop: Arc<AtomicBool>,
    /// "hardware" or "software", once it is known.
    pub how: Arc<Mutex<String>>,
}

/// What came of asking for compact versions.
#[derive(Default, Serialize)]
pub struct Asked {
    pub queued: usize,
    pub refused: Vec<Refused>,
}

#[derive(Serialize)]
pub struct Refused {
    pub title_id: i64,
    pub title: String,
    pub why: String,
}

/// What is to be made from one file, and where it will go.
struct Plan {
    src: PathBuf,
    /// The folder it goes in, and that folder as the record keeps it: None for the title's own.
    folder: PathBuf,
    base: Option<String>,
    rel_path: String,
    secs: f64,
    /// Roughly how big it will be.
    bytes: u64,
    info: MediaInfo,
}

const LINE: &str = "compact_waiting";
const CALLED_OFF: &str = "the compact version was cancelled";
/// A file must be at least this many times the size it would become to be worth doing.
const WORTH_IT: f64 = 1.5;

#[derive(Clone, Copy, PartialEq, Debug)]
enum How {
    /// Read, resized and written by the video hardware, never leaving it.
    Hardware,
    /// Read and resized by the processor, written by the video hardware.
    HardwareWrite,
    Software,
}

/// The words on the end of a compact version's name, which is how one is known for what it
/// is when a folder is read: "Film (1999) - Compact 1080p.mkv".
pub fn compact_suffix(height: u32) -> String {
    format!(" - Compact {height}p")
}

/// The name a compact version was made from, if this is the name of one.
pub fn compact_source_stem(stem: &str) -> Option<&str> {
    let (before, after) = stem.rsplit_once(" - Compact")?;
    let after = after.trim();
    (after.is_empty() || after.strip_suffix('p').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))).then_some(before)
}

fn stem_of(rel: &str) -> String {
    Path::new(rel).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

/// The picture size to ask for, to fit a screen `height` high: None when it fits already.
fn fit(width: u32, height: u32, target: u32) -> Option<(i64, i64)> {
    let wide = (target as u64 * 16 / 9) as u32;
    if width == 0 || height == 0 {
        return None;
    }
    // Wider than the screen is shaped: it is the width that has to come down.
    if width as u64 * 9 >= height as u64 * 16 {
        (width > wide).then_some((wide as i64, -2))
    } else {
        (height > target).then_some((-2, target as i64))
    }
}

/// The screen a picture is made for, as a label has it: a wide film at 1920 by 1038 is 1080p.
fn screen(width: u32, height: u32) -> u32 {
    [480, 720, 1080].into_iter().find(|&h| height <= h && width as u64 * 9 <= h as u64 * 16).unwrap_or(2160)
}

fn bits_a_second(info: &MediaInfo, size: u64) -> f64 {
    if info.bit_rate > 0 {
        info.bit_rate as f64
    } else if info.runtime_secs > 0.0 {
        size as f64 * 8.0 / info.runtime_secs
    } else {
        0.0
    }
}

/// What to hand ffmpeg to make a compact version of a file one way.
fn arguments(c: &Compact, info: &MediaInfo, src: &Path, out: &Path, how: How) -> Vec<String> {
    let mut a: Vec<String> = ["-nostdin", "-hide_banner", "-loglevel", "error", "-nostats", "-progress", "pipe:1", "-y"].iter().map(|s| s.to_string()).collect();
    let mut add = |items: &[&str]| a.extend(items.iter().map(|s| s.to_string()));
    if how == How::Hardware {
        add(&["-hwaccel", "videotoolbox", "-hwaccel_output_format", "videotoolbox_vld"]);
    }
    add(&["-i", &src.to_string_lossy()]);
    add(&["-map", "0:v:0"]);

    // Sound and subtitles are taken stream by stream when the file's streams are on record,
    // so that each can be treated as it needs.
    let audio: Vec<&spool_core::mediainfo::Stream> = info.streams.iter().filter(|s| s.kind == "audio").collect();
    let subs: Vec<&spool_core::mediainfo::Stream> = info.streams.iter().filter(|s| s.kind == "subtitle").collect();
    let mut codecs: Vec<String> = vec![];
    if audio.is_empty() {
        add(&["-map", "0:a?"]);
        codecs.extend(["-c:a".to_string(), "eac3".into(), "-b:a".into(), format!("{}k", c.audio_kbps), "-ac".into(), "6".into()]);
    }
    for (n, s) in audio.iter().enumerate() {
        add(&["-map", &format!("0:{}", s.index)]);
        if matches!(s.codec.as_str(), "aac" | "ac3" | "eac3") {
            codecs.extend([format!("-c:a:{n}"), "copy".into()]);
        } else if s.channels >= 6 {
            // Dolby Digital Plus carries six channels; more are folded into them.
            codecs.extend([format!("-c:a:{n}"), "eac3".into(), format!("-b:a:{n}"), format!("{}k", c.audio_kbps), format!("-ac:a:{n}"), "6".into()]);
        } else {
            codecs.extend([format!("-c:a:{n}"), "eac3".into(), format!("-b:a:{n}"), "224k".into()]);
        }
    }
    let mut kept = 0;
    for s in &subs {
        match s.codec.as_str() {
            // Kinds a Matroska file cannot hold.
            "eia_608" | "dvb_teletext" | "ttml" => continue,
            "mov_text" | "tx3g" => codecs.extend([format!("-c:s:{kept}"), "srt".into()]),
            _ => codecs.extend([format!("-c:s:{kept}"), "copy".into()]),
        }
        add(&["-map", &format!("0:{}", s.index)]);
        kept += 1;
    }
    if info.streams.is_empty() {
        add(&["-map", "0:s?", "-c:s", "copy"]);
    }
    // Fonts the subtitles are drawn in.
    add(&["-map", "0:t?", "-c:t", "copy"]);

    let size = fit(info.width, info.height, c.height);
    let ten = info.bit_depth >= 10;
    let rate = format!("{}k", c.video_kbps);
    match how {
        How::Hardware => {
            if let Some((w, h)) = size {
                add(&["-vf", &format!("scale_vt=w={w}:h={h}")]);
            }
            add(&["-c:v", "hevc_videotoolbox", "-profile:v", if ten { "main10" } else { "main" }, "-b:v", &rate, "-prio_speed", "1"]);
        }
        How::HardwareWrite => {
            if let Some((w, h)) = size {
                add(&["-vf", &format!("scale={w}:{h}")]);
            }
            add(&["-c:v", "hevc_videotoolbox", "-profile:v", if ten { "main10" } else { "main" }, "-b:v", &rate, "-prio_speed", "1"]);
        }
        How::Software => {
            if let Some((w, h)) = size {
                add(&["-vf", &format!("scale={w}:{h}")]);
            }
            add(&["-c:v", "libx265", "-preset", "fast", "-b:v", &rate, "-pix_fmt", if ten { "yuv420p10le" } else { "yuv420p" }, "-x265-params", "log-level=error"]);
        }
    }
    a.extend(codecs);
    let mut add = |items: &[&str]| a.extend(items.iter().map(|s| s.to_string()));
    add(&["-map_metadata", "0", "-map_chapters", "0", "-max_muxing_queue_size", "4096", "-f", "matroska", &out.to_string_lossy()]);
    a
}

impl App {
    /// Which ways of encoding this machine's ffmpeg offers, best first.
    fn compact_ways(&self) -> Vec<How> {
        static HARDWARE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let hardware = *HARDWARE.get_or_init(|| {
            self.ffmpeg_path().and_then(|tool| std::process::Command::new(tool).args(["-hide_banner", "-encoders"]).output().ok()).is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains("hevc_videotoolbox"))
        });
        let mut ways = if hardware { vec![How::Hardware, How::HardwareWrite] } else { vec![] };
        if self.settings.general().compact.software || !hardware {
            ways.push(How::Software);
        }
        ways
    }

    /// What this machine makes compact versions with, in words.
    pub fn compact_means(&self) -> &'static str {
        match (self.ffmpeg_path().is_some(), self.compact_ways().first()) {
            (false, _) => "ffmpeg is not installed, so none can be made",
            (true, Some(How::Hardware | How::HardwareWrite)) => "this machine's video hardware (VideoToolbox)",
            _ => "the processor, which is slow: this machine's ffmpeg offers no video hardware",
        }
    }

    /// The compact versions made from a file.
    pub fn compacts_of(&self, file_id: i64, files: &[MediaFile]) -> Vec<MediaFile> {
        files.iter().filter(|f| f.compact_of == Some(file_id)).cloned().collect()
    }

    /// The files of a title that a compact version could be made from: the film, or each
    /// episode's file. Versions kept beside a film, and compact versions, are not.
    pub fn compact_sources(&self, title: &Title) -> Result<Vec<MediaFile>> {
        let files = self.db.files(title.id)?;
        Ok(match title.kind {
            Kind::Movie => {
                let plain: Vec<&MediaFile> = files.iter().filter(|f| f.compact_of.is_none()).collect();
                let main: Vec<&MediaFile> = plain.iter().copied().filter(|f| !f.extra).collect();
                // A film held only as copies on other locations has no main file; the largest stands in.
                if main.is_empty() { plain.into_iter().max_by_key(|f| f.size).into_iter().cloned().collect() } else { main.into_iter().cloned().collect() }
            }
            Kind::Series => {
                let held: std::collections::HashSet<i64> = self.db.episodes(title.id)?.iter().filter_map(|e| e.file_id).collect();
                files.into_iter().filter(|f| held.contains(&f.id) && f.compact_of.is_none()).collect()
            }
        })
    }

    /// Work out what a compact version of a file would be and where it would go, or why
    /// there is to be none.
    fn compact_plan(&self, title: &Title, file: &MediaFile) -> Result<Plan> {
        let c = self.settings.general().compact;
        if file.compact_of.is_some() {
            bail!("this is a compact version already");
        }
        if self.db.files(title.id)?.iter().any(|f| f.compact_of == Some(file.id)) {
            bail!("it already has a compact version");
        }
        let Some(info) = file.media_info.clone() else { bail!("its details have not been read, so it is not known what is in it") };
        if self.ffmpeg_path().is_none() {
            bail!("ffmpeg is not installed");
        }
        let places = self.placement();
        let here = title.folder_of(file).to_string();
        if !places.online_at(&here) {
            bail!("it is on a location that cannot be reached right now");
        }
        // Dolby Vision with nothing else under it cannot be re-encoded without the colours
        // coming out wrong; with an HDR10 or SDR layer, that layer is what is kept.
        if info.dynamic_range_type == "DV" {
            bail!("it is Dolby Vision with no HDR10 layer, which cannot be made smaller without spoiling its colours");
        }
        let wanted = (c.video_kbps + c.audio_kbps) as f64 * 1000.0;
        let has = bits_a_second(&info, file.size);
        if has > 0.0 && has < wanted * WORTH_IT {
            bail!("it is small already: {:.1} Mb/s, where a compact version would be {:.1}", has / 1e6, wanted / 1e6);
        }
        let secs = info.runtime_secs;
        if secs <= 0.0 {
            bail!("how long it runs is not known");
        }
        // Beside the original where Spool may write; else on the location chosen for them.
        let managed = |folder: &str| places.of(folder).map(|p| p.0.manage).unwrap_or(true);
        let name = format!("{}{}.mkv", stem_of(&file.rel_path), compact_suffix(c.height));
        let within = Path::new(&file.rel_path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let rel_path = within.join(&name).to_string_lossy().to_string();
        let (folder, base) = if managed(&here) {
            (PathBuf::from(&here), file.base.clone())
        } else {
            let from = places.of(&here).map(|p| p.0.name.clone()).unwrap_or_default();
            let Some((to, online)) = places.0.iter().find(|p| p.0.id == c.location && c.location != 0) else {
                bail!("it is on {from}, which Spool only reads, and no location is chosen for compact versions of such titles");
            };
            let root = to.root(title.kind);
            if !to.manage || root.is_empty() {
                bail!("{} cannot take compact versions: it is only read, or has no folder for {}", to.name, if title.kind == Kind::Movie { "films" } else { "series" });
            }
            if !online {
                bail!("{} cannot be reached right now", to.name);
            }
            let folder = Path::new(root).join(Path::new(&title.path).file_name().ok_or_else(|| anyhow!("the title has no folder name"))?);
            let base = Some(folder.to_string_lossy().to_string()).filter(|f| *f != title.path);
            (folder, base)
        };
        if folder.join(&rel_path).exists() {
            bail!("a file of that name is already there: {}", folder.join(&rel_path).display());
        }
        let bytes = (wanted / 8.0 * secs * 1.1) as u64;
        if let Some(free) = crate::app::free_space(&folder) {
            if free < bytes + (2 << 30) {
                bail!("there is no room for it: about {:.1} GB is needed and {:.1} GB is free", bytes as f64 / 1e9, free as f64 / 1e9);
            }
        }
        Ok(Plan { src: title.file_path(file), folder, base, rel_path, secs, bytes, info })
    }

    /// Why a file can have no compact version, or roughly how big one would be.
    pub fn compact_estimate(&self, title: &Title, file: &MediaFile) -> std::result::Result<u64, String> {
        self.compact_plan(title, file).map(|p| p.bytes).map_err(|e| format!("{e:#}"))
    }

    fn keep_compact_line(&self) {
        let line: Vec<i64> = {
            let e = self.encodes.lock();
            e.now.iter().map(|n| n.file_id).chain(e.queue.iter().copied()).collect()
        };
        let _ = self.db.set_setting(LINE, &line);
    }

    /// Take up the line a restart interrupted, after clearing away what the interrupted one
    /// had written.
    pub fn resume_compacts(&self) -> usize {
        let line: Vec<i64> = self.db.get_setting(LINE);
        if line.is_empty() || self.encodes.lock().working {
            return 0;
        }
        let asked = self.compact_files(&line, false);
        self.keep_compact_line();
        if asked.queued > 0 {
            tracing::info!(resumed = asked.queued, "took up the compact versions interrupted by the last shutdown");
        }
        asked.queued
    }

    /// Put every file of these titles that would gain from a compact version in line.
    pub fn compact_titles(&self, ids: &[i64]) -> Result<Asked> {
        let mut files = vec![];
        let mut all = Asked::default();
        for id in ids {
            let Some(title) = self.db.title(*id)? else { continue };
            let sources = self.compact_sources(&title)?;
            if sources.is_empty() {
                all.refused.push(Refused { title_id: title.id, title: title.title.clone(), why: "it has no file".into() });
            }
            files.extend(sources.into_iter().map(|f| f.id));
        }
        let asked = self.compact_files(&files, ids.len() == 1);
        all.queued = asked.queued;
        all.refused.extend(asked.refused);
        Ok(all)
    }

    /// Put files in line for a compact version, and start on them if none is being made.
    /// Each is checked first. `say_small` is whether to report the files that are small
    /// already, which for a whole library is most of them and not worth the saying.
    pub fn compact_files(&self, ids: &[i64], say_small: bool) -> Asked {
        let mut asked = Asked::default();
        for id in ids {
            let Ok(Some(file)) = self.db.file(*id) else { continue };
            let Ok(Some(title)) = self.db.title(file.title_id) else { continue };
            let (waiting, making) = {
                let e = self.encodes.lock();
                (e.queue.contains(id), e.now.as_ref().is_some_and(|n| n.file_id == *id))
            };
            if waiting || making {
                continue;
            }
            match self.compact_plan(&title, &file) {
                Ok(_) => {
                    self.encodes.lock().queue.push_back(*id);
                    asked.queued += 1;
                }
                Err(e) => {
                    let why = format!("{e:#}");
                    if say_small || !(why.starts_with("it is small already") || why.starts_with("it already has")) {
                        asked.refused.push(Refused { title_id: title.id, title: title.title.clone(), why });
                    }
                }
            }
        }
        self.keep_compact_line();
        let start = !std::mem::replace(&mut self.encodes.lock().working, true);
        if start {
            let app = self.clone();
            tokio::spawn(async move {
                loop {
                    let id = {
                        let mut e = app.encodes.lock();
                        match e.queue.pop_front() {
                            Some(id) => {
                                e.now = Some(Encoding { file_id: id, title_id: 0, done_ms: Default::default(), total_ms: 0, speed: Default::default(), stop: Default::default(), how: Default::default() });
                                id
                            }
                            None => {
                                e.working = false;
                                break;
                            }
                        }
                    };
                    let (title_id, name) = app.db.file(id).ok().flatten().and_then(|f| app.db.title(f.title_id).ok().flatten()).map(|t| (t.id, t.title)).unwrap_or_default();
                    match app.compact_one(id).await {
                        Ok(said) => {
                            tracing::info!(title = %name, "{said}");
                            app.emit(Event::Task { name: "compact".into(), running: false, message: said });
                        }
                        Err(e) if e.to_string() == CALLED_OFF => tracing::info!(title = %name, "a compact version was cancelled"),
                        Err(e) => {
                            tracing::warn!(title = %name, error = %format!("{e:#}"), "could not make a compact version");
                            let _ = app.db.add_history(Some(title_id).filter(|t| *t != 0), "compact_failed", json!({"reason": format!("{e:#}")}));
                            let _ = app.db.add_attention("config", Some(title_id).filter(|t| *t != 0), None, &format!("A compact version of {name} could not be made: {e:#}. The original is untouched."), json!({}));
                            app.emit(Event::Attention);
                        }
                    }
                    app.encodes.lock().now = None;
                    app.keep_compact_line();
                    app.emit(Event::Title { id: title_id });
                }
            });
        }
        asked
    }

    /// Call compact versions off: the files named, waiting or being made; or with none
    /// named, all that wait. Says how many were taken out of the line, and whether the one
    /// being made was told to stop.
    pub fn cancel_compacts(&self, ids: &[i64]) -> (usize, bool) {
        let (taken, stopping) = {
            let mut e = self.encodes.lock();
            let before = e.queue.len();
            e.queue.retain(|id| !(ids.is_empty() || ids.contains(id)));
            let stopping = e.now.as_ref().is_some_and(|n| ids.contains(&n.file_id));
            if let Some(n) = e.now.as_ref().filter(|_| stopping) {
                n.stop.store(true, Ordering::Relaxed);
            }
            (before - e.queue.len(), stopping)
        };
        self.keep_compact_line();
        self.emit(Event::Title { id: 0 });
        (taken, stopping)
    }

    /// Whether a compact version of this file is being made or waiting to be.
    pub fn compacting(&self, file_id: i64) -> bool {
        let e = self.encodes.lock();
        e.queue.contains(&file_id) || e.now.as_ref().is_some_and(|n| n.file_id == file_id)
    }

    /// The one being made and those waiting, as Activity shows them.
    pub fn compacts_shown(&self) -> serde_json::Value {
        let (now, waiting) = {
            let e = self.encodes.lock();
            (e.now.clone(), e.queue.iter().copied().collect::<Vec<_>>())
        };
        if now.is_none() && waiting.is_empty() {
            return json!({"now": null, "waiting": [], "waiting_count": 0});
        }
        let shown = |file_id: i64| {
            let file = self.db.file(file_id).ok().flatten();
            let t = file.as_ref().and_then(|f| self.db.title(f.title_id).ok().flatten());
            // An episode is named by its number, since a series has many files.
            let episode = t.as_ref().filter(|t| t.kind == Kind::Series).and_then(|t| self.db.episodes(t.id).ok()).and_then(|eps| {
                let mine: Vec<String> = eps.iter().filter(|e| e.file_id == Some(file_id)).map(|e| format!("S{:02}E{:02}", e.season, e.episode)).collect();
                (!mine.is_empty()).then(|| mine.join(", "))
            });
            json!({
                "file_id": file_id,
                "title_id": t.as_ref().map(|t| t.id),
                "title": t.as_ref().map(|t| t.title.clone()),
                "episode": episode,
                "size": file.as_ref().map(|f| f.size),
            })
        };
        let now = now.map(|n| {
            let mut v = shown(n.file_id);
            v["done_ms"] = n.done_ms.load(Ordering::Relaxed).min(n.total_ms).into();
            v["total_ms"] = n.total_ms.into();
            v["speed"] = (n.speed.load(Ordering::Relaxed) as f64 / 100.0).into();
            v["how"] = n.how.lock().clone().into();
            v["stopping"] = n.stop.load(Ordering::Relaxed).into();
            v
        });
        // A whole library in line is hundreds of files; the first of them say enough.
        let listed: Vec<serde_json::Value> = waiting.iter().take(50).map(|id| shown(*id)).collect();
        json!({"now": now, "waiting": listed, "waiting_count": waiting.len()})
    }

    async fn compact_one(&self, file_id: i64) -> Result<String> {
        let file = self.db.file(file_id)?.ok_or_else(|| anyhow!("the file is no longer in the library"))?;
        let title = self.db.title(file.title_id)?.ok_or_else(|| anyhow!("the title was removed"))?;
        let plan = self.compact_plan(&title, &file)?;
        let c = self.settings.general().compact;
        let tool = self.ffmpeg_path().ok_or_else(|| anyhow!("ffmpeg is not installed"))?;
        let (done, speed, stop, how) = {
            let mut e = self.encodes.lock();
            let was = e.now.clone();
            let now = Encoding {
                file_id,
                title_id: title.id,
                done_ms: Default::default(),
                total_ms: (plan.secs * 1000.0) as u64,
                speed: Default::default(),
                stop: was.map(|w| w.stop).unwrap_or_default(),
                how: Default::default(),
            };
            let parts = (now.done_ms.clone(), now.speed.clone(), now.stop.clone(), now.how.clone());
            e.now = Some(now);
            parts
        };
        self.emit(Event::Title { id: title.id });
        tokio::fs::create_dir_all(plan.folder.join(Path::new(&plan.rel_path).parent().unwrap_or(Path::new("")))).await.with_context(|| format!("creating {}", plan.folder.display()))?;
        // Under a hidden name until it is whole, so that nothing takes a half-made file for a film.
        let working = plan.folder.join(Path::new(&plan.rel_path).parent().unwrap_or(Path::new(""))).join(format!(".spool-compact-{file_id}.mkv"));
        let started = std::time::Instant::now();
        let mut last_error = String::new();
        let mut made = None;
        for way in self.compact_ways() {
            if stop.load(Ordering::Relaxed) {
                bail!(CALLED_OFF);
            }
            *how.lock() = if way == How::Software { "software" } else { "hardware" }.into();
            done.store(0, Ordering::Relaxed);
            match run(&tool, &arguments(&c, &plan.info, &plan.src, &working, way), &done, &speed, &stop).await {
                Ok(()) => {
                    made = Some(way);
                    break;
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&working).await;
                    if e.to_string() == CALLED_OFF {
                        return Err(e);
                    }
                    tracing::info!(title = %title.title, way = ?way, error = %format!("{e:#}"), "that way of making a compact version did not work; trying the next");
                    last_error = format!("{e:#}");
                }
            }
        }
        let Some(way) = made else { bail!("ffmpeg could not do it: {last_error}") };

        // Read it back before it is given its name: it has to be video, and as long as the original.
        let checked = match self.probe(&working, "").await {
            Ok(Some(info)) if (info.runtime_secs - plan.secs).abs() <= (plan.secs * 0.01).max(3.0) => Ok(info),
            Ok(Some(info)) => Err(anyhow!("it came out {:.0} minutes long where the original runs {:.0}", info.runtime_secs / 60.0, plan.secs / 60.0)),
            Ok(None) => Err(anyhow!("it could not be read back to be checked")),
            Err(e) => Err(e),
        };
        let info = match checked {
            Ok(info) => info,
            Err(e) => {
                let _ = tokio::fs::remove_file(&working).await;
                return Err(e);
            }
        };
        let target = plan.folder.join(&plan.rel_path);
        tokio::fs::rename(&working, &target).await.with_context(|| format!("naming {}", target.display()))?;
        let size = tokio::fs::metadata(&target).await.map(|m| m.len()).unwrap_or(0);

        let flavor = title.kind.flavor();
        let source = file.quality.quality.name(flavor).to_lowercase();
        let from = if source.contains("remux") || source.contains("bluray") { "BluRay" } else if source.contains("web") { "WEB-DL" } else { "HDTV" };
        let record = MediaFile {
            id: 0,
            title_id: title.id,
            rel_path: plan.rel_path.clone(),
            size,
            quality: spool_core::quality::parse_quality(&format!("Compact.{}p.{from}.x265", screen(info.width, info.height)), flavor),
            release_group: None,
            edition: file.edition.clone(),
            media_info: Some(info),
            scene_name: None,
            languages: file.languages.clone(),
            added_at: crate::db::now(),
            extra: true,
            subtitles: vec![],
            base: plan.base.clone(),
            compact_of: Some(file_id),
        };
        self.db.with(|c| c.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1,?2,?3,?4)", rusqlite::params![title.id, record.rel_path, record.size as i64, serde_json::to_string(&record).unwrap_or_default()]))?;
        // Kept on another location, its folder becomes one more place the title is held.
        if let Some(base) = &plan.base {
            let mut t = self.db.title(title.id)?.ok_or_else(|| anyhow!("the title was removed"))?;
            if !t.also.contains(base) {
                t.also.push(base.clone());
                self.db.save_title(&mut t)?;
            }
        }
        let took = started.elapsed().as_secs();
        self.db.add_history(Some(title.id), "compact_made", json!({"path": target, "from_bytes": file.size, "bytes": size, "seconds": took, "how": if way == How::Software { "software" } else { "hardware" }}))?;
        let folder = plan.folder.to_string_lossy().to_string();
        let app = self.clone();
        tokio::spawn(async move {
            let _ = app.plex_refresh(&folder).await;
        });
        Ok(format!("A compact version of {} was made in {} min: {:.1} GB from {:.1} GB", title.title, (took + 30) / 60, size as f64 / 1e9, file.size as f64 / 1e9))
    }

    /// Take a compact version away: the file and its record.
    pub fn delete_compact(&self, file_id: i64) -> Result<()> {
        let file = self.db.file(file_id)?.ok_or_else(|| anyhow!("no such file"))?;
        if file.compact_of.is_none() {
            bail!("that is not a compact version");
        }
        let title = self.db.title(file.title_id)?.ok_or_else(|| anyhow!("no such title"))?;
        let folder = title.folder_of(&file).to_string();
        let places = self.placement();
        if !places.online_at(&folder) {
            bail!("it is on a location that cannot be reached right now");
        }
        if places.of(&folder).is_some_and(|p| !p.0.manage) {
            bail!("it is on a location Spool only reads");
        }
        let abs = title.file_path(&file);
        if abs.exists() {
            // Made here and makeable again, so it is not kept in the recycle folder.
            std::fs::remove_file(&abs).with_context(|| format!("deleting {}", abs.display()))?;
            if let Some(parent) = abs.parent() {
                let _ = std::fs::remove_dir(parent);
            }
            if file.base.is_some() {
                let _ = std::fs::remove_dir(&folder);
            }
        }
        self.db.with(|c| c.execute("DELETE FROM files WHERE id = ?1", [file_id]))?;
        // A folder on another location that held nothing else is no longer one of the title's.
        if let Some(base) = &file.base {
            if !self.db.files(title.id)?.iter().any(|f| f.base.as_ref() == Some(base)) && !Path::new(base).exists() {
                let mut t = title.clone();
                t.also.retain(|a| a != base);
                self.db.save_title(&mut t)?;
            }
        }
        self.db.add_history(Some(title.id), "compact_removed", json!({"path": abs.to_string_lossy()}))?;
        self.emit(Event::Title { id: title.id });
        Ok(())
    }

    /// Take away the compact versions made from a file that is itself going or gone: one
    /// made from a file that has been replaced is of the wrong film.
    pub fn drop_compacts_of(&self, file_id: i64) {
        self.cancel_compacts(&[file_id]);
        let Ok(Some(file)) = self.db.file(file_id) else {
            // The record has gone already; its compact versions are found by what they point at.
            for f in self.db.all_files().unwrap_or_default().into_iter().filter(|f| f.compact_of == Some(file_id)) {
                if let Err(e) = self.delete_compact(f.id) {
                    tracing::warn!(error = %format!("{e:#}"), "could not take away a compact version whose original has gone");
                }
            }
            return;
        };
        for f in self.db.files(file.title_id).unwrap_or_default().into_iter().filter(|f| f.compact_of == Some(file_id)) {
            if let Err(e) = self.delete_compact(f.id) {
                tracing::warn!(error = %format!("{e:#}"), "could not take away a compact version whose original has gone");
            }
        }
    }

    /// Keep the compact version and let the original go, to make room. The original is
    /// recycled like any deleted file; the compact version becomes the title's file.
    pub fn keep_only_compact(&self, compact_id: i64) -> Result<()> {
        let compact = self.db.file(compact_id)?.ok_or_else(|| anyhow!("no such file"))?;
        let Some(original_id) = compact.compact_of else { bail!("that is not a compact version") };
        let title = self.db.title(compact.title_id)?.ok_or_else(|| anyhow!("no such title"))?;
        if !title.file_path(&compact).exists() {
            bail!("the compact version is not on disk, so the original is kept");
        }
        if compact.base.is_some() {
            bail!("the compact version is kept on another location than the original, which stays where it is");
        }
        if let Some(original) = self.db.file(original_id)? {
            if original.base.is_some() {
                bail!("the original is a copy on another location, where Spool changes nothing");
            }
            // The compact version takes the original's place before the original goes, so
            // there is no moment when the title has no file.
            let mut kept = compact.clone();
            (kept.compact_of, kept.extra) = (None, original.extra);
            self.db.save_file(&kept)?;
            self.db.with(|c| c.execute("UPDATE episodes SET file_id = ?2 WHERE file_id = ?1", rusqlite::params![original_id, compact_id]))?;
            self.delete_file(original_id)?;
        } else {
            let mut kept = compact.clone();
            (kept.compact_of, kept.extra) = (None, false);
            self.db.save_file(&kept)?;
        }
        self.db.add_history(Some(title.id), "compact_kept", json!({"path": title.file_path(&compact).to_string_lossy()}))?;
        self.emit(Event::Title { id: title.id });
        Ok(())
    }

    /// After something has arrived for a title: make compact versions of what would gain
    /// from one, if the settings say to.
    pub fn compact_arrivals(&self, title_id: i64) {
        if !self.settings.general().compact.auto {
            return;
        }
        if let Ok(asked) = self.compact_titles(&[title_id]) {
            if asked.queued > 0 {
                tracing::info!(title = title_id, files = asked.queued, "making compact versions of what arrived");
            }
        }
    }

    /// How the library stands for compact versions: what has one, what would gain from one
    /// and how much room and time that would take.
    pub fn compact_report(&self) -> Result<serde_json::Value> {
        let titles = self.db.titles(None)?;
        let all = self.db.all_files()?;
        let made: Vec<&MediaFile> = all.iter().filter(|f| f.compact_of.is_some()).collect();
        let (mut would, mut bytes, mut secs, mut from) = (0usize, 0u64, 0f64, 0u64);
        let mut why_not: std::collections::BTreeMap<String, usize> = Default::default();
        for t in &titles {
            for f in self.compact_sources(t).unwrap_or_default() {
                if all.iter().any(|x| x.compact_of == Some(f.id)) {
                    continue;
                }
                match self.compact_plan(t, &f) {
                    Ok(p) => {
                        would += 1;
                        bytes += p.bytes;
                        secs += p.secs;
                        from += f.size;
                    }
                    Err(e) => {
                        // Grouped by the kind of reason, without the numbers that differ file by file.
                        let why = format!("{e:#}");
                        let kind = why.split([':', ',']).next().unwrap_or(&why).trim().to_string();
                        *why_not.entry(kind).or_default() += 1;
                    }
                }
            }
        }
        let originals: u64 = made.iter().filter_map(|m| all.iter().find(|f| Some(f.id) == m.compact_of)).map(|f| f.size).sum();
        Ok(json!({
            "means": self.compact_means(),
            "made": {"files": made.len(), "bytes": made.iter().map(|f| f.size).sum::<u64>(), "original_bytes": originals},
            "would": {"files": would, "bytes": bytes, "original_bytes": from, "running_secs": secs},
            "why_not": why_not,
        }))
    }
}

/// Run ffmpeg to the end, passing on how far it has got, and stopping it when told to.
async fn run(tool: &Path, args: &[String], done: &AtomicU64, speed: &AtomicU64, stop: &AtomicBool) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let mut child = tokio::process::Command::new(tool).args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().context("starting ffmpeg")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or_else(|| anyhow!("no output from ffmpeg"))?).lines();
    let mut errors = child.stderr.take().ok_or_else(|| anyhow!("no output from ffmpeg"))?;
    let said = tokio::spawn(async move {
        let mut text = String::new();
        let _ = errors.read_to_string(&mut text).await;
        text
    });
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if let Some(us) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms=")).and_then(|v| v.trim().parse::<i64>().ok()) {
                        done.store((us.max(0) / 1000) as u64, Ordering::Relaxed);
                    } else if let Some(x) = line.strip_prefix("speed=").and_then(|v| v.trim().trim_end_matches('x').parse::<f64>().ok()) {
                        speed.store((x * 100.0) as u64, Ordering::Relaxed);
                    }
                }
                _ => break,
            },
            _ = tick.tick() => {
                if stop.load(Ordering::Relaxed) {
                    let _ = child.kill().await;
                    bail!(CALLED_OFF);
                }
            }
        }
    }
    let status = child.wait().await?;
    if stop.load(Ordering::Relaxed) {
        bail!(CALLED_OFF);
    }
    if !status.success() {
        let text = said.await.unwrap_or_default();
        let why = text.lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("it stopped without saying why").trim().to_string();
        bail!("{}", why.chars().take(300).collect::<String>());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compact_version_is_known_by_its_name() {
        assert_eq!(compact_source_stem("Heat (1995) [Remux-2160p] - Compact 1080p"), Some("Heat (1995) [Remux-2160p]"));
        assert_eq!(compact_source_stem("Heat (1995) - Compact"), Some("Heat (1995)"));
        assert_eq!(compact_source_stem(&format!("Show - S01E02 - Pilot{}", compact_suffix(720))), Some("Show - S01E02 - Pilot"));
        assert_eq!(compact_source_stem("Compact Disc - The Film (2001)"), None);
        assert_eq!(compact_source_stem("Heat (1995) - Compact Edition"), None);
    }

    #[test]
    fn a_wide_picture_is_labelled_by_the_screen_it_fits() {
        assert_eq!(screen(1920, 1038), 1080);
        assert_eq!(screen(1920, 800), 1080);
        assert_eq!(screen(1440, 1080), 1080);
        assert_eq!(screen(1280, 534), 720);
        assert_eq!(screen(720, 480), 480);
        assert_eq!(screen(3840, 1600), 2160);
    }

    #[test]
    fn the_picture_is_made_to_fit_and_never_made_bigger() {
        assert_eq!(fit(3840, 2160, 1080), Some((1920, -2)));
        // A wide film is brought down by its width; a tall one by its height.
        assert_eq!(fit(3840, 1608, 1080), Some((1920, -2)));
        assert_eq!(fit(2880, 2160, 1080), Some((-2, 1080)));
        assert_eq!(fit(1920, 804, 1080), None);
        assert_eq!(fit(1280, 720, 1080), None);
        assert_eq!(fit(0, 0, 1080), None);
    }

    #[test]
    fn sound_that_is_small_is_copied_and_the_rest_converted() {
        let stream = |index: u32, kind: &str, codec: &str, channels: u32| spool_core::mediainfo::Stream { index, kind: kind.into(), codec: codec.into(), channels, ..Default::default() };
        let info = MediaInfo {
            width: 3840,
            height: 2160,
            bit_depth: 10,
            streams: vec![stream(0, "video", "hevc", 0), stream(1, "audio", "truehd", 8), stream(2, "audio", "ac3", 6), stream(3, "audio", "dts", 2), stream(4, "subtitle", "hdmv_pgs_subtitle", 0), stream(5, "subtitle", "eia_608", 0), stream(6, "subtitle", "mov_text", 0)],
            ..Default::default()
        };
        let said = arguments(&Compact::default(), &info, Path::new("/in.mkv"), Path::new("/out.mkv"), How::Hardware).join(" ");
        assert!(said.contains("-hwaccel videotoolbox") && said.contains("scale_vt=w=1920:h=-2") && said.contains("-profile:v main10 -b:v 8000k"), "{said}");
        assert!(said.contains("-c:a:0 eac3 -b:a:0 640k -ac:a:0 6") && said.contains("-c:a:1 copy") && said.contains("-c:a:2 eac3 -b:a:2 224k"), "{said}");
        // Subtitles a Matroska file can hold are kept, in order; the kind it cannot is left out.
        assert!(said.contains("-map 0:4") && !said.contains("-map 0:5") && said.contains("-map 0:6") && said.contains("-c:s:0 copy") && said.contains("-c:s:1 srt"), "{said}");
        let soft = arguments(&Compact::default(), &info, Path::new("/in.mkv"), Path::new("/out.mkv"), How::Software).join(" ");
        assert!(!soft.contains("hwaccel") && soft.contains("-vf scale=1920:-2") && soft.contains("libx265") && soft.contains("yuv420p10le"), "{soft}");
    }
}
