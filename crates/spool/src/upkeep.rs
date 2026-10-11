//! Questions about Spool itself and how it is set up: whether it can be restarted without
//! interrupting anything, whether a location works, and whether the profiles describe what
//! is on disk. Each of these was once answered by hand with a script.

use crate::app::App;
use crate::db::now;
use crate::models::*;
use crate::settings::Location;
use anyhow::Result;
use serde::Serialize;
use spool_nntp::JobState;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Whether stopping Spool now would interrupt something that does not simply carry on
/// afterwards.
#[derive(Serialize)]
pub struct Busy {
    pub busy: bool,
    /// What would be interrupted, in words.
    pub reasons: Vec<String>,
    /// Downloads that would pause and pick up where they left off.
    pub resumable: usize,
}

/// One thing looked at on a location, and how it went.
#[derive(Serialize)]
pub struct Checked {
    pub what: String,
    pub ok: bool,
    pub detail: String,
}

/// How a profile fits the files of the titles that use it, on one location.
#[derive(Default, Serialize)]
pub struct Fit {
    pub location: String,
    pub profile: String,
    pub kind: String,
    pub titles: usize,
    pub monitored: usize,
    pub files: usize,
    /// Files in a quality the profile does not allow.
    pub not_allowed: usize,
    /// Files over the profile's size target, and by how much in all.
    pub over_target: usize,
    pub over_bytes: u64,
    /// Files the profile would replace with something better, were the title monitored.
    pub below_cutoff: usize,
    pub upgrades: bool,
    /// What to make of it, when the profile plainly does not describe what is here.
    pub note: String,
}

/// Run something that may never return, a call into a share that has gone, and give up on it
/// after a while. The thread is left to finish by itself.
fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(limit).ok()
}

/// The first ordinary file of some size under a folder, looking a few levels down.
fn a_file(root: &Path, depth: u32) -> Option<(PathBuf, u64)> {
    let mut folders = vec![];
    for e in std::fs::read_dir(root).ok()?.flatten().take(200) {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "@eaDir" {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_file() && meta.len() > (1 << 20) {
            return Some((e.path(), meta.len()));
        }
        if meta.is_dir() {
            folders.push(e.path());
        }
    }
    if depth == 0 {
        return None;
    }
    folders.into_iter().take(8).find_map(|f| a_file(&f, depth - 1))
}

fn check_root(label: &str, root: &str, manage: bool) -> Vec<Checked> {
    let mut out = vec![];
    let mut say = |what: &str, ok: bool, detail: String| out.push(Checked { what: format!("{label}: {what}"), ok, detail });
    let path = PathBuf::from(root);
    let mac = cfg!(target_os = "macos");

    let p = path.clone();
    match within(Duration::from_secs(5), move || std::fs::metadata(&p).map(|m| m.is_dir())) {
        Some(Ok(true)) => say("the folder is there", true, root.to_string()),
        Some(Ok(false)) => return vec![Checked { what: format!("{label}: the folder is there"), ok: false, detail: format!("{root} is a file, not a folder") }],
        Some(Err(e)) => return vec![Checked { what: format!("{label}: the folder is there"), ok: false, detail: format!("{root}: {e}. If it is a share from another machine, it is not mounted at this path.") }],
        None => return vec![Checked { what: format!("{label}: the folder is there"), ok: false, detail: format!("{root} did not answer within 5 seconds. The machine it is on may be off, or the share has gone stale and needs mounting again.") }],
    }

    let p = path.clone();
    match within(Duration::from_secs(8), move || std::fs::read_dir(&p).map(|d| d.take(5000).count())) {
        Some(Ok(n)) => say("its contents can be listed", true, format!("{n} entries")),
        Some(Err(e)) => {
            say("its contents can be listed", false, format!("{e}. The user Spool runs as may not be allowed to read this folder."));
            return out;
        }
        None => {
            let why = if mac {
                "The folder is there and listing it never answers. On a Mac this is the privacy protection for network and removable volumes: give Spool's program Full Disk Access under System Settings, Privacy & Security, or answer the question waiting on that Mac's screen."
            } else {
                "The folder is there and listing it never answers. The share may have gone stale; mount it again."
            };
            say("its contents can be listed", false, why.to_string());
            return out;
        }
    }

    let p = path.clone();
    let read = within(Duration::from_secs(25), move || -> std::io::Result<Option<(String, u64, f64)>> {
        use std::io::Read;
        let Some((file, _)) = a_file(&p, 3) else { return Ok(None) };
        let started = Instant::now();
        let mut f = std::fs::File::open(&file)?;
        let mut buffer = vec![0u8; 1 << 20];
        let mut total = 0u64;
        while total < (16 << 20) {
            let n = f.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            total += n as u64;
        }
        Ok(Some((file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), total, started.elapsed().as_secs_f64())))
    });
    match read {
        Some(Ok(Some((name, bytes, secs)))) => say("a file can be read", true, format!("{:.0} MB of {name} in {secs:.2} s ({:.0} MB/s)", bytes as f64 / 1e6, bytes as f64 / 1e6 / secs.max(0.001))),
        Some(Ok(None)) => say("a file can be read", true, "no file to try it on: the folder has nothing in it yet".into()),
        Some(Err(e)) => say("a file can be read", false, format!("{e}")),
        None => say("a file can be read", false, "reading did not finish within 25 seconds; the connection to this location is very slow or stalled".into()),
    }

    if manage {
        let p = path.join(format!(".spool-check-{}-{}", std::process::id(), now()));
        let wrote = within(Duration::from_secs(20), move || -> std::io::Result<f64> {
            let started = Instant::now();
            let data = vec![0u8; 4 << 20];
            let made = std::fs::write(&p, &data).and_then(|_| std::fs::metadata(&p).map(|m| m.len()));
            let gone = std::fs::remove_file(&p);
            match made {
                Ok(len) if len == data.len() as u64 => gone.map(|_| started.elapsed().as_secs_f64()),
                Ok(len) => Err(std::io::Error::other(format!("wrote 4 MB and found {len} bytes"))),
                Err(e) => Err(e),
            }
        });
        match wrote {
            Some(Ok(secs)) => say("Spool can write here", true, format!("wrote and removed a 4 MB test file in {secs:.2} s")),
            Some(Err(e)) => say("Spool can write here", false, format!("{e}. The location is set for Spool to change files, and it cannot: the share may be mounted or exported read-only, or the user Spool runs as may not own the folder.")),
            None => say("Spool can write here", false, "writing a small test file did not finish within 20 seconds".into()),
        }
    } else {
        say("Spool can write here", true, "not tried: Spool only reads this location".into());
    }
    match crate::app::free_space(&path) {
        Some(free) => say("room", free > (5 << 30) || !manage, format!("{:.1} GB free", free as f64 / 1e9)),
        None => say("room", true, "the free space could not be read".into()),
    }
    out
}

impl App {
    /// Whether Spool can be stopped now without interrupting anything. Downloads in flight
    /// do not count: they are checkpointed and resume.
    pub fn busy(&self) -> Busy {
        let mut reasons = vec![];
        let many = |n: usize, one: &str, more: &str| format!("{n} {}", if n == 1 { one } else { more });
        let jobs = self.engine.jobs();
        let working = jobs.iter().filter(|j| matches!(j.state, JobState::Verifying | JobState::Repairing | JobState::Extracting | JobState::Finishing)).count();
        if working > 0 {
            reasons.push(format!("{} being checked, repaired or unpacked", many(working, "download is", "downloads are")));
        }
        let importing = self.db.active_acquisitions().unwrap_or_default().iter().filter(|a| a.state == AcqState::Importing).count();
        if importing > 0 {
            reasons.push(format!("{} being filed into the library", many(importing, "download is", "downloads are")));
        }
        let moving = {
            let m = self.moves.lock();
            m.queue.len() + m.now.is_some() as usize
        };
        if moving > 0 {
            reasons.push(format!("{} being moved to another location", many(moving, "title is", "titles are")));
        }
        if self.adoption.lock().as_ref().is_some_and(|a| a.running) {
            reasons.push("a location is being looked through or added from".into());
        }
        let played = now() - self.last_playing.load(Ordering::Relaxed);
        if played < 120 {
            reasons.push(format!("a player was playing {played} seconds ago"));
        }
        Busy { busy: !reasons.is_empty(), reasons, resumable: jobs.iter().filter(|j| matches!(j.state, JobState::Downloading | JobState::Queued)).count() }
    }

    /// Try a location out: is each folder there, can it be listed and read, can it be written
    /// where Spool is meant to write, and how fast. Each step gives up after a while, so a
    /// share that has gone is reported and not waited on.
    pub async fn check_up(&self, location: &Location) -> Vec<Checked> {
        let l = location.clone();
        tokio::task::spawn_blocking(move || {
            let mut out = vec![];
            for (label, root) in [("Movies", &l.movie_root), ("Series", &l.series_root)] {
                if !root.is_empty() {
                    out.extend(check_root(label, root, l.manage));
                }
            }
            if out.is_empty() {
                out.push(Checked { what: "folders".into(), ok: false, detail: "the location has no folder set for films or for series".into() });
            }
            out
        })
        .await
        .unwrap_or_default()
    }

    /// The profile new titles get on a location: the one set for that location, or the usual one.
    pub fn profile_for_new(&self, kind: Kind, location: u32) -> i64 {
        let g = self.settings.general();
        let own = g.locations.iter().find(|l| l.id == location && location != 0).map(|l| if kind == Kind::Movie { l.movie_profile } else { l.series_profile }).unwrap_or(0);
        let usual = if kind == Kind::Movie { g.default_movie_profile } else { g.default_series_profile };
        if own != 0 && self.db.profiles().is_ok_and(|p| p.iter().any(|p| p.id == own)) {
            own
        } else {
            usual
        }
    }

    /// As [`profile_for_new`], for a title that will live in this folder.
    pub fn profile_for_folder(&self, kind: Kind, folder: &str) -> i64 {
        let location = self.placement().of(folder).map(|p| p.0.id).unwrap_or(0);
        self.profile_for_new(kind, location)
    }

    /// How each profile fits the files of the titles that use it, location by location. A
    /// profile that does not describe what is on disk is harmless while nothing is monitored
    /// and a trap once something is: it will try to make the files into what it describes.
    pub fn profile_fit(&self) -> Result<Vec<Fit>> {
        let places = self.placement();
        let profiles = self.db.profiles()?;
        let mut files: std::collections::HashMap<i64, Vec<MediaFile>> = Default::default();
        for f in self.db.all_files()?.into_iter().filter(|f| f.base.is_none()) {
            files.entry(f.title_id).or_default().push(f);
        }
        let mut rows: std::collections::BTreeMap<(String, i64), Fit> = Default::default();
        for t in self.db.titles(None)? {
            let Some(profile) = profiles.iter().find(|p| p.id == t.profile_id) else { continue };
            let location = places.of(&t.path).map(|p| p.0.name.clone()).unwrap_or_default();
            let row = rows.entry((location.clone(), profile.id)).or_insert_with(|| Fit { location, profile: profile.name.clone(), kind: if t.kind == Kind::Movie { "films".into() } else { "series".into() }, upgrades: profile.upgrade_allowed, ..Default::default() });
            row.titles += 1;
            row.monitored += t.monitored as usize;
            for f in files.get(&t.id).into_iter().flatten().filter(|f| !f.extra) {
                row.files += 1;
                if !profile.allows(f.quality.quality) {
                    row.not_allowed += 1;
                }
                if let Some(ceiling) = profile.size_ceiling().filter(|c| f.size > *c) {
                    row.over_target += 1;
                    row.over_bytes += f.size - ceiling;
                }
                if profile.upgrade_allowed && profile.allows(f.quality.quality) && profile.compare(f.quality.quality, profile.cutoff) == std::cmp::Ordering::Less {
                    row.below_cutoff += 1;
                }
            }
        }
        let mut out: Vec<Fit> = rows.into_values().collect();
        for r in &mut out {
            let off = r.not_allowed + r.over_target;
            if r.files >= 4 && off * 4 >= r.files {
                r.note = if r.monitored == 0 {
                    format!("{} of {} files are not what this profile asks for. Nothing here is monitored, so nothing will change; a profile that keeps things as they are would describe it better.", off, r.files)
                } else {
                    format!("{} of {} files are not what this profile asks for, and {} titles are monitored: Spool will work to replace them. Check that is what you want.", off, r.files, r.monitored)
                };
            }
        }
        Ok(out)
    }
}
