//! The messages Spool sends and understands, by their codes, and the shapes some of them share.

use crate::wire::{inflate, In, Out};
use serde::{Deserialize, Serialize};
use std::io;
use std::net::Ipv4Addr;

/// Messages to and from the server.
pub mod server {
    pub const LOGIN: u32 = 1;
    pub const SET_WAIT_PORT: u32 = 2;
    pub const GET_PEER_ADDRESS: u32 = 3;
    pub const CONNECT_TO_PEER: u32 = 18;
    pub const FILE_SEARCH: u32 = 26;
    pub const SET_STATUS: u32 = 28;
    pub const PING: u32 = 32;
    pub const SHARED_FOLDERS_FILES: u32 = 35;
    pub const RELOGGED: u32 = 41;
    pub const HAVE_NO_PARENT: u32 = 71;
    pub const EMBEDDED_MESSAGE: u32 = 93;
    pub const ACCEPT_CHILDREN: u32 = 100;
    pub const POSSIBLE_PARENTS: u32 = 102;
    pub const SEND_UPLOAD_SPEED: u32 = 121;
    pub const BRANCH_LEVEL: u32 = 126;
    pub const BRANCH_ROOT: u32 = 127;
    pub const RESET_DISTRIBUTED: u32 = 130;
    pub const EXCLUDED_SEARCH_PHRASES: u32 = 160;
    pub const CANT_CONNECT_TO_PEER: u32 = 1001;
}

/// Messages between peers.
pub mod peer {
    pub const SHARED_FILE_LIST_REQUEST: u32 = 4;
    pub const SHARED_FILE_LIST_RESPONSE: u32 = 5;
    pub const FILE_SEARCH_RESPONSE: u32 = 9;
    pub const USER_INFO_REQUEST: u32 = 15;
    pub const USER_INFO_RESPONSE: u32 = 16;
    pub const FOLDER_CONTENTS_REQUEST: u32 = 36;
    pub const FOLDER_CONTENTS_RESPONSE: u32 = 37;
    pub const TRANSFER_REQUEST: u32 = 40;
    pub const TRANSFER_RESPONSE: u32 = 41;
    pub const QUEUE_UPLOAD: u32 = 43;
    pub const PLACE_IN_QUEUE_RESPONSE: u32 = 44;
    pub const UPLOAD_FAILED: u32 = 46;
    pub const UPLOAD_DENIED: u32 = 50;
    pub const PLACE_IN_QUEUE_REQUEST: u32 = 51;
}

/// Messages that open a connection.
pub mod init {
    pub const PIERCE_FIREWALL: u8 = 0;
    pub const PEER_INIT: u8 = 1;
}

/// Messages of the search network.
pub mod distrib {
    pub const SEARCH: u8 = 3;
    pub const BRANCH_LEVEL: u8 = 4;
    pub const BRANCH_ROOT: u8 = 5;
    pub const EMBEDDED: u8 = 93;
}

/// Spool's place among the clients: 177 is kept for clients that are not the established
/// ones, each choosing its own minor version.
pub const MAJOR_VERSION: u32 = 177;
pub const MINOR_VERSION: u32 = 79;

/// The server's addresses come as a number whose bytes run the other way.
pub fn ip(n: u32) -> Ipv4Addr {
    Ipv4Addr::from(n)
}

/// A file as peers describe it to one another.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    /// The whole path in a search result; the name alone in a folder's listing.
    pub path: String,
    pub size: u64,
    pub ext: String,
    /// Kilobits a second, for lossy files.
    pub bitrate: Option<u32>,
    pub seconds: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub vbr: Option<bool>,
}

impl FileEntry {
    /// Its extension, lower case: from the entry, or from its name when that is left blank.
    pub fn extension(&self) -> String {
        let e = if self.ext.is_empty() { self.path.rsplit_once('.').map(|(_, e)| e).unwrap_or("") } else { &self.ext };
        e.trim_start_matches('.').to_ascii_lowercase()
    }

    pub fn write(&self, o: Out) -> Out {
        let mut attrs = vec![];
        if let Some(b) = self.bitrate {
            attrs.push((0, b));
        }
        if let Some(s) = self.seconds {
            attrs.push((1, s));
        }
        if let Some(v) = self.vbr {
            attrs.push((2, v as u32));
        }
        if let Some(r) = self.sample_rate {
            attrs.push((4, r));
        }
        if let Some(d) = self.bit_depth {
            attrs.push((5, d));
        }
        let mut o = o.u8(1).str(&self.path).u64(self.size).str(&self.ext).u32(attrs.len() as u32);
        for (code, v) in attrs {
            o = o.u32(code).u32(v);
        }
        o
    }

    /// One entry; None when its path is not UTF-8, and so could not be asked for.
    pub fn read(i: &mut In) -> io::Result<Option<FileEntry>> {
        let _code = i.u8()?;
        let path = i.utf8()?;
        let size = i.u64()?;
        let ext = i.str()?;
        let n = i.count(8)?;
        let mut f = FileEntry { path: path.clone().unwrap_or_default(), size, ext, ..Default::default() };
        for _ in 0..n {
            let (code, v) = (i.u32()?, i.u32()?);
            match code {
                0 => f.bitrate = Some(v),
                1 => f.seconds = Some(v),
                2 => f.vbr = Some(v != 0),
                4 => f.sample_rate = Some(v),
                5 => f.bit_depth = Some(v),
                _ => {}
            }
        }
        Ok(path.map(|_| f))
    }
}

/// What a peer found for a search of ours.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchReply {
    pub user: String,
    pub token: u32,
    pub files: Vec<FileEntry>,
    /// Whether they would start sending at once.
    pub slot_free: bool,
    /// Their upload speed, in bytes a second.
    pub speed: u32,
    /// How many uploads wait in their queue.
    pub queue: u32,
}

impl SearchReply {
    pub fn read(body: &[u8]) -> io::Result<SearchReply> {
        let b = inflate(body)?;
        let mut i = In::new(&b);
        let user = i.str()?;
        let token = i.u32()?;
        let n = i.count(17)?;
        let mut files = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            if let Some(f) = FileEntry::read(&mut i)? {
                files.push(f);
            }
        }
        let slot_free = i.bool()?;
        let speed = i.u32()?;
        let queue = i.u32()?;
        Ok(SearchReply { user, token, files, slot_free, speed, queue })
    }

    /// As a peer message, compressed.
    pub fn write(&self) -> Vec<u8> {
        let mut o = Out::new().str(&self.user).u32(self.token).u32(self.files.len() as u32);
        for f in &self.files {
            o = f.write(o);
        }
        o.bool(self.slot_free).u32(self.speed).u32(self.queue).u32(0).u32(0).zlib().msg(peer::FILE_SEARCH_RESPONSE)
    }
}

/// A folder's files, as a list of folders is sent: each folder's path, then its files by name.
pub fn write_folders(mut o: Out, folders: &[(String, Vec<FileEntry>)]) -> Out {
    o = o.u32(folders.len() as u32);
    for (path, files) in folders {
        o = o.str(path).u32(files.len() as u32);
        for f in files {
            o = f.write(o);
        }
    }
    o
}

/// A peer's answer to a transfer request of ours: allowed, or why not.
pub fn read_transfer_response(b: &[u8]) -> io::Result<(u32, bool, String)> {
    let mut i = In::new(b);
    let token = i.u32()?;
    let allowed = i.bool()?;
    let reason = if allowed || i.is_empty() { String::new() } else { i.str()? };
    Ok((token, allowed, reason))
}

/// A search that came to us: who asked, their token, and what for.
pub struct Searched {
    pub user: String,
    pub token: u32,
    pub query: String,
}

/// A search as the search network passes it on.
pub fn read_distrib_search(b: &[u8]) -> io::Result<Searched> {
    let mut i = In::new(b);
    let _id = i.u32()?;
    Ok(Searched { user: i.str()?, token: i.u32()?, query: i.str()? })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::code32;

    fn flac(path: &str) -> FileEntry {
        FileEntry { path: path.into(), size: 30 << 20, ext: "flac".into(), seconds: Some(240), sample_rate: Some(44100), bit_depth: Some(16), ..Default::default() }
    }

    #[test]
    fn a_search_reply_reads_back() {
        let r = SearchReply { user: "u".into(), token: 9, files: vec![flac("Music\\A - B\\01 x.flac"), flac("Music\\A - B\\02 y.flac")], slot_free: true, speed: 900_000, queue: 3 };
        let m = r.write();
        let (code, body) = code32(&m[4..]).unwrap();
        assert_eq!(code, peer::FILE_SEARCH_RESPONSE);
        let back = SearchReply::read(body).unwrap();
        assert_eq!((back.user.as_str(), back.token, back.files.len(), back.slot_free, back.speed, back.queue), ("u", 9, 2, true, 900_000, 3));
        assert_eq!(back.files[1], r.files[1]);
        assert_eq!(back.files[0].extension(), "flac");
    }

    #[test]
    fn an_extension_left_blank_is_taken_from_the_name() {
        let f = FileEntry { path: "a\\01.MP3".into(), ..Default::default() };
        assert_eq!(f.extension(), "mp3");
    }

    #[test]
    fn addresses_read_the_right_way_round() {
        assert_eq!(ip(u32::from_le_bytes([4, 3, 2, 1])).to_string(), "1.2.3.4");
    }
}
