//! Downloads: a folder's files from one person, each asked for, waited for in their queue,
//! and received. Kept in a file, so that a restart carries on where it was and a file part
//! received is not received again.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// A file to fetch: its name as its owner shares it, its size, and where it goes in the
/// download's folder ("CD1/01 Track.flac").
#[derive(Clone, Debug)]
pub struct Fetch {
    pub name: String,
    pub size: u64,
    pub rel: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "state", content = "why")]
pub enum FileState {
    /// Asked for, and waiting in its owner's queue.
    Waiting,
    /// Its owner is sending it.
    Coming,
    Done,
    /// Its owner will not send it, and why.
    Failed(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobFile {
    pub name: String,
    pub rel: String,
    pub size: u64,
    pub got: u64,
    pub state: FileState,
    /// Its place in its owner's queue, when they have said.
    pub place: Option<u32>,
    /// How often it began coming and broke off.
    #[serde(default)]
    pub breaks: u32,
    #[serde(skip)]
    pub(crate) asked_at: i64,
    #[serde(skip)]
    pub(crate) place_asked_at: i64,
    #[serde(skip)]
    pub(crate) token: Option<u32>,
    #[serde(skip)]
    pub(crate) coming_at: i64,
    /// Whether its owner connected to send it, this time it was offered.
    #[serde(skip)]
    pub(crate) connected: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub user: String,
    /// The folder it is received into.
    pub dest: PathBuf,
    pub files: Vec<JobFile>,
    pub added: i64,
    /// When anything last came: a file offered, or part of one received.
    pub progress_at: i64,
    /// What is in its way, in words; empty when nothing.
    pub note: String,
}

impl Job {
    pub fn done(&self) -> bool {
        self.files.iter().all(|f| f.state == FileState::Done)
    }

    /// Why it cannot finish: the first file its owner refused.
    pub fn failed(&self) -> Option<String> {
        self.files.iter().find_map(|f| match &f.state {
            FileState::Failed(why) => Some(format!("{}: {why}", base(&f.name))),
            _ => None,
        })
    }

    /// Whether anything of it has come yet.
    pub fn started(&self) -> bool {
        self.files.iter().any(|f| f.got > 0 || f.state == FileState::Done)
    }

    /// The nearest it is to the front of its owner's queue, when they have said.
    pub fn place(&self) -> Option<u32> {
        self.files.iter().filter(|f| f.state == FileState::Waiting).filter_map(|f| f.place).min()
    }

    pub fn got(&self) -> u64 {
        self.files.iter().map(|f| f.got.min(f.size)).sum()
    }

    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Where a file of it is put, and where it is received until it is whole.
    pub fn paths(&self, f: &JobFile) -> (PathBuf, PathBuf) {
        let to = self.dest.join(&f.rel);
        let name = to.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
        let part = to.with_file_name(format!(".{name}.part"));
        (to, part)
    }
}

/// The last part of a shared file's name.
pub(crate) fn base(name: &str) -> &str {
    name.rsplit(['\\', '/']).next().unwrap_or(name)
}

/// A file's place in the download's folder, made safe: no way out of it, nothing hidden.
pub(crate) fn safe_rel(rel: &str) -> String {
    let parts: Vec<String> = rel
        .split(['/', '\\'])
        .map(|p| p.chars().filter(|c| !c.is_control()).collect::<String>())
        .map(|p| p.trim().trim_start_matches('.').to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        "file".into()
    } else {
        parts.join("/")
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct Jobs {
    pub next: u64,
    pub list: Vec<Job>,
    #[serde(skip)]
    pub dirty: bool,
}

impl Jobs {
    /// The downloads as they were left. A file that was coming is asked for again.
    pub fn load(path: &Path) -> Jobs {
        let mut j: Jobs = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        for job in &mut j.list {
            for f in &mut job.files {
                if f.state == FileState::Coming {
                    f.state = FileState::Waiting;
                }
            }
            job.progress_at = now();
        }
        j
    }

    pub fn save(&mut self, path: &Path) {
        let Ok(b) = serde_json::to_vec(self) else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, b).and_then(|_| std::fs::rename(&tmp, path)).is_ok() {
            self.dirty = false;
        }
    }

    pub fn add(&mut self, user: &str, dest: &Path, files: Vec<super::Fetch>) -> u64 {
        self.next += 1;
        let mut taken: Vec<String> = vec![];
        let files = files
            .into_iter()
            .map(|f| {
                // Two names that come out the same once made safe are kept apart.
                let rel = unique(safe_rel(&f.rel), &taken);
                taken.push(rel.to_lowercase());
                JobFile { name: f.name, rel, size: f.size, got: 0, state: FileState::Waiting, place: None, breaks: 0, asked_at: 0, place_asked_at: 0, token: None, coming_at: 0, connected: false }
            })
            .collect();
        self.list.push(Job { id: self.next, user: user.to_string(), dest: dest.to_path_buf(), files, added: now(), progress_at: now(), note: String::new() });
        self.dirty = true;
        self.next
    }

    /// The file of this person's with this name still to come.
    pub fn file_mut(&mut self, user: &str, name: &str) -> Option<(&mut Job, usize)> {
        for job in self.list.iter_mut().filter(|j| j.user == user) {
            if let Some(n) = job.files.iter().position(|f| f.name == name && !matches!(f.state, FileState::Done | FileState::Failed(_))) {
                return Some((job, n));
            }
        }
        None
    }

    /// They are ready to send a file: it is taken if it is still wanted, and no bigger than
    /// they said it was when it was found.
    pub fn offered(&mut self, user: &str, name: &str, token: u32, size: u64) -> bool {
        let Some((job, n)) = self.file_mut(user, name) else { return false };
        let f = &mut job.files[n];
        if f.size > 0 && size > f.size.saturating_add(f.size / 100) {
            f.state = FileState::Failed(format!("offered as {size} bytes, after being listed as {}", f.size));
            self.dirty = true;
            return false;
        }
        f.connected = false;
        f.state = FileState::Coming;
        f.token = Some(token);
        f.size = size;
        f.place = None;
        f.coming_at = now();
        job.progress_at = now();
        job.note.clear();
        self.dirty = true;
        true
    }

    /// The file of a download being sent on the connection opened with this token: None when
    /// the download was removed, or the file asked for again since.
    pub fn sending(&mut self, id: u64, n: usize, token: u32) -> Option<&mut JobFile> {
        let job = self.list.iter_mut().find(|j| j.id == id)?;
        job.progress_at = now();
        job.files.get_mut(n).filter(|f| f.token == Some(token) && f.state == FileState::Coming)
    }

    /// They will not send a file.
    pub fn denied(&mut self, user: &str, name: &str, why: &str) {
        if let Some((job, n)) = self.file_mut(user, name) {
            job.files[n].state = FileState::Failed(if why.is_empty() { "refused".into() } else { why.trim_end_matches('.').to_string() });
            self.dirty = true;
        }
    }

    /// A file stopped coming: it is asked for again soon, unless it keeps breaking off.
    pub fn broke(&mut self, user: &str, name: &str) {
        if let Some((job, n)) = self.file_mut(user, name) {
            let f = &mut job.files[n];
            f.breaks += 1;
            f.token = None;
            f.state = if f.breaks > 8 { FileState::Failed("it kept breaking off".into()) } else { FileState::Waiting };
            // Asked for again in a minute.
            f.asked_at = now() - super::client::ASK_AGAIN + 60;
            self.dirty = true;
        }
    }
}

/// A place in the folder no other file has taken, by a number before its extension.
fn unique(rel: String, taken: &[String]) -> String {
    if !taken.contains(&rel.to_lowercase()) {
        return rel;
    }
    let (stem, ext) = match rel.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && !s.ends_with('/') => (s.to_string(), format!(".{e}")),
        _ => (rel.clone(), String::new()),
    };
    (2..).map(|n| format!("{stem} ({n}){ext}")).find(|r| !taken.contains(&r.to_lowercase())).expect("a free name")
}

/// Forget a download kept in the file, when no client is running to do it. Gives what it was.
pub fn forget(path: &Path, id: u64) -> Option<Job> {
    let mut j = Jobs::load(path);
    let at = j.list.iter().position(|x| x.id == id)?;
    let job = j.list.remove(at);
    j.save(path);
    Some(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch(name: &str) -> crate::Fetch {
        crate::Fetch { name: name.into(), size: 100, rel: base(name).into() }
    }

    #[test]
    fn a_file_goes_from_asked_to_coming_and_back_when_it_breaks() {
        let mut j = Jobs::default();
        let id = j.add("u", Path::new("/d"), vec![fetch("M\\A\\01.flac"), fetch("M\\A\\02.flac")]);
        assert!(j.offered("u", "M\\A\\01.flac", 7, 99));
        assert!(!j.offered("someone", "M\\A\\01.flac", 7, 99), "only from whom it was asked");
        let job = &j.list[0];
        assert_eq!((job.id, &job.files[0].state, job.files[0].size, job.size()), (id, &FileState::Coming, 99, 199));
        j.broke("u", "M\\A\\01.flac");
        assert_eq!(j.list[0].files[0].state, FileState::Waiting);
        j.denied("u", "M\\A\\02.flac", "File not shared.");
        assert_eq!(j.list[0].failed().as_deref(), Some("02.flac: File not shared"));
    }

    #[test]
    fn a_file_offered_bigger_than_listed_is_refused() {
        let mut j = Jobs::default();
        j.add("u", Path::new("/d"), vec![fetch("M\\A\\01.flac")]);
        assert!(!j.offered("u", "M\\A\\01.flac", 7, 50_000_000));
        assert!(j.list[0].failed().is_some());
    }

    #[test]
    fn names_that_come_out_the_same_are_kept_apart() {
        let mut j = Jobs::default();
        j.add("u", Path::new("/d"), vec![fetch("M\\A\\01.flac"), fetch("M\\A\\.01.flac"), fetch("M\\A\\01.FLAC")]);
        let rel: Vec<&str> = j.list[0].files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rel, ["01.flac", "01 (2).flac", "01 (3).FLAC"]);
    }

    #[test]
    fn a_file_cannot_be_put_outside_its_folder() {
        assert_eq!(safe_rel("../../etc/passwd"), "etc/passwd");
        assert_eq!(safe_rel("CD1\\01 a.flac"), "CD1/01 a.flac");
        assert_eq!(safe_rel(".hidden"), "hidden");
        assert_eq!(safe_rel("/"), "file");
    }

    #[test]
    fn downloads_carry_on_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.json");
        let mut j = Jobs::default();
        j.add("u", Path::new("/d"), vec![fetch("M\\A\\01.flac")]);
        j.offered("u", "M\\A\\01.flac", 7, 100);
        j.save(&path);
        let back = Jobs::load(&path);
        assert_eq!((back.next, back.list[0].files[0].state.clone()), (1, FileState::Waiting));
        let (to, part) = back.list[0].paths(&back.list[0].files[0]);
        assert_eq!((to.to_str().unwrap(), part.to_str().unwrap()), ("/d/01.flac", "/d/.01.flac.part"));
    }
}
