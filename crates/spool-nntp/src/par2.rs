//! Just enough PAR2 parsing to know which files a set protects and their checksums. That lets the
//! engine skip a full verify when every file is intact and recover real names for obfuscated posts.
//! Repair itself is done by the `par2` helper.

use md5::{Digest, Md5};
use std::io::Read;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct FileDesc {
    pub name: String,
    pub length: u64,
    pub md5: [u8; 16],
    /// MD5 of the first 16 KiB.
    pub md5_16k: [u8; 16],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Par2Info {
    /// Identifies the recovery set; every file of one set carries the same id.
    pub set_id: [u8; 16],
    pub slice_size: u64,
    pub files: Vec<FileDesc>,
    /// Recovery slices present in the parsed data.
    pub recovery_blocks: u32,
}

const MAGIC: &[u8; 8] = b"PAR2\0PKT";

pub fn parse(data: &[u8]) -> Par2Info {
    let mut info = Par2Info::default();
    let mut pos = 0;
    while let Some(i) = memchr::memmem::find(&data[pos..], MAGIC) {
        let start = pos + i;
        if start + 64 > data.len() {
            break;
        }
        let len = u64::from_le_bytes(data[start + 8..start + 16].try_into().unwrap()) as usize;
        if len < 64 || start + len > data.len() {
            pos = start + 8;
            continue;
        }
        info.set_id = data[start + 32..start + 48].try_into().unwrap();
        let kind = &data[start + 48..start + 64];
        let body = &data[start + 64..start + len];
        if kind == b"PAR 2.0\0FileDesc" && body.len() >= 56 {
            let name_bytes = &body[56..];
            let end = name_bytes.iter().position(|b| *b == 0).unwrap_or(name_bytes.len());
            let fd = FileDesc {
                md5: body[16..32].try_into().unwrap(),
                md5_16k: body[32..48].try_into().unwrap(),
                length: u64::from_le_bytes(body[48..56].try_into().unwrap()),
                name: String::from_utf8_lossy(&name_bytes[..end]).to_string(),
            };
            if !info.files.iter().any(|f| f.name == fd.name) {
                info.files.push(fd);
            }
        } else if kind == b"PAR 2.0\0Main\0\0\0\0" && body.len() >= 8 {
            info.slice_size = u64::from_le_bytes(body[0..8].try_into().unwrap());
        } else if kind == b"PAR 2.0\0RecvSlic" {
            info.recovery_blocks += 1;
        }
        pos = start + len;
    }
    info
}

pub fn parse_file(path: &Path) -> std::io::Result<Par2Info> {
    Ok(parse(&std::fs::read(path)?))
}

/// MD5 of a whole file and of its first 16 KiB.
pub fn hash_file(path: &Path) -> std::io::Result<([u8; 16], [u8; 16], u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut full = Md5::new();
    let mut head = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if total < 16384 {
            let take = ((16384 - total) as usize).min(n);
            head.update(&buf[..take]);
        }
        full.update(&buf[..n]);
        total += n as u64;
    }
    Ok((full.finalize().into(), head.finalize().into(), total))
}

/// MD5 of the first 16 KiB only. Cheap enough to run on every file when matching names.
pub fn hash_16k(path: &Path) -> std::io::Result<[u8; 16]> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 16384];
    let mut filled = 0;
    while filled < buf.len() {
        let n = f.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(Md5::digest(&buf[..filled]).into())
}

/// Blocks a recovery volume holds, read from its name ("x.vol012+34.par2" holds 34).
pub fn blocks_from_name(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let v = lower.rfind(".vol")?;
    let rest = &lower[v + 4..];
    let plus = rest.find('+')?;
    let end = rest[plus + 1..].find(|c: char| !c.is_ascii_digit()).map(|i| plus + 1 + i).unwrap_or(rest.len());
    rest[plus + 1..end].parse().ok()
}

/// True when the file starts with a PAR2 packet, whatever it is called.
pub fn has_magic(path: &Path) -> bool {
    let mut head = [0u8; 8];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == MAGIC
}

/// The recovery-set id from a PAR2 file's first packet, without reading the whole file.
pub fn set_id_of(path: &Path) -> Option<[u8; 16]> {
    let mut head = [0u8; 48];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).ok()?;
    (&head[..8] == MAGIC).then(|| head[32..48].try_into().unwrap())
}

pub fn is_par2(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".par2")
}

/// A recovery volume, as opposed to the small index file.
pub fn is_recovery_volume(name: &str) -> bool {
    is_par2(name) && blocks_from_name(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_names() {
        assert_eq!(blocks_from_name("Movie.vol012+34.par2"), Some(34));
        assert_eq!(blocks_from_name("Movie.vol000+01.PAR2"), Some(1));
        assert_eq!(blocks_from_name("Movie.par2"), None);
        assert!(is_recovery_volume("a.vol1+2.par2"));
        assert!(!is_recovery_volume("a.par2"));
    }

    #[test]
    fn parses_real_par2_output() {
        let Ok(found) = std::process::Command::new("par2").arg("--version").output() else { return };
        if !found.status.success() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(dir.path().join("payload.bin"), &data).unwrap();
        let ok = std::process::Command::new("par2")
            .args(["create", "-q", "-q", "-r10", "-n1", "set.par2", "payload.bin"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(ok.success());
        let info = parse_file(&dir.path().join("set.par2")).unwrap();
        assert_eq!(info.files.len(), 1);
        assert_eq!(info.files[0].name, "payload.bin");
        assert_eq!(info.files[0].length, 200_000);
        let (md5, md5_16k, len) = hash_file(&dir.path().join("payload.bin")).unwrap();
        assert_eq!(len, 200_000);
        assert_eq!(md5, info.files[0].md5);
        assert_eq!(md5_16k, info.files[0].md5_16k);
        assert_eq!(hash_16k(&dir.path().join("payload.bin")).unwrap(), md5_16k);
        assert!(info.slice_size > 0);
    }
}
