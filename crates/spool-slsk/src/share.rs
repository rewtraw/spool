//! What is shared: each file under the name others see it by, the words searches are matched
//! against, and the lists of files peers ask for.

use crate::msg::{peer, write_folders, FileEntry};
use crate::wire::Out;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// A file shared, and where it really is.
#[derive(Clone, Debug, Default)]
pub struct Shared {
    /// What others see and ask for: `Music\Artist - Album\01 Track.flac`.
    pub name: String,
    pub real: PathBuf,
    pub size: u64,
    pub seconds: u32,
    /// Kilobits a second; for lossless files, nothing.
    pub bitrate: u32,
    pub sample_rate: u32,
    pub bit_depth: u32,
    pub lossless: bool,
}

impl Shared {
    fn entry(&self, path: String) -> FileEntry {
        let ext = self.name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        let some = |v: u32| (v > 0).then_some(v);
        if self.lossless {
            FileEntry { path, size: self.size, ext, seconds: some(self.seconds), sample_rate: some(self.sample_rate), bit_depth: some(self.bit_depth), ..Default::default() }
        } else {
            FileEntry { path, size: self.size, ext, bitrate: some(self.bitrate), seconds: some(self.seconds), vbr: Some(false), ..Default::default() }
        }
    }

    fn folder(&self) -> &str {
        self.name.rsplit_once('\\').map(|(f, _)| f).unwrap_or("")
    }

    fn base(&self) -> &str {
        self.name.rsplit_once('\\').map(|(_, b)| b).unwrap_or(&self.name)
    }
}

/// Words as searches are matched: lower case, without accents, split at anything not a
/// letter or a digit.
pub fn words(s: &str) -> Vec<String> {
    deunicode::deunicode(s).to_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

#[derive(Default)]
pub struct Shares {
    files: Vec<Shared>,
    by_name: HashMap<String, usize>,
    /// For each word, the files with it in their names, in order.
    words: HashMap<String, Vec<u32>>,
    folders: usize,
}

impl Shares {
    pub fn new(files: Vec<Shared>) -> Shares {
        let mut s = Shares::default();
        let mut folders = std::collections::HashSet::new();
        for (n, f) in files.into_iter().enumerate() {
            let mut w = words(&f.name);
            w.sort();
            w.dedup();
            for w in w {
                s.words.entry(w).or_default().push(n as u32);
            }
            folders.insert(f.folder().to_string());
            s.by_name.insert(f.name.clone(), n);
            s.files.push(f);
        }
        s.folders = folders.len();
        s
    }

    pub fn files(&self) -> usize {
        self.files.len()
    }

    pub fn folders(&self) -> usize {
        self.folders
    }

    pub fn get(&self, name: &str) -> Option<&Shared> {
        self.by_name.get(name).map(|&n| &self.files[n])
    }

    /// The files a search finds: every word it asks for in the name, none it rules out with
    /// a "-" before it, nor any phrase the server keeps off the network. At most `most`.
    pub fn search(&self, query: &str, excluded: &[String], most: usize) -> Vec<FileEntry> {
        let (mut wanted, mut not) = (vec![], vec![]);
        for term in query.split_whitespace() {
            // A wildcard matches part of a word: too loose to answer from an index of words.
            if term.contains('*') {
                continue;
            }
            match term.strip_prefix('-') {
                Some(t) => not.extend(words(t)),
                None => wanted.extend(words(term)),
            }
        }
        if wanted.is_empty() {
            return vec![];
        }
        let mut lists: Vec<&Vec<u32>> = vec![];
        for w in &wanted {
            match self.words.get(w) {
                Some(l) => lists.push(l),
                None => return vec![],
            }
        }
        lists.sort_by_key(|l| l.len());
        let (first, rest) = lists.split_first().expect("a word");
        let mut out = vec![];
        for &n in first.iter() {
            if !rest.iter().all(|l| l.binary_search(&n).is_ok()) {
                continue;
            }
            let f = &self.files[n as usize];
            if !not.is_empty() && not.iter().any(|w| self.words.get(w).is_some_and(|l| l.binary_search(&n).is_ok())) {
                continue;
            }
            if !excluded.is_empty() {
                let lower = f.name.to_lowercase();
                if excluded.iter().any(|p| lower.contains(p.as_str())) {
                    continue;
                }
            }
            out.push(f.entry(f.name.clone()));
            if out.len() >= most {
                break;
            }
        }
        out
    }

    fn grouped<'a>(&self, files: impl Iterator<Item = &'a Shared>) -> Vec<(String, Vec<FileEntry>)> {
        let mut by: BTreeMap<String, Vec<FileEntry>> = BTreeMap::new();
        for f in files {
            by.entry(f.folder().to_string()).or_default().push(f.entry(f.base().to_string()));
        }
        by.into_iter().collect()
    }

    /// Everything shared, as a peer that browses it is sent it.
    pub fn list(&self) -> Vec<u8> {
        let folders = self.grouped(self.files.iter());
        write_folders(Out::new(), &folders).u32(0).u32(0).zlib().msg(peer::SHARED_FILE_LIST_RESPONSE)
    }

    /// One folder and those inside it, as asked for by its path.
    pub fn folder(&self, token: u32, path: &str) -> Vec<u8> {
        let inside = format!("{path}\\");
        let folders = self.grouped(self.files.iter().filter(|f| f.folder() == path || f.folder().starts_with(&inside)));
        write_folders(Out::new().u32(token).str(path), &folders).zlib().msg(peer::FOLDER_CONTENTS_RESPONSE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{code32, inflate, In};

    fn file(name: &str) -> Shared {
        Shared { name: name.into(), real: PathBuf::from("/x"), size: 1000, seconds: 200, lossless: true, sample_rate: 44100, bit_depth: 16, ..Default::default() }
    }

    fn shares() -> Shares {
        Shares::new(vec![
            file("Music\\Can - Tago Mago (1971) [FLAC]\\01 Paperhouse.flac"),
            file("Music\\Can - Tago Mago (1971) [FLAC]\\02 Mushroom.flac"),
            file("Music\\Can - Future Days [FLAC]\\01 Future Days.flac"),
            file("Music\\Björk - Homogenic\\01 Hunter.flac"),
        ])
    }

    #[test]
    fn a_search_needs_every_word() {
        let s = shares();
        assert_eq!(s.search("can tago mago", &[], 100).len(), 2);
        assert_eq!(s.search("CAN future", &[], 100).len(), 1);
        assert_eq!(s.search("can kraftwerk", &[], 100).len(), 0);
        assert_eq!(s.search("bjork hunter", &[], 100).len(), 1, "accents are not needed");
        assert_eq!(s.search("can -mushroom tago", &[], 100).len(), 1, "a word ruled out");
        assert_eq!(s.search("can", &[], 1).len(), 1, "held to the most asked");
        assert_eq!(s.search("*ago", &[], 100).len(), 0);
        assert_eq!(s.search("can tago", &["paperhouse".into()], 100).len(), 1, "a phrase the server keeps off");
        let hit = &s.search("hunter", &[], 100)[0];
        assert_eq!(hit.path, "Music\\Björk - Homogenic\\01 Hunter.flac");
        assert_eq!((hit.ext.as_str(), hit.bit_depth, hit.bitrate), ("flac", Some(16), None));
    }

    #[test]
    fn folders_are_counted_and_listed() {
        let s = shares();
        assert_eq!((s.files(), s.folders()), (4, 3));
        assert!(s.get("Music\\Can - Future Days [FLAC]\\01 Future Days.flac").is_some());
        assert!(s.get("Music\\..\\etc\\passwd").is_none());
        let m = s.folder(5, "Music\\Can - Tago Mago (1971) [FLAC]");
        let (code, body) = code32(&m[4..]).unwrap();
        assert_eq!(code, peer::FOLDER_CONTENTS_RESPONSE);
        let b = inflate(body).unwrap();
        let mut i = In::new(&b);
        assert_eq!((i.u32().unwrap(), i.str().unwrap().as_str(), i.u32().unwrap()), (5, "Music\\Can - Tago Mago (1971) [FLAC]", 1));
        assert_eq!((i.str().unwrap().as_str(), i.u32().unwrap()), ("Music\\Can - Tago Mago (1971) [FLAC]", 2));
    }
}
