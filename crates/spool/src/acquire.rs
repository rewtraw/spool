//! Finding releases, judging them, and sending the chosen one to the downloader.

use crate::app::{App, Event};
use crate::models::*;
use crate::newznab::{self, Query};
use crate::settings::Mode;
use anyhow::{anyhow, bail, Result};
use spool_core::decision::{self, Candidate, Context, ExistingFile, Rejection};
use spool_core::matching;
use spool_core::parser::{parse_episode_title, parse_movie_title};
use spool_core::profile::QualityProfile;
use spool_core::{Quality, QualityModel};
use std::collections::{HashMap, HashSet};

pub const COMPACT_REASON: &str = "smaller copy that fits the profile's size target";
pub const BY_HAND_REASON: &str = "chosen by hand";

/// What a search is for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scope {
    Movie,
    Episode(i64),
    Season(u32),
    /// Every monitored, aired episode without a file.
    Missing,
    /// Files larger than the profile's size target, to be replaced by copies that fit it.
    Compact,
}

/// A release whose articles have mostly left the Usenet servers, found before downloading it.
#[derive(Debug)]
pub struct Gone {
    pub found: usize,
    pub asked: usize,
}

impl std::fmt::Display for Gone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no longer on your Usenet servers ({} of {} sampled articles found)", self.found, self.asked)
    }
}

impl std::error::Error for Gone {}

struct Judged {
    record: DecisionRecord,
    candidate: Candidate,
}

fn rej(code: &str, message: impl Into<String>) -> Rejection {
    Rejection { code: code.into(), message: message.into() }
}

fn rejected(title_id: i64, release: &Release, source: &str, quality: QualityModel, r: Rejection) -> DecisionRecord {
    DecisionRecord { id: 0, ts: 0, title_id, release: release.clone(), quality, languages: vec![], release_group: None, covers: String::new(), episode_ids: vec![], accepted: false, rejections: vec![r], source: source.into() }
}

/// Everything about one title that judging a release needs, loaded once per batch.
pub struct TitleContext {
    pub title: Title,
    pub profile: QualityProfile,
    pub episodes: Vec<Episode>,
    pub files: Vec<MediaFile>,
    pub active: Vec<Acquisition>,
    pub blocklist: Vec<(String, String)>,
    /// Judge for a smaller copy of what is on disk, not for an upgrade.
    pub compact: bool,
    /// Files over the profile's size target, when compacting.
    pub oversize: HashSet<i64>,
    /// Where each indexer stands against its daily allowance.
    pub budgets: HashMap<i64, IndexerBudget>,
}

impl App {
    pub fn title_context(&self, title: Title) -> Result<TitleContext> {
        let profiles = self.db.profiles()?;
        let profile = profiles
            .iter()
            .find(|p| p.id == title.profile_id)
            .or_else(|| profiles.iter().find(|p| p.kind == title.kind.profile_kind()))
            .cloned()
            .unwrap_or_else(|| QualityProfile::default_hd(title.kind.profile_kind()));
        Ok(TitleContext {
            episodes: if title.kind == Kind::Series { self.db.episodes(title.id)? } else { vec![] },
            files: self.db.files(title.id)?,
            active: self.db.title_acquisitions(title.id)?.into_iter().filter(|a| a.state.is_active()).collect(),
            blocklist: self.db.blocklisted(title.id)?,
            profile,
            title,
            compact: false,
            oversize: HashSet::new(),
            budgets: self.indexer_budgets(),
        })
    }

    fn judge(&self, tc: &TitleContext, release: &Release, source: &str, scope: Option<Scope>, user: bool) -> Judged {
        let g = self.settings.general();
        let limits = self.settings.size_limits();
        let t = &tc.title;
        let flavor = t.kind.flavor();
        let unknown = QualityModel::new(Quality::Unknown);
        let blank = |r: Rejection, q: QualityModel| Judged {
            record: rejected(t.id, release, source, q, r),
            candidate: Candidate { quality: q, size_bytes: release.size, age_days: release.age_days(), languages: vec![], release_group: None, indexer_priority: release.indexer_priority, item_count: 0, dubbed: false, full_season: false, multi_season: false, efficient_codec: false, playback: Default::default() },
        };

        let (quality, languages, group, covered, covers, full_season, multi_season, pre): (QualityModel, Vec<String>, Option<String>, Vec<&Episode>, String, bool, bool, Option<Rejection>) = match t.kind {
            Kind::Movie => match parse_movie_title(&release.title, false) {
                Some(p) => {
                    let by_id = release.imdb_id.is_some() && release.imdb_id == t.imdb_id;
                    let mut pre = None;
                    if matching::match_movie(&p, &[t.key()]).is_none() {
                        // The indexer tagged this release with the movie's own id. Radarr trusts that; Spool
                        // does too, unless the release names a clearly different year (a remake or a mis-tag).
                        // Only a tag on the release itself counts, never the fact that we searched by id.
                        let year_close = p.year == 0 || p.year.abs_diff(t.year) <= 1 || t.alt_years.contains(&p.year);
                        if !(by_id && year_close) {
                            pre = Some(rej("wrong_title", format!("looks like \"{}\"{}, not {} ({})", p.title(), if p.year > 0 { format!(" ({})", p.year) } else { String::new() }, t.title, t.year)));
                        }
                    }
                    (p.quality, p.languages, p.release_group, vec![], String::new(), false, false, pre)
                }
                // The parser wants a year in a film's release name, and some posts, fan releases of
                // anime above all, leave it out. If the name plainly carries this film's full title,
                // take it as this film and read the rest of the name as usual.
                None if names_film_without_year(t, &release.title) => (
                    spool_core::quality::parse_quality(&release.title, flavor),
                    spool_core::parser::parse_languages(&release.title),
                    spool_core::parser::parse_release_group(&release.title, flavor),
                    vec![],
                    String::new(),
                    false,
                    false,
                    None,
                ),
                None => return blank(rej("unparseable", "the release name could not be understood"), unknown),
            },
            Kind::Series => {
                let Some(p) = parse_episode_title(&release.title) else {
                    return blank(rej("unparseable", "the release name could not be understood"), unknown);
                };
                let q = p.quality.unwrap_or(unknown);
                let by_id = release.tvdb_id.is_some() && release.tvdb_id == t.tvdb_id;
                let mut pre = None;
                if !by_id && matching::match_series(&p, &[t.key()]).is_none() {
                    pre = Some(rej("wrong_title", format!("looks like \"{}\", not {}", p.series_title, t.title)));
                }
                let season = p.season_number();
                let mut covered: Vec<&Episode> = vec![];
                let mut covers = String::new();
                if pre.is_none() {
                    if p.is_multi_season() {
                        covers = format!("Seasons {}-{}", p.season_numbers.first().unwrap_or(&0), p.season_numbers.last().unwrap_or(&0));
                    } else if let Some(date) = &p.air_date {
                        covered = tc.episodes.iter().filter(|e| e.air_date.as_deref() == Some(date.as_str())).collect();
                        covers = date.clone();
                        if covered.is_empty() {
                            pre = Some(rej("unknown_episode", format!("no episode aired on {date}")));
                        }
                    } else if p.full_season {
                        covered = tc.episodes.iter().filter(|e| e.season == season).collect();
                        covers = format!("Season {season}");
                        if covered.is_empty() {
                            pre = Some(rej("unknown_episode", format!("season {season} is not in the episode list")));
                        }
                    } else if !p.episode_numbers.is_empty() {
                        covered = tc.episodes.iter().filter(|e| e.season == season && p.episode_numbers.contains(&e.episode)).collect();
                        covers = format!("S{season:02}{}", p.episode_numbers.iter().map(|n| format!("E{n:02}")).collect::<String>());
                        if covered.len() != p.episode_numbers.len() {
                            pre = Some(rej("unknown_episode", format!("{covers} is not in the episode list")));
                        }
                    } else if p.is_absolute() {
                        // Anime numbering: one count across seasons.
                        match episodes_by_absolute(&tc.episodes, &p.absolute_episode_numbers) {
                            Some(found) => {
                                covers = found.iter().map(|e| format!("S{:02}E{:02}", e.season, e.episode)).collect::<Vec<_>>().join(" ");
                                if found.len() > 3 {
                                    covers = format!("{} to {}", covers.split(' ').next().unwrap_or(""), covers.split(' ').next_back().unwrap_or(""));
                                }
                                covered = found;
                            }
                            None => pre = Some(rej("unknown_episode", format!("episode {} by absolute count is not in the episode list", p.absolute_episode_numbers.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")))),
                        }
                    } else {
                        pre = Some(rej("unsupported_release", "partial seasons, extras and unnumbered specials are not handled"));
                    }
                }
                if pre.is_none() && (p.is_partial_season || p.is_season_extra || (p.special && p.episode_numbers.is_empty())) {
                    pre = Some(rej("unsupported_release", "partial seasons, extras and unnumbered specials are not handled"));
                }
                {
                    let (full, multi) = (p.full_season, p.is_multi_season());
                    (q, p.languages, p.release_group, covered, covers, full, multi, pre)
                }
            }
        };

        let covered_ids: Vec<i64> = covered.iter().map(|e| e.id).collect();
        let candidate = Candidate {
            quality,
            size_bytes: release.size,
            age_days: release.age_days(),
            languages: languages.clone(),
            release_group: group.clone(),
            // Between equal releases, one from an indexer with allowance to spare goes first.
            indexer_priority: release.indexer_priority + if tc.budgets.get(&release.indexer_id).is_some_and(|b| b.grabs_low()) { 1000 } else { 0 },
            item_count: covered.len().max(1) as u32,
            dubbed: !g.language.is_empty() && languages.iter().any(|l| l != "original" && *l != g.language && !t.original_language.as_deref().is_some_and(|o| l.starts_with(o))),
            full_season,
            multi_season,
            efficient_codec: {
                let t = decision::PlaybackTraits::from_title(&release.title);
                t.hevc || t.av1
            },
            playback: decision::PlaybackTraits::from_title(&release.title),
        };
        let mut record = DecisionRecord { id: 0, ts: 0, title_id: t.id, release: release.clone(), quality, languages, release_group: group, covers, episode_ids: covered_ids.clone(), accepted: false, rejections: vec![], source: source.into() };

        if let Some(r) = pre {
            record.rejections.push(r);
            return Judged { record, candidate };
        }

        // A search for one episode or season should not grab something else that happened to come back.
        match scope {
            Some(Scope::Episode(id)) if !covered_ids.contains(&id) => {
                record.rejections.push(rej("not_requested", "does not contain the requested episode"));
                return Judged { record, candidate };
            }
            Some(Scope::Season(n)) if covered.iter().any(|e| e.season != n) || covered.is_empty() => {
                record.rejections.push(rej("not_requested", format!("is not season {n}")));
                return Judged { record, candidate };
            }
            _ => {}
        }

        // Compacting replaces what is too big and leaves alone what already fits.
        if tc.compact {
            let touches = match t.kind {
                Kind::Movie => tc.files.iter().any(|f| tc.oversize.contains(&f.id)),
                Kind::Series => covered.iter().any(|e| e.file_id.is_some_and(|f| tc.oversize.contains(&f))),
            };
            if !touches {
                record.rejections.push(rej("not_requested", "what it covers is already within the size target"));
                return Judged { record, candidate };
            }
        }

        let now = chrono::Utc::now();
        let today = chrono::Local::now().date_naive();
        let (existing, monitored, available, season_complete, runtime): (Vec<ExistingFile>, bool, bool, bool, u32) = match t.kind {
            Kind::Movie => (
                tc.files.iter().map(|f| ExistingFile { quality: f.quality, release_group: f.release_group.clone(), size_bytes: f.size }).collect(),
                t.monitored,
                t.movie_available(today),
                true,
                t.runtime,
            ),
            Kind::Series => {
                let mut file_ids: Vec<i64> = covered.iter().filter_map(|e| e.file_id).collect();
                file_ids.sort_unstable();
                file_ids.dedup();
                let existing = tc.files.iter().filter(|f| file_ids.contains(&f.id)).map(|f| ExistingFile { quality: f.quality, release_group: f.release_group.clone(), size_bytes: f.size }).collect();
                // A pack is wanted if anything in it is; a single release needs its episodes monitored.
                let monitored = t.monitored && covered.iter().any(|e| e.monitored && t.season_monitored(e.season));
                let aired = covered.iter().all(|e| e.has_aired(now));
                let runtime: u32 = covered.iter().map(|e| if e.runtime > 0 { e.runtime } else { t.runtime }).sum();
                (existing, monitored, aired, aired, runtime)
            }
        };
        let queued: Vec<QualityModel> = tc
            .active
            .iter()
            .filter(|a| t.kind == Kind::Movie || a.episode_ids.iter().any(|id| covered_ids.contains(id)))
            .map(|a| a.quality)
            .collect();
        let blocklisted = tc.blocklist.iter().any(|(guid, name)| (!guid.is_empty() && *guid == release.guid) || *name == release.title);
        let limit_key = format!("{}:{}", t.kind.profile_kind(), quality.quality.key());
        let ctx = Context {
            flavor,
            profile: &tc.profile,
            size_limit: limits.get(&limit_key).copied(),
            runtime_minutes: runtime,
            existing: &existing,
            queued: &queued,
            blocklisted,
            compact: tc.compact,
            monitored,
            available,
            season_complete,
            retention_days: g.retention_days,
            proper_policy: g.proper_policy,
            wanted_language: if g.language.is_empty() { None } else { Some(g.language.as_str()) },
            original_language: t.original_language.as_deref(),
            user_invoked: user,
        };
        record.rejections = decision::evaluate(&candidate, &ctx);
        record.accepted = record.rejections.is_empty();
        Judged { record, candidate }
    }

    /// Judge stored releases again against the library as it is now. A verdict recorded before a
    /// file arrived, or before a profile changed, would otherwise keep saying "acceptable".
    pub fn rejudge(&self, tc: &TitleContext, stored: Vec<DecisionRecord>) -> Vec<DecisionRecord> {
        let policy = self.settings.general().proper_policy;
        let mut judged: Vec<Judged> = stored
            .into_iter()
            .map(|d| {
                let mut fresh = self.judge(tc, &d.release, &d.source, None, false);
                fresh.record.id = d.id;
                fresh.record.ts = d.ts;
                fresh
            })
            .collect();
        // Acceptable releases first, in the order Spool would choose them; then the rest, newest first.
        judged.sort_by(|a, b| {
            b.record.accepted.cmp(&a.record.accepted).then_with(|| {
                if a.record.accepted {
                    decision::compare(&a.candidate, &b.candidate, &tc.profile, policy)
                } else {
                    b.record.ts.cmp(&a.record.ts)
                }
            })
        });
        judged.into_iter().map(|j| j.record).collect()
    }

    /// Remember what an indexer last said about the account's allowance.
    pub fn note_indexer_limits(&self, indexer_id: i64, limits: newznab::Limits) {
        let mut all: HashMap<String, serde_json::Value> = self.db.get_setting("indexer_reported");
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        let entry = serde_json::json!({"day": day, "limits": limits});
        if all.get(&indexer_id.to_string()) != Some(&entry) {
            all.insert(indexer_id.to_string(), entry);
            let _ = self.db.set_setting("indexer_reported", &all);
        }
    }

    /// Each indexer's use today against its allowance: Spool's own count, or the indexer's figure
    /// when that is higher (other programs may share the account).
    pub fn indexer_budgets(&self) -> HashMap<i64, IndexerBudget> {
        let used = self.db.indexer_usage_today().unwrap_or_default();
        let reported: HashMap<String, serde_json::Value> = self.db.get_setting("indexer_reported");
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        self.db
            .indexers()
            .unwrap_or_default()
            .iter()
            .map(|ix| {
                let (mut requests, mut grabs) = used.get(&ix.id).copied().unwrap_or((0, 0));
                let said: Option<newznab::Limits> = reported.get(&ix.id.to_string()).and_then(|v| serde_json::from_value(v["limits"].clone()).ok());
                let today = reported.get(&ix.id.to_string()).is_some_and(|v| v["day"] == day.as_str());
                if let (Some(l), true) = (said, today) {
                    requests = requests.max(l.api_current.unwrap_or(0));
                    grabs = grabs.max(l.grab_current.unwrap_or(0));
                }
                let set = ix.daily_requests > 0 || ix.daily_grabs > 0;
                let (request_limit, grab_limit) = if set { (ix.daily_requests, ix.daily_grabs) } else { (said.and_then(|l| l.api_max).unwrap_or(0), said.and_then(|l| l.grab_max).unwrap_or(0)) };
                let source = if set {
                    Some("set".to_string())
                } else if request_limit > 0 || grab_limit > 0 {
                    Some("reported".to_string())
                } else {
                    // Usage reported with no maximum: the indexer is saying the account has no cap.
                    said.filter(|l| l.api_current.is_some() || l.grab_current.is_some()).map(|_| "reported_unlimited".to_string())
                };
                (ix.id, IndexerBudget { requests, grabs, request_limit, grab_limit, source })
            })
            .collect()
    }

    /// Ask one indexer, spacing requests so a burst of searches does not hammer it.
    async fn query_indexer(&self, ix: &Indexer, q: &Query, user: bool) -> Result<Vec<Release>> {
        // A failing indexer is left alone for a while, longer each time. Many have small daily
        // request allowances, and asking again every few minutes only makes that worse.
        if !user {
            if let Some((_, until, why)) = self.indexer_backoff.lock().get(&ix.id) {
                let now = std::time::Instant::now();
                if *until > now {
                    bail!("{} skipped for another {} min after: {why}", ix.name, (*until - now).as_secs() / 60 + 1);
                }
            }
        }
        let wait = {
            let mut last = self.indexer_last.lock();
            let now = std::time::Instant::now();
            let gap = std::time::Duration::from_millis(2000);
            let wait = last.get(&ix.id).map(|t| gap.saturating_sub(now.duration_since(*t))).unwrap_or_default();
            last.insert(ix.id, now + wait);
            wait
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        // What is left of a daily allowance is kept for searches a person starts.
        if !user && self.indexer_budgets().get(&ix.id).is_some_and(|b| b.requests_low()) {
            bail!("{} skipped: most of today's request allowance is used, and the rest is kept for searches you start", ix.name);
        }
        let _ = self.db.count_indexer(ix.id, 1, 0);
        match newznab::fetch(&self.http, ix, q).await {
            Ok((r, limits)) => {
                self.indexer_backoff.lock().remove(&ix.id);
                if let Some(l) = limits {
                    self.note_indexer_limits(ix.id, l);
                }
                Ok(r)
            }
            Err(e) => {
                let msg = e.to_string();
                let mut b = self.indexer_backoff.lock();
                let failures = b.get(&ix.id).map(|x| x.0).unwrap_or(0) + 1;
                let limited = msg.to_lowercase().contains("limit");
                let minutes: u64 = if limited { 180 } else { [5, 15, 60, 180, 360][(failures as usize - 1).min(4)] };
                b.insert(ix.id, (failures, std::time::Instant::now() + std::time::Duration::from_secs(minutes * 60), msg.clone()));
                Err(e)
            }
        }
    }

    async fn query_all(&self, kind: Kind, queries: &[Query], rss: bool, user: bool) -> (Vec<Release>, Vec<String>) {
        let indexers: Vec<Indexer> = self
            .db
            .indexers()
            .unwrap_or_default()
            .into_iter()
            .filter(|i| if rss { i.enable_rss } else { i.enable_search })
            .filter(|i| if kind == Kind::Movie { i.movies } else { i.tv })
            .collect();
        let mut out: Vec<Release> = vec![];
        let mut errors = vec![];
        let results = futures::future::join_all(indexers.iter().map(|ix| async move {
            let mut all = vec![];
            let mut errs = vec![];
            for q in queries {
                match self.query_indexer(ix, q, user).await {
                    Ok(r) => all.extend(r),
                    Err(e) => {
                        errs.push(e.to_string());
                        break;
                    }
                }
            }
            (all, errs)
        }))
        .await;
        let mut seen = HashSet::new();
        for (releases, errs) in results {
            errors.extend(errs);
            for r in releases {
                if seen.insert((r.indexer_id, r.guid.clone())) {
                    out.push(r);
                }
            }
        }
        (out, errors)
    }

    /// Search the indexers for one title and record a verdict for every result.
    /// With `grab`, the best acceptable results are sent to the downloader.
    pub async fn search(&self, title_id: i64, scope: Scope, user: bool, grab: bool) -> Result<SearchOutcome> {
        self.search_with(title_id, scope, user, grab, user).await
    }

    /// As [`search`], with a separate say over disk space: `may_queue` lets the chosen release
    /// join the queue and wait for room, which is right for anything a person just asked for,
    /// even when the rest of the search runs under the automatic rules.
    pub async fn search_with(&self, title_id: i64, scope: Scope, user: bool, grab: bool, may_queue: bool) -> Result<SearchOutcome> {
        let title = self.db.title(title_id)?.ok_or_else(|| anyhow!("no such title"))?;
        let mut tc = self.title_context(title)?;
        tc.compact = scope == Scope::Compact;
        let now = chrono::Utc::now();
        // Files over the profile's per-item size target; what a compacting search is about.
        let oversize: HashSet<i64> = match tc.profile.size_ceiling() {
            Some(ceiling) if tc.compact => tc.files.iter().filter(|f| f.size > ceiling).map(|f| f.id).collect(),
            _ => HashSet::new(),
        };
        tc.oversize = oversize.clone();
        let t = &tc.title;
        if tc.compact && oversize.is_empty() {
            let message = match tc.profile.target_size_gb {
                _ if tc.files.is_empty() => "Nothing to compact: there is no file on disk yet".to_string(),
                Some((_, hi)) => format!("Nothing to compact: every file is within the {hi:.0} GB target of profile \"{}\"", tc.profile.name),
                None => format!("Profile \"{}\" has no size target to compact to", tc.profile.name),
            };
            return Ok(SearchOutcome { decisions: self.db.decisions(title_id)?, grabbed: vec![], errors: vec![], message });
        }
        let queries: Vec<Query> = match (t.kind, scope) {
            (Kind::Movie, _) => vec![Query::Movie { imdb_id: t.imdb_id.clone(), title: t.title.clone(), year: t.year }],
            (Kind::Series, Scope::Episode(id)) => {
                let e = tc.episodes.iter().find(|e| e.id == id).ok_or_else(|| anyhow!("no such episode"))?;
                vec![Query::Episode { tvdb_id: t.tvdb_id, title: t.title.clone(), season: e.season, episode: e.episode }]
            }
            (Kind::Series, Scope::Season(n)) => vec![Query::Season { tvdb_id: t.tvdb_id, title: t.title.clone(), season: n }],
            (Kind::Series, Scope::Compact) => {
                let mut seasons: Vec<u32> = tc.episodes.iter().filter(|e| e.file_id.is_some_and(|f| oversize.contains(&f))).map(|e| e.season).collect();
                seasons.sort_unstable();
                seasons.dedup();
                seasons.into_iter().take(6).map(|n| Query::Season { tvdb_id: t.tvdb_id, title: t.title.clone(), season: n }).collect()
            }
            (Kind::Series, _) => {
                // One season query per season with something missing; it returns packs and single episodes.
                let mut seasons: Vec<u32> = tc
                    .episodes
                    .iter()
                    .filter(|e| e.season > 0 && e.monitored && t.season_monitored(e.season) && e.file_id.is_none() && e.has_aired(now))
                    .map(|e| e.season)
                    .collect();
                seasons.sort_unstable();
                seasons.dedup();
                seasons.into_iter().map(|n| Query::Season { tvdb_id: t.tvdb_id, title: t.title.clone(), season: n }).collect()
            }
        };
        if queries.is_empty() {
            return Ok(SearchOutcome { decisions: vec![], grabbed: vec![], errors: vec![], message: "Nothing is missing".into() });
        }
        let (mut releases, errors) = self.query_all(t.kind, &queries, false, user).await;
        // Releases saved earlier join the results, so a search still finds something when every
        // indexer is down, and a title removed and re-added gets its old options back.
        let mut gone: HashMap<String, (usize, usize)> = HashMap::new();
        for n in self.archive_candidates(t).await {
            if n.usable() == Some(false) {
                gone.insert(n.release.title.clone(), n.available.unwrap_or((0, 0)));
            }
            if !releases.iter().any(|r| r.title == n.release.title) {
                let mut r = n.release.clone();
                r.link = App::archive_link(n.id);
                r.indexer = "Archive".into();
                r.indexer_id = -1;
                r.indexer_priority = 100;
                releases.push(r);
            }
        }
        let scope_filter = match scope {
            Scope::Episode(_) | Scope::Season(_) => Some(scope),
            _ => None,
        };
        let source = if user { "manual" } else { "search" };
        let mut judged: Vec<Judged> = releases.iter().map(|r| self.judge(&tc, r, source, scope_filter, user)).collect();

        // Indexers find a series by its id only where they tagged the release with it, and many
        // posts are not tagged. If the id search leaves wanted episodes without an acceptable
        // release, ask again by name.
        let mut errors = errors;
        // A film the indexer has not linked to its id is invisible to a search by id, and anime
        // films sit outside the movie categories altogether. When the first search turns up
        // nothing acceptable, ask by name: with the year, then without.
        if t.kind == Kind::Movie && !tc.compact && !judged.iter().any(|j| j.record.accepted) {
            let plain: String = t.title.chars().map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
            let mut texts = vec![];
            if t.year > 0 {
                texts.push(format!("{plain} {}", t.year));
            }
            texts.push(plain);
            for text in texts {
                let (more, errs) = self.query_all(t.kind, &[Query::MovieText { text }], false, user).await;
                errors.extend(errs);
                for r in more {
                    if !releases.iter().any(|x| x.title == r.title && x.indexer_id == r.indexer_id) {
                        judged.push(self.judge(&tc, &r, source, scope_filter, user));
                        releases.push(r);
                    }
                }
                if judged.iter().any(|j| j.record.accepted) {
                    break;
                }
            }
        }
        if t.kind == Kind::Series {
            let wanted: Vec<&Episode> = match scope {
                Scope::Episode(id) => tc.episodes.iter().filter(|e| e.id == id).collect(),
                Scope::Season(n) => tc.episodes.iter().filter(|e| e.season == n && e.file_id.is_none() && e.has_aired(now)).collect(),
                Scope::Compact => tc.episodes.iter().filter(|e| e.file_id.is_some_and(|f| oversize.contains(&f))).collect(),
                _ => tc.episodes.iter().filter(|e| e.season > 0 && e.monitored && t.season_monitored(e.season) && e.file_id.is_none() && e.has_aired(now)).collect(),
            };
            let uncovered = |judged: &[Judged]| -> Vec<&Episode> {
                let have: HashSet<i64> = judged.iter().filter(|j| j.record.accepted).flat_map(|j| j.record.episode_ids.iter().copied()).collect();
                wanted.iter().filter(|e| !have.contains(&e.id)).copied().collect()
            };
            let mut texts: Vec<String> = vec![];
            let left = uncovered(&judged);
            if !left.is_empty() {
                texts.push(match (scope, left.as_slice()) {
                    (Scope::Episode(_), [e]) => format!("{} S{:02}E{:02}", t.title, e.season, e.episode),
                    _ => t.title.clone(),
                });
            }
            for round in 0..2 {
                if texts.is_empty() {
                    break;
                }
                let queries: Vec<Query> = texts.drain(..).map(|text| Query::TvText { text }).collect();
                let (more, errs) = self.query_all(t.kind, &queries, false, user).await;
                errors.extend(errs);
                for r in more {
                    if !releases.iter().any(|x| x.title == r.title && x.indexer_id == r.indexer_id) {
                        judged.push(self.judge(&tc, &r, source, scope_filter, user));
                        releases.push(r);
                    }
                }
                // A long series returns more than one page of names; narrow to the seasons still short.
                if round == 0 && !matches!(scope, Scope::Episode(_)) {
                    let mut seasons: Vec<u32> = uncovered(&judged).iter().map(|e| e.season).collect();
                    seasons.sort_unstable();
                    seasons.dedup();
                    texts = seasons.into_iter().take(3).map(|n| format!("{} S{n:02}", t.title)).collect();
                }
            }
        }
        for j in &mut judged {
            // Only an archive result is condemned by a failed check; a live result with the same
            // name may be a fresh repost.
            if let (Some((found, asked)), true) = (gone.get(&j.record.release.title), j.record.release.indexer_id == -1) {
                j.record.rejections.push(rej("gone_from_usenet", format!("saved earlier, but no longer on your Usenet servers ({found} of {asked} sampled articles found)")));
                j.record.accepted = false;
            }
        }
        self.db.record_decisions(&judged.iter().map(|j| j.record.clone()).collect::<Vec<_>>())?;
        self.emit(Event::Decisions { title_id });

        let mut grabbed: Vec<Acquisition> = vec![];
        let mut grab_errors: Vec<String> = vec![];
        let mut blocked: Vec<(u64, String)> = vec![];
        if grab {
            let policy = self.settings.general().proper_policy;
            // A chosen release that turns out to be gone from Usenet is set aside and the choice
            // made again from what is left, a few times at most.
            let mut dead: HashSet<String> = HashSet::new();
            for _ in 0..4 {
                let claimed: HashSet<i64> = grabbed.iter().flat_map(|a| a.episode_ids.iter().copied()).collect();
                let live: Vec<&Judged> = judged
                    .iter()
                    .filter(|j| !dead.contains(&j.record.release.title))
                    .filter(|j| if j.record.episode_ids.is_empty() { grabbed.is_empty() } else { !j.record.episode_ids.iter().any(|id| claimed.contains(id)) })
                    .collect();
                let mut any_gone = false;
                for j in pick(&live, &tc.profile, policy) {
                    match self.grab(&j.record, &tc.title, if tc.compact { COMPACT_REASON } else { "best acceptable release from search" }, may_queue).await {
                        Ok(Some(a)) => grabbed.push(a),
                        Ok(None) => {}
                        Err(e) if e.downcast_ref::<Gone>().is_some() => {
                            tracing::info!(release = %j.record.release.title, "{e}; choosing again");
                            dead.insert(j.record.release.title.clone());
                            any_gone = true;
                        }
                        Err(e) => {
                            tracing::warn!(release = %j.record.release.title, error = %e, "grab failed");
                            grab_errors.push(format!("Could not start {}: {e}", j.record.release.title));
                            blocked.push((j.record.release.size, e.to_string()));
                            dead.insert(j.record.release.title.clone());
                        }
                    }
                }
                if !any_gone {
                    break;
                }
            }
        }
        if !grabbed.is_empty() {
            self.archive_runner_ups(&tc.title, runner_ups(&judged, &grabbed, &tc.profile, self.settings.general().proper_policy, self.settings.general().archive_runner_ups as usize));
        }
        let mut t2 = tc.title.clone();
        t2.last_search_at = crate::db::now();
        self.db.save_title(&mut t2)?;

        let accepted = judged.iter().filter(|j| j.record.accepted).count();
        let message = if releases.is_empty() && !errors.is_empty() {
            format!("Search failed: {}", errors.join("; "))
        } else if blocked.len() > 1 {
            // Several refusals nearly always share one cause; say it once.
            let total: u64 = blocked.iter().map(|b| b.0).sum();
            let cause = blocked[0].1.split(", and this release").next().unwrap_or(&blocked[0].1).to_string();
            let same = blocked.iter().all(|b| b.1.split(", and this release").next() == Some(cause.as_str()));
            format!("Could not start {} downloads ({:.0} GB in all): {}{}", blocked.len(), total as f64 / 1e9, cause, if same { "" } else { ", among other reasons" })
        } else if !grab_errors.is_empty() {
            grab_errors.join("; ")
        } else {
            format!("{} results, {} acceptable{}", releases.len(), accepted, if grabbed.is_empty() { String::new() } else { format!(", {} sent to download", grabbed.len()) })
        };
        Ok(SearchOutcome { decisions: self.db.decisions(title_id)?, grabbed, errors, message })
    }

    /// Read every indexer's recent-release feed and act on anything the library wants.
    pub async fn rss_sync(&self) -> Result<String> {
        let g = self.settings.general();
        let mut considered = 0;
        let mut grabbed = 0;
        let mut errors = vec![];
        for kind in [Kind::Movie, Kind::Series] {
            let titles = self.db.titles(Some(kind))?;
            if titles.is_empty() {
                continue;
            }
            let (releases, errs) = self.query_all(kind, &[if kind == Kind::Movie { Query::RecentMovies } else { Query::RecentTv }], true, false).await;
            errors.extend(errs);
            let keys: Vec<matching::TitleKey> = titles.iter().map(|t| t.key()).collect();
            let by_tvdb: HashMap<u32, i64> = titles.iter().filter_map(|t| t.tvdb_id.map(|id| (id, t.id))).collect();
            let by_imdb: HashMap<&str, i64> = titles.iter().filter_map(|t| t.imdb_id.as_deref().map(|id| (id, t.id))).collect();

            // Sort each release to the title it belongs to; most belong to none.
            let mut per_title: HashMap<i64, Vec<Release>> = HashMap::new();
            for r in releases {
                let id = match kind {
                    Kind::Movie => r.imdb_id.as_deref().and_then(|i| by_imdb.get(i).copied()).or_else(|| parse_movie_title(&r.title, false).and_then(|p| matching::match_movie(&p, &keys).map(|k| k.id))),
                    Kind::Series => r.tvdb_id.and_then(|i| by_tvdb.get(&i).copied()).or_else(|| parse_episode_title(&r.title).and_then(|p| matching::match_series(&p, &keys).map(|k| k.id))),
                };
                if let Some(id) = id {
                    per_title.entry(id).or_default().push(r);
                }
            }
            for (title_id, releases) in per_title {
                let Some(title) = titles.iter().find(|t| t.id == title_id).cloned() else { continue };
                let tc = self.title_context(title)?;
                let judged: Vec<Judged> = releases.iter().map(|r| self.judge(&tc, r, "rss", None, false)).collect();
                considered += judged.len();
                self.db.record_decisions(&judged.iter().map(|j| j.record.clone()).collect::<Vec<_>>())?;
                for j in pick(&judged.iter().collect::<Vec<_>>(), &tc.profile, g.proper_policy) {
                    match self.grab(&j.record, &tc.title, "wanted release seen in the indexer feed", false).await {
                        Ok(Some(a)) => {
                            grabbed += 1;
                            self.archive_runner_ups(&tc.title, runner_ups(&judged, std::slice::from_ref(&a), &tc.profile, g.proper_policy, g.archive_runner_ups as usize));
                        }
                        Ok(None) => {}
                        Err(e) => tracing::warn!(release = %j.record.release.title, error = %e, "grab failed"),
                    }
                }
                self.emit(Event::Decisions { title_id });
            }
        }
        let mut msg = format!("{considered} matching releases, {grabbed} {}", if g.mode == Mode::Shadow { "would be grabbed" } else { "grabbed" });
        if !errors.is_empty() {
            msg.push_str(&format!("; errors: {}", errors.join("; ")));
        }
        Ok(msg)
    }

    /// Send a release to the downloader. In shadow mode this only records what would have happened.
    pub async fn grab(&self, d: &DecisionRecord, title: &Title, reason: &str, user: bool) -> Result<Option<Acquisition>> {
        // One grab of a given release at a time; a concurrent duplicate waits and then finds the first.
        let key = (title.id, d.release.title.clone());
        for _ in 0..600 {
            if self.grabbing.lock().insert(key.clone()) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        let result = self.grab_inner(d, title, reason, user).await;
        self.grabbing.lock().remove(&key);
        result
    }

    async fn grab_inner(&self, d: &DecisionRecord, title: &Title, reason: &str, user: bool) -> Result<Option<Acquisition>> {
        let summary = serde_json::json!({
            "release": d.release.title, "indexer": d.release.indexer, "size": d.release.size, "guid": d.release.guid,
            "quality": d.quality, "covers": d.covers, "episode_ids": d.episode_ids, "reason": reason,
        });
        if !self.is_active() {
            let already = self.db.history(Some(title.id), 200)?.iter().any(|h| h.kind == "would_grab" && h.data["guid"] == d.release.guid);
            if !already {
                self.db.add_history(Some(title.id), "would_grab", summary)?;
            }
            return Ok(None);
        }
        if self.settings.servers().iter().all(|s| !s.enabled || s.host.is_empty()) {
            bail!("no usenet server is configured");
        }
        if !self.volume_ok() {
            bail!("the media volume is not mounted");
        }
        // Asking twice for the same release gets the download already under way, not a second one.
        let already = |app: &App| -> Result<Option<Acquisition>> {
            Ok(app.db.title_acquisitions(title.id)?.into_iter().find(|a| a.state.is_active() && ((!a.release.guid.is_empty() && a.release.guid == d.release.guid) || a.release.title == d.release.title)))
        };
        if let Some(existing) = already(self)? {
            return Ok(Some(existing));
        }
        self.check_space(d.release.size, user)?;
        // A release already in the archive needs no indexer at all.
        let saved = match App::archive_id_from_link(&d.release.link) {
            Some(id) => self.archive_get(id)?,
            None => self.archive_find(title, &d.release.title)?,
        };
        let mut nzb = None;
        let mut from_archive = false;
        if let Some(n) = &saved {
            match self.archive_read(n.id) {
                Ok(bytes) => {
                    nzb = Some(bytes.into());
                    from_archive = true;
                }
                Err(e) => tracing::warn!(release = %d.release.title, error = %e, "saved NZB unreadable; asking the indexer"),
            }
        }
        let from_indexer = nzb.is_none() && !d.release.link.is_empty() && App::archive_id_from_link(&d.release.link).is_none();
        if from_indexer && !user && self.indexer_budgets().get(&d.release.indexer_id).is_some_and(|b| b.grabs_spent()) {
            bail!("{}'s download allowance for today is used up", d.release.indexer);
        }
        if from_indexer {
            let _ = self.db.count_indexer(d.release.indexer_id, 0, 1);
        }
        // Indexers stall now and then; one more try is cheap and usually enough.
        let mut last_error = String::from("the saved NZB could not be read and there is no indexer link for it");
        for attempt in 0..2 {
            if nzb.is_some() || d.release.link.is_empty() || App::archive_id_from_link(&d.release.link).is_some() {
                break;
            }
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
            let fetched = async {
                let resp = self.http.get(&d.release.link).timeout(std::time::Duration::from_secs(150)).send().await?;
                if !resp.status().is_success() {
                    return Ok(Err(format!("{} answered HTTP {} for the NZB", d.release.indexer, resp.status().as_u16())));
                }
                resp.bytes().await.map(Ok)
            }
            .await;
            match fetched {
                Ok(Ok(bytes)) => {
                    nzb = Some(bytes);
                    break;
                }
                Ok(Err(status)) => {
                    last_error = status;
                    break;
                }
                Err(e) if e.is_timeout() => last_error = format!("{} did not send the NZB within 150 seconds", d.release.indexer),
                Err(e) => last_error = format!("could not fetch the NZB from {}: {}", d.release.indexer, e.without_url()),
            }
        }
        let Some(nzb) = nzb else { bail!("{last_error}") };
        // The fetch can take a while; a second request may have got there first.
        if let Some(existing) = already(self)? {
            return Ok(Some(existing));
        }
        // A look before the leap: a post that has mostly left the servers is not worth starting.
        // If the servers cannot be asked, the download itself will find out.
        if let Ok((found, asked)) = self.engine.availability(&nzb, 24).await {
            if asked > 0 && found * 100 < asked * 90 {
                let gone = Gone { found, asked };
                self.db.add_blocklist(title.id, &d.release.guid, &d.release.title, &gone.to_string())?;
                self.db.add_history(Some(title.id), "download_failed", serde_json::json!({"release": d.release.title, "indexer": d.release.indexer, "reason": gone.to_string()}))?;
                return Err(gone.into());
            }
        }
        let job_id = self.engine.add(&nzb, &d.release.title, None).map_err(|e| anyhow!("{}: {e}", d.release.indexer))?;
        // Hold it at once if the disk has no room yet, before a byte is fetched.
        self.watch_space();
        if !from_archive {
            if let Err(e) = self.archive_save(title, &d.release, d.quality, &d.covers, "grabbed", &nzb) {
                tracing::warn!(release = %d.release.title, error = %e, "NZB not saved to the archive");
            }
        }
        let mut acq = Acquisition {
            id: 0,
            title_id: title.id,
            episode_ids: d.episode_ids.clone(),
            state: AcqState::Downloading,
            release: d.release.clone(),
            quality: d.quality,
            job_id: Some(job_id),
            output_path: None,
            error: None,
            reason: reason.to_string(),
            replace_better: reason == COMPACT_REASON || reason == BY_HAND_REASON,
            created_at: 0,
            updated_at: 0,
        };
        self.db.save_acquisition(&mut acq)?;
        self.db.add_history(Some(title.id), "grabbed", summary)?;
        self.emit(Event::Acquisition { id: acq.id, title_id: title.id, state: acq.state.as_str().into() });
        Ok(Some(acq))
    }

    /// Refuse to start a download the disk cannot hold. A packed release needs room for the
    /// archive and for what comes out of it at the same time, and downloads already running have
    /// claimed their share.
    pub fn check_space(&self, release_bytes: u64, user: bool) -> Result<()> {
        let g = self.settings.general();
        let dir = self.engine.config().incomplete_dir;
        let Some(free) = crate::app::free_space(&dir) else { return Ok(()) };
        let gb = |b: u64| b as f64 / (1u64 << 30) as f64;
        let reserve = g.min_free_gb as u64 * (1 << 30);
        // Room to download it, plus half again while a packed release is unpacked.
        let alone = release_bytes + release_bytes / 2;
        if free < alone + reserve {
            bail!("not enough free space: {:.0} GB free, and this {:.0} GB release needs about {:.0} GB while unpacking, with {} GB kept free", gb(free), gb(release_bytes), gb(alone), g.min_free_gb);
        }
        // Otherwise it may join the queue and wait there for room. What Spool queues on its own
        // account is bounded, so a long backlog cannot pile up behind a full disk.
        if !user {
            let jobs = self.engine.jobs();
            let pending: u64 = jobs.iter().filter(|j| !j.state.is_terminal()).map(|j| j.total_bytes.saturating_sub(j.done_bytes)).sum();
            let cap = g.space_wait_gb as u64 * (1 << 30);
            if free < alone + reserve + pending && pending + release_bytes > free.saturating_sub(reserve) + cap {
                bail!("not enough free space: {:.0} GB free, {:.0} GB already queued, and this release is {:.0} GB", gb(free), gb(pending), gb(release_bytes));
            }
        }
        Ok(())
    }

    /// Download a release from the archive for the title it was saved for.
    pub async fn grab_saved(&self, id: i64) -> Result<Option<Acquisition>> {
        let n = self.archive_get(id)?.ok_or_else(|| anyhow!("no such saved release"))?;
        let title = self.db.find_title(n.kind, n.tmdb_id, n.tvdb_id)?.ok_or_else(|| anyhow!("{} is not in the library; add it first", n.name))?;
        let tc = self.title_context(title)?;
        let mut r = n.release.clone();
        r.link = App::archive_link(n.id);
        r.indexer = "Archive".into();
        r.indexer_id = -1;
        let j = self.judge(&tc, &r, "manual", None, true);
        self.db.record_decisions(std::slice::from_ref(&j.record))?;
        let d = self.db.decisions(tc.title.id)?.into_iter().find(|d| d.release.link == r.link).unwrap_or(j.record);
        self.grab(&d, &tc.title, "chosen from saved releases", true).await
    }

    /// Queue an NZB the user supplied for a known title.
    pub fn add_manual_nzb(&self, title: &Title, episode_ids: Vec<i64>, name: &str, nzb: &[u8]) -> Result<Acquisition> {
        if !self.is_active() {
            bail!("Spool is in shadow mode and does not download");
        }
        if !self.volume_ok() {
            bail!("the media volume is not mounted");
        }
        let quality = spool_core::quality::parse_quality(name, title.kind.flavor());
        let job_id = self.engine.add(nzb, name, None)?;
        let manual = Release { title: name.trim_end_matches(".nzb").to_string(), indexer: "Manual".into(), size: spool_nntp::nzb::parse(nzb).map(|n| n.bytes()).unwrap_or(0), ..Default::default() };
        let _ = self.archive_save(title, &manual, quality, "", "manual", nzb);
        let mut acq = Acquisition {
            id: 0,
            title_id: title.id,
            episode_ids,
            state: AcqState::Downloading,
            release: Release { guid: format!("manual:{job_id}"), title: name.trim_end_matches(".nzb").to_string(), indexer: "Manual".into(), ..Default::default() },
            quality,
            job_id: Some(job_id),
            output_path: None,
            error: None,
            reason: "added by hand".into(),
            replace_better: true,
            created_at: 0,
            updated_at: 0,
        };
        self.db.save_acquisition(&mut acq)?;
        self.emit(Event::Acquisition { id: acq.id, title_id: title.id, state: acq.state.as_str().into() });
        Ok(acq)
    }
}

/// The best acceptable releases that were not chosen, for saving as fallbacks.
fn runner_ups(judged: &[Judged], grabbed: &[Acquisition], profile: &QualityProfile, policy: decision::ProperPolicy, n: usize) -> Vec<DecisionRecord> {
    let mut accepted: Vec<&Judged> = judged.iter().filter(|j| j.record.accepted && !grabbed.iter().any(|g| g.release.title == j.record.release.title)).collect();
    accepted.sort_by(|a, b| decision::compare(&a.candidate, &b.candidate, profile, policy));
    accepted.into_iter().take(n).map(|j| j.record.clone()).collect()
}

/// Whether a release name with no year in it is plainly this film: it contains the film's whole
/// title (or a known alternative), and is not an episode of something. Short titles are left
/// out, since "Alien" is inside "Aliens" and a year is the only thing that tells them apart.
fn names_film_without_year(t: &Title, release: &str) -> bool {
    let name = spool_core::parser::clean_movie_title(release);
    let is_episode = parse_episode_title(release).is_some_and(|p| !p.episode_numbers.is_empty() || p.full_season);
    !is_episode
        && std::iter::once(&t.title).chain(t.alt_titles.iter()).any(|title| {
            let key = spool_core::parser::clean_movie_title(title);
            key.chars().count() >= 14 && title.split_whitespace().count() >= 3 && name.contains(&key)
        })
}

/// Choose what to grab from a judged batch.
///
/// A movie gets its single best release. For a series the aim is a season from as few sources
/// as possible: a season pack, or one group's run of single episodes, whichever covers the most
/// of what is wanted; quality decides between options that cover the same. Whatever is still
/// uncovered is then filled the same way.
fn pick<'a>(judged: &[&'a Judged], profile: &QualityProfile, policy: decision::ProperPolicy) -> Vec<&'a Judged> {
    let mut accepted: Vec<&'a Judged> = judged.iter().copied().filter(|j| j.record.accepted).collect();
    accepted.sort_by(|a, b| decision::compare(&a.candidate, &b.candidate, profile, policy));
    let mut out: Vec<&'a Judged> = vec![];
    if let Some(movie) = accepted.iter().find(|j| j.record.episode_ids.is_empty()) {
        out.push(movie);
    }

    // Releases over the profile's size target are a last resort: coverage is worked out from
    // those that fit first, and the oversized fill only what is still uncovered.
    let over = |j: &Judged| profile.size_ceiling().is_some_and(|c| j.candidate.size_bytes > c * j.candidate.item_count.max(1) as u64);
    let mut claimed: HashSet<i64> = HashSet::new();
    for oversized in [false, true] {
        pick_runs(&accepted.iter().copied().filter(|j| !j.record.episode_ids.is_empty() && over(j) == oversized).collect::<Vec<_>>(), &mut claimed, &mut out);
    }
    out
}

/// Cover as many unclaimed episodes as possible from as few sources as possible. `accepted` is
/// best first.
fn pick_runs<'a>(accepted: &[&'a Judged], claimed: &mut HashSet<i64>, out: &mut Vec<&'a Judged>) {
    // Options, each a list of releases in preference order.
    let mut options: Vec<Vec<&'a Judged>> = vec![];
    let mut runs: HashMap<(Option<String>, Quality), usize> = HashMap::new();
    for j in accepted.iter() {
        if j.record.episode_ids.len() > 1 {
            options.push(vec![j]);
        } else {
            let key = (j.record.release_group.as_ref().map(|g| g.to_lowercase()), j.record.quality.quality);
            let at = *runs.entry(key).or_insert_with(|| {
                options.push(vec![]);
                options.len() - 1
            });
            options[at].push(j);
        }
    }
    loop {
        // What each option would add: a pack only if none of it is taken, a run episode by episode.
        let gain = |opt: &Vec<&'a Judged>| -> usize {
            let mut seen: HashSet<i64> = HashSet::new();
            opt.iter().filter(|j| j.record.episode_ids.iter().all(|id| !claimed.contains(id))).flat_map(|j| j.record.episode_ids.iter().copied()).filter(|id| seen.insert(*id)).count()
        };
        // Options are in order of their best release, so the first of equal gain is the better one.
        let Some((best, _)) = options.iter().enumerate().map(|(i, o)| (i, gain(o))).filter(|(_, g)| *g > 0).max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))) else { break };
        for j in &options[best] {
            if j.record.episode_ids.iter().all(|id| !claimed.contains(id)) {
                claimed.extend(j.record.episode_ids.iter().copied());
                out.push(j);
            }
        }
    }
}

#[derive(serde::Serialize)]
pub struct SearchOutcome {
    pub decisions: Vec<DecisionRecord>,
    pub grabbed: Vec<Acquisition>,
    pub errors: Vec<String>,
    pub message: String,
}
