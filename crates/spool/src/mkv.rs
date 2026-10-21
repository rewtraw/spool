//! Text subtitles read straight out of a Matroska file, without reading the rest of it.
//!
//! A track of subtitles is a few kilobytes spread through a file of many gigabytes, and ffmpeg
//! reads all of it to find them: minutes for a film on a share across the network. But most
//! files say in their index (Cues) where each subtitle is, as mkvmerge writes them, so the
//! few thousand small pieces can be read where they lie. A file without such an index, or with
//! anything here unsure, is left to ffmpeg.

use crate::subs::Cue;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

const SEGMENT: u64 = 0x18538067;
const SEEK_HEAD: u64 = 0x114D9B74;
const SEEK: u64 = 0x4DBB;
const SEEK_ID: u64 = 0x53AB;
const SEEK_POSITION: u64 = 0x53AC;
const INFO: u64 = 0x1549A966;
const TIMESTAMP_SCALE: u64 = 0x2AD7B1;
const MUXING_APP: u64 = 0x4D80;
const TRACKS: u64 = 0x1654AE6B;
const TRACK_ENTRY: u64 = 0xAE;
const TRACK_NUMBER: u64 = 0xD7;
const TRACK_TYPE: u64 = 0x83;
const CODEC_ID: u64 = 0x86;
const CONTENT_ENCODINGS: u64 = 0x6D80;
const CONTENT_ENCODING: u64 = 0x6240;
const CONTENT_ENCODING_SCOPE: u64 = 0x5032;
const CONTENT_COMPRESSION: u64 = 0x5034;
const CONTENT_COMP_ALGO: u64 = 0x4254;
const CONTENT_COMP_SETTINGS: u64 = 0x4255;
const CONTENT_ENCRYPTION: u64 = 0x5035;
const CUES: u64 = 0x1C53BB6B;
const CUE_POINT: u64 = 0xBB;
const CUE_TIME: u64 = 0xB3;
const CUE_TRACK_POSITIONS: u64 = 0xB7;
const CUE_TRACK: u64 = 0xF7;
const CUE_CLUSTER_POSITION: u64 = 0xF1;
const CUE_RELATIVE_POSITION: u64 = 0xF0;
const CUE_DURATION: u64 = 0xB2;
const CLUSTER: u64 = 0x1F43B675;
const BLOCK_GROUP: u64 = 0xA0;
const BLOCK: u64 = 0xA1;
const BLOCK_DURATION: u64 = 0x9B;
const SIMPLE_BLOCK: u64 = 0xA3;

/// How a track's pieces are stored: as they are, squeezed with zlib, or with the bytes every
/// piece begins with left out.
#[derive(Clone, Debug, PartialEq)]
enum Stored {
    Plain,
    Zlib,
    Stripped(Vec<u8>),
}

impl Stored {
    fn open(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        match self {
            Stored::Plain => Some(bytes.to_vec()),
            Stored::Zlib => {
                let mut out = vec![];
                flate2::read::ZlibDecoder::new(bytes).take((1 << 20) + 1).read_to_end(&mut out).ok()?;
                // More than any line of subtitles holds is not a line of subtitles.
                (out.len() <= 1 << 20).then_some(out)
            }
            Stored::Stripped(head) => Some([head.as_slice(), bytes].concat()),
        }
    }
}

/// How a track says its pieces are stored; None for anything else, such as encryption.
fn stored(r: &mut Reader, end: u64) -> Option<Stored> {
    let mut how = Stored::Plain;
    let mut ok = true;
    r.each(end, |r, id, size, body| {
        if id == CONTENT_ENCODING {
            let mut scope = 1;
            let mut this = None;
            r.each(body.checked_add(size)?, |r, id, size, body| {
                match id {
                    CONTENT_ENCODING_SCOPE => scope = r.uint(size)?,
                    CONTENT_ENCRYPTION => ok = false,
                    CONTENT_COMPRESSION => {
                        let (mut algo, mut settings) = (0, vec![]);
                        r.each(body.checked_add(size)?, |r, id, size, _| {
                            match id {
                                CONTENT_COMP_ALGO => algo = r.uint(size)?,
                                CONTENT_COMP_SETTINGS => settings = r.bytes(size)?,
                                _ => {}
                            }
                            Some(true)
                        })?;
                        this = match algo {
                            0 => Some(Stored::Zlib),
                            3 => Some(Stored::Stripped(settings)),
                            _ => None,
                        };
                        ok &= this.is_some();
                    }
                    _ => {}
                }
                Some(true)
            })?;
            // Only one way, and of each piece: anything more is left to ffmpeg.
            if scope & 1 == 0 || how != Stored::Plain {
                ok = false;
            }
            if let Some(t) = this {
                how = t;
            }
        }
        Some(true)
    })?;
    ok.then_some(how)
}

/// Whether a track's codec in Matroska's words is the codec ffmpeg names.
fn same_codec(id: &str, ffmpeg: &str) -> bool {
    matches!((id, ffmpeg), ("S_TEXT/UTF8", "subrip" | "srt" | "text") | ("S_TEXT/WEBVTT", "webvtt") | ("S_TEXT/ASS" | "S_TEXT/SSA", "ass" | "ssa") | ("S_HDMV/PGS", "hdmv_pgs_subtitle"))
}

/// What stands for the words of a subtitle that is a picture.
pub const PICTURE: &str = "[picture]";

struct Reader {
    file: BufReader<File>,
    at: u64,
}

impl Reader {
    fn seek(&mut self, to: u64) -> Option<()> {
        if to != self.at {
            match i64::try_from(to as i128 - self.at as i128) {
                Ok(by) => self.file.seek_relative(by).ok()?,
                Err(_) => {
                    self.file.seek(std::io::SeekFrom::Start(to)).ok()?;
                }
            }
            self.at = to;
        }
        Some(())
    }

    fn bytes(&mut self, n: u64) -> Option<Vec<u8>> {
        // Nothing here is large: a field, a line of subtitles.
        if n > 1 << 20 {
            return None;
        }
        let mut out = vec![0; n as usize];
        self.file.read_exact(&mut out).ok()?;
        self.at += n;
        Some(out)
    }

    fn byte(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }

    /// An element's id, as it is written, with its marker bits.
    fn id(&mut self) -> Option<u64> {
        let first = self.byte()?;
        let n = first.leading_zeros() as u64 + 1;
        if n > 4 {
            return None;
        }
        let mut v = first as u64;
        for b in self.bytes(n - 1)? {
            v = (v << 8) | b as u64;
        }
        Some(v)
    }

    /// An element's size; None for a size not given, which is never wanted here.
    fn size(&mut self) -> Option<u64> {
        let first = self.byte()?;
        let n = first.leading_zeros() as u64 + 1;
        if n > 8 {
            return None;
        }
        let mut v = (first as u64) & (0xFF >> n);
        for b in self.bytes(n - 1)? {
            v = (v << 8) | b as u64;
        }
        (v != (1 << (7 * n)) - 1).then_some(v)
    }

    fn uint(&mut self, size: u64) -> Option<u64> {
        if size > 8 {
            return None;
        }
        Some(self.bytes(size)?.iter().fold(0, |v, b| (v << 8) | *b as u64))
    }

    /// The elements from here up to `end`: each one's id, size and where its body starts. The
    /// reader is left at the start of each body, and moved past it before the next.
    fn each(&mut self, end: u64, mut f: impl FnMut(&mut Reader, u64, u64, u64) -> Option<bool>) -> Option<()> {
        let mut at = self.at;
        while at < end {
            self.seek(at)?;
            let id = self.id()?;
            let size = self.size()?;
            let body = self.at;
            if !f(self, id, size, body)? {
                return Some(());
            }
            at = body.checked_add(size)?;
        }
        Some(())
    }
}

/// Where a subtitle is: its cluster, its place in it, and when and for how long the index
/// says it is shown.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Place {
    cluster: u64,
    relative: u64,
    time: u64,
    duration: Option<u64>,
}

/// Every line of the subtitles that are the `stream`th track of a Matroska file, read by the
/// file's index; None when that cannot be done and the whole file must be read instead.
/// `streams` are the file's streams as ffmpeg numbers them, each with its kind and codec, to
/// be sure each is the track in the same place: one ffmpeg passed over would put them out.
///
/// Subtitles that are pictures, as on a disc, come back as when each is shown and taken away,
/// with no words: enough to tell how other subtitles should be timed.
pub fn track(path: &Path, stream: u32, streams: &[(u32, String, String)]) -> Option<Vec<Cue>> {
    let codec = streams.iter().find(|s| s.0 == stream).map(|s| s.2.as_str())?;
    let mut r = Reader { file: BufReader::with_capacity(16 << 10, File::open(path).ok()?), at: 0 };
    // The EBML header, then the segment that holds everything.
    if r.id()? != 0x1A45DFA3 {
        return None;
    }
    let header = r.size()?;
    r.seek(r.at.checked_add(header)?)?;
    if r.id()? != SEGMENT {
        return None;
    }
    r.size().or(Some(u64::MAX))?;
    let segment = r.at;
    let mut found: BTreeMap<u64, u64> = BTreeMap::new();
    r.each(u64::MAX, |r, id, size, body| {
        if id == SEEK_HEAD {
            r.each(body.checked_add(size)?, |r, id, size, body| {
                if id == SEEK {
                    let (mut which, mut position) = (None, None);
                    r.each(body.checked_add(size)?, |r, id, size, _| {
                        match id {
                            SEEK_ID => which = Some(r.bytes(size)?.iter().fold(0u64, |v, b| (v << 8) | *b as u64)),
                            SEEK_POSITION => position = Some(r.uint(size)?),
                            _ => {}
                        }
                        Some(true)
                    })?;
                    if let (Some(w), Some(p)) = (which, position) {
                        found.entry(w).or_insert(p);
                    }
                }
                Some(true)
            })?;
            return Some(false);
        }
        // The index of positions comes before the first cluster, when it is there at all.
        Some(id != CLUSTER)
    })?;

    // An index can leave the rest to a second one, further on.
    if !found.contains_key(&CUES) {
        if let Some(more) = found.get(&SEEK_HEAD).copied() {
            r.seek(segment.checked_add(more)?)?;
            let (id, size) = (r.id()?, r.size()?);
            if id == SEEK_HEAD {
                r.each(r.at.checked_add(size)?, |r, id, size, body| {
                    if id == SEEK {
                        let (mut which, mut position) = (None, None);
                        r.each(body.checked_add(size)?, |r, id, size, _| {
                            match id {
                                SEEK_ID => which = Some(r.bytes(size)?.iter().fold(0u64, |v, b| (v << 8) | *b as u64)),
                                SEEK_POSITION => position = Some(r.uint(size)?),
                                _ => {}
                            }
                            Some(true)
                        })?;
                        if let (Some(w), Some(p)) = (which, position) {
                            found.entry(w).or_insert(p);
                        }
                    }
                    Some(true)
                })?;
            }
        }
    }

    // The track: by its place among the tracks, as ffmpeg numbers streams.
    r.seek(segment.checked_add(*found.get(&TRACKS)?)?)?;
    let (id, size) = (r.id()?, r.size()?);
    if id != TRACKS {
        return None;
    }
    let (mut n, mut track, mut agree) = (0u32, None, true);
    r.each(r.at.checked_add(size)?, |r, id, size, body| {
        if id == TRACK_ENTRY {
            {
                let (mut number, mut kind, mut codec, mut how) = (None, None, String::new(), Some(Stored::Plain));
                r.each(body.checked_add(size)?, |r, id, size, body| {
                    match id {
                        TRACK_NUMBER => number = Some(r.uint(size)?),
                        TRACK_TYPE => kind = Some(r.uint(size)?),
                        CODEC_ID => codec = String::from_utf8_lossy(&r.bytes(size)?).trim_end_matches('\0').to_string(),
                        CONTENT_ENCODINGS => how = stored(r, body.checked_add(size)?),
                        _ => {}
                    }
                    Some(true)
                })?;
                // Each stream ffmpeg tells of is of the same kind, and a subtitle of the same
                // codec, as the track in its place.
                if let Some((_, k, c)) = streams.iter().find(|s| s.0 == n) {
                    let same = match k.as_str() {
                        "video" => kind == Some(1),
                        "audio" => kind == Some(2),
                        "subtitle" => kind == Some(0x11) && same_codec(&codec, c),
                        _ => true,
                    };
                    agree &= same;
                }
                // Subtitles, stored in a way that can be read here.
                if n == stream && kind == Some(0x11) {
                    track = number.zip(how).map(|(t, how)| (t, codec, how));
                }
            }
            n += 1;
        }
        Some(true)
    })?;
    if !agree || streams.iter().any(|s| s.0 >= n && matches!(s.1.as_str(), "video" | "audio" | "subtitle")) {
        return None;
    }
    let (track, id, how) = track?;
    if !same_codec(&id, codec) {
        return None;
    }
    let text: fn(&[u8]) -> String = match id.as_str() {
        "S_TEXT/UTF8" | "S_TEXT/WEBVTT" => |b| String::from_utf8_lossy(b).into_owned(),
        // Read order, layer, style, name, three margins and effect come before the words.
        "S_TEXT/ASS" | "S_TEXT/SSA" => |b| String::from_utf8_lossy(b).splitn(9, ',').nth(8).unwrap_or("").to_string(),
        // A picture is shown when its composition has something in it, and taken away by one
        // that has nothing.
        "S_HDMV/PGS" => |b| if b.first() == Some(&0x16) && b.get(13).is_some_and(|n| *n > 0) { PICTURE.into() } else { String::new() },
        _ => return None,
    };
    let pictures = id == "S_HDMV/PGS";

    // The scale of every time in the file: when it cannot be found, nothing can be timed.
    let mut scale = 1_000_000u64;
    {
        r.seek(segment.checked_add(*found.get(&INFO)?)?)?;
        let (id, size) = (r.id()?, r.size()?);
        if id != INFO {
            return None;
        }
        let mut made_by = String::new();
        r.each(r.at.checked_add(size)?, |r, id, size, _| {
            match id {
                TIMESTAMP_SCALE => scale = r.uint(size)?,
                MUXING_APP => made_by = String::from_utf8_lossy(&r.bytes(size)?).into_owned(),
                _ => {}
            }
            Some(true)
        })?;
        // Only an index made by mkvmerge or ffmpeg, which list every line of subtitles, is
        // taken to be whole: from anything else a line missing from it would be lost.
        if !(made_by.contains("libmatroska") || made_by.starts_with("Lavf")) || scale == 0 {
            return None;
        }
    }

    // Where the index says each of the track's lines is.
    r.seek(segment.checked_add(*found.get(&CUES)?)?)?;
    let (id, size) = (r.id()?, r.size()?);
    if id != CUES {
        return None;
    }
    let mut places: Vec<Place> = vec![];
    let mut unplaced = false;
    r.each(r.at.checked_add(size)?, |r, id, size, body| {
        if id == CUE_POINT {
            let mut time = None;
            r.each(body.checked_add(size)?, |r, id, size, body| {
                if id == CUE_TIME {
                    time = Some(r.uint(size)?);
                }
                if id == CUE_TRACK_POSITIONS {
                    let (mut t, mut cluster, mut relative, mut duration) = (None, None, None, None);
                    r.each(body.checked_add(size)?, |r, id, size, _| {
                        match id {
                            CUE_TRACK => t = Some(r.uint(size)?),
                            CUE_CLUSTER_POSITION => cluster = Some(r.uint(size)?),
                            CUE_RELATIVE_POSITION => relative = Some(r.uint(size)?),
                            CUE_DURATION => duration = Some(r.uint(size)?),
                            _ => {}
                        }
                        Some(true)
                    })?;
                    if t == Some(track) {
                        match (cluster, relative, time) {
                            (Some(cluster), Some(relative), Some(time)) => places.push(Place { cluster, relative, time, duration }),
                            _ => unplaced = true,
                        }
                    }
                }
                Some(true)
            })?;
        }
        Some(true)
    })?;
    // Lines the index does not place exactly cannot be found without reading everything.
    if places.is_empty() || unplaced {
        return None;
    }
    places.sort();
    places.dedup_by_key(|p| (p.cluster, p.relative));

    // Read from several places at once: across a network each read waits its turn otherwise.
    const AT_ONCE: usize = 16;
    let share = places.len().div_ceil(AT_ONCE);
    let read: Vec<Option<Vec<Cue>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = places.chunks(share).map(|part| {
            let how = &how;
            scope.spawn(move || lines_at(path, segment, part, track, scale, text, how))
        }).collect();
        handles.into_iter().map(|h| h.join().ok().flatten()).collect()
    });
    let mut cues: Vec<Cue> = vec![];
    for part in read {
        cues.extend(part?);
    }
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    if pictures {
        // Each picture is up until whatever comes next.
        for i in 0..cues.len() {
            cues[i].end = cues.get(i + 1).map(|c| c.start).unwrap_or(cues[i].start + 5.0);
        }
        cues.retain(|c| c.text == PICTURE);
        return Some(cues);
    }
    // A line must say how long it is up: one that does not is left to ffmpeg.
    if cues.iter().any(|c| c.end.is_nan()) {
        return None;
    }
    Some(cues)
}

/// The lines at some of the places the index gives.
fn lines_at(path: &Path, segment: u64, places: &[Place], track: u64, scale: u64, text: fn(&[u8]) -> String, how: &Stored) -> Option<Vec<Cue>> {
    let file = File::open(path).ok()?;
    // Each read is a few kilobytes far from the last: reading on ahead would only waste time.
    #[cfg(target_os = "macos")]
    unsafe {
        use std::os::fd::AsRawFd;
        libc::fcntl(file.as_raw_fd(), libc::F_RDAHEAD, 0);
    }
    let mut r = Reader { file: BufReader::with_capacity(4 << 10, file), at: 0 };
    let mut cues: Vec<Cue> = vec![];
    // How long a cluster's id and size are as this file writes them, learned from the first
    // and checked by what is found after: so each line is one read, where it lies.
    let mut head: Option<u64> = None;
    for p in places.iter().copied() {
        let mut again = false;
        let (block, duration) = loop {
            let len = match head {
                Some(len) => len,
                None => {
                    r.seek(segment.checked_add(p.cluster)?)?;
                    if r.id()? != CLUSTER {
                        return None;
                    }
                    r.size()?;
                    let len = r.at - (segment + p.cluster);
                    head = Some(len);
                    len
                }
            };
            r.seek(segment.checked_add(p.cluster)?.checked_add(len)?.checked_add(p.relative)?)?;
            match block_at(&mut r, track) {
                Some(found) => break found,
                None if !again => {
                    head = None;
                    again = true;
                }
                None => return None,
            }
        };
        let start = p.time as f64 * scale as f64 / 1e9;
        let end = duration.or(p.duration).map(|d| start + d as f64 * scale as f64 / 1e9);
        cues.push(Cue { start, end: end.unwrap_or(f64::NAN), text: text(&how.open(&block)?) });
    }
    Some(cues)
}

/// The words of a line of the track, and how long it lasts when the file says, read from
/// where the reader is; None when that is not such a line.
fn block_at(r: &mut Reader, track: u64) -> Option<(Vec<u8>, Option<u64>)> {
    let (id, size) = (r.id()?, r.size()?);
    let (mut block, mut duration) = (None, None);
    match id {
        SIMPLE_BLOCK => block = Some(r.bytes(size)?),
        BLOCK_GROUP => r.each(r.at.checked_add(size)?, |r, id, size, _| {
            match id {
                BLOCK => block = Some(r.bytes(size)?),
                BLOCK_DURATION => duration = Some(r.uint(size)?),
                _ => {}
            }
            Some(true)
        })?,
        _ => return None,
    }
    let block = block?;
    // The track number, the time from the cluster's, and flags: lines are never laced.
    let n = block.first()?.leading_zeros() as usize + 1;
    let number = block.get(..n)?.iter().enumerate().fold(0u64, |v, (i, b)| (v << 8) | if i == 0 { (*b & (0xFF >> n)) as u64 } else { *b as u64 });
    if n > 8 || number != track || block.len() < n + 3 || block[n + 2] & 0x06 != 0 {
        return None;
    }
    Some((block[n + 3..].to_vec(), duration))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn pieces_are_opened_as_the_track_says_they_are_stored() {
        let mut squeezed = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::default());
        squeezed.write_all(b"Wake up.").unwrap();
        assert_eq!(Stored::Zlib.open(&squeezed.finish().unwrap()).unwrap(), b"Wake up.");
        assert_eq!(Stored::Stripped(b"Wake".to_vec()).open(b" up.").unwrap(), b"Wake up.");
        assert_eq!(Stored::Plain.open(b"Wake up.").unwrap(), b"Wake up.");
    }

    #[test]
    fn sizes_and_ids_are_read_as_matroska_writes_them() {
        let path = std::env::temp_dir().join(format!("spool-mkv-{}", std::process::id()));
        std::fs::write(&path, [0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x40, 0x02, 0x01, 0xFF]).unwrap();
        let mut r = Reader { file: BufReader::new(File::open(&path).unwrap()), at: 0 };
        assert_eq!((r.id(), r.size()), (Some(0x1A45DFA3), Some(4)));
        // A size of two bytes, and one that says it is not given.
        assert_eq!(r.size(), Some(2));
        assert_eq!(r.byte(), Some(0x01));
        assert_eq!(r.size(), None);
        std::fs::remove_file(path).unwrap();
    }
}
