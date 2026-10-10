//! Subtitles: what each file has, finding what it lacks, and checking that a subtitle file
//! really is in time with the film.
//!
//! The check listens to the film. Speech shows up in the soundtrack as bursts of energy in the
//! voice band, and a subtitle file is a list of when someone is speaking. Slide one against the
//! other and, if they belong together, one position stands far above all the rest. Where that
//! position is tells how far out the subtitles are; how far it stands out tells whether they
//! belong to this film at all.

use crate::app::{App, Event};
use crate::db::now;
use crate::models::{Kind, MediaFile, Title};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Seconds of sound summed into one measurement.
const FRAME: f64 = 0.05;
/// How far either way a subtitle file may be out and still be found.
const MAX_SHIFT: f64 = 90.0;
/// At or above this, the subtitles belong to this soundtrack. Right subtitles score 15 to 30
/// on real films and wrong ones under 5.
const SURE: f64 = 8.0;
/// Below this they do not.
const NOT_IT: f64 = 5.0;
/// Subtitles this close to the speech need no correction.
const CLOSE_ENOUGH: f64 = 0.35;
/// Speeds a subtitle file may have been timed at, relative to the film: the same, or any pair
/// of the usual frame rates (23.976, 24 and 25 a second).
const RATES: [f64; 7] = [1.0, 25.0 / 23.976, 23.976 / 25.0, 24.0 / 23.976, 23.976 / 24.0, 25.0 / 24.0, 24.0 / 25.0];

#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// A subtitle file kept beside a video.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sidecar {
    /// Two-letter language code, or "und" when the name does not say.
    pub language: String,
    /// Path relative to the title folder.
    pub rel_path: String,
    /// Where it came from: "found" beside the video, "download" with the release, or "opensubtitles".
    pub source: String,
    /// The release the subtitles were made for, when known.
    pub release: String,
    pub hearing_impaired: bool,
    pub forced: bool,
    pub sync: Option<SyncReport>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// In time with the speech.
    InSync,
    /// Belongs to this film but is out by a known amount.
    Shifted,
    /// Does not line up with this soundtrack anywhere: for another cut, or another film.
    NoMatch,
    /// Too little to go on, as with a film that is mostly music or has very few lines.
    Unsure,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncReport {
    pub verdict: Verdict,
    /// Seconds to add to every time in the file to bring it into line.
    pub offset: f64,
    /// What every time in the file should be multiplied by first; 1 unless it was timed for a
    /// different frame rate.
    pub rate: f64,
    /// How far the best position stands above the rest.
    pub confidence: f64,
    pub checked_at: i64,
    /// The file was rewritten with the correction, and the figures above are from before it.
    #[serde(default)]
    pub corrected: bool,
}

// ---------------------------------------------------------------- subtitle files

fn clock(s: &str) -> Option<f64> {
    // 00:01:02,345, with a full stop for the comma in some files and the hours left off in others.
    let s = s.trim().replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    let nums: Option<Vec<f64>> = parts.iter().map(|p| p.trim().parse::<f64>().ok()).collect();
    match nums?.as_slice() {
        [h, m, sec] => Some(h * 3600.0 + m * 60.0 + sec),
        [m, sec] => Some(m * 60.0 + sec),
        _ => None,
    }
}

/// Read SubRip text, forgiving the usual faults: a byte-order mark, Windows line ends, missing
/// numbers, and position notes after the times.
pub fn parse_srt(text: &str) -> Vec<Cue> {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    let mut cues: Vec<Cue> = vec![];
    let mut current: Option<Cue> = None;
    for line in text.lines() {
        if let Some((a, b)) = line.split_once("-->") {
            let end = b.split_whitespace().next().unwrap_or("");
            if let (Some(start), Some(end)) = (clock(a), clock(end)) {
                if let Some(mut c) = current.take() {
                    // The line before a time line is the next cue's number, not this one's words.
                    if let Some(i) = c.text.rfind('\n') {
                        if c.text[i + 1..].trim().chars().all(|ch| ch.is_ascii_digit()) {
                            c.text.truncate(i);
                        }
                    } else if c.text.trim().chars().all(|ch| ch.is_ascii_digit()) {
                        c.text.clear();
                    }
                    cues.push(c);
                }
                current = Some(Cue { start, end: end.max(start), text: String::new() });
                continue;
            }
        }
        if let Some(c) = current.as_mut() {
            if !c.text.is_empty() {
                c.text.push('\n');
            }
            c.text.push_str(line);
        }
    }
    cues.extend(current);
    for c in &mut cues {
        c.text = c.text.trim().to_string();
    }
    cues.retain(|c| !c.text.is_empty());
    cues
}

fn stamp(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

pub fn write_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        out.push_str(&format!("{}\n{} --> {}\n{}\n\n", i + 1, stamp(c.start), stamp(c.end), c.text));
    }
    out
}

/// Move every cue to where the report says it belongs.
pub fn retime(cues: &[Cue], rate: f64, offset: f64) -> Vec<Cue> {
    cues.iter().map(|c| Cue { start: c.start * rate + offset, end: c.end * rate + offset, text: c.text.clone() }).filter(|c| c.end > 0.0).collect()
}

/// Subtitle files are often not UTF-8. Anything that is not is read as Windows Latin-1, which
/// is right for most Western files and at least keeps the times for the rest.
pub fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|b| *b as char).collect(),
    }
}

// ---------------------------------------------------------------- listening to the film

/// Turn raw sound, in frames of mean energy, into how much louder each moment is than its
/// surroundings. Speech comes and goes by the second; music and room tone move slowly, and
/// taking away a running median leaves the part that behaves like speech.
pub fn speech_shape(energy: &[f32]) -> Vec<f32> {
    let n = energy.len();
    if n == 0 {
        return vec![];
    }
    let window = (20.0 / FRAME) as usize;
    let step = (1.0 / FRAME) as usize;
    let mut floor_at: Vec<f32> = vec![];
    let mut buf: Vec<f32> = Vec::with_capacity(window);
    let mut i = 0;
    while i < n {
        let (a, b) = (i.saturating_sub(window / 2), (i + window / 2).min(n));
        buf.clear();
        buf.extend_from_slice(&energy[a..b]);
        let mid = buf.len() / 2;
        buf.select_nth_unstable_by(mid, |x, y| x.total_cmp(y));
        floor_at.push(buf[mid]);
        i += step;
    }
    let mut shape: Vec<f32> = (0..n)
        .map(|i| {
            let (k, frac) = (i / step, (i % step) as f32 / step as f32);
            let lo = floor_at[k.min(floor_at.len() - 1)];
            let hi = floor_at[(k + 1).min(floor_at.len() - 1)];
            (energy[i] - (lo + (hi - lo) * frac)).clamp(-2.0, 4.0)
        })
        .collect();
    let mean = shape.iter().sum::<f32>() / n as f32;
    for v in &mut shape {
        *v -= mean;
    }
    shape
}

/// Find where a set of cues sits best against the film's speech.
pub fn align(shape: &[f32], cues: &[Cue]) -> SyncReport {
    let n = shape.len();
    let unsure = SyncReport { verdict: Verdict::Unsure, offset: 0.0, rate: 1.0, confidence: 0.0, checked_at: now(), corrected: false };
    if n < (120.0 / FRAME) as usize || cues.len() < 20 {
        return unsure;
    }
    // Running total, so the speech under any cue is one subtraction.
    let mut total: Vec<f64> = Vec::with_capacity(n + 1);
    total.push(0.0);
    for v in shape {
        total.push(total.last().unwrap() + *v as f64);
    }
    let at = |i: i64| total[i.clamp(0, n as i64) as usize];
    let lags = (MAX_SHIFT / FRAME) as i64;
    let near = (3.0 / FRAME) as i64;
    let mut best: Option<(f64, f64, f64)> = None;
    for rate in RATES {
        let spans: Vec<(i64, i64)> = cues.iter().map(|c| ((c.start * rate / FRAME) as i64, (c.end * rate / FRAME) as i64)).filter(|(a, b)| b > a).collect();
        let scores: Vec<f64> = (-lags..=lags).map(|lag| spans.iter().map(|(a, b)| at(b + lag) - at(a + lag)).sum()).collect();
        let (peak_at, peak) = scores.iter().copied().enumerate().fold((0, f64::MIN), |m, (i, v)| if v > m.1 { (i, v) } else { m });
        // Everything more than a few seconds from the best position is what chance looks like.
        let rest: Vec<f64> = scores.iter().enumerate().filter(|(i, _)| (*i as i64 - peak_at as i64).abs() > near).map(|(_, v)| *v).collect();
        if rest.len() < 100 {
            continue;
        }
        let mean = rest.iter().sum::<f64>() / rest.len() as f64;
        let spread = (rest.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / rest.len() as f64).sqrt();
        let standing = if spread > 0.0 { (peak - mean) / spread } else { 0.0 };
        // A different speed has to earn its place: the plain reading wins a near tie.
        let handicap = if rate == 1.0 { 0.0 } else { 1.0 };
        if best.is_none_or(|b| standing - handicap > b.0) {
            best = Some((standing - handicap, (peak_at as i64 - lags) as f64 * FRAME, rate));
        }
    }
    let Some((confidence, offset, rate)) = best else { return unsure };
    let verdict = if confidence >= SURE {
        if rate == 1.0 && offset.abs() <= CLOSE_ENOUGH {
            Verdict::InSync
        } else {
            Verdict::Shifted
        }
    } else if confidence < NOT_IT {
        Verdict::NoMatch
    } else {
        Verdict::Unsure
    };
    SyncReport { verdict, offset: (offset * 100.0).round() / 100.0, rate, confidence: (confidence * 10.0).round() / 10.0, checked_at: now(), corrected: false }
}

/// The checksum OpenSubtitles knows files by: the size plus the first and last 64 KiB read as
/// 64-bit numbers. A match means subtitles timed against this very file.
pub fn movie_hash(path: &Path) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    const CHUNK: u64 = 65536;
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    if size < CHUNK * 2 {
        bail!("file too small to hash");
    }
    let mut hash = size;
    let mut buf = vec![0u8; CHUNK as usize];
    for start in [0, size - CHUNK] {
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut buf)?;
        for w in buf.chunks_exact(8) {
            hash = hash.wrapping_add(u64::from_le_bytes(w.try_into().unwrap()));
        }
    }
    Ok(format!("{hash:016x}"))
}

// ---------------------------------------------------------------- names and languages

const LANGUAGES: &[(&str, &str, &str)] = &[
    ("en", "eng", "english"),
    ("es", "spa", "spanish"),
    ("fr", "fre", "french"),
    ("de", "ger", "german"),
    ("it", "ita", "italian"),
    ("pt", "por", "portuguese"),
    ("nl", "dut", "dutch"),
    ("sv", "swe", "swedish"),
    ("no", "nor", "norwegian"),
    ("da", "dan", "danish"),
    ("fi", "fin", "finnish"),
    ("pl", "pol", "polish"),
    ("ru", "rus", "russian"),
    ("ja", "jpn", "japanese"),
    ("ko", "kor", "korean"),
    ("zh", "chi", "chinese"),
    ("ar", "ara", "arabic"),
    ("he", "heb", "hebrew"),
    ("tr", "tur", "turkish"),
    ("el", "gre", "greek"),
    ("cs", "cze", "czech"),
    ("hu", "hun", "hungarian"),
    ("ro", "rum", "romanian"),
    ("uk", "ukr", "ukrainian"),
    ("hi", "hin", "hindi"),
    ("th", "tha", "thai"),
    ("vi", "vie", "vietnamese"),
    ("id", "ind", "indonesian"),
];

/// A two-letter code from whatever a file or a stream calls its language.
pub fn language_code(word: &str) -> Option<&'static str> {
    let w = word.trim().to_lowercase();
    let alt = match w.as_str() {
        "fra" => "fre",
        "deu" => "ger",
        "zho" | "cht" | "chs" => "chi",
        "nld" => "dut",
        "ces" => "cze",
        "ron" => "rum",
        "ell" => "gre",
        "pt-br" | "pob" => "por",
        other => other,
    };
    LANGUAGES.iter().find(|(two, three, name)| *two == alt || *three == alt || *name == alt).map(|l| l.0)
}

pub fn language_name(code: &str) -> String {
    LANGUAGES.iter().find(|l| l.0 == code).map(|l| format!("{}{}", l.2[..1].to_uppercase(), &l.2[1..])).unwrap_or_else(|| if code == "und" { "Unknown language".into() } else { code.to_uppercase() })
}

/// What a subtitle file's name says about it, given the part after the video's own name:
/// ".en", ".eng.sdh", ".English.forced", or the whole name of a file in a Subs folder.
fn read_tags(tail: &str) -> (String, bool, bool) {
    let (mut language, mut sdh, mut forced) = (None, false, false);
    for word in tail.split(|c: char| !c.is_alphanumeric() && c != '-').filter(|w| !w.is_empty()) {
        let w = word.to_lowercase();
        match w.as_str() {
            "sdh" | "hi" | "cc" => sdh = true,
            "forced" | "foreign" => forced = true,
            _ => {
                if language.is_none() {
                    language = language_code(&w);
                }
            }
        }
    }
    (language.unwrap_or("und").to_string(), sdh, forced)
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

fn is_srt(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("srt"))
}

/// The languages a file already carries inside it.
pub fn embedded_languages(file: &MediaFile) -> Vec<String> {
    let mut out: Vec<String> = file.media_info.as_ref().map(|m| m.subtitles.iter().map(|s| language_code(s).unwrap_or("und").to_string()).collect()).unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// The wanted languages, as two-letter codes.
pub fn wanted_languages(setting: &str) -> Vec<String> {
    let mut out: Vec<String> = setting.split([',', ' ', ';']).filter_map(language_code).map(str::to_string).collect();
    out.dedup();
    out
}

// ---------------------------------------------------------------- OpenSubtitles

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Candidate {
    /// The provider's id for the file to download.
    pub file_id: i64,
    pub language: String,
    pub release: String,
    pub downloads: u64,
    pub hearing_impaired: bool,
    pub forced: bool,
    /// Timed against this exact video file.
    pub hash_match: bool,
    pub machine_translated: bool,
    /// Made for the same release group or source as the video.
    pub same_release: bool,
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 1).map(str::to_string).collect()
}

/// Best first: timed for this file, then made for the same release, then the most used.
/// Machine translations and captions for the hard of hearing come after their plain equals.
pub fn rank(mut list: Vec<Candidate>, scene_name: &str, group: Option<&str>) -> Vec<Candidate> {
    let mine = words(scene_name);
    let sources = ["bluray", "bdrip", "brrip", "remux", "web", "webrip", "webdl", "hdtv", "dvdrip", "dvd"];
    let my_source: Vec<&String> = mine.iter().filter(|w| sources.contains(&w.as_str())).collect();
    for c in &mut list {
        let theirs = words(&c.release);
        let same_group = group.is_some_and(|g| theirs.contains(&g.to_lowercase()));
        let same_source = !my_source.is_empty() && my_source.iter().any(|s| theirs.contains(s) || (s.starts_with("web") && theirs.iter().any(|t| t.starts_with("web"))));
        c.same_release = same_group || (same_source && theirs.iter().filter(|w| mine.contains(w)).count() >= 4);
    }
    list.sort_by(|a, b| {
        let key = |c: &Candidate| (c.hash_match, !c.machine_translated, c.same_release, !c.hearing_impaired, c.downloads);
        key(b).cmp(&key(a))
    });
    list
}

impl App {
    pub fn ffmpeg_path(&self) -> Option<PathBuf> {
        let configured = self.settings.general().ffprobe_path;
        if !configured.is_empty() {
            let beside = Path::new(&configured).with_file_name("ffmpeg");
            if beside.is_file() {
                return Some(beside);
            }
        }
        ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].iter().map(PathBuf::from).find(|p| p.is_file()).or_else(|| std::env::var("PATH").ok().and_then(|p| p.split(':').map(|d| Path::new(d).join("ffmpeg")).find(|p| p.is_file())))
    }

    /// Listen to a video's first soundtrack and return the shape of its speech.
    pub async fn listen(&self, video: &Path, channels: f64) -> Result<Vec<f32>> {
        use tokio::io::AsyncReadExt;
        let tool = self.ffmpeg_path().ok_or_else(|| anyhow!("ffmpeg is not installed, and checking subtitles needs it"))?;
        // Films with surround sound keep dialogue in the centre speaker, away from most music
        // and effects. Fall back to an ordinary mix-down if the layout has no centre.
        let filters = if channels >= 5.0 { vec!["pan=mono|c0=FC,highpass=f=200,lowpass=f=3500", "highpass=f=200,lowpass=f=3500"] } else { vec!["highpass=f=200,lowpass=f=3500"] };
        const RATE: usize = 8000;
        let per_frame = (RATE as f64 * FRAME) as usize;
        for filter in filters {
            let mut child = tokio::process::Command::new(&tool)
                .args(["-v", "error", "-nostdin", "-i"])
                .arg(video)
                .args(["-map", "0:a:0", "-vn", "-sn", "-af", filter, "-ac", "1", "-ar", &RATE.to_string(), "-f", "s16le", "-"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .context("starting ffmpeg")?;
            let mut out = child.stdout.take().ok_or_else(|| anyhow!("no output from ffmpeg"))?;
            let mut energy: Vec<f32> = vec![];
            let mut buf = vec![0u8; 1 << 16];
            let mut carry: Vec<u8> = vec![];
            let (mut sum, mut count) = (0f64, 0usize);
            loop {
                let got = out.read(&mut buf).await?;
                if got == 0 {
                    break;
                }
                carry.extend_from_slice(&buf[..got]);
                let whole = carry.len() / 2 * 2;
                for s in carry[..whole].chunks_exact(2) {
                    let v = i16::from_le_bytes([s[0], s[1]]) as f64;
                    sum += v * v;
                    count += 1;
                    if count == per_frame {
                        energy.push((sum / per_frame as f64).ln_1p() as f32);
                        (sum, count) = (0.0, 0);
                    }
                }
                carry.drain(..whole);
            }
            let _ = child.wait().await;
            // Anything at all will do here; whether it is enough to judge by is decided later.
            if !energy.is_empty() {
                return Ok(tokio::task::spawn_blocking(move || speech_shape(&energy)).await?);
            }
        }
        bail!("could not read the soundtrack")
    }

    fn file_and_title(&self, file_id: i64) -> Result<(MediaFile, Title)> {
        let file = self.db.file(file_id)?.ok_or_else(|| anyhow!("no such file"))?;
        let title = self.db.title(file.title_id)?.ok_or_else(|| anyhow!("no such title"))?;
        Ok((file, title))
    }

    fn save_file_data(&self, file: &MediaFile) -> Result<()> {
        self.db.with(|c| c.execute("UPDATE files SET data = ?2 WHERE id = ?1", rusqlite::params![file.id, serde_json::to_string(file).unwrap_or_default()]))?;
        self.emit(Event::Title { id: file.title_id });
        Ok(())
    }

    /// Look beside a video for subtitle files and remember them. Files Spool already knows keep
    /// what it has learned about them; ones that have gone are forgotten.
    pub fn find_sidecars(&self, file_id: i64) -> Result<Vec<Sidecar>> {
        let (mut file, title) = self.file_and_title(file_id)?;
        let video = Path::new(&title.path).join(&file.rel_path);
        let Some(dir) = video.parent() else { return Ok(file.subtitles) };
        let name = stem(&video);
        let mut found: Vec<Sidecar> = vec![];
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = entry.path();
            let s = stem(&p);
            if !is_srt(&p) || !s.starts_with(&name) {
                continue;
            }
            let rel = p.strip_prefix(&title.path).unwrap_or(&p).to_string_lossy().to_string();
            let (language, hearing_impaired, forced) = read_tags(&s[name.len()..]);
            let known = file.subtitles.iter().find(|k| k.rel_path == rel).cloned();
            found.push(known.unwrap_or(Sidecar { language, rel_path: rel, source: "found".into(), release: String::new(), hearing_impaired, forced, sync: None }));
        }
        found.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        if found != file.subtitles {
            file.subtitles = found.clone();
            self.save_file_data(&file)?;
        }
        Ok(found)
    }

    /// Bring a download's subtitle files into the library beside the video they belong to.
    /// Called once the video is in place and before the download folder is cleared.
    pub fn carry_subtitles(&self, download_root: &Path, src_video: &Path, dst_video: &Path, only_video: bool) -> usize {
        let name = stem(src_video);
        let mut srts: Vec<PathBuf> = vec![];
        let mut stack = vec![download_root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if is_srt(&p) {
                    srts.push(p);
                }
            }
        }
        let mut carried = 0;
        let mut used: Vec<String> = vec![];
        for p in srts {
            let s = stem(&p);
            // Named after the video, in a folder named after it, or the download holds one video.
            let in_own_folder = p.parent().and_then(|d| d.file_name()).is_some_and(|d| d.to_string_lossy() == name);
            let tail = if s.starts_with(&name) {
                s[name.len()..].to_string()
            } else if in_own_folder || only_video {
                s.clone()
            } else {
                continue;
            };
            let (language, sdh, forced) = read_tags(&tail);
            let mut tag = language.clone();
            if sdh {
                tag.push_str(".sdh");
            }
            if forced {
                tag.push_str(".forced");
            }
            if used.contains(&tag) {
                continue;
            }
            let dst = dst_video.with_file_name(format!("{}.{tag}.srt", stem(dst_video)));
            if !dst.exists() && std::fs::copy(&p, &dst).is_ok() {
                used.push(tag);
                carried += 1;
            }
        }
        carried
    }

    /// Remove a subtitle file and forget it.
    pub fn delete_sidecar(&self, file_id: i64, rel_path: &str) -> Result<()> {
        let (mut file, title) = self.file_and_title(file_id)?;
        if !file.subtitles.iter().any(|s| s.rel_path == rel_path) {
            bail!("no such subtitle file");
        }
        let _ = std::fs::remove_file(Path::new(&title.path).join(rel_path));
        file.subtitles.retain(|s| s.rel_path != rel_path);
        self.save_file_data(&file)
    }

    /// Check one subtitle file against the film's speech, correcting it when it is out by a
    /// known amount and `correct` is set.
    pub async fn check_sidecar(&self, file_id: i64, rel_path: &str, correct: bool) -> Result<SyncReport> {
        let (file, title) = self.file_and_title(file_id)?;
        let video = Path::new(&title.path).join(&file.rel_path);
        let shape = self.listen(&video, file.media_info.as_ref().map(|m| m.audio_channels).unwrap_or(2.0)).await?;
        self.check_against(file_id, rel_path, &shape, correct)
    }

    fn check_against(&self, file_id: i64, rel_path: &str, shape: &[f32], correct: bool) -> Result<SyncReport> {
        let (mut file, title) = self.file_and_title(file_id)?;
        let path = Path::new(&title.path).join(rel_path);
        let cues = parse_srt(&decode(&std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?));
        if cues.is_empty() {
            bail!("the subtitle file has no readable lines");
        }
        let mut report = align(shape, &cues);
        if correct && report.verdict == Verdict::Shifted {
            let fixed = retime(&cues, report.rate, report.offset);
            // Only keep the correction if the corrected file then checks out.
            if align(shape, &fixed).verdict == Verdict::InSync {
                std::fs::write(&path, write_srt(&fixed))?;
                report.corrected = true;
                tracing::info!(file = %rel_path, offset = report.offset, rate = report.rate, "corrected subtitle timing");
            }
        }
        if let Some(s) = file.subtitles.iter_mut().find(|s| s.rel_path == rel_path) {
            s.sync = Some(report.clone());
            self.save_file_data(&file)?;
        }
        Ok(report)
    }

    // ------------------------------------------------------------ finding subtitles

    fn opensubtitles(&self, method: reqwest::Method, path: &str) -> Result<reqwest::RequestBuilder> {
        let g = self.settings.general();
        if g.opensubtitles_api_key.is_empty() {
            bail!("no OpenSubtitles API key is set; add one in Settings");
        }
        let base = if g.opensubtitles_url.is_empty() { "https://api.opensubtitles.com/api/v1" } else { g.opensubtitles_url.trim_end_matches('/') };
        Ok(self.http.request(method, format!("{base}{path}")).header("Api-Key", &g.opensubtitles_api_key).header("User-Agent", concat!("Spool v", env!("CARGO_PKG_VERSION"))).header("Accept", "application/json"))
    }

    /// Ask OpenSubtitles what it has for a file, best first.
    pub async fn search_subtitles(&self, file_id: i64, language: &str) -> Result<Vec<Candidate>> {
        let (file, title) = self.file_and_title(file_id)?;
        let req = self.opensubtitles(reqwest::Method::GET, "/subtitles")?;
        // The service wants parameters in alphabetical order and lower case.
        let mut q: Vec<(String, String)> = vec![("languages".into(), language.to_lowercase())];
        match title.kind {
            Kind::Movie => {
                if let Some(id) = title.tmdb_id {
                    q.push(("tmdb_id".into(), id.to_string()));
                } else if let Some(imdb) = &title.imdb_id {
                    q.push(("imdb_id".into(), imdb.trim_start_matches("tt").trim_start_matches('0').to_string()));
                } else {
                    q.push(("query".into(), title.title.to_lowercase()));
                }
            }
            Kind::Series => {
                let ep = self.db.episodes(title.id)?.into_iter().find(|e| e.file_id == Some(file_id)).ok_or_else(|| anyhow!("this file is not matched to an episode"))?;
                q.push(("episode_number".into(), ep.episode.to_string()));
                q.push(("season_number".into(), ep.season.to_string()));
                match (title.tmdb_id, &title.imdb_id) {
                    (Some(id), _) => q.push(("parent_tmdb_id".into(), id.to_string())),
                    (_, Some(imdb)) => q.push(("parent_imdb_id".into(), imdb.trim_start_matches("tt").trim_start_matches('0').to_string())),
                    _ => q.push(("query".into(), title.title.to_lowercase())),
                }
            }
        }
        let video = Path::new(&title.path).join(&file.rel_path);
        if let Ok(Ok(hash)) = tokio::task::spawn_blocking(move || movie_hash(&video)).await {
            q.push(("moviehash".into(), hash));
        }
        q.sort();
        let resp = req.query(&q).send().await.map_err(|e| anyhow!("OpenSubtitles: {}", e.without_url()))?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        if !status.is_success() {
            bail!("OpenSubtitles says: {}", body["message"].as_str().or(body["errors"][0].as_str()).unwrap_or(status.as_str()));
        }
        let list: Vec<Candidate> = body["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|d| {
                        let at = &d["attributes"];
                        Some(Candidate {
                            file_id: at["files"][0]["file_id"].as_i64()?,
                            language: language_code(at["language"].as_str().unwrap_or(language)).unwrap_or("und").to_string(),
                            release: at["release"].as_str().unwrap_or_default().to_string(),
                            downloads: at["download_count"].as_u64().unwrap_or(0),
                            hearing_impaired: at["hearing_impaired"].as_bool().unwrap_or(false),
                            forced: at["foreign_parts_only"].as_bool().unwrap_or(false),
                            hash_match: at["moviehash_match"].as_bool().unwrap_or(false),
                            machine_translated: at["ai_translated"].as_bool().unwrap_or(false) || at["machine_translated"].as_bool().unwrap_or(false),
                            same_release: false,
                        })
                    })
                    .filter(|c| !c.forced)
                    .collect()
            })
            .unwrap_or_default();
        Ok(rank(list, file.scene_name.as_deref().unwrap_or(&file.rel_path), file.release_group.as_deref()))
    }

    async fn opensubtitles_token(&self) -> Result<Option<String>> {
        let g = self.settings.general();
        if g.opensubtitles_username.is_empty() {
            return Ok(None);
        }
        if let Some((token, at)) = self.subtitle_login.lock().clone() {
            if now() - at < 20 * 3600 {
                return Ok(Some(token));
            }
        }
        let resp = self.opensubtitles(reqwest::Method::POST, "/login")?.json(&serde_json::json!({"username": g.opensubtitles_username, "password": g.opensubtitles_password})).send().await.map_err(|e| anyhow!("OpenSubtitles: {}", e.without_url()))?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        let Some(token) = body["token"].as_str() else { bail!("OpenSubtitles would not sign in: {}", body["message"].as_str().unwrap_or(status.as_str())) };
        *self.subtitle_login.lock() = Some((token.to_string(), now()));
        Ok(Some(token.to_string()))
    }

    /// Fetch one subtitle file from OpenSubtitles. Each fetch counts against the account's
    /// daily allowance.
    async fn fetch_subtitle(&self, provider_file_id: i64) -> Result<Vec<u8>> {
        let token = self.opensubtitles_token().await?;
        let mut req = self.opensubtitles(reqwest::Method::POST, "/download")?.json(&serde_json::json!({"file_id": provider_file_id, "sub_format": "srt"}));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await.map_err(|e| anyhow!("OpenSubtitles: {}", e.without_url()))?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        let Some(link) = body["link"].as_str() else { bail!("OpenSubtitles would not hand over the file: {}", body["message"].as_str().unwrap_or(status.as_str())) };
        let bytes = self.http.get(link).send().await.map_err(|e| anyhow!("OpenSubtitles: {}", e.without_url()))?.error_for_status().map_err(|e| anyhow!("OpenSubtitles: {}", e.without_url()))?.bytes().await?;
        Ok(bytes.to_vec())
    }

    /// Download a chosen subtitle, put it beside the video and check it against the speech.
    /// With `shape` given the film has been listened to already.
    pub async fn add_subtitle(&self, file_id: i64, c: &Candidate, shape: Option<&[f32]>) -> Result<(Sidecar, SyncReport)> {
        let bytes = self.fetch_subtitle(c.file_id).await?;
        let cues = parse_srt(&decode(&bytes));
        if cues.is_empty() {
            bail!("what OpenSubtitles sent is not a readable subtitle file");
        }
        let (mut file, title) = self.file_and_title(file_id)?;
        let video = Path::new(&title.path).join(&file.rel_path);
        let tag = if c.hearing_impaired { format!("{}.sdh", c.language) } else { c.language.clone() };
        let path = video.with_file_name(format!("{}.{tag}.srt", stem(&video)));
        // Written out as clean UTF-8 whatever it arrived as.
        std::fs::write(&path, write_srt(&cues))?;
        let rel = path.strip_prefix(&title.path).unwrap_or(&path).to_string_lossy().to_string();
        file.subtitles.retain(|s| s.rel_path != rel);
        let side = Sidecar { language: c.language.clone(), rel_path: rel.clone(), source: "opensubtitles".into(), release: c.release.clone(), hearing_impaired: c.hearing_impaired, forced: false, sync: None };
        file.subtitles.push(side.clone());
        self.save_file_data(&file)?;
        let report = match shape {
            Some(s) => self.check_against(file_id, &rel, s, true)?,
            None => self.check_sidecar(file_id, &rel, true).await?,
        };
        Ok((Sidecar { sync: Some(report.clone()), ..side }, report))
    }

    /// Find subtitles in one language for one file and keep the first that proves to be in
    /// time, trying a few. Returns what was kept, if anything.
    pub async fn auto_subtitle(&self, file_id: i64, language: &str) -> Result<Option<Sidecar>> {
        const TRIES: usize = 3;
        let candidates = self.search_subtitles(file_id, language).await?;
        if candidates.is_empty() {
            return Ok(None);
        }
        let (file, title) = self.file_and_title(file_id)?;
        let video = Path::new(&title.path).join(&file.rel_path);
        // A film that cannot be listened to can still take subtitles timed for this exact file.
        let shape = self.listen(&video, file.media_info.as_ref().map(|m| m.audio_channels).unwrap_or(2.0)).await.unwrap_or_default();
        for c in candidates.iter().take(TRIES) {
            let (side, report) = self.add_subtitle(file_id, c, Some(&shape)).await?;
            match report.verdict {
                Verdict::InSync => return Ok(Some(side)),
                Verdict::Shifted if report.corrected => return Ok(Some(side)),
                // Timed against this very file by its checksum: trust it where the sound cannot say.
                Verdict::Unsure if c.hash_match => return Ok(Some(side)),
                _ => {
                    tracing::info!(title = %title.title, release = %c.release, verdict = ?report.verdict, "subtitles did not fit; trying the next");
                    self.delete_sidecar(file_id, &side.rel_path)?;
                }
            }
        }
        Ok(None)
    }

    /// Files that lack subtitles in a wanted language, with the languages each is missing.
    /// A film in a wanted language spoken throughout still gets them: that is what they are for.
    pub fn missing_subtitles(&self) -> Result<Vec<(i64, Vec<String>)>> {
        let wanted = wanted_languages(&self.settings.general().subtitle_languages);
        if wanted.is_empty() {
            return Ok(vec![]);
        }
        let mut out = vec![];
        for t in self.db.titles(None)? {
            for f in self.db.files(t.id)? {
                let have: Vec<String> = embedded_languages(&f).into_iter().chain(f.subtitles.iter().map(|s| s.language.clone())).collect();
                let missing: Vec<String> = wanted.iter().filter(|w| !have.contains(w)).cloned().collect();
                if !missing.is_empty() {
                    out.push((f.id, missing));
                }
            }
        }
        Ok(out)
    }

    /// The "subtitles" task: look for what is missing, a few files a run, newest files first,
    /// so the daily allowance goes to what was just added.
    pub async fn subtitle_sweep(&self) -> Result<String> {
        const PER_RUN: usize = 6;
        let g = self.settings.general();
        if g.opensubtitles_api_key.is_empty() {
            return Ok("skipped: no OpenSubtitles key is set".into());
        }
        if wanted_languages(&g.subtitle_languages).is_empty() {
            return Ok("skipped: no subtitle languages are chosen".into());
        }
        if !self.volume_ok() {
            return Ok("skipped: the media volume is not mounted".into());
        }
        let mut missing = self.missing_subtitles()?;
        let total = missing.len();
        // Files already searched lately wait their turn.
        let tried: std::collections::HashMap<i64, i64> = self.db.get_setting("subtitle_tried");
        missing.retain(|(id, _)| now() - tried.get(id).copied().unwrap_or(0) > 7 * 86400);
        missing.sort_by_key(|(id, _)| std::cmp::Reverse(*id));
        let (mut found, mut looked) = (0, 0);
        let mut tried = tried;
        for (file_id, languages) in missing.into_iter().take(PER_RUN) {
            // A file put there by hand since the last look counts.
            let beside = self.find_sidecars(file_id).unwrap_or_default();
            let languages: Vec<String> = languages.into_iter().filter(|l| !beside.iter().any(|s| &s.language == l)).collect();
            if languages.is_empty() {
                continue;
            }
            looked += 1;
            tried.insert(file_id, now());
            for language in languages {
                match self.auto_subtitle(file_id, &language).await {
                    Ok(Some(_)) => found += 1,
                    Ok(None) => {}
                    Err(e) => {
                        let _ = self.db.set_setting("subtitle_tried", &tried);
                        return Ok(format!("{total} files lack subtitles; found {found} before stopping: {e}"));
                    }
                }
            }
        }
        tried.retain(|_, at| now() - *at < 30 * 86400);
        self.db.set_setting("subtitle_tried", &tried)?;
        Ok(format!("{total} files lack subtitles, looked for {looked}, found {found} in time with the film"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_subrip() {
        let text = "\u{feff}1\r\n00:00:01,000 --> 00:00:02,500 X1:10\r\nHello\r\nthere\r\n\r\n2\r\n00:01:02.345 --> 00:01:04.000\r\n<i>Bye</i>\r\n\r\n00:02:00,000 --> 00:02:01,000\r\n12\r\n";
        let cues = parse_srt(text);
        assert_eq!(cues.len(), 3);
        assert_eq!(cues[0], Cue { start: 1.0, end: 2.5, text: "Hello\nthere".into() });
        assert_eq!((cues[1].start, cues[1].text.as_str()), (62.345, "<i>Bye</i>"));
        assert_eq!(cues[2].text, "12");
        let again = parse_srt(&write_srt(&cues));
        assert_eq!(again, cues);
        let moved = retime(&cues, 1.0, -1.5);
        assert_eq!((moved[0].start, moved[0].end), (-0.5, 1.0));
        assert_eq!(stamp(3723.4567), "01:02:03,457");
    }

    #[test]
    fn reads_languages_from_names() {
        assert_eq!(read_tags(".en"), ("en".into(), false, false));
        assert_eq!(read_tags(".eng.sdh"), ("en".into(), true, false));
        assert_eq!(read_tags(".English.forced"), ("en".into(), false, true));
        assert_eq!(read_tags("2_Spanish"), ("es".into(), false, false));
        assert_eq!(read_tags(""), ("und".into(), false, false));
        assert_eq!(wanted_languages("en, spanish;fre"), vec!["en", "es", "fr"]);
        assert_eq!(language_name("en"), "English");
    }

    /// A made-up film: speech in irregular bursts over a slowly swelling score, with noise.
    fn film(minutes: usize, seed: u64) -> (Vec<f32>, Vec<Cue>) {
        let n = (minutes as f64 * 60.0 / FRAME) as usize;
        let mut state = seed;
        let mut rnd = move || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 33) as f64) / (1u64 << 31) as f64
        };
        let mut energy: Vec<f32> = (0..n).map(|i| 6.0 + 1.5 * ((i as f64) * FRAME / 90.0).sin() as f32).collect();
        let mut cues = vec![];
        let mut t = 5.0;
        while t < minutes as f64 * 60.0 - 10.0 {
            let len = 1.0 + rnd() * 3.0;
            cues.push(Cue { start: t, end: t + len, text: "line".into() });
            for i in (t / FRAME) as usize..((t + len) / FRAME) as usize {
                energy[i] += 2.0;
            }
            t += len + 0.5 + rnd() * 9.0;
        }
        for v in &mut energy {
            *v += (rnd() as f32 - 0.5) * 1.5;
        }
        (speech_shape(&energy), cues)
    }

    #[test]
    fn finds_how_far_out_subtitles_are() {
        let (shape, cues) = film(40, 7);
        let r = align(&shape, &cues);
        assert_eq!(r.verdict, Verdict::InSync, "{r:?}");

        // Late by seven and a bit seconds: found, and the correction puts it right.
        let late = retime(&cues, 1.0, 7.3);
        let r = align(&shape, &late);
        assert_eq!(r.verdict, Verdict::Shifted, "{r:?}");
        assert!((r.offset + 7.3).abs() < 0.11 && r.rate == 1.0, "{r:?}");
        assert_eq!(align(&shape, &retime(&late, r.rate, r.offset)).verdict, Verdict::InSync);

        // Timed for a film running at 25 frames a second, and early as well.
        let pal = retime(&cues, 23.976 / 25.0, -4.0);
        let r = align(&shape, &pal);
        assert_eq!(r.verdict, Verdict::Shifted, "{r:?}");
        assert!((r.rate - 25.0 / 23.976).abs() < 1e-9, "{r:?}");
        assert_eq!(align(&shape, &retime(&pal, r.rate, r.offset)).verdict, Verdict::InSync);
    }

    #[test]
    fn subtitles_for_another_film_do_not_match() {
        let (shape, _) = film(40, 7);
        let (_, other) = film(40, 99);
        let r = align(&shape, &other);
        assert_eq!(r.verdict, Verdict::NoMatch, "{r:?}");
        // Too little to judge by.
        assert_eq!(align(&shape, &other[..5]).verdict, Verdict::Unsure);
    }

    #[test]
    fn ranks_exact_matches_first() {
        let c = |file_id, release: &str, downloads, hash_match, machine_translated| Candidate { file_id, release: release.into(), downloads, hash_match, machine_translated, language: "en".into(), ..Default::default() };
        let list = vec![c(1, "Film.2001.DVDRip.XviD-OLD", 9000, false, false), c(2, "Film.2001.1080p.BluRay.x264-GRP", 40, false, false), c(3, "Film.2001.WEB", 5, true, false), c(4, "Film.2001.1080p.BluRay.x264-GRP", 99999, false, true)];
        let ranked = rank(list, "Film.2001.1080p.BluRay.x264-GRP", Some("GRP"));
        assert_eq!(ranked.iter().map(|c| c.file_id).collect::<Vec<_>>(), vec![3, 2, 1, 4]);
        assert!(ranked[1].same_release && !ranked[2].same_release);
    }
}
