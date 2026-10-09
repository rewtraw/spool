use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use spool::app::App;
use spool::db::Db;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "spool", version, about = "Finds, downloads and organizes movies and television")]
struct Cli {
    /// Where Spool keeps its database, logs and backups.
    #[arg(long, global = true, env = "SPOOL_DATA_DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the service.
    Serve {
        #[arg(long, default_value = "0.0.0.0:7979", env = "SPOOL_BIND")]
        bind: String,
    },
    /// Import settings and catalog from Radarr, Sonarr and SABnzbd. Safe to run again.
    Migrate {
        #[arg(long)]
        radarr_url: Option<String>,
        #[arg(long, env = "RADARR_KEY")]
        radarr_key: Option<String>,
        #[arg(long)]
        sonarr_url: Option<String>,
        #[arg(long, env = "SONARR_KEY")]
        sonarr_key: Option<String>,
        /// Copy of radarr.db, for the indexer keys and Plex token the API hides.
        #[arg(long)]
        radarr_db: Option<PathBuf>,
        #[arg(long)]
        sonarr_db: Option<PathBuf>,
        /// Copy of sabnzbd.ini, for the usenet servers.
        #[arg(long)]
        sab_ini: Option<PathBuf>,
        /// Container path to host path, as FROM=TO. Repeatable.
        #[arg(long = "map")]
        map: Vec<String>,
    },
    /// Compare where every library file is with where Spool would put it.
    CheckPaths,
    /// Compare Spool's reading of releases with Radarr's and Sonarr's grab history.
    ShadowReport {
        #[arg(long)]
        radarr_url: Option<String>,
        #[arg(long, env = "RADARR_KEY")]
        radarr_key: Option<String>,
        #[arg(long)]
        sonarr_url: Option<String>,
        #[arg(long, env = "SONARR_KEY")]
        sonarr_key: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Download one NZB with the built-in engine, outside the library. For testing and benchmarks.
    Download {
        nzb: PathBuf,
        /// Folder for the working and finished files.
        #[arg(long)]
        out: PathBuf,
    },
    /// Write a consistent copy of the database.
    Backup { dest: PathBuf },
    /// Switch between shadow and active mode.
    Mode { mode: String },
}

fn data_dir(cli: &Cli) -> PathBuf {
    cli.data_dir.clone().unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join("Library/Application Support/Spool")
    })
}

fn pair(url: Option<String>, key: Option<String>) -> Option<(String, String)> {
    match (url, key) {
        (Some(u), Some(k)) => Some((u, k)),
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).with_target(false).init();
    let dir = data_dir(&cli);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let db = Db::open(&dir.join("spool.db"))?;

    match cli.command {
        Command::Serve { bind } => {
            let app = App::start(&dir, db).await?;
            // Ask for access to the media volume now, so any consent prompt appears at startup
            // rather than in the middle of the first import.
            let _ = app.volume_ok();
            let recovered = if app.volume_ok() { app.recover()? } else { 0 };
            if recovered > 0 {
                tracing::info!(recovered, "finished imports interrupted by the last shutdown");
            }
            if std::env::var_os("SPOOL_NO_SCHEDULER").is_none() {
                app.spawn_scheduler();
            }
            let listener = tokio::net::TcpListener::bind(&bind).await.with_context(|| format!("binding {bind}"))?;
            tracing::info!(%bind, mode = ?app.mode(), data = %dir.display(), "Spool is running");
            let engine = app.engine.clone();
            axum::serve(listener, spool::api::router(app))
                .with_graceful_shutdown(async move {
                    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = term.recv() => {}
                    }
                    tracing::info!("shutting down; saving download progress");
                    engine.shutdown().await;
                })
                .await?;
        }
        Command::Migrate { radarr_url, radarr_key, sonarr_url, sonarr_key, radarr_db, sonarr_db, sab_ini, map } => {
            let app = App::start(&dir, db).await?;
            let path_map = map.iter().filter_map(|m| m.split_once('=').map(|(a, b)| (a.to_string(), b.to_string()))).collect();
            let report = app.migrate(&spool::migrate::Options { radarr: pair(radarr_url, radarr_key), sonarr: pair(sonarr_url, sonarr_key), radarr_db, sonarr_db, sab_ini, path_map }).await?;
            println!("Imported {} movies, {} series, {} episodes, {} files, {} profiles, {} indexers, {} usenet servers.", report.movies, report.series, report.episodes, report.files, report.profiles, report.indexers, report.servers);
            for n in &report.notes {
                println!("note: {n}");
            }
            if report.unsupported.is_empty() {
                println!("Every setting found has an equivalent in Spool.");
            } else {
                println!("Not carried over ({}):", report.unsupported.len());
                for u in &report.unsupported {
                    println!("  - {u}");
                }
            }
        }
        Command::CheckPaths => {
            let app = App::start(&dir, db).await?;
            let (same, differ) = app.check_paths()?;
            println!("{same} files are exactly where Spool would put them; {} differ.", differ.len());
            for (actual, expected) in &differ {
                println!("  on disk:  {actual}\n  expected: {expected}\n");
            }
            if !differ.is_empty() {
                std::process::exit(1);
            }
        }
        Command::ShadowReport { radarr_url, radarr_key, sonarr_url, sonarr_key, json } => {
            let app = App::start(&dir, db).await?;
            let r = app.shadow_report(pair(radarr_url, radarr_key), pair(sonarr_url, sonarr_key)).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                for (name, s) in [("Movies (vs Radarr)", &r.movies), ("Television (vs Sonarr)", &r.tv)] {
                    println!("{name}: {} grabs examined; same title {} (+{} by indexer id), same quality {}; of {} automatic grabs, {} allowed by Spool's profile; {} manual grabs not compared", s.examined, s.same_title, s.matched_by_id, s.same_quality, s.automatic, s.quality_allowed, s.manual);
                    for d in &s.disagreements {
                        println!("  [{}] {}\n      theirs: {}\n      ours:   {}", d.kind, d.release, d.theirs, d.ours);
                    }
                }
                println!("Would have grabbed while shadowing: {}", r.would_grab.len());
                for w in &r.would_grab {
                    println!("  {} {}", w["release"].as_str().unwrap_or(""), w["covers"].as_str().unwrap_or(""));
                }
            }
        }
        Command::Download { nzb, out } => {
            let settings = spool::settings::Settings::new(db);
            let cfg = spool_nntp::EngineConfig { servers: settings.servers(), incomplete_dir: out.join("incomplete"), complete_dir: out.join("complete"), ..Default::default() };
            if cfg.servers.is_empty() {
                anyhow::bail!("no usenet servers are configured");
            }
            let data = std::fs::read(&nzb).with_context(|| format!("reading {}", nzb.display()))?;
            let name = nzb.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "download".into());
            let engine = spool_nntp::Engine::start(cfg).await?;
            let id = engine.add(&data, &name, None)?;
            let started = std::time::Instant::now();
            let mut download_secs = 0.0;
            let mut last_state = String::new();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let Some(j) = engine.job(&id) else { anyhow::bail!("the job disappeared") };
                let state = format!("{:?}", j.state);
                if state != last_state {
                    if last_state == "Downloading" {
                        download_secs = started.elapsed().as_secs_f64();
                    }
                    println!("[{:>6.1}s] {state}: {}", started.elapsed().as_secs_f64(), j.message);
                    last_state = state;
                }
                if j.state == spool_nntp::JobState::Downloading {
                    println!("[{:>6.1}s]   {:.1}% at {:.1} MB/s", started.elapsed().as_secs_f64(), j.done_bytes as f64 * 100.0 / j.total_bytes.max(1) as f64, j.speed as f64 / 1048576.0);
                }
                if j.state.is_terminal() {
                    let total = started.elapsed().as_secs_f64();
                    println!("Finished: {:?} in {total:.1}s ({:.1}s downloading, {:.1} MB/s average over {:.0} MB); missing articles {}, damaged {}", j.state, download_secs, j.total_bytes as f64 / 1048576.0 / download_secs.max(0.1), j.total_bytes as f64 / 1048576.0, j.missing_articles, j.damaged_articles);
                    println!("Unpacked while downloading: {}", j.unpacked_directly);
                    if let Some(e) = j.error {
                        println!("Error: {e}");
                    }
                    if let Some(p) = j.output_path {
                        println!("Output: {}", p.display());
                    }
                    break;
                }
            }
        }
        Command::Backup { dest } => {
            db.backup(&dest)?;
            println!("Wrote {}", dest.display());
        }
        Command::Mode { mode } => {
            let settings = spool::settings::Settings::new(db);
            let mut g = settings.general();
            g.mode = match mode.as_str() {
                "active" => spool::settings::Mode::Active,
                "shadow" => spool::settings::Mode::Shadow,
                other => anyhow::bail!("unknown mode {other}; use shadow or active"),
            };
            settings.set_general(&g)?;
            println!("Mode is now {mode}. Restart the service for it to take effect.");
        }
    }
    Ok(())
}
