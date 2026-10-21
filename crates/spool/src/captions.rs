//! Subtitles drawn into a clip.
//!
//! Which ones are drawn follows the rule a player is given: in the household's language when
//! what is heard is another, and otherwise only the lines a film always shows, signs and words
//! in another tongue. Or always, or never, or in a language asked for.
//!
//! Subtitles that are text, inside the file or beside it, are read for the clip's stretch of
//! time and drawn here, white with a dark edge, as pictures the width of the frame. ffmpeg lays
//! them over the picture, so nothing depends on how its own text drawing was built. Subtitles
//! that are already pictures, as on a disc, are laid over as they are.

use crate::subs::{Cue, Sidecar};
use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use anyhow::{anyhow, Result};
use spool_core::mediainfo::{MediaInfo, Stream};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const FONT: &[u8] = include_bytes!("../fonts/Roboto-Regular.ttf");

/// Where the subtitles of a clip come from.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A track of text inside the file, by its stream number.
    Text(u32),
    /// A file of text beside the video.
    Beside(PathBuf),
    /// A track of pictures inside the file, as on a disc.
    Picture(u32),
}

/// The subtitles chosen for a clip, and their language as two letters.
#[derive(Clone, Debug, PartialEq)]
pub struct Chosen {
    pub source: Source,
    pub language: String,
}

pub(crate) fn is_text(codec: &str) -> bool {
    matches!(codec, "subrip" | "srt" | "ass" | "ssa" | "mov_text" | "webvtt" | "text")
}

fn is_picture(codec: &str) -> bool {
    matches!(codec, "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle")
}

/// A track of only the words shown on screen, signs and songs, though not marked forced, as
/// anime often has beside the full subtitles.
pub(crate) fn only_signs(s: &Stream) -> bool {
    let t = s.title.to_lowercase();
    t.contains("sign") || t.contains("song") || t.contains("forced")
}

pub(crate) fn for_the_hard_of_hearing(s: &Stream) -> bool {
    let t = s.title.to_lowercase();
    t.contains("sdh") || t.contains("hearing") || t.split(|c: char| !c.is_alphanumeric()).any(|w| w == "cc")
}

/// Choose the subtitles to draw. `wanted` is "auto", "none", "on" (in the household's
/// language), or a language. `heard` is the language of the soundtrack the clip has.
pub fn choose(info: &MediaInfo, beside: &[(Sidecar, PathBuf)], heard: Option<&str>, household: &str, wanted: &str) -> Option<Chosen> {
    let (language, full) = match wanted.trim().to_lowercase().as_str() {
        "none" | "off" => return None,
        "" | "auto" => (household.to_string(), heard.is_some_and(|h| h != household)),
        "on" => (household.to_string(), true),
        other => (crate::subs::language_code(other)?.to_string(), true),
    };
    if language.is_empty() {
        return None;
    }
    let ours = |code: &str| crate::subs::language_code(code) == Some(language.as_str());
    // Each candidate with how well it suits: full subtitles before those for the hard of
    // hearing, and lines a film always shows only when nothing else is there or wanted.
    let mut found: Vec<((u8, u8, u8), Source)> = vec![];
    for s in info.streams.iter().filter(|s| s.kind == "subtitle" && ours(&s.language)) {
        let kind = if is_text(&s.codec) { 0 } else if is_picture(&s.codec) { 2 } else { continue };
        found.push((((s.forced || only_signs(s)) as u8, for_the_hard_of_hearing(s) as u8, kind), if kind == 0 { Source::Text(s.index) } else { Source::Picture(s.index) }));
    }
    for (s, path) in beside.iter().filter(|(s, _)| ours(&s.language)) {
        found.push(((s.forced as u8, s.hearing_impaired as u8, 1), Source::Beside(path.clone())));
    }
    if !full {
        found.retain(|(rank, _)| rank.0 == 1);
    }
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().next().map(|(_, source)| Chosen { source, language })
}

/// A subtitle's words without what it says about how to show them: tags, and the codes in
/// braces that some files carry.
pub fn clean(text: &str) -> String {
    let mut out = String::new();
    let (mut tag, mut code) = (false, false);
    for c in text.replace("\\N", "\n").replace("\\n", "\n").replace("\\h", " ").chars() {
        match c {
            '<' if !code => tag = true,
            '>' if tag => tag = false,
            '{' if !tag => code = true,
            '}' if code => code = false,
            _ if tag || code => {}
            _ => out.push(c),
        }
    }
    out.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

/// The lines of a clip's stretch of time, timed from the start of the clip. Stops, and says
/// so, when `stop` is set.
pub async fn cues(tool: &Path, src: &Path, source: &Source, start: f64, seconds: f64, stop: &AtomicBool) -> Result<Vec<Cue>> {
    let all: Vec<Cue> = match source {
        Source::Text(index) => {
            // Read from a little before the clip, with the file's own times: ffmpeg's times
            // counted from where it was asked to begin shift when a line is up at that moment.
            let from = (start - 10.0).max(0.0);
            let window = ["-ss".to_string(), format!("{from:.3}"), "-t".to_string(), format!("{:.3}", seconds + (start - from))];
            read_track(tool, src, *index, &window, stop).await?
        }
        Source::Beside(path) => crate::subs::parse_srt(&crate::subs::decode(&tokio::fs::read(path).await?)),
        Source::Picture(_) => return Ok(vec![]),
    };
    let read = all.into_iter().filter(|c| c.end > start && c.start < start + seconds).map(|c| Cue { start: c.start - start, end: c.end - start, text: c.text });
    Ok(read
        .into_iter()
        .map(|c| Cue { start: c.start.max(0.0), end: c.end.min(seconds), text: clean(&c.text) })
        .filter(|c| c.end > c.start && !c.text.is_empty())
        .collect())
}

/// The streams of a file as [`crate::mkv::track`] takes them: each one's number, kind and codec.
pub fn streams_of(info: &MediaInfo) -> Vec<(u32, String, String)> {
    info.streams.iter().map(|s| (s.index, s.kind.clone(), s.codec.clone())).collect()
}

/// Every line of subtitles that are text, from start to end, cleaned of how to show them.
/// Reading a track inside a file reads the whole file.
/// `streams` are the file's streams, as [`crate::mkv::track`] takes them.
pub async fn every_line(tool: &Path, src: &Path, source: &Source, streams: &[(u32, String, String)], stop: &AtomicBool) -> Result<Vec<Cue>> {
    let all = match source {
        Source::Text(index) => {
            // Where the file's index says where each line is, only the lines are read.
            let (path, index, streams) = (src.to_path_buf(), *index, streams.to_vec());
            match tokio::task::spawn_blocking(move || crate::mkv::track(&path, index, &streams)).await? {
                Some(cues) => cues,
                None => read_track(tool, src, index, &[], stop).await?,
            }
        }
        Source::Beside(path) => crate::subs::parse_srt(&crate::subs::decode(&tokio::fs::read(path).await?)),
        Source::Picture(_) => return Ok(vec![]),
    };
    Ok(all.into_iter().map(|c| Cue { text: clean(&c.text), ..c }).filter(|c| c.end > c.start && !c.text.is_empty()).collect())
}

/// A track of text subtitles inside a file, read by ffmpeg in the file's own times, from
/// where `window` says or from the start.
async fn read_track(tool: &Path, src: &Path, index: u32, window: &[String], stop: &AtomicBool) -> Result<Vec<Cue>> {
    let mut child = tokio::process::Command::new(tool)
        .args(["-nostdin", "-v", "error"])
        .args(window)
        .arg("-i")
        .arg(src)
        .args(["-copyts", "-map", &format!("0:{index}"), "-f", "srt", "pipe:1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let (mut out, mut err) = (child.stdout.take().ok_or_else(|| anyhow!("no output from ffmpeg"))?, child.stderr.take().ok_or_else(|| anyhow!("no output from ffmpeg"))?);
    let read = async move {
        use tokio::io::AsyncReadExt;
        let (mut text, mut said) = (Vec::new(), Vec::new());
        let _ = tokio::join!(out.read_to_end(&mut text), err.read_to_end(&mut said));
        (text, said)
    };
    let watch = async {
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    };
    let (text, said) = tokio::select! {
        biased;
        _ = watch => return Err(anyhow!(crate::compact::CALLED_OFF)),
        got = read => got,
    };
    if !child.wait().await?.success() {
        return Err(anyhow!("the subtitles could not be read: {}", String::from_utf8_lossy(&said).lines().last().unwrap_or("").trim()));
    }
    Ok(crate::subs::parse_srt(&String::from_utf8_lossy(&text)))
}

/// What is on screen from moment to moment: the times at which it changes, each with the
/// words shown until the next. Lines shown together are put one above another.
fn timeline(cues: &[Cue], seconds: f64) -> Vec<(f64, String)> {
    let mut times: Vec<f64> = cues.iter().flat_map(|c| [c.start, c.end]).chain([0.0, seconds]).filter(|t| (0.0..=seconds).contains(t)).collect();
    times.sort_by(f64::total_cmp);
    times.dedup_by(|a, b| (*a - *b).abs() < 0.001);
    let mut out: Vec<(f64, String)> = vec![];
    for pair in times.windows(2) {
        let mid = (pair[0] + pair[1]) / 2.0;
        let words = cues.iter().filter(|c| c.start <= mid && mid < c.end).map(|c| c.text.as_str()).collect::<Vec<_>>().join("\n");
        if out.last().is_none_or(|(_, last)| *last != words) {
            out.push((pair[0], words));
        }
    }
    out
}

/// Draw words as they are shown on screen, centred at the foot of a band as wide as the
/// picture: an RGBA image of `width` by `band`, for a picture `height` high.
fn draw(font: &FontRef, words: &str, width: u32, band: u32, height: u32) -> image::RgbaImage {
    let size = (height as f32 * 0.052).max(14.0);
    let scaled = font.as_scaled(PxScale::from(size));
    let edge = (size * 0.09).max(1.5);
    let room = width as f32 * 0.88;
    let advance = |text: &str| {
        let mut w = 0.0;
        let mut last = None;
        for c in text.chars() {
            let id = scaled.glyph_id(c);
            if id.0 == 0 {
                continue;
            }
            if let Some(prev) = last {
                w += scaled.kern(prev, id);
            }
            w += scaled.h_advance(id);
            last = Some(id);
        }
        w
    };
    // Each line as written, broken again where it is wider than the picture allows.
    let mut lines: Vec<String> = vec![];
    for given in words.lines() {
        let mut line = String::new();
        for word in given.split_whitespace() {
            let longer = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if !line.is_empty() && advance(&longer) > room {
                lines.push(std::mem::replace(&mut line, word.to_string()));
            } else {
                line = longer;
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    let (w, h) = (width as usize, band as usize);
    let mut fill = vec![0f32; w * h];
    let step = size * 1.22;
    let bottom = band as f32 - height as f32 * 0.06;
    for (i, line) in lines.iter().enumerate() {
        let baseline = bottom - (lines.len() - 1 - i) as f32 * step - scaled.descent().abs();
        let mut x = (width as f32 - advance(line)) / 2.0;
        let mut last = None;
        for c in line.chars() {
            // A letter the font does not have is left out, not drawn as an empty box.
            let id = scaled.glyph_id(c);
            if id.0 == 0 {
                continue;
            }
            if let Some(prev) = last {
                x += scaled.kern(prev, id);
            }
            if let Some(shape) = font.outline_glyph(id.with_scale_and_position(size, point(x, baseline))) {
                let b = shape.px_bounds();
                shape.draw(|gx, gy, c| {
                    let (px, py) = (b.min.x as i64 + gx as i64, b.min.y as i64 + gy as i64);
                    if (0..w as i64).contains(&px) && (0..h as i64).contains(&py) {
                        let at = py as usize * w + px as usize;
                        fill[at] = fill[at].max(c);
                    }
                });
            }
            x += scaled.h_advance(id);
            last = Some(id);
        }
    }
    // The dark edge: the letters grown outwards, softly at the rim.
    let r = edge.ceil() as i64;
    let reach: Vec<(i64, i64, f32)> = (-r..=r).flat_map(|dy| (-r..=r).map(move |dx| (dx, dy))).filter_map(|(dx, dy)| {
        let d = ((dx * dx + dy * dy) as f32).sqrt();
        (d <= edge + 0.5).then_some((dx, dy, (edge + 0.5 - d).clamp(0.0, 1.0)))
    }).collect();
    let mut img = image::RgbaImage::new(width, band);
    for y in 0..h {
        for x in 0..w {
            let mut around = 0f32;
            for &(dx, dy, weight) in &reach {
                let (sx, sy) = (x as i64 + dx, y as i64 + dy);
                if (0..w as i64).contains(&sx) && (0..h as i64).contains(&sy) {
                    around = around.max(fill[sy as usize * w + sx as usize] * weight);
                    if around >= 1.0 {
                        break;
                    }
                }
            }
            let f = fill[y * w + x];
            let alpha = f.max(around * 0.92);
            if alpha > 0.0 {
                let light = (255.0 * f / alpha).round() as u8;
                img.put_pixel(x as u32, y as u32, image::Rgba([light, light, light, (alpha * 255.0).round() as u8]));
            }
        }
    }
    img
}

/// The height of the band subtitles are drawn in, at the foot of a picture `height` high.
pub fn band(height: u32) -> u32 {
    ((height as f32 * 0.4) as u32 / 2 * 2).max(2)
}

/// Draw the lines of a clip into `dir` as pictures, and write the list ffmpeg reads them by,
/// each shown for as long as it is on screen. Gives the list, or nothing when there is nothing
/// to show.
pub fn render(cues: &[Cue], seconds: f64, width: u32, height: u32, dir: &Path, stop: &std::sync::atomic::AtomicBool) -> Result<Option<PathBuf>> {
    let shown = timeline(cues, seconds);
    if shown.iter().all(|(_, words)| words.is_empty()) {
        return Ok(None);
    }
    std::fs::create_dir_all(dir)?;
    // ffmpeg reads a name in the list as from the list's own folder, so each is given in full.
    let dir = &std::fs::canonicalize(dir)?;
    let font = FontRef::try_from_slice(FONT).map_err(|e| anyhow!("the subtitle font could not be read: {e}"))?;
    let band = band(height);
    let mut drawn: HashMap<String, PathBuf> = HashMap::new();
    let mut list = String::from("ffconcat version 1.0\n");
    let mut last = PathBuf::new();
    for (i, (at, words)) in shown.iter().enumerate() {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(anyhow!(crate::compact::CALLED_OFF));
        }
        let until = shown.get(i + 1).map_or(seconds, |(next, _)| *next);
        let picture = match drawn.get(words) {
            Some(p) => p.clone(),
            None => {
                let p = dir.join(format!("{}.png", drawn.len()));
                draw(&font, words, width, band, height).save(&p)?;
                drawn.insert(words.clone(), p.clone());
                p
            }
        };
        list.push_str(&format!("file '{}'\nduration {:.3}\n", picture.to_string_lossy().replace('\'', "'\\''"), (until - at).max(0.001)));
        last = picture;
    }
    // The last picture is named again, or ffmpeg does not keep it up for its time.
    list.push_str(&format!("file '{}'\n", last.to_string_lossy().replace('\'', "'\\''")));
    let path = dir.join("list.ffconcat");
    std::fs::write(&path, list)?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(index: u32, codec: &str, language: &str, forced: bool, title: &str) -> Stream {
        Stream { index, kind: "subtitle".into(), codec: codec.into(), language: language.into(), forced, title: title.into(), ..Default::default() }
    }

    #[test]
    fn subtitles_are_chosen_as_a_player_would_and_as_asked() {
        let info = MediaInfo { streams: vec![stream(3, "hdmv_pgs_subtitle", "eng", false, ""), stream(4, "subrip", "eng", false, "English SDH"), stream(5, "subrip", "eng", false, "English"), stream(6, "subrip", "eng", true, "Signs"), stream(7, "ass", "fre", false, "")], ..Default::default() };
        // Heard in another language: the full English text track, not the one for the hard of hearing.
        assert_eq!(choose(&info, &[], Some("ja"), "en", "auto").unwrap().source, Source::Text(5));
        // Heard in English: only the lines always shown.
        assert_eq!(choose(&info, &[], Some("en"), "en", "auto").unwrap().source, Source::Text(6));
        // Asked for, whatever is heard; or in French; or not at all.
        assert_eq!(choose(&info, &[], Some("en"), "en", "on").unwrap().source, Source::Text(5));
        assert_eq!(choose(&info, &[], Some("en"), "en", "french").unwrap(), Chosen { source: Source::Text(7), language: "fr".into() });
        assert_eq!(choose(&info, &[], Some("ja"), "en", "none"), None);
        // A file beside the video comes before pictures, and pictures when there is nothing else.
        let pictures = MediaInfo { streams: vec![stream(3, "hdmv_pgs_subtitle", "eng", false, "")], ..Default::default() };
        assert_eq!(choose(&pictures, &[], Some("ja"), "en", "auto").unwrap().source, Source::Picture(3));
        let beside = Sidecar { language: "en".into(), rel_path: "a.en.srt".into(), source: "found".into(), release: String::new(), hearing_impaired: false, forced: false, sync: None };
        assert_eq!(choose(&pictures, &[(beside, PathBuf::from("/a.en.srt"))], Some("ja"), "en", "auto").unwrap().source, Source::Beside(PathBuf::from("/a.en.srt")));
        assert_eq!(choose(&pictures, &[], Some("en"), "en", "auto"), None, "no forced lines, so nothing");
        // A track of signs and songs, as anime has, counts as the lines always shown even unmarked.
        let anime = MediaInfo { streams: vec![stream(3, "ass", "eng", false, "Signs & Songs"), stream(4, "ass", "eng", false, "Full Subtitles")], ..Default::default() };
        assert_eq!(choose(&anime, &[], Some("ja"), "en", "auto").unwrap().source, Source::Text(4));
        assert_eq!(choose(&anime, &[], Some("en"), "en", "auto").unwrap().source, Source::Text(3));
    }

    #[test]
    fn only_the_words_are_kept() {
        assert_eq!(clean("<font face=\"Open Sans\" size=\"45\"><b><i>Come pay your rent,\nimmediately.</i></b></font>"), "Come pay your rent,\nimmediately.");
        assert_eq!(clean("{\\an8}{\\pos(10,20)}Sign\\Nhere"), "Sign\nhere");
    }

    #[test]
    fn what_is_on_screen_changes_where_a_line_begins_or_ends() {
        let cue = |start: f64, end: f64, text: &str| Cue { start, end, text: text.into() };
        let shown = timeline(&[cue(1.0, 3.0, "One"), cue(2.0, 4.0, "Two")], 5.0);
        assert_eq!(shown, vec![(0.0, String::new()), (1.0, "One".into()), (2.0, "One\nTwo".into()), (3.0, "Two".into()), (4.0, String::new())]);
    }

    #[test]
    fn words_are_drawn_light_with_a_dark_edge_at_the_foot_of_the_band() {
        let font = FontRef::try_from_slice(FONT).unwrap();
        let img = draw(&font, "Hello there", 640, band(360), 360);
        assert_eq!((img.width(), img.height()), (640, 144));
        let seen: Vec<_> = img.pixels().filter(|p| p[3] > 200).collect();
        assert!(seen.iter().any(|p| p[0] > 230) && seen.iter().any(|p| p[0] < 40), "white letters and a dark edge");
        let top = img.enumerate_pixels().filter(|(_, _, p)| p[3] > 0).map(|(_, y, _)| y).min().unwrap();
        assert!(top > 80, "near the foot: {top}");
    }
}
