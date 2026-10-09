//! End-to-end engine tests against an in-process NNTP server.

use spool_nntp::testing::{bytes, FakeServer, Post};
use spool_nntp::{Engine, EngineConfig, JobState, JobStatus};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--help").output().is_ok()
}

fn cfg(dir: &Path, servers: Vec<spool_nntp::ServerConfig>) -> EngineConfig {
    EngineConfig { servers, incomplete_dir: dir.join("incomplete"), complete_dir: dir.join("complete"), ..Default::default() }
}

async fn wait_done(engine: &Engine, id: &str) -> JobStatus {
    for _ in 0..600 {
        if let Some(s) = engine.job(id) {
            if s.state.is_terminal() {
                return s;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("job did not finish: {:?}", engine.job(id));
}

fn read(status: &JobStatus, name: &str) -> Vec<u8> {
    std::fs::read(status.output_path.as_ref().expect("output path").join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn names(status: &JobStatus) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(status.output_path.as_ref().unwrap()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    v.sort();
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_a_multi_file_job() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (bytes(1, 700_000), bytes(2, 150_001));
    let mut post = Post::new();
    post.add_file("Some.Movie.2020.1080p.mkv", &a, 50_000);
    post.add_file("Some.Movie.2020.1080p.nfo", &b, 50_000);
    let server = FakeServer::start(post.articles.clone()).await;
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 6)])).await.unwrap();

    let id = engine.add(&post.nzb(), "Some.Movie.2020.1080p.nzb", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert_eq!(done.missing_articles, 0);
    assert_eq!(read(&done, "Some.Movie.2020.1080p.mkv"), a);
    assert_eq!(read(&done, "Some.Movie.2020.1080p.nfo"), b);
    assert!(done.output_path.unwrap().ends_with("complete/Some.Movie.2020.1080p"));
    assert_eq!(server.max_served_per_article(), 1, "no article should be fetched twice");
    assert!(server.connections.load(std::sync::atomic::Ordering::SeqCst) <= 6);
}

#[tokio::test(flavor = "multi_thread")]
async fn falls_back_to_second_provider_for_missing_and_corrupt_articles() {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(3, 400_000);
    let mut post = Post::new();
    let ids = post.add_file("file.bin", &data, 20_000);
    let primary = FakeServer::start(post.articles.clone()).await;
    let backup = FakeServer::start(post.articles.clone()).await;
    {
        let mut s = primary.state.lock();
        s.missing.insert(ids[3].clone());
        s.missing.insert(ids[11].clone());
        // corrupt one article on the primary: flip a byte in the middle
        let art = s.articles.get_mut(&ids[7]).unwrap();
        let mid = art.len() / 2;
        art[mid] = art[mid].wrapping_add(1);
    }
    let engine = Engine::start(cfg(tmp.path(), vec![primary.config("primary", 0, 4), backup.config("backup", 1, 2)])).await.unwrap();
    let id = engine.add(&post.nzb(), "job", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert_eq!((done.missing_articles, done.damaged_articles), (0, 0));
    assert_eq!(read(&done, "file.bin"), data);
    assert_eq!(backup.total_served(), 3, "the backup is asked only for what the primary could not supply");
}

#[tokio::test(flavor = "multi_thread")]
async fn survives_dropped_connections() {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(4, 600_000);
    let mut post = Post::new();
    post.add_file("file.bin", &data, 10_000);
    let server = FakeServer::start(post.articles.clone()).await;
    server.state.lock().drop_after = Some(9);
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 3)])).await.unwrap();
    let id = engine.add(&post.nzb(), "job", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert_eq!(read(&done, "file.bin"), data);
}

#[tokio::test(flavor = "multi_thread")]
async fn resumes_after_clean_shutdown_without_refetching() {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(5, 2_000_000);
    let mut post = Post::new();
    post.add_file("big.bin", &data, 10_000);
    let server = FakeServer::start(post.articles.clone()).await;
    server.state.lock().delay_ms = 4;
    let config = cfg(tmp.path(), vec![server.config("main", 0, 4)]);

    let engine = Engine::start(config.clone()).await.unwrap();
    let id = engine.add(&post.nzb(), "job", None).unwrap();
    loop {
        let s = engine.job(&id).unwrap();
        if s.done_bytes * 4 > s.total_bytes {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    engine.shutdown().await;
    let before = server.total_served();
    assert!(before > 20 && before < 200, "should stop part way, served {before}");

    let engine = Engine::start(config).await.unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert_eq!(read(&done, "big.bin"), data);
    assert_eq!(server.max_served_per_article(), 1, "finished segments must not be fetched again");
    assert_eq!(server.total_served(), 200);
}

#[test]
fn recovers_from_a_crash_mid_download() {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(6, 2_000_000);
    let mut post = Post::new();
    post.add_file("big.bin", &data, 10_000);
    let nzb = post.nzb();

    // First process: start downloading, then vanish without any shutdown.
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
    let (server, config, id) = rt.block_on(async {
        let server = FakeServer::start(post.articles.clone()).await;
        server.state.lock().delay_ms = 4;
        let config = cfg(tmp.path(), vec![server.config("main", 0, 4)]);
        let engine = Engine::start(config.clone()).await.unwrap();
        let id = engine.add(&nzb, "job", None).unwrap();
        loop {
            let s = engine.job(&id).unwrap();
            if s.done_bytes * 3 > s.total_bytes {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        (server, config, id)
    });
    let articles = post.articles.clone();
    drop(server);
    rt.shutdown_background();

    // Second process.
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
    rt.block_on(async {
        let server = FakeServer::start(articles).await;
        let mut config = config;
        config.servers = vec![server.config("main", 0, 4)];
        let engine = Engine::start(config).await.unwrap();
        let done = wait_done(&engine, &id).await;
        assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
        assert_eq!(read(&done, "big.bin"), data, "data written before the crash must be intact or refetched");
    });
}

/// Build a PAR2 set for `files` in `dir` and return (name, bytes) for every par2 file created.
fn make_par2(dir: &Path, base: &str, files: &[&str], redundancy: u32, volumes: u32) -> Vec<(String, Vec<u8>)> {
    let ok = Command::new("par2")
        .args(["create", "-q", "-q", "-s20000", &format!("-r{redundancy}"), &format!("-n{volumes}"), &format!("{base}.par2")])
        .args(files)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(ok.success());
    let mut out = vec![];
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.ends_with(".par2") {
            out.push((n, std::fs::read(e.path()).unwrap()));
        }
    }
    out.sort();
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn repairs_a_damaged_job_fetching_only_the_recovery_it_needs() {
    if !have("par2") {
        eprintln!("par2 not installed; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let data = bytes(7, 1_000_000);
    std::fs::write(src.join("movie.mkv"), &data).unwrap();
    let pars = make_par2(&src, "movie", &["movie.mkv"], 20, 4);

    let mut post = Post::new();
    let ids = post.add_file("movie.mkv", &data, 20_000);
    let mut par_ids = vec![];
    for (name, body) in &pars {
        par_ids.push((name.clone(), post.add_file(name, body, 20_000)));
    }
    let server = FakeServer::start(post.articles.clone()).await;
    for i in [5usize, 6, 30] {
        server.state.lock().missing.insert(ids[i].clone());
    }
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 4)])).await.unwrap();
    let id = engine.add(&post.nzb(), "Movie.Job", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert_eq!(done.missing_articles, 3);
    assert_eq!(read(&done, "movie.mkv"), data, "repair must restore the original bytes");
    assert_eq!(names(&done), ["movie.mkv"], "recovery files are cleaned up");

    // Not every recovery volume should have been needed.
    let served = server.state.lock().served.clone();
    let volumes_touched = par_ids.iter().filter(|(n, ids)| n.contains(".vol") && ids.iter().any(|i| served.contains_key(i))).count();
    let volumes_total = par_ids.iter().filter(|(n, _)| n.contains(".vol")).count();
    assert!(volumes_touched >= 1 && volumes_touched < volumes_total, "fetched {volumes_touched} of {volumes_total} volumes");
}

#[tokio::test(flavor = "multi_thread")]
async fn fails_cleanly_when_damage_exceeds_recovery_data() {
    if !have("par2") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let data = bytes(8, 1_000_000);
    std::fs::write(src.join("movie.mkv"), &data).unwrap();
    let pars = make_par2(&src, "movie", &["movie.mkv"], 5, 1);
    let mut post = Post::new();
    let ids = post.add_file("movie.mkv", &data, 20_000);
    for (name, body) in &pars {
        post.add_file(name, body, 20_000);
    }
    let server = FakeServer::start(post.articles.clone()).await;
    for id in ids.iter().take(20) {
        server.state.lock().missing.insert(id.clone());
    }
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 4)])).await.unwrap();
    let id = engine.add(&post.nzb(), "Movie.Job", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Failed);
    let err = done.error.unwrap();
    assert!(err.contains("not repairable"), "{err}");
    assert!(!tmp.path().join("complete/Movie.Job").exists(), "a failed job must not appear as completed");

    // And with no recovery data at all:
    let mut post = Post::new();
    let ids = post.add_file("plain.mkv", &data, 20_000);
    let server = FakeServer::start(post.articles.clone()).await;
    server.state.lock().missing.insert(ids[0].clone());
    let engine2 = Engine::start(cfg(&tmp.path().join("b"), vec![server.config("main", 0, 2)])).await.unwrap();
    let id = engine2.add(&post.nzb(), "Plain", None).unwrap();
    let done = wait_done(&engine2, &id).await;
    assert_eq!(done.state, JobState::Failed);
    assert!(done.error.unwrap().contains("no recovery data"));
    drop(engine);
}

#[tokio::test(flavor = "multi_thread")]
async fn restores_obfuscated_names_from_recovery_data() {
    if !have("par2") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let data = bytes(9, 300_000);
    std::fs::write(src.join("Real.Name.2021.mkv"), &data).unwrap();
    let pars = make_par2(&src, "Real.Name.2021", &["Real.Name.2021.mkv"], 10, 1);
    let mut post = Post::new();
    post.add_file("b082fa0beaa644d3aa01045d5b8d0b36.mkv", &data, 20_000);
    for (name, body) in &pars {
        post.add_file(name, body, 20_000);
    }
    let server = FakeServer::start(post.articles.clone()).await;
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 4)])).await.unwrap();
    let id = engine.add(&post.nzb(), "Obfuscated.Job", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert_eq!(names(&done), ["Real.Name.2021.mkv"]);
    assert_eq!(read(&done, "Real.Name.2021.mkv"), data);
}

#[tokio::test(flavor = "multi_thread")]
async fn renames_a_meaningless_file_after_the_job_when_there_is_no_recovery_data() {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(10, 200_000);
    let mut post = Post::new();
    post.add_file("b082fa0beaa644d3aa01045d5b8d0b36.mkv", &data, 20_000);
    let server = FakeServer::start(post.articles.clone()).await;
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 2)])).await.unwrap();
    let id = engine.add(&post.nzb(), "Show.S01E02.1080p.WEB.h264-GRP", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(names(&done), ["Show.S01E02.1080p.WEB.h264-GRP.mkv"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn extracts_multi_volume_and_password_protected_archives() {
    if !have("7zz") {
        eprintln!("7zz not installed; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("Sub")).unwrap();
    let (movie, sub) = (bytes(11, 500_000), bytes(12, 20_000));
    std::fs::write(src.join("movie.mkv"), &movie).unwrap();
    std::fs::write(src.join("Sub/movie.en.srt"), &sub).unwrap();
    let ok = Command::new("7zz").args(["a", "-mx0", "-v150k", "-psecret", "-mhe=on", "pack.7z", "movie.mkv", "Sub"]).current_dir(&src).output().unwrap();
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stdout));

    let mut post = Post::new();
    post.password = Some("secret".into());
    let mut volumes = 0;
    for e in std::fs::read_dir(&src).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with("pack.7z.") {
            post.add_file(&n, &std::fs::read(e.path()).unwrap(), 40_000);
            volumes += 1;
        }
    }
    assert!(volumes >= 3);
    let server = FakeServer::start(post.articles.clone()).await;
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 4)])).await.unwrap();

    let id = engine.add(&post.nzb(), "Packed.Job", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert_eq!(read(&done, "movie.mkv"), movie);
    assert_eq!(read(&done, "Sub/movie.en.srt"), sub);
    assert_eq!(names(&done), ["Sub", "movie.mkv"], "archive volumes are removed after extraction");

    // Same post without the password: a clear failure, nothing in completed.
    post.password = None;
    let id = engine.add(&post.nzb(), "Locked.Job", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Failed);
    assert!(done.error.unwrap().contains("password"));
    assert!(!tmp.path().join("complete/Locked.Job").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn joins_raw_split_files() {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(13, 250_000);
    let mut post = Post::new();
    for (i, chunk) in data.chunks(100_000).enumerate() {
        post.add_file(&format!("clip.mkv.{:03}", i + 1), chunk, 30_000);
    }
    let server = FakeServer::start(post.articles.clone()).await;
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 3)])).await.unwrap();
    let id = engine.add(&post.nzb(), "Split", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert_eq!(read(&done, "clip.mkv"), data);
    assert_eq!(names(&done), ["clip.mkv"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn waits_and_retries_when_servers_are_unreachable_or_reject_credentials() {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(14, 100_000);
    let mut post = Post::new();
    post.add_file("f.bin", &data, 20_000);
    let server = FakeServer::start(post.articles.clone()).await;
    server.state.lock().auth = Some(("user".into(), "right".into()));
    let mut bad = server.config("main", 0, 2);
    bad.password = "wrong".into();
    let engine = Engine::start(cfg(tmp.path(), vec![bad.clone()])).await.unwrap();
    let id = engine.add(&post.nzb(), "job", None).unwrap();

    let mut waiting = None;
    for _ in 0..100 {
        let s = engine.job(&id).unwrap();
        if s.message.starts_with("Waiting to retry") {
            waiting = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let s = waiting.expect("job should be waiting, not failed");
    assert_eq!(s.state, JobState::Queued);
    assert!(s.message.contains("authentication"), "{}", s.message);
    assert_eq!(s.missing_articles, 0, "an unreachable server says nothing about the articles");

    // Fixing the credentials lets the same job finish.
    let mut good = bad;
    good.password = "right".into();
    engine.set_config(cfg(tmp.path(), vec![good]));
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert_eq!(read(&done, "f.bin"), data);
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_resume_and_remove() {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(15, 1_000_000);
    let mut post = Post::new();
    post.add_file("f.bin", &data, 10_000);
    let server = FakeServer::start(post.articles.clone()).await;
    server.state.lock().delay_ms = 5;
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 2)])).await.unwrap();
    let id = engine.add(&post.nzb(), "job", None).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    engine.pause(&id).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let at_pause = server.total_served();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(server.total_served(), at_pause, "a paused job fetches nothing");
    assert_eq!(engine.job(&id).unwrap().state, JobState::Paused);

    engine.resume(&id).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed);
    assert_eq!(read(&done, "f.bin"), data);

    let id2 = engine.add(&post.nzb(), "second", None).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    engine.remove(&id2, true).unwrap();
    assert!(engine.job(&id2).is_none());
    tokio::time::sleep(Duration::from_millis(900)).await;
    let left: Vec<_> = std::fs::read_dir(tmp.path().join("incomplete")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("second")).collect();
    assert!(left.is_empty(), "removed job leaves no working directory");
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_file_names_stay_inside_the_job_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(16, 50_000);
    let mut post = Post::new();
    post.add_file_with_subject("../../../escape.bin", "\"../../../escape.bin\" yEnc", &data, 20_000);
    let server = FakeServer::start(post.articles.clone()).await;
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 2)])).await.unwrap();
    let id = engine.add(&post.nzb(), "Evil", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert_eq!(names(&done), ["escape.bin"]);
    assert!(!tmp.path().join("escape.bin").exists());
    assert!(!tmp.path().parent().unwrap().join("escape.bin").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn gives_up_early_when_most_of_a_post_is_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(17, 4_000_000);
    let mut post = Post::new();
    let ids = post.add_file("gone.mkv", &data, 10_000);
    let server = FakeServer::start(post.articles.clone()).await;
    {
        let mut s = server.state.lock();
        s.missing.extend(ids.iter().cloned());
        s.delay_ms = 2;
    }
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 2)])).await.unwrap();
    let id = engine.add(&post.nzb(), "Gone", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Failed);
    assert!(done.error.unwrap().contains("too much of the post is missing"));
    assert!(done.missing_articles < 200, "stopped after {} of 400 articles", done.missing_articles);
    assert_eq!(done.speed, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn repairs_when_recovery_files_arrive_under_meaningless_names() {
    if !have("par2") {
        return;
    }
    // Obfuscated posts name files one way in the NZB subject and another inside the articles.
    // Recovery volumes then land on disk without a .par2 name, and par2 will not pick them up
    // unless they are given one.
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let data = bytes(21, 1_000_000);
    std::fs::write(src.join("movie.mkv"), &data).unwrap();
    let pars = make_par2(&src, "movie", &["movie.mkv"], 20, 4);

    let mut post = Post::new();
    let ids = post.add_file_with_subject("q7Zk2LmPa91xT", "\"abc123.01\" yEnc", &data, 20_000);
    for (i, (name, body)) in pars.iter().enumerate() {
        let subject = if name.contains(".vol") { format!("\"abc123.{}\" yEnc", &name[name.find(".vol").unwrap() + 1..]) } else { "\"abc123.par2\" yEnc".to_string() };
        post.add_file_with_subject(&format!("Rnd{i}xYz81kQ"), &subject, body, 20_000);
    }
    let server = FakeServer::start(post.articles.clone()).await;
    server.state.lock().missing.insert(ids[7].clone());
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 4)])).await.unwrap();
    let id = engine.add(&post.nzb(), "Obfuscated.Repair", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert_eq!(read(&done, "movie.mkv"), data);
    assert_eq!(names(&done), ["movie.mkv"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_how_much_of_a_post_is_still_available_without_downloading() {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(31, 1_000_000);
    let mut post = Post::new();
    let ids = post.add_file("f.bin", &data, 10_000);
    let primary = FakeServer::start(post.articles.clone()).await;
    let backup = FakeServer::start(post.articles.clone()).await;
    let engine = Engine::start(cfg(tmp.path(), vec![primary.config("primary", 0, 2), backup.config("backup", 1, 2)])).await.unwrap();

    assert_eq!(engine.availability(&post.nzb(), 20).await.unwrap(), (20, 20));
    // Gone from the primary only: the backup still has everything.
    primary.state.lock().missing.extend(ids.iter().cloned());
    assert_eq!(engine.availability(&post.nzb(), 20).await.unwrap(), (20, 20));
    // Gone from both.
    backup.state.lock().missing.extend(ids.iter().cloned());
    assert_eq!(engine.availability(&post.nzb(), 20).await.unwrap(), (0, 20));
    assert_eq!(primary.total_served() + backup.total_served(), 0, "nothing is downloaded by a check");
}

#[tokio::test(flavor = "multi_thread")]
async fn repairs_an_archive_that_arrived_intact_but_was_posted_damaged() {
    if !have("7zz") || !have("par2") {
        eprintln!("7zz or par2 not installed; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let movie = bytes(21, 600_000);
    std::fs::write(src.join("movie.mkv"), &movie).unwrap();
    let ok = Command::new("7zz").args(["a", "-mx0", "-v200k", "pack.7z", "movie.mkv"]).current_dir(&src).output().unwrap();
    assert!(ok.status.success());
    std::fs::remove_file(src.join("movie.mkv")).unwrap();
    let mut volumes: Vec<String> = std::fs::read_dir(&src).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    volumes.sort();
    let pars = make_par2(&src, "pack", &volumes.iter().map(String::as_str).collect::<Vec<_>>(), 20, 2);

    // One volume goes up with a stretch of wrong bytes. Every article is internally consistent
    // and every file has the right size, so nothing looks wrong until the archive is opened.
    let mut post = Post::new();
    for (i, n) in volumes.iter().enumerate() {
        let mut body = std::fs::read(src.join(n)).unwrap();
        if i == 1 {
            for b in &mut body[50_000..52_000] {
                *b ^= 0x5a;
            }
        }
        post.add_file(n, &body, 40_000);
    }
    for (name, body) in &pars {
        post.add_file(name, body, 40_000);
    }
    let server = FakeServer::start(post.articles.clone()).await;
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 4)])).await.unwrap();
    let id = engine.add(&post.nzb(), "Posted.Damaged", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert_eq!((done.missing_articles, done.damaged_articles), (0, 0));
    assert_eq!(read(&done, "movie.mkv"), movie);
    assert_eq!(names(&done), ["movie.mkv"]);
}

/// Volume names the way posters write them: name.part1.rar for RAR 5, name.rar/.r00 for RAR 4.
fn volume_name(base: &str, old_style: bool, i: usize) -> String {
    match (old_style, i) {
        (true, 0) => format!("{base}.rar"),
        (true, n) => format!("{base}.r{:02}", n - 1),
        (false, n) => format!("{base}.part{:02}.rar", n + 1),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unpacks_stored_archives_while_downloading() {
    for old_style in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let movie = bytes(31, 900_000);
        let volumes = if old_style { spool_nntp::rar::testing::rar4_volumes("movie.mkv", &movie, 200_000) } else { spool_nntp::rar::testing::rar5_volumes("movie.mkv", &movie, 200_000) };
        assert_eq!(volumes.len(), 5);
        let mut post = Post::new();
        for (i, v) in volumes.iter().enumerate() {
            post.add_file(&volume_name("movie", old_style, i), v, 40_000);
        }
        post.add_file("movie.nfo", b"about this release", 40_000);
        let server = FakeServer::start(post.articles.clone()).await;
        // No extraction tool at all: only the built-in unpacking can produce the file.
        let mut config = cfg(tmp.path(), vec![server.config("main", 0, 4)]);
        config.sevenzip_path = "/nonexistent/7zz".into();
        let engine = Engine::start(config).await.unwrap();
        let id = engine.add(&post.nzb(), "Stored.Job", None).unwrap();
        let done = wait_done(&engine, &id).await;
        assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
        assert!(done.unpacked_directly, "old_style={old_style}");
        assert_eq!(read(&done, "movie.mkv"), movie);
        assert_eq!(names(&done), ["movie.mkv", "movie.nfo"], "volumes and working files are gone");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_volume_puts_the_others_back_and_the_job_is_repaired() {
    if !have("7zz") || !have("par2") {
        eprintln!("7zz or par2 not installed; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let movie = bytes(32, 900_000);
    let volumes = spool_nntp::rar::testing::rar5_volumes("movie.mkv", &movie, 200_000);
    let names_: Vec<String> = (0..volumes.len()).map(|i| volume_name("movie", false, i)).collect();
    for (n, v) in names_.iter().zip(&volumes) {
        std::fs::write(src.join(n), v).unwrap();
    }
    let pars = make_par2(&src, "movie", &names_.iter().map(String::as_str).collect::<Vec<_>>(), 30, 2);

    // The fourth volume goes up with wrong bytes in its payload; three good ones come first and
    // are unpacked and deleted before the bad one is seen.
    let mut post = Post::new();
    for (i, (n, v)) in names_.iter().zip(&volumes).enumerate() {
        let mut body = v.clone();
        if i == 3 {
            for b in &mut body[100_000..101_000] {
                *b ^= 0x33;
            }
        }
        post.add_file(n, &body, 40_000);
    }
    for (name, body) in &pars {
        post.add_file(name, body, 40_000);
    }
    let server = FakeServer::start(post.articles.clone()).await;
    let engine = Engine::start(cfg(tmp.path(), vec![server.config("main", 0, 2)])).await.unwrap();
    let id = engine.add(&post.nzb(), "Stored.Damaged", None).unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert!(!done.unpacked_directly);
    assert_eq!(read(&done, "movie.mkv"), movie, "repaired from recovery data after the volumes were put back");
    assert_eq!(names(&done), ["movie.mkv"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn unpacking_carries_on_after_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let movie = bytes(33, 6_000_000);
    let volumes = spool_nntp::rar::testing::rar5_volumes("movie.mkv", &movie, 250_000);
    let mut post = Post::new();
    for (i, v) in volumes.iter().enumerate() {
        post.add_file(&volume_name("movie", false, i), v, 10_000);
    }
    let server = FakeServer::start(post.articles.clone()).await;
    server.state.lock().delay_ms = 4;
    let mut config = cfg(tmp.path(), vec![server.config("main", 0, 4)]);
    config.sevenzip_path = "/nonexistent/7zz".into();

    let engine = Engine::start(config.clone()).await.unwrap();
    let id = engine.add(&post.nzb(), "Stored.Resumed", None).unwrap();
    // Stop once a volume has been unpacked and deleted, with plenty still to download.
    loop {
        let s = engine.job(&id).unwrap();
        let unpacked_one = std::fs::read_dir(tmp.path().join("incomplete")).ok().and_then(|mut d| d.next()).and_then(|e| e.ok()).is_some_and(|e| e.path().join("_direct/state.json").exists());
        assert!(s.done_bytes < s.total_bytes, "finished before it could be interrupted");
        if unpacked_one && s.done_bytes * 3 > s.total_bytes {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    engine.shutdown().await;
    // Some volumes are already unpacked and gone; the rest are still to come.
    let job_dir = std::fs::read_dir(tmp.path().join("incomplete")).unwrap().flatten().next().unwrap().path();
    let left: Vec<String> = std::fs::read_dir(&job_dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.ends_with(".rar")).collect();
    assert!(left.len() < volumes.len() && job_dir.join("_direct/state.json").exists(), "{left:?}");

    let engine = Engine::start(config).await.unwrap();
    let done = wait_done(&engine, &id).await;
    assert_eq!(done.state, JobState::Completed, "{:?}", done.error);
    assert!(done.unpacked_directly);
    assert_eq!(read(&done, "movie.mkv"), movie);
    assert_eq!(server.max_served_per_article(), 1, "nothing is fetched twice");
}
