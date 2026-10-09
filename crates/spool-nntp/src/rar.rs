//! Just enough of the RAR 4 and RAR 5 formats to find where each stored file's bytes sit in a
//! volume. Usenet posts are almost always packed without compression, so the payload can be
//! copied straight out as volumes arrive, with no extraction tool and no second copy at the end.
//!
//! Anything this reader is not sure of (compression, encryption, a header that fails its
//! checksum) is reported as unsupported, and the caller falls back to ordinary extraction.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const RAR4: &[u8] = b"Rar!\x1a\x07\x00";
const RAR5: &[u8] = b"Rar!\x1a\x07\x01\x00";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// Where this file's bytes for this volume start, and how many there are.
    pub data_offset: u64,
    pub data_len: u64,
    /// Checksum of the bytes in this volume when the file continues in the next one;
    /// otherwise of the whole file.
    pub crc: Option<u32>,
    pub split_before: bool,
    pub split_after: bool,
    pub stored: bool,
    pub encrypted: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Volume {
    pub entries: Vec<Entry>,
    pub len: u64,
}

fn read_at(f: &mut File, pos: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; len];
    f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
    f.read_exact(&mut buf).map_err(|_| "the volume ends in the middle of a header".to_string())?;
    Ok(buf)
}

/// A RAR 5 variable-length integer: seven bits per byte, low bits first.
fn vint(b: &[u8], at: &mut usize) -> Result<u64, String> {
    let mut v = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *b.get(*at).ok_or("a header field runs past the end of its header")?;
        *at += 1;
        v |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err("a header field is too long".into())
}

fn le32(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]])).ok_or_else(|| "a header is shorter than its fields".to_string())
}

fn le16(b: &[u8], at: usize) -> Result<u16, String> {
    b.get(at..at + 2).map(|x| u16::from_le_bytes([x[0], x[1]])).ok_or_else(|| "a header is shorter than its fields".to_string())
}

/// Read the headers of one complete volume.
pub(crate) fn scan(path: &Path) -> Result<Volume, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let head = read_at(&mut f, 0, 8.min(len as usize))?;
    if head.starts_with(RAR5) {
        scan5(&mut f, len)
    } else if head.starts_with(RAR4) {
        scan4(&mut f, len)
    } else {
        Err("not a RAR volume".into())
    }
}

/// The scan in plain tuples, for the diagnostic tool in `examples/`.
#[allow(clippy::type_complexity)]
pub fn scan_for_tools(path: &Path) -> Result<(Vec<(String, u64, u64, Option<u32>, bool, bool, bool, bool)>, u64), String> {
    let v = scan(path)?;
    Ok((v.entries.into_iter().map(|e| (e.name, e.data_offset, e.data_len, e.crc, e.split_before, e.split_after, e.stored, e.encrypted)).collect(), v.len))
}

fn scan5(f: &mut File, len: u64) -> Result<Volume, String> {
    let mut vol = Volume { entries: vec![], len };
    let mut pos = RAR5.len() as u64;
    while pos < len {
        // Checksum, then the header's own length; three bytes of length is the format's limit.
        let lead = read_at(f, pos, 7.min((len - pos) as usize))?;
        let stored_crc = le32(&lead, 0)?;
        let mut at = 4;
        let header_size = vint(&lead, &mut at)? as usize;
        if header_size == 0 || header_size > 2 << 20 {
            return Err("a header has an impossible size".into());
        }
        let size_len = at - 4;
        let header = read_at(f, pos + 4, size_len + header_size)?;
        if crc32fast::hash(&header) != stored_crc {
            return Err("a header failed its checksum".into());
        }
        let h = &header[size_len..];
        let mut at = 0;
        let kind = vint(h, &mut at)?;
        let flags = vint(h, &mut at)?;
        let extra_size = if flags & 0x01 != 0 { vint(h, &mut at)? as usize } else { 0 };
        let data_size = if flags & 0x02 != 0 { vint(h, &mut at)? } else { 0 };
        let data_offset = pos + 4 + (size_len + header_size) as u64;
        if data_offset + data_size > len {
            return Err("the volume is shorter than its headers say".into());
        }
        match kind {
            4 => return Err("the archive is encrypted".into()),
            5 => break,
            2 => {
                let file_flags = vint(h, &mut at)?;
                let _unpacked = vint(h, &mut at)?;
                let _attributes = vint(h, &mut at)?;
                if file_flags & 0x02 != 0 {
                    at += 4;
                }
                let crc = if file_flags & 0x04 != 0 {
                    let c = le32(h, at)?;
                    at += 4;
                    Some(c)
                } else {
                    None
                };
                let compression = vint(h, &mut at)?;
                let _host = vint(h, &mut at)?;
                let name_len = vint(h, &mut at)? as usize;
                let name = String::from_utf8_lossy(h.get(at..at + name_len).ok_or("a file name runs past its header")?).to_string();
                // Extra records sit at the end of the header: size, type, data. Type 1 is encryption.
                let mut encrypted = false;
                if extra_size > 0 && extra_size <= h.len() {
                    let extra = &h[h.len() - extra_size..];
                    let mut e = 0;
                    while e < extra.len() {
                        let size = vint(extra, &mut e)? as usize;
                        let start = e;
                        if vint(extra, &mut e)? == 1 {
                            encrypted = true;
                        }
                        e = start + size;
                    }
                }
                vol.entries.push(Entry {
                    name,
                    is_dir: file_flags & 0x01 != 0,
                    data_offset,
                    data_len: data_size,
                    crc,
                    split_before: flags & 0x08 != 0,
                    split_after: flags & 0x10 != 0,
                    stored: (compression >> 7) & 7 == 0,
                    encrypted,
                });
            }
            // Main header, service records (recovery data, quick-open index) and anything newer.
            _ => {}
        }
        pos = data_offset + data_size;
    }
    Ok(vol)
}

fn scan4(f: &mut File, len: u64) -> Result<Volume, String> {
    let mut vol = Volume { entries: vec![], len };
    let mut pos = RAR4.len() as u64;
    while pos + 7 <= len {
        let lead = read_at(f, pos, 7)?;
        let (stored_crc, kind, flags, size) = (le16(&lead, 0)?, lead[2], le16(&lead, 3)?, le16(&lead, 5)? as usize);
        if size < 7 {
            return Err("a header has an impossible size".into());
        }
        let h = read_at(f, pos, size)?;
        let mut data_size = 0u64;
        match kind {
            0x73 => {
                if flags & 0x0080 != 0 {
                    return Err("the archive is encrypted".into());
                }
            }
            0x74 | 0x7a => {
                // The checksum covers everything after its own two bytes.
                if (crc32fast::hash(&h[2..]) & 0xffff) as u16 != stored_crc {
                    return Err("a header failed its checksum".into());
                }
                data_size = le32(&h, 7)? as u64;
                let crc = le32(&h, 16)?;
                let method = *h.get(25).ok_or("a header is shorter than its fields")?;
                let name_len = le16(&h, 26)? as usize;
                let mut at = 32;
                if flags & 0x0100 != 0 {
                    data_size |= (le32(&h, at)? as u64) << 32;
                    at += 8;
                }
                let raw = h.get(at..at + name_len).ok_or("a file name runs past its header")?;
                // With the unicode flag the plain name comes first, then a zero, then an encoded form.
                let plain = raw.split(|b| *b == 0).next().unwrap_or(raw);
                if kind == 0x74 {
                    vol.entries.push(Entry {
                        name: String::from_utf8_lossy(plain).replace('\\', "/"),
                        is_dir: flags & 0x00e0 == 0x00e0,
                        data_offset: pos + size as u64,
                        data_len: data_size,
                        crc: Some(crc),
                        split_before: flags & 0x01 != 0,
                        split_after: flags & 0x02 != 0,
                        stored: method == 0x30,
                        encrypted: flags & 0x04 != 0,
                    });
                }
            }
            0x7b => break,
            _ => {
                if flags & 0x8000 != 0 {
                    data_size = le32(&h, 7)? as u64;
                }
            }
        }
        if pos + size as u64 + data_size > len {
            return Err("the volume is shorter than its headers say".into());
        }
        pos += size as u64 + data_size;
    }
    Ok(vol)
}

/// Writers for the two formats, for tests: store mode, split across volumes, the way a poster's
/// tool does it. Checked against 7-Zip in the engine tests.
pub mod testing {
    fn put_vint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    fn block5(out: &mut Vec<u8>, body: &[u8]) {
        let mut sized = vec![];
        put_vint(&mut sized, body.len() as u64);
        sized.extend_from_slice(body);
        out.extend_from_slice(&crc32fast::hash(&sized).to_le_bytes());
        out.extend_from_slice(&sized);
    }

    /// Split `data` into RAR 5 volumes holding at most `per_volume` bytes of it each.
    pub fn rar5_volumes(name: &str, data: &[u8], per_volume: usize) -> Vec<Vec<u8>> {
        let chunks: Vec<&[u8]> = data.chunks(per_volume.max(1)).collect();
        let whole = crc32fast::hash(data);
        chunks
            .iter()
            .enumerate()
            .map(|(i, chunk)| {
                let (first, last) = (i == 0, i + 1 == chunks.len());
                let mut out = super::RAR5.to_vec();
                // Main header: a volume, with its number from the second one on.
                let mut main = vec![];
                put_vint(&mut main, 1);
                put_vint(&mut main, 0);
                put_vint(&mut main, if first { 0x01 } else { 0x03 });
                if !first {
                    put_vint(&mut main, i as u64);
                }
                block5(&mut out, &main);
                let mut file = vec![];
                put_vint(&mut file, 2);
                put_vint(&mut file, 0x02 | if first { 0 } else { 0x08 } | if last { 0 } else { 0x10 });
                put_vint(&mut file, chunk.len() as u64);
                put_vint(&mut file, 0x04);
                put_vint(&mut file, data.len() as u64);
                put_vint(&mut file, 0x20);
                file.extend_from_slice(&(if last { whole } else { crc32fast::hash(chunk) }).to_le_bytes());
                put_vint(&mut file, 0);
                put_vint(&mut file, 0);
                put_vint(&mut file, name.len() as u64);
                file.extend_from_slice(name.as_bytes());
                block5(&mut out, &file);
                out.extend_from_slice(chunk);
                let mut end = vec![];
                put_vint(&mut end, 5);
                put_vint(&mut end, 0);
                put_vint(&mut end, if last { 0 } else { 0x01 });
                block5(&mut out, &end);
                out
            })
            .collect()
    }

    fn block4(out: &mut Vec<u8>, kind: u8, flags: u16, fields: &[u8]) {
        let mut h = vec![kind];
        h.extend_from_slice(&flags.to_le_bytes());
        h.extend_from_slice(&((fields.len() + 7) as u16).to_le_bytes());
        h.extend_from_slice(fields);
        out.extend_from_slice(&((crc32fast::hash(&h) & 0xffff) as u16).to_le_bytes());
        out.extend_from_slice(&h);
    }

    /// Split `data` into RAR 4 volumes (name.rar, name.r00, ...) the same way.
    pub fn rar4_volumes(name: &str, data: &[u8], per_volume: usize) -> Vec<Vec<u8>> {
        let chunks: Vec<&[u8]> = data.chunks(per_volume.max(1)).collect();
        let whole = crc32fast::hash(data);
        chunks
            .iter()
            .enumerate()
            .map(|(i, chunk)| {
                let (first, last) = (i == 0, i + 1 == chunks.len());
                let mut out = super::RAR4.to_vec();
                // Main header: volume, and "first volume" on the first.
                block4(&mut out, 0x73, 0x0001 | if first { 0x0100 } else { 0 }, &[0u8; 6]);
                let mut f = vec![];
                f.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
                f.extend_from_slice(&(data.len() as u32).to_le_bytes());
                f.push(3);
                f.extend_from_slice(&(if last { whole } else { crc32fast::hash(chunk) }).to_le_bytes());
                f.extend_from_slice(&0x5021_0000u32.to_le_bytes());
                f.push(20);
                f.push(0x30);
                f.extend_from_slice(&(name.len() as u16).to_le_bytes());
                f.extend_from_slice(&0x20u32.to_le_bytes());
                f.extend_from_slice(name.as_bytes());
                block4(&mut out, 0x74, 0x8000 | if first { 0 } else { 0x01 } | if last { 0 } else { 0x02 }, &f);
                out.extend_from_slice(chunk);
                block4(&mut out, 0x7b, 0x4000 | if last { 0 } else { 0x0001 }, &[]);
                out
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_stored_data_in_both_formats() {
        let dir = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..50_000u32).map(|i| (i * 31 % 251) as u8).collect();
        for (label, vols) in [("rar5", testing::rar5_volumes("film.mkv", &data, 20_000)), ("rar4", testing::rar4_volumes("film.mkv", &data, 20_000))] {
            assert_eq!(vols.len(), 3);
            let mut joined = vec![];
            for (i, v) in vols.iter().enumerate() {
                let p = dir.path().join(format!("{label}.{i}"));
                std::fs::write(&p, v).unwrap();
                let vol = scan(&p).unwrap_or_else(|e| panic!("{label} volume {i}: {e}"));
                assert_eq!(vol.entries.len(), 1);
                let e = &vol.entries[0];
                assert_eq!((e.name.as_str(), e.stored, e.encrypted, e.is_dir), ("film.mkv", true, false, false));
                assert_eq!((e.split_before, e.split_after), (i > 0, i < 2), "{label} volume {i}");
                let part = &v[e.data_offset as usize..(e.data_offset + e.data_len) as usize];
                if e.split_after {
                    assert_eq!(e.crc, Some(crc32fast::hash(part)));
                }
                joined.extend_from_slice(part);
            }
            assert_eq!(joined, data);
        }
        std::fs::write(dir.path().join("junk"), b"not an archive at all").unwrap();
        assert!(scan(&dir.path().join("junk")).is_err());
        // A flipped header byte is caught, not misread.
        let mut bad = testing::rar5_volumes("film.mkv", &data, 20_000).remove(0);
        bad[20] ^= 0xff;
        std::fs::write(dir.path().join("bad"), &bad).unwrap();
        assert!(scan(&dir.path().join("bad")).is_err());
    }
}
