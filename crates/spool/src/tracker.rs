//! Follows downloads from the engine into the library, and handles the ones that fail.

use crate::acquire::Scope;
use crate::app::{App, Event};
use crate::models::*;
use anyhow::Result;
use spool_nntp::{JobState, JobStatus};

impl App {
    /// Bring one acquisition in line with its download job.
    pub async fn reconcile(&self, acq: &mut Acquisition, job: Option<JobStatus>) -> Result<()> {
        if acq.state != AcqState::Downloading {
            return Ok(());
        }
        match job {
            // Most often a downloader that restarted and could not read its record. The release
            // is not at fault: it stays choosable, and the search may well take it again.
            None => self.fail_acquisition(acq, "the download disappeared from the queue", false).await,
            Some(j) if j.state == JobState::Failed => {
                let why = j.error.unwrap_or_else(|| "the download failed".into());
                if why.contains("could not write to disk") {
                    // The release is not at fault. If the disk went away, wait for it; the volume
                    // watcher retries the job when it returns.
                    if !self.volume_ok() {
                        return Ok(());
                    }
                    if let Some(id) = &acq.job_id {
                        let _ = self.job_remove(id, true);
                    }
                    acq.state = AcqState::Failed;
                    acq.error = Some(why.clone());
                    self.db.save_acquisition(acq)?;
                    self.db.add_attention("download_failed", Some(acq.title_id), Some(acq.id), &format!("A download stopped because the disk could not be written: {why}"), serde_json::json!({"release": acq.release.title}))?;
                    self.emit(Event::Acquisition { id: acq.id, title_id: acq.title_id, state: acq.state.as_str().into() });
                    self.emit(Event::Attention);
                    return Ok(());
                }
                if let Some(id) = &acq.job_id {
                    let _ = self.job_remove(id, true);
                }
                self.fail_acquisition(acq, &why, true).await
            }
            Some(j) if j.state == JobState::Completed => {
                acq.output_path = j.output_path.map(|p| p.to_string_lossy().to_string());
                self.db.save_acquisition(acq)?;
                self.import(acq.id, None).await
            }
            Some(_) => Ok(()),
        }
    }

    /// Record a failed download and look for another. A release at fault is kept from being chosen again.
    pub async fn fail_acquisition(&self, acq: &mut Acquisition, why: &str, release_at_fault: bool) -> Result<()> {
        acq.state = AcqState::Failed;
        acq.error = Some(why.to_string());
        self.db.save_acquisition(acq)?;
        if release_at_fault {
            self.db.add_blocklist(acq.title_id, &acq.release.guid, &acq.release.title, why)?;
        }
        self.db.add_history(Some(acq.title_id), "download_failed", serde_json::json!({"release": acq.release.title, "indexer": acq.release.indexer, "reason": why}))?;
        self.emit(Event::Acquisition { id: acq.id, title_id: acq.title_id, state: acq.state.as_str().into() });
        tracing::warn!(release = %acq.release.title, reason = %why, "download failed");

        if self.is_active() && !acq.release.guid.starts_with("manual:") {
            let (app, title_id, episodes) = (self.clone(), acq.title_id, acq.episode_ids.clone());
            let (release, reason) = (acq.release.title.clone(), why.to_string());
            tokio::spawn(async move {
                let scope = match (app.db.title(title_id), episodes.as_slice()) {
                    (Ok(Some(t)), _) if t.kind == Kind::Movie => Scope::Movie,
                    (_, [one]) => Scope::Episode(*one),
                    _ => Scope::Missing,
                };
                match app.search(title_id, scope, false, true).await {
                    Ok(o) if o.grabbed.is_empty() => {
                        // Name the episodes: other downloads for the same series may well succeed.
                        let eps: Vec<Episode> = episodes.iter().filter_map(|id| app.db.episode(*id).ok().flatten()).collect();
                        let mut labels: Vec<String> = eps.iter().map(|e| format!("S{:02}E{:02}", e.season, e.episode)).collect();
                        labels.sort();
                        let what = match labels.len() {
                            0 => "The download".to_string(),
                            1..=3 => format!("The download of {}", labels.join(", ")),
                            n if eps.iter().all(|e| e.season == eps[0].season) => format!("The download of season {} ({n} episodes)", eps[0].season),
                            n => format!("The download of {n} episodes"),
                        };
                        // Say which it was: nothing else acceptable, or something found that could not be started.
                        let why = if o.message.starts_with("Could not start") {
                            format!("{what} failed. Replacements were found, but: {}.{}", lower_first(&o.message), if o.message.contains("free space") { " Spool tries again on its next search for missing titles." } else { "" })
                        } else {
                            let every = app.settings.general().backlog_interval_minutes.max(60);
                            format!("{what} failed and no other acceptable release was found. Spool keeps looking in its search for what is missing, every {}", crate::names::span_minutes(every as i64))
                        };
                        let _ = app.db.add_attention("download_failed", Some(title_id), None, &why, serde_json::json!({"release": release, "reason": reason, "episode_ids": episodes}));
                        app.emit(Event::Attention);
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "search after a failed download did not run"),
                }
            });
        }
        Ok(())
    }

    /// Pause downloading while the media volume is away, and pick up where it stopped on return.
    pub fn watch_volume(&self, was_ok: &mut bool) {
        // A refusal may have been reversed in System Settings since the last look.
        let _ = self.volume_access.compare_exchange(3, 0, std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed);
        let ok = self.volume_ok();
        if *was_ok != ok {
            // Its titles come and go with it, for anything that keeps a picture of the library.
            self.location_changes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.emit(crate::app::Event::Locations);
        }
        if *was_ok && !ok {
            tracing::warn!(reason = %self.volume_problem().unwrap_or_default(), "the media volume cannot be used; pausing downloads");
            self.engine.pause_all();
        } else if !*was_ok && ok {
            tracing::info!("the media volume is back; resuming downloads");
            if let Err(e) = self.recover() {
                tracing::warn!(error = %e, "could not finish interrupted imports");
            }
            for job in self.engine.jobs() {
                if job.state == JobState::Failed && job.error.as_deref().is_some_and(|e| e.contains("could not write to disk")) {
                    let _ = self.engine.retry(&job.id);
                }
            }
            self.engine.resume_all();
        }
        *was_ok = ok;
    }

    /// Hold back queued downloads the disk cannot take, and release them when it can.
    ///
    /// Walks the queue in order, giving each job the space it still needs. A job that does not
    /// fit in what is left, after the reserve, waits; so does everything behind it that does not
    /// fit either. Jobs the user paused are left alone.
    pub fn watch_space(&self) {
        const WAITING: &str = "Waiting for free space";
        let g = self.settings.general();
        // Jobs the user resumed by hand are theirs to run, whatever the arithmetic says.
        let overridden = self.waiting_for_space.lock().clone();
        let (mut waiting, mut waiting_bytes, mut short, mut free) = (0u32, 0u64, 0i128, 0u64);
        // Each place downloads run has its own disk and its own queue: this machine, and
        // every helper.
        let mut places: Vec<(Option<u64>, Vec<JobStatus>)> = vec![(crate::app::free_space(&self.engine.config().incomplete_dir), self.engine.jobs())];
        places.extend(self.remotes().iter().filter(|r| r.seen().reachable).map(|r| (r.seen().free, r.jobs())));
        for (room, jobs) in places {
            let Some(room) = room else { continue };
            let mut budget = room.saturating_sub(g.min_free_gb as u64 * (1 << 30)) as i128;
            let (before, mut wanted) = (waiting, 0i128);
            for job in jobs {
                // The hold is recognised by its message, so it survives a restart of Spool.
                let held = job.state == JobState::Paused && job.message == WAITING;
                let remaining = job.total_bytes.saturating_sub(job.done_bytes) as i128;
                match job.state {
                    JobState::Verifying | JobState::Repairing | JobState::Extracting | JobState::Finishing => budget -= job.total_bytes as i128,
                    // A download under way is held too if the disk cannot take the rest of it; stopping
                    // costs nothing, and running the disk full costs every other job.
                    JobState::Queued | JobState::Downloading | JobState::Paused if job.state != JobState::Paused || held => {
                        if overridden.contains(&job.id) {
                            budget -= remaining;
                            continue;
                        }
                        // Room to download it, plus half again while a packed release is unpacked.
                        let needed = remaining + (job.total_bytes / 2) as i128;
                        if needed <= budget {
                            budget -= remaining;
                            if held {
                                let _ = self.job_resume(&job.id);
                            }
                        } else {
                            waiting += 1;
                            waiting_bytes += job.total_bytes;
                            wanted += needed;
                            if !held && self.job_hold(&job.id, WAITING).is_ok() {
                                tracing::info!(job = %job.name, "holding a download until there is room for it");
                            }
                        }
                    }
                    _ => {}
                }
            }
            if waiting > before {
                free += room;
                short += wanted - budget.max(0);
            }
        }
        // One standing notice for however many are waiting, kept current and closed when none are.
        let gb = |b: i128| (b.max(0) as f64 / (1u64 << 30) as f64).ceil();
        let message = (waiting > 0).then(|| {
            format!(
                "{waiting} {} ({:.0} GB) {} waiting for free space. {:.0} GB is free; freeing about {:.0} GB more would let {} run.",
                if waiting == 1 { "download" } else { "downloads" },
                gb(waiting_bytes as i128),
                if waiting == 1 { "is" } else { "are" },
                gb(free as i128),
                gb(short),
                if waiting == 1 { "it" } else { "them all" }
            )
        });
        if self.db.set_standing_attention("space", message.as_deref(), serde_json::json!({"waiting": waiting})).unwrap_or(false) {
            self.emit(Event::Attention);
        }
    }

    /// Check every active acquisition against the engine.
    pub async fn reconcile_all(&self) -> Result<()> {
        for mut acq in self.db.active_acquisitions()? {
            match acq.state {
                AcqState::Downloading => {
                    let job = match acq.job_id.as_deref().map(|id| self.job_lookup(id)) {
                        Some(crate::remote::Found::Job(j)) => Some(j),
                        // It runs on a helper that cannot be asked right now. It is waited
                        // for, not written off.
                        Some(crate::remote::Found::Unknown) => continue,
                        _ => None,
                    };
                    if let Err(e) = self.reconcile(&mut acq, job).await {
                        tracing::warn!(acquisition = acq.id, error = %e, "reconcile failed");
                    }
                }
                // An import that was cut short by a restart, or that waited for the media volume.
                AcqState::Importing if !self.importing.lock().contains(&acq.id) => {
                    if let Err(e) = self.import(acq.id, None).await {
                        tracing::warn!(acquisition = acq.id, error = %e, "import failed");
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Pass a download's progress on to whoever is watching, and file it when it is done.
    pub fn on_job(&self, job: JobStatus) {
        let terminal = job.state.is_terminal();
        let id = job.id.clone();
        self.emit(Event::Job { job: job.clone() });
        if terminal {
            if let Ok(Some(mut acq)) = self.db.acquisition_by_job(&id) {
                let app = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = app.reconcile(&mut acq, Some(job)).await {
                        tracing::warn!(error = %e, "could not finish a download");
                    }
                });
            }
        }
    }

    /// Follow downloads as they happen, here and on every helper.
    pub fn spawn_tracker(&self) {
        let app = self.clone();
        let mut rx = self.engine.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(job) => app.on_job(job),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
        self.spawn_remotes();
    }

    /// Stop a download and forget it. The release is not blocklisted: the user chose to stop.
    pub async fn cancel_acquisition(&self, id: i64, blocklist: bool) -> Result<()> {
        let Some(mut acq) = self.db.acquisition(id)? else { return Ok(()) };
        if let Some(job) = &acq.job_id {
            let _ = self.job_remove(job, true);
        }
        if let Some(out) = &acq.output_path {
            self.remove_download_folder(acq.job_id.as_deref(), std::path::Path::new(out)).await;
        }
        acq.state = AcqState::Cancelled;
        self.db.save_acquisition(&mut acq)?;
        if blocklist {
            self.db.add_blocklist(acq.title_id, &acq.release.guid, &acq.release.title, "removed by hand")?;
        }
        self.db.resolve_attention_for(acq.id)?;
        self.emit(Event::Acquisition { id: acq.id, title_id: acq.title_id, state: acq.state.as_str().into() });
        self.emit(Event::Attention);
        Ok(())
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_lowercase().collect::<String>() + c.as_str()).unwrap_or_default()
}
