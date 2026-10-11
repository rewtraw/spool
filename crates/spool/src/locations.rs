//! The places the library lives.
//!
//! There is always a main one: the two library folders in the general settings, on the volume
//! named there. Further locations can be added beside it, each with its own folders, and each
//! either managed (Spool may put files there, replace and delete them) or only read (Spool
//! lists what is there and plays it, and changes nothing).
//!
//! A title belongs to whichever location's folders hold its own folder. A location that cannot
//! be reached takes its titles offline: they stay in the library, are not searched for, and
//! nothing about them is concluded from files that cannot be seen.

use crate::app::{is_mount_point, App, Event};
use crate::models::{Kind, Title};
use crate::settings::Location;
use std::path::Path;

impl Location {
    pub fn root(&self, kind: Kind) -> &str {
        match kind {
            Kind::Movie => &self.movie_root,
            Kind::Series => &self.series_root,
        }
    }

    /// How deep inside this location a folder is: the length of the library folder holding it.
    fn holds(&self, path: &str) -> Option<usize> {
        [&self.movie_root, &self.series_root].into_iter().filter(|r| !r.is_empty() && Path::new(path).starts_with(r)).map(|r| r.len()).max()
    }
}

/// Is the location there to be read? A network share that has gone away can keep a caller
/// waiting, so this only ever runs on a thread of its own.
fn probe(l: &Location) -> bool {
    let readable = |dir: &str| std::fs::read_dir(dir).and_then(|mut d| d.next().transpose()).is_ok();
    if !l.volume.is_empty() {
        return is_mount_point(Path::new(&l.volume)) && readable(&l.volume);
    }
    let roots: Vec<&String> = [&l.movie_root, &l.series_root].into_iter().filter(|r| !r.is_empty()).collect();
    !roots.is_empty() && roots.into_iter().all(|r| readable(r))
}

/// Every location with whether it can be reached, as of one moment.
pub struct Placement(pub Vec<(Location, bool)>);

impl Placement {
    pub fn of(&self, path: &str) -> Option<&(Location, bool)> {
        if path.is_empty() {
            return None;
        }
        self.0.iter().filter_map(|p| p.0.holds(path).map(|depth| (depth, p))).max_by_key(|(depth, _)| *depth).map(|(_, p)| p)
    }

    fn main_online(&self) -> bool {
        self.0.first().is_some_and(|p| p.1)
    }

    /// Whether this title's files can be read right now. A title somewhere no location covers
    /// is taken to be on the main volume, as every title was before there were locations.
    pub fn online(&self, t: &Title) -> bool {
        self.online_at(&t.path)
    }

    /// Whether the location a folder is on can be reached.
    pub fn online_at(&self, path: &str) -> bool {
        self.of(path).map(|p| p.1).unwrap_or_else(|| self.main_online())
    }

    /// Whether any of a title's files can be reached: in its own folder or a copy's.
    pub fn playable(&self, t: &Title) -> bool {
        self.online(t) || t.also.iter().any(|f| self.online_at(f))
    }

    /// Whether Spool leaves this title's files as they are.
    pub fn read_only(&self, t: &Title) -> bool {
        self.of(&t.path).is_some_and(|p| !p.0.manage)
    }

    /// Why Spool may not change this title's files now, in words for the user; None when it may.
    pub fn locked(&self, t: &Title) -> Option<String> {
        match self.of(&t.path) {
            Some((l, _)) if !l.manage => Some(format!("{} is on {}, which Spool only reads", t.title, l.name)),
            Some((l, false)) if l.id != 0 => Some(format!("{} is on {}, which cannot be reached right now", t.title, l.name)),
            Some((_, true)) => None,
            _ if self.main_online() => None,
            _ => Some("the media volume is not mounted".into()),
        }
    }

    /// Where a title with no folder yet will be put.
    pub fn for_new(&self, kind: Kind, wanted: u32) -> Option<&Location> {
        let fits = |p: &&(Location, bool)| p.0.manage && p.1 && !p.0.root(kind).is_empty();
        self.0.iter().find(|p| p.0.id == wanted && wanted != 0 && fits(p)).or_else(|| self.0.first()).map(|p| &p.0)
    }
}

impl App {
    /// Every location, the main one first.
    pub fn locations(&self) -> Vec<Location> {
        let g = self.settings.general();
        let name = Path::new(&g.required_volume).file_name().map(|n| n.to_string_lossy().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| "Library".into());
        let mut all = vec![Location { id: 0, name, movie_root: g.movie_root, series_root: g.series_root, volume: g.required_volume, manage: true, movie_profile: 0, series_profile: 0 }];
        all.extend(g.locations.into_iter().filter(|l| l.id != 0));
        all
    }

    pub fn placement(&self) -> Placement {
        Placement(self.locations().into_iter().map(|l| { let on = self.location_online(&l); (l, on) }).collect())
    }

    /// Whether a location can be reached, as last found. The first time it is asked about, it
    /// is looked at there and then.
    pub fn location_online(&self, l: &Location) -> bool {
        if l.id == 0 {
            return self.volume_ok();
        }
        let known = self.location_state.lock().get(&l.id).copied();
        known.unwrap_or_else(|| self.check_location(l))
    }

    /// Look at a location now, waiting a few seconds at most for an answer. One that is slow to
    /// say counts as away until it does.
    pub fn check_location(&self, l: &Location) -> bool {
        if !self.location_checking.lock().insert(l.id) {
            // Still waiting on the last look.
            return self.location_state.lock().get(&l.id).copied().unwrap_or(false);
        }
        let (app, loc) = (self.clone(), l.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let ok = probe(&loc);
            app.location_checking.lock().remove(&loc.id);
            app.note_location(&loc, ok);
            let _ = tx.send(ok);
        });
        match rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(ok) => ok,
            Err(_) => {
                self.note_location(l, false);
                false
            }
        }
    }

    fn note_location(&self, l: &Location, ok: bool) {
        let before = self.location_state.lock().insert(l.id, ok);
        if before == Some(ok) {
            return;
        }
        match (before, ok) {
            (_, true) => tracing::info!(location = %l.name, "the location can be reached"),
            (_, false) => tracing::warn!(location = %l.name, "the location cannot be reached; its titles are offline"),
        }
        self.location_changes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if before.is_some() {
            self.emit(Event::Locations);
            if ok {
                // Imports that were waiting for it can go ahead.
                if let Err(e) = self.recover() {
                    tracing::warn!(error = %e, "could not finish interrupted imports");
                }
            }
        }
    }

    /// Look at every further location again, and forget those that are no longer set up.
    pub fn watch_locations(&self) {
        let all = self.locations();
        self.location_state.lock().retain(|id, _| all.iter().any(|l| l.id == *id));
        for l in all.iter().filter(|l| l.id != 0) {
            self.check_location(l);
        }
    }

    pub fn title_online(&self, t: &Title) -> bool {
        self.placement().online(t)
    }

    /// Why Spool may not change this title's files now; None when it may.
    pub fn title_locked(&self, t: &Title) -> Option<String> {
        if self.moving(t.id) {
            return Some(format!("{} is being moved to another location", t.title));
        }
        self.placement().locked(t)
    }

    /// Check a list of further locations as someone has edited it, and give new ones their ids.
    pub fn tidy_locations(&self, list: Vec<Location>) -> Result<Vec<Location>, String> {
        let g = self.settings.general();
        let mut next: u32 = self.db.get_setting::<u32>("location_seq").max(g.locations.iter().map(|l| l.id).max().unwrap_or(0));
        let clean = |s: &str| {
            let s = s.trim();
            if s.len() > 1 { s.trim_end_matches('/').to_string() } else { s.to_string() }
        };
        let mut out: Vec<Location> = vec![];
        for mut l in list {
            l.name = l.name.trim().to_string();
            (l.movie_root, l.series_root, l.volume) = (clean(&l.movie_root), clean(&l.series_root), clean(&l.volume));
            if l.name.is_empty() {
                return Err("every location needs a name".into());
            }
            if l.movie_root.is_empty() && l.series_root.is_empty() {
                return Err(format!("{} needs a folder for films or for television", l.name));
            }
            if [&l.movie_root, &l.series_root, &l.volume].into_iter().any(|p| !p.is_empty() && !p.starts_with('/')) {
                return Err(format!("the folders of {} must be full paths, starting with /", l.name));
            }
            if l.id == 0 || out.iter().any(|o| o.id == l.id) {
                next += 1;
                l.id = next;
            }
            out.push(l);
        }
        // A folder inside another location's folder would leave its titles belonging to both.
        let main = Location { id: 0, name: "the main library".into(), movie_root: g.movie_root, series_root: g.series_root, ..Default::default() };
        let all: Vec<&Location> = std::iter::once(&main).chain(out.iter()).collect();
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                let roots = |l: &Location| [l.movie_root.clone(), l.series_root.clone()].into_iter().filter(|r| !r.is_empty()).collect::<Vec<_>>();
                if roots(a).iter().any(|x| roots(b).iter().any(|y| Path::new(x).starts_with(y) || Path::new(y).starts_with(x))) {
                    return Err(format!("{} and {} share a folder; each location needs folders of its own", a.name, b.name));
                }
            }
        }
        self.db.set_setting("location_seq", &next).map_err(|e| e.to_string())?;
        Ok(out)
    }

    /// Each location as Settings and the disk page show it.
    pub fn location_report(&self) -> Vec<serde_json::Value> {
        let g = self.settings.general();
        let titles = self.db.titles(None).unwrap_or_default();
        let sizes = self.db.file_counts().unwrap_or_default();
        let places = self.placement();
        places
            .0
            .iter()
            .map(|(l, online)| {
                let mine: Vec<&Title> = titles.iter().filter(|t| places.of(&t.path).is_some_and(|p| p.0.id == l.id)).collect();
                // Asking a share that has gone away how full it is can wait a long time.
                let free = online.then(|| [&l.movie_root, &l.series_root].into_iter().find(|r| !r.is_empty()).and_then(|r| crate::app::free_space(Path::new(r)))).flatten();
                serde_json::json!({
                    "id": l.id, "name": l.name, "movie_root": l.movie_root, "series_root": l.series_root, "volume": l.volume, "manage": l.manage,
                    "online": online, "free": free, "titles": mine.len(), "bytes": mine.iter().map(|t| sizes.get(&t.id).map(|s| s.1).unwrap_or(0)).sum::<u64>(),
                    "new_titles": places.for_new(Kind::Movie, g.new_titles_location).is_some_and(|n| n.id == l.id),
                    "movie_profile": l.movie_profile, "series_profile": l.series_profile,
                })
            })
            .collect()
    }

    /// What is wrong with the further locations right now, one line each.
    pub fn location_problems(&self) -> Vec<String> {
        let titles = self.db.titles(None).unwrap_or_default();
        self.placement()
            .0
            .iter()
            .filter(|(l, on)| l.id != 0 && !on)
            .map(|(l, _)| {
                let n = titles.iter().filter(|t| l.holds(&t.path).is_some()).count();
                format!("{} cannot be reached. Its {n} title{} are offline until it returns.", l.name, if n == 1 { "" } else { "s" })
            })
            .collect()
    }
}
