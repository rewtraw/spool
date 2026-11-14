//! What is wanted from the indexers besides films and episodes: an album, an audiobook. Each
//! goes Wanted → Downloading → Have by the same steps, kept here. A release is looked for, the
//! best one started, its download followed, and when it finishes it is handed back to be
//! checked and filed. A release that turns out not to do is remembered and passed over, and
//! the next best taken.
//!
//! What differs between them is what to search for, how a release is judged, and how a
//! download is checked and filed: each kind says that for itself.

use crate::app::App;
use crate::db::now;
use crate::models::Release;
use crate::music::scan;
use crate::newznab::Query;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use spool_nntp::JobState;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HuntState {
    #[default]
    Wanted,
    Downloading,
    Have,
}

impl HuntState {
    pub fn as_str(self) -> &'static str {
        match self {
            HuntState::Wanted => "wanted",
            HuntState::Downloading => "downloading",
            HuntState::Have => "have",
        }
    }
}

/// A release that would not do, and why.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TurnedDown {
    pub guid: String,
    pub name: String,
    pub why: String,
    pub at: i64,
}

/// Where a wanted thing has got to. Kept inside it, and saved as its own fields.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Hunt {
    pub state: HuntState,
    /// What is happening to it, or what it waits for, in words; empty when nothing needs saying.
    pub note: String,
    pub release: Option<Release>,
    pub job_id: Option<String>,
    pub turned_down: Vec<TurnedDown>,
    pub searched_at: i64,
}

/// Not found is looked for again after this long.
const AGAIN: i64 = 6 * 3600;
/// After a few releases that did not do in an hour, the rest wait for the next.
const LATER: i64 = AGAIN - 3600;

impl Hunt {
    /// The download under way, if there is one.
    pub fn download(&self) -> Option<&str> {
        self.job_id.as_deref().filter(|_| self.state == HuntState::Downloading)
    }

    /// Look for it again now, giving releases turned down another chance. False when it is
    /// not being looked for.
    pub fn search_again(&mut self) -> bool {
        if self.state != HuntState::Wanted {
            return false;
        }
        self.searched_at = 0;
        self.turned_down.clear();
        self.note = "Looking for it.".into();
        true
    }

    /// Here, without a download: found where it is kept.
    pub fn found(&mut self, note: &str) {
        self.state = HuntState::Have;
        self.note = note.into();
    }

    /// The download was filed.
    pub fn filed(&mut self) {
        self.state = HuntState::Have;
        self.job_id = None;
        self.note.clear();
    }

    /// The download did not do. When the release is at fault it is passed over from now on.
    pub fn failed(&mut self, why: &str, release_at_fault: bool) {
        if let (true, Some(r)) = (release_at_fault, &self.release) {
            self.turned_down.push(TurnedDown { guid: r.guid.clone(), name: r.title.clone(), why: why.to_string(), at: now() });
        }
        self.state = HuntState::Wanted;
        self.job_id = None;
        self.release = None;
        self.searched_at = 0;
        self.note = format!("A download did not do: {why}. Trying the next.");
    }

    /// Downloaded, but it could not be filed now; it is tried again later.
    pub fn not_filed_yet(&mut self, why: &str) {
        self.note = format!("Downloaded, but not filed yet: {why}. It is tried again later.");
    }
}

/// What to look for, and how to judge what is found.
pub struct Search<'a, G> {
    /// What it is, for the log and the notes: "album", "audiobook".
    pub what: &'static str,
    /// Its name, for the log.
    pub name: &'a str,
    /// Why it cannot be filed, when no folder for it can be reached.
    pub no_folder: Option<&'a str>,
    /// Asked in turn until one finds something.
    pub queries: Vec<Query>,
    /// How good a copy a release would give, or None when it is not the thing or will not do,
    /// given the query that found it. The best is taken, then the indexer put first, then the
    /// newest.
    pub grade: &'a (dyn Fn(&Release, usize) -> Option<G> + Sync),
    /// What is said when nothing was found at all.
    pub none: &'a str,
}

/// Where its download has got to.
pub enum Followed {
    /// Still going, or nothing can be known now.
    Going,
    /// Failed or gone; whether the release was at fault.
    Failed(String, bool),
    /// Finished: the job, and the folder it left.
    Finished(String, PathBuf),
}

/// Why a release could not be started.
pub(crate) enum Grab {
    /// The release is at fault, and is passed over from now on.
    Release(String),
    /// Something else is, and the release may be tried again.
    Later(String),
}

/// Why a finished download was not filed.
pub(crate) enum Filing {
    /// Not the thing, or not all of it.
    Wrong(String),
    /// Could not be filed now, and may be later.
    Later(String),
}

/// The release to take: the best grade not turned down, then the indexer put first, then the newest.
pub(crate) fn best<'a, G: Ord + Copy>(found: &'a [(Release, G)], turned_down: &[TurnedDown]) -> Option<&'a (Release, G)> {
    found.iter().filter(|(r, _)| !turned_down.iter().any(|t| t.guid == r.guid || t.name == r.title)).max_by_key(|(r, g)| (*g, r.indexer_priority, r.published))
}

impl App {
    /// Look for it, and start the best release found downloading. Gives the grade of the one
    /// started.
    pub(crate) async fn hunt_search<G: Ord + Copy>(&self, h: &mut Hunt, s: Search<'_, G>) -> Result<Option<G>> {
        if now() - h.searched_at < AGAIN {
            return Ok(None);
        }
        // Each release that did not do cost a download from an indexer's daily allowance.
        let lately = h.turned_down.iter().filter(|t| now() - t.at < 3600).count();
        if lately >= 3 {
            h.searched_at = now() - LATER;
            h.note = format!("{lately} releases did not do in the last hour. The next is tried in an hour.");
            return Ok(None);
        }
        if !self.is_active() {
            h.note = "Spool is not downloading right now.".into();
            return Ok(None);
        }
        if let Some(why) = s.no_folder {
            h.note = why.into();
            return Ok(None);
        }
        let indexers: Vec<_> = self.db.indexers()?.into_iter().filter(|i| i.enable_search).collect();
        if indexers.is_empty() {
            h.note = "No indexer is set up to search.".into();
            return Ok(None);
        }
        h.searched_at = now();
        let (mut found, mut missed, mut trouble) = (vec![], vec![], vec![]);
        for (n, q) in s.queries.iter().enumerate() {
            for ix in &indexers {
                match self.query_indexer(ix, q, false).await {
                    Ok(rs) => {
                        for r in rs {
                            match (s.grade)(&r, n) {
                                Some(g) => found.push((r, g)),
                                None => missed.push(r.title),
                            }
                        }
                    }
                    Err(e) => trouble.push(e.to_string()),
                }
            }
            if !found.is_empty() {
                break;
            }
        }
        let Some((r, g)) = best(&found, &h.turned_down).cloned() else {
            h.note = if found.is_empty() && !trouble.is_empty() {
                format!("The indexers could not be searched: {}. It is looked for again later.", trouble.join("; "))
            } else if !found.is_empty() {
                "None of the releases found would do. It is looked for again every six hours.".into()
            } else if !missed.is_empty() {
                // What was found and passed over, so it can be seen why.
                let some: Vec<&str> = missed.iter().take(3).map(String::as_str).collect();
                format!("{} found, none of them this {}, such as: {}. It is looked for again every six hours.", missed.len(), s.what, some.join("; "))
            } else {
                format!("{} It is looked for again every six hours.", s.none)
            };
            return Ok(None);
        };
        match self.grab_release(&r).await {
            Ok(job) => {
                tracing::info!(name = %s.name, release = %r.title, "downloading an {}", s.what);
                h.state = HuntState::Downloading;
                h.job_id = Some(job);
                h.release = Some(r);
                h.note.clear();
                Ok(Some(g))
            }
            Err(Grab::Release(why)) => {
                h.turned_down.push(TurnedDown { guid: r.guid.clone(), name: r.title.clone(), why: why.clone(), at: now() });
                h.searched_at = 0;
                h.note = format!("A release would not do: {why}. Trying the next.");
                Ok(None)
            }
            // Nothing wrong with the release: it is tried again in an hour.
            Err(Grab::Later(why)) => {
                h.searched_at = now() - LATER;
                h.note = format!("Found {}, but it could not be started: {why}. It is tried again in an hour.", r.title);
                Ok(None)
            }
        }
    }

    /// See how its download is getting on. A failed one is taken out of the queue here.
    pub(crate) fn hunt_follow(&self, h: &mut Hunt) -> Followed {
        let Some(job_id) = h.job_id.clone() else {
            h.state = HuntState::Wanted;
            return Followed::Going;
        };
        let job = match self.job_lookup(&job_id) {
            crate::remote::Found::Job(j) => j,
            crate::remote::Found::Unknown => return Followed::Going,
            crate::remote::Found::Gone => return Followed::Failed("the download disappeared from the queue".into(), false),
        };
        match job.state {
            JobState::Failed => {
                let _ = self.job_remove(&job_id, true);
                Followed::Failed(job.error.unwrap_or_else(|| "the download failed".into()), true)
            }
            JobState::Completed => match job.output_path {
                Some(out) => Followed::Finished(job_id, out),
                None => Followed::Failed("the download finished without a folder".into(), true),
            },
            _ => Followed::Going,
        }
    }

    /// Clear away a finished download: its job and its folder.
    pub(crate) async fn hunt_clear(&self, job: &str, out: &Path) {
        let _ = self.job_remove(job, true);
        self.remove_download_folder(Some(job), out).await;
    }

    /// Fetch a release's NZB and start it downloading here.
    pub(crate) async fn grab_release(&self, r: &Release) -> std::result::Result<String, Grab> {
        let later = |s: String| Grab::Later(s);
        if self.settings.servers().iter().all(|s| !s.enabled || s.host.is_empty()) {
            return Err(later("no usenet server is configured".into()));
        }
        if self.indexer_budgets().get(&r.indexer_id).is_some_and(|b| b.grabs_spent()) {
            return Err(later(format!("{}'s download allowance for today is used up", r.indexer)));
        }
        let (free, _) = self.download_room(None);
        let reserve = self.settings.general().min_free_gb as u64 * (1 << 30);
        if free.is_some_and(|f| f < r.size * 2 + reserve) {
            return Err(later("not enough free space to download it".into()));
        }
        let client = self.nzb_client(r.indexer_id, &r.link).map_err(|why| later(format!("{} was not asked for the NZB: {why}", r.indexer)))?;
        let _ = self.db.count_indexer(r.indexer_id, 0, 1);
        let resp = client.get(self.nzb_link(r)).timeout(std::time::Duration::from_secs(150)).send().await.map_err(|e| later(format!("could not fetch the NZB from {}: {}", r.indexer, e.without_url())))?;
        match resp.status().as_u16() {
            200..=299 => {}
            // The indexer no longer has it.
            404 | 410 => return Err(Grab::Release(format!("{} no longer has it", r.indexer))),
            code => return Err(later(format!("{} answered HTTP {code} for the NZB", r.indexer))),
        }
        let nzb = crate::app::read_at_most(resp, crate::app::NZB_MOST).await.map_err(|why| Grab::Release(format!("the NZB {} sent was not kept: {why}", r.indexer)))?;
        if let Ok((found, asked)) = self.engine.availability(&nzb, 24).await {
            if asked > 0 && found * 100 < asked * 90 {
                return Err(Grab::Release("most of it has gone from the usenet servers".into()));
            }
        }
        self.engine.add(&nzb, &r.title, None).map_err(|e| Grab::Release(format!("the NZB could not be read: {e}")))
    }
}

/// Files kept beside the audio.
const KEPT: &[&str] = &["jpg", "jpeg", "png", "webp", "cue", "log", "txt", "pdf"];

/// What a download holds that belongs in the folder it is filed in, each with where it goes
/// in it. Audio in folders of its own ("CD1", "Disc 2") keeps those folders.
pub(crate) fn download_files(out: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let (audio, _) = scan::audio_files(out);
    if audio.is_empty() {
        bail!("the download held no audio files");
    }
    // Its own folder: the deepest one all the audio is under, above any disc folders.
    let folders: Vec<PathBuf> = audio.iter().map(|(p, ..)| scan::album_folder(p).0).collect();
    let mut root = folders[0].clone();
    while !folders.iter().all(|f| f.starts_with(&root)) {
        if !root.pop() {
            break;
        }
    }
    if !root.starts_with(out) {
        root = out.to_path_buf();
    }
    let mut files = vec![];
    let mut dirs = vec![root.clone()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d)?.flatten() {
            let p = e.path();
            let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') {
                continue;
            }
            if m.is_dir() {
                dirs.push(p);
            } else if m.is_file() {
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                if audio.iter().any(|(a, ..)| *a == p) || KEPT.contains(&ext.as_str()) {
                    let rel = p.strip_prefix(&root)?.to_path_buf();
                    files.push((p, rel));
                }
            }
        }
    }
    Ok(files)
}

/// Whether `dest` holds every file with the size it has in the download.
pub(crate) fn same_files(files: &[(PathBuf, PathBuf)], dest: &Path) -> bool {
    files.iter().all(|(from, rel)| match (std::fs::metadata(from), std::fs::metadata(dest.join(rel))) {
        (Ok(a), Ok(b)) => a.len() == b.len(),
        _ => false,
    })
}

/// Copy the files into `dest`, by way of a hidden folder beside it so that a copy cut short
/// is never taken for the real thing.
pub(crate) fn copy_files(files: &[(PathBuf, PathBuf)], dest: &Path) -> Result<()> {
    let parent = dest.parent().ok_or_else(|| anyhow!("no folder to file it in"))?;
    let part = parent.join(format!(".spool-{}", dest.file_name().and_then(|n| n.to_str()).unwrap_or("album")));
    if part.exists() {
        std::fs::remove_dir_all(&part)?;
    }
    let done = (|| -> Result<()> {
        for (from, rel) in files {
            let to = part.join(rel);
            if let Some(p) = to.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(from, &to).map_err(|e| anyhow!("copying {}: {e}", rel.display()))?;
        }
        std::fs::rename(&part, dest)?;
        Ok(())
    })();
    if done.is_err() {
        let _ = std::fs::remove_dir_all(&part);
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(title: &str, priority: i32, published: i64) -> Release {
        Release { guid: title.into(), title: title.into(), indexer_priority: priority, published, ..Default::default() }
    }

    #[test]
    fn the_best_release_not_turned_down_is_taken() {
        let found = vec![(rel("a", 0, 1), 1), (rel("b", 0, 1), 2), (rel("c", 5, 1), 1)];
        assert_eq!(best(&found, &[]).unwrap().0.title, "b");
        let down = [TurnedDown { guid: "b".into(), ..Default::default() }];
        assert_eq!(best(&found, &down).unwrap().0.title, "c", "then the indexer put first");
    }

    #[test]
    fn a_failed_release_is_passed_over_and_the_next_looked_for() {
        let mut h = Hunt { state: HuntState::Downloading, job_id: Some("j".into()), release: Some(rel("r", 0, 0)), searched_at: 9, ..Default::default() };
        h.failed("it was short", true);
        assert_eq!((h.state, h.job_id.as_deref(), h.searched_at), (HuntState::Wanted, None, 0));
        assert_eq!(h.turned_down.len(), 1);
        assert!(h.release.is_none());
        let mut h = Hunt { state: HuntState::Downloading, release: Some(rel("r", 0, 0)), ..Default::default() };
        h.failed("it went", false);
        assert!(h.turned_down.is_empty(), "the release was not at fault");
    }

    #[test]
    fn its_fields_are_saved_beside_the_things_own() {
        #[derive(Serialize, Deserialize, Default)]
        #[serde(default)]
        struct Thing {
            title: String,
            #[serde(flatten)]
            hunt: Hunt,
        }
        let old = r#"{"title":"T","state":"downloading","note":"n","job_id":"j","searched_at":5,"turned_down":[{"guid":"g","name":"x","why":"y","at":1}]}"#;
        let t: Thing = serde_json::from_str(old).unwrap();
        assert_eq!((t.hunt.state, t.hunt.job_id.as_deref(), t.hunt.searched_at, t.hunt.turned_down.len()), (HuntState::Downloading, Some("j"), 5, 1));
        let v: serde_json::Value = serde_json::to_value(&t).unwrap();
        assert_eq!(v["state"], "downloading");
        assert_eq!(v["title"], "T");
        let empty: Thing = serde_json::from_str(r#"{"title":"T"}"#).unwrap();
        assert_eq!(empty.hunt.state, HuntState::Wanted);
    }
}
