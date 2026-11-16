//! Soulseek, for Spool: albums fetched from other people's shared folders, and the music
//! folder shared back.
//!
//! The protocol is not published by its owners. This follows the description kept by the
//! Nicotine+ project, and does what current clients do: it signs in as a client of its own
//! (major version 177, kept for clients that are not the established ones), asks for files
//! with QueueUpload, answers searches from the search network as a leaf, and sends files
//! from a small number of upload slots. Rooms, chat and browsing others are left out.

mod client;
mod jobs;
pub mod msg;
pub mod share;
pub mod wire;

pub use client::{Client, Config, Status, Upload};
pub use jobs::{forget, Fetch, FileState, Job, JobFile};
pub use msg::{FileEntry, SearchReply};
pub use share::{Shared, Shares};

/// One person's folder among search results: the files found in it, with where each goes
/// in a copy of it. Disc folders ("CD1", "Disc 2") are counted as their album's.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Folder {
    pub user: String,
    /// Its path as they share it.
    pub path: String,
    pub files: Vec<(FileEntry, String)>,
    pub slot_free: bool,
    pub speed: u32,
    pub queue: u32,
}

impl Folder {
    pub fn size(&self) -> u64 {
        self.files.iter().map(|(f, _)| f.size).sum()
    }

    /// The last parts of its path, which say what it is: "Can\Tago Mago (1971)".
    pub fn name(&self, parts: usize) -> String {
        let p: Vec<&str> = self.path.split('\\').filter(|s| !s.is_empty()).collect();
        p[p.len().saturating_sub(parts)..].join("\\")
    }
}

/// Whether a folder's name is that of one disc of an album.
fn a_disc(name: &str) -> bool {
    let w = share::words(name);
    let disc = |s: &str| matches!(s, "cd" | "disc" | "disk");
    let number = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    match w.as_slice() {
        [one] => ["cd", "disc", "disk"].iter().any(|d| one.strip_prefix(d).is_some_and(number)),
        [a, b] => disc(a) && number(b),
        _ => false,
    }
}

/// Search results by folder.
pub fn folders(replies: &[SearchReply]) -> Vec<Folder> {
    let mut out: Vec<Folder> = vec![];
    for r in replies {
        for f in &r.files {
            let Some((dir, _)) = f.path.rsplit_once('\\') else { continue };
            let album = match dir.rsplit_once('\\') {
                Some((up, last)) if a_disc(last) => up,
                _ => dir,
            };
            let rel = f.path[album.len() + 1..].replace('\\', "/");
            match out.iter_mut().find(|x| x.user == r.user && x.path == album) {
                Some(x) => {
                    if !x.files.iter().any(|(e, _)| e.path == f.path) {
                        x.files.push((f.clone(), rel));
                    }
                }
                None => out.push(Folder { user: r.user.clone(), path: album.to_string(), files: vec![(f.clone(), rel)], slot_free: r.slot_free, speed: r.speed, queue: r.queue }),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(user: &str, paths: &[&str]) -> SearchReply {
        SearchReply { user: user.into(), files: paths.iter().map(|p| FileEntry { path: p.to_string(), size: 10, ..Default::default() }).collect(), ..Default::default() }
    }

    #[test]
    fn results_are_gathered_by_folder_with_their_discs() {
        let f = folders(&[
            reply("a", &["Music\\Can\\Tago Mago\\CD1\\01.flac", "Music\\Can\\Tago Mago\\CD2\\01.flac", "Music\\Can\\Tago Mago\\cover.jpg"]),
            reply("b", &["Can - Tago Mago\\01.flac", "Can - Tago Mago\\02.flac"]),
            reply("a", &["Music\\Can\\Tago Mago\\CD1\\01.flac"]),
        ]);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "Music\\Can\\Tago Mago");
        let rel: Vec<&str> = f[0].files.iter().map(|(_, r)| r.as_str()).collect();
        assert_eq!(rel, ["CD1/01.flac", "CD2/01.flac", "cover.jpg"], "the same file twice is counted once");
        assert_eq!((f[1].user.as_str(), f[1].files.len(), f[1].size()), ("b", 2, 20));
        assert_eq!(f[0].name(2), "Can\\Tago Mago");
    }

    #[test]
    fn disc_folders_are_known() {
        assert!(a_disc("CD1") && a_disc("Disc 2") && a_disc("disk03"));
        assert!(!a_disc("CD") && !a_disc("Discovery") && !a_disc("Disc 2 - Live"));
    }
}
