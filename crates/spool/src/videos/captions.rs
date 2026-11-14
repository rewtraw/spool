//! A video's captions, made into subtitles players and the search of what is said can read.
//!
//! A site can have captions people wrote and captions it made itself from the speech, in
//! several forms of each language. One is kept for each language wanted: written before made,
//! the plain language before a region's, and of those the site made, the original before a
//! translation. The site's own captions roll: each line is shown again with the next one added
//! below it, a word at a time. Kept as they come, every line would be read twice and found
//! twice; they are made into one cue for each line, shown for as long as it was on screen.

use super::ytdlp::Info;
use crate::subs::{parse_srt, write_srt, Cue};
use std::path::Path;

/// One subtitle text for each language wanted that the video has captions in: (language, SubRip text).
pub fn choose(dir: &Path, info: &Info, languages: &[String]) -> Vec<(String, String)> {
    let files: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_string();
            let tag = name.strip_prefix("video.")?.strip_suffix(".vtt").or_else(|| name.strip_prefix("video.")?.strip_suffix(".srt"))?.to_string();
            Some((tag, p))
        })
        .collect();
    let mut out = vec![];
    for lang in languages {
        let written = |tag: &str| info.subtitles.contains_key(tag);
        let of_lang = |tag: &str| tag == lang || tag.starts_with(&format!("{lang}-"));
        let rank = |tag: &str| -> Option<u32> {
            if !of_lang(tag) {
                return None;
            }
            Some(match (written(tag), tag == lang, tag.ends_with("-orig")) {
                (true, true, _) => 0,
                (true, false, _) => 1,
                (false, _, true) => 2,
                (false, true, _) => 3,
                (false, false, _) => 4,
            })
        };
        let Some((tag, path)) = files.iter().filter_map(|(t, p)| rank(t).map(|r| (r, t, p))).min_by_key(|(r, t, _)| (*r, t.len())).map(|(_, t, p)| (t, p)) else { continue };
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let cues = plain(&parse_srt(&text));
        let cues = if written(tag) { cues } else { unroll(&cues) };
        if !cues.is_empty() {
            out.push((lang.clone(), write_srt(&cues)));
        }
    }
    out
}

/// Cues without the markup captions carry: word timings, colours, positions.
fn plain(cues: &[Cue]) -> Vec<Cue> {
    cues.iter()
        .map(|c| {
            let mut text = String::new();
            let mut in_tag = false;
            for ch in c.text.chars() {
                match ch {
                    '<' => in_tag = true,
                    '>' if in_tag => in_tag = false,
                    _ if !in_tag => text.push(ch),
                    _ => {}
                }
            }
            let text = text.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ");
            let lines: Vec<String> = text.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect();
            Cue { start: c.start, end: c.end, text: lines.join("\n") }
        })
        .filter(|c| !c.text.is_empty())
        .collect()
}

/// Rolling captions as one cue for each line, from when it first appears until it is last
/// shown.
fn unroll(cues: &[Cue]) -> Vec<Cue> {
    let mut out: Vec<Cue> = vec![];
    // The lines the last cue showed, each with where it is in `out`.
    let mut showing: Vec<(String, usize)> = vec![];
    for c in cues {
        // A cue shown for a moment only carries a line over to the next; it says nothing new.
        if c.end - c.start < 0.05 {
            continue;
        }
        let mut now: Vec<(String, usize)> = vec![];
        for line in c.text.lines() {
            match showing.iter().find(|(l, _)| l == line) {
                Some((_, i)) => {
                    out[*i].end = out[*i].end.max(c.end);
                    now.push((line.to_string(), *i));
                }
                None => {
                    out.push(Cue { start: c.start, end: c.end, text: line.to_string() });
                    now.push((line.to_string(), out.len() - 1));
                }
            }
        }
        showing = now;
    }
    // Each line until the next begins, so two are never shown at once.
    for i in 1..out.len() {
        if out[i - 1].end > out[i].start {
            out[i - 1].end = out[i].start.max(out[i - 1].start + 0.3);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROLLING: &str = "WEBVTT\nKind: captions\nLanguage: en\n\n00:00:00.320 --> 00:00:02.990 align:start position:0%\n \nso<00:00:00.640><c> today</c><00:00:01.000><c> we're</c>\n\n00:00:02.990 --> 00:00:03.000 align:start position:0%\nso today we're\n \n\n00:00:03.000 --> 00:00:05.270 align:start position:0%\nso today we're\ntalking<00:00:03.500><c> about</c><00:00:04.000><c> UFOs</c>\n\n00:00:05.270 --> 00:00:05.280 align:start position:0%\ntalking about UFOs\n \n\n00:00:05.280 --> 00:00:07.000 align:start position:0%\ntalking about UFOs\nand &amp; more\n";

    #[test]
    fn rolling_captions_become_one_cue_a_line() {
        let cues = unroll(&plain(&parse_srt(ROLLING)));
        let lines: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(lines, ["so today we're", "talking about UFOs", "and & more"]);
        assert!((cues[0].start - 0.32).abs() < 1e-6);
        assert!(cues[0].end <= cues[1].start + 1e-9);
        assert!((cues[2].end - 7.0).abs() < 1e-6);
    }

    #[test]
    fn written_captions_come_before_the_sites_own() {
        let dir = std::env::temp_dir().join(format!("spool-captions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("video.en.vtt"), ROLLING).unwrap();
        std::fs::write(dir.join("video.en-GB.vtt"), "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nWritten by a person\n").unwrap();
        std::fs::write(dir.join("video.de.vtt"), "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHallo\n").unwrap();
        let info: Info = serde_json::from_value(serde_json::json!({"subtitles": {"en-GB": []}})).unwrap();
        let got = choose(&dir, &info, &["en".to_string()]);
        assert_eq!(got.len(), 1);
        assert!(got[0].1.contains("Written by a person"));
        // Without written ones, the site's own, unrolled.
        let none: Info = serde_json::from_value(serde_json::json!({})).unwrap();
        std::fs::remove_file(dir.join("video.en-GB.vtt")).unwrap();
        let got = choose(&dir, &none, &["en".to_string()]);
        assert_eq!(got[0].1.matches("so today we're").count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
