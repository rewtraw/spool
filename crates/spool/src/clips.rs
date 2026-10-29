//! Clips: a few seconds or minutes cut from a film or an episode, made to be sent on.
//!
//! A clip is made as the one kind of video that plays everywhere it might be sent: H.264 in an
//! MP4 that can start before it has all arrived, no larger than 1080p, with the soundtrack in
//! the household's language as stereo AAC. A picture in HDR is brought down to ordinary colour
//! by the video hardware, so that it does not look washed out on a phone. Clips are kept in
//! Spool's own folder, apart from the library, with a still from the middle of each.
//!
//! One is made at a time. A clip being made when Spool stops is made again when it starts.

use crate::app::{App, Event};
use crate::models::*;
use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use spool_core::mediainfo::MediaInfo;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

/// What a clip is cut from, in words: "Westworld S01E07 · Trompe L'Oeil", "Dark City (1998)".
pub fn cut_from(title: &Title, episode: Option<&Episode>) -> String {
    match episode {
        Some(e) if e.title.is_empty() => format!("{} S{:02}E{:02}", title.title, e.season, e.episode),
        Some(e) => format!("{} S{:02}E{:02} · {}", title.title, e.season, e.episode, e.title),
        None if title.year > 0 => format!("{} ({})", title.title, title.year),
        None => title.title.clone(),
    }
}

/// The longest clip there can be, in seconds.
pub const LONGEST: f64 = 600.0;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Clip {
    pub id: i64,
    /// The title it was cut from, while that is in the library.
    pub title_id: Option<i64>,
    pub episode_id: Option<i64>,
    /// Who made it.
    pub account_id: Option<i64>,
    pub name: String,
    /// What it was cut from, in words: "Westworld S01E07 · Trompe L'Oeil".
    pub from: String,
    /// Where in that it starts, and how long it runs, in seconds.
    pub start: f64,
    pub seconds: f64,
    /// "making", "ready" or "failed".
    pub state: String,
    pub error: String,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub created_at: i64,
    /// What its link is known by: the link plays it without signing in, for whoever it is sent
    /// to that can reach Spool.
    pub link: String,
    /// Which subtitles were asked for: "auto", "none", "on", or a language.
    pub subtitles: String,
    /// The language of the subtitles drawn into it, in words; empty for none.
    pub subtitles_shown: String,
    /// What is said in it, from the dialogue index, timed from the clip's own start. None
    /// until the file it was cut from has been read.
    pub said: Option<Vec<Said>>,
}

/// A line said in a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Said {
    /// Seconds from the start of the clip.
    pub at: f64,
    pub text: String,
}

/// What a clip is asked for with.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wanted {
    pub title_id: i64,
    /// One episode of a series, by its id or by its season and number.
    pub episode_id: Option<i64>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    /// Where it starts: seconds, or as written: "30m", "1h2m5s", "1:02:05", "30:00".
    pub start: serde_json::Value,
    /// How long it runs, in seconds or as written; thirty seconds when not said.
    pub seconds: Option<serde_json::Value>,
    pub name: Option<String>,
    /// "auto" (the default: when what is heard is not in the household's language, and the
    /// lines always shown otherwise), "none", "on", or a language.
    pub subtitles: Option<String>,
}

/// How far each clip being made has got, in thousandths of a second, and whether to stop it.
static MAKING: LazyLock<Mutex<HashMap<i64, Making>>> = LazyLock::new(Default::default);
type Making = (Arc<AtomicU64>, Arc<AtomicBool>);
/// Whether a clip is being made or waits to be, for work that would slow it to stand aside.
pub fn any_making() -> bool {
    !MAKING.lock().is_empty()
}

/// One clip at a time.
static ONE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// A time as people write it, in seconds: "90", "90s", "30m", "1h2m5s", "1:02:05", "2.5m".
pub fn read_time(v: &serde_json::Value) -> Result<f64> {
    if let Some(n) = v.as_f64() {
        return if n.is_finite() && n >= 0.0 { Ok(n) } else { bail!("a time is a number of seconds from the start") };
    }
    let text = v.as_str().ok_or_else(|| anyhow!("a time is a number of seconds or written as 30m, 1h2m5s or 1:02:05"))?.trim().to_lowercase();
    let wrong = || anyhow!("\"{text}\" is not a time Spool can read: write it as 30m, 1h2m5s, 1:02:05 or a number of seconds");
    if text.is_empty() {
        return Err(wrong());
    }
    if text.contains(':') {
        let parts: Vec<&str> = text.split(':').collect();
        if parts.len() > 3 {
            return Err(wrong());
        }
        let mut secs = 0.0;
        for p in &parts {
            let n: f64 = p.trim().parse().map_err(|_| wrong())?;
            if !n.is_finite() || n < 0.0 {
                return Err(wrong());
            }
            secs = secs * 60.0 + n;
        }
        return Ok(secs);
    }
    let (mut secs, mut number, mut any) = (0.0, String::new(), false);
    for c in text.chars().filter(|c| !c.is_whitespace()) {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        let unit = match c {
            'h' => 3600.0,
            'm' => 60.0,
            's' => 1.0,
            _ => return Err(wrong()),
        };
        let n: f64 = number.parse().map_err(|_| wrong())?;
        if !n.is_finite() {
            return Err(wrong());
        }
        secs += n * unit;
        number.clear();
        any = true;
    }
    if !number.is_empty() {
        // A bare number is seconds; one after "1m" is seconds too.
        secs += number.parse::<f64>().map_err(|_| wrong())?;
        any = true;
    }
    if any && secs.is_finite() { Ok(secs) } else { Err(wrong()) }
}

/// A time written as a clock: "1:02:05", or "2:05" under an hour.
pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Whether a picture is in HDR, and so has to be brought down to ordinary colour.
fn is_hdr(info: &MediaInfo) -> bool {
    !info.dynamic_range_type.is_empty() && info.dynamic_range_type != "DV SDR"
}

/// Whether this machine's ffmpeg can bring HDR down to ordinary colour: on a Mac's video hardware.
fn can_tone_map(tool: &Path) -> bool {
    static CAN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CAN.get_or_init(|| std::process::Command::new(tool).args(["-hide_banner", "-filters"]).output().ok().is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains("scale_vt")))
}

/// The soundtrack to use: one in the household's language, the file's own first choice of
/// those, or failing that the file's first choice of all.
fn soundtrack(info: &MediaInfo, language: &str) -> Option<u32> {
    let audio: Vec<_> = info.streams.iter().filter(|s| s.kind == "audio").collect();
    let ours: Vec<_> = audio.iter().filter(|s| !language.is_empty() && crate::subs::language_code(&s.language) == Some(language)).collect();
    ours.iter().find(|s| s.default).or(ours.first()).copied().or(audio.iter().find(|s| s.default)).or(audio.first()).map(|s| s.index)
}

/// What is laid over the picture of a clip.
#[derive(Debug, PartialEq)]
enum Over {
    /// Subtitles drawn here: the list of pictures ffmpeg reads them by.
    Drawn(PathBuf),
    /// A track of picture subtitles inside the file, by its stream number.
    Picture(u32),
}

/// The size a picture comes out at: no larger than 1080p, its shape kept, both sides even.
fn out_size(width: u32, height: u32) -> (u32, u32) {
    let even = |n: f64| ((n / 2.0).round() as u32 * 2).max(2);
    match crate::compact::fit(width, height, 1080) {
        Some((w, -2)) => (w as u32, even(height as f64 * w as f64 / width as f64)),
        Some((-2, h)) => (even(width as f64 * h as f64 / height as f64), h as u32),
        _ => (width, height),
    }
}

/// How a clip is drawn as a GIF: with fewer pictures a second the longer it runs, and as large
/// as keeps it to about 10 MB, so that it is still a file that can be sent; in colours chosen for it.
fn gif_filter(seconds: f64, width: u32, height: u32) -> String {
    let fps = if seconds <= 10.0 { 15 } else if seconds <= 20.0 { 12 } else { 10 };
    // About two thirds of a byte for each point of each picture, as GIFs of films come out.
    const POINTS: f64 = 16_000_000.0;
    let tall = if width > 0 && height > 0 { height as f64 / width as f64 } else { 9.0 / 16.0 };
    let wide = (POINTS / (seconds.max(1.0) * fps as f64) / tall).sqrt();
    let wide = (wide.clamp(240.0, 480.0) as u32).min(if width > 0 { width } else { u32::MAX }) / 2 * 2;
    format!("fps={fps},scale={wide}:-2:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle")
}

/// Cut a stretch of any video, outside the library, with the subtitles of a file of them drawn
/// in as a clip of the library has its own drawn. Without subtitles it is cut plain.
pub async fn clip_file(src: &Path, out: &Path, start: f64, seconds: f64, subtitles: Option<&Path>) -> Result<()> {
    let tool = crate::subs::ffmpeg_installed().ok_or_else(|| anyhow!("ffmpeg is not installed"))?;
    let probe = tool.with_file_name("ffprobe");
    let info = crate::import::probe_with(&probe, src, "").await?;
    let seconds = seconds.min((info.runtime_secs - start).max(0.0));
    if seconds <= 0.0 {
        bail!("the video ends before {}", clock(start));
    }
    let stop = Arc::new(AtomicBool::new(false));
    // The subtitles are drawn as pictures in a folder beside what is made, cleared away after.
    struct Tidy(PathBuf);
    impl Drop for Tidy {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let drawing = Tidy(out.with_extension("subtitles"));
    let over = match subtitles {
        None => None,
        Some(path) => {
            let source = crate::captions::Source::Beside(path.to_path_buf());
            let cues = crate::captions::cues(&tool, src, &source, &[], start, seconds, &stop).await?;
            if cues.is_empty() {
                bail!("{} has no lines between {} and {}", path.display(), clock(start), clock(start + seconds));
            }
            let (w, h) = out_size(info.width, info.height);
            let dir = drawing.0.clone();
            let halt = stop.clone();
            tokio::task::spawn_blocking(move || crate::captions::render(&cues, seconds, w, h, &dir, &halt)).await??.map(Over::Drawn)
        }
    };
    let (done, speed) = (AtomicU64::new(0), AtomicU64::new(0));
    crate::compact::run(&tool, &arguments(&info, src, out, start, seconds, "", over.as_ref()), crate::compact::frames_a_second(&info), &done, &speed, &stop).await
}

/// What to hand ffmpeg to cut a clip.
fn arguments(info: &MediaInfo, src: &Path, out: &Path, start: f64, seconds: f64, language: &str, over: Option<&Over>) -> Vec<String> {
    let mut a: Vec<String> = vec![];
    let mut add = |items: &[&str]| a.extend(items.iter().map(|s| s.to_string()));
    add(&["-nostdin", "-hide_banner", "-loglevel", "error", "-nostats", "-progress", "pipe:1", "-y"]);
    let hdr = is_hdr(info);
    if hdr {
        add(&["-hwaccel", "videotoolbox", "-hwaccel_output_format", "videotoolbox_vld"]);
    }
    add(&["-ss", &format!("{start:.3}"), "-i", &src.to_string_lossy()]);
    if let Some(Over::Drawn(list)) = over {
        add(&["-f", "concat", "-safe", "0", "-i", &list.to_string_lossy()]);
    }
    let size = crate::compact::fit(info.width, info.height, 1080);
    let picture = if hdr {
        // The video hardware maps the picture to ordinary colour as it resizes it.
        let (w, h) = size.map_or(("iw".to_string(), "ih".to_string()), |(w, h)| (w.to_string(), h.to_string()));
        format!("scale_vt=w={w}:h={h}:color_matrix=bt709:color_primaries=bt709:color_transfer=bt709,hwdownload,format=p010le,format=yuv420p")
    } else {
        match size {
            Some((w, h)) => format!("scale={w}:{h},format=yuv420p"),
            None => "format=yuv420p".into(),
        }
    };
    let (w, h) = out_size(info.width, info.height);
    match over {
        None => add(&["-map", "0:v:0", "-vf", &picture]),
        Some(Over::Drawn(_)) => {
            add(&["-filter_complex", &format!("[0:v:0]{picture}[v];[1:v]format=rgba[s];[v][s]overlay=x=0:y=main_h-overlay_h:eof_action=pass:format=auto,format=yuv420p[out]")]);
            add(&["-map", "[out]"]);
        }
        Some(Over::Picture(index)) => {
            add(&["-filter_complex", &format!("[0:v:0]{picture}[v];[0:{index}]scale={w}:{h}[s];[v][s]overlay=eof_action=pass:format=auto,format=yuv420p[out]")]);
            add(&["-map", "[out]"]);
        }
    }
    match soundtrack(info, language) {
        Some(i) => add(&["-map", &format!("0:{i}")]),
        None => add(&["-map", "0:a:0?"]),
    }
    // How long, as an option of what is written, after every input: placed before an input it
    // would be that input's.
    add(&["-t", &format!("{seconds:.3}")]);
    add(&["-c:v", "libx264", "-preset", "medium", "-crf", "20", "-profile:v", "high", "-maxrate", "12M", "-bufsize", "24M"]);
    add(&["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709"]);
    add(&["-c:a", "aac", "-b:a", "192k", "-ac", "2"]);
    add(&["-sn", "-dn", "-map_metadata", "-1", "-map_chapters", "-1", "-movflags", "+faststart", "-f", "mp4", &out.to_string_lossy()]);
    a
}

impl App {
    fn clips_dir(&self) -> PathBuf {
        self.data_dir.join("clips")
    }

    /// Where a clip's video is kept.
    pub fn clip_video(&self, id: i64) -> PathBuf {
        self.clips_dir().join(format!("{id}.mp4"))
    }

    /// Where a clip as a GIF is kept, once someone has asked for it.
    pub fn clip_gif(&self, id: i64) -> PathBuf {
        self.clips_dir().join(format!("{id}.gif"))
    }

    /// A clip as a GIF, made from its video the first time it is asked for.
    pub async fn gif_of(&self, c: &Clip) -> Result<PathBuf> {
        static ONE_GIF: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let gif = self.clip_gif(c.id);
        let _one = ONE_GIF.lock().await;
        if gif.is_file() {
            return Ok(gif);
        }
        let tool = self.ffmpeg_path().ok_or_else(|| anyhow!("ffmpeg is not installed"))?;
        let part = self.clips_dir().join(format!(".{}.gif.part", c.id));
        let made = tokio::process::Command::new(&tool)
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(self.clip_video(c.id))
            .args(["-vf", &gif_filter(c.seconds, c.width, c.height), "-loop", "0", "-f", "gif"])
            .arg(&part)
            .kill_on_drop(true)
            .output()
            .await?;
        if !made.status.success() {
            let _ = tokio::fs::remove_file(&part).await;
            bail!("the GIF could not be made: {}", String::from_utf8_lossy(&made.stderr).lines().last().unwrap_or("ffmpeg failed"));
        }
        tokio::fs::rename(&part, &gif).await?;
        Ok(gif)
    }

    /// Where the still from a clip is kept.
    pub fn clip_still(&self, id: i64) -> PathBuf {
        self.clips_dir().join(format!("{id}.jpg"))
    }

    /// How far a clip being made has got, from nought to one.
    pub fn clip_progress(&self, c: &Clip) -> Option<f64> {
        let m = MAKING.lock();
        let (done, _) = m.get(&c.id)?;
        Some((done.load(Ordering::Relaxed) as f64 / 1000.0 / c.seconds.max(0.1)).clamp(0.0, 1.0))
    }

    /// The file a clip of a title, or of one of its episodes, is cut from, and what it is
    /// called.
    fn clip_source(&self, title: &Title, episode: Option<&Episode>) -> Result<(MediaFile, String)> {
        let file = match (title.kind, episode) {
            (Kind::Movie, _) => self.film_file(title)?.ok_or_else(|| anyhow!("{} has no file to cut a clip from", title.title))?,
            (Kind::Series, None) => bail!("say which episode of {} to cut it from", title.title),
            (Kind::Series, Some(e)) => {
                let id = e.file_id.ok_or_else(|| anyhow!("{} S{:02}E{:02} has no file to cut a clip from", title.title, e.season, e.episode))?;
                self.db.file(id)?.ok_or_else(|| anyhow!("the episode's file is no longer in the library"))?
            }
        };
        if !self.placement().online_at(title.folder_of(&file)) {
            bail!("it is on a location that cannot be reached right now");
        }
        Ok((file, cut_from(title, episode)))
    }

    /// The file of a film that clips are cut from, and whose words are searched: its largest
    /// main file.
    pub(crate) fn film_file(&self, title: &Title) -> Result<Option<MediaFile>> {
        Ok(self.compact_sources(title)?.into_iter().max_by_key(|f| f.size))
    }

    /// The subtitles to draw into a clip of a file, as asked.
    async fn clip_subtitles(&self, title: &Title, file: &MediaFile, info: &MediaInfo, wanted: &str) -> Option<crate::captions::Chosen> {
        if matches!(wanted.trim().to_lowercase().as_str(), "none" | "off") {
            return None;
        }
        let household = crate::subs::language_code(&self.settings.general().language).unwrap_or_default();
        let heard = soundtrack(info, household).and_then(|i| info.streams.iter().find(|s| s.index == i)).and_then(|s| crate::subs::language_code(&s.language));
        // What lies beside the video is looked for afresh, and remembered; away from the
        // work of answering, as a slow share can take a while to list.
        let (app, folder, id, known) = (self.clone(), title.path.clone(), file.id, file.subtitles.clone());
        let beside = tokio::task::spawn_blocking(move || {
            let found = app.find_sidecars(id).unwrap_or(known);
            found.into_iter().map(|s| { let p = Path::new(&folder).join(&s.rel_path); (s, p) }).filter(|(_, p)| p.is_file()).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        crate::captions::choose(info, &beside, heard, household, wanted)
    }

    /// Ask for a clip. It is recorded at once and made in the background.
    pub async fn add_clip(&self, w: &Wanted, account: Option<i64>) -> Result<Clip> {
        let title = self.db.title(w.title_id)?.ok_or_else(|| anyhow!("there is no such title"))?;
        let episode = match (w.episode_id, w.season, w.episode) {
            (Some(id), _, _) => Some(self.db.episode(id)?.filter(|e| e.title_id == title.id).ok_or_else(|| anyhow!("{} has no such episode", title.title))?),
            (None, Some(s), Some(n)) => Some(self.db.episodes(title.id)?.into_iter().find(|e| e.season == s && e.episode == n).ok_or_else(|| anyhow!("{} has no S{s:02}E{n:02}", title.title))?),
            (None, None, None) => None,
            _ => bail!("an episode is named by its season and its number together"),
        };
        if self.ffmpeg_path().is_none() {
            bail!("ffmpeg is not installed, so no clip can be made");
        }
        let (file, from) = self.clip_source(&title, episode.as_ref())?;
        // A file whose details were never read is read now: how long it runs and what is in it decide the clip.
        let info = match file.media_info.clone() {
            Some(info) => info,
            None => self.probe(&title.file_path(&file), "").await?.ok_or_else(|| anyhow!("the file could not be read, so it is not known what is in it"))?,
        };
        if info.dynamic_range_type == "DV" {
            bail!("it is Dolby Vision with no HDR10 layer, whose colours cannot be brought down for a clip");
        }
        if is_hdr(&info) && !self.ffmpeg_path().is_some_and(|tool| can_tone_map(&tool)) {
            bail!("it is HDR, and bringing HDR down to ordinary colour needs a Mac's video hardware, which this machine's ffmpeg does not offer");
        }
        let start = read_time(&w.start)?;
        let seconds = match &w.seconds {
            Some(v) => read_time(v)?,
            None => 30.0,
        };
        if seconds < 1.0 {
            bail!("a clip runs for at least a second");
        }
        if seconds > LONGEST {
            bail!("a clip runs for at most {} minutes", LONGEST / 60.0);
        }
        let runs = info.runtime_secs;
        if runs > 0.0 && start >= runs - 0.5 {
            bail!("it runs {}, so there is nothing at {}", clock(runs), clock(start));
        }
        let seconds = if runs > 0.0 { seconds.min(runs - start) } else { seconds };
        let subtitles = w.subtitles.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("auto").to_lowercase();
        if !matches!(subtitles.as_str(), "auto" | "none" | "off") && self.clip_subtitles(&title, &file, &info, &subtitles).await.is_none() {
            let language = if subtitles == "on" { self.settings.general().language } else { subtitles.clone() };
            bail!("it has no {language} subtitles to draw in");
        }
        let name = w.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| {
            let short = match &episode {
                Some(e) => format!("{} S{:02}E{:02}", title.title, e.season, e.episode),
                None => title.title.clone(),
            };
            format!("{short} at {}", clock(start))
        });
        let said = self.said_between(file.id, start, seconds);
        let mut clip = Clip {
            said,
            title_id: Some(title.id),
            episode_id: episode.map(|e| e.id),
            account_id: account,
            name,
            from,
            start,
            seconds,
            state: "making".into(),
            created_at: crate::db::now(),
            link: crate::settings::new_key(),
            subtitles,
            ..Default::default()
        };
        clip.id = self.db.add_clip(&clip)?;
        self.make_clip_later(clip.id);
        Ok(clip)
    }

    /// What is said in a file between two times, timed from the first; None while the file
    /// has not been read.
    fn said_between(&self, file_id: i64, start: f64, seconds: f64) -> Option<Vec<Said>> {
        if !self.dialogue.has_read(file_id).unwrap_or(false) {
            return None;
        }
        let lines = self.dialogue.lines_between(file_id, start, start + seconds).ok()?;
        Some(sentences(&lines).into_iter().map(|(at, text)| Said { at: (at - start).max(0.0), text }).collect())
    }

    /// Fill in what is said in clips made before their film or episode was read. Returns
    /// whether any changed.
    pub fn fill_clips_said(&self, clips: &mut [Clip]) -> bool {
        let mut changed = false;
        for c in clips.iter_mut().filter(|c| c.said.is_none()) {
            let Some(title) = c.title_id.and_then(|id| self.db.title(id).ok().flatten()) else { continue };
            let file_id = match c.episode_id {
                Some(e) => self.db.episode(e).ok().flatten().and_then(|e| e.file_id),
                None => self.film_file(&title).ok().flatten().map(|f| f.id),
            };
            let Some(said) = file_id.and_then(|f| self.said_between(f, c.start, c.seconds)) else { continue };
            c.said = Some(said);
            changed |= self.db.save_clip(c).is_ok();
        }
        changed
    }

    fn make_clip_later(&self, id: i64) {
        let (done, stop) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)));
        MAKING.lock().insert(id, (done.clone(), stop.clone()));
        self.emit(Event::Clip { id });
        let app = self.clone();
        tokio::spawn(async move {
            let made = {
                let _turn = ONE.acquire().await;
                if stop.load(Ordering::Relaxed) { Err(anyhow!(crate::compact::CALLED_OFF)) } else { app.make_clip(id, &done, stop.clone()).await }
            };
            MAKING.lock().remove(&id);
            if stop.load(Ordering::Relaxed) {
                // Removed while it was being made: what was made of it goes too.
                let _ = std::fs::remove_file(app.clip_video(id));
                let _ = std::fs::remove_file(app.clip_still(id));
                return;
            }
            if let Err(e) = made {
                tracing::warn!(clip = id, error = %format!("{e:#}"), "a clip could not be made");
                let _ = std::fs::remove_file(app.clip_video(id));
                if let Ok(Some(mut c)) = app.db.clip(id) {
                    c.state = "failed".into();
                    c.error = format!("{e:#}");
                    let _ = app.db.save_clip(&c);
                }
            }
            app.emit(Event::Clip { id });
        });
    }

    async fn make_clip(&self, id: i64, done: &AtomicU64, stop: Arc<AtomicBool>) -> Result<()> {
        let mut clip = self.db.clip(id)?.ok_or_else(|| anyhow!("the clip was removed"))?;
        let title = self.db.title(clip.title_id.ok_or_else(|| anyhow!("the title it was cut from was removed"))?)?.ok_or_else(|| anyhow!("the title it was cut from was removed"))?;
        let episode = match clip.episode_id {
            Some(e) => Some(self.db.episode(e)?.ok_or_else(|| anyhow!("the episode it was cut from was removed"))?),
            None => None,
        };
        let (file, _) = self.clip_source(&title, episode.as_ref())?;
        let info = match file.media_info.clone() {
            Some(info) => info,
            None => self.probe(&title.file_path(&file), "").await?.unwrap_or_default(),
        };
        let tool = self.ffmpeg_path().ok_or_else(|| anyhow!("ffmpeg is not installed"))?;
        tokio::fs::create_dir_all(self.clips_dir()).await.context("making the folder clips are kept in")?;
        let out = self.clip_video(id);
        let language = crate::subs::language_code(&self.settings.general().language).unwrap_or_default();
        let src = title.file_path(&file);
        // The subtitles, drawn as pictures in a folder of their own while the clip is made,
        // and cleared away however that ends.
        struct Tidy(PathBuf);
        impl Drop for Tidy {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let drawing = Tidy(self.clips_dir().join(format!(".{id}-subtitles")));
        let chosen = self.clip_subtitles(&title, &file, &info, &clip.subtitles).await;
        let over = match &chosen {
            None => None,
            Some(c) => match &c.source {
                crate::captions::Source::Picture(index) => Some(Over::Picture(*index)),
                source => {
                    let cues = crate::captions::cues(&tool, &src, source, &crate::captions::streams_of(&info), clip.start, clip.seconds, &stop).await?;
                    let (w, h) = out_size(info.width, info.height);
                    let (dir, seconds, halt) = (drawing.0.clone(), clip.seconds, stop.clone());
                    tokio::task::spawn_blocking(move || crate::captions::render(&cues, seconds, w, h, &dir, &halt)).await??.map(Over::Drawn)
                }
            },
        };
        clip.subtitles_shown = match (&chosen, &over) {
            (Some(c), Some(_)) => crate::subs::language_name(&c.language),
            _ => String::new(),
        };
        let speed = AtomicU64::new(0);
        crate::compact::run(&tool, &arguments(&info, &src, &out, clip.start, clip.seconds, language, over.as_ref()), crate::compact::frames_a_second(&info), done, &speed, &stop).await?;
        drop(drawing);
        let made = self.probe(&out, "").await?.ok_or_else(|| anyhow!("the clip could not be read back"))?;
        if made.runtime_secs < 0.5 {
            bail!("the clip came out empty");
        }
        // A still from the middle, to show it by.
        let still = self.clip_still(id);
        let _ = tokio::process::Command::new(&tool)
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{:.3}", made.runtime_secs / 2.0), "-i"])
            .arg(&out)
            .args(["-frames:v", "1", "-vf", "scale=640:-2", "-q:v", "4"])
            .arg(&still)
            .status()
            .await;
        clip.state = "ready".into();
        clip.seconds = made.runtime_secs;
        clip.size = tokio::fs::metadata(&out).await.map(|m| m.len()).unwrap_or(0);
        (clip.width, clip.height) = (made.width, made.height);
        self.db.save_clip(&clip)?;
        tracing::info!(clip = id, name = %clip.name, bytes = clip.size, "a clip was made");
        Ok(())
    }

    /// Make again the clips that were being made when Spool last stopped.
    pub fn resume_clips(&self) {
        for c in self.db.clips().unwrap_or_default().into_iter().filter(|c| c.state == "making") {
            self.make_clip_later(c.id);
        }
    }

    /// Remove a clip, and stop it if it is being made.
    pub fn delete_clip(&self, id: i64) -> Result<()> {
        if let Some((_, stop)) = MAKING.lock().get(&id) {
            stop.store(true, Ordering::Relaxed);
        }
        self.db.delete_clip(id)?;
        for path in [self.clip_video(id), self.clip_still(id), self.clip_gif(id)] {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("removing {}", path.display())),
                _ => {}
            }
        }
        self.emit(Event::Clip { id });
        Ok(())
    }
}

/// Subtitle cues put back together into what was said. A cue left open, ending on a comma or
/// on no mark at all, runs on into the next, unless that one starts with a dash, which is how
/// subtitles show another speaker, or comes after a pause of more than a few seconds.
/// Returns each sentence with when it starts.
fn sentences(lines: &[crate::dialogue::Line]) -> Vec<(f64, String)> {
    const PAUSE: f64 = 3.0;
    let closed = |t: &str| t.trim_end().ends_with(['.', '?', '!', '…', '♪', '"', '”', ')', ']']) && !t.trim_end().ends_with("...") && !t.trim_end().ends_with('…');
    let trailing = |t: &str| t.trim_end().ends_with("...") || t.trim_end().ends_with('…');
    let another_speaker = |t: &str| t.trim_start().starts_with(['-', '–', '—']);
    let goes_on = |t: &str| t.trim_start().starts_with(|c: char| c.is_lowercase()) || t.trim_start().starts_with("...") || t.trim_start().starts_with('…');
    let mut out: Vec<(f64, String, f64)> = vec![];
    for l in lines {
        let text = l.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue;
        }
        let joins = out.last().is_some_and(|(_, prev, end)| {
            !another_speaker(&text) && l.start - end <= PAUSE && (!closed(prev) && (!trailing(prev) || goes_on(&text)))
        });
        match out.last_mut() {
            Some((_, prev, end)) if joins => {
                // "It's just..." then "...too much." is said once with its pause.
                let rest = if trailing(prev) { text.trim_start_matches(['.', '…']).trim_start() } else { text.as_str() };
                prev.push(' ');
                prev.push_str(rest);
                *end = l.end;
            }
            _ => out.push((l.start, text, l.end)),
        }
    }
    out.into_iter().map(|(at, text, _)| (at, text)).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn subtitle_cues_are_put_back_into_sentences() {
        let cues = [
            (0.0, 1.0, "He's still fighting you."),
            (1.2, 2.0, "- No, my friend."),
            (2.2, 3.0, "Arnold didn't know"),
            (3.1, 4.0, "how to save you."),
            (4.5, 5.0, "And now,"),
            (5.1, 6.0, "it is time to say goodbye,"),
            (6.1, 7.0, "old friend."),
            (7.2, 8.0, "I was going to"),
            (8.1, 9.0, "- Don't."),
            (9.5, 10.0, "Wait for"),
            (20.0, 21.0, "Another time."),
            (21.2, 22.0, "I thought..."),
            (22.1, 23.0, "Never mind."),
            (23.2, 24.0, "It's just..."),
            (24.1, 25.0, "...too much."),
        ];
        let lines: Vec<crate::dialogue::Line> = cues.iter().map(|(s, e, t)| crate::dialogue::Line { start: *s, end: *e, text: t.to_string() }).collect();
        let said: Vec<String> = super::sentences(&lines).into_iter().map(|(_, t)| t).collect();
        assert_eq!(
            said,
            [
                "He's still fighting you.",
                "- No, my friend.",
                "Arnold didn't know how to save you.",
                "And now, it is time to say goodbye, old friend.",
                "I was going to",
                "- Don't.",
                "Wait for",
                "Another time.",
                "I thought...",
                "Never mind.",
                "It's just... too much.",
            ]
        );
        assert_eq!(super::sentences(&lines)[2].0, 2.2, "a sentence starts when its first part does");
    }
    use super::*;
    use serde_json::json;

    #[test]
    fn times_are_read_as_people_write_them() {
        for (written, secs) in [(json!(90), 90.0), (json!("90"), 90.0), (json!("90s"), 90.0), (json!("30m"), 1800.0), (json!("1h2m5s"), 3725.0), (json!("1m30"), 90.0), (json!("2.5m"), 150.0), (json!("1:02:05"), 3725.0), (json!("30:00"), 1800.0), (json!(" 1h 2m "), 3720.0)] {
            assert_eq!(read_time(&written).unwrap(), secs, "{written}");
        }
        for wrong in [json!(""), json!("soon"), json!("1:2:3:4"), json!(-5), json!("m"), json!("NaN:0"), json!("inf"), json!("1:inf"), json!("1e400s")] {
            assert!(read_time(&wrong).is_err(), "{wrong}");
        }
        assert_eq!(clock(3725.0), "1:02:05");
        assert_eq!(clock(1800.0), "30:00");
    }

    fn stream(index: u32, kind: &str, language: &str, default: bool) -> spool_core::mediainfo::Stream {
        spool_core::mediainfo::Stream { index, kind: kind.into(), language: language.into(), default, ..Default::default() }
    }

    #[test]
    fn the_soundtrack_is_the_households_language_where_there_is_one() {
        let info = MediaInfo { streams: vec![stream(0, "video", "", true), stream(1, "audio", "jpn", true), stream(2, "audio", "eng", false)], ..Default::default() };
        assert_eq!(soundtrack(&info, "en"), Some(2));
        assert_eq!(soundtrack(&info, "fr"), Some(1));
        assert_eq!(soundtrack(&MediaInfo::default(), "en"), None);
    }

    #[test]
    fn hdr_is_brought_down_on_the_video_hardware_and_a_big_picture_made_1080p() {
        let hdr = MediaInfo { width: 3840, height: 2160, dynamic_range_type: "HDR10".into(), ..Default::default() };
        let a = arguments(&hdr, Path::new("/f.mkv"), Path::new("/c.mp4"), 1800.0, 30.0, "en", None).join(" ");
        assert!(a.contains("-hwaccel videotoolbox"));
        assert!(a.contains("scale_vt=w=1920:h=-2:color_matrix=bt709"));
        assert!(a.contains("-ss 1800.000 -i /f.mkv") && a.contains("-t 30.000 -c:v libx264"));
        let sdr = MediaInfo { width: 1920, height: 1080, ..Default::default() };
        let a = arguments(&sdr, Path::new("/f.mkv"), Path::new("/c.mp4"), 0.0, 30.0, "en", None).join(" ");
        assert!(!a.contains("videotoolbox"));
        assert!(a.contains("-vf format=yuv420p"));
        assert!(a.contains("-c:v libx264") && a.contains("+faststart"));
    }

    #[test]
    fn a_long_clip_makes_a_smaller_gif() {
        assert!(gif_filter(5.0, 1920, 1080).starts_with("fps=15,scale=480:-2"));
        assert!(gif_filter(10.0, 1920, 1080).starts_with("fps=15,scale=434:-2"));
        assert!(gif_filter(30.0, 1920, 1080).starts_with("fps=10,scale=306:-2"));
        assert!(gif_filter(120.0, 1920, 1080).starts_with("fps=10,scale=240:-2"));
        assert!(gif_filter(5.0, 300, 200).starts_with("fps=15,scale=300:-2"), "never larger than the clip");
    }

    #[test]
    fn subtitles_are_laid_over_the_picture_at_the_size_it_comes_out() {
        assert_eq!(out_size(3840, 2160), (1920, 1080));
        assert_eq!(out_size(3840, 1600), (1920, 800));
        assert_eq!(out_size(1440, 1080), (1440, 1080));
        let hdr = MediaInfo { width: 3840, height: 2160, dynamic_range_type: "HDR10".into(), ..Default::default() };
        let a = arguments(&hdr, Path::new("/f.mkv"), Path::new("/c.mp4"), 60.0, 30.0, "en", Some(&Over::Picture(4))).join(" ");
        assert!(a.contains("[0:4]scale=1920:1080[s];[v][s]overlay") && a.contains("-map [out]") && !a.contains("-vf"), "{a}");
        let sdr = MediaInfo { width: 1920, height: 1080, ..Default::default() };
        let a = arguments(&sdr, Path::new("/f.mkv"), Path::new("/c.mp4"), 60.0, 30.0, "en", Some(&Over::Drawn(PathBuf::from("/d/list.ffconcat")))).join(" ");
        assert!(a.contains("-f concat -safe 0 -i /d/list.ffconcat") && a.contains("y=main_h-overlay_h"), "{a}");
    }
}
