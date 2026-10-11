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
use std::sync::atomic::{AtomicU64, Ordering};
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
}

fn count<S: serde::Serializer>(n: &Arc<AtomicU64>, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_u64(n.load(Ordering::Relaxed))
}

/// Every file under a folder, relative to it, with its size. What a file system keeps for
/// itself, and Spool's own recycle folder, are left behind.
fn listing(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut files = vec![];
    crate::catalog::collect(root, &mut files);
    files.into_iter().filter(|p| !p.components().any(|c| c.as_os_str() == ".spool-recycle")).filter_map(|p| Some((p.strip_prefix(root).ok()?.to_path_buf(), std::fs::metadata(&p).ok()?.len()))).collect()
}

/// Copy a folder's files to another, counting bytes as they go, and check each against its
/// original by size.
fn copy_all(from: &Path, to: &Path, files: &[(PathBuf, u64)], done: &AtomicU64) -> Result<()> {
    use std::io::{Read, Write};
    let mut buffer = vec![0u8; 4 << 20];
    for (rel, size) in files {
        let (src, dst) = (from.join(rel), to.join(rel));
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut input = std::fs::File::open(&src).with_context(|| format!("reading {}", src.display()))?;
        let mut output = std::fs::File::create(&dst).with_context(|| format!("writing {}", dst.display()))?;
        loop {
            let n = input.read(&mut buffer).with_context(|| format!("reading {}", src.display()))?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n]).with_context(|| format!("writing {}", dst.display()))?;
            done.fetch_add(n as u64, Ordering::Relaxed);
        }
        output.sync_all().with_context(|| format!("writing {}", dst.display()))?;
        let written = std::fs::metadata(&dst)?.len();
        if written != *size {
            bail!("{} came to {written} bytes where the original has {size}", dst.display());
        }
    }
    Ok(())
}

/// Take away the files that were copied, and the folders left empty by that.
fn remove_all(root: &Path, files: &[(PathBuf, u64)]) {
    for (rel, _) in files {
        let _ = std::fs::remove_file(root.join(rel));
    }
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

    /// Put titles in line to be moved to a location, and start on them if nothing is moving.
    /// Each is checked first, so what cannot be moved is refused now and not later.
    pub fn move_titles(&self, ids: &[i64], location: u32) -> Result<usize> {
        let mut queued = 0;
        for id in ids {
            let t = self.db.title(*id)?.ok_or_else(|| anyhow!("no such title"))?;
            if let Err(e) = self.move_target(&t, location) {
                if ids.len() == 1 {
                    return Err(e);
                }
                continue;
            }
            if self.db.title_acquisitions(*id)?.iter().any(|a| a.state.is_active()) {
                if ids.len() == 1 {
                    bail!("{} has a download under way; move it when that is done", t.title);
                }
                continue;
            }
            let mut moves = self.moves.lock();
            if !moves.queue.iter().any(|m| m.0 == *id) && moves.now.as_ref().is_none_or(|m| m.title_id != *id) {
                moves.queue.push_back((*id, location));
                queued += 1;
            }
        }
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
                                moves.now = Some(Moving { title_id: next.0, to: String::new(), done: Arc::new(AtomicU64::new(0)), total: 0 });
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
                        Err(e) => {
                            tracing::warn!(title = %name, error = %format!("{e:#}"), "could not move a title");
                            let _ = app.db.add_attention("config", Some(id), None, &format!("{name} could not be moved: {e:#}. It is still where it was."), serde_json::json!({}));
                            app.emit(Event::Attention);
                        }
                    }
                    app.moves.lock().now = None;
                    app.emit(Event::Title { id });
                }
            });
        }
        Ok(queued)
    }

    /// Whether a title is being moved or waiting to be: its files are not to be touched meanwhile.
    pub fn moving(&self, id: i64) -> bool {
        let moves = self.moves.lock();
        moves.now.as_ref().is_some_and(|m| m.title_id == id) || moves.queue.iter().any(|m| m.0 == id)
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
        let files = {
            let from = from.clone();
            tokio::task::spawn_blocking(move || listing(&from)).await?
        };
        let total: u64 = files.iter().map(|f| f.1).sum();
        if let Some(free) = crate::app::free_space(target.parent().unwrap_or(&target)) {
            if free < total + (1 << 30) {
                bail!("{to_name} has {:.1} GB free and the title needs {:.1} GB", free as f64 / 1e9, total as f64 / 1e9);
            }
        }
        let done = Arc::new(AtomicU64::new(0));
        self.moves.lock().now = Some(Moving { title_id: id, to: to_name.clone(), done: done.clone(), total });
        self.emit(Event::Title { id });

        // Under a working name first, so a half-made folder is never taken for the title's.
        let working = target.with_file_name(format!(".spool-moving-{id}"));
        let (w, t, f, fl, d) = (working.clone(), target.clone(), from.clone(), files.clone(), done.clone());
        tokio::task::spawn_blocking(move || -> Result<()> {
            let _ = std::fs::remove_dir_all(&w);
            // On the same disk a folder is moved by renaming it, which takes no time.
            if !keep_original && std::fs::rename(&f, &t).is_ok() {
                return Ok(());
            }
            let copied = copy_all(&f, &w, &fl, &d).and_then(|_| std::fs::rename(&w, &t).with_context(|| format!("naming {}", t.display())));
            if copied.is_err() {
                let _ = std::fs::remove_dir_all(&w);
            }
            copied
        })
        .await??;

        // The title now lives at the new place. Its files keep their names within the folder.
        let mut moved = self.db.title(id)?.ok_or_else(|| anyhow!("the title was removed"))?;
        moved.path = target.to_string_lossy().to_string();
        if keep_original && !moved.also.iter().any(|a| Path::new(a) == from) {
            moved.also.push(from.to_string_lossy().to_string());
        }
        self.db.save_title(&mut moved)?;
        self.db.add_history(Some(id), "moved", serde_json::json!({"from": from, "to": target, "bytes": total, "kept_original": keep_original}))?;
        if !keep_original && from.exists() {
            let (f, fl) = (from.clone(), files);
            tokio::task::spawn_blocking(move || remove_all(&f, &fl)).await?;
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
        let files = listing(&from);
        assert_eq!(files.len(), 4);
        let done = AtomicU64::new(0);
        copy_all(&from, &to, &files, &done).unwrap();
        assert_eq!(done.load(Ordering::Relaxed), files.iter().map(|f| f.1).sum::<u64>());
        assert_eq!(std::fs::read(to.join("Trailers/teaser.mkv")).unwrap(), b"teaser");
        assert_eq!(std::fs::metadata(to.join("Film (1999).mkv")).unwrap().len(), 5 << 20);
        assert!(from.join("Film (1999).mkv").exists(), "the original is there until it is taken away");
        remove_all(&from, &files);
        assert!(!from.join("Film (1999).mkv").exists() && !from.join("Trailers").exists());
        assert!(from.join(".AppleDouble").exists(), "what was not copied is not removed");
    }
}
