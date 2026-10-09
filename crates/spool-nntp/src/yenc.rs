//! yEnc decoding of a raw NNTP article body.

use memchr::memchr;

#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// File name from the `=ybegin` header.
    pub name: String,
    /// Size of the whole file.
    pub file_size: u64,
    /// Zero-based offset of this part within the file.
    pub offset: u64,
    pub data: Vec<u8>,
    /// False when the article states a CRC and the data does not match it.
    pub crc_ok: bool,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum YencError {
    #[error("article has no yEnc header")]
    NoHeader,
    #[error("yEnc header is malformed")]
    BadHeader,
}

fn field<'a>(line: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let needle = format!(" {key}=");
    let pos = memchr::memmem::find(line, needle.as_bytes())?;
    let rest = &line[pos + needle.len()..];
    if key == "name" {
        return Some(rest);
    }
    let end = rest.iter().position(|b| *b == b' ').unwrap_or(rest.len());
    Some(&rest[..end])
}

fn num(line: &[u8], key: &str) -> Option<u64> {
    std::str::from_utf8(field(line, key)?).ok()?.trim().parse().ok()
}

fn hex(line: &[u8], key: &str) -> Option<u32> {
    let s = std::str::from_utf8(field(line, key)?).ok()?.trim();
    u32::from_str_radix(s, 16).ok()
}

fn next_line(body: &[u8], pos: usize) -> (&[u8], usize) {
    match memchr(b'\n', &body[pos..]) {
        Some(i) => {
            let end = pos + i;
            let line_end = if end > pos && body[end - 1] == b'\r' { end - 1 } else { end };
            (&body[pos..line_end], end + 1)
        }
        None => (&body[pos..], body.len()),
    }
}

/// Decode one article body. `body` is the multi-line block as received, still dot-stuffed, without
/// the terminating ".\r\n" line.
pub fn decode(body: &[u8]) -> Result<Decoded, YencError> {
    // Find =ybegin; some posts carry junk lines before it.
    let start = memchr::memmem::find(body, b"=ybegin ").ok_or(YencError::NoHeader)?;
    let (begin, mut pos) = next_line(body, start);
    let file_size = num(begin, "size").ok_or(YencError::BadHeader)?;
    let name = String::from_utf8_lossy(field(begin, "name").ok_or(YencError::BadHeader)?).trim().to_string();
    let multipart = field(begin, "part").is_some();

    let mut offset = 0u64;
    let mut expected_len: Option<u64> = None;
    if multipart {
        let (part, after) = next_line(body, pos);
        if part.starts_with(b"=ypart ") {
            let b = num(part, "begin").ok_or(YencError::BadHeader)?;
            let e = num(part, "end").ok_or(YencError::BadHeader)?;
            offset = b.saturating_sub(1);
            expected_len = Some(e.saturating_sub(offset));
            pos = after;
        }
    }

    let mut data = Vec::with_capacity(body.len().saturating_sub(pos));
    let mut trailer: Option<&[u8]> = None;
    while pos < body.len() {
        let (line, after) = next_line(body, pos);
        pos = after;
        if line.starts_with(b"=yend") {
            trailer = Some(line);
            break;
        }
        // NNTP dot-stuffing: a leading ".." stands for one ".".
        let line = if line.starts_with(b"..") { &line[1..] } else { line };
        let mut i = 0;
        while i < line.len() {
            match memchr(b'=', &line[i..]) {
                None => {
                    data.extend(line[i..].iter().map(|b| b.wrapping_sub(42)));
                    break;
                }
                Some(e) => {
                    data.extend(line[i..i + e].iter().map(|b| b.wrapping_sub(42)));
                    i += e + 1;
                    if i < line.len() {
                        data.push(line[i].wrapping_sub(64).wrapping_sub(42));
                        i += 1;
                    }
                }
            }
        }
    }

    let mut crc_ok = true;
    if let Some(t) = trailer {
        let stated = if multipart { hex(t, "pcrc32").or_else(|| hex(t, "crc32")) } else { hex(t, "crc32") };
        if let Some(crc) = stated {
            crc_ok = crc32fast::hash(&data) == crc;
        }
        if let Some(size) = num(t, "size") {
            if size != data.len() as u64 {
                crc_ok = false;
            }
        }
    } else {
        crc_ok = false;
    }
    if let Some(len) = expected_len {
        if len != data.len() as u64 {
            crc_ok = false;
        }
    }
    Ok(Decoded { name, file_size, offset, data, crc_ok })
}

/// Encode one part of a file as a yEnc article body (dot-stuffed, no terminator). Test support.
#[cfg(any(test, feature = "testing"))]
pub fn encode(name: &str, file_size: u64, part: u32, total_parts: u32, offset: u64, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 40 + 200);
    out.extend(format!("=ybegin part={part} total={total_parts} line=128 size={file_size} name={name}\r\n").as_bytes());
    out.extend(format!("=ypart begin={} end={}\r\n", offset + 1, offset + data.len() as u64).as_bytes());
    let mut col = 0;
    for &b in data {
        let e = b.wrapping_add(42);
        let critical = matches!(e, 0 | b'\n' | b'\r' | b'=') || (col == 0 && matches!(e, b'\t' | b' ' | b'.'));
        if critical {
            out.push(b'=');
            out.push(e.wrapping_add(64));
            col += 2;
        } else {
            out.push(e);
            col += 1;
        }
        if col >= 128 {
            out.extend(b"\r\n");
            col = 0;
        }
    }
    if col > 0 {
        out.extend(b"\r\n");
    }
    out.extend(format!("=yend size={} part={part} pcrc32={:08x}\r\n", data.len(), crc32fast::hash(data)).as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_byte_value() {
        let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        let enc = encode("file.bin", 9000, 2, 3, 4000, &data);
        let d = decode(&enc).unwrap();
        assert_eq!(d.name, "file.bin");
        assert_eq!(d.file_size, 9000);
        assert_eq!(d.offset, 4000);
        assert!(d.crc_ok);
        assert_eq!(d.data, data);
    }

    #[test]
    fn handles_dot_stuffing_and_names_with_spaces() {
        // 0x04 encodes to '.', so a line of them starts with a dot and arrives stuffed.
        let body = b"=ybegin line=128 size=3 name=my file name.bin\r\n...\r\n=yend size=3 crc32=00000000\r\n";
        let d = decode(body).unwrap();
        assert_eq!(d.name, "my file name.bin");
        assert_eq!(d.data, vec![4, 4]);
        assert!(!d.crc_ok, "size mismatch must be reported");
    }

    #[test]
    fn detects_corruption() {
        let data = vec![7u8; 1000];
        let mut enc = encode("f", 1000, 1, 1, 0, &data);
        let i = enc.len() / 2;
        enc[i] = enc[i].wrapping_add(1);
        assert!(!decode(&enc).unwrap().crc_ok);
        assert_eq!(decode(b"no header here\r\n"), Err(YencError::NoHeader));
    }
}
