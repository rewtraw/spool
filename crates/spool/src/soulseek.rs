//! Soulseek: albums from other people's shared folders when the indexers have none, and the
//! music folders shared back. The client itself is the `spool-slsk` crate; this starts it
//! with the account in the settings, tells it what to share, and takes its downloads
//! through the steps every wanted thing takes (see [`crate::hunt`]).

use crate::app::App;
use crate::db::now;
use crate::hunt::{Followed, Hunt, HuntState};
use crate::models::Release;
use serde::{Deserialize, Serialize};
use spool_slsk::{Client, Config, Fetch, Folder, Shared, Shares};
use std::path::{Path, PathBuf};

/// The account, and where others reach Spool.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SoulseekSettings {
    /// Chosen by the person. The server makes the account the first time a name is used.
    pub username: String,
    pub password: String,
    /// Forwarded on the router, others can connect to Spool; without it, only those whose
    /// own port is open can be reached.
    pub port: u16,
}

impl Default for SoulseekSettings {
    fn default() -> Self {
        SoulseekSettings { username: String::new(), password: String::new(), port: 2234 }
    }
}

const SERVER: &str = "server.slsknet.org:2242";
/// Uploads sent at once.
const SLOTS: usize = 2;
/// How long peers are given to answer a search.
const LISTEN: std::time::Duration = std::time::Duration::from_secs(15);
/// A download that has had nothing come for this long is given up, and the next folder tried.
const STALLED: i64 = 3600;
/// Someone who has not said where a download is in their queue in this long has not taken it.
const UNANSWERED: i64 = 15 * 60;
/// Further back in someone's queue than this, the next folder is tried instead.
const TOO_FAR_BACK: u32 = 20;
/// Someone with a longer queue than this when found is passed over, as Soularr does.
const LONGEST_QUEUE: u32 = 50;
/// How many albums are taken from one person at once.
const PER_PERSON: usize = 2;
/// How many of the folders that would do are kept to try in turn.
const KEPT_FOLDERS: usize = 10;
/// An album is looked for on Soulseek once a day, unless someone asks.
pub(crate) const AGAIN: i64 = 24 * 3600;
/// What is fetched of a folder besides its audio.
const KEPT: &[&str] = &["jpg", "jpeg", "png", "cue", "log"];

/// Something to look for on Soulseek, and how to judge each folder found.
pub struct Look<'a, G> {
    pub query: String,
    pub grade: &'a (dyn Fn(&Folder) -> Option<G> + Sync),
}

/// What looking on Soulseek came to.
pub(crate) enum Looked<G> {
    Started(G),
    /// Nothing would do, in words.
    Nothing(String),
    /// Another album has had this turn to search; it waits for the next.
    NotYet,
}

/// What came of taking one of the folders kept.
enum Took<G> {
    Started(G),
    /// None of them can be taken now, and why.
    Not(String),
}

/// Whether a folder is one Spool made to receive a download from Soulseek, wherever the
/// downloads folder was at the time: `…/soulseek/<when>-<whose and what>`. Nothing else is
/// ever cleared away.
fn received_here(dest: &Path) -> bool {
    let under = dest.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "soulseek");
    let named = dest.file_name().and_then(|n| n.to_str()).and_then(|n| n.split_once('-')).is_some_and(|(when, what)| when.len() >= 9 && when.chars().all(|c| c.is_ascii_digit()) && !what.trim().is_empty());
    under && named
}

fn job_number(job: &str) -> Option<u64> {
    job.strip_prefix("slsk:").and_then(|n| n.parse().ok())
}

pub fn is_job(job: &str) -> bool {
    job.starts_with("slsk:")
}

fn guid(f: &Folder) -> String {
    format!("slsk:{}\\{}", f.user, f.path)
}

/// Lossless by its kind of file.
fn lossless(suffix: &str, bit_depth: u8) -> bool {
    matches!(suffix, "flac" | "wav" | "aiff" | "aif" | "wv" | "ape") || (suffix == "m4a" && bit_depth > 0)
}

impl App {
    pub fn soulseek(&self) -> Option<Client> {
        self.soulseek.lock().as_ref().map(|(_, c)| c.clone())
    }

    /// Start, restart or stop the client to match the settings.
    pub fn sync_soulseek(&self) {
        let want = self.settings.general().soulseek;
        let mut slot = self.soulseek.lock();
        if let Some((had, c)) = slot.as_ref() {
            // Signed out because the account signed in elsewhere: asked again, it signs in anew.
            if *had == want && !c.relogged() {
                return;
            }
            c.stop();
        }
        *slot = None;
        if want.username.trim().is_empty() || want.password.is_empty() {
            return;
        }
        let c = Client::start(Config { username: want.username.trim().to_string(), password: want.password.clone(), server: SERVER.into(), port: want.port, state: self.data_dir.join("soulseek.json"), upload_slots: SLOTS });
        *slot = Some((want, c));
        drop(slot);
        self.soulseek_share();
    }

    /// Share the music folders, as the library has them, and nothing outside them.
    pub fn soulseek_share(&self) {
        let Some(c) = self.soulseek() else { return };
        let roots: Vec<String> = self.music_roots().into_iter().map(|(r, _)| r).filter(|r| !r.is_empty()).collect();
        let lib = self.music.library();
        let mut files = vec![];
        for t in &lib.tracks {
            let Some((n, root)) = roots.iter().enumerate().find(|(_, r)| Path::new(&t.path).starts_with(r)) else { continue };
            let Ok(rel) = Path::new(&t.path).strip_prefix(root) else { continue };
            let parts: Vec<&str> = rel.iter().filter_map(|p| p.to_str()).collect();
            if parts.is_empty() || parts.iter().any(|p| p.starts_with('.')) {
                continue;
            }
            let top = if n == 0 { "Music".to_string() } else { format!("Music {}", n + 1) };
            files.push(Shared {
                name: format!("{top}\\{}", parts.join("\\")),
                real: PathBuf::from(&t.path),
                size: t.size,
                seconds: t.seconds.round() as u32,
                bitrate: t.bitrate,
                sample_rate: t.sample_rate,
                bit_depth: t.bit_depth as u32,
                lossless: lossless(&t.suffix.to_ascii_lowercase(), t.bit_depth),
            });
        }
        c.share(Shares::new(files));
    }

    /// Where downloads from Soulseek are received.
    fn soulseek_dir(&self) -> PathBuf {
        let g = self.settings.general();
        let downloads = if g.downloads_dir.is_empty() { self.data_dir.join("downloads") } else { PathBuf::from(&g.downloads_dir) };
        downloads.join("soulseek")
    }

    /// Whether Soulseek can be searched now.
    pub(crate) fn soulseek_ready(&self) -> bool {
        self.soulseek().is_some_and(|c| c.online())
    }

    /// Look on Soulseek, and start the best folder found downloading: the best grade, then
    /// someone who would send it at once, a short queue, and speed. The folders found last
    /// time are tried in turn before searching again. A search made by itself is a wishlist
    /// search, in turn with every other album's; one someone asked for is made at once.
    pub(crate) async fn soulseek_hunt<G: Ord + Copy>(&self, h: &mut Hunt, look: &Look<'_, G>, what: &str, name: &str, asked: bool) -> Looked<G> {
        let Some(c) = self.soulseek().filter(|c| c.online()) else { return Looked::Nothing("Soulseek is not signed in.".into()) };
        if !asked && !h.soulseek_left.is_empty() {
            match self.soulseek_take(&c, h, look, what, name) {
                Took::Started(g) => return Looked::Started(g),
                Took::Not(why) if now() - h.soulseek_at < AGAIN => return Looked::Nothing(why),
                Took::Not(_) => h.soulseek_left.clear(),
            }
        }
        let replies = if asked {
            c.search(&look.query, LISTEN).await
        } else {
            match c.wishlist_search(&look.query, LISTEN).await {
                Ok(Some(r)) => Ok(r),
                Ok(None) => return Looked::NotYet,
                Err(e) => Err(e),
            }
        };
        let replies = match replies {
            Ok(r) => r,
            Err(e) => return Looked::Nothing(format!("Soulseek could not be searched: {e}.")),
        };
        h.soulseek_at = now();
        let folders = spool_slsk::folders(&replies);
        // Those that would do, best first, kept to try in turn.
        let mut good: Vec<(G, Folder)> = folders.iter().filter_map(|f| (look.grade)(f).map(|g| (g, f.clone()))).collect();
        good.sort_by_key(|(g, f)| std::cmp::Reverse((*g, f.slot_free, std::cmp::Reverse(f.queue), f.speed)));
        h.soulseek_left = good.into_iter().take(KEPT_FOLDERS).map(|(_, f)| f).collect();
        if !h.soulseek_left.is_empty() {
            match self.soulseek_take(&c, h, look, what, name) {
                Took::Started(g) => return Looked::Started(g),
                Took::Not(why) => return Looked::Nothing(why),
            }
        }
        // Some of what was passed over, so it can be seen why.
        let some: Vec<String> = folders.iter().take(3).map(|f| format!("{} ({} files)", f.name(2).replace('\\', " / "), f.files.len())).collect();
        Looked::Nothing(match folders.len() {
            0 => "Nothing on Soulseek. Looked for there again tomorrow.".into(),
            1 => format!("One folder on Soulseek, and it would not do: {}. Looked for there again tomorrow.", some.join("; ")),
            n => format!("{n} folders on Soulseek, none of which would do, such as: {}. Looked for there again tomorrow.", some.join("; ")),
        })
    }

    /// Start the best of the folders kept that can be taken now: not turned down, from
    /// someone whose queue was not long, and who is not already sending two albums.
    fn soulseek_take<G: Ord + Copy>(&self, c: &Client, h: &mut Hunt, look: &Look<'_, G>, what: &str, name: &str) -> Took<G> {
        let jobs = c.jobs();
        let busy = |user: &str| jobs.iter().filter(|j| j.user == user && !j.done() && j.failed().is_none()).count();
        let turned_down = |f: &Folder| h.turned_down.iter().any(|t| t.guid == guid(f));
        let pick = h.soulseek_left.iter().enumerate().filter(|(_, f)| !turned_down(f) && f.queue <= LONGEST_QUEUE && busy(&f.user) < PER_PERSON).filter_map(|(n, f)| (look.grade)(f).map(|g| (n, g, f))).max_by_key(|(_, g, f)| (*g, f.slot_free, std::cmp::Reverse(f.queue), f.speed)).map(|(n, g, _)| (n, g));
        let Some((n, g)) = pick else {
            let left = h.soulseek_left.iter().filter(|f| !turned_down(f)).count();
            return Took::Not(if left == 0 {
                "None of the folders found on Soulseek would do. Looked for there again tomorrow.".into()
            } else {
                format!("{left} folders on Soulseek wait for those who have them to be less busy.")
            });
        };
        let f = h.soulseek_left[n].clone();
        let fetch: Vec<Fetch> = f
            .files
            .iter()
            .filter(|(e, _)| crate::music::scan::is_audio(Path::new(&e.path.replace('\\', "/"))) || KEPT.contains(&e.extension().as_str()))
            .map(|(e, rel)| Fetch { name: e.path.clone(), size: e.size, rel: rel.clone() })
            .collect();
        let size: u64 = fetch.iter().map(|f| f.size).sum();
        let reserve = self.settings.general().min_free_gb as u64 * (1 << 30);
        let dir = self.soulseek_dir();
        let _ = std::fs::create_dir_all(&dir);
        if crate::app::free_space(&dir).is_some_and(|free| free < size * 2 + reserve) {
            return Took::Not(format!("Found on Soulseek from {}, but there is not enough free space to download it.", f.user));
        }
        h.soulseek_left.remove(n);
        let dest = dir.join(format!("{}-{}", now(), crate::music::want::folder_safe(&format!("{} - {}", f.user, f.name(1)))));
        let id = c.download(&f.user, &dest, fetch);
        let release = Release { guid: guid(&f), title: f.name(2).replace('\\', " / "), indexer: format!("Soulseek, from {}", f.user), size, published: now(), ..Default::default() };
        tracing::info!(name, from = %f.user, folder = %f.path, "downloading an {what} from Soulseek");
        h.state = HuntState::Downloading;
        h.job_id = Some(format!("slsk:{id}"));
        h.release = Some(release);
        h.note.clear();
        h.soulseek_note.clear();
        Took::Started(g)
    }

    /// How a download from Soulseek is getting on.
    pub(crate) fn soulseek_follow(&self, job: &str) -> Followed {
        let Some(n) = job_number(job) else { return Followed::Failed("the download is not one Spool knows".into(), false) };
        let Some(c) = self.soulseek() else {
            self.soulseek_drop(job);
            return Followed::Failed("Soulseek was turned off".into(), false);
        };
        let Some(j) = c.job(n) else { return Followed::Failed("the download from Soulseek disappeared".into(), false) };
        if let Some(why) = j.failed() {
            self.soulseek_drop(job);
            return Followed::Failed(format!("{} would not send it ({why})", j.user), true);
        }
        if j.done() {
            return Followed::Finished(job.to_string(), j.dest);
        }
        // While signed out nothing can come, and that is not their doing: the client counts
        // that time as if something had.
        if !c.online() {
            return Followed::Going;
        }
        // Not begun: given up soon when they do not answer or have it far back in their
        // queue, as Soularr does, so the next folder is tried while this one would wait.
        let why = if !j.started() {
            match j.place() {
                Some(p) if p > TOO_FAR_BACK => Some(format!("{} had it number {p} in their queue", j.user)),
                None if now() - j.progress_at > UNANSWERED => Some(format!("{} did not say where it was in their queue in fifteen minutes", j.user)),
                _ if now() - j.progress_at > STALLED => Some(format!("{} did not begin sending it in an hour", j.user)),
                _ => None,
            }
        } else if now() - j.progress_at > STALLED {
            Some(format!("nothing came from {} in an hour", j.user))
        } else {
            None
        };
        if let Some(why) = why {
            self.soulseek_drop(job);
            return Followed::Failed(why, true);
        }
        Followed::Going
    }

    /// Stop a download from Soulseek, and clear away what it received; with Soulseek turned
    /// off, from the downloads it keeps for next time.
    pub(crate) fn soulseek_drop(&self, job: &str) {
        let Some(n) = job_number(job) else { return };
        let gone = match self.soulseek() {
            Some(c) => {
                let j = c.job(n);
                c.remove(n);
                j
            }
            None => spool_slsk::forget(&self.data_dir.join("soulseek.json"), n),
        };
        if let Some(j) = gone.filter(|j| received_here(&j.dest)) {
            let _ = std::fs::remove_dir_all(&j.dest);
        }
    }

    /// How a download from Soulseek stands, for showing.
    pub fn soulseek_progress(&self, job: &str) -> Option<serde_json::Value> {
        let j = self.soulseek()?.job(job_number(job)?)?;
        let done = j.files.iter().filter(|f| f.state == spool_slsk::FileState::Done).count();
        let place = j.files.iter().filter_map(|f| f.place).min();
        Some(serde_json::json!({"user": j.user, "got": j.got(), "size": j.size(), "files": j.files.len(), "done": done, "place": place, "note": j.note, "progress_at": j.progress_at}))
    }

    pub fn soulseek_status(&self) -> serde_json::Value {
        let g = self.settings.general().soulseek;
        match self.soulseek() {
            None => serde_json::json!({"on": false, "set": !g.username.is_empty()}),
            Some(c) => serde_json::json!({"on": true, "set": true, "status": c.status(), "downloads": c.jobs().len()}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_folders_made_for_downloads_are_cleared_away() {
        assert!(received_here(Path::new("/Volumes/Tardis/Downloads/soulseek/1794843697-someone - Tago Mago")));
        assert!(!received_here(Path::new("/Volumes/Tardis/Downloads/soulseek")));
        assert!(!received_here(Path::new("/Volumes/Tardis/Music/Can - Tago Mago")));
        assert!(!received_here(Path::new("/Volumes/Tardis/Downloads/soulseek/Can - Tago Mago")));
        assert!(!received_here(Path::new("/soulseek/1794843697-")));
    }
}
