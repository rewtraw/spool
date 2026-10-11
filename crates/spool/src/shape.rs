//! Whether a download is the shape its name says.
//!
//! A release's name says what it is, and the name is sometimes wrong, or right about a work
//! that is not shaped the way the library has it: a film posted in two parts, an OVA the
//! film source calls one film, two episodes in one file, a season run together. The one
//! thing a name cannot fake is how long the video runs, so each video is measured before it
//! is filed and compared with what it is supposed to be. Where the two disagree the import
//! either reads the video for what it is or stops to ask; "Import anyway" is the answer, and
//! is not asked for twice.

use crate::app::App;
use crate::import::Blocked;
use crate::models::*;
use std::collections::HashSet;
use std::path::Path;

/// One video of a download, with how long it runs in seconds; 0 when that could not be read.
pub(crate) struct Video<'a> {
    pub path: &'a Path,
    pub name: &'a str,
    pub ran: f64,
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

impl App {
    /// A film's video that is one part of it, arrived on its own: named as a part and well
    /// short of the film. Gives the part's number when it is to be filed as that part: with
    /// the rest already here, or on its way from the same release, or after a yes. "Part 1"
    /// in a film's own title is not a marker.
    pub(crate) fn lone_part(&self, acq: &Acquisition, title: &Title, files: &[MediaFile], v: &Video, forced: bool) -> Result<Option<u32>, Blocked> {
        let film = title.runtime as f64 * 60.0;
        if v.ran <= 0.0 || film <= 0.0 {
            return Ok(None);
        }
        let own_title = title.title.to_lowercase().contains("part");
        let named = crate::local::part_number(&stem(v.path)).filter(|_| !own_title).or_else(|| crate::local::part_of_release(&acq.release.title).filter(|p| !(p.1 && own_title)).map(|p| p.0));
        let Some(n) = named.filter(|_| v.ran < film * 0.75) else { return Ok(None) };
        let key = crate::local::part_of_release(&acq.release.title).map(|p| p.2);
        let others = self.db.title_acquisitions(title.id).unwrap_or_default();
        let with_set = key.is_some() && others.iter().any(|o| o.id != acq.id && !matches!(o.state, AcqState::Failed | AcqState::Cancelled) && crate::local::part_of_release(&o.release.title).map(|p| p.2) == key);
        let mains: Vec<&MediaFile> = files.iter().filter(|f| !f.extra).collect();
        let beside_parts = !mains.is_empty() && mains.iter().all(|f| crate::local::is_part(&stem(Path::new(&f.rel_path))));
        if !(forced || with_set || beside_parts) {
            return Err(Blocked::Needs(format!(
                "{} is part {n} of the film: it runs {:.0} of {} minutes, and the other parts were not posted with it. Import anyway keeps it as part {n}, and the rest can be added beside it; or discard it and find the film whole.",
                v.name,
                v.ran / 60.0,
                title.runtime
            )));
        }
        if !mains.is_empty() && !beside_parts {
            return Err(Blocked::Needs(format!("{} is only part {n} of the film, and the library already has the film whole", v.name)));
        }
        Ok(Some(n))
    }

    /// A film's video that runs far too short or far too long to be the film.
    pub(crate) fn film_length(&self, acq: &Acquisition, title: &Title, v: &Video) -> Result<(), Blocked> {
        let film = title.runtime as f64 * 60.0;
        if v.ran <= 0.0 || film <= 0.0 {
            return Ok(());
        }
        if v.ran < film * 0.4 {
            // Short, and named as an episode: the film is posted in episodes. Remember it, so
            // the rest are turned down by name instead of downloaded to find out.
            if crate::acquire::names_an_episode(&acq.release.title, title) || crate::acquire::names_an_episode(v.name, title) {
                if let Some(mut t) = self.db.title(title.id).ok().flatten().filter(|t| !t.in_episodes) {
                    t.in_episodes = true;
                    let _ = self.db.save_title(&mut t);
                }
                return Err(Blocked::Needs(format!(
                    "{} is one episode ({:.0} minutes) of a film that runs {} minutes. {} is posted in episodes, and Spool files a film as one video. Other single episodes will now be turned down. To collect the episodes, remove this film and add it as a series.",
                    v.name,
                    v.ran / 60.0,
                    title.runtime,
                    title.title
                )));
            }
            return Err(Blocked::Needs(format!("{} runs {:.0} minutes but the movie is {} minutes; it may be a sample", v.name, v.ran / 60.0, title.runtime)));
        }
        // More than twice as long, and by an hour or more: another film, or several in one.
        if v.ran > film * 2.2 && v.ran - film >= 3600.0 {
            return Err(Blocked::Needs(format!("{} runs {:.0} minutes but the movie is {} minutes; it may be a different film, or several films in one file", v.name, v.ran / 60.0, title.runtime)));
        }
        Ok(())
    }
}

/// The episodes a series' video holds, given the ones its name gave it. One file named as one
/// episode that runs as long as it and the next ones together holds them all; one file named
/// for many that is too short to hold them, or is a whole season run together, waits for a
/// yes. `claimed` are episodes other videos of the same download are.
pub(crate) fn episodes_held(title: &Title, episodes: &[Episode], eps: Vec<Episode>, claimed: &HashSet<i64>, v: &Video) -> Result<Vec<Episode>, Blocked> {
    if v.ran <= 0.0 {
        return Ok(eps);
    }
    // How long an episode runs, in seconds; 0 when the source does not say.
    let length = |e: &Episode| (if e.runtime > 0 { e.runtime } else { title.runtime }) as f64 * 60.0;
    if let [first] = eps.as_slice() {
        // Only when the source gives each one's length, and the sum fits: a long episode is
        // otherwise just a long episode.
        let own = length(first);
        if own > 0.0 && v.ran > own * 1.6 {
            let mut run = vec![first.clone()];
            let mut sum = own;
            while run.len() < 4 && sum < v.ran * 0.85 {
                let last = run.last().unwrap();
                let Some(next) = episodes.iter().find(|e| e.season == last.season && e.episode == last.episode + 1).filter(|e| length(e) > 0.0 && !claimed.contains(&e.id)) else { break };
                sum += length(next);
                run.push(next.clone());
            }
            if run.len() > 1 && (sum - v.ran).abs() <= v.ran * 0.15 {
                tracing::info!(file = %v.name, episodes = run.len(), "one file holds several episodes");
                return Ok(run);
            }
        }
    } else if eps.len() >= 3 && eps.iter().all(|e| length(e) > 0.0) {
        let sum: f64 = eps.iter().map(&length).sum();
        let season = eps[0].season;
        let whole = eps.len() >= 4 && eps.iter().all(|e| e.season == season) && episodes.iter().filter(|e| e.season == season).all(|e| eps.iter().any(|x| x.id == e.id));
        if v.ran < sum * 0.6 {
            return Err(Blocked::Needs(format!(
                "{} runs {:.0} minutes but is named as {} episodes, which run {:.0} minutes together; it is probably not all of them. Import anyway counts all {} as present.",
                v.name,
                v.ran / 60.0,
                eps.len(),
                sum / 60.0,
                eps.len()
            )));
        }
        if whole {
            return Err(Blocked::Needs(format!(
                "{} is the whole of season {season} in one file ({:.0} minutes). Spool keeps one file per episode. Import anyway counts all {} episodes as present in this one file; or discard it and find the episodes.",
                v.name,
                v.ran / 60.0,
                eps.len()
            )));
        }
    }
    Ok(eps)
}
