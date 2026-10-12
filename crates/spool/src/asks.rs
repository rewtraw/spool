//! What the people who play ask for: a film or a series that is not in the library yet.
//!
//! Someone signed in with their account names a title. It waits until whoever runs Spool
//! adds it or turns it down, unless that account's asks are added at once. Whoever asked can
//! see how it stands: asked, on its way, ready to play, or not this time.

use crate::app::{App, Event};
use crate::db::now;
use crate::models::*;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;

/// How many asks one account may have waiting at a time.
const WAITING: usize = 30;
/// How many lookups one account may have made for it in a minute.
const LOOKUPS: u32 = 30;

/// The ids a title is asked for by: TMDB's for a film; TVmaze's and TVDB's for a series.
type Ids = (Option<u32>, Option<u32>, Option<u32>);

fn ids(a: &Ask) -> Ids {
    (a.tmdb_id, a.tvmaze_id, a.tvdb_id)
}

/// The library as an ask is measured against it.
pub struct Shelf {
    titles: Vec<Title>,
    files: HashMap<i64, (u32, u64)>,
    episodes: HashMap<i64, (u32, u32)>,
}

impl Shelf {
    /// The title something asked for is, when the library has it: the one it became, or
    /// one of the same ids added some other way.
    fn title(&self, kind: Kind, ids: Ids, became: Option<i64>) -> Option<&Title> {
        let (tmdb, tvmaze, tvdb) = ids;
        became.and_then(|id| self.titles.iter().find(|t| t.id == id && t.kind == kind)).or_else(|| {
            self.titles.iter().find(|t| {
                t.kind == kind
                    && match kind {
                        Kind::Movie => tmdb.is_some() && t.tmdb_id == tmdb,
                        // A series that came from elsewhere is known by one id and not the other.
                        Kind::Series => tvmaze.is_some() && t.tvmaze_id == tvmaze || tvdb.is_some() && t.tvdb_id == tvdb,
                    }
            })
        })
    }

    /// How a title stands: "ready" with something to play, "coming" with nothing yet, and
    /// for a series how many of its episodes are here.
    fn standing(&self, t: &Title) -> (&'static str, u32, u32) {
        match t.kind {
            Kind::Movie => (if self.files.get(&t.id).is_some_and(|f| f.0 > 0) { "ready" } else { "coming" }, 0, 0),
            Kind::Series => {
                let (of, have) = self.episodes.get(&t.id).copied().unwrap_or((0, 0));
                (if have > 0 { "ready" } else { "coming" }, have, of)
            }
        }
    }

    /// How what was asked for stands, for whoever asked and whoever answers.
    pub fn describe(&self, a: &Ask) -> Value {
        let found = self.title(a.kind, ids(a), a.title_id);
        let (status, have, of) = match (a.state.as_str(), found) {
            ("declined", _) => ("declined", 0, 0),
            (_, Some(t)) => self.standing(t),
            ("added", None) => ("gone", 0, 0),
            _ => ("asked", 0, 0),
        };
        json!({
            "id": a.id, "kind": a.kind, "tmdb_id": a.tmdb_id, "tvmaze_id": a.tvmaze_id, "title": a.title, "year": a.year, "poster": a.poster,
            "asked_at": a.asked_at, "answered_at": a.answered_at, "status": status, "have": have, "of": of, "title_id": found.map(|t| t.id),
        })
    }
}

impl App {
    pub fn shelf(&self) -> Result<Shelf> {
        Ok(Shelf { titles: self.db.titles(None)?, files: self.db.file_counts()?, episodes: self.db.episode_counts()? })
    }

    /// What has been asked for, with how each stands: everyone's, or one account's.
    pub fn asks_report(&self, account: Option<i64>) -> Result<Vec<Value>> {
        let shelf = self.shelf()?;
        let names: HashMap<i64, String> = self.db.accounts()?.into_iter().map(|a| (a.id, a.name)).collect();
        Ok(self
            .db
            .asks(account)?
            .iter()
            .map(|a| {
                let mut v = shelf.describe(a);
                v["account"] = json!(names.get(&a.account_id));
                v
            })
            .collect())
    }

    /// How many asks are waiting for an answer.
    pub fn asks_waiting(&self) -> usize {
        self.asks_report(None).map(|all| all.iter().filter(|a| a["status"] == "asked").count()).unwrap_or(0)
    }

    /// Titles by name, for someone choosing what to ask for: each with how it stands in the
    /// library and whether this account has asked for it already.
    pub async fn asks_find(&self, account: i64, query: &str) -> Result<Vec<Value>> {
        if query.trim().is_empty() {
            return Ok(vec![]);
        }
        self.may_look_up(account)?;
        let found = self.discover(query).await?;
        let (shelf, mine) = (self.shelf()?, self.db.asks(Some(account))?);
        let mut out = vec![];
        for (list, kind) in [("movies", Kind::Movie), ("series", Kind::Series)] {
            for v in found[list].as_array().into_iter().flatten().take(12) {
                let id = |name: &str| v[name].as_u64().map(|n| n as u32);
                let (tmdb, tvmaze, tvdb) = (id("tmdb_id"), id("tvmaze_id"), id("tvdb_id"));
                // What cannot be added by its id cannot be asked for.
                if (kind == Kind::Movie && tmdb.is_none()) || (kind == Kind::Series && tvmaze.is_none()) {
                    continue;
                }
                let same = |a: &&Ask| a.kind == kind && if kind == Kind::Movie { a.tmdb_id == tmdb } else { a.tvmaze_id == tvmaze };
                let status = match (shelf.title(kind, if kind == Kind::Movie { (tmdb, None, None) } else { (None, tvmaze, tvdb) }, v["library_id"].as_i64()), mine.iter().find(same)) {
                    (Some(t), _) => shelf.standing(t).0,
                    (None, Some(a)) if a.state == "declined" => "declined",
                    (None, Some(a)) if a.state == "asked" => "asked",
                    _ => "",
                };
                out.push(json!({"kind": kind, "tmdb_id": if kind == Kind::Movie { tmdb } else { None }, "tvmaze_id": if kind == Kind::Series { tvmaze } else { None }, "title": v["title"], "year": v["year"], "poster": v["poster"], "overview": v["overview"], "status": status}));
            }
        }
        Ok(out)
    }

    /// Count a lookup against an account, and refuse it past so many in a minute: each one
    /// asks something of the places titles are looked up.
    fn may_look_up(&self, account: i64) -> Result<()> {
        let now = now();
        let mut all = self.looked_up.lock();
        all.retain(|_, (since, _)| now - *since < 60);
        let (_, n) = all.entry(account).or_insert((now, 0));
        if *n >= LOOKUPS {
            bail!("that is a lot of looking up; wait a minute and carry on");
        }
        *n += 1;
        Ok(())
    }

    /// Ask for a title on an account's behalf. What it is called and looks like is read from
    /// where titles are looked up, never taken from whoever asks.
    pub async fn ask_for(&self, account: &Account, kind: Kind, tmdb_id: Option<u32>, tvmaze_id: Option<u32>) -> Result<Value> {
        let (tmdb_id, tvmaze_id) = match kind {
            Kind::Movie => (Some(tmdb_id.ok_or_else(|| anyhow!("a film is asked for by its TMDB id"))?), None),
            Kind::Series => (None, Some(tvmaze_id.ok_or_else(|| anyhow!("a series is asked for by its TVmaze id"))?)),
        };
        let same = |a: &&Ask| a.kind == kind && a.tmdb_id == tmdb_id && a.tvmaze_id == tvmaze_id;
        // An ask that was turned down, or whose title has since left the library, is over:
        // asked again, it is a new one.
        let over = |shelf: &Shelf, a: &Ask| matches!(shelf.describe(a)["status"].as_str(), Some("declined" | "gone"));
        // Before anything is looked up: is this one of theirs already, and have they room for another?
        {
            let (shelf, mine) = (self.shelf()?, self.db.asks(Some(account.id))?);
            if let Some(a) = mine.iter().find(same).filter(|a| !over(&shelf, a)) {
                return Ok(shelf.describe(a));
            }
            if mine.iter().filter(|a| shelf.describe(a)["status"] == "asked").count() >= WAITING {
                bail!("{WAITING} things are waiting for an answer already; ask again when some have been answered");
            }
        }
        self.may_look_up(account.id)?;
        let g = self.settings.general();
        let listed = match kind {
            Kind::Movie => self.meta.movie(&g.tmdb_api_key, tmdb_id.unwrap_or_default()).await?,
            Kind::Series => self.meta.series(tvmaze_id.unwrap_or_default()).await?.0,
        };
        let tvdb_id = listed.tvdb_id.filter(|_| kind == Kind::Series);

        let _one = self.asking.lock().await;
        let shelf = self.shelf()?;
        if let Some(a) = self.db.asks(Some(account.id))?.iter().find(same) {
            if !over(&shelf, a) {
                return Ok(shelf.describe(a));
            }
            self.db.remove_ask(a.id, Some(account.id))?;
        }
        // One the library has already is as good as added: it only remains to say when it is ready.
        let here = shelf.title(kind, (tmdb_id, tvmaze_id, tvdb_id), None).map(|t| t.id);
        let ask = Ask {
            id: 0,
            account_id: account.id,
            kind,
            tmdb_id,
            tvmaze_id,
            tvdb_id,
            title: listed.title.clone(),
            year: listed.year,
            poster: listed.poster.clone().unwrap_or_default(),
            state: if here.is_some() { "added" } else { "asked" }.into(),
            title_id: here,
            asked_at: now(),
            answered_at: if here.is_some() { now() } else { 0 },
        };
        let id = self.db.add_ask(&ask)?;
        tracing::info!(account = %account.name, title = %ask.title, "something was asked for");
        if here.is_none() && account.adds {
            if let Err(e) = self.ask_add_held(id).await {
                tracing::warn!(error = %e, title = %ask.title, "what was asked for could not be added; it waits to be answered");
            }
        }
        self.emit(Event::Attention);
        let ask = self.db.ask(id)?.ok_or_else(|| anyhow!("the ask was not kept"))?;
        Ok(self.shelf()?.describe(&ask))
    }

    /// Add what was asked for to the library and look for it, as adding it by hand would.
    /// Everyone who asked for the same thing is answered by it.
    pub async fn ask_add(&self, id: i64) -> Result<Title> {
        let _one = self.asking.lock().await;
        let title = self.ask_add_held(id).await?;
        self.emit(Event::Attention);
        Ok(title)
    }

    /// As [`ask_add`], for a caller that holds `asking` already: one at a time, so that two
    /// answers at once add one title.
    async fn ask_add_held(&self, id: i64) -> Result<Title> {
        let ask = self.db.ask(id)?.ok_or_else(|| anyhow!("no such ask"))?;
        let g = self.settings.general();
        let here = self.shelf()?.title(ask.kind, ids(&ask), ask.title_id).cloned();
        let title = match here {
            Some(t) => t,
            None => self.add_title(ask.kind, ask.tmdb_id, ask.tvmaze_id, self.profile_for_new(ask.kind, g.new_titles_location), true, true).await?,
        };
        for other in self.db.asks(None)?.iter().filter(|a| a.kind == ask.kind && a.tmdb_id == ask.tmdb_id && a.tvmaze_id == ask.tvmaze_id && (a.state != "added" || a.title_id != Some(title.id))) {
            self.db.answer_ask(other.id, "added", Some(title.id))?;
        }
        Ok(title)
    }

    pub fn ask_decline(&self, id: i64) -> Result<()> {
        self.db.ask(id)?.ok_or_else(|| anyhow!("no such ask"))?;
        self.db.answer_ask(id, "declined", None)?;
        self.emit(Event::Attention);
        Ok(())
    }
}
