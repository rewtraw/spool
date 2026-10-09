//! End-to-end tests: a fake indexer and a fake usenet server on one side, a temporary library on
//! the other, and the real application in between.

use axum::extract::{Path as AxPath, State};
use axum::routing::get;
use parking_lot::Mutex;
use spool::acquire::Scope;
use spool::app::App;
use spool::db::Db;
use spool::models::*;
use spool::settings::Mode;
use spool_core::profile::QualityProfile;
use spool_core::{Quality, QualityModel};
use spool_nntp::testing::{bytes, FakeServer, Post};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// A two-second real video, made once, so ffprobe has something true to say.
fn sample_video() -> Option<Vec<u8>> {
    static CELL: std::sync::OnceLock<Option<Vec<u8>>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let dir = tempfile::tempdir().ok()?;
        let out = dir.path().join("v.mkv");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=24:duration=2", "-f", "lavfi", "-i", "sine=duration=2", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac", "2", "-y"])
            .arg(&out)
            .status()
            .ok()?;
        if !ok.success() {
            return None;
        }
        std::fs::read(out).ok()
    })
    .clone()
}

#[derive(Default)]
struct IndexerState {
    /// (release title, size in bytes, nzb)
    releases: Vec<(String, u64, Vec<u8>)>,
    hits: usize,
    /// Release names the indexer has not linked to a series id: a search by id misses them.
    untagged: std::collections::HashSet<String>,
    /// Every query string received.
    queries: Vec<String>,
}

type Shared = Arc<Mutex<IndexerState>>;

async fn feed(State(s): State<Shared>, headers: axum::http::HeaderMap, axum::extract::RawQuery(query): axum::extract::RawQuery) -> String {
    let host = headers.get("host").and_then(|h| h.to_str().ok()).unwrap_or("").to_string();
    let query = query.unwrap_or_default();
    let by_id = query.contains("tvdbid=") || query.contains("imdbid=");
    let mut st = s.lock();
    st.hits += 1;
    st.queries.push(query);
    let items: String = st
        .releases
        .iter()
        .enumerate()
        .filter(|(_, (title, _, _))| !(by_id && st.untagged.contains(title)))
        .map(|(i, (title, size, _))| {
            format!(
                "<item><title>{title}</title><guid>guid-{i}-{title}</guid><pubDate>{}</pubDate><enclosure url=\"http://{host}/nzb/{i}\" length=\"{size}\" type=\"application/x-nzb\"/><newznab:attr name=\"size\" value=\"{size}\"/></item>",
                (chrono::Utc::now() - chrono::Duration::days(3)).to_rfc2822()
            )
        })
        .collect();
    format!("<?xml version=\"1.0\"?><rss xmlns:newznab=\"http://www.newznab.com/DTD/2010/feeds/attributes/\"><channel>{items}</channel></rss>")
}

async fn nzb(State(s): State<Shared>, AxPath(i): AxPath<usize>) -> Vec<u8> {
    s.lock().releases.get(i).map(|r| r.2.clone()).unwrap_or_default()
}

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    app: App,
    indexer: Shared,
    usenet: FakeServer,
    post: Post,
}

impl World {
    async fn new(mode: Mode) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let usenet = FakeServer::start(HashMap::new()).await;
        let indexer: Shared = Default::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = axum::Router::new().route("/api", get(feed)).route("/nzb/{i}", get(nzb)).with_state(indexer.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let db = Db::open(&root.join("data/spool.db")).unwrap();
        let app = App::start(&root.join("data"), db).await.unwrap();
        let mut g = app.settings.general();
        g.mode = mode;
        g.movie_root = root.join("Movies").to_string_lossy().to_string();
        g.series_root = root.join("TV Shows").to_string_lossy().to_string();
        g.downloads_dir = root.join("downloads").to_string_lossy().to_string();
        app.settings.set_general(&g).unwrap();
        app.settings.set_servers(&[usenet.config("main", 0, 4)]).unwrap();
        app.apply_engine_config();
        let mut ix: Indexer = serde_json::from_value(serde_json::json!({"name": "Fake", "url": format!("http://{addr}"), "api_key": "k"})).unwrap();
        app.db.save_indexer(&mut ix).unwrap();
        for kind in ["movie", "tv"] {
            app.db.save_profile(&mut QualityProfile::default_hd(kind)).unwrap();
        }
        std::fs::create_dir_all(root.join("Movies")).unwrap();
        std::fs::create_dir_all(root.join("TV Shows")).unwrap();
        app.spawn_tracker();
        World { _tmp: tmp, root, app, indexer, usenet, post: Post::new() }
    }

    fn profile(&self, kind: &str) -> i64 {
        self.app.db.profiles().unwrap().into_iter().find(|p| p.kind == kind).unwrap().id
    }

    fn movie(&self, title: &str, year: u32, imdb: &str) -> Title {
        let mut t: Title = serde_json::from_value(serde_json::json!({"kind": "movie", "title": title, "year": year, "imdb_id": imdb, "minimum_availability": "announced"})).unwrap();
        t.profile_id = self.profile("movie");
        self.app.db.save_title(&mut t).unwrap();
        t
    }

    fn series(&self, title: &str, tvdb: u32, episodes: &[(u32, u32, &str)]) -> (Title, Vec<i64>) {
        let mut t: Title = serde_json::from_value(serde_json::json!({"kind": "series", "title": title, "year": 2016, "tvdb_id": tvdb, "seasons": [{"number": 1, "monitored": true}]})).unwrap();
        t.profile_id = self.profile("tv");
        self.app.db.save_title(&mut t).unwrap();
        let ids = episodes
            .iter()
            .map(|(s, e, name)| {
                self.app
                    .db
                    .upsert_episode(&Episode { id: 0, title_id: t.id, season: *s, episode: *e, absolute: None, title: name.to_string(), overview: String::new(), air_date: Some("2016-10-02".into()), air_date_utc: None, runtime: 0, monitored: true, file_id: None })
                    .unwrap()
            })
            .collect();
        (t, ids)
    }

    /// Publish a release: its files go to the usenet server and its NZB to the indexer.
    /// Returns the message ids of the first file.
    fn release(&mut self, title: &str, files: &[(&str, &[u8])]) -> Vec<String> {
        let mut post = Post::new();
        // Keep message ids unique across releases.
        let offset = self.post.articles.len();
        let mut first = vec![];
        for (i, (name, data)) in files.iter().enumerate() {
            let ids = post.add_file(name, data, 30_000);
            if i == 0 {
                first = ids;
            }
        }
        let nzb = String::from_utf8(post.nzb()).unwrap();
        let mut renamed = nzb.clone();
        let mut state = self.usenet.state.lock();
        let mut first_renamed = vec![];
        for (id, body) in post.articles {
            let new_id = format!("r{offset}-{id}");
            renamed = renamed.replace(&format!(">{id}<"), &format!(">{new_id}<"));
            if first.contains(&id) {
                first_renamed.push(new_id.clone());
            }
            self.post.articles.insert(new_id.clone(), vec![]);
            state.articles.insert(new_id, body);
        }
        let size = files.iter().map(|f| f.1.len() as u64).sum::<u64>().max(3 << 30);
        self.indexer.lock().releases.push((title.to_string(), size, renamed.into_bytes()));
        first_renamed
    }

    async fn wait_state(&self, title_id: i64, want: AcqState) -> Acquisition {
        for _ in 0..400 {
            let all = self.app.db.title_acquisitions(title_id).unwrap();
            if let Some(a) = all.iter().find(|a| a.state == want) {
                return a.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no acquisition reached {want:?}: {:#?}", self.app.db.title_acquisitions(title_id).unwrap().iter().map(|a| (a.state, a.error.clone(), a.release.title.clone())).collect::<Vec<_>>());
    }
}

/// Damage one article in a way only downloading it reveals: the server still says it has it.
fn corrupt(w: &World, id: &str) {
    let mut st = w.usenet.state.lock();
    let body = st.articles.get_mut(id).expect("article exists");
    let mid = body.len() / 2;
    for b in &mut body[mid..mid + 40] {
        if *b != b'\r' && *b != b'\n' && *b != b'=' {
            *b = if *b == b'A' { b'B' } else { b'A' };
        }
    }
}

fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.push(p.strip_prefix(base).unwrap().to_string_lossy().to_string());
            }
        }
    }
    let mut out = vec![];
    walk(dir, dir, &mut out);
    out.sort();
    out
}

macro_rules! video_or_skip {
    () => {
        match sample_video() {
            Some(v) => v,
            None => {
                eprintln!("ffmpeg not available; skipping");
                return;
            }
        }
    };
}

#[tokio::test(flavor = "multi_thread")]
async fn movie_goes_from_search_to_library() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let movie = w.movie("Night of the Example", 2020, "tt1234567");
    w.release("Night.of.the.Example.2020.720p.BluRay.x264-LOW", &[("low.mkv", &video)]);
    w.release("Night.of.the.Example.2020.1080p.WEB-DL.DD5.1.H264-WEB", &[("web.mkv", &video)]);
    w.release("Night.of.the.Example.2020.1080p.BluRay.x264-GRP", &[("night.of.the.example.2020.1080p.bluray.x264-grp.mkv", &video), ("night.nfo", b"info")]);
    w.release("Night.of.the.Example.1968.1080p.BluRay.x264-OLD", &[("old.mkv", &video)]);
    w.release("Completely.Different.Film.2020.1080p.BluRay.x264-NOPE", &[("nope.mkv", &video)]);

    let outcome = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "{}", outcome.message);
    assert_eq!(outcome.grabbed[0].release.title, "Night.of.the.Example.2020.1080p.BluRay.x264-GRP");

    let reasons: HashMap<String, Vec<String>> = outcome.decisions.iter().map(|d| (d.release.title.clone(), d.rejections.iter().map(|r| r.code.clone()).collect())).collect();
    assert_eq!(reasons["Night.of.the.Example.2020.720p.BluRay.x264-LOW"], ["quality_not_wanted"]);
    assert_eq!(reasons["Night.of.the.Example.1968.1080p.BluRay.x264-OLD"], ["wrong_title"]);
    assert_eq!(reasons["Completely.Different.Film.2020.1080p.BluRay.x264-NOPE"], ["wrong_title"]);
    assert!(reasons["Night.of.the.Example.2020.1080p.WEB-DL.DD5.1.H264-WEB"].is_empty(), "acceptable but not chosen");

    w.wait_state(movie.id, AcqState::Imported).await;
    assert_eq!(tree(&w.root.join("Movies")), ["Night of the Example (2020)/Night of the Example (2020) {imdb-tt1234567} [Bluray-1080p][AAC 2.0][x264]-GRP.mkv"]);
    let files = w.app.db.files(movie.id).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].quality.quality, Quality::Bluray1080p);
    assert_eq!(files[0].size, video.len() as u64);
    assert!(files[0].media_info.as_ref().is_some_and(|m| m.width == 320 && m.video_codec == "x264"));
    for _ in 0..40 {
        if tree(&w.root.join("downloads")).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(tree(&w.root.join("downloads")).is_empty(), "download folders are cleaned up: {:?}", tree(&w.root.join("downloads")));
    let kinds: Vec<String> = w.app.db.history(Some(movie.id), 10).unwrap().into_iter().map(|h| h.kind).collect();
    assert!(kinds.contains(&"grabbed".to_string()) && kinds.contains(&"imported".to_string()));

    // Searching again finds nothing worth grabbing: the cutoff is met.
    let again = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert!(again.grabbed.is_empty());
    let d = again.decisions.iter().find(|d| d.release.title.ends_with("-GRP")).unwrap();
    assert_eq!(d.rejections[0].code, "not_an_upgrade");
}

#[tokio::test(flavor = "multi_thread")]
async fn season_pack_and_single_episode_import() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let (show, eps) = w.series("Example World", 296762, &[(1, 1, "The Original"), (1, 2, "Chestnut"), (1, 3, "The Stray")]);
    // A pack is only wanted when the whole season has aired; all three have.
    w.release(
        "Example.World.S01.1080p.BluRay.x264-PACK",
        &[("Example.World.S01E01.1080p.BluRay.x264-PACK.mkv", &video), ("Example.World.S01E02.1080p.BluRay.x264-PACK.mkv", &video), ("Example.World.S01E03.1080p.BluRay.x264-PACK.mkv", &video)],
    );
    w.release("Example.World.S01E02.1080p.WEB.H264-SOLO", &[("b082fa0beaa644d3aa01045d5b8d0b36.mkv", &video)]);

    let outcome = w.app.search(show.id, Scope::Missing, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "one pack beats a pack plus an overlapping single: {}", outcome.message);
    assert_eq!(outcome.grabbed[0].episode_ids, eps);
    w.wait_state(show.id, AcqState::Imported).await;
    assert_eq!(
        tree(&w.root.join("TV Shows")),
        [
            "Example World/Season 1/Example World - S01E01 - The Original Bluray-1080p.mkv",
            "Example World/Season 1/Example World - S01E02 - Chestnut Bluray-1080p.mkv",
            "Example World/Season 1/Example World - S01E03 - The Stray Bluray-1080p.mkv",
        ]
    );
    assert!(w.app.db.episodes(show.id).unwrap().iter().all(|e| e.file_id.is_some()));

    // A single obfuscated episode, asked for by hand, also lands in the right place.
    let (show2, eps2) = w.series("Second Show", 111, &[(1, 1, "Pilot")]);
    w.release("Second.Show.S01E01.1080p.WEB.H264-SOLO", &[("b082fa0beaa644d3aa01045d5b8d0b36.mkv", &video)]);
    let outcome = w.app.search(show2.id, Scope::Episode(eps2[0]), true, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "{}", outcome.message);
    w.wait_state(show2.id, AcqState::Imported).await;
    assert_eq!(tree(&w.root.join("TV Shows/Second Show")), ["Season 1/Second Show - S01E01 - Pilot WEBDL-1080p.mkv"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_download_is_blocklisted_and_the_next_release_is_tried() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let movie = w.movie("Broken Arrow Example", 2019, "tt7654321");
    let broken = w.release("Broken.Arrow.Example.2019.1080p.BluRay.x264-BAD", &[("bad.mkv", &video)]);
    w.release("Broken.Arrow.Example.2019.1080p.WEB-DL.H264-GOOD", &[("good.mkv", &video)]);
    corrupt(&w, &broken[0]);

    let outcome = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert!(outcome.grabbed[0].release.title.ends_with("-BAD"));
    let imported = w.wait_state(movie.id, AcqState::Imported).await;
    assert!(imported.release.title.ends_with("-GOOD"));

    let all = w.app.db.title_acquisitions(movie.id).unwrap();
    let failed = all.iter().find(|a| a.state == AcqState::Failed).expect("the first attempt is recorded as failed");
    assert!(failed.error.as_ref().unwrap().contains("no recovery data"), "{:?}", failed.error);
    assert_eq!(w.app.db.blocklisted(movie.id).unwrap().len(), 1);
    assert_eq!(tree(&w.root.join("Movies")), ["Broken Arrow Example (2019)/Broken Arrow Example (2019) {imdb-tt7654321} [WEBDL-1080p][AAC 2.0][x264]-GOOD.mkv"]);

    // The blocklisted release is never offered again.
    let again = w.app.search(movie.id, Scope::Movie, true, false).await.unwrap();
    let bad = again.decisions.iter().find(|d| d.release.title.ends_with("-BAD")).unwrap();
    assert!(bad.rejections.iter().any(|r| r.code == "blocklisted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn upgrade_replaces_the_old_file_and_keeps_it_in_the_recycle_folder() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let mut movie = w.movie("Upgrade Me", 2018, "tt1111111");
    movie.path = w.root.join("Movies/Upgrade Me (2018)").to_string_lossy().to_string();
    w.app.db.save_title(&mut movie).unwrap();
    std::fs::create_dir_all(&movie.path).unwrap();
    std::fs::write(Path::new(&movie.path).join("old.mkv"), b"old file").unwrap();
    let old: MediaFile = serde_json::from_value(serde_json::json!({"title_id": movie.id, "rel_path": "old.mkv", "size": 8, "quality": {"quality": "hdtv-1080p"}})).unwrap();
    w.app.db.with(|c| c.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1, 'old.mkv', 8, ?2)", rusqlite::params![movie.id, serde_json::to_string(&old).unwrap()])).unwrap();

    w.release("Upgrade.Me.2018.1080p.BluRay.x264-NEW", &[("new.mkv", &video)]);
    let outcome = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "{}", outcome.message);
    w.wait_state(movie.id, AcqState::Imported).await;

    let files = tree(&w.root.join("Movies"));
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(files.iter().any(|f| f == "Upgrade Me (2018)/Upgrade Me (2018) {imdb-tt1111111} [Bluray-1080p][AAC 2.0][x264]-NEW.mkv"));
    let recycled = files.iter().find(|f| f.starts_with(".spool-recycle/")).expect("old file is recycled, not deleted");
    assert_eq!(std::fs::read(w.root.join("Movies").join(recycled)).unwrap(), b"old file");
    let db_files = w.app.db.files(movie.id).unwrap();
    assert_eq!(db_files.len(), 1);
    assert_eq!(db_files[0].quality, QualityModel::new(Quality::Bluray1080p));
    assert!(w.app.db.history(Some(movie.id), 10).unwrap().iter().any(|h| h.kind == "upgraded"));
}

#[tokio::test(flavor = "multi_thread")]
async fn shadow_mode_decides_but_touches_nothing() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Shadow).await;
    let movie = w.movie("Shadow Play", 2021, "tt2222222");
    w.release("Shadow.Play.2021.1080p.BluRay.x264-GRP", &[("s.mkv", &video)]);

    let outcome = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert!(outcome.grabbed.is_empty());
    assert!(outcome.decisions[0].accepted);
    let msg = w.app.rss_sync().await.unwrap();
    assert!(msg.contains("would be grabbed"), "{msg}");

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(w.app.db.title_acquisitions(movie.id).unwrap().is_empty());
    assert!(w.app.engine.jobs().is_empty());
    assert_eq!(w.usenet.total_served(), 0, "nothing is downloaded in shadow mode");
    assert!(tree(&w.root.join("Movies")).is_empty());
    let would: Vec<_> = w.app.db.history(Some(movie.id), 10).unwrap().into_iter().filter(|h| h.kind == "would_grab").collect();
    assert_eq!(would.len(), 1, "the same release is noted once, however often it is seen");
}

#[tokio::test(flavor = "multi_thread")]
async fn rss_feed_grabs_only_what_the_library_wants() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let wanted = w.movie("Wanted Film", 2022, "tt3333333");
    let mut unmonitored = w.movie("Ignored Film", 2022, "tt4444444");
    unmonitored.monitored = false;
    w.app.db.save_title(&mut unmonitored).unwrap();
    w.release("Wanted.Film.2022.1080p.BluRay.x264-GRP", &[("w.mkv", &video)]);
    w.release("Ignored.Film.2022.1080p.BluRay.x264-GRP", &[("i.mkv", &video)]);
    w.release("Unknown.Film.2022.1080p.BluRay.x264-GRP", &[("u.mkv", &video)]);

    let msg = w.app.rss_sync().await.unwrap();
    assert!(msg.contains("1 grabbed"), "{msg}");
    w.wait_state(wanted.id, AcqState::Imported).await;
    assert!(w.app.db.title_acquisitions(unmonitored.id).unwrap().is_empty());
    let d = w.app.db.decisions(unmonitored.id).unwrap();
    assert_eq!(d[0].rejections[0].code, "not_monitored");
    assert_eq!(tree(&w.root.join("Movies")).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_imports_are_finished_at_startup() {
    let w = World::new(Mode::Active).await;
    let mut movie = w.movie("Crash Test", 2017, "tt5555555");
    movie.path = w.root.join("Movies/Crash Test (2017)").to_string_lossy().to_string();
    w.app.db.save_title(&mut movie).unwrap();
    let file: MediaFile = serde_json::from_value(serde_json::json!({"title_id": movie.id, "rel_path": "Crash Test (2017).mkv", "size": 5, "quality": {"quality": "bluray-1080p"}})).unwrap();
    let journal = |src: &Path, dst: &Path| {
        let op = spool::import::Operation { acquisition_id: None, title_id: movie.id, episode_ids: vec![], src: src.to_string_lossy().to_string(), dst: dst.to_string_lossy().to_string(), replaces: vec![], file: file.clone() };
        w.app.db.with(|c| c.execute("INSERT INTO journal(ts, acquisition_id, state, data) VALUES (0, NULL, 'planned', ?1)", [serde_json::to_string(&op).unwrap()])).unwrap();
    };

    // Crashed before the move: the file is still in the download folder.
    let src = w.root.join("downloads/dl/movie.mkv");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, b"video").unwrap();
    let dst = Path::new(&movie.path).join("Crash Test (2017).mkv");
    journal(&src, &dst);
    assert_eq!(w.app.recover().unwrap(), 1);
    assert_eq!(std::fs::read(&dst).unwrap(), b"video");
    assert!(!src.exists());
    assert_eq!(w.app.db.files(movie.id).unwrap().len(), 1);

    // Crashed after the move but before the database commit.
    w.app.db.with(|c| c.execute("DELETE FROM files", [])).unwrap();
    journal(&src, &dst);
    assert_eq!(w.app.recover().unwrap(), 1);
    assert_eq!(w.app.db.files(movie.id).unwrap().len(), 1, "the catalog catches up with the disk");
    assert_eq!(w.app.recover().unwrap(), 0, "nothing is done twice");

    // Both gone: reported, not silently dropped.
    std::fs::remove_file(&dst).unwrap();
    w.app.db.with(|c| c.execute("DELETE FROM files", [])).unwrap();
    journal(&src, &dst);
    assert_eq!(w.app.recover().unwrap(), 0);
    assert_eq!(w.app.db.attention(false).unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_written_while_the_media_volume_is_missing() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let movie = w.movie("Unplugged", 2023, "tt6666666");
    w.release("Unplugged.2023.1080p.BluRay.x264-GRP", &[("u.mkv", &video)]);
    // A plain directory is not a mount point, which is what a disconnected disk looks like.
    let mut g = w.app.settings.general();
    g.required_volume = w.root.join("Movies").to_string_lossy().to_string();
    w.app.settings.set_general(&g).unwrap();
    assert!(!w.app.volume_ok());

    // With the disk away, nothing is even started.
    let outcome = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert!(outcome.grabbed.is_empty());
    assert!(w.app.engine.jobs().is_empty());
    assert!(!w.root.join("downloads").exists(), "no download folders are created either");

    // The disk is there when the download starts and gone by the time it finishes.
    g.required_volume = String::new();
    w.app.settings.set_general(&g).unwrap();
    w.usenet.state.lock().delay_ms = 40;
    assert_eq!(w.app.search(movie.id, Scope::Movie, false, true).await.unwrap().grabbed.len(), 1);
    g.required_volume = w.root.join("Movies").to_string_lossy().to_string();
    w.app.settings.set_general(&g).unwrap();
    let mut waiting = None;
    for _ in 0..300 {
        let a = w.app.db.title_acquisitions(movie.id).unwrap();
        if a.first().is_some_and(|a| a.error.as_deref().is_some_and(|e| e.contains("not mounted"))) {
            waiting = a.into_iter().next();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let waiting = waiting.expect("the import should be waiting for the volume");
    assert_eq!(waiting.state, AcqState::Downloading);
    assert!(tree(&w.root.join("Movies")).is_empty(), "no folders are created on the wrong disk");

    g.required_volume = String::new();
    w.app.settings.set_general(&g).unwrap();
    w.app.reconcile_all().await.unwrap();
    w.wait_state(movie.id, AcqState::Imported).await;
    assert_eq!(tree(&w.root.join("Movies")).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn unidentified_download_waits_in_the_attention_inbox_until_resolved() {
    let video = video_or_skip!();
    let w = World::new(Mode::Active).await;
    let (show, eps) = w.series("Mystery Show", 222, &[(1, 1, "One"), (1, 2, "Two")]);
    // Two unlabelled videos: Spool cannot know which episode is which.
    let mut post = Post::new();
    post.add_file("aaa.mkv", &video, 30_000);
    let mut second = video.clone();
    second.extend_from_slice(&bytes(1, 10));
    post.add_file("bbb.mkv", &second, 30_000);
    w.usenet.state.lock().articles.extend(post.articles.clone());
    let acq = w.app.add_manual_nzb(&show, vec![], "Mystery.Show.Stuff.1080p.WEB.nzb", &post.nzb()).unwrap();

    let blocked = w.wait_state(show.id, AcqState::ImportBlocked).await;
    assert!(blocked.error.unwrap().contains("which episodes"));
    // The notice is written just after the state changes.
    for _ in 0..40 {
        if !w.app.db.attention(false).unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let inbox = w.app.db.attention(false).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].acquisition_id, Some(acq.id));
    assert!(tree(&w.root.join("TV Shows")).is_empty());
    assert!(Path::new(blocked.output_path.as_ref().unwrap()).exists(), "the download is kept until someone decides");

    // Discarding it removes the download and clears the inbox.
    w.app.cancel_acquisition(acq.id, false).await.unwrap();
    assert!(w.app.db.attention(false).unwrap().is_empty());
    assert!(!Path::new(blocked.output_path.as_ref().unwrap()).exists());
    let _ = eps;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_recycling_off_replaced_and_deleted_files_are_removed_at_once() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let mut g = w.app.settings.general();
    g.recycle_days = 0;
    w.app.settings.set_general(&g).unwrap();

    let mut movie = w.movie("No Bin", 2018, "tt1212121");
    movie.path = w.root.join("Movies/No Bin (2018)").to_string_lossy().to_string();
    w.app.db.save_title(&mut movie).unwrap();
    std::fs::create_dir_all(&movie.path).unwrap();
    std::fs::write(Path::new(&movie.path).join("old.mkv"), b"old file").unwrap();
    let old: MediaFile = serde_json::from_value(serde_json::json!({"title_id": movie.id, "rel_path": "old.mkv", "size": 8, "quality": {"quality": "hdtv-1080p"}})).unwrap();
    w.app.db.with(|c| c.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1, 'old.mkv', 8, ?2)", rusqlite::params![movie.id, serde_json::to_string(&old).unwrap()])).unwrap();

    w.release("No.Bin.2018.1080p.BluRay.x264-NEW", &[("new.mkv", &video)]);
    w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    w.wait_state(movie.id, AcqState::Imported).await;
    assert_eq!(tree(&w.root.join("Movies")), ["No Bin (2018)/No Bin (2018) {imdb-tt1212121} [Bluray-1080p][AAC 2.0][x264]-NEW.mkv"], "the old file is gone and nothing was recycled");

    let file = w.app.db.files(movie.id).unwrap().remove(0);
    w.app.delete_file(file.id).unwrap();
    assert!(tree(&w.root.join("Movies")).is_empty());
    assert!(!Path::new(&movie.path).exists(), "the empty title folder is removed too");
    assert_eq!(w.app.recycle_usage(), (0, 0));

    // With recycling on, a deleted file is parked, counted, and can be emptied on demand.
    g.recycle_days = 7;
    w.app.settings.set_general(&g).unwrap();
    std::fs::create_dir_all(&movie.path).unwrap();
    std::fs::write(Path::new(&movie.path).join("again.mkv"), b"12345").unwrap();
    let again: MediaFile = serde_json::from_value(serde_json::json!({"title_id": movie.id, "rel_path": "again.mkv", "size": 5, "quality": {"quality": "hdtv-1080p"}})).unwrap();
    w.app.db.with(|c| c.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1, 'again.mkv', 5, ?2)", rusqlite::params![movie.id, serde_json::to_string(&again).unwrap()])).unwrap();
    let file = w.app.db.files(movie.id).unwrap().remove(0);
    w.app.delete_file(file.id).unwrap();
    assert_eq!(w.app.recycle_usage(), (1, 5));
    assert_eq!(w.app.clean_recycle(false).unwrap(), 0, "today's files are kept for the configured days");
    assert_eq!(w.app.clean_recycle(true).unwrap(), 5);
    assert_eq!(w.app.recycle_usage(), (0, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_the_disk_cannot_hold_is_not_started() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let movie = w.movie("Too Big", 2024, "tt9090909");
    w.release("Too.Big.2024.1080p.BluRay.x264-GRP", &[("t.mkv", &video)]);
    let free = spool::app::free_space(&w.root).expect("free space is readable");
    // Ask for more headroom than the disk has.
    let mut g = w.app.settings.general();
    g.min_free_gb = (free >> 30) as u32 + 50;
    w.app.settings.set_general(&g).unwrap();

    let err = w.app.check_space(3 << 30, false).unwrap_err().to_string();
    assert!(err.contains("not enough free space"), "{err}");
    let outcome = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert!(outcome.grabbed.is_empty());
    assert!(w.app.engine.jobs().is_empty());
    assert!(outcome.decisions.iter().any(|d| d.accepted), "the release is still acceptable; it just was not started");

    g.min_free_gb = 0;
    w.app.settings.set_general(&g).unwrap();
    if free > 8 << 30 {
        assert_eq!(w.app.search(movie.id, Scope::Movie, false, true).await.unwrap().grabbed.len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_releases_serve_a_search_when_the_indexer_has_nothing() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let mut movie = w.movie("Archive Example", 2021, "tt2222222");
    movie.tmdb_id = Some(2222);
    w.app.db.save_title(&mut movie).unwrap();
    w.release("Archive.Example.2021.1080p.BluRay.x264-BEST", &[("best.mkv", &video)]);
    let second = w.release("Archive.Example.2021.1080p.WEB-DL.H264-NEXT", &[("next.mkv", &video)]);

    let outcome = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert!(outcome.grabbed[0].release.title.ends_with("-BEST"));
    w.wait_state(movie.id, AcqState::Imported).await;

    // The chosen release is saved at once, the acceptable one beside it shortly after.
    let mut saved = vec![];
    for _ in 0..200 {
        saved = w.app.archive_for(Kind::Movie, movie.tmdb_id, movie.tvdb_id).unwrap();
        if saved.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let sources: HashMap<String, String> = saved.iter().map(|n| (n.release.title.clone(), n.source.clone())).collect();
    assert_eq!(sources["Archive.Example.2021.1080p.BluRay.x264-BEST"], "grabbed");
    assert_eq!(sources["Archive.Example.2021.1080p.WEB-DL.H264-NEXT"], "runner_up");
    assert!(saved.iter().all(|n| !n.release.link.contains("http")), "indexer addresses are not kept");

    // The file is deleted to make room, and the indexer no longer lists anything.
    let file = w.app.db.files(movie.id).unwrap().remove(0);
    w.app.delete_file(file.id).unwrap();
    w.indexer.lock().releases.clear();
    let again = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(again.grabbed.len(), 1, "{}", again.message);
    assert_eq!(again.grabbed[0].release.indexer, "Archive");
    assert!(again.grabbed[0].release.title.ends_with("-BEST"));
    for _ in 0..400 {
        if !w.app.db.files(movie.id).unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(w.app.db.files(movie.id).unwrap().len(), 1, "downloaded again from the saved NZB");

    // A saved release whose articles have since left the servers is shown, with the reason, and not chosen.
    {
        let mut st = w.usenet.state.lock();
        let ids: Vec<String> = st.articles.keys().filter(|k| k.starts_with(second[0].split('-').next().unwrap())).cloned().collect();
        st.missing.extend(ids);
    }
    let next = saved.iter().find(|n| n.source == "runner_up").unwrap();
    let checked = w.app.archive_check(next.id).await.unwrap();
    assert_eq!(checked.usable(), Some(false), "{:?}", checked.available);
    let last = w.app.search(movie.id, Scope::Movie, true, false).await.unwrap();
    let d = last.decisions.iter().find(|d| d.release.title.ends_with("-NEXT")).unwrap();
    assert!(d.rejections.iter().any(|r| r.code == "gone_from_usenet"), "{:?}", d.rejections);

    // Removing the title from the library leaves its saved releases, and they are found again
    // when the same film is added back.
    w.app.db.delete_title(movie.id).unwrap();
    assert_eq!(w.app.archive_all().unwrap().len(), 2);
    let mut back = w.movie("Archive Example", 2021, "tt2222222");
    back.tmdb_id = Some(2222);
    w.app.db.save_title(&mut back).unwrap();
    assert_eq!(w.app.archive_for(Kind::Movie, back.tmdb_id, None).unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_episode_is_named_and_the_notice_closes_when_it_arrives() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let (show, _) = w.series("Notice Example", 4242, &[(1, 1, "One"), (1, 2, "Two")]);
    let broken = w.release("Notice.Example.S01E01.1080p.WEB-DL.H264-BAD", &[("one.mkv", &video)]);
    w.release("Notice.Example.S01E02.1080p.WEB-DL.H264-OK", &[("two.mkv", &video)]);
    corrupt(&w, &broken[0]);

    let outcome = w.app.search(show.id, Scope::Missing, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 2, "{}", outcome.message);
    w.wait_state(show.id, AcqState::Imported).await;
    let mut open = vec![];
    for _ in 0..200 {
        open = w.app.db.attention(false).unwrap();
        if !open.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(open.len(), 1);
    assert!(open[0].message.contains("S01E01"), "{}", open[0].message);
    assert_eq!(open[0].data["release"], "Notice.Example.S01E01.1080p.WEB-DL.H264-BAD");

    // The other episode arriving does not close it; the failed one arriving does.
    w.release("Notice.Example.S01E01.1080p.WEB-DL.H264-FIXED", &[("one.fixed.mkv", &video)]);
    let retry = w.app.search(show.id, Scope::Missing, false, true).await.unwrap();
    assert_eq!(retry.grabbed.len(), 1, "{}", retry.message);
    for _ in 0..400 {
        if w.app.db.attention(false).unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(w.app.db.attention(false).unwrap().is_empty(), "closed once S01E01 is in the library");
    assert_eq!(w.app.db.files(show.id).unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn plex_items_are_matched_to_titles_and_the_differences_listed() {
    let w = World::new(Mode::Active).await;
    let by_id = w.movie("Matched By Id", 2001, "tt0000001");
    let by_name = w.movie("Matched By Name", 2002, "tt0000002");
    let absent = w.movie("Not In Plex", 2003, "tt0000003");
    w.app.db.with(|c| c.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1, 'x.mkv', 1, '{}')", [absent.id])).unwrap();

    async fn identity() -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({"MediaContainer": {"machineIdentifier": "abc123"}}))
    }
    async fn sections() -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({"MediaContainer": {"Directory": [{"key": "1", "type": "movie", "title": "Movies"}, {"key": "9", "type": "artist", "title": "Music"}]}}))
    }
    async fn all() -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({"MediaContainer": {"Metadata": [
            {"ratingKey": "10", "title": "Renamed In Plex", "year": 2001, "Guid": [{"id": "imdb://tt0000001"}], "Media": [{"Part": [{"file": "/m/a.mkv"}]}]},
            {"ratingKey": "11", "title": "Matched by Name", "year": 2003, "Media": [{"Part": [{"file": "/m/b.mkv"}]}]},
            {"ratingKey": "12", "title": "Only In Plex", "year": 1999, "Guid": [{"id": "tmdb://555"}], "Media": [{"Part": [{"file": "/m/Only In Plex (1999)/c.mkv"}]}]}
        ]}}))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new().route("/identity", get(identity)).route("/library/sections", get(sections)).route("/library/sections/1/all", get(all));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut g = w.app.settings.general();
    g.plex_url = format!("http://{addr}");
    g.plex_token = "t".into();
    w.app.settings.set_general(&g).unwrap();

    assert_eq!(w.app.plex_sync().await.unwrap(), "3 items read from Plex");
    let idx = w.app.plex_index().unwrap();
    let item = idx.find(&by_id).expect("matched by imdb id despite the different name");
    assert_eq!(item.rating_key, "10");
    assert_eq!(idx.url(item).unwrap(), "https://app.plex.tv/desktop/#!/server/abc123/details?key=%2Flibrary%2Fmetadata%2F10");
    assert_eq!(idx.find(&by_name).expect("matched by name, a year apart").rating_key, "11");
    assert!(idx.find(&absent).is_none());

    let report = w.app.plex_report().unwrap();
    assert_eq!((report.plex_items, report.matched), (3, 2));
    assert_eq!(report.plex_only.len(), 1);
    assert_eq!(report.plex_only[0]["title"], "Only In Plex");
    assert_eq!(report.plex_only[0]["can_track"], true);
    let spool_only: Vec<&str> = report.spool_only.iter().filter_map(|v| v["title"].as_str()).collect();
    assert_eq!(spool_only, ["Not In Plex"]);
}

/// One JSON-RPC call to the MCP endpoint.
async fn mcp(router: &axum::Router, key: Option<&str>, method: &str, params: serde_json::Value) -> (u16, serde_json::Value) {
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder().method("POST").uri("/mcp").header("content-type", "application/json");
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let resp = router.clone().oneshot(req.body(axum::body::Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 8 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

/// Call a tool and return (is_error, its JSON or text answer).
async fn tool(router: &axum::Router, key: &str, name: &str, args: serde_json::Value) -> (bool, serde_json::Value) {
    let (status, v) = mcp(router, Some(key), "tools/call", serde_json::json!({"name": name, "arguments": args})).await;
    assert_eq!(status, 200, "{v}");
    let text = v["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("no content: {v}")).to_string();
    (v["result"]["isError"] == true, serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ai_session_can_inspect_and_operate_spool_over_mcp() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let mut g = w.app.settings.general();
    g.password = "pw".into();
    g.api_key = "full-key".into();
    g.mcp_read_key = "read-key".into();
    w.app.settings.set_general(&g).unwrap();
    let router = spool::api::router(w.app.clone());

    // No key, no entry; the web sign-in is a separate matter.
    assert_eq!(mcp(&router, None, "tools/list", serde_json::json!({})).await.0, 401);
    assert_eq!(mcp(&router, Some("wrong"), "tools/list", serde_json::json!({})).await.0, 401);

    let (_, init) = mcp(&router, Some("full-key"), "initialize", serde_json::json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}})).await;
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(init["result"]["serverInfo"]["name"], "spool");

    let (_, all) = mcp(&router, Some("full-key"), "tools/list", serde_json::json!({})).await;
    let (_, read) = mcp(&router, Some("read-key"), "tools/list", serde_json::json!({})).await;
    let names = |v: &serde_json::Value| v["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert!(names(&all).contains(&"remove_title".to_string()) && names(&all).contains(&"diagnose".to_string()));
    assert!(names(&read).contains(&"status".to_string()) && !names(&read).contains(&"remove_title".to_string()));
    assert!(all["result"]["tools"].as_array().unwrap().iter().all(|t| t["inputSchema"]["type"] == "object" && t["description"].as_str().is_some_and(|d| d.len() > 30)));

    // Find the film, see why nothing has happened, search, download, watch it arrive.
    let mut movie = w.movie("Protocol Example", 2022, "tt3333333");
    movie.tmdb_id = Some(3333);
    w.app.db.save_title(&mut movie).unwrap();
    w.release("Protocol.Example.2022.1080p.BluRay.x264-GRP", &[("pe.mkv", &video)]);
    w.release("Protocol.Example.2022.720p.BluRay.x264-LOW", &[("low.mkv", &video)]);
    let (_, found) = tool(&router, "read-key", "find_title", serde_json::json!({"query": "protocol 2022"})).await;
    assert_eq!(found["in_library"][0]["title_id"], movie.id);

    let (_, why) = tool(&router, "read-key", "diagnose", serde_json::json!({"title_id": movie.id})).await;
    assert!(why["findings"].as_array().unwrap().iter().any(|f| f.as_str().unwrap().contains("never been searched")), "{why}");

    let (refused, msg) = tool(&router, "read-key", "search_releases", serde_json::json!({"title_id": movie.id, "grab": true})).await;
    assert!(refused && msg.as_str().unwrap().contains("read-only"));

    let (err, looked) = tool(&router, "full-key", "search_releases", serde_json::json!({"title_id": movie.id})).await;
    assert!(!err, "{looked}");
    assert_eq!(looked["acceptable"][0]["name"], "Protocol.Example.2022.1080p.BluRay.x264-GRP");
    assert!(looked["rejected"][0]["rejected_because"][0].as_str().is_some());
    assert!(looked["started"].as_array().unwrap().is_empty(), "looking does not download");
    let text = looked.to_string();
    assert!(!text.contains("http") && !text.contains("guid"), "no indexer addresses in answers: {text}");

    let (err, started) = tool(&router, "full-key", "download_release", serde_json::json!({"release_id": looked["acceptable"][0]["release_id"]})).await;
    assert!(!err && started["started"] == true, "{started}");
    w.wait_state(movie.id, AcqState::Imported).await;

    let (_, detail) = tool(&router, "read-key", "title_detail", serde_json::json!({"title_id": movie.id})).await;
    assert_eq!(detail["files"][0]["quality"], "bluray-1080p");
    assert_eq!(detail["saved_releases"][0]["kept_as"], "grabbed");
    let (_, status) = tool(&router, "read-key", "status", serde_json::json!({})).await;
    assert_eq!((status["mode"].as_str(), status["movies"].as_i64()), (Some("active"), Some(1)));
    let (_, act) = tool(&router, "read-key", "activity", serde_json::json!({})).await;
    assert_eq!(act["finished"][0]["state"], "imported");

    // Deleting files takes an explicit confirmation; removing the title alone does not.
    let (refused, msg) = tool(&router, "full-key", "remove_title", serde_json::json!({"title_id": movie.id, "delete_files": true})).await;
    assert!(refused && msg.as_str().unwrap().contains("confirm=true"), "{msg}");
    assert_eq!(w.app.db.files(movie.id).unwrap().len(), 1);
    let (err, gone) = tool(&router, "full-key", "remove_title", serde_json::json!({"title_id": movie.id})).await;
    assert!(!err && gone["files_deleted"] == false, "{gone}");
    assert!(w.app.db.title(movie.id).unwrap().is_none());
    assert_eq!(tree(&w.root.join("Movies")).len(), 1, "the file stays on disk");

    // The web API does not accept the read-only key.
    use tower::ServiceExt;
    let resp = router.clone().oneshot(axum::http::Request::builder().uri("/api/status").header("x-api-key", "read-key").body(axum::body::Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_series_search_asks_by_name_too_and_takes_the_complete_run() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let (show, eps) = w.series("Vision Example", 7700, &[(1, 1, "One"), (1, 2, "Two"), (1, 3, "Three"), (1, 4, "Four")]);
    // The indexer knows two better-quality episodes by the series id...
    w.release("Vision.Example.S01E01.1080p.BluRay.x265-IVY", &[("a.mkv", &video)]);
    w.release("Vision.Example.S01E03.1080p.BluRay.x265-IVY", &[("b.mkv", &video)]);
    // ...and has a whole run it never tagged, under a slightly different name.
    for n in 1..=4 {
        let name = format!("The.Vision.Example.S01E0{n}.1080p.WEB-DL.AAC2.0.H.264-OLDT");
        w.release(&name, &[(&format!("{n}.mkv"), &video)]);
        w.indexer.lock().untagged.insert(name);
    }

    let outcome = w.app.search(show.id, Scope::Missing, false, true).await.unwrap();
    let queries = w.indexer.lock().queries.clone();
    assert!(queries[0].contains("tvdbid=7700"), "{queries:?}");
    assert!(queries.iter().any(|q| q.contains("t=search") && q.contains("q=Vision%20Example")), "{queries:?}");
    let mut got: Vec<&str> = outcome.grabbed.iter().map(|a| a.release.title.as_str()).collect();
    got.sort();
    assert_eq!(got.len(), 4, "{}", outcome.message);
    assert!(got.iter().all(|t| t.ends_with("-OLDT")), "one complete run, not a mix: {got:?}");
    for _ in 0..400 {
        if w.app.db.episodes(show.id).unwrap().iter().all(|e| e.file_id.is_some()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(w.app.db.files(show.id).unwrap().len(), eps.len());

    // With nothing missing, a search by id is enough and the name is not asked again.
    let before = w.indexer.lock().queries.len();
    w.app.search(show.id, Scope::Missing, false, true).await.unwrap();
    assert_eq!(w.indexer.lock().queries.len(), before, "nothing is missing, so nothing is asked");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_release_gone_from_usenet_is_skipped_before_downloading() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let movie = w.movie("Vanished Example", 2018, "tt4444444");
    let gone = w.release("Vanished.Example.2018.1080p.BluRay.x264-GONE", &[("gone.mkv", &video)]);
    w.release("Vanished.Example.2018.1080p.WEB-DL.H264-HERE", &[("here.mkv", &video)]);
    // Half of the better release has expired.
    w.usenet.state.lock().missing.extend(gone.iter().step_by(2).cloned());

    let outcome = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "{}", outcome.message);
    assert!(outcome.grabbed[0].release.title.ends_with("-HERE"));
    let all = w.app.db.title_acquisitions(movie.id).unwrap();
    assert_eq!(all.len(), 1, "the dead release never became a download");
    assert_eq!(w.app.db.blocklisted(movie.id).unwrap().len(), 1);
    let again = w.app.search(movie.id, Scope::Movie, true, false).await.unwrap();
    assert!(again.decisions.iter().find(|d| d.release.title.ends_with("-GONE")).unwrap().rejections.iter().any(|r| r.code == "blocklisted"));
    w.wait_state(movie.id, AcqState::Imported).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_wait_for_space_under_one_notice() {
    let video = video_or_skip!();
    let w = World::new(Mode::Active).await;
    let movie = w.movie("Patient Example", 2017, "tt5555555");
    let mut post = Post::new();
    post.add_file("patient.example.2017.1080p.bluray.x264-grp.mkv", &video, 30_000);
    // The server is down for now, so the download sits in the queue while space is judged.
    w.usenet.state.lock().down = true;
    w.usenet.state.lock().articles.extend(post.articles.clone());
    let free = spool::app::free_space(&w.root).unwrap();
    let mut g = w.app.settings.general();
    g.min_free_gb = (free >> 30) as u32 + 50;
    w.app.settings.set_general(&g).unwrap();
    let acq = w.app.add_manual_nzb(&movie, vec![], "Patient.Example.2017.1080p.BluRay.x264-GRP.nzb", &post.nzb()).unwrap();

    w.app.watch_space();
    let job = w.app.engine.job(acq.job_id.as_ref().unwrap()).unwrap();
    assert_eq!(job.message, "Waiting for free space");
    let notices = w.app.db.attention(false).unwrap();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].kind == "space" && notices[0].message.starts_with("1 download"), "{}", notices[0].message);
    w.app.watch_space();
    assert_eq!(w.app.db.attention(false).unwrap().len(), 1, "one notice, however often it is checked");

    // Room appears: the notice closes and the download goes ahead by itself.
    g.min_free_gb = 0;
    w.app.settings.set_general(&g).unwrap();
    w.usenet.state.lock().down = false;
    w.app.watch_space();
    assert!(w.app.db.attention(false).unwrap().is_empty());
    w.wait_state(movie.id, AcqState::Imported).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_compacting_search_replaces_an_oversized_file_with_one_that_fits() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let movie = w.movie("Hefty Example", 2016, "tt6666666");
    w.release("Hefty.Example.2016.1080p.BluRay.x264-BIG", &[("big.mkv", &video)]);
    w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    w.wait_state(movie.id, AcqState::Imported).await;

    // The profile now aims for at most 20 GB, and the file on disk is taken to be 40 GB.
    let mut profile = w.app.db.profile(w.profile("movie")).unwrap().unwrap();
    profile.target_size_gb = Some((1.0, 20.0));
    w.app.db.save_profile(&mut profile).unwrap();
    w.app
        .db
        .with(|c| c.execute("UPDATE files SET size = ?1, data = json_set(data, '$.size', ?1) WHERE title_id = ?2", rusqlite::params![40i64 << 30, movie.id]))
        .unwrap();
    assert_eq!(w.app.db.files(movie.id).unwrap()[0].size, 40 << 30);
    w.indexer.lock().releases[0].1 = 40 << 30;
    // The indexer reports 3 GB for this lower-quality release.
    w.release("Hefty.Example.2016.1080p.WEB-DL.H264-TRIM", &[("trim.mkv", &video)]);

    // An ordinary search does not go down in quality...
    let normal = w.app.search(movie.id, Scope::Movie, true, true).await.unwrap();
    assert!(normal.grabbed.is_empty());
    let trim = normal.decisions.iter().find(|d| d.release.title.ends_with("-TRIM")).unwrap();
    assert_eq!(trim.rejections[0].code, "not_an_upgrade");

    // ...a compacting one does, because the copy fits the target and is much smaller.
    let compact = w.app.search(movie.id, Scope::Compact, true, true).await.unwrap();
    assert_eq!(compact.grabbed.len(), 1, "{}", compact.message);
    assert!(compact.grabbed[0].release.title.ends_with("-TRIM"));
    for _ in 0..400 {
        if w.app.db.files(movie.id).unwrap().first().is_some_and(|f| f.quality.quality == Quality::Webdl1080p) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let files = w.app.db.files(movie.id).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].quality.quality, Quality::Webdl1080p);
    assert!(tree(&w.root.join("Movies")).iter().any(|p| p.contains(".spool-recycle") && p.contains("-BIG")), "the replaced file waits in the recycle folder");

    // With everything inside the target there is nothing left to do, and no indexer is asked.
    let hits = w.indexer.lock().hits;
    let again = w.app.search(movie.id, Scope::Compact, true, true).await.unwrap();
    assert!(again.grabbed.is_empty() && again.message.starts_with("Nothing to compact"), "{}", again.message);
    assert_eq!(w.indexer.lock().hits, hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daily_allowance_is_kept_for_searches_a_person_starts() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let mut ix = w.app.db.indexers().unwrap().remove(0);
    ix.daily_requests = 10;
    ix.daily_grabs = 1;
    w.app.db.save_indexer(&mut ix).unwrap();
    let one = w.movie("Allowance One", 2011, "tt7000001");
    let two = w.movie("Allowance Two", 2012, "tt7000002");
    w.release("Allowance.One.2011.1080p.BluRay.x264-GRP", &[("one.mkv", &video)]);
    w.release("Allowance.Two.2012.1080p.BluRay.x264-GRP", &[("two.mkv", &video)]);

    // The first download uses the day's one allowed download.
    let first = w.app.search(one.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(first.grabbed.len(), 1, "{}", first.message);
    let budget = w.app.indexer_budgets()[&ix.id].clone();
    assert_eq!((budget.grabs, budget.grab_limit, budget.source.as_deref()), (1, 1, Some("set")));
    assert!(budget.requests >= 1);

    // Spool does not spend past it on its own account...
    let second = w.app.search(two.id, Scope::Movie, false, true).await.unwrap();
    assert!(second.grabbed.is_empty());
    assert!(second.message.contains("allowance for today is used up"), "{}", second.message);
    // ...but a person asking for it gets it.
    let asked = w.app.search(two.id, Scope::Movie, true, true).await.unwrap();
    assert_eq!(asked.grabbed.len(), 1, "{}", asked.message);

    // Once nine in ten requests are gone, background searches stop asking; a person's still go.
    w.app.db.count_indexer(ix.id, 9, 0).unwrap();
    let hits = w.indexer.lock().hits;
    let quiet = w.app.search(one.id, Scope::Movie, false, false).await.unwrap();
    assert_eq!(w.indexer.lock().hits, hits, "not asked");
    assert!(quiet.errors.iter().any(|e| e.contains("kept for searches you start")), "{:?}", quiet.errors);
    w.app.search(one.id, Scope::Movie, true, false).await.unwrap();
    assert!(w.indexer.lock().hits > hits);
}

#[tokio::test(flavor = "multi_thread")]
async fn anime_numbering_and_dubs_are_handled() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let (show, eps) = w.series("Titan Example", 8800, &[(1, 1, "A"), (1, 2, "B"), (2, 1, "C"), (2, 2, "D")]);
    // Episode 3 by a single count across seasons is the first of season two.
    w.release("[SubGroup] Titan Example - 03 [1080p][ABCD1234]", &[("[SubGroup] Titan Example - 03 [1080p].mkv", &video)]);
    // Two releases of the same episode: one dubbed into German with the original alongside.
    w.release("Titan.Example.S01E01.German.DL.1080p.BluRay.x264-DUB", &[("dub.mkv", &video)]);
    w.release("Titan.Example.S01E01.1080p.WEB-DL.H264-PLAIN", &[("plain.mkv", &video)]);

    let outcome = w.app.search(show.id, Scope::Missing, false, true).await.unwrap();
    let grabbed: HashMap<i64, &str> = outcome.grabbed.iter().map(|a| (a.episode_ids[0], a.release.title.as_str())).collect();
    assert_eq!(grabbed.get(&eps[2]).copied(), Some("[SubGroup] Titan Example - 03 [1080p][ABCD1234]"), "{}: {:?}", outcome.message, outcome.decisions.iter().map(|d| (&d.release.title, &d.rejections)).collect::<Vec<_>>());
    assert!(grabbed[&eps[0]].ends_with("-PLAIN"), "the plain release beats the dub despite lower quality: {grabbed:?}");
    for _ in 0..400 {
        if w.app.db.files(show.id).unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let tree = tree(&w.root.join("TV Shows"));
    assert!(tree.iter().any(|p| p.contains("S02E01")), "filed under its season and episode: {tree:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_film_the_indexer_never_linked_to_its_id_is_found_by_name() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let movie = w.movie("Legend of the Example Heroes: My Conquest Is the Sea of Stars", 1988, "tt0095512");
    let name = "Legend.of.the.Example.Heroes.My.Conquest.Is.the.Sea.of.Stars.1988.BD.1080p.HEVC.FLAC";
    w.release(name, &[("film.mkv", &video)]);
    w.indexer.lock().untagged.insert(name.to_string());

    let outcome = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    let queries = w.indexer.lock().queries.clone();
    assert!(queries[0].contains("imdbid=0095512"), "{queries:?}");
    // Asked again in words, without the colon, and with the anime category alongside the movie ones.
    let by_name = queries.iter().find(|q| q.contains("t=search")).expect("a search by name");
    assert!(by_name.contains("q=Legend%20of%20the%20Example%20Heroes%20My%20Conquest%20Is%20the%20Sea%20of%20Stars%201988") && by_name.contains(",5070"), "{by_name}");
    assert_eq!(outcome.grabbed.len(), 1, "{}", outcome.message);
    w.wait_state(movie.id, AcqState::Imported).await;

    // A film found by its id needs no second question.
    let other = w.movie("Tagged Example", 2005, "tt0000042");
    w.release("Tagged.Example.2005.1080p.BluRay.x264-GRP", &[("t.mkv", &video)]);
    let before = w.indexer.lock().queries.len();
    w.app.search(other.id, Scope::Movie, false, false).await.unwrap();
    assert_eq!(w.indexer.lock().queries.len(), before + 1);
}
