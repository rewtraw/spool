//! Background work on a timer.

use crate::acquire::Scope;
use crate::app::App;
use crate::db::now;
use crate::models::Kind;
use std::time::Duration;

impl App {
    /// Run a named task unless it is already running.
    pub async fn run_task(&self, name: &str) -> String {
        if !self.task_start(name, "Running") {
            return "already running".into();
        }
        let result: anyhow::Result<String> = match name {
            "rss" => self.rss_sync().await,
            "backlog" => self.backlog().await,
            "refresh" => self.refresh_due(true).await,
            "scan" => self.scan_all().await.map(|r| format!("{} files on disk, {} added, {} missing, {} unmatched", r.files_found, r.files_added, r.files_missing, r.unmatched.len())),
            "housekeeping" => self.housekeeping().await,
            "compact" => self.compact_library().await,
            "subtitles" => self.subtitle_sweep().await,
            "plex" if self.settings.general().plex_token.is_empty() => Ok("skipped: Plex is not set up".into()),
            "plex" => self.plex_sync().await,
            other => Err(anyhow::anyhow!("unknown task {other}")),
        };
        let message = match result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(task = name, error = %e, "task failed");
                format!("Failed: {e}")
            }
        };
        self.task_end(name, &message);
        message
    }

    /// Search for a few missing titles, least recently searched first.
    async fn backlog(&self) -> anyhow::Result<String> {
        if !self.is_active() {
            return Ok("skipped in shadow mode".into());
        }
        let g = self.settings.general();
        let files = self.db.file_counts()?;
        let today = chrono::Local::now().date_naive();
        let nowt = chrono::Utc::now();
        let mut wanted = vec![];
        for t in self.db.titles(None)? {
            if !t.monitored {
                continue;
            }
            let missing = match t.kind {
                Kind::Movie => !files.contains_key(&t.id) && t.movie_available(today),
                Kind::Series => self.db.episodes(t.id)?.iter().any(|e| e.season > 0 && e.monitored && t.season_monitored(e.season) && e.file_id.is_none() && e.has_aired(nowt)),
            };
            let busy = self.db.title_acquisitions(t.id)?.iter().any(|a| a.state.is_active());
            if missing && !busy {
                wanted.push(t);
            }
        }
        // Titles already turned away for space would only be turned away again.
        let before = wanted.len();
        wanted.retain(|t| !self.still_short_of_space(t.id));
        let short = before - wanted.len();
        wanted.sort_by_key(|t| t.last_search_at);
        let total = wanted.len();
        let mut grabbed = 0;
        let batch: Vec<_> = wanted.into_iter().take(g.backlog_batch as usize).collect();
        let searched = batch.len();
        for t in batch {
            let scope = if t.kind == Kind::Movie { Scope::Movie } else { Scope::Missing };
            match self.search(t.id, scope, false, true).await {
                Ok(o) => grabbed += o.grabbed.len(),
                Err(e) => tracing::warn!(title = %t.title, error = %e, "backlog search failed"),
            }
        }
        Ok(format!("{} titles missing, searched {searched}, grabbed {grabbed}{}", total + short, if short > 0 { format!(", {short} waiting for disk space") } else { String::new() }))
    }

    /// Look for smaller copies of the titles furthest over their profile's size target. Only ever
    /// run by hand: it replaces files, and a few titles at a time keeps it within what the disk
    /// and the indexers can take.
    async fn compact_library(&self) -> anyhow::Result<String> {
        const PER_RUN: usize = 8;
        if !self.is_active() {
            return Ok("skipped in shadow mode".into());
        }
        let profiles = self.db.profiles()?;
        let mut over: Vec<(u64, crate::models::Title)> = vec![];
        for t in self.db.titles(None)? {
            let Some(ceiling) = profiles.iter().find(|p| p.id == t.profile_id).and_then(|p| p.size_ceiling()) else { continue };
            let excess: u64 = self.db.files(t.id)?.iter().map(|f| f.size.saturating_sub(ceiling)).sum();
            if excess > 0 && !self.db.title_acquisitions(t.id)?.iter().any(|a| a.state.is_active()) {
                over.push((excess, t));
            }
        }
        over.sort_by_key(|o| std::cmp::Reverse(o.0));
        let total = over.len();
        let (mut searched, mut started) = (0, 0);
        for (_, t) in over.into_iter().filter(|o| !self.still_short_of_space(o.1.id)).take(PER_RUN) {
            searched += 1;
            match self.search(t.id, Scope::Compact, false, true).await {
                Ok(o) => started += o.grabbed.len(),
                Err(e) => tracing::warn!(title = %t.title, error = %e, "compacting search failed"),
            }
        }
        let left = total.saturating_sub(searched);
        Ok(format!("{total} titles over their size target, searched {searched}, {started} smaller {} started{}", if started == 1 { "copy" } else { "copies" }, if left > 0 { format!("; run again for the other {left}") } else { String::new() }))
    }

    /// Refresh metadata that is due: continuing series daily, everything else weekly.
    async fn refresh_due(&self, force: bool) -> anyhow::Result<String> {
        let mut n = 0;
        for t in self.db.titles(None)? {
            let age = now() - t.last_refresh_at;
            let due = match (t.kind, t.status.as_str()) {
                (Kind::Series, "ended") => age > 30 * 86400,
                (Kind::Series, _) => age > 20 * 3600,
                (Kind::Movie, "released") => age > 30 * 86400,
                (Kind::Movie, _) => age > 3 * 86400,
            };
            if due || (force && age > 3600) {
                if let Err(e) = self.refresh_title(t.id).await {
                    tracing::debug!(title = %t.title, error = %e, "refresh failed");
                } else {
                    n += 1;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        }
        Ok(format!("refreshed {n} titles"))
    }

    async fn housekeeping(&self) -> anyhow::Result<String> {
        let recycled = self.clean_recycle(false).unwrap_or(0) / (1 << 20);
        let dir = self.data_dir.join("backups");
        std::fs::create_dir_all(&dir)?;
        let name = format!("spool-{}.db", chrono::Local::now().format("%Y%m%d"));
        self.db.backup(&dir.join(&name))?;
        let mut backups: Vec<_> = std::fs::read_dir(&dir)?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "db")).collect();
        backups.sort();
        while backups.len() > 14 {
            let _ = std::fs::remove_file(backups.remove(0));
        }
        // A copy on another disk is what survives this one failing.
        let second = self.settings.general().backup_dir.trim().to_string();
        let mut copied = String::new();
        if !second.is_empty() {
            let dest = std::path::PathBuf::from(&second);
            match std::fs::create_dir_all(&dest).and_then(|_| std::fs::copy(dir.join(&name), dest.join(&name))) {
                Ok(_) => {
                    let mut old: Vec<_> = std::fs::read_dir(&dest)?.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("spool-")) && p.extension().is_some_and(|e| e == "db")).collect();
                    old.sort();
                    while old.len() > 14 {
                        let _ = std::fs::remove_file(old.remove(0));
                    }
                    copied = format!(" and copied to {second}");
                }
                Err(e) => {
                    tracing::warn!(folder = %second, error = %e, "could not copy the backup to the second folder");
                    copied = format!("; the copy to {second} failed: {e}");
                }
            }
        }
        self.trim_log();
        self.db.with(|c| c.execute("DELETE FROM history WHERE ts < ?1 AND kind = 'would_grab'", [now() - 60 * 86400]))?;
        Ok(format!("backup {name} written{copied}, {recycled} MB of recycled files deleted"))
    }

    /// Keep the service log from growing for ever where the system writes it for us and never
    /// trims it: past 20 MB, the last few megabytes are kept and the rest dropped.
    fn trim_log(&self) {
        const LIMIT: u64 = 20 << 20;
        const KEEP: u64 = 4 << 20;
        if crate::app::own_log_path(&self.data_dir).is_some() {
            return; // Spool writes this one itself and rolls it over at start.
        }
        let path = crate::app::log_path(&self.data_dir);
        let Ok(len) = std::fs::metadata(&path).map(|m| m.len()) else { return };
        if len <= LIMIT {
            return;
        }
        use std::io::{Read, Seek, SeekFrom, Write};
        let tail = (|| -> std::io::Result<Vec<u8>> {
            let mut f = std::fs::File::open(&path)?;
            f.seek(SeekFrom::Start(len - KEEP))?;
            let mut buf = Vec::with_capacity(KEEP as usize);
            f.read_to_end(&mut buf)?;
            // Start at a whole line.
            let cut = buf.iter().position(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0);
            Ok(buf.split_off(cut))
        })();
        // The system holds the file open for appending, so it is emptied in place, not replaced.
        if let Ok(tail) = tail {
            if let Ok(mut f) = std::fs::OpenOptions::new().write(true).truncate(true).open(&path) {
                let _ = f.write_all(&tail);
                tracing::info!("trimmed the log to its last {} MB", KEEP >> 20);
            }
        }
    }

    pub fn spawn_scheduler(&self) {
        // Downloads: react to events, and sweep periodically in case one was missed.
        self.spawn_tracker();
        let app = self.clone();
        tokio::spawn(async move {
            let mut volume_was_ok = true;
            loop {
                app.watch_volume(&mut volume_was_ok);
                if app.volume_ok() {
                    app.watch_space();
                }
                if let Err(e) = app.reconcile_all().await {
                    tracing::warn!(error = %e, "reconcile sweep failed");
                }
                tokio::time::sleep(Duration::from_secs(20)).await;
            }
        });

        let app = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let n = app.warm_art().await;
            if n > 0 {
                tracing::info!(posters = n, "cached artwork");
            }
        });

        let app = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(20)).await;
            let mut last: std::collections::HashMap<&str, i64> = Default::default();
            loop {
                let g = app.settings.general();
                let due = |name: &str, every_minutes: i64, last: &std::collections::HashMap<&str, i64>| now() - last.get(name).copied().unwrap_or(0) >= every_minutes * 60;
                for (name, every) in [
                    ("rss", g.rss_interval_minutes.max(10) as i64),
                    ("backlog", g.backlog_interval_minutes.max(60) as i64),
                    ("refresh", 6 * 60),
                    ("housekeeping", 24 * 60),
                    ("plex", 60),
                    ("subtitles", 6 * 60),
                ] {
                    if name == "subtitles" && !g.subtitles_auto {
                        continue;
                    }
                    if due(name, every, &last) {
                        last.insert(name, now());
                        if name == "refresh" {
                            if app.task_start("refresh", "Running") {
                                let m = app.refresh_due(false).await.unwrap_or_else(|e| format!("Failed: {e}"));
                                app.task_end("refresh", &m);
                            }
                        } else {
                            app.run_task(name).await;
                        }
                    }
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
    }
}
