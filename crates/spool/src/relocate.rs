//! Moving a title from one location to another.
//!
//! A title's folder is copied whole to the other location under a working name, every file
//! is checked against its original, the copy is given the folder's name, the title is pointed
//! at it, and only then is the original taken away. A failure at any point leaves the title
//! where it was and playable; the worst an interruption can leave behind is a working folder
//! at the destination, cleared the next time, or the original still on its disk.
//!
//! A location Spool only reads gives nothing up. Moving a title off one copies it, and keeps
//! the original as a further copy of the title.

use crate::app::{App, Event};
use crate::models::*;
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// The titles waiting to be moved, and the one being moved now.
#[derive(Default)]
pub struct Moves {
    pub queue: VecDeque<(i64, u32)>,
    pub now: Option<Moving>,
    pub working: bool,
}

/// One move under way.
#[derive(Clone, Serialize)]
pub struct Moving {
    pub title_id: i64,
    pub to: String,
    #[serde(serialize_with = "count")]
    pub done: Arc<AtomicU64>,
    pub total: u64,
    /// Set when the person calls the move off; the copy stops at the next block.
    #[serde(skip)]
    pub stop: Arc<AtomicBool>,
    /// The location it is going to, by id, as it was asked for.
    #[serde(skip)]
    pub location: u32,
}

/// What came of asking for titles to be moved.
#[derive(Default, Serialize)]
pub struct Asked {
    pub queued: usize,
    /// The titles that were not put in line, each with the reason.
    pub refused: Vec<Refused>,
}

#[derive(Serialize)]
pub struct Refused {
    pub title_id: i64,
    pub title: String,
    pub why: String,
}

/// Where the line of moves is kept, so a restart does not forget it.
const LINE: &str = "moves_waiting";
/// Room left over on a disk after a move, so it is never filled to the brim.
const SPARE: u64 = 1 << 30;

/// What a file system or a file manager keeps about other files. It is not part of a title:
/// it is not copied with it, and it is cleared away with the original.
fn litter(name: &str) -> bool {
    name == ".DS_Store" || name == ".AppleDouble" || name == "@eaDir" || name.starts_with("._")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if litter(&name) || name == ".spool-recycle" || name == "#recycle" {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Clear a folder of what a file system left in it, deepest first, so the folder can go.
fn sweep(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let (p, name) = (e.path(), e.file_name().to_string_lossy().to_string());
        if litter(&name) {
            let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        } else if p.is_dir() && name != ".spool-recycle" && name != "#recycle" {
            sweep(&p);
        }
    }
}

/// For looking at a move as it goes: bytes a second to hold the copy to, which also has a
/// move within one disk copied instead of renamed. See tools/demo.mjs.
pub(crate) fn pace() -> Option<u64> {
    std::env::var("SPOOL_MOVE_PACE").ok().and_then(|p| p.parse().ok()).filter(|p| *p > 0)
}

fn same_disk(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    matches!((std::fs::metadata(a), std::fs::metadata(b)), (Ok(x), Ok(y)) if x.dev() == y.dev())
}

/// What a move that was called off ends with, so it is not reported as a failure.
pub(crate) const CALLED_OFF: &str = "the move was cancelled";

fn count<S: serde::Serializer>(n: &Arc<AtomicU64>, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_u64(n.load(Ordering::Relaxed))
}

/// Every file under a folder, relative to it, with its size: the film or episodes and all
/// that lies beside them, a media server's hidden notes such as `.plexmatch` included. What
/// a file system keeps for itself, and recycle folders, are left behind.
pub(crate) fn listing(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut files = vec![];
    walk(root, &mut files);
    files.into_iter().filter_map(|p| Some((p.strip_prefix(root).ok()?.to_path_buf(), std::fs::metadata(&p).ok()?.len()))).collect()
}

/// Copy a folder's files to another, counting bytes as they go, and check each against its
/// original by size.
pub(crate) fn copy_all(from: &Path, to: &Path, files: &[(PathBuf, u64)], done: &AtomicU64, stop: &AtomicBool) -> Result<()> {
    use std::io::{Read, Write};
    let mut buffer = vec![0u8; 4 << 20];
    let pace = pace();
    if pace.is_some() {
        buffer.truncate(256 << 10);
    }
    for (rel, size) in files {
        let (src, dst) = (from.join(rel), to.join(rel));
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut input = std::fs::File::open(&src).with_context(|| format!("reading {}", src.display()))?;
        let mut output = std::fs::File::create(&dst).with_context(|| format!("writing {}", dst.display()))?;
        loop {
            if stop.load(Ordering::Relaxed) {
                bail!(CALLED_OFF);
            }
            let n = input.read(&mut buffer).with_context(|| format!("reading {}", src.display()))?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n]).with_context(|| format!("writing {}", dst.display()))?;
            done.fetch_add(n as u64, Ordering::Relaxed);
            if let Some(pace) = pace {
                std::thread::sleep(std::time::Duration::from_millis(n as u64 * 1000 / pace));
            }
        }
        output.sync_all().with_context(|| format!("writing {}", dst.display()))?;
        let written = std::fs::metadata(&dst)?.len();
        if written != *size {
            bail!("{} came to {written} bytes where the original has {size}", dst.display());
        }
    }
    Ok(())
}

/// Take away the files that were copied, and the folders left empty by that. A file that is
/// no longer the size it was when it was copied is another file by now, and is left.
pub(crate) fn remove_all(root: &Path, files: &[(PathBuf, u64)]) {
    for (rel, size) in files {
        let path = root.join(rel);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.len() == *size) {
            let _ = std::fs::remove_file(path);
        }
    }
    sweep(root);
    let mut folders: Vec<PathBuf> = files.iter().filter_map(|(rel, _)| rel.parent().map(|p| root.join(p))).collect();
    folders.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    folders.dedup();
    for f in folders {
        let _ = std::fs::remove_dir(f);
    }
    let _ = std::fs::remove_dir(root);
}

impl App {
    /// Why a title cannot be moved to a location, or the folder it would have there.
    pub fn move_target(&self, t: &Title, location: u32) -> Result<PathBuf> {
        let places = self.placement();
        let Some((to, online)) = places.0.iter().find(|p| p.0.id == location) else { bail!("there is no such location") };
        if t.path.is_empty() {
            bail!("{} has no folder yet", t.title);
        }
        if !online {
            bail!("{} cannot be reached right now", to.name);
        }
        if !to.manage {
            bail!("{} is a location Spool only reads, so nothing can be put there", to.name);
        }
        let root = to.root(t.kind);
        if root.is_empty() {
            bail!("{} has no folder for {}", to.name, if t.kind == Kind::Movie { "films" } else { "series" });
        }
        if places.of(&t.path).is_some_and(|p| p.0.id == location) {
            bail!("{} is already on {}", t.title, to.name);
        }
        if !places.online(t) {
            bail!("{} is on a location that cannot be reached right now", t.title);
        }
        let name = Path::new(&t.path).file_name().ok_or_else(|| anyhow!("{} has no folder name", t.title))?;
        let target = Path::new(root).join(name);
        if target.exists() && std::fs::read_dir(&target).map(|mut d| d.next().is_some()).unwrap_or(true) {
            bail!("{} already has a folder called {}", to.name, name.to_string_lossy());
        }
        Ok(target)
    }

    /// How much room moving a title under a folder will take there, by the files Spool has
    /// recorded for it. Within one disk a folder is renamed into place and takes none, unless
    /// it is on a location Spool only reads, which keeps its original and is always copied.
    fn needs(&self, t: &Title, root: &Path) -> u64 {
        if same_disk(Path::new(&t.path), root) && !self.placement().read_only(t) {
            return 0;
        }
        self.db.files(t.id).map(|f| f.iter().filter(|f| f.base.is_none()).map(|f| f.size).sum()).unwrap_or(0)
    }

    /// Room on a location for what is still to arrive there: what is free now, less what the
    /// titles already in line for it will take.
    fn room_on(&self, location: u32, root: &Path) -> Option<u64> {
        let free = crate::app::free_space(root)?;
        let (now, waiting) = {
            let moves = self.moves.lock();
            (moves.now.clone(), moves.queue.iter().copied().collect::<Vec<_>>())
        };
        let coming = |id: i64| self.db.title(id).ok().flatten().map(|t| self.needs(&t, root)).unwrap_or(0);
        let mut owed: u64 = waiting.iter().filter(|m| m.1 == location).map(|m| coming(m.0)).sum();
        if let Some(m) = now.filter(|m| m.location == location) {
            owed += m.total.max(coming(m.title_id)).saturating_sub(m.done.load(Ordering::Relaxed));
        }
        Some(free.saturating_sub(owed))
    }

    /// Write the line down: the title being moved, then those waiting.
    fn keep_line(&self) {
        let line: Vec<(i64, u32)> = {
            let moves = self.moves.lock();
            moves.now.iter().map(|m| (m.title_id, m.location)).chain(moves.queue.iter().copied()).collect()
        };
        let _ = self.db.set_setting(LINE, &line);
    }

    /// Take up the moves a restart interrupted, after clearing away the half-made copy the
    /// interrupted one left behind. Says how many were put back in line.
    pub async fn resume_moves(&self) -> usize {
        let line: Vec<(i64, u32)> = self.db.get_setting(LINE);
        if self.moves.lock().working {
            return 0;
        }
        let roots: Vec<PathBuf> = self.placement().0.iter().filter(|p| p.1 && p.0.manage).flat_map(|p| [p.0.movie_root.clone(), p.0.series_root.clone()]).filter(|r| !r.is_empty()).map(PathBuf::from).collect();
        let _ = tokio::task::spawn_blocking(move || {
            for root in roots {
                for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
                    if e.file_name().to_string_lossy().starts_with(".spool-moving-") {
                        tracing::info!(folder = %e.path().display(), "clearing away a copy an interrupted move left behind");
                        let _ = std::fs::remove_dir_all(e.path());
                    }
                }
            }
        })
        .await;
        let mut resumed = 0;
        for (id, location) in &line {
            match self.move_titles(&[*id], *location) {
                Ok(asked) => resumed += asked.queued,
                Err(e) => {
                    tracing::warn!(title = id, error = %format!("{e:#}"), "a move interrupted by a restart could not be taken up again");
                    let _ = self.db.add_attention("config", Some(*id), None, &format!("A move was interrupted by a restart and could not be taken up again: {e:#}. The title is still where it was."), serde_json::json!({}));
                    self.emit(Event::Attention);
                }
            }
        }
        self.keep_line();
        if resumed > 0 {
            tracing::info!(resumed, "took up the moves interrupted by the last shutdown");
        }
        resumed
    }

    /// Put titles in line to be moved to a location, and start on them if nothing is moving.
    /// Each is checked first, so what cannot be moved is refused now and not later: among
    /// the reasons, that the location has no room for it once the titles ahead of it in line
    /// have arrived. One title alone that is refused is an error; of several, the refused are
    /// named in the answer.
    pub fn move_titles(&self, ids: &[i64], location: u32) -> Result<Asked> {
        let mut asked = Asked::default();
        let mut room: Option<Option<u64>> = None;
        let name = self.placement().0.iter().find(|p| p.0.id == location).map(|p| p.0.name.clone()).unwrap_or_default();
        for id in ids {
            let t = self.db.title(*id)?.ok_or_else(|| anyhow!("no such title"))?;
            let checked = (|| -> Result<u64> {
                let target = self.move_target(&t, location)?;
                if self.db.title_acquisitions(*id)?.iter().any(|a| a.state.is_active()) {
                    bail!("{} has a download under way; move it when that is done", t.title);
                }
                let root = target.parent().unwrap_or(&target);
                let need = self.needs(&t, root);
                let left = *room.get_or_insert_with(|| self.room_on(location, root));
                if let Some(left) = left.filter(|left| need > 0 && *left < need + SPARE) {
                    bail!("{name} has no room for {}: it needs {:.1} GB and {:.1} GB is left once the titles ahead of it have arrived", t.title, need as f64 / 1e9, left as f64 / 1e9);
                }
                Ok(need)
            })();
            let need = match checked {
                Ok(need) => need,
                Err(e) if ids.len() == 1 => return Err(e),
                Err(e) => {
                    asked.refused.push(Refused { title_id: *id, title: t.title.clone(), why: format!("{e:#}") });
                    continue;
                }
            };
            let mut moves = self.moves.lock();
            if !moves.queue.iter().any(|m| m.0 == *id) && moves.now.as_ref().is_none_or(|m| m.title_id != *id) {
                moves.queue.push_back((*id, location));
                asked.queued += 1;
                if let Some(Some(left)) = room.as_mut() {
                    *left = left.saturating_sub(need);
                }
            }
        }
        self.keep_line();
        let start = {
            let mut moves = self.moves.lock();
            !std::mem::replace(&mut moves.working, true)
        };
        if start {
            let app = self.clone();
            tokio::spawn(async move {
                loop {
                    // Taken from the line and marked as under way in one step, so there is no
                    // moment when a title being moved looks free to be worked on; and the
                    // line is closed in the same step it is found empty.
                    let (id, to) = {
                        let mut moves = app.moves.lock();
                        match moves.queue.pop_front() {
                            Some(next) => {
                                moves.now = Some(Moving { title_id: next.0, to: String::new(), done: Arc::new(AtomicU64::new(0)), total: 0, stop: Default::default(), location: next.1 });
                                next
                            }
                            None => {
                                moves.working = false;
                                break;
                            }
                        }
                    };
                    let name = app.db.title(id).ok().flatten().map(|t| t.title).unwrap_or_default();
                    match app.move_one(id, to).await {
                        Ok(said) => {
                            tracing::info!(title = %name, "{said}");
                            app.emit(Event::Task { name: "move".into(), running: false, message: said });
                        }
                        Err(e) if e.to_string() == CALLED_OFF => {
                            tracing::info!(title = %name, "a move was cancelled; the title is where it was");
                            app.emit(Event::Task { name: "move".into(), running: false, message: format!("The move of {name} was cancelled. It is still where it was.") });
                        }
                        Err(e) => {
                            tracing::warn!(title = %name, error = %format!("{e:#}"), "could not move a title");
                            let _ = app.db.add_attention("config", Some(id), None, &format!("{name} could not be moved: {e:#}. It is still where it was."), serde_json::json!({}));
                            app.emit(Event::Attention);
                        }
                    }
                    app.moves.lock().now = None;
                    app.keep_line();
                    app.emit(Event::Title { id });
                }
            });
        }
        Ok(asked)
    }

    /// Whether a title is being moved or waiting to be: its files are not to be touched meanwhile.
    pub fn moving(&self, id: i64) -> bool {
        let moves = self.moves.lock();
        moves.now.as_ref().is_some_and(|m| m.title_id == id) || moves.queue.iter().any(|m| m.0 == id)
    }

    /// Call moves off. Titles waiting their turn are taken out of the line: those named, or
    /// all of them when none are named. The move under way is stopped only when its title is
    /// named: its copy is abandoned and what was copied is removed, and the title stays where
    /// it was. Once the copy is complete the move is past stopping and finishes. Says how
    /// many were taken out of the line, and whether the move under way was told to stop.
    pub fn cancel_moves(&self, ids: &[i64]) -> (usize, bool) {
        let (taken, stopping): (Vec<i64>, bool) = {
            let mut moves = self.moves.lock();
            let taken = moves.queue.iter().map(|m| m.0).filter(|id| ids.is_empty() || ids.contains(id)).collect::<Vec<_>>();
            moves.queue.retain(|m| !taken.contains(&m.0));
            let stopping = moves.now.as_ref().is_some_and(|m| ids.contains(&m.title_id));
            if let Some(m) = moves.now.as_ref().filter(|_| stopping) {
                m.stop.store(true, Ordering::Relaxed);
            }
            (taken, stopping)
        };
        self.keep_line();
        for id in &taken {
            self.emit(Event::Title { id: *id });
        }
        (taken.len(), stopping)
    }

    /// The move under way and the titles waiting their turn, as Activity shows them.
    pub fn moves_shown(&self) -> serde_json::Value {
        let (now, waiting) = {
            let moves = self.moves.lock();
            (moves.now.clone(), moves.queue.iter().copied().collect::<Vec<_>>())
        };
        if now.is_none() && waiting.is_empty() {
            return serde_json::json!({"now": null, "waiting": []});
        }
        let places = self.placement();
        let shown = |id: i64| {
            let t = self.db.title(id).ok().flatten();
            let from = t.as_ref().and_then(|t| places.of(&t.path)).map(|p| p.0.name.clone());
            serde_json::json!({
                "title_id": id,
                "title": t.as_ref().map(|t| t.title.clone()),
                "kind": t.as_ref().map(|t| t.kind),
                "poster": t.as_ref().and_then(crate::art::small_url),
                "from": from,
            })
        };
        let now = now.map(|m| {
            let mut v = shown(m.title_id);
            v["to"] = m.to.clone().into();
            v["done"] = m.done.load(Ordering::Relaxed).min(m.total).into();
            v["total"] = m.total.into();
            v["stopping"] = m.stop.load(Ordering::Relaxed).into();
            v
        });
        let waiting: Vec<serde_json::Value> = waiting
            .iter()
            .map(|(id, location)| {
                let mut v = shown(*id);
                v["to"] = places.0.iter().find(|p| p.0.id == *location).map(|p| p.0.name.clone()).into();
                v
            })
            .collect();
        serde_json::json!({"now": now, "waiting": waiting})
    }

    async fn move_one(&self, id: i64, location: u32) -> Result<String> {
        let title = self.db.title(id)?.ok_or_else(|| anyhow!("the title was removed"))?;
        let target = self.move_target(&title, location)?;
        let places = self.placement();
        let to_name = places.0.iter().find(|p| p.0.id == location).map(|p| p.0.name.clone()).unwrap_or_default();
        let from_name = places.of(&title.path).map(|p| p.0.name.clone()).unwrap_or_default();
        // A location Spool only reads gives nothing up: the original stays, as a copy.
        let keep_original = places.read_only(&title);
        let from = PathBuf::from(&title.path);
        // When both ends are on one helper's disks, the helper moves it there and nothing
        // crosses the network.
        let helper = self.remotes().into_iter().find(|r| r.covers(&from) && r.covers(&target) && r.seen().usable);
        let files = match helper {
            // The helper reads the folder itself.
            Some(_) => vec![],
            None => {
                let from = from.clone();
                tokio::task::spawn_blocking(move || listing(&from)).await?
            }
        };
        let mut total: u64 = if helper.is_some() { self.needs(&title, target.parent().unwrap_or(&target)) } else { files.iter().map(|f| f.1).sum() };
        if let Some(free) = crate::app::free_space(target.parent().unwrap_or(&target)) {
            if (keep_original || !same_disk(&from, target.parent().unwrap_or(&target))) && free < total + SPARE {
                bail!("{to_name} has {:.1} GB free and the title needs {:.1} GB", free as f64 / 1e9, total as f64 / 1e9);
            }
        }
        let done = Arc::new(AtomicU64::new(0));
        // The flag made when the title was taken from the line is kept, so a move called off
        // while its folder was being read is still called off.
        let stop = {
            let mut moves = self.moves.lock();
            let stop = moves.now.as_ref().map(|m| m.stop.clone()).unwrap_or_default();
            moves.now = Some(Moving { title_id: id, to: to_name.clone(), done: done.clone(), total, stop: stop.clone(), location });
            stop
        };
        if stop.load(Ordering::Relaxed) {
            bail!(CALLED_OFF);
        }
        self.emit(Event::Title { id });

        // Under a working name first, so a half-made folder is never taken for the title's.
        let working = target.with_file_name(format!(".spool-moving-{id}"));
        let mut handed: Option<(Arc<crate::remote::Remote>, String)> = None;
        // The helper restarted after the title had arrived and before it could say so, and
        // the original may still be where it was.
        let mut left_behind = false;
        if let Some(r) = helper {
            let there = |p: &Path| r.there(p).ok_or_else(|| anyhow!("{} is not on {}'s disks", p.display(), r.conf.name));
            let op = r.move_start(&there(&from)?, &there(&target)?, &there(&working)?, keep_original).await?;
            let (mut told, mut silent, mut moved_by_helper) = (false, 0u32, true);
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if stop.load(Ordering::Relaxed) && !told {
                    told = r.move_cancel(&op).await.is_ok();
                }
                let said = match r.move_state(&op).await {
                    Ok(Some(said)) => said,
                    // A helper that has restarted has forgotten the move, and cleared away
                    // what it had half copied. A folder under the title's own name is only
                    // ever a whole one, so if that is there the title did arrive.
                    Ok(None) => {
                        let (t, f) = (target.clone(), from.clone());
                        let (arrived, original) = tokio::task::spawn_blocking(move || (t.is_dir(), f.is_dir())).await?;
                        if !arrived {
                            bail!("{}'s helper was restarted in the middle of the move", r.conf.name);
                        }
                        (left_behind, moved_by_helper) = (original, false);
                        break;
                    }
                    // One that has gone quiet is waited for: it may well be copying still.
                    Err(e) => {
                        silent += 1;
                        if silent > 1200 {
                            bail!("{e:#}. If it finished the copy without Spool, a folder for {} is on {to_name} as well as where it was", title.title);
                        }
                        continue;
                    }
                };
                silent = 0;
                done.store(said["done"].as_u64().unwrap_or(0), Ordering::Relaxed);
                if let Some(now) = said["total"].as_u64().filter(|t| *t > 0 && *t != total) {
                    total = now;
                    if let Some(m) = self.moves.lock().now.as_mut() {
                        m.total = now;
                    }
                }
                match said["state"].as_str().unwrap_or_default() {
                    "running" => continue,
                    "done" => break,
                    ended => {
                        let _ = r.move_finish(&op, false).await;
                        bail!("{}", if ended == "cancelled" { CALLED_OFF } else { ended });
                    }
                }
            }
            handed = moved_by_helper.then_some((r, op));
        } else {
            let (w, t, f, fl, d, st) = (working.clone(), target.clone(), from.clone(), files.clone(), done.clone(), stop.clone());
            tokio::task::spawn_blocking(move || -> Result<()> {
                let _ = std::fs::remove_dir_all(&w);
                // On the same disk a folder is moved by renaming it, which takes no time.
                if !keep_original && pace().is_none() && std::fs::rename(&f, &t).is_ok() {
                    return Ok(());
                }
                let copied = copy_all(&f, &w, &fl, &d, &st).and_then(|_| std::fs::rename(&w, &t).with_context(|| format!("naming {}", t.display())));
                if copied.is_err() {
                    let _ = std::fs::remove_dir_all(&w);
                }
                copied
            })
            .await??;
        }

        // The title now lives at the new place. Its files keep their names within the folder.
        let mut moved = self.db.title(id)?.ok_or_else(|| anyhow!("the title was removed"))?;
        moved.path = target.to_string_lossy().to_string();
        // An original that is still there is recorded as a copy, never lost track of.
        if (keep_original || left_behind) && !moved.also.iter().any(|a| Path::new(a) == from) {
            moved.also.push(from.to_string_lossy().to_string());
        }
        self.db.save_title(&mut moved)?;
        self.db.add_history(Some(id), "moved", serde_json::json!({"from": from, "to": target, "bytes": total, "kept_original": keep_original}))?;
        match handed {
            // The helper takes the original away, now that the move is on record.
            Some((r, op)) => {
                if let Err(e) = r.move_finish(&op, !keep_original).await {
                    tracing::warn!(title = %title.title, error = %format!("{e:#}"), "the helper did not take the original away; it is still in its old folder");
                }
            }
            None if !keep_original && from.exists() => {
                let (f, fl) = (from.clone(), files);
                tokio::task::spawn_blocking(move || remove_all(&f, &fl)).await?;
            }
            None => {}
        }
        // Whatever could not be taken away, or was changed while the title was being moved
        // and so was left, is still a folder of this title's, and is recorded as one.
        if !keep_original && !left_behind {
            let f = from.clone();
            if tokio::task::spawn_blocking(move || f.is_dir()).await? && !moved.also.iter().any(|a| Path::new(a) == from) {
                moved.also.push(from.to_string_lossy().to_string());
                self.db.save_title(&mut moved)?;
            }
        }
        let mut report = crate::catalog::ScanReport::default();
        self.scan_title(id, &mut report).await?;
        for folder in [from.to_string_lossy().to_string(), moved.path.clone()] {
            let app = self.clone();
            tokio::spawn(async move {
                let _ = app.plex_refresh(&folder).await;
            });
        }
        Ok(if keep_original { format!("{} copied to {to_name}; the original stays on {from_name}, which Spool only reads", title.title) } else { format!("{} moved from {from_name} to {to_name}", title.title) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_copied_whole_checked_and_only_then_taken_away() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("here/Film (1999)"), dir.path().join("there/.spool-moving-1"));
        let lay = |rel: &str, bytes: &[u8]| {
            let p = from.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        };
        lay("Film (1999).mkv", &vec![7u8; 5 << 20]);
        lay("Film (1999).en.srt", b"1\n");
        lay("Trailers/teaser.mkv", b"teaser");
        lay("poster.jpg", b"jpg");
        // What a file system keeps for itself is not part of the title and stays behind.
        lay(".AppleDouble/Film (1999).mkv", b"junk");
        lay("._Film (1999).mkv", b"junk");
        lay(".DS_Store", b"junk");
        // A media server's hidden note is part of it.
        lay(".plexmatch", b"tmdbid: 949\n");
        lay(".spool-recycle/old.mkv", b"recycled");
        let files = listing(&from);
        assert_eq!(files.len(), 5);
        let done = AtomicU64::new(0);
        copy_all(&from, &to, &files, &done, &AtomicBool::new(false)).unwrap();
        assert_eq!(done.load(Ordering::Relaxed), files.iter().map(|f| f.1).sum::<u64>());
        assert_eq!(std::fs::read(to.join("Trailers/teaser.mkv")).unwrap(), b"teaser");
        assert_eq!(std::fs::metadata(to.join("Film (1999).mkv")).unwrap().len(), 5 << 20);
        assert!(from.join("Film (1999).mkv").exists(), "the original is there until it is taken away");
        // Called off, a copy stops before it reads another block and takes nothing from the original.
        let (again, counted) = (dir.path().join("there/.spool-moving-2"), AtomicU64::new(0));
        let stopped = copy_all(&from, &again, &files, &counted, &AtomicBool::new(true)).unwrap_err();
        assert_eq!((stopped.to_string().as_str(), counted.load(Ordering::Relaxed)), (CALLED_OFF, 0));
        assert!(from.join("Film (1999).mkv").exists());
        remove_all(&from, &files);
        assert!(!from.join("Film (1999).mkv").exists() && !from.join("Trailers").exists());
        assert!(!from.join(".AppleDouble").exists() && !from.join(".DS_Store").exists(), "what the file system left is cleared away");
        assert!(from.join(".spool-recycle/old.mkv").exists(), "what was recycled stays, and can be got back");
        assert!(to.join(".plexmatch").exists() && !to.join(".DS_Store").exists() && !to.join(".spool-recycle").exists());
    }
}
