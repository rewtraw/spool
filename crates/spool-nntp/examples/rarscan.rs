//! Read RAR volumes given in order and check every part against its checksum, without writing.
use std::io::{Read, Seek, SeekFrom};
fn main() {
    let mut whole = crc32fast::Hasher::new();
    for path in std::env::args().skip(1) {
        let (entries, len) = match spool_nntp::rar::scan_for_tools(std::path::Path::new(&path)) {
            Ok(v) => v,
            Err(e) => {
                println!("{path}: unsupported: {e}");
                continue;
            }
        };
        println!("{} ({len} bytes)", path.rsplit('/').next().unwrap());
        for (name, off, dlen, crc, before, after, stored, encrypted) in entries {
            let mut f = std::fs::File::open(&path).unwrap();
            f.seek(SeekFrom::Start(off)).unwrap();
            let mut part = crc32fast::Hasher::new();
            let mut left = dlen;
            let mut buf = vec![0u8; 1 << 20];
            while left > 0 {
                let n = f.read(&mut buf[..left.min(1 << 20) as usize]).unwrap();
                if n == 0 { break; }
                part.update(&buf[..n]);
                whole.update(&buf[..n]);
                left -= n as u64;
            }
            let part = part.finalize();
            let verdict = if after { if crc == Some(part) { "part ok".into() } else { format!("PART MISMATCH got {part:08x}") } } else {
                let w = std::mem::replace(&mut whole, crc32fast::Hasher::new()).finalize();
                if crc == Some(w) || (!before && crc == Some(part)) { "whole ok".into() } else { format!("WHOLE MISMATCH got {w:08x}") }
            };
            println!("  {name} off={off} len={dlen} crc={crc:08x?} before={before} after={after} stored={stored} enc={encrypted} -> {verdict}");
        }
    }
}
