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

/// A small real video that runs for `secs`, made once per length.
fn video_of(secs: u32) -> Option<Vec<u8>> {
    static MADE: std::sync::OnceLock<Mutex<HashMap<u32, Option<Vec<u8>>>>> = std::sync::OnceLock::new();
    let mut made = MADE.get_or_init(Default::default).lock();
    made.entry(secs)
        .or_insert_with(|| {
            let dir = tempfile::tempdir().ok()?;
            let out = dir.path().join("v.mkv");
            let rate = if secs > 600 { 1 } else { 5 };
            let ok = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-f", "lavfi", "-i", &format!("testsrc=size=64x48:rate={rate}:duration={secs}"), "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-y"])
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
        Self::with_sources(mode, None).await
    }

    /// A world whose film and series details come from the given addresses.
    async fn with_sources(mode: Mode, sources: Option<(&str, &str)>) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let usenet = FakeServer::start(HashMap::new()).await;
        let indexer: Shared = Default::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = axum::Router::new().route("/api", get(feed)).route("/nzb/{i}", get(nzb)).with_state(indexer.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let db = Db::open(&root.join("data/spool.db")).unwrap();
        let app = App::start_with(&root.join("data"), db, sources).await.unwrap();
        let mut g = app.settings.general();
        g.mode = mode;
        g.movie_root = root.join("Movies").to_string_lossy().to_string();
        g.series_root = root.join("TV Shows").to_string_lossy().to_string();
        g.downloads_dir = root.join("downloads").to_string_lossy().to_string();
        // How much room the machine running the tests happens to have is not what is being
        // tested; the tests about room set this for themselves.
        g.min_free_gb = 0;
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
        // Someone to play as: tests that are about accounts make more.
        app.db.add_account("spool", "").unwrap();
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
                    .upsert_episode(&Episode { id: 0, title_id: t.id, season: *s, episode: *e, absolute: None, title: name.to_string(), overview: String::new(), air_date: Some("2016-10-02".into()), air_date_utc: None, runtime: 0, monitored: true, file_id: None, still: None, rating: None })
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
async fn a_search_lists_what_it_found_best_first_and_says_which_can_be_heard() {
    let mut w = World::new(Mode::Active).await;
    let mut film = w.movie("Spirit Away", 2001, "tt3333333");
    film.original_language = Some("ja".into());
    w.app.db.save_title(&mut film).unwrap();
    w.release("Spirit.Away.2001.1080p.BluRay.x264-SUB", &[("a.mkv", b"a")]);
    w.release("Spirit.Away.2001.1080p.WEB-DL.DUAL.x264-DUB", &[("b.mkv", b"b")]);
    w.release("Spirit.Away.2001.720p.WEB-DL.x264-WEB", &[("c.mkv", b"c")]);
    w.release("Spirit.Away.2001.DVDRip.DUAL.x264-OLD", &[("d.mkv", b"d")]);

    let outcome = w.app.search(film.id, Scope::Movie, true, false).await.unwrap();
    let order: Vec<(&str, bool, bool)> = outcome.decisions.iter().map(|d| (d.release.title.as_str(), d.accepted, d.speaks_wanted)).collect();
    assert_eq!(&order[..2], &[("Spirit.Away.2001.1080p.WEB-DL.DUAL.x264-DUB", true, true), ("Spirit.Away.2001.1080p.BluRay.x264-SUB", true, false)], "{order:?}");
    // What was turned down is ranked the same way, so the best of it can be taken by hand.
    assert_eq!(&order[2..], &[("Spirit.Away.2001.DVDRip.DUAL.x264-OLD", false, true), ("Spirit.Away.2001.720p.WEB-DL.x264-WEB", false, false)], "{order:?}");
    assert!(outcome.decisions.iter().all(|d| d.id > 0), "each can be grabbed by its id");
}

#[tokio::test(flavor = "multi_thread")]
async fn anime_without_the_household_language_is_fetched_again_with_it_once() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let mut film = w.movie("Star Drift", 2001, "tt4444444");
    film.original_language = Some("ja".into());
    film.anilist = vec![1];
    w.app.db.save_title(&mut film).unwrap();
    w.release("Star.Drift.2001.1080p.WEB-DL.x264-JPN", &[("a.mkv", &video)]);
    let outcome = w.app.search(film.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "{}", outcome.message);
    w.wait_state(film.id, AcqState::Imported).await;
    // What was imported turns out to have Japanese only.
    let app = w.app.clone();
    let heard = |scene: &str| {
        let mut v = serde_json::to_value(app.db.files(film.id).unwrap().remove(0)).unwrap();
        v["media_info"]["audio_languages"] = serde_json::json!(["jpn"]);
        v["scene_name"] = serde_json::json!(scene);
        app.db.save_file(&serde_json::from_value(v).unwrap()).unwrap();
    };
    heard("Star.Drift.2001.1080p.WEB-DL.x264-JPN");
    w.release("Star.Drift.2001.1080p.WEB-DL.DUAL.x264-DUB", &[("b.mkv", &video)]);
    let dub = |o: &spool::acquire::SearchOutcome| o.decisions.iter().find(|d| d.release.title.ends_with("-DUB")).map(|d| d.rejections.iter().map(|r| r.code.clone()).collect::<Vec<_>>()).unwrap();
    let found = w.app.search(film.id, Scope::Movie, true, false).await.unwrap();
    assert!(dub(&found).is_empty(), "the same quality with English is worth having: {:?}", dub(&found));

    // A file whose release already promised English is not replaced for it again.
    heard("Star.Drift.2001.1080p.WEB-DL.DUAL.x264-OLD");
    assert_eq!(dub(&w.app.search(film.id, Scope::Movie, true, false).await.unwrap()), ["not_an_upgrade"]);
    // Nor is a film that is not anime: it is kept in the language it was made in.
    heard("Star.Drift.2001.1080p.WEB-DL.x264-JPN");
    film.anilist = vec![];
    w.app.db.save_title(&mut film).unwrap();
    assert_eq!(dub(&w.app.search(film.id, Scope::Movie, true, false).await.unwrap()), ["not_an_upgrade"]);
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
async fn two_downloads_of_one_film_finishing_together_are_filed_one_after_the_other() {
    let video = video_or_skip!();
    let w = World::new(Mode::Active).await;
    let mut movie = w.movie("Cytotropism", 2006, "tt0791205");
    movie.path = w.root.join("Movies/Cytotropism (2006)").to_string_lossy().to_string();
    w.app.db.save_title(&mut movie).unwrap();
    let mut ids = vec![];
    for (name, quality) in [("Cytotropism.2006.1080p.WEB-DL.DDP2.0.H.264-GRP", "webdl-1080p"), ("Cytotropism.2006.1080p.BluRay.REMUX.AVC.TrueHD-GRP", "remux-1080p")] {
        let folder = w.root.join("downloads/complete").join(name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(format!("{name}.mkv")), &video).unwrap();
        let mut acq: Acquisition = serde_json::from_value(serde_json::json!({
            "title_id": movie.id, "state": "downloading", "output_path": folder.to_string_lossy(), "reason": "chosen by hand",
            "release": {"guid": name, "title": name, "indexer_id": 1, "indexer": "Test", "link": "", "info_url": "", "size": video.len(), "published": 0, "imdb_id": null, "tvdb_id": null, "indexer_priority": 25},
            "quality": {"quality": quality},
        })).unwrap();
        w.app.db.save_acquisition(&mut acq).unwrap();
        ids.push(acq.id);
    }
    let (a, b) = tokio::join!(w.app.import(ids[0], None), w.app.import(ids[1], None));
    a.unwrap();
    b.unwrap();
    // The second is planned with the first's file in place: it replaces it or is held back,
    // and never lies beside it belonging to nothing.
    let files: Vec<MediaFile> = w.app.db.files(movie.id).unwrap();
    assert_eq!(files.len(), 1, "{:?}", files.iter().map(|f| &f.rel_path).collect::<Vec<_>>());
    let kept: Vec<String> = tree(&w.root.join("Movies")).into_iter().filter(|p| !p.starts_with(".spool-recycle")).collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
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

    let err = w.app.check_space(&movie, 3 << 30, false).unwrap_err().to_string();
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
#[tokio::test(flavor = "multi_thread")]
async fn what_was_watched_in_plex_is_brought_to_an_account_and_nothing_is_unmarked() {
    use axum::extract::Query;
    use serde_json::{json, Value};
    let w = World::new(Mode::Active).await;
    let seen = w.movie("Seen In Plex", 2001, "tt0000001");
    let half = w.movie("Half Seen In Plex", 2002, "tt0000002");
    let here = w.movie("Seen Here Only", 2003, "tt0000003");
    let (show, eps) = w.series("Example World", 296762, &[(1, 1, "The Original"), (1, 2, "Chestnut")]);
    w.app.db.add_account("Ada", "").unwrap();
    // Seen here already, and part-way through another: neither is Plex's to change.
    w.app.db.save_watched(1, "movie", here.id, &Watched { played: true, play_count: 1, ..Default::default() }).unwrap();
    w.app.db.save_watched(1, "episode", eps[1], &Watched { position: 90.0, ..Default::default() }).unwrap();

    async fn sections() -> axum::Json<Value> {
        axum::Json(json!({"MediaContainer": {"Directory": [{"key": "1", "type": "movie", "title": "Movies"}, {"key": "2", "type": "show", "title": "TV"}]}}))
    }
    async fn films() -> axum::Json<Value> {
        axum::Json(json!({"MediaContainer": {"Metadata": [
            {"ratingKey": "10", "title": "Seen In Plex", "year": 2001, "Guid": [{"id": "imdb://tt0000001"}], "viewCount": 2, "lastViewedAt": 1_700_000_000},
            {"ratingKey": "11", "title": "Half Seen In Plex", "year": 2002, "Guid": [{"id": "imdb://tt0000002"}], "viewOffset": 600_000, "lastViewedAt": 1_700_000_100},
            {"ratingKey": "12", "title": "Seen Here Only", "year": 2003, "Guid": [{"id": "imdb://tt0000003"}]},
            {"ratingKey": "13", "title": "Not In Spool", "year": 1999, "viewCount": 1}
        ]}}))
    }
    async fn shows(Query(q): Query<HashMap<String, String>>) -> axum::Json<Value> {
        axum::Json(if q.get("type").is_some_and(|t| t == "4") {
            json!({"MediaContainer": {"Metadata": [
                {"ratingKey": "21", "grandparentRatingKey": "20", "parentIndex": 1, "index": 1, "viewCount": 1, "lastViewedAt": 1_700_000_200},
                {"ratingKey": "22", "grandparentRatingKey": "20", "parentIndex": 1, "index": 2, "viewOffset": 30_000},
                {"ratingKey": "23", "grandparentRatingKey": "20", "parentIndex": 9, "index": 9, "viewCount": 1},
                {"ratingKey": "31", "grandparentRatingKey": "30", "parentIndex": 1, "index": 1, "viewCount": 1}
            ]}})
        } else {
            json!({"MediaContainer": {"Metadata": [{"ratingKey": "20", "title": "Example World", "year": 2016, "Guid": [{"id": "tvdb://296762"}], "leafCount": 2, "Location": [{"path": "/tv/Example World"}]}]}})
        })
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .route("/identity", get(|| async { axum::Json(json!({"MediaContainer": {"machineIdentifier": "abc123"}})) }))
        .route("/library/sections", get(sections))
        .route("/library/sections/1/all", get(films))
        .route("/library/sections/2/all", get(shows));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut g = w.app.settings.general();
    g.plex_url = format!("http://{addr}");
    g.plex_token = "t".into();
    w.app.settings.set_general(&g).unwrap();
    let api = spool::api::router(w.app.clone());

    // Spool had not read Plex's library; bringing the history over reads it first.
    // Turned off, nothing is asked of Plex, though its address and token are still there.
    let mut g = w.app.settings.general();
    g.plex_on = false;
    w.app.settings.set_general(&g).unwrap();
    let (status, said) = call(&api, "POST", "/api/accounts/1/plex", Value::Null).await;
    assert!(status >= 400 && said.to_string().contains("turned off"), "{status} {said}");
    let (_, report) = call(&api, "GET", "/api/plex", Value::Null).await;
    assert_eq!((report["configured"].as_bool(), report["off"].as_bool()), (Some(false), Some(true)), "{report}");
    assert!(!w.app.settings.general().plex_token.is_empty());
    g.plex_on = true;
    w.app.settings.set_general(&g).unwrap();
    let (status, brought) = call(&api, "POST", "/api/accounts/1/plex", Value::Null).await;
    assert_eq!((status, brought.to_string().as_str()), (200, r#"{"episodes":1,"films":1,"resumed":1}"#));
    let of = |account: i64, kind: &str, id: i64| w.app.db.watched(account, kind, id).unwrap();
    let film = of(1, "movie", seen.id);
    assert_eq!((film.played, film.play_count, film.last_played_at), (true, 2, 1_700_000_000));
    let part = of(1, "movie", half.id);
    assert_eq!((part.played, part.position, part.last_played_at), (false, 600.0, 1_700_000_100), "carried on from where Plex had it");
    assert!(of(1, "movie", here.id).played, "what was watched here stays watched though Plex has not seen it");
    assert!(of(1, "episode", eps[0]).played);
    assert_eq!(of(1, "episode", eps[1]).position, 90.0, "a place kept here is not moved to Plex's");
    assert_eq!(w.app.db.files(show.id).unwrap().len(), 0, "nothing but what was watched is touched");
    assert!(!of(2, "movie", seen.id).played, "one account's, and nobody else's");
    // Asked again, there is nothing left to bring; and another account may have the same.
    let (_, again) = call(&api, "POST", "/api/accounts/1/plex", Value::Null).await;
    assert_eq!(again.to_string(), r#"{"episodes":0,"films":0,"resumed":0}"#);
    let (_, hers) = call(&api, "POST", "/api/accounts/2/plex", Value::Null).await;
    assert_eq!(hers.to_string(), r#"{"episodes":1,"films":1,"resumed":2}"#);
    assert_eq!(call(&api, "POST", "/api/accounts/99/plex", Value::Null).await.0, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn someone_who_plays_can_ask_for_a_title_and_see_when_it_is_ready() {
    use serde_json::{json, Value};
    let source = details_source().await;
    let w = World::with_sources(Mode::Shadow, Some((&source, &source))).await;
    let mut g = w.app.settings.general();
    g.tmdb_api_key = "key".into();
    g.players_enabled = true;
    w.app.settings.set_general(&g).unwrap();
    w.app.db.add_account("Ada", &spool::settings::hash_password("her-own").unwrap()).unwrap();
    let (api, jf) = (spool::api::router(w.app.clone()), spool::jellyfin::router(w.app.clone()));
    // Someone has a password, so the app is asked through its key.
    let key = w.app.settings.general().api_key;
    let by = Some(("x-api-key", key.as_str()));
    let read = |bytes: &[u8]| serde_json::from_slice::<Value>(bytes).unwrap_or(Value::Null);

    // The players' address gives a browser a page to ask from, which signs in as a player does.
    let (status, head, body) = player(&jf, "GET", "/", None, Value::Null).await;
    assert_eq!((status, head["content-type"].to_str().unwrap()), (200, "text/html; charset=utf-8"));
    assert!(String::from_utf8_lossy(&body).contains("Who is asking?"));
    let sign_in = |name: &'static str, pw: &'static str| {
        let jf = jf.clone();
        async move { read(&player(&jf, "POST", "/Users/AuthenticateByName", None, json!({"Username": name, "Pw": pw})).await.2)["AccessToken"].as_str().unwrap().to_string() }
    };
    let (me, ada) = (sign_in("spool", "").await, sign_in("Ada", "her-own").await);
    let (me, ada) = (Some(me.as_str()), Some(ada.as_str()));
    let asks_of = |who: Option<&str>| {
        let jf = jf.clone();
        let who = who.map(str::to_string);
        async move { serde_json::from_slice::<Value>(&player(&jf, "GET", "/spool/asks", who.as_deref(), Value::Null).await.2).unwrap() }
    };
    let waiting = || {
        let api = api.clone();
        async move { call_as(&api, by, "GET", "/api/status", Value::Null).await.1["asks"].as_u64() }
    };

    // Nothing is found or asked for without signing in.
    assert_eq!(player(&jf, "GET", "/spool/find?q=Heat", None, Value::Null).await.0, 401);
    assert_eq!(player(&jf, "POST", "/spool/asks", None, json!({"kind": "movie", "tmdb_id": 949})).await.0, 401);
    let (status, _, body) = player(&jf, "GET", "/spool/find?q=Heat", me, Value::Null).await;
    let found = read(&body);
    assert_eq!((status, found.as_array().unwrap().len(), found[0]["title"].as_str(), found[0]["tmdb_id"].as_u64(), found[0]["status"].as_str()), (200, 1, Some("Heat"), Some(949), Some("")), "{found}");

    // Asked for, it waits; asked for twice, it is one ask; and what it is called is read
    // from where titles are looked up, whatever the page sent.
    let (status, _, body) = player(&jf, "POST", "/spool/asks", me, json!({"kind": "movie", "tmdb_id": 949, "title": "Something Else"})).await;
    let asked = read(&body);
    assert_eq!((status, asked["status"].as_str(), asked["title"].as_str(), asked["year"].as_u64()), (200, Some("asked"), Some("Heat"), Some(1995)), "{asked}");
    let (_, _, body) = player(&jf, "POST", "/spool/asks", me, json!({"kind": "movie", "tmdb_id": 949})).await;
    assert_eq!(read(&body)["id"], asked["id"]);
    assert_eq!(player(&jf, "POST", "/spool/asks", me, json!({"kind": "movie"})).await.0, 400);
    assert_eq!(player(&jf, "POST", "/spool/asks", me, json!({"kind": "music", "tmdb_id": 1})).await.0, 400);
    let (_, _, body) = player(&jf, "GET", "/spool/find?q=Heat", me, Value::Null).await;
    assert_eq!(read(&body)[0]["status"], "asked");
    let (_, _, body) = player(&jf, "POST", "/spool/asks", ada, json!({"kind": "movie", "tmdb_id": 949})).await;
    let hers = read(&body);
    assert_ne!(hers["id"], asked["id"], "each has an ask of their own");
    assert_eq!(waiting().await, Some(2));
    assert_eq!(asks_of(me).await.as_array().unwrap().len(), 1, "one sees one's own asks and no one else's");
    assert!(w.app.db.titles(None).unwrap().is_empty(), "nothing is added until someone says so");

    // Whoever runs Spool sees who asked for what, and adding it answers everyone who did.
    let (_, all) = call_as(&api, by, "GET", "/api/asks", Value::Null).await;
    assert_eq!(all.as_array().unwrap().iter().map(|a| (a["account"].as_str().unwrap(), a["status"].as_str().unwrap())).collect::<Vec<_>>(), [("Ada", "asked"), ("spool", "asked")], "{all}");
    let (status, added) = call_as(&api, by, "POST", &format!("/api/asks/{}/add", asked["id"]), Value::Null).await;
    assert_eq!(status, 200, "{added}");
    let heat = w.app.db.title(added["title_id"].as_i64().unwrap()).unwrap().unwrap();
    assert_eq!((heat.title.as_str(), heat.monitored), ("Heat", true));
    assert_eq!(waiting().await, Some(0));
    assert_eq!((asks_of(me).await[0]["status"].as_str(), asks_of(ada).await[0]["status"].as_str()), (Some("coming"), Some("coming")));
    assert_eq!(call_as(&api, by, "POST", &format!("/api/asks/{}/add", hers["id"]), Value::Null).await.0, 200, "saying so twice does no harm");
    assert_eq!(w.app.db.titles(None).unwrap().len(), 1);
    // Once there is something to play, whoever asked is told so.
    w.app.db.with(|c| c.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1, 'x.mkv', 1, '{}')", [heat.id])).unwrap();
    assert_eq!(asks_of(ada).await[0]["status"], "ready");
    let (_, _, body) = player(&jf, "GET", "/spool/find?q=Heat", ada, Value::Null).await;
    assert_eq!(read(&body)[0]["status"], "ready");

    // Turned down, it says so, and may be asked for again as a new ask.
    let (_, _, body) = player(&jf, "POST", "/spool/asks", ada, json!({"kind": "movie", "tmdb_id": 593})).await;
    let solaris = read(&body);
    assert_eq!(call_as(&api, by, "POST", &format!("/api/asks/{}/decline", solaris["id"]), Value::Null).await.0, 200);
    assert_eq!(call_as(&api, by, "POST", &format!("/api/asks/{}/burn", solaris["id"]), Value::Null).await.0, 404);
    assert_eq!((asks_of(ada).await[0]["status"].as_str(), waiting().await), (Some("declined"), Some(0)));
    let (_, _, body) = player(&jf, "POST", "/spool/asks", ada, json!({"kind": "movie", "tmdb_id": 593})).await;
    let again = read(&body);
    assert_eq!((again["status"].as_str(), again["answered_at"].as_i64(), waiting().await), (Some("asked"), Some(0), Some(1)));
    // An ask is taken back by whoever made it, and by nobody else.
    assert_eq!(player(&jf, "DELETE", &format!("/spool/asks/{}", again["id"]), me, Value::Null).await.0, 404);
    assert_eq!(player(&jf, "DELETE", &format!("/spool/asks/{}", again["id"]), ada, Value::Null).await.0, 200);
    assert_eq!((asks_of(ada).await.as_array().unwrap().len(), waiting().await), (1, Some(0)));

    // An account whose asks are added at once does not wait; and what the library has
    // already is not asked for twice over: it only remains to say when it is ready.
    assert_eq!(call_as(&api, by, "PUT", "/api/accounts/2", json!({"adds": true})).await.0, 200);
    let (_, people) = call_as(&api, by, "GET", "/api/accounts", Value::Null).await;
    assert_eq!((people["accounts"][0]["adds"].as_bool(), people["accounts"][1]["adds"].as_bool()), (Some(false), Some(true)));
    let (_, _, body) = player(&jf, "POST", "/spool/asks", ada, json!({"kind": "series", "tvmaze_id": 77})).await;
    let series = read(&body);
    assert_eq!((series["status"].as_str(), series["title"].as_str(), waiting().await), (Some("coming"), Some("Example World"), Some(0)), "{series}");
    assert_eq!(w.app.db.titles(Some(Kind::Series)).unwrap().len(), 1);
    let (_, _, body) = player(&jf, "POST", "/spool/asks", me, json!({"kind": "series", "tvmaze_id": 77})).await;
    assert_eq!((read(&body)["status"].as_str(), read(&body)["title_id"] == series["title_id"], w.app.db.titles(None).unwrap().len()), (Some("coming"), true, 2));
    // Removed from the library, what was asked for says it has gone, and a title added
    // afterwards under the number it had is not mistaken for it. Asked for again, it waits again.
    w.app.db.delete_title(heat.id).unwrap();
    let heat_of = |asks: Value| asks.as_array().unwrap().iter().find(|a| a["title"] == "Heat").unwrap().clone();
    assert_eq!(heat_of(asks_of(me).await)["status"], "gone");
    let other = w.movie("Something Else", 1990, "tt0000990");
    assert_eq!((other.id >= heat.id, heat_of(asks_of(me).await)["status"].as_str()), (true, Some("gone")));
    let (_, _, body) = player(&jf, "POST", "/spool/asks", me, json!({"kind": "movie", "tmdb_id": 949})).await;
    assert_eq!((read(&body)["status"].as_str(), waiting().await), (Some("asked"), Some(1)));
    // Two asks of one title at once make one ask.
    let twice = |jf: axum::Router, who: String| async move { player(&jf, "POST", "/spool/asks", Some(&who), json!({"kind": "movie", "tmdb_id": 593})).await.0 };
    let who = me.unwrap().to_string();
    assert_eq!(tokio::join!(twice(jf.clone(), who.clone()), twice(jf.clone(), who.clone())), (200, 200));
    assert_eq!(asks_of(me).await.as_array().unwrap().iter().filter(|a| a["title"] == "Solaris").count(), 1);
    // Looking up is counted, and past a point in a minute it is refused.
    let mut refused = 0;
    for _ in 0..40 {
        refused += (player(&jf, "GET", "/spool/find?q=Heat", me, Value::Null).await.0 == 400) as u32;
    }
    assert!((8..=20).contains(&refused), "{refused} of forty were refused");
    assert_eq!(call_as(&api, by, "DELETE", "/api/accounts/2", Value::Null).await.0, 200);
    let (_, all) = call_as(&api, by, "GET", "/api/asks", Value::Null).await;
    assert!(all.as_array().unwrap().iter().all(|a| a["account"] == "spool"), "{all}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_held_to_compact_versions_is_given_only_those_while_away() {
    use serde_json::{json, Value};
    let w = World::new(Mode::Active).await;
    let api = spool::api::router(w.app.clone());
    // Two films and a series of two episodes. One film and one episode have a compact version.
    let lay = |t: &mut Title, folder: &str| {
        t.path = w.root.join(folder).to_string_lossy().to_string();
        std::fs::create_dir_all(&t.path).unwrap();
        w.app.db.save_title(t).unwrap();
    };
    let file = |t: &Title, name: &str, bytes: &[u8], of: Option<i64>| -> i64 {
        std::fs::write(Path::new(&t.path).join(name), bytes).unwrap();
        let data = json!({"title_id": t.id, "rel_path": name, "size": bytes.len(), "quality": QualityModel::new(Quality::Bluray1080p), "compact_of": of}).to_string();
        w.app.db.with(|c| c.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1, ?2, ?3, ?4)", rusqlite::params![t.id, name, bytes.len() as i64, data]).map(|_| c.last_insert_rowid())).unwrap()
    };
    let (mut both, mut whole) = (w.movie("Both Sizes", 2001, "tt0000001"), w.movie("Whole Only", 2002, "tt0000002"));
    lay(&mut both, "Movies/Both Sizes (2001)");
    lay(&mut whole, "Movies/Whole Only (2002)");
    let original = file(&both, "Both Sizes.mkv", b"the original, which is large", None);
    file(&both, "Both Sizes.compact.mkv", b"the compact one", Some(original));
    file(&whole, "Whole Only.mkv", b"only ever whole", None);
    let (mut show, eps) = w.series("Example World", 296762, &[(1, 1, "The Original"), (1, 2, "Chestnut")]);
    lay(&mut show, "TV Shows/Example World");
    let first = file(&show, "S01E01.mkv", b"episode one, large", None);
    file(&show, "S01E01.compact.mkv", b"episode one, small", Some(first));
    let second = file(&show, "S01E02.mkv", b"episode two, large", None);
    for (episode, of) in [(eps[0], first), (eps[1], second)] {
        w.app.db.with(|c| c.execute("UPDATE episodes SET file_id = ?2 WHERE id = ?1", [episode, of])).unwrap();
    }
    w.app.db.add_account("Ada", "").unwrap();

    let mut g = w.app.settings.general();
    g.players_enabled = true;
    w.app.settings.set_general(&g).unwrap();
    let door = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", door.local_addr().unwrap());
    let jf = spool::jellyfin::router(w.app.clone());
    tokio::spawn(async move { axum::serve(door, jf.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
    let http = reqwest::Client::new();
    const WHO: &str = r#"MediaBrowser Client="Infuse-Direct", Device="Phone", DeviceId="test-device", Version="8.5""#;
    let mut auths = vec![];
    for name in ["spool", "Ada"] {
        let signed = http.post(format!("{url}/Users/AuthenticateByName")).header("x-emby-authorization", WHO).json(&json!({"Username": name, "Pw": ""})).send().await.unwrap().json::<Value>().await.unwrap();
        auths.push(format!(r#"{WHO}, Token="{}""#, signed["AccessToken"].as_str().unwrap()));
    }
    let (me, ada) = (auths[0].clone(), auths[1].clone());
    let ask = |auth: String, path: String| {
        let (http, url) = (http.clone(), url.clone());
        async move {
            let r = http.get(format!("{url}{path}")).header("x-emby-authorization", auth).send().await.unwrap();
            (r.status().as_u16(), r.bytes().await.unwrap().to_vec())
        }
    };
    let names = |auth: &str, kind: &'static str| {
        let (ask, auth) = (ask.clone(), auth.to_string());
        async move {
            let listed: Value = serde_json::from_slice(&ask(auth, format!("/Items?Recursive=true&IncludeItemTypes={kind}&Fields=MediaSources")).await.1).unwrap();
            let mut items: Vec<(String, String, usize)> = listed["Items"].as_array().unwrap().iter().map(|i| (i["Name"].as_str().unwrap().to_string(), i["Id"].as_str().unwrap().to_string(), i["MediaSources"].as_array().map(Vec::len).unwrap_or(0))).collect();
            items.sort();
            items
        }
    };
    let played = |auth: &str, id: String| {
        let (ask, auth) = (ask.clone(), auth.to_string());
        async move { ask(auth, format!("/Videos/{id}/stream?static=true")).await }
    };

    // At home, and before the account is held to anything, it has everything.
    assert_eq!(call(&api, "PUT", "/api/accounts/1", json!({"compact_away": true})).await.0, 200);
    let films = names(&me, "Movie").await;
    assert_eq!(films.iter().map(|f| (f.0.as_str(), f.2)).collect::<Vec<_>>(), [("Both Sizes", 2), ("Whole Only", 1)], "at home both versions are offered");
    let (both_id, whole_id) = (films[0].1.clone(), films[1].1.clone());
    assert_eq!(played(&me, both_id.clone()).await.1, b"the original, which is large");

    // Away, it is shown only what has a compact version, and given that version: by the
    // film's name, and by the original's own.
    *w.app.home.lock() = Some(vec![]);
    let films = names(&me, "Movie").await;
    assert_eq!(films.iter().map(|f| (f.0.as_str(), f.2)).collect::<Vec<_>>(), [("Both Sizes", 1)]);
    assert_eq!(played(&me, both_id.clone()).await.1, b"the compact one");
    let by_source = ask(me.clone(), format!("/Videos/{both_id}/stream?static=true&MediaSourceId={}", spool::jellyfin::ids::Id::File(original))).await;
    assert_eq!(by_source.1, b"the compact one", "the original is not had by naming it");
    assert_eq!(played(&me, whole_id.clone()).await.0, 404, "nor is a film that has no compact version");
    assert_eq!(ask(me.clone(), format!("/Items/{whole_id}")).await.0, 404);
    let episodes = names(&me, "Episode").await;
    assert_eq!(episodes.iter().map(|e| (e.0.as_str(), e.2)).collect::<Vec<_>>(), [("The Original", 1)]);
    assert_eq!(played(&me, episodes[0].1.clone()).await.1, b"episode one, small");
    // Someone not held to them has everything, away as at home.
    assert_eq!(names(&ada, "Movie").await.len(), 2);
    assert_eq!(played(&ada, both_id.clone()).await.1, b"the original, which is large");
    assert_eq!(names(&ada, "Episode").await.len(), 2);
    // Home again, so has the first account.
    *w.app.home.lock() = None;
    assert_eq!(names(&me, "Movie").await.len(), 2);

    // The settings page is told how much has no compact version, which is what would be hidden.
    let (_, people) = call(&api, "GET", "/api/accounts", Value::Null).await;
    assert_eq!((people["accounts"][0]["compact_away"].as_bool(), people["accounts"][1]["compact_away"].as_bool(), people["without_compact"].to_string().as_str()), (Some(true), Some(false), r#"{"episodes":1,"films":1}"#), "{people}");
}

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
    let mut me = w.app.db.account(1).unwrap().unwrap();
    me.password = spool::settings::hash_password("pw").unwrap();
    w.app.db.save_account(&me).unwrap();
    let mut g = w.app.settings.general();
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

    // Subtitles can be read with either key; acting on many titles needs the full one, and
    // deleting through it needs confirmation like any other deletion.
    let (err, subs) = tool(&router, "read-key", "subtitles", serde_json::json!({"title_id": movie.id})).await;
    assert!(!err && subs["files"].as_array().unwrap().len() == 1 && subs["search_available"] == false, "{subs}");
    assert!(subs["files"][0]["file_id"].as_i64().is_some() && subs["files"][0]["beside_the_video"].as_array().unwrap().is_empty(), "{subs}");
    assert!(tool(&router, "read-key", "bulk_titles", serde_json::json!({"title_ids": [movie.id], "action": "unmonitor"})).await.0);
    let (err, done) = tool(&router, "full-key", "bulk_titles", serde_json::json!({"title_ids": [movie.id], "action": "unmonitor"})).await;
    assert!(!err && done["titles_changed"] == 1, "{done}");
    assert!(!w.app.db.title(movie.id).unwrap().unwrap().monitored);
    let (err, done) = tool(&router, "full-key", "bulk_titles", serde_json::json!({"title_ids": [movie.id], "action": "set_profile", "profile": "no such profile"})).await;
    assert!(err, "{done}");
    let (refused, msg) = tool(&router, "full-key", "bulk_titles", serde_json::json!({"title_ids": [movie.id], "action": "delete_files"})).await;
    assert!(refused && msg.as_str().unwrap().contains("confirm=true"), "{msg}");
    assert_eq!(w.app.db.files(movie.id).unwrap().len(), 1);

    // The person given a password above signs in to the web app with it.
    assert_eq!(call(&router, "POST", "/api/login", serde_json::json!({"name": "spool", "password": "wrong"})).await.0, 401);
    assert_eq!(call(&router, "POST", "/api/login", serde_json::json!({"name": "Spool", "password": "pw"})).await.0, 200);

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
async fn a_search_for_a_smaller_copy_replaces_an_oversized_file_with_one_that_fits() {
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

    // ...one for a smaller copy does, because the copy fits the target and is much smaller.
    let compact = w.app.search(movie.id, Scope::Smaller, true, true).await.unwrap();
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
    let again = w.app.search(movie.id, Scope::Smaller, true, true).await.unwrap();
    assert!(again.grabbed.is_empty() && again.message.starts_with("Nothing to make smaller"), "{}", again.message);
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

    // Fan releases often leave the year out. The full title is enough to know the film, and
    // the quality is still read from the name; a different film in the same series is not it.
    let wings = w.movie("Legend of the Example Heroes: Golden Wings", 1992, "tt0000077");
    for name in ["[Fan Group] Legend of the Example Heroes - Golden Wings [BD][720p]", "A-FanRips.Legend.of.the.Example.Heroes.-.Golden.Wings.BD.1080p.A233F476", "Legend of the Example Heroes - Overture to a New War [BD][720p]", "Legend.of.the.Example.Heroes.Golden.Wings.S01E02.1080p.WEB.H264-GRP"] {
        w.release(name, &[("w.mkv", &video)]);
        w.indexer.lock().untagged.insert(name.to_string());
    }
    let found = w.app.search(wings.id, Scope::Movie, true, false).await.unwrap();
    let verdict: HashMap<&str, (bool, String)> = found.decisions.iter().map(|d| (d.release.title.as_str(), (d.accepted, format!("{:?} {:?}", d.quality.quality, d.rejections.iter().map(|r| r.code.as_str()).collect::<Vec<_>>())))).collect();
    assert!(verdict["[Fan Group] Legend of the Example Heroes - Golden Wings [BD][720p]"].1.starts_with("Bluray720p"), "{verdict:?}");
    assert!(verdict["A-FanRips.Legend.of.the.Example.Heroes.-.Golden.Wings.BD.1080p.A233F476"].0, "{verdict:?}");
    assert!(!verdict["Legend of the Example Heroes - Overture to a New War [BD][720p]"].0, "{verdict:?}");
    assert!(!verdict["Legend.of.the.Example.Heroes.Golden.Wings.S01E02.1080p.WEB.H264-GRP"].0, "{verdict:?}");

    // A film found by its id needs no second question.
    let other = w.movie("Tagged Example", 2005, "tt0000042");
    w.release("Tagged.Example.2005.1080p.BluRay.x264-GRP", &[("t.mkv", &video)]);
    let before = w.indexer.lock().queries.len();
    w.app.search(other.id, Scope::Movie, false, false).await.unwrap();
    assert_eq!(w.indexer.lock().queries.len(), before + 1);
}

/// One call to the web API, as the app makes them.
#[tokio::test(flavor = "multi_thread")]
async fn a_film_posted_in_episodes_is_learned_from_the_first_and_the_rest_are_turned_down() {
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    // An OVA the metadata source lists as one three-hour film, posted as its episodes.
    let mut ova = w.movie("Pocket Robot War 0080: A Small Example", 1989, "tt0000080");
    ova.runtime = 180;
    w.app.db.save_title(&mut ova).unwrap();
    let episode = |n: u32| format!("[FanSub] Pocket Robot War - 0080 A Small Example - {n:02} - A Chapter (BD x264 1080p DTS)");
    let whole = "[FanSub] Pocket Robot War 0080 A Small Example (BD x264 1080p DTS)";
    w.release(&episode(5), &[("05.mkv", &video)]);
    w.indexer.lock().untagged.insert(episode(5));

    // Nothing in the name settles it, so the first one is downloaded, and is far too short.
    let outcome = w.app.search(ova.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "{}", outcome.message);
    let blocked = w.wait_state(ova.id, AcqState::ImportBlocked).await;
    let why = blocked.error.unwrap_or_default();
    assert!(why.contains("posted in episodes") && why.contains("add it as a series"), "{why}");
    assert!(w.app.db.title(ova.id).unwrap().unwrap().in_episodes);

    // The other episodes are now known for what they are by name; the whole film is still wanted.
    for name in [episode(6), whole.to_string()] {
        w.release(&name, &[("v.mkv", &video)]);
        w.indexer.lock().untagged.insert(name);
    }
    let found = w.app.search(ova.id, Scope::Movie, true, false).await.unwrap();
    let codes = |name: &str| found.decisions.iter().find(|d| d.release.title == name).map(|d| d.rejections.iter().map(|r| r.code.clone()).collect::<Vec<_>>()).unwrap_or_else(|| panic!("{name} was not judged"));
    assert_eq!(codes(&episode(6)), ["episode"]);
    assert!(!codes(whole).contains(&"episode".to_string()), "{:?}", codes(whole));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_film_listed_elsewhere_as_a_series_is_known_before_anything_is_downloaded() {
    let video = video_or_skip!();
    let source = details_source().await;
    let mut w = World::with_sources(Mode::Active, Some((&source, &source))).await;
    let episode = |n: u32| format!("[FanSub] Pocket Robot War - {n:02} - A Chapter (BD x264 1080p DTS)");

    let mut g = w.app.settings.general();
    g.tmdb_api_key = "key".into();
    w.app.settings.set_general(&g).unwrap();
    // Adding the film asks the series source about it, by the id the two share.
    let film = w.app.add_title(Kind::Movie, Some(80), None, w.profile("movie"), true, false).await.unwrap();
    let twin = film.as_series.clone().expect("the series listing");
    assert_eq!((twin.tvmaze_id, twin.episodes, twin.minutes, film.in_episodes), (88, 6, 180, true));
    for n in 1..=2 {
        w.release(&episode(n), &[("v.mkv", &video)]);
        w.indexer.lock().untagged.insert(episode(n));
    }
    let outcome = w.app.search(film.id, Scope::Movie, false, true).await.unwrap();
    assert!(outcome.grabbed.is_empty(), "{}", outcome.message);
    assert!(outcome.decisions.iter().filter(|d| d.release.title.starts_with("[FanSub]")).all(|d| d.rejections.iter().any(|r| r.code == "episode")), "{:?}", outcome.decisions.iter().map(|d| &d.rejections).collect::<Vec<_>>());

    // A film nobody lists as a series: a run of numbered releases in the results says it.
    let saga = w.movie("Another Robot Saga 0079", 1981, "tt0000079");
    for n in 1..=3 {
        let name = format!("[FanSub] Another Robot Saga 0079 - {n:02} (BD 1080p)");
        w.release(&name, &[("v.mkv", &video)]);
        w.indexer.lock().untagged.insert(name);
    }
    let outcome = w.app.search(saga.id, Scope::Movie, false, true).await.unwrap();
    assert!(outcome.grabbed.is_empty(), "{}", outcome.message);
    assert!(w.app.db.title(saga.id).unwrap().unwrap().in_episodes, "{:?}", outcome.decisions.iter().map(|d| (&d.release.title, d.accepted, &d.rejections)).collect::<Vec<_>>());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_film_becomes_the_series_it_is_listed_as_and_its_download_is_filed_as_an_episode() {
    let video = video_or_skip!();
    let source = details_source().await;
    let mut w = World::with_sources(Mode::Active, Some((&source, &source))).await;
    // In the library since before Spool asked the series source, so nothing is known yet.
    let mut film = w.movie("Pocket Robot War", 1989, "tt0000080");
    film.runtime = 180;
    w.app.db.save_title(&mut film).unwrap();
    let name = "[FanSub] Pocket Robot War - 05 - A Chapter (BD x264 1080p DTS)";
    w.release(name, &[("05.mkv", &video)]);
    w.indexer.lock().untagged.insert(name.to_string());
    w.app.search(film.id, Scope::Movie, false, true).await.unwrap();
    w.wait_state(film.id, AcqState::ImportBlocked).await;

    let series = w.app.film_to_series(film.id, None).await.unwrap();
    assert_eq!((series.kind, series.title.as_str(), series.monitored), (Kind::Series, "Pocket Robot War", true));
    assert!(w.app.db.title(film.id).unwrap().is_none(), "the film's entry goes");
    assert!(!w.root.join("Movies/Pocket Robot War (1989)").exists(), "and its empty folder");

    // The episode that was downloaded as the film is filed where it belongs.
    let mut filed = None;
    for _ in 0..100 {
        filed = w.app.db.episodes(series.id).unwrap().into_iter().find(|e| e.file_id.is_some());
        if filed.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let filed = filed.expect("an episode with a file");
    assert_eq!((filed.season, filed.episode), (1, 5));
    assert!(tree(&w.root.join("TV Shows")).iter().any(|f| f.contains("S01E05")), "{:?}", tree(&w.root.join("TV Shows")));
    assert!(w.app.db.attention(false).unwrap().is_empty(), "the notice is closed");

    // A film with a file is not turned into anything.
    let kept = w.movie("Kept Film", 2001, "tt0000081");
    w.release("Kept.Film.2001.1080p.BluRay.x264-GRP", &[("k.mkv", &video)]);
    w.app.search(kept.id, Scope::Movie, false, true).await.unwrap();
    w.wait_state(kept.id, AcqState::Imported).await;
    assert!(w.app.film_to_series(kept.id, Some(88)).await.is_err());
}

/// Wait until the title has this many files on record.
async fn wait_files(w: &World, title_id: i64, n: usize) -> Vec<MediaFile> {
    for _ in 0..150 {
        let files = w.app.db.files(title_id).unwrap();
        if files.len() >= n {
            return files;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    panic!("still {} files, wanted {n}", w.app.db.files(title_id).unwrap().len());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_film_in_parts_is_filed_whole_whether_the_parts_come_together_or_apart() {
    let (Some(_), Some(half)) = (sample_video(), video_of(28)) else {
        eprintln!("ffmpeg not available; skipping");
        return;
    };
    let mut w = World::new(Mode::Active).await;
    let film = |w: &World, title: &str, year: u32, imdb: &str| {
        let mut t = w.movie(title, year, imdb);
        t.runtime = 1;
        w.app.db.save_title(&mut t).unwrap();
        t
    };
    let parts = |files: &[MediaFile]| {
        let mut names: Vec<String> = files.iter().filter(|f| !f.extra).map(|f| f.rel_path.rsplit(" - ").next().unwrap_or_default().to_string()).collect();
        names.sort();
        names
    };

    // Both halves in one release: taking the larger alone would file half a film as the film.
    let together = film(&w, "Two Disc Example", 2001, "tt0002001");
    w.release("Two.Disc.Example.2001.1080p.BluRay.x264-GRP", &[("two.disc.example-cd1.mkv", &half), ("two.disc.example-cd2.mkv", &half)]);
    let outcome = w.app.search(together.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "{} {:?}", outcome.message, outcome.decisions.iter().map(|d| (&d.release.title, &d.rejections)).collect::<Vec<_>>());
    w.wait_state(together.id, AcqState::Imported).await;
    assert_eq!(parts(&w.app.db.files(together.id).unwrap()), ["part1.mkv", "part2.mkv"]);

    // Each half a release of its own: the one chosen brings the other with it.
    let apart = film(&w, "Apart Example", 2002, "tt0002002");
    for n in 1..=2 {
        w.release(&format!("Apart.Example.2002.1080p.BluRay.x264.CD{n}-GRP"), &[("ae.mkv", &half)]);
    }
    let outcome = w.app.search(apart.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 2, "{}", outcome.message);
    assert_eq!(parts(&wait_files(&w, apart.id, 2).await), ["part1.mkv", "part2.mkv"]);

    // One half and no other: a question, and after a yes it is kept as the part it is.
    let lonely = film(&w, "Lonely Example", 2003, "tt0002003");
    w.release("Lonely.Example.2003.1080p.BluRay.x264.CD1-GRP", &[("le.mkv", &half)]);
    w.app.search(lonely.id, Scope::Movie, false, true).await.unwrap();
    let stopped = w.wait_state(lonely.id, AcqState::ImportBlocked).await;
    assert!(stopped.error.as_deref().unwrap_or_default().contains("is part 1 of the film"), "{:?}", stopped.error);
    w.app.import(stopped.id, Some(vec![])).await.unwrap();
    assert_eq!(parts(&wait_files(&w, lonely.id, 1).await), ["part1.mkv"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn files_that_hold_more_or_less_than_their_name_says_are_noticed() {
    let (Some(short), Some(double), Some(season), Some(hours)) = (sample_video(), video_of(120), video_of(240), video_of(3700)) else {
        eprintln!("ffmpeg not available; skipping");
        return;
    };
    let mut w = World::new(Mode::Active).await;
    let minute_long = |w: &World, show: &Title| {
        for mut e in w.app.db.episodes(show.id).unwrap() {
            e.runtime = 1;
            w.app.db.upsert_episode(&e).unwrap();
        }
    };

    // A film that runs an hour longer than it should, and more than twice as long.
    let mut film = w.movie("Long Example", 2004, "tt0002004");
    film.runtime = 1;
    w.app.db.save_title(&mut film).unwrap();
    w.release("Long.Example.2004.1080p.BluRay.x264-GRP", &[("long.mkv", &hours)]);
    w.app.search(film.id, Scope::Movie, false, true).await.unwrap();
    let stopped = w.wait_state(film.id, AcqState::ImportBlocked).await;
    assert!(stopped.error.as_deref().unwrap_or_default().contains("a different film, or several films"), "{:?}", stopped.error);

    // One file named for the first episode that runs as long as the first two together.
    let (show, eps) = w.series("Double Show", 501, &[(1, 1, "One"), (1, 2, "Two"), (1, 3, "Three")]);
    minute_long(&w, &show);
    w.release("Double.Show.S01E01.1080p.WEB.H264-GRP", &[("d.mkv", &double)]);
    w.app.search(show.id, Scope::Episode(eps[0]), true, true).await.unwrap();
    w.wait_state(show.id, AcqState::Imported).await;
    let have: Vec<bool> = w.app.db.episodes(show.id).unwrap().iter().map(|e| e.file_id.is_some()).collect();
    assert_eq!(have, [true, true, false]);
    assert!(tree(&w.root.join("TV Shows/Double Show")).iter().any(|f| f.contains("S01E01-02")), "{:?}", tree(&w.root.join("TV Shows/Double Show")));

    // A whole season run together into one file: all its episodes are not counted without a yes.
    let four = [(1, 1, "One"), (1, 2, "Two"), (1, 3, "Three"), (1, 4, "Four")];
    let (stitched, _) = w.series("Stitched Show", 502, &four);
    minute_long(&w, &stitched);
    w.release("Stitched.Show.S01.1080p.WEB.H264-GRP", &[("all.of.it.mkv", &season)]);
    w.app.search(stitched.id, Scope::Missing, false, true).await.unwrap();
    let stopped = w.wait_state(stitched.id, AcqState::ImportBlocked).await;
    assert!(stopped.error.as_deref().unwrap_or_default().contains("the whole of season 1 in one file"), "{:?}", stopped.error);
    assert!(w.app.db.episodes(stitched.id).unwrap().iter().all(|e| e.file_id.is_none()));
    w.app.import(stopped.id, Some(stopped.episode_ids.clone())).await.unwrap();
    assert!(w.app.db.episodes(stitched.id).unwrap().iter().all(|e| e.file_id.is_some()));

    // The same name on a file far too short to be the season.
    let (thin, _) = w.series("Thin Show", 503, &four);
    minute_long(&w, &thin);
    w.release("Thin.Show.S01.1080p.WEB.H264-GRP", &[("thin.mkv", &short)]);
    w.app.search(thin.id, Scope::Missing, false, true).await.unwrap();
    let stopped = w.wait_state(thin.id, AcqState::ImportBlocked).await;
    assert!(stopped.error.as_deref().unwrap_or_default().contains("probably not all of them"), "{:?}", stopped.error);
}

#[tokio::test(flavor = "multi_thread")]
async fn anime_is_known_by_its_own_source_and_its_series_and_films_make_a_collection() {
    let source = details_source().await;
    let w = World::with_sources(Mode::Shadow, Some((&source, &source))).await;
    let mut g = w.app.settings.general();
    g.tmdb_api_key = "key".into();
    w.app.settings.set_general(&g).unwrap();
    // The series the OVA is a side story of, in the library since before Spool asked.
    let (show, _) = w.series("Example World", 296762, &[(1, 1, "The Original")]);

    // A "film" that AniList knows as a six-episode OVA: said outright, before any search.
    let film = w.app.add_title(Kind::Movie, Some(80), None, w.profile("movie"), true, false).await.unwrap();
    assert_eq!((film.anilist.as_slice(), film.in_episodes), (&[82u32][..], true));
    assert!(film.alt_titles.iter().any(|a| a == "Pocket Robot War (romaji)"), "the name releases carry: {:?}", film.alt_titles);
    let ova = w.app.db.anime(82).unwrap().unwrap();
    assert_eq!((ova.format.as_str(), ova.episodes, ova.minutes), ("OVA", 6, 30));
    assert!(w.app.collections().unwrap().is_empty(), "one title is not a collection");

    // The sweep reads the rest of the library, and the two are found to belong together.
    let said = w.app.anime_sweep().await.unwrap();
    // Its first run reads every title, the film included.
    assert!(said.starts_with("2 anime titles"), "{said}");
    assert_eq!(w.app.db.title(show.id).unwrap().unwrap().anilist, [80, 81], "a season each");
    let found = w.app.collections().unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    let c = &found[0];
    assert_eq!((c.name.as_str(), c.kind.as_str()), ("Robot War", "anime"));
    let roles: HashMap<i64, &str> = c.members.iter().map(|m| (m.title_id, m.role.as_str())).collect();
    assert_eq!((roles[&show.id], roles[&film.id]), ("", "Side story"));
    // The sequel series is part of the story, is not here, and can be added by its ids.
    assert_eq!(c.missing.iter().map(|m| (m.anilist_id, m.title.as_str(), m.can_add)).collect::<Vec<_>>(), [(85, "Robot War Z", true)]);

    // The id list ties this film's id to a set of specials. The work beside that set which
    // carries the film's name is the film, and a film is not posted in episodes.
    let border = w.app.add_title(Kind::Movie, Some(91), None, w.profile("movie"), true, false).await.unwrap();
    assert_eq!((border.anilist.as_slice(), border.in_episodes), (&[91u32][..], false));

    // A series of films is read whole, so the film between the two that are here shows as missing.
    for tmdb in [301, 303] {
        w.app.add_title(Kind::Movie, Some(tmdb), None, w.profile("movie"), true, false).await.unwrap();
    }
    w.app.anime_sweep().await.unwrap();
    let found = w.app.collections().unwrap();
    let saga = found.iter().find(|c| c.name == "Saga Collection").expect("the film collection");
    assert_eq!((saga.kind.as_str(), saga.members.len()), ("films", 2));
    assert_eq!(saga.missing.iter().map(|m| (m.tmdb_id, m.title.as_str())).collect::<Vec<_>>(), [(Some(302), "Saga Two")]);

    // Searching for something to add asks all three sources. The OVA comes back as the film
    // TMDB has, marked as posted in episodes, and as the series to add in its place.
    let both = w.app.discover("Pocket Robot War").await.unwrap();
    assert_eq!((both["movies"][0]["title"].as_str(), both["movies"][0]["anime"]["as_series"].as_bool(), both["movies"][0]["library_id"].as_i64()), (Some("Pocket Robot War"), Some(true), Some(film.id)));
    assert_eq!((both["series"][0]["title"].as_str(), both["series"][0]["anime"]["format"].as_str(), both["series"][0]["anime"]["episodes"].as_u64()), (Some("Pocket Robot War"), Some("OVA"), Some(6)), "{both}");

    // The web app and players are given the same collection.
    let router = spool::api::router(w.app.clone());
    let (status, listed) = call(&router, "GET", "/api/collections", serde_json::Value::Null).await;
    assert_eq!((status, listed[0]["name"].as_str(), listed[0]["members"].as_array().map(|m| m.len())), (200, Some("Robot War"), Some(2)));

    // The user's word stands over the sources: a new name, a role, and a title taken out.
    let robot = listed[0]["id"].as_u64().unwrap();
    assert_eq!(call(&router, "PUT", &format!("/api/collections/{robot}"), serde_json::json!({"name": "Robots"})).await.0, 200);
    assert_eq!(call(&router, "PUT", &format!("/api/titles/{}/collection", film.id), serde_json::json!({"role": "Spin-off"})).await.0, 200);
    let (_, listed) = call(&router, "GET", "/api/collections", serde_json::Value::Null).await;
    let robots = listed.as_array().unwrap().iter().find(|c| c["id"].as_u64() == Some(robot)).unwrap();
    let role = robots["members"].as_array().unwrap().iter().find(|m| m["title_id"].as_i64() == Some(film.id)).and_then(|m| m["role"].as_str());
    assert_eq!((robots["name"].as_str(), role), (Some("Robots"), Some("Spin-off")));
    let (_, made) = call(&router, "POST", "/api/collections", serde_json::json!({"name": "Rainy days"})).await;
    assert_eq!(call(&router, "PUT", &format!("/api/titles/{}/collection", film.id), serde_json::json!({"collection": made["id"]})).await.0, 200);
    let (_, listed) = call(&router, "GET", "/api/collections", serde_json::Value::Null).await;
    let names: Vec<&str> = listed.as_array().unwrap().iter().filter_map(|c| c["name"].as_str()).collect();
    assert_eq!(names, ["Rainy days", "Saga Collection"], "the anime collection is down to one title, and so is gone");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_later_season_posted_under_its_own_name_and_numbered_from_one_lands_in_that_season() {
    let video = video_or_skip!();
    let source = details_source().await;
    let mut w = World::with_sources(Mode::Active, Some((&source, &source))).await;
    let (mut show, eps) = w.series("Example World", 296762, &[(1, 1, "The Original"), (2, 1, "Journey"), (2, 2, "Reunion")]);
    show.seasons.push(SeasonInfo { number: 2, monitored: true });
    w.app.db.save_title(&mut show).unwrap();
    w.app.anime_sweep().await.unwrap();

    // The second season is "Robot War II" to the people who post it, and starts again at 01.
    for n in 1..=2 {
        w.release(&format!("[FanSub] Robot War II - {n:02} (BD 1080p)"), &[(&format!("[FanSub] Robot War II - {n:02}.mkv"), &video)]);
        w.indexer.lock().untagged.insert(format!("[FanSub] Robot War II - {n:02} (BD 1080p)"));
    }
    let outcome = w.app.search(show.id, Scope::Season(2), true, true).await.unwrap();
    let covers: HashMap<&str, &str> = outcome.decisions.iter().map(|d| (d.release.title.as_str(), d.covers.as_str())).collect();
    assert_eq!((covers["[FanSub] Robot War II - 01 (BD 1080p)"], covers["[FanSub] Robot War II - 02 (BD 1080p)"]), ("S02E01", "S02E02"), "{:?}", outcome.decisions.iter().map(|d| (&d.release.title, &d.rejections)).collect::<Vec<_>>());
    assert_eq!(outcome.grabbed.len(), 2, "{}", outcome.message);
    for _ in 0..150 {
        if w.app.db.episodes(show.id).unwrap().iter().filter(|e| e.file_id.is_some()).count() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let have: Vec<(u32, u32, bool)> = w.app.db.episodes(show.id).unwrap().iter().map(|e| (e.season, e.episode, e.file_id.is_some())).collect();
    assert_eq!(have, [(1, 1, false), (2, 1, true), (2, 2, true)], "the files' own names say episodes 1 and 2 of the whole run; the release is believed");
    assert_eq!(eps.len(), 3);
}

async fn call(router: &axum::Router, method: &str, uri: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
    call_as(router, None, method, uri, body).await
}

/// A call with a header that says who is asking: `("x-api-key", key)` or `("cookie", ...)`.
async fn call_as(router: &axum::Router, by: Option<(&str, &str)>, method: &str, uri: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder().method(method).uri(uri).header("content-type", "application/json");
    if let Some((name, value)) = by {
        req = req.header(name, value);
    }
    let resp = router.clone().oneshot(req.body(axum::body::Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 8 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_app_answers_only_its_own_pages_and_reads_only_its_own_subtitles() {
    use serde_json::json;
    use tower::ServiceExt;
    let w = World::new(Mode::Active).await;
    let mut movie = w.movie("Sidecar", 2011, "tt3333333");
    movie.path = w.root.join("Movies/Sidecar (2011)").to_string_lossy().to_string();
    w.app.db.save_title(&mut movie).unwrap();
    std::fs::create_dir_all(&movie.path).unwrap();
    std::fs::write(Path::new(&movie.path).join("film.mkv"), b"film").unwrap();
    let file: MediaFile = serde_json::from_value(json!({"title_id": movie.id, "rel_path": "film.mkv", "size": 4, "quality": {"quality": "bluray-1080p"}})).unwrap();
    w.app.db.with(|c| c.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1, 'film.mkv', 4, ?2)", rusqlite::params![movie.id, serde_json::to_string(&file).unwrap()])).unwrap();
    let file_id = w.app.db.files(movie.id).unwrap()[0].id;
    let router = spool::api::router(w.app.clone());

    // A file that is not one of the film's subtitles is not opened, however it is named.
    let elsewhere = w.root.join("not-a-subtitle.srt");
    std::fs::write(&elsewhere, b"1\n00:00:01,000 --> 00:00:02,000\nkept as it is\n").unwrap();
    for rel in [elsewhere.to_string_lossy().to_string(), "../../not-a-subtitle.srt".into()] {
        let (status, said) = call(&router, "POST", &format!("/api/files/{file_id}/subtitles/check"), json!({"rel_path": rel, "correct": true})).await;
        assert!(status >= 400 && said.to_string().contains("no such subtitle"), "{rel}: {status} {said}");
    }
    assert_eq!(std::fs::read(&elsewhere).unwrap(), b"1\n00:00:01,000 --> 00:00:02,000\nkept as it is\n");

    let send = |method: &str, host: &str, origin: Option<&str>| {
        let mut req = axum::http::Request::builder().method(method).uri("/api/queue/pause").header("host", host).header("content-type", "application/json");
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        let router = router.clone();
        let req = req.body(axum::body::Body::from("{}")).unwrap();
        async move { router.oneshot(req).await.unwrap().status().as_u16() }
    };
    // A name a web page has pointed at this machine is not answered, even to read.
    assert_eq!(send("GET", "evil.example.com:7979", None).await, 421);
    // A change from another page, even on the same machine, is refused; from Spool's own, made.
    assert_eq!(send("POST", "casper:7979", Some("https://evil.example.com")).await, 403);
    assert_eq!(send("POST", "casper:7979", Some("http://casper:8088")).await, 403);
    assert!(!w.app.engine.is_paused(), "and neither paused anything");
    assert_eq!(send("POST", "casper:7979", Some("http://casper:7979")).await, 200);
    assert!(w.app.engine.is_paused());
}

#[tokio::test(flavor = "multi_thread")]
async fn bulk_actions_cover_many_titles_and_respect_kind() {
    use serde_json::json;
    let w = World::new(Mode::Active).await;
    let a = w.movie("Alpha", 2001, "tt0000001");
    let b = w.movie("Beta", 2002, "tt0000002");
    let (s, _) = w.series("Gamma", 1001, &[(1, 1, "2016-01-01")]);
    let router = spool::api::router(w.app.clone());
    let ids = json!([a.id, b.id, s.id]);
    let monitored = |id: i64| w.app.db.title(id).unwrap().unwrap().monitored;

    let (status, r) = call(&router, "POST", "/api/titles/bulk", json!({"ids": ids, "action": "unmonitor"})).await;
    assert_eq!((status, r["done"].as_i64()), (200, Some(3)));
    assert!(!monitored(a.id) && !monitored(b.id) && !monitored(s.id));
    call(&router, "POST", "/api/titles/bulk", json!({"ids": [a.id], "action": "monitor"})).await;
    assert!(monitored(a.id) && !monitored(b.id));

    // A film profile is applied to the films and passed over for the series.
    let mut other = QualityProfile::default_hd("movie");
    other.name = "Other".into();
    w.app.db.save_profile(&mut other).unwrap();
    let (_, r) = call(&router, "POST", "/api/titles/bulk", json!({"ids": ids, "action": "profile", "profile_id": other.id})).await;
    assert_eq!(r["done"].as_i64(), Some(2));
    assert_eq!(w.app.db.title(b.id).unwrap().unwrap().profile_id, other.id);
    assert_eq!(w.app.db.title(s.id).unwrap().unwrap().profile_id, s.profile_id);
    assert_eq!(call(&router, "POST", "/api/titles/bulk", json!({"ids": ids, "action": "profile", "profile_id": 99999})).await.0, 400);

    assert_eq!(call(&router, "POST", "/api/titles/bulk", json!({"ids": [], "action": "monitor"})).await.0, 400);
    assert_eq!(call(&router, "POST", "/api/titles/bulk", json!({"ids": ids, "action": "nonsense"})).await.0, 400);

    // Removing takes the titles out and leaves the rest alone.
    let (_, r) = call(&router, "POST", "/api/titles/bulk", json!({"ids": [b.id, s.id], "action": "remove"})).await;
    assert_eq!(r["done"].as_i64(), Some(2));
    assert!(w.app.db.title(b.id).unwrap().is_none() && w.app.db.title(s.id).unwrap().is_none());
    assert!(w.app.db.title(a.id).unwrap().is_some());

    // A search runs in the background and reports through the task.
    let (status, r) = call(&router, "POST", "/api/titles/bulk", json!({"ids": [a.id], "action": "search"})).await;
    assert_eq!((status, r["started"].as_i64()), (200, Some(1)));
    for _ in 0..100 {
        if w.app.tasks.lock().get("bulk").is_some_and(|t| !t.running) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let task = w.app.tasks.lock().get("bulk").cloned().unwrap();
    assert!(!task.running && task.last_message.starts_with("searched 1 of 1"), "{}", task.last_message);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_film_can_keep_another_version_that_upgrades_leave_alone() {
    use serde_json::json;
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let mut movie = w.movie("Keep Both", 2018, "tt2222222");
    movie.tmdb_id = Some(2222);
    movie.path = w.root.join("Movies/Keep Both (2018)").to_string_lossy().to_string();
    w.app.db.save_title(&mut movie).unwrap();
    std::fs::create_dir_all(&movie.path).unwrap();
    std::fs::write(Path::new(&movie.path).join("old.mkv"), b"old file").unwrap();
    let old: MediaFile = serde_json::from_value(json!({"title_id": movie.id, "rel_path": "old.mkv", "size": 8, "quality": {"quality": "hdtv-1080p"}})).unwrap();
    w.app.db.with(|c| c.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1, 'old.mkv', 8, ?2)", rusqlite::params![movie.id, serde_json::to_string(&old).unwrap()])).unwrap();
    let router = spool::api::router(w.app.clone());

    // A lesser copy, wanted all the same: taken as another version, it replaces nothing.
    w.release("Keep.Both.2018.720p.BluRay.x264-SMALL", &[("small.mkv", &video)]);
    let outcome = w.app.search(movie.id, Scope::Movie, true, false).await.unwrap();
    let d = outcome.decisions.iter().find(|d| d.release.title.contains("SMALL")).expect("the release was seen");
    let (status, _) = call(&router, "POST", &format!("/api/decisions/{}/grab", d.id), json!({"another_version": true})).await;
    assert_eq!(status, 200);
    w.wait_state(movie.id, AcqState::Imported).await;
    let files = w.app.db.files(movie.id).unwrap();
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(files.iter().any(|f| f.rel_path == "old.mkv" && !f.extra));
    let extra = files.iter().find(|f| f.extra).expect("the new file is an extra version").clone();
    assert_eq!(extra.quality, QualityModel::new(Quality::Bluray720p));
    assert!(Path::new(&movie.path).join("old.mkv").exists() && Path::new(&movie.path).join(&extra.rel_path).exists());

    // A saved release can be kept beside them the same way.
    let (_, detail) = call(&router, "GET", &format!("/api/titles/{}", movie.id), serde_json::Value::Null).await;
    let saved = detail["saved"].as_array().and_then(|s| s.iter().find(|n| n["release"]["title"].as_str().is_some_and(|t| t.contains("SMALL")))).unwrap_or_else(|| panic!("the download was saved: {}", detail["saved"]))["id"].as_i64().unwrap();
    let (status, said) = call(&router, "POST", &format!("/api/archive/{saved}/grab"), json!({"another_version": true})).await;
    assert_eq!(status, 200, "{said}");
    for _ in 0..400 {
        if w.app.db.files(movie.id).unwrap().len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let files = w.app.db.files(movie.id).unwrap();
    assert_eq!((files.len(), files.iter().filter(|f| f.extra).count()), (3, 2), "{files:?}");
    let second = files.iter().find(|f| f.extra && f.id != extra.id).unwrap().id;
    w.app.delete_file(second).unwrap();

    // An upgrade replaces the main file and leaves the extra version where it is.
    w.release("Keep.Both.2018.1080p.BluRay.x264-NEW", &[("new.mkv", &video)]);
    let outcome = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "{}", outcome.message);
    for _ in 0..400 {
        if w.app.db.files(movie.id).unwrap().iter().any(|f| f.quality == QualityModel::new(Quality::Bluray1080p)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let files = w.app.db.files(movie.id).unwrap();
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(files.iter().any(|f| f.quality == QualityModel::new(Quality::Bluray1080p) && !f.extra));
    assert!(files.iter().any(|f| f.id == extra.id && f.extra));
    assert!(!Path::new(&movie.path).join("old.mkv").exists() && Path::new(&movie.path).join(&extra.rel_path).exists());

    // Marking the extra as main again puts it back in line for replacement.
    let (status, _) = call(&router, "PATCH", &format!("/api/files/{}", extra.id), json!({"extra": false})).await;
    assert_eq!(status, 200);
    assert!(w.app.db.files(movie.id).unwrap().iter().all(|f| !f.extra));
}

#[tokio::test(flavor = "multi_thread")]
async fn subtitles_come_with_the_release_and_can_be_found_and_checked() {
    use axum::routing::post;
    use serde_json::json;
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let movie = w.movie("Subbed", 2019, "tt3333333");
    let srt = "1\n00:00:00,500 --> 00:00:01,500\nHello\n\n2\n00:00:02,000 --> 00:00:03,000\nGoodbye\n";

    // A stand-in for OpenSubtitles: one result timed for this exact file, one that is not.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let (h1, h2) = (hits.clone(), hits.clone());
    let router = axum::Router::new()
        .route(
            "/subtitles",
            get(move |headers: axum::http::HeaderMap, axum::extract::RawQuery(q): axum::extract::RawQuery| async move {
                h1.lock().push(format!("search {} key={}", q.unwrap_or_default(), headers.get("api-key").and_then(|v| v.to_str().ok()).unwrap_or("")));
                axum::Json(json!({"data": [
                    {"attributes": {"language": "en", "release": "Subbed.2019.DVDRip-OTHER", "download_count": 5000, "files": [{"file_id": 11}]}},
                    {"attributes": {"language": "en", "release": "Subbed.2019.1080p.BluRay.x264-GRP", "download_count": 12, "moviehash_match": true, "files": [{"file_id": 22}]}},
                ]}))
            }),
        )
        .route(
            "/download",
            post(move |axum::Json(b): axum::Json<serde_json::Value>| async move {
                h2.lock().push(format!("download {}", b["file_id"]));
                axum::Json(json!({"link": format!("http://{addr}/file.srt")}))
            }),
        )
        .route("/file.srt", get(move || async move { srt }));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut g = w.app.settings.general();
    g.opensubtitles_api_key = "os-key".into();
    g.opensubtitles_url = format!("http://{addr}");
    g.subtitle_languages = "en, es".into();
    w.app.settings.set_general(&g).unwrap();

    // The release brings Spanish subtitles of its own; they land beside the film.
    w.release("Subbed.2019.1080p.BluRay.x264-GRP", &[("subbed.mkv", &video), ("subbed.spa.srt", srt.as_bytes())]);
    let outcome = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "{}", outcome.message);
    w.wait_state(movie.id, AcqState::Imported).await;
    let file = w.app.db.files(movie.id).unwrap().remove(0);
    assert_eq!(file.subtitles.len(), 1, "{:?}", file.subtitles);
    assert_eq!((file.subtitles[0].language.as_str(), file.subtitles[0].source.as_str()), ("es", "download"));
    let folder = Path::new(&w.app.db.title(movie.id).unwrap().unwrap().path).to_path_buf();
    assert!(folder.join(&file.subtitles[0].rel_path).exists());
    assert!(file.subtitles[0].rel_path.ends_with(".es.srt"));

    // English is still wanted. The result timed for this exact file is put first, whatever
    // its popularity.
    assert_eq!(w.app.missing_subtitles().unwrap(), vec![(file.id, vec!["en".to_string()])]);
    let found = w.app.search_subtitles(file.id, "en").await.unwrap();
    assert_eq!(found.iter().map(|c| c.file_id).collect::<Vec<_>>(), vec![22, 11]);
    let asked = hits.lock()[0].clone();
    // (The sample clip is too small to have a checksum; a real film sends one as well.)
    assert!(asked.contains("languages=en") && asked.contains("imdb_id=3333333") && asked.ends_with("key=os-key"), "{asked}");

    // The clip is too short to judge timing by, so only the exact match is trusted.
    let kept = w.app.auto_subtitle(file.id, "en", 0).await.unwrap().expect("the exact match is kept");
    assert_eq!((kept.language.as_str(), kept.source.as_str()), ("en", "opensubtitles"));
    assert_eq!(kept.sync.as_ref().unwrap().verdict, spool::subs::Verdict::Unsure);
    assert_eq!(hits.lock().iter().filter(|h| h.starts_with("download")).cloned().collect::<Vec<_>>(), vec!["download 22"]);
    let file = w.app.db.files(movie.id).unwrap().remove(0);
    assert_eq!(file.subtitles.len(), 2);
    assert!(w.app.missing_subtitles().unwrap().is_empty());
    let english = file.subtitles.iter().find(|s| s.language == "en").unwrap();
    assert_eq!(spool::subs::parse_srt(&std::fs::read_to_string(folder.join(&english.rel_path)).unwrap()).len(), 2);

    // Deleting the film takes its subtitle files with it.
    let paths: Vec<_> = file.subtitles.iter().map(|s| folder.join(&s.rel_path)).collect();
    w.app.delete_file(file.id).unwrap();
    assert!(paths.iter().all(|p| !p.exists()));
}

/// One request as a player makes them, with the header a player signs its requests with.
async fn player(router: &axum::Router, method: &str, uri: &str, token: Option<&str>, body: serde_json::Value) -> (u16, axum::http::HeaderMap, Vec<u8>) {
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder().method(method).uri(uri).header("content-type", "application/json").header("user-agent", "Infuse-Direct/8.5");
    let mut auth = r#"MediaBrowser Client="Infuse-Direct", Device="Apple TV", DeviceId="test-device", Version="8.5""#.to_string();
    if let Some(t) = token {
        auth.push_str(&format!(r#", Token="{t}""#));
    }
    req = req.header("x-emby-authorization", auth);
    let body = if body.is_null() { axum::body::Body::empty() } else { axum::body::Body::from(body.to_string()) };
    let resp = router.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let (status, headers) = (resp.status().as_u16(), resp.headers().clone());
    (status, headers, axum::body::to_bytes(resp.into_body(), 64 << 20).await.unwrap().to_vec())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_player_can_sign_in_browse_play_and_report_what_it_watched() {
    use serde_json::{json, Value};
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let srt = "1\n00:00:00,500 --> 00:00:01,500\nHello\n";

    // A film with a subtitle file beside it and the details a player shows, and a short series.
    let mut movie = w.movie("The Signal", 2019, "tt4444444");
    movie.rating = Some(7.4);
    movie.certification = Some("PG-13".into());
    movie.overview = "A signal arrives.".into();
    movie.genres = vec!["Science Fiction".into()];
    movie.people = vec![Person { id: 31, name: "Ada Vale".into(), role: "The Listener".into(), kind: "actor".into(), photo: None }];
    w.app.db.save_title(&mut movie).unwrap();
    w.release("The.Signal.2019.1080p.BluRay.x264-GRP", &[("signal.mkv", &video), ("signal.eng.srt", srt.as_bytes())]);
    assert_eq!(w.app.search(movie.id, Scope::Movie, false, true).await.unwrap().grabbed.len(), 1);
    w.wait_state(movie.id, AcqState::Imported).await;
    let (show, _) = w.series("Example World", 296762, &[(1, 1, "The Original"), (1, 2, "Chestnut")]);
    w.release("Example.World.S01.1080p.BluRay.x264-PACK", &[("Example.World.S01E01.1080p.BluRay.x264-PACK.mkv", &video), ("Example.World.S01E02.1080p.BluRay.x264-PACK.mkv", &video)]);
    assert_eq!(w.app.search(show.id, Scope::Missing, false, true).await.unwrap().grabbed.len(), 1);
    w.wait_state(show.id, AcqState::Imported).await;
    // A title with nothing to play is not shown to players.
    w.movie("Not Here Yet", 2030, "tt5555555");

    let mut g = w.app.settings.general();
    g.players_enabled = true;
    w.app.settings.set_general(&g).unwrap();
    let mut me = w.app.db.account(1).unwrap().unwrap();
    me.password = spool::settings::hash_password("tv-pass").unwrap();
    w.app.db.save_account(&me).unwrap();
    let jf = spool::jellyfin::router(w.app.clone());
    let read = |bytes: &[u8]| serde_json::from_slice::<Value>(bytes).unwrap_or(Value::Null);

    // Before signing in a player can ask who is here, and nothing more.
    let (status, _, body) = player(&jf, "GET", "/System/Info/Public", None, Value::Null).await;
    let info = read(&body);
    assert_eq!((status, info["ServerName"].as_str(), info["Id"].as_str().map(str::len)), (200, Some("Spool"), Some(32)));
    assert_eq!(player(&jf, "GET", "/Users/Me", None, Value::Null).await.0, 401);
    assert_eq!(player(&jf, "GET", "/Items", Some("made-up"), Value::Null).await.0, 401);
    assert_eq!(player(&jf, "POST", "/Users/AuthenticateByName", None, json!({"Username": "spool", "Pw": "wrong"})).await.0, 401);
    let (status, _, body) = player(&jf, "POST", "/Users/AuthenticateByName", None, json!({"Username": "Spool", "Pw": "tv-pass"})).await;
    let signed = read(&body);
    assert_eq!(status, 200, "{signed}");
    let token = signed["AccessToken"].as_str().unwrap().to_string();
    let user = signed["User"]["Id"].as_str().unwrap().to_string();
    assert_eq!(signed["SessionInfo"]["Client"], "Infuse-Direct");
    let t = Some(token.as_str());
    // A player's token opens the library and nothing of Spool's own.
    let app_api = spool::api::router(w.app.clone());
    assert_eq!(player(&app_api, "GET", &format!("/api/settings/general?api_key={token}"), t, Value::Null).await.0, 401);

    // The two libraries, then the films in one: names and paths in whatever capitals.
    let (_, _, body) = player(&jf, "GET", &format!("/Users/{user}/Views"), t, Value::Null).await;
    let views = read(&body);
    assert_eq!(views["Items"].as_array().unwrap().iter().map(|v| v["CollectionType"].as_str().unwrap()).collect::<Vec<_>>(), ["movies", "tvshows"]);
    let movies_view = views["Items"][0]["Id"].as_str().unwrap().to_string();
    let (status, _, body) = player(&jf, "GET", &format!("/emby/users/{user}/items?parentId={movies_view}&includeItemTypes=Movie&fields=MediaSources,Overview,People,Genres&sortBy=SortName"), t, Value::Null).await;
    let list = read(&body);
    assert_eq!((status, list["TotalRecordCount"].as_u64()), (200, Some(1)), "{list}");
    let film = &list["Items"][0];
    let film_id = film["Id"].as_str().unwrap().to_string();
    assert_eq!((film["Name"].as_str(), film["Type"].as_str(), film["ProductionYear"].as_u64()), (Some("The Signal"), Some("Movie"), Some(2019)));
    assert_eq!((film["CommunityRating"].as_f64().map(|r| (r * 10.0).round()), film["OfficialRating"].as_str(), film["Overview"].as_str()), (Some(74.0), Some("PG-13"), Some("A signal arrives.")));
    assert_eq!((film["People"][0]["Name"].as_str(), film["People"][0]["Role"].as_str(), film["Genres"][0].as_str()), (Some("Ada Vale"), Some("The Listener"), Some("Science Fiction")));
    let source = &film["MediaSources"][0];
    assert_eq!((source["Id"].as_str(), source["Container"].as_str(), source["SupportsDirectPlay"].as_bool()), (Some(film_id.as_str()), Some("mkv"), Some(true)));
    let streams = source["MediaStreams"].as_array().unwrap();
    assert_eq!(streams.iter().map(|s| (s["Type"].as_str().unwrap(), s["Codec"].as_str().unwrap())).collect::<Vec<_>>(), [("Video", "h264"), ("Audio", "aac"), ("Subtitle", "subrip")], "{streams:?}");
    assert_eq!(streams.iter().enumerate().all(|(i, s)| s["Index"] == i), true);
    // A list without the costly parts asked for does not carry them.
    let (_, _, body) = player(&jf, "GET", &format!("/Items?ParentId={movies_view}"), t, Value::Null).await;
    assert!(read(&body)["Items"][0].get("MediaSources").is_none());
    // One item is always given whole.
    let (_, _, body) = player(&jf, "GET", &format!("/Items/{film_id}"), t, Value::Null).await;
    assert!(read(&body)["MediaSources"][0]["RunTimeTicks"].as_i64().unwrap() > 10_000_000);
    let (_, _, body) = player(&jf, "POST", &format!("/Items/{film_id}/PlaybackInfo?userId={user}"), t, json!({"DeviceProfile": {}})).await;
    assert_eq!(read(&body)["MediaSources"][0]["Id"], film_id.as_str());

    // The film itself: whole, in part, and never squeezed on the way.
    let on_disk = std::fs::read(Path::new(&w.app.db.title(movie.id).unwrap().unwrap().path).join(&w.app.db.files(movie.id).unwrap()[0].rel_path)).unwrap();
    let (status, headers, body) = player(&jf, "GET", &format!("/Videos/{film_id}/stream.mkv?static=true&MediaSourceId={film_id}"), t, Value::Null).await;
    assert_eq!((status, body.len(), headers.get("accept-ranges").unwrap().to_str().unwrap()), (200, on_disk.len(), "bytes"));
    assert!(headers.get("content-encoding").is_none() && body == on_disk);
    use tower::ServiceExt;
    let part = axum::http::Request::builder().uri(format!("/Videos/{film_id}/stream?api_key={token}")).header("range", "bytes=100-299").header("accept-encoding", "gzip").body(axum::body::Body::empty()).unwrap();
    let resp = jf.clone().oneshot(part).await.unwrap();
    assert_eq!((resp.status().as_u16(), resp.headers().get("content-range").unwrap().to_str().unwrap().to_string()), (206, format!("bytes 100-299/{}", on_disk.len())));
    assert!(resp.headers().get("content-encoding").is_none());
    assert_eq!(axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap().to_vec(), on_disk[100..300]);
    assert_eq!(player(&jf, "GET", &format!("/Videos/{film_id}/stream"), None, Value::Null).await.0, 401);
    let past = axum::http::Request::builder().uri(format!("/Videos/{film_id}/stream?api_key={token}")).header("range", format!("bytes={}-", on_disk.len())).body(axum::body::Body::empty()).unwrap();
    assert_eq!(jf.clone().oneshot(past).await.unwrap().status().as_u16(), 416);
    // The subtitle file beside it, at the address the item gave.
    let (status, _, body) = player(&jf, "GET", streams[2]["DeliveryUrl"].as_str().unwrap(), t, Value::Null).await;
    assert_eq!((status, String::from_utf8_lossy(&body).contains("Hello")), (200, true));
    let (_, _, body) = player(&jf, "GET", &streams[2]["DeliveryUrl"].as_str().unwrap().replace(".srt", ".vtt"), t, Value::Null).await;
    assert!(String::from_utf8_lossy(&body).starts_with("WEBVTT"));

    // The series: its seasons, its episodes in order.
    let shows_view = views["Items"][1]["Id"].as_str().unwrap().to_string();
    let (_, _, body) = player(&jf, "GET", &format!("/Items?ParentId={shows_view}&IncludeItemTypes=Series&Recursive=true"), t, Value::Null).await;
    let series = read(&body)["Items"][0].clone();
    let series_id = series["Id"].as_str().unwrap().to_string();
    assert_eq!((series["Name"].as_str(), series["Type"].as_str(), series["ChildCount"].as_u64(), series["UserData"]["UnplayedItemCount"].as_u64()), (Some("Example World"), Some("Series"), Some(1), Some(2)));
    let (_, _, body) = player(&jf, "GET", &format!("/Shows/{series_id}/Seasons?userId={user}"), t, Value::Null).await;
    let seasons = read(&body);
    assert_eq!((seasons["TotalRecordCount"].as_u64(), seasons["Items"][0]["Name"].as_str(), seasons["Items"][0]["IndexNumber"].as_u64()), (Some(1), Some("Season 1"), Some(1)));
    let season_id = seasons["Items"][0]["Id"].as_str().unwrap().to_string();
    let (_, _, body) = player(&jf, "GET", &format!("/Shows/{series_id}/Episodes?seasonId={season_id}&fields=MediaSources"), t, Value::Null).await;
    let episodes = read(&body)["Items"].as_array().unwrap().clone();
    assert_eq!(episodes.iter().map(|e| (e["IndexNumber"].as_u64().unwrap(), e["ParentIndexNumber"].as_u64().unwrap(), e["Name"].as_str().unwrap().to_string())).collect::<Vec<_>>(), [(1, 1, "The Original".to_string()), (2, 1, "Chestnut".to_string())]);
    assert_eq!((episodes[0]["SeriesId"].as_str(), episodes[0]["SeasonId"].as_str(), episodes[0]["Type"].as_str()), (Some(series_id.as_str()), Some(season_id.as_str()), Some("Episode")));
    let (first, second) = (episodes[0]["Id"].as_str().unwrap().to_string(), episodes[1]["Id"].as_str().unwrap().to_string());
    assert_eq!(player(&jf, "HEAD", &format!("/Videos/{first}/stream.mkv"), t, Value::Null).await.0, 200);
    // The same children, asked for the other way players do.
    let (_, _, body) = player(&jf, "GET", &format!("/Items?ParentId={season_id}"), t, Value::Null).await;
    assert_eq!(read(&body)["Items"].as_array().unwrap().len(), 2);

    // Part-way through the film: it is offered to resume. Stopped near the end: watched.
    assert_eq!(player(&jf, "POST", "/Sessions/Playing/Progress", t, json!({"ItemId": film_id, "PositionTicks": 10_000_000, "IsPaused": false})).await.0, 204);
    let (_, _, body) = player(&jf, "GET", "/UserItems/Resume", t, Value::Null).await;
    let resume = read(&body);
    assert_eq!((resume["Items"][0]["Id"].as_str(), resume["Items"][0]["UserData"]["PlaybackPositionTicks"].as_i64()), (Some(film_id.as_str()), Some(10_000_000)));
    assert_eq!(player(&jf, "POST", "/Sessions/Playing/Stopped", t, json!({"ItemId": film_id, "PositionTicks": 19_500_000})).await.0, 204);
    let seen = w.app.db.watched(1, "movie", movie.id).unwrap();
    assert_eq!((seen.played, seen.play_count, seen.position), (true, 1, 0.0));
    let (_, _, body) = player(&jf, "GET", "/UserItems/Resume", t, Value::Null).await;
    assert_eq!(read(&body)["TotalRecordCount"], 0);
    let (_, _, body) = player(&jf, "GET", &format!("/Items?ParentId={movies_view}&Filters=IsUnplayed"), t, Value::Null).await;
    assert_eq!(read(&body)["TotalRecordCount"], 0);

    // Marking the first episode watched puts the second up next; un-marking takes it back.
    let (_, _, body) = player(&jf, "GET", "/Shows/NextUp", t, Value::Null).await;
    assert_eq!(read(&body)["TotalRecordCount"], 0);
    let (status, _, body) = player(&jf, "POST", &format!("/UserPlayedItems/{first}"), t, Value::Null).await;
    assert_eq!((status, read(&body)["Played"].as_bool()), (200, Some(true)));
    let (_, _, body) = player(&jf, "GET", &format!("/Shows/NextUp?seriesId={series_id}"), t, Value::Null).await;
    assert_eq!(read(&body)["Items"][0]["Id"], second.as_str());
    let (_, _, body) = player(&jf, "GET", &format!("/Items/{series_id}"), t, Value::Null).await;
    assert_eq!(read(&body)["UserData"]["UnplayedItemCount"], 1);
    let (_, _, body) = player(&jf, "POST", &format!("/Users/{user}/PlayedItems/{series_id}"), t, Value::Null).await;
    assert_eq!(read(&body)["Played"], true);
    let (_, _, body) = player(&jf, "DELETE", &format!("/Users/{user}/PlayedItems/{series_id}"), t, Value::Null).await;
    assert_eq!((read(&body)["Played"].as_bool(), read(&body)["UnplayedItemCount"].as_u64()), (Some(false), Some(2)));
    let (_, _, body) = player(&jf, "POST", &format!("/UserFavoriteItems/{series_id}"), t, Value::Null).await;
    assert_eq!(read(&body)["IsFavorite"], true);
    let (_, _, body) = player(&jf, "GET", "/Items?Recursive=true&IsFavorite=true", t, Value::Null).await;
    assert_eq!(read(&body)["Items"][0]["Id"], series_id.as_str());

    // What Spool does not offer is answered quietly where Jellyfin has such a thing, and
    // plainly refused where it has not, so a player does not think a plugin is installed.
    let (status, _, body) = player(&jf, "GET", &format!("/Items/{film_id}/Similar"), t, Value::Null).await;
    assert_eq!((status, read(&body)["TotalRecordCount"].as_u64()), (200, Some(0)));
    assert_eq!(player(&jf, "POST", "/InfuseSync/Checkpoint", t, Value::Null).await.0, 404);
    let (_, _, body) = player(&jf, "GET", "/Search/Hints?searchTerm=signal", t, Value::Null).await;
    assert_eq!(read(&body)["SearchHints"][0]["Name"], "The Signal");

    // What players asked for is kept for a while, without their tokens.
    let log = w.app.player_requests.lock().iter().cloned().collect::<Vec<_>>().join("\n");
    assert!(log.contains("/Shows/NextUp") && log.contains("Infuse-Direct/8.5") && !log.contains(&token), "{log}");

    // Each person has an account of their own. Another is added, and has watched nothing
    // of what the first has: the film the first finished is unplayed to them.
    let key = w.app.settings.general().api_key;
    let by = Some(("x-api-key", key.as_str()));
    let (status, added) = call_as(&app_api, by, "POST", "/api/accounts", json!({"name": "Ada", "password": "her-own"})).await;
    assert_eq!((status, added["id"].as_i64()), (200, Some(2)), "{added}");
    assert_eq!(call_as(&app_api, by, "POST", "/api/accounts", json!({"name": "ada"})).await.0, 409, "one name, whatever its capitals");
    assert_eq!(call_as(&app_api, by, "POST", "/api/accounts", json!({"name": "  "})).await.0, 400);
    assert_eq!(player(&jf, "POST", "/Users/AuthenticateByName", None, json!({"Username": "Ada", "Pw": "tv-pass"})).await.0, 401, "one account's password is not another's");
    let (status, _, body) = player(&jf, "POST", "/Users/AuthenticateByName", None, json!({"Username": "ada", "Pw": "her-own"})).await;
    assert_eq!(status, 200);
    let hers = read(&body);
    assert_eq!(hers["User"]["Name"], "Ada");
    assert_ne!(hers["User"]["Id"], signed["User"]["Id"], "each account is its own person to a player");
    let ada = hers["AccessToken"].as_str().unwrap().to_string();
    let a = Some(ada.as_str());
    let (_, _, body) = player(&jf, "GET", &format!("/Items/{film_id}"), a, Value::Null).await;
    assert_eq!(read(&body)["UserData"]["Played"], false, "what one has watched is not what another has");
    let (_, _, body) = player(&jf, "GET", &format!("/Items/{film_id}"), t, Value::Null).await;
    assert_eq!(read(&body)["UserData"]["Played"], true);
    assert_eq!(player(&jf, "POST", "/Sessions/Playing/Progress", a, json!({"ItemId": film_id, "PositionTicks": 20_000_000})).await.0, 204);
    assert_eq!(w.app.db.watched(2, "movie", movie.id).unwrap().position, 2.0);
    assert_eq!(w.app.db.watched(1, "movie", movie.id).unwrap().position, 0.0);

    // The accounts, the players signed in to each, and nothing that would let one in.
    let (_, listed) = call_as(&app_api, by, "GET", "/api/accounts", Value::Null).await;
    let of = |name: &str| listed["accounts"].as_array().unwrap().iter().find(|x| x["name"] == name).unwrap().clone();
    assert_eq!((of("spool")["has_password"].as_bool(), of("spool")["devices"].as_array().unwrap().len(), of("Ada")["devices"][0]["client"].as_str()), (Some(true), 1, Some("Infuse-Direct")), "{listed}");
    assert!(!listed.to_string().contains(&ada) && !listed.to_string().contains(&token) && !listed.to_string().contains("argon2"), "no token and no password leaves by this door");

    // A player is signed out by name; a new password signs out every player of the account,
    // and of that account only; and an account that may not sign in is turned away.
    let hers_device = of("Ada")["devices"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(call_as(&app_api, by, "DELETE", &format!("/api/accounts/2/devices/{hers_device}"), Value::Null).await.0, 200);
    assert_eq!(player(&jf, "GET", "/Users/Me", a, Value::Null).await.0, 401);
    assert_eq!(player(&jf, "GET", "/Users/Me", t, Value::Null).await.0, 200);
    assert_eq!(call_as(&app_api, by, "PUT", "/api/accounts/1", json!({"password": "another"})).await.0, 200);
    assert_eq!(player(&jf, "GET", "/Users/Me", t, Value::Null).await.0, 401);
    assert_eq!(player(&jf, "POST", "/Users/AuthenticateByName", None, json!({"Username": "spool", "Pw": "another"})).await.0, 200);
    let (_, _, body) = player(&jf, "POST", "/Users/AuthenticateByName", None, json!({"Username": "Ada", "Pw": "her-own"})).await;
    let ada = read(&body)["AccessToken"].as_str().unwrap().to_string();
    assert_eq!(call_as(&app_api, by, "PUT", "/api/accounts/2", json!({"enabled": false})).await.0, 200);
    assert_eq!(player(&jf, "GET", "/Users/Me", Some(ada.as_str()), Value::Null).await.0, 401);
    assert_eq!(player(&jf, "POST", "/Users/AuthenticateByName", None, json!({"Username": "Ada", "Pw": "her-own"})).await.0, 401);
    let (_, status) = call_as(&app_api, by, "GET", "/api/players", Value::Null).await;
    assert_eq!((status["enabled"].as_bool(), status["signed_in"].as_u64()), (Some(true), Some(1)));
    // A player may ask who there is to sign in as before it has: the accounts that may, by
    // name, and nothing that would let it in.
    let (status, _, body) = player(&jf, "GET", "/Users/Public", None, Value::Null).await;
    let offered = read(&body);
    assert_eq!((status, offered.as_array().unwrap().len(), offered[0]["Name"].as_str(), offered[0]["HasPassword"].as_bool()), (200, 1, Some("spool"), Some(true)), "Ada may not sign in, so is not offered: {offered}");
    assert!(!offered.to_string().contains("argon2"));
    assert_eq!(call_as(&app_api, by, "PUT", "/api/accounts/2", json!({"enabled": true})).await.0, 200);
    let (_, _, body) = player(&jf, "GET", "/Users/Public", None, Value::Null).await;
    assert_eq!(read(&body).as_array().unwrap().iter().map(|u| u["Name"].as_str().unwrap()).collect::<Vec<_>>(), ["spool", "Ada"]);

    // A player that signed in before names were kept is given the one it goes by when it
    // next asks for anything; and one not seen for ninety days is signed out, where one that
    // has only not been seen yet is kept.
    let at = spool::db::now();
    for (token, created_at) in [("carried-over", at - 86400), ("long-gone", at - spool::app::SIGNED_IN_FOR - 86400)] {
        w.app.db.add_device(&Device { token: token.into(), account_id: 1, name: String::new(), client: String::new(), created_at, seen_at: 0 }).unwrap();
        w.app.db.with(|c| c.execute("UPDATE devices SET seen_at = 0 WHERE token = ?1", [token])).unwrap();
    }
    w.app.reload_players();
    assert_eq!(w.app.db.remove_devices_unseen(at - spool::app::SIGNED_IN_FOR).unwrap(), 1);
    w.app.reload_players();
    assert_eq!(player(&jf, "GET", "/Users/Me", Some("long-gone"), Value::Null).await.0, 401);
    assert_eq!(player(&jf, "GET", "/Users/Me", Some("carried-over"), Value::Null).await.0, 200);
    let named = w.app.db.devices().unwrap().into_iter().find(|d| d.token == "carried-over").unwrap();
    assert_eq!((named.name.as_str(), named.client.as_str()), ("Apple TV", "Infuse-Direct"));

    // Removed, an account takes what it watched with it, and leaves the others' alone.
    assert_eq!(call_as(&app_api, by, "DELETE", "/api/accounts/2", Value::Null).await.0, 200);
    assert_eq!(w.app.db.watched(2, "movie", movie.id).unwrap().position, 0.0);
    assert!(w.app.db.watched(1, "movie", movie.id).unwrap().played);
}

#[tokio::test(flavor = "multi_thread")]
async fn each_person_signs_in_as_themselves_and_keeps_what_they_watched() {
    use serde_json::{json, Value};
    use tower::ServiceExt;
    let w = World::new(Mode::Active).await;
    let router = spool::api::router(w.app.clone());
    let mut movie = w.movie("Kept", 2011, "tt4444444");
    movie.path = w.root.join("Movies/Kept (2011)").to_string_lossy().to_string();
    w.app.db.save_title(&mut movie).unwrap();
    let file: MediaFile = serde_json::from_value(json!({"title_id": movie.id, "rel_path": "film.mkv", "size": 4, "quality": {"quality": "bluray-1080p"}})).unwrap();
    w.app.db.with(|c| c.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1, 'film.mkv', 4, ?2)", rusqlite::params![movie.id, serde_json::to_string(&file).unwrap()])).unwrap();
    let seen = |list: &Value| list.as_array().unwrap().iter().find(|t| t["id"] == movie.id).unwrap()["watched"].clone();

    // With nobody's password set the app is open, as the first person who runs Spool, and a
    // browser is signed in as them so that it stays theirs once there are passwords.
    let opened = router.clone().oneshot(axum::http::Request::builder().uri("/api/session").body(axum::body::Body::empty()).unwrap()).await.unwrap();
    let mine = opened.headers().get("set-cookie").and_then(|c| c.to_str().ok()).map(|c| c.split(';').next().unwrap().to_string()).expect("a session for the open app");
    let session: Value = serde_json::from_slice(&axum::body::to_bytes(opened.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!((session["required"].as_bool(), session["me"]["name"].as_str(), session["manages"].as_bool()), (Some(false), Some("spool"), Some(true)), "{session}");
    assert_eq!(call(&router, "POST", "/api/watched", json!({"title_id": movie.id, "played": true})).await.0, 200);
    assert_eq!(seen(&call(&router, "GET", "/api/titles", Value::Null).await.1)["done"], true);

    // Once someone has a password, everyone signs in as themselves; the browser that was
    // open is still its person's.
    let as_me = Some(("cookie", mine.as_str()));
    assert_eq!(call_as(&router, as_me, "POST", "/api/accounts", json!({"name": "Ada", "password": "hers"})).await.0, 200);
    assert_eq!(call(&router, "GET", "/api/status", Value::Null).await.0, 401);
    assert_eq!(call_as(&router, as_me, "PUT", "/api/accounts/1", json!({"password": "mine"})).await.0, 200);
    assert_eq!(call_as(&router, as_me, "GET", "/api/status", Value::Null).await.0, 200, "the browser that changed its own password stays in");
    let sign_in = |name: &'static str, password: &'static str| {
        let router = router.clone();
        async move {
            let req = axum::http::Request::builder().method("POST").uri("/api/login").header("content-type", "application/json").body(axum::body::Body::from(json!({"name": name, "password": password}).to_string())).unwrap();
            let resp = router.oneshot(req).await.unwrap();
            let cookie = resp.headers().get("set-cookie").and_then(|c| c.to_str().ok()).map(|c| c.split(';').next().unwrap().to_string());
            (resp.status().as_u16(), cookie)
        }
    };
    assert_eq!(sign_in("ada", "mine").await.0, 401, "one person's password is not another's");
    let (status, ada) = sign_in("ADA", "hers").await;
    assert_eq!(status, 200);
    let ada = ada.unwrap();
    let as_ada = Some(("cookie", ada.as_str()));
    let (_, session) = call_as(&router, as_ada, "GET", "/api/session", Value::Null).await;
    assert_eq!((session["me"]["name"].as_str(), session["manages"].as_bool()), (Some("Ada"), Some(false)), "{session}");

    // Someone who does not run Spool browses, and keeps what they watched; what the first
    // person watched is not theirs.
    let (status, list) = call_as(&router, as_ada, "GET", "/api/titles", Value::Null).await;
    assert_eq!((status, seen(&list)), (200, Value::Null));
    assert_eq!(call_as(&router, as_ada, "POST", "/api/watched", json!({"title_id": movie.id, "played": true})).await.0, 200);
    assert!(w.app.db.watched(2, "movie", movie.id).unwrap().played);
    let (_, page) = call_as(&router, as_ada, "GET", &format!("/api/titles/{}", movie.id), Value::Null).await;
    assert_eq!(page["watched"]["done"], true);
    assert!(page.get("decisions").is_none() && page.get("acquisitions").is_none(), "how it was got is not theirs to see");
    // and changes nothing else.
    for (method, uri) in [("GET", "/api/settings/general"), ("GET", "/api/accounts"), ("PATCH", &format!("/api/titles/{}", movie.id) as &str), ("DELETE", &format!("/api/titles/{}", movie.id)), ("POST", "/api/tasks/rss")] {
        assert_eq!(call_as(&router, as_ada, method, uri, json!({})).await.0, 403, "{method} {uri}");
    }
    assert!(w.app.db.title(movie.id).unwrap().is_some());

    // Unmarked, it is to watch again; a program with the key is nobody, so has nothing to mark.
    assert_eq!(call_as(&router, as_ada, "POST", "/api/watched", json!({"title_id": movie.id, "played": false})).await.0, 200);
    assert!(!w.app.db.watched(2, "movie", movie.id).unwrap().played && w.app.db.watched(1, "movie", movie.id).unwrap().played);
    let key = w.app.settings.general().api_key;
    let by_key = Some(("x-api-key", key.as_str()));
    assert_eq!(call_as(&router, by_key, "POST", "/api/watched", json!({"title_id": movie.id, "played": true})).await.0, 400);

    // Somebody is always left who runs Spool.
    assert_eq!(call_as(&router, by_key, "PUT", "/api/accounts/1", json!({"manages": false})).await.0, 409);
    assert_eq!(call_as(&router, by_key, "DELETE", "/api/accounts/1", Value::Null).await.0, 409);
    assert_eq!(call_as(&router, by_key, "PUT", "/api/accounts/2", json!({"manages": true})).await.0, 200);
    assert_eq!(call_as(&router, as_ada, "GET", "/api/settings/general", Value::Null).await.0, 200, "what someone may do follows them at once");
    assert_eq!(call_as(&router, by_key, "PUT", "/api/accounts/1", json!({"manages": false})).await.0, 200);

    // A new password signs a person out of every browser.
    assert_eq!(call_as(&router, by_key, "PUT", "/api/accounts/2", json!({"password": "new"})).await.0, 200);
    assert_eq!(call_as(&router, as_ada, "GET", "/api/titles", Value::Null).await.0, 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn every_change_to_the_library_is_counted_and_can_be_asked_for() {
    use rusqlite::params;
    use serde_json::{json, Value};
    let w = World::new(Mode::Active).await;
    let db = &w.app.db;
    let at = |kind: &str, id: i64| db.changes_since(0, &[kind], u32::MAX).unwrap().into_iter().find(|c| c.id == id);
    let start = db.revision().unwrap();

    // Adding a title counts once; saving it as it was, or with only its search time moved, does not.
    let mut film = w.movie("Alpha", 2001, "tt0000001");
    let added = db.revision().unwrap();
    assert_eq!(added, start + 1);
    db.save_title(&mut film).unwrap();
    film.last_search_at = 99;
    film.last_refresh_at = 99;
    film.details_at = 99;
    db.save_title(&mut film).unwrap();
    assert_eq!(db.revision().unwrap(), added, "nothing a listing shows has changed");
    film.overview = "Now with words.".into();
    db.save_title(&mut film).unwrap();
    assert_eq!(db.revision().unwrap(), added + 1);
    // However often it changes, a title has one place in the order: its latest.
    assert_eq!(db.changes_since(0, &["title"], u32::MAX).unwrap().iter().filter(|c| c.id == film.id).count(), 1);
    assert_eq!(at("title", film.id).unwrap().rev, added + 1);

    // Files and episodes, written from anywhere, even by a statement that names its own way
    // of settling conflicts.
    let (show, eps) = w.series("Gamma", 1001, &[(1, 1, "One"), (1, 2, "Two")]);
    let file_data = json!({"title_id": show.id, "rel_path": "one.mkv", "quality": {"quality": "bluray-1080p"}}).to_string();
    let file_id = db
        .with(|c| {
            c.execute("INSERT OR REPLACE INTO files(title_id, rel_path, size, data) VALUES (?1, 'one.mkv', 10, ?2)", params![show.id, file_data])?;
            Ok(c.last_insert_rowid())
        })
        .unwrap();
    assert_eq!(at("file", file_id).map(|c| (c.title_id, c.removed)), Some((show.id, false)));
    let before = db.revision().unwrap();
    db.with(|c| c.execute("UPDATE files SET data = data WHERE id = ?1", [file_id])).unwrap();
    assert_eq!(db.revision().unwrap(), before, "a file written back as it was has not changed");
    db.with(|c| c.execute("UPDATE episodes SET file_id = ?2 WHERE id = ?1", params![eps[0], file_id])).unwrap();
    assert_eq!(at("episode", eps[0]).unwrap().rev, before + 1);
    // Deleting the file unlinks the episode, and both are noted.
    db.with(|c| c.execute("DELETE FROM files WHERE id = ?1", [file_id])).unwrap();
    assert!(at("file", file_id).unwrap().removed);
    assert!(at("episode", eps[0]).unwrap().rev > before + 1);
    assert!(!at("episode", eps[0]).unwrap().removed);

    // What a player reports is counted too, under the title it belongs to.
    db.add_account("someone", "").unwrap();
    db.save_watched(1, "episode", eps[1], &Watched { position: 30.0, ..Default::default() }).unwrap();
    assert_eq!(at("watched_episode", eps[1]).map(|c| c.title_id), Some(show.id));

    // The web app's list: everything the first time, then only what moved.
    let api = spool::api::router(w.app.clone());
    let (_, plain) = call(&api, "GET", "/api/titles", Value::Null).await;
    assert_eq!(plain.as_array().map(Vec::len), Some(2), "without a count the list is as it always was");
    let (_, first) = call(&api, "GET", "/api/titles?since=0", Value::Null).await;
    assert_eq!((first["full"].as_bool(), first["titles"].as_array().map(Vec::len)), (Some(true), Some(2)));
    let seen = first["revision"].as_i64().unwrap();
    let (_, quiet) = call(&api, "GET", &format!("/api/titles?since={seen}"), Value::Null).await;
    assert_eq!(quiet, json!({"revision": seen, "full": false, "titles": [], "removed": []}));

    // A download starting changes how its title is listed; renaming a profile changes every
    // title that uses it; a deleted title is named as gone, with its episodes.
    let release = json!({"guid": "g", "title": "Alpha.2001.1080p.BluRay.x264-GRP"});
    let mut acq: Acquisition = serde_json::from_value(json!({"title_id": film.id, "state": "downloading", "release": release, "quality": {"quality": "bluray-1080p"}})).unwrap();
    db.save_acquisition(&mut acq).unwrap();
    let (_, moved) = call(&api, "GET", &format!("/api/titles?since={seen}"), Value::Null).await;
    assert_eq!(moved["titles"].as_array().unwrap().iter().map(|t| (t["id"].as_i64().unwrap(), t["active"].as_str())).collect::<Vec<_>>(), [(film.id, Some("downloading"))]);
    let seen = moved["revision"].as_i64().unwrap();
    let mut tv = db.profiles().unwrap().into_iter().find(|p| p.id == show.profile_id).unwrap();
    tv.name = "Television".into();
    db.save_profile(&mut tv).unwrap();
    db.delete_title(film.id).unwrap();
    let (_, after) = call(&api, "GET", &format!("/api/titles?since={seen}"), Value::Null).await;
    assert_eq!(after["titles"].as_array().unwrap().iter().map(|t| (t["id"].as_i64().unwrap(), t["profile"].as_str())).collect::<Vec<_>>(), [(show.id, Some("Television"))]);
    assert_eq!(after["removed"], json!([film.id]));
    db.delete_title(show.id).unwrap();
    assert!(eps.iter().all(|e| at("episode", *e).unwrap().removed));

    // The same record, for anything else that keeps a copy.
    let (_, log) = call(&api, "GET", "/api/changes?since=0&kinds=title", Value::Null).await;
    assert_eq!(log["complete"], true);
    assert_eq!(log["changes"].as_array().unwrap().iter().map(|c| (c["id"].as_i64().unwrap(), c["removed"].as_bool().unwrap())).collect::<Vec<_>>(), [(film.id, true), (show.id, true)]);
    let (_, paged) = call(&api, "GET", "/api/changes?since=0&limit=1", Value::Null).await;
    assert_eq!((paged["changes"].as_array().map(Vec::len), paged["more"].as_bool()), (Some(1), Some(true)));

    // Removals are forgotten in time. Someone who last looked before then is told to start over.
    let latest = db.revision().unwrap();
    assert!(db.prune_changes(spool::db::now() + 10).unwrap() > 0);
    assert_eq!(db.changes_floor().unwrap(), latest);
    let (_, stale) = call(&api, "GET", &format!("/api/titles?since={seen}"), Value::Null).await;
    assert_eq!(stale["full"], true);
    let (_, stale) = call(&api, "GET", &format!("/api/changes?since={seen}"), Value::Null).await;
    assert_eq!(stale["complete"], false);
    let (_, ahead) = call(&api, "GET", &format!("/api/titles?since={}", latest + 50), Value::Null).await;
    assert_eq!(ahead["full"], true, "a count this database never gave out");
}

#[tokio::test(flavor = "multi_thread")]
async fn grids_get_a_small_poster_and_the_full_one_is_kept() {
    use serde_json::Value;
    // A stand-in for the artwork source: one tall poster, larger than any grid shows it.
    let mut big = Vec::new();
    let picture = image::RgbImage::from_fn(500, 750, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]));
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut big, 90).encode_image(&picture).unwrap();
    let served = big.clone();
    let source = axum::Router::new().route("/poster.jpg", get(move || { let b = served.clone(); async move { b } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, source).await.unwrap() });

    let w = World::new(Mode::Active).await;
    let mut film = w.movie("Alpha", 2001, "tt0000001");
    film.poster = Some(format!("http://{addr}/poster.jpg"));
    w.app.db.save_title(&mut film).unwrap();
    let api = spool::api::router(w.app.clone());
    let (_, list) = call(&api, "GET", "/api/titles", Value::Null).await;
    let (small_url, full_url) = (list[0]["poster_small"].as_str().unwrap().to_string(), list[0]["poster"].as_str().unwrap().to_string());
    assert!(small_url.starts_with(&format!("/api/art/{}/small?v=", film.id)) && full_url.starts_with(&format!("/api/art/{}/poster?v=", film.id)));

    let width = |bytes: &[u8]| image::load_from_memory(bytes).unwrap().width();
    let small = w.app.art_small(film.id).await.unwrap();
    assert_eq!(width(&small), 300);
    assert!(small.len() < big.len() / 2, "{} against {}", small.len(), big.len());
    // Asking for the small one fetched the full one too, and both stay on disk.
    let full = w.app.art(film.id, spool::art::Art::Poster).await.unwrap();
    assert_eq!(width(&full), 400);
    let kept = tree(&w.app.data_dir.join("art"));
    assert_eq!(kept.iter().filter(|f| f.contains("-poster-")).count(), 1, "{kept:?}");
    assert_eq!(kept.iter().filter(|f| f.contains("-small-")).count(), 1, "{kept:?}");

    // A new poster replaces each copy and leaves no old ones behind.
    film.poster = Some(format!("http://{addr}/poster.jpg?again"));
    w.app.db.save_title(&mut film).unwrap();
    w.app.art_small(film.id).await.unwrap();
    assert_eq!(tree(&w.app.data_dir.join("art")).len(), 2);

    // A saved release keeps its poster after the title has gone, and lists get that small too.
    const NZB: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><file poster="p" date="1" subject="alpha.mkv (1/1)"><groups><group>alt.binaries.test</group></groups><segments><segment bytes="100" number="1">a@b</segment></segments></file></nzb>"#;
    let release: Release = serde_json::from_value(serde_json::json!({"guid": "g", "title": "Alpha.2001.1080p.BluRay.x264-GRP"})).unwrap();
    let quality = serde_json::from_value(serde_json::json!({"quality": "bluray-1080p"})).unwrap();
    let saved = w.app.archive_save(&film, &release, quality, "movie", "grabbed", NZB).unwrap();
    w.app.db.delete_title(film.id).unwrap();
    let (_, archive) = call(&api, "GET", "/api/archive", Value::Null).await;
    let listed = archive.to_string();
    assert!(listed.contains(&format!("/api/archive/{saved}/art?v=")) && listed.contains("&size=small"), "{listed}");
    assert_eq!(width(&w.app.archive_art_small(saved).await.unwrap()), 300);
    assert_eq!(width(&w.app.archive_art(saved).await.unwrap()), 400);
    let kept = tree(&w.app.data_dir.join("art"));
    assert_eq!(kept.iter().filter(|f| f.starts_with("saved-")).count(), 2, "{kept:?}");
}

/// A stand-in for TMDB and TVmaze that knows a handful of films and one series.
async fn details_source() -> String {
    use axum::extract::Query;
    use serde_json::{json, Value};
    fn films() -> Vec<Value> {
        vec![
            json!({"id": 949, "title": "Heat", "release_date": "1995-12-15", "status": "Released"}),
            json!({"id": 593, "title": "Solaris", "release_date": "1972-03-20", "status": "Released"}),
            json!({"id": 2103, "title": "Solaris", "release_date": "2002-11-27", "status": "Released"}),
            json!({"id": 111, "title": "Alpha", "release_date": "2001-01-01", "status": "Released"}),
            json!({"id": 80, "title": "Pocket Robot War", "release_date": "1989-03-25", "status": "Released", "runtime": 180, "imdb_id": "tt0000080"}),
            json!({"id": 91, "title": "Border Two Example", "release_date": "2013-11-30", "status": "Released", "runtime": 57, "imdb_id": "tt0000091"}),
            json!({"id": 301, "title": "Saga One", "release_date": "2001-05-01", "status": "Released", "belongs_to_collection": {"id": 500, "name": "Saga Collection"}}),
            json!({"id": 303, "title": "Saga Three", "release_date": "2005-05-01", "status": "Released", "belongs_to_collection": {"id": 500, "name": "Saga Collection"}}),
        ]
    }
    fn show() -> Value {
        json!({"id": 77, "name": "Example World", "premiered": "2016-10-02", "status": "Ended", "externals": {"thetvdb": 296762}})
    }
    fn ova() -> Value {
        json!({"id": 88, "name": "Pocket Robot War", "premiered": "1989-03-25", "status": "Ended", "externals": {"thetvdb": 8088, "imdb": "tt0000080"}})
    }
    let router = axum::Router::new()
        .route(
            "/search/movie",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                let want = q.get("query").cloned().unwrap_or_default().to_lowercase();
                axum::Json(json!({"results": films().into_iter().filter(|f| f["title"].as_str().unwrap().to_lowercase() == want).collect::<Vec<_>>()}))
            }),
        )
        .route("/movie/{id}", get(|AxPath(id): AxPath<u64>| async move { axum::Json(films().into_iter().find(|f| f["id"] == id).unwrap_or(Value::Null)) }))
        // A film collection whole, and a film by its IMDb id.
        .route(
            "/collection/500",
            get(|| async { axum::Json(json!({"id": 500, "name": "Saga Collection", "parts": [{"id": 301, "title": "Saga One", "release_date": "2001-05-01"}, {"id": 302, "title": "Saga Two", "release_date": "2003-05-01"}, {"id": 303, "title": "Saga Three", "release_date": "2005-05-01"}]})) }),
        )
        .route("/find/{imdb}", get(|AxPath(imdb): AxPath<String>| async move { axum::Json(json!({"movie_results": films().into_iter().filter(|f| f["imdb_id"] == imdb.as_str()).collect::<Vec<_>>()})) }))
        .route(
            "/search/shows",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                let hit = q.get("q").is_some_and(|n| n.eq_ignore_ascii_case("Example World"));
                axum::Json(if hit { json!([{"show": show()}]) } else { json!([]) })
            }),
        )
        // The same work as the series source lists it: six half-hour episodes.
        .route(
            "/lookup/shows",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                if q.get("imdb").is_some_and(|i| i == "tt0000080") || q.get("thetvdb").is_some_and(|i| i == "8088") {
                    Ok(axum::Json(ova()))
                } else {
                    Err(axum::http::StatusCode::NOT_FOUND)
                }
            }),
        )
        .route("/shows/88", get(|| async { axum::Json(ova()) }))
        .route("/shows/88/akas", get(|| async { axum::Json(json!([])) }))
        .route("/shows/88/episodes", get(|| async { axum::Json(Value::Array((1..=6).map(|n| json!({"type": "regular", "season": 1, "number": n, "name": format!("Chapter {n}"), "airdate": "1989-03-25", "runtime": 30})).collect())) }))
        // The anime sources: the list that ties ids together, and AniList itself.
        .route(
            "/anime-ids.json",
            get(|| async {
                axum::Json(json!([
                    {"type": "OVA", "anilist_id": 82, "imdb_id": ["tt0000080"], "tvdb_id": 8088, "season": {"tvdb": 1}},
                    {"type": "TV", "anilist_id": 80, "tvdb_id": 296762, "season": {"tvdb": 1}},
                    {"type": "TV", "anilist_id": 85, "tvdb_id": 9085, "season": {"tvdb": 1}},
                    // The second season of the first series, a work of its own to AniList.
                    {"type": "TV", "anilist_id": 81, "tvdb_id": 296762, "season": {"tvdb": 2}},
                    // A set of specials that the list gives two films' ids to.
                    {"type": "SPECIAL", "anilist_id": 90, "imdb_id": ["tt0000091", "tt0000092"], "tvdb_id": 9090, "season": {"tvdb": 0}},
                ]))
            }),
        )
        .route(
            "/anilist",
            axum::routing::post(|axum::Json(q): axum::Json<Value>| async move {
                let brief = |id: u64, format: &str, year: u64, english: &str| json!({"id": id, "type": "ANIME", "format": format, "seasonYear": year, "title": {"english": english, "romaji": format!("{english} (romaji)")}});
                let with = |mut m: Value, edges: Value| {
                    m["relations"] = json!({"edges": edges});
                    m
                };
                let series = || with(brief(80, "TV", 1979, "Robot War"), json!([{"relationType": "SIDE_STORY", "node": brief(82, "OVA", 1989, "Pocket Robot War")}, {"relationType": "SEQUEL", "node": brief(85, "TV", 1985, "Robot War Z")}]));
                let ova = || {
                    let mut m = with(brief(82, "OVA", 1989, "Pocket Robot War"), json!([{"relationType": "PARENT", "node": series()}]));
                    (m["episodes"], m["duration"]) = (json!(6), json!(30));
                    m
                };
                let mut whole = series();
                whole["relations"]["edges"][0]["node"] = with(brief(82, "OVA", 1989, "Pocket Robot War"), json!([{"relationType": "PARENT", "node": brief(80, "TV", 1979, "Robot War")}]));
                whole["relations"]["edges"][1]["node"] = with(brief(85, "TV", 1985, "Robot War Z"), json!([{"relationType": "PREQUEL", "node": brief(80, "TV", 1979, "Robot War")}]));
                let asked: Vec<u64> = q["variables"]["ids"].as_array().into_iter().flatten().filter_map(|x| x.as_u64()).collect();
                let mut shorts = with(brief(90, "SPECIAL", 2013, "Border Shorts"), json!([{"relationType": "PARENT", "node": with(brief(91, "MOVIE", 2013, "Border: Two Example"), json!([]))}]));
                shorts["episodes"] = json!(4);
                // A search by name finds the OVA under any of its names.
                if let Some(name) = q["variables"]["q"].as_str() {
                    let hit = name.to_lowercase().contains("pocket robot");
                    return axum::Json(json!({"data": {"Page": {"media": if hit { vec![ova()] } else { vec![] }}}}));
                }
                let second = with(brief(81, "TV", 1981, "Robot War II"), json!([{"relationType": "PREQUEL", "node": with(brief(80, "TV", 1979, "Robot War"), json!([]))}]));
                let media: Vec<Value> = asked.iter().filter_map(|id| match id { 80 => Some(whole.clone()), 81 => Some(second.clone()), 82 => Some(ova()), 90 => Some(shorts.clone()), _ => None }).collect();
                axum::Json(json!({"data": {"Page": {"media": media}}}))
            }),
        )
        .route("/shows/77", get(|| async { axum::Json(show()) }))
        .route("/shows/77/akas", get(|| async { axum::Json(json!([])) }))
        .route(
            "/shows/77/episodes",
            get(|| async { axum::Json(json!([{"type": "regular", "season": 1, "number": 1, "name": "The Original", "airdate": "2016-10-02"}, {"type": "regular", "season": 1, "number": 2, "name": "Chestnut", "airdate": "2016-10-09"}])) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_location_is_brought_in_read_or_managed_and_left_alone_while_away() {
    use serde_json::{json, Value};
    let source = details_source().await;
    let w = World::with_sources(Mode::Active, Some((&source, &source))).await;
    let api = spool::api::router(w.app.clone());
    let general = || serde_json::to_value(w.app.settings.general()).unwrap();
    let put = |v: Value| { let api = api.clone(); async move { call(&api, "PUT", "/api/settings/general", v).await } };

    // A library that already exists somewhere else, named the ways people name things.
    let (there, away) = (w.root.join("Elsewhere"), w.root.join("Elsewhere.away"));
    let lay = |rel: &str| {
        let p = there.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"not really video").unwrap();
    };
    lay("films/Heat (1995)/Heat.1995.1080p.BluRay.x264-GRP.mkv");
    lay("films/Solaris/solaris.mkv");
    lay("films/Mystery Thing (2001)/thing.mkv");
    lay("films/Alpha (2001)/alpha.mkv");
    lay("films/Notes/readme.txt");
    // A folder whose name says nothing, with the notes a media server left in it; and one
    // holding only a trailer, which is no film.
    lay("films/Sci Fi Night/film.mkv");
    std::fs::write(there.join("films/Sci Fi Night/movie.nfo"), "<movie><title>Solaris</title><tmdbid>2103</tmdbid></movie>").unwrap();
    lay("films/Coming Soon/Trailers/teaser.mkv");
    lay("tv/Example World/Season 01/Example.World.S01E01.1080p.BluRay.x264-GRP.mkv");
    // One of them is in the main library already, without a file: a title brought over
    // from another manager, whose file was here all along.
    let mut alpha = w.movie("Alpha", 2001, "tt0000001");
    alpha.tmdb_id = Some(111);
    w.app.db.save_title(&mut alpha).unwrap();

    let mut g = general();
    g["tmdb_api_key"] = json!("key");
    // A location may not share a folder with another.
    g["locations"] = json!([{"name": "Inside", "movie_root": w.root.join("Movies/sub")}]);
    let (status, refused) = put(g.clone()).await;
    assert_eq!(status, 400, "{refused}");
    g["locations"] = json!([{"name": "Elsewhere", "movie_root": there.join("films"), "series_root": format!("{}/", there.join("tv").display()), "manage": false}]);
    assert_eq!(put(g).await.0, 200);
    let place = w.app.settings.general().locations[0].clone();
    assert_eq!((place.id, place.series_root.as_str()), (1, there.join("tv").to_str().unwrap()));
    let (_, places) = call(&api, "GET", "/api/locations", Value::Null).await;
    assert_eq!(places.as_array().unwrap().iter().map(|p| (p["id"].as_u64().unwrap(), p["online"].as_bool().unwrap(), p["manage"].as_bool().unwrap())).collect::<Vec<_>>(), [(0, true, true), (1, true, false)]);

    // Looking says what each folder is, and adds nothing.
    let finished = |api: axum::Router| async move {
        for _ in 0..200 {
            let (_, f) = call(&api, "GET", "/api/locations/1/found", Value::Null).await;
            if f["running"] == false {
                return f;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the location was never finished with");
    };
    assert_eq!(call(&api, "POST", "/api/locations/1/add", json!({})).await.0, 409, "not before looking");
    assert_eq!(call(&api, "POST", "/api/locations/1/look", Value::Null).await.0, 200);
    let found = finished(api.clone()).await;
    let state = |f: &Value, name: &str| f["found"].as_array().unwrap().iter().find(|x| x["folder"].as_str().unwrap().ends_with(name)).map(|x| x["state"].as_str().unwrap().to_string());
    assert_eq!(state(&found, "Heat (1995)").as_deref(), Some("new"));
    assert_eq!(state(&found, "Solaris").as_deref(), Some("unclear"), "two films share the name and the folder gives no year");
    assert_eq!(state(&found, "Mystery Thing (2001)").as_deref(), Some("unknown"));
    assert_eq!(state(&found, "Alpha (2001)").as_deref(), Some("new"), "the same title, to be pointed at this folder");
    assert_eq!(state(&found, "Example World").as_deref(), Some("new"));
    assert_eq!(state(&found, "Notes"), None, "a folder with no video is not a title");
    assert_eq!(state(&found, "Sci Fi Night").as_deref(), Some("new"), "its notes name it");
    assert_eq!(state(&found, "Coming Soon"), None, "a trailer alone is not a film");
    assert_eq!(w.app.db.titles(None).unwrap().len(), 1);

    // Adding takes the sure ones and the one settled by hand, where they lie.
    let before = tree(&there);
    assert_eq!(call(&api, "POST", "/api/locations/1/add", json!({"picks": [{"folder": there.join("films/Solaris"), "tmdb_id": 593}]})).await.0, 200);
    let added = finished(api.clone()).await;
    assert_eq!(added["message"], "Added 5 titles", "{added}");
    assert_eq!(tree(&there), before, "nothing on the location was touched");
    let titles = w.app.db.titles(None).unwrap();
    let by = |name: &str| titles.iter().find(|t| t.title == name && t.year != 2002).cloned().unwrap();
    assert!(titles.iter().any(|t| t.year == 2002 && t.path.ends_with("Sci Fi Night") && t.local.notes.tmdb_id == Some(2103)));
    let (heat, solaris, show) = (by("Heat"), by("Solaris"), by("Example World"));
    // The title that was already in the library is the same one, now found where its file is.
    let pointed = by("Alpha");
    assert_eq!((pointed.id, pointed.path.as_str(), w.app.db.files(pointed.id).unwrap().len()), (alpha.id, there.join("films/Alpha (2001)").to_str().unwrap(), 1));
    assert_eq!(titles.iter().filter(|t| t.title == "Alpha").count(), 1, "and not a second copy of it");
    assert_eq!((heat.path.as_str(), heat.monitored, solaris.year), (there.join("films/Heat (1995)").to_str().unwrap(), false, 1972));
    let heat_file = w.app.db.files(heat.id).unwrap().remove(0);
    assert_eq!(serde_json::to_value(&heat_file.quality).unwrap()["quality"], "bluray-1080p");
    let eps = w.app.db.episodes(show.id).unwrap();
    assert_eq!(eps.iter().map(|e| e.file_id.is_some()).collect::<Vec<_>>(), [true, false]);
    // A second look finds them where they now belong.
    call(&api, "POST", "/api/locations/1/look", Value::Null).await;
    assert_eq!(state(&finished(api.clone()).await, "Heat (1995)").as_deref(), Some("have"));

    // On a location Spool only reads, a title is listed and played, and nothing else.
    let listed = |api: axum::Router, id: i64| async move { call(&api, "GET", "/api/titles", Value::Null).await.1.as_array().unwrap().iter().find(|t| t["id"] == id).cloned().unwrap() };
    let row = listed(api.clone(), heat.id).await;
    assert_eq!((row["location"].as_str(), row["read_only"].as_bool(), row["offline"].as_bool(), row["file_count"].as_u64()), (Some("Elsewhere"), Some(true), Some(false), Some(1)));
    let outcome = w.app.search(heat.id, Scope::Movie, true, true).await.unwrap();
    assert!(outcome.message.starts_with("Not searched: Heat is on Elsewhere, which Spool only reads"), "{}", outcome.message);
    assert!(w.app.delete_file(heat_file.id).is_err());
    assert_eq!(call(&api, "DELETE", &format!("/api/files/{}", heat_file.id), Value::Null).await.0, 400);
    assert!(there.join("films/Heat (1995)/Heat.1995.1080p.BluRay.x264-GRP.mkv").exists());
    assert!(w.app.missing_subtitles().unwrap().is_empty() || !w.app.missing_subtitles().unwrap().iter().any(|(f, _)| *f == heat_file.id));

    // Players see it while the location is there.
    let mut g = w.app.settings.general();
    g.players_enabled = true;
    w.app.settings.set_general(&g).unwrap();
    let jf = spool::jellyfin::router(w.app.clone());
    let (_, _, body) = player(&jf, "POST", "/Users/AuthenticateByName", None, json!({"Username": "spool", "Pw": ""})).await;
    let token = serde_json::from_slice::<Value>(&body).unwrap()["AccessToken"].as_str().unwrap().to_string();
    let films = |jf: axum::Router, token: String| async move {
        let (_, _, body) = player(&jf, "GET", "/Items?Recursive=true&IncludeItemTypes=Movie", Some(&token), Value::Null).await;
        let mut names: Vec<String> = serde_json::from_slice::<Value>(&body).unwrap()["Items"].as_array().unwrap().iter().map(|i| i["Name"].as_str().unwrap().to_string()).collect();
        names.sort();
        names
    };
    assert_eq!(films(jf.clone(), token.clone()).await, ["Alpha", "Heat", "Solaris", "Solaris"]);

    // The location goes away. Its titles stay, marked offline, and nothing is concluded from
    // files that cannot be seen.
    std::fs::rename(&there, &away).unwrap();
    w.app.watch_locations();
    let row = listed(api.clone(), heat.id).await;
    assert_eq!((row["offline"].as_bool(), row["file_count"].as_u64()), (Some(true), Some(1)));
    let (_, status) = call(&api, "GET", "/api/status", Value::Null).await;
    assert!(status["warnings"].as_array().unwrap().iter().any(|m| m.as_str().unwrap().starts_with("Elsewhere cannot be reached. Its 5 titles are offline")), "{status}");
    w.app.scan_all().await.unwrap();
    assert_eq!(w.app.db.files(heat.id).unwrap().len(), 1, "a file that cannot be seen is not a file that has gone");
    assert!(films(jf.clone(), token.clone()).await.is_empty(), "players are not offered what cannot be played");
    assert_eq!(call(&api, "POST", "/api/locations/1/look", Value::Null).await.0, 409);

    // And comes back.
    std::fs::rename(&away, &there).unwrap();
    w.app.watch_locations();
    assert_eq!(listed(api.clone(), heat.id).await["offline"], false);
    assert_eq!(films(jf.clone(), token.clone()).await, ["Alpha", "Heat", "Solaris", "Solaris"]);

    // Allowed to manage the location, Spool treats it as it does the main one, and can be
    // told to put new titles there.
    let mut g = general();
    g["locations"][0]["manage"] = json!(true);
    g["new_titles_location"] = json!(1);
    assert_eq!(put(g).await.0, 200);
    assert_eq!(listed(api.clone(), heat.id).await["read_only"], false);
    let mut fresh = w.movie("Gamma", 2010, "tt0000003");
    w.app.ensure_title_path(&mut fresh).unwrap();
    assert!(fresh.path.starts_with(there.join("films").to_str().unwrap()), "{}", fresh.path);
    w.app.delete_file(heat_file.id).unwrap();
    assert!(!there.join("films/Heat (1995)/Heat.1995.1080p.BluRay.x264-GRP.mkv").exists());
    assert!(tree(&there.join("films/.spool-recycle")).iter().any(|f| f.ends_with("Heat.1995.1080p.BluRay.x264-GRP.mkv")), "recycled on the same location");

    // With that location away, a new title goes to the main library rather than waiting.
    std::fs::rename(&there, &away).unwrap();
    w.app.watch_locations();
    let mut later = w.movie("Delta", 2011, "tt0000004");
    w.app.ensure_title_path(&mut later).unwrap();
    assert!(later.path.starts_with(w.root.join("Movies").to_str().unwrap()), "{}", later.path);
    let blocked = w.app.search(solaris.id, Scope::Movie, true, true).await.unwrap();
    assert!(blocked.message.contains("cannot be reached right now"), "{}", blocked.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_title_is_kept_on_the_location_the_user_chooses() {
    use serde_json::{json, Value};
    let source = details_source().await;
    let w = World::with_sources(Mode::Shadow, Some((&source, &source))).await;
    let api = spool::api::router(w.app.clone());
    let (shelf, vault) = (w.root.join("Shelf"), w.root.join("Vault"));
    for d in [&shelf, &vault] {
        std::fs::create_dir_all(d.join("films")).unwrap();
    }
    let mut g = serde_json::to_value(w.app.settings.general()).unwrap();
    g["tmdb_api_key"] = json!("key");
    g["locations"] = json!([{"name": "Shelf", "movie_root": shelf.join("films"), "manage": true}, {"name": "Vault", "movie_root": vault.join("films"), "manage": false}]);
    assert_eq!(call(&api, "PUT", "/api/settings/general", g).await.0, 200);
    let add = |tmdb: u32, location: Value| {
        let api = api.clone();
        let profile = w.profile("movie");
        async move { call(&api, "POST", "/api/titles", json!({"kind": "movie", "tmdb_id": tmdb, "profile_id": profile, "search": false, "location": location})).await }
    };
    // With none named, where new titles go by default: the main library.
    let (_, heat) = add(949, Value::Null).await;
    assert!(heat["path"].as_str().unwrap().starts_with(w.root.join("Movies").to_str().unwrap()), "{heat}");
    // Named: on that location.
    let (_, alpha) = add(111, json!(1)).await;
    assert!(alpha["path"].as_str().unwrap().starts_with(shelf.join("films").to_str().unwrap()), "{alpha}");
    // A location Spool only reads cannot take it, and says so; the title is not added elsewhere.
    let (status, refused) = add(593, json!(2)).await;
    assert!(status >= 400 && refused.to_string().contains("Vault cannot take a new film"), "{status} {refused}");
    assert_eq!(w.app.db.titles(None).unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn spool_can_say_whether_it_is_busy_whether_a_location_works_and_whether_a_profile_fits() {
    use serde_json::{json, Value};
    let w = World::new(Mode::Active).await;
    let api = spool::api::router(w.app.clone());
    let (shelf, vault) = (w.root.join("Shelf/films"), w.root.join("Vault/films"));
    std::fs::create_dir_all(&shelf).unwrap();
    std::fs::create_dir_all(vault.join("Heat (1995)")).unwrap();
    std::fs::write(vault.join("Heat (1995)/Heat.1995.Remux-1080p.mkv"), vec![1u8; 2 << 20]).unwrap();
    // A profile that keeps things as they are, for what is brought in from the vault.
    let mut keep = QualityProfile::default_hd("movie");
    keep.name = "Keep as is".into();
    keep.id = 0;
    for item in &mut keep.items {
        item.allowed = true;
    }
    let keep_id = w.app.db.save_profile(&mut keep).unwrap();
    let mut g = serde_json::to_value(w.app.settings.general()).unwrap();
    g["locations"] = json!([
        {"name": "Shelf", "movie_root": shelf, "manage": true},
        {"name": "Vault", "movie_root": vault, "manage": false, "movie_profile": keep_id},
        {"name": "Gone", "movie_root": w.root.join("nowhere/films"), "manage": false},
    ]);
    assert_eq!(call(&api, "PUT", "/api/settings/general", g).await.0, 200);

    // Nothing is under way, so a restart interrupts nothing; a player playing changes that.
    let (_, idle) = call(&api, "GET", "/api/busy", Value::Null).await;
    assert_eq!((idle["busy"].as_bool(), idle["reasons"].as_array().map(|r| r.len())), (Some(false), Some(0)), "{idle}");
    w.app.last_playing.store(spool::db::now(), std::sync::atomic::Ordering::Relaxed);
    let (_, playing) = call(&api, "GET", "/api/busy", Value::Null).await;
    assert!(playing["busy"] == true && playing["reasons"][0].as_str().unwrap().contains("playing"), "{playing}");
    w.app.last_playing.store(0, std::sync::atomic::Ordering::Relaxed);

    // A location is tried out: one Spool manages is written to, one it only reads is not,
    // and one that is not there is said to be missing, with what that usually means.
    let checked = |id: u32| {
        let api = api.clone();
        async move { call(&api, "POST", &format!("/api/locations/{id}/check"), Value::Null).await.1 }
    };
    let line = |r: &Value, what: &str| r["checks"].as_array().unwrap().iter().find(|c| c["what"].as_str().unwrap().contains(what)).cloned().unwrap_or(Value::Null);
    let managed = checked(1).await;
    assert_eq!(managed["ok"], true, "{managed}");
    assert!(line(&managed, "can write")["detail"].as_str().unwrap().contains("wrote and removed"), "{managed}");
    assert_eq!(tree(&shelf), Vec::<String>::new(), "the test file is gone");
    let read_only = checked(2).await;
    assert!(read_only["ok"] == true && line(&read_only, "can write")["detail"].as_str().unwrap().contains("not tried"), "{read_only}");
    assert!(line(&read_only, "can be read")["detail"].as_str().unwrap().contains("Heat.1995"), "{read_only}");
    let gone = checked(3).await;
    assert!(gone["ok"] == false && line(&gone, "is there")["detail"].as_str().unwrap().contains("not mounted"), "{gone}");

    // What is brought in from a location gets that location's profile, not the usual one.
    let mut heat = w.movie("Heat", 1995, "tt0113277");
    heat.path = vault.join("Heat (1995)").to_string_lossy().to_string();
    heat.profile_id = w.app.profile_for_folder(Kind::Movie, &heat.path);
    w.app.db.save_title(&mut heat).unwrap();
    let usual = w.app.settings.general().default_movie_profile;
    assert_eq!((heat.profile_id, w.app.profile_for_new(Kind::Movie, 1), w.app.profile_for_new(Kind::Movie, 0)), (keep_id, usual, usual));
    w.app.scan_all().await.unwrap();

    // The profile fits what is there; the usual one would not have allowed a remux.
    let (_, fit) = call(&api, "GET", "/api/profiles/fit", Value::Null).await;
    let row = fit.as_array().unwrap().iter().find(|f| f["location"] == "Vault").unwrap().clone();
    assert_eq!((row["profile"].as_str(), row["files"].as_u64(), row["not_allowed"].as_u64(), row["over_target"].as_u64()), (Some("Keep as is"), Some(1), Some(0), Some(0)), "{row}");
    let mut heat = w.app.db.title(heat.id).unwrap().unwrap();
    heat.profile_id = w.profile("movie");
    w.app.db.save_title(&mut heat).unwrap();
    let (_, fit) = call(&api, "GET", "/api/profiles/fit", Value::Null).await;
    let row = fit.as_array().unwrap().iter().find(|f| f["location"] == "Vault").unwrap().clone();
    assert_eq!(row["not_allowed"].as_u64(), Some(1), "{row}");

    // An assistant can ask the same things.
    let mut g = w.app.settings.general();
    g.api_key = "full-key".into();
    w.app.settings.set_general(&g).unwrap();
    let mcp_router = spool::api::router(w.app.clone());
    let (failed, said) = tool(&mcp_router, "full-key", "restart_check", json!({})).await;
    assert!(!failed && said["busy"] == false, "{said}");
    let (failed, places) = tool(&mcp_router, "full-key", "locations", json!({})).await;
    assert!(!failed && places.as_array().unwrap().iter().any(|p| p["name"] == "Vault" && p["spool_manages_it"] == false), "{places}");
    let (failed, fits) = tool(&mcp_router, "full-key", "profile_fit", json!({})).await;
    assert!(!failed && fits.as_array().unwrap().iter().any(|f| f["location"] == "Vault" && f["files_in_a_quality_not_allowed"] == 1), "{fits}");
    let (refused, why) = tool(&mcp_router, "full-key", "move_titles", json!({"title_ids": [heat.id], "location_id": 2})).await;
    assert!(refused && why.to_string().contains("only reads"), "{why}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_demonstration_library_is_laid_out_and_recorded() {
    let w = World::new(Mode::Shadow).await;
    let root = w.root.join("demo");
    assert_eq!(spool::demo::seed(&w.app, &root).await.unwrap(), 14);
    let titles = w.app.db.titles(None).unwrap();
    let on = |part: &str| titles.iter().filter(|t| t.path.contains(part)).count();
    assert_eq!((on("/Movies/"), on("/Shelf/"), on("/Vault/"), titles.iter().filter(|t| t.path.is_empty()).count()), (8, 2, 2, 1));
    let series = titles.iter().find(|t| t.kind == Kind::Series).unwrap();
    assert_eq!(w.app.db.episodes(series.id).unwrap().iter().filter(|e| e.file_id.is_some()).count(), 6, "six of its eight episodes are on disk");
    assert!(spool::demo::seed(&w.app, &root).await.is_err(), "it will not be laid over a library that is there");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_title_is_moved_to_another_location_whole_and_a_read_only_place_keeps_its_original() {
    use serde_json::{json, Value};
    let w = World::new(Mode::Active).await;
    let api = spool::api::router(w.app.clone());
    let (shelf, vault) = (w.root.join("Shelf/films"), w.root.join("Vault/films"));
    for d in [&shelf, &vault] {
        std::fs::create_dir_all(d).unwrap();
    }
    let mut g = serde_json::to_value(w.app.settings.general()).unwrap();
    g["locations"] = json!([{"name": "Shelf", "movie_root": shelf, "manage": true}, {"name": "Vault", "movie_root": vault, "manage": false}]);
    assert_eq!(call(&api, "PUT", "/api/settings/general", g).await.0, 200);
    let lay = |path: PathBuf, bytes: &[u8]| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    };
    let film = |name: &str, year: u32, imdb: &str, folder: PathBuf| {
        let mut t = w.movie(name, year, imdb);
        t.path = folder.to_string_lossy().to_string();
        w.app.db.save_title(&mut t).unwrap();
        t
    };
    let settled = |id: i64| {
        let app = w.app.clone();
        async move {
            for _ in 0..200 {
                if !app.moving(id) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            panic!("the move never finished");
        }
    };

    // A film in the main library, with what lies beside it.
    let home = w.root.join("Movies/Heat (1995)");
    lay(home.join("Heat (1995).mkv"), b"the film");
    lay(home.join("Heat (1995).en.srt"), b"1\n");
    lay(home.join("Trailers/teaser.mkv"), b"teaser");
    lay(home.join("poster.jpg"), b"jpg");
    let heat = film("Heat", 1995, "tt0113277", home.clone());
    w.app.scan_all().await.unwrap();
    let before = tree(&home);

    // Moved to a location Spool manages: everything in the folder goes, and nothing is left.
    let (status, said) = call(&api, "POST", "/api/titles/move", json!({"ids": [heat.id], "location": 1})).await;
    assert_eq!((status, said["queued"].as_u64()), (200, Some(1)), "{said}");
    settled(heat.id).await;
    let there = shelf.join("Heat (1995)");
    assert_eq!(tree(&there), before, "{:?}", w.app.db.attention(false).unwrap().iter().map(|a| a.message.clone()).collect::<Vec<_>>());
    assert!(!home.exists(), "the original folder is gone");
    assert_eq!(tree(&shelf), before.iter().map(|f| format!("Heat (1995)/{f}")).collect::<Vec<_>>(), "and no working folder is left");
    let moved = w.app.db.title(heat.id).unwrap().unwrap();
    assert_eq!(moved.path, there.to_string_lossy());
    let files = w.app.db.files(heat.id).unwrap();
    assert_eq!((files.len(), files[0].rel_path.as_str(), files[0].base.as_deref()), (1, "Heat (1995).mkv", None));
    assert!(w.app.db.history(Some(heat.id), 10).unwrap().iter().any(|h| h.kind == "moved"));
    let (_, row) = call(&api, "GET", &format!("/api/titles/{}", heat.id), Value::Null).await;
    assert_eq!((row["location"].as_str(), row["moving"].as_bool()), (Some("Shelf"), None));

    // Activity shows the move under way and the titles waiting their turn, and nothing after.
    let (_, idle) = call(&api, "GET", "/api/activity", Value::Null).await;
    assert_eq!((idle["moves"]["now"].is_null(), idle["moves"]["waiting"].as_array().map(Vec::len)), (true, Some(0)), "{}", idle["moves"]);
    {
        use std::sync::{atomic::AtomicU64, Arc};
        let mut moves = w.app.moves.lock();
        moves.now = Some(spool::relocate::Moving { title_id: heat.id, to: "Main".into(), done: Arc::new(AtomicU64::new(300)), total: 1200, stop: Default::default(), location: 0 });
        moves.queue.push_back((heat.id, 2));
    }
    let (_, busy) = call(&api, "GET", "/api/activity", Value::Null).await;
    let (now, waiting) = (&busy["moves"]["now"], &busy["moves"]["waiting"]);
    assert_eq!((now["title"].as_str(), now["from"].as_str(), now["to"].as_str(), now["done"].as_u64(), now["total"].as_u64()), (Some("Heat"), Some("Shelf"), Some("Main"), Some(300), Some(1200)), "{now}");
    assert_eq!((waiting.as_array().map(Vec::len), waiting[0]["title"].as_str(), waiting[0]["to"].as_str()), (Some(1), Some("Heat"), Some("Vault")), "{waiting}");
    // A waiting title can be taken out of the line; the move under way is not touched.
    let (_, row) = call(&api, "GET", &format!("/api/titles/{}", heat.id), Value::Null).await;
    assert_eq!(row["move_waiting"].as_bool(), Some(true));
    w.app.moves.lock().queue.push_back((77, 2));
    let (status, said) = call(&api, "POST", "/api/titles/move/cancel", json!({"ids": [77]})).await;
    assert_eq!((status, said["cancelled"].as_u64(), said["stopping"].as_bool()), (200, Some(1), Some(false)), "{said}");
    {
        let moves = w.app.moves.lock();
        assert_eq!((moves.queue.iter().map(|m| m.0).collect::<Vec<_>>(), moves.now.as_ref().map(|m| m.title_id)), (vec![heat.id], Some(heat.id)));
    }
    let (_, said) = call(&api, "POST", "/api/titles/move/cancel", Value::Null).await;
    assert_eq!((said["cancelled"].as_u64(), said["stopping"].as_bool()), (Some(1), Some(false)), "with no titles named, all that wait are taken out");
    assert!(!w.app.moves.lock().now.as_ref().unwrap().stop.load(std::sync::atomic::Ordering::Relaxed), "and the move under way is left alone");
    // Named, the move under way is told to stop, and Activity says so.
    let (_, said) = call(&api, "POST", "/api/titles/move/cancel", json!({"ids": [heat.id]})).await;
    assert_eq!((said["cancelled"].as_u64(), said["stopping"].as_bool()), (Some(0), Some(true)), "{said}");
    let (_, busy) = call(&api, "GET", "/api/activity", Value::Null).await;
    assert_eq!(busy["moves"]["now"]["stopping"].as_bool(), Some(true));
    {
        let mut moves = w.app.moves.lock();
        assert!(moves.queue.is_empty() && moves.now.is_some());
        moves.now = None;
    }

    // Off a location Spool only reads: copied, and the original kept as a further copy.
    let kept = vault.join("Alpha (2001)");
    lay(kept.join("Alpha (2001).mkv"), b"another film");
    let alpha = film("Alpha", 2001, "tt0000001", kept.clone());
    w.app.scan_all().await.unwrap();
    assert_eq!(call(&api, "POST", "/api/titles/move", json!({"ids": [alpha.id], "location": 1})).await.0, 200);
    settled(alpha.id).await;
    let now = w.app.db.title(alpha.id).unwrap().unwrap();
    assert_eq!((now.path.as_str(), now.also.as_slice()), (shelf.join("Alpha (2001)").to_str().unwrap(), &[kept.to_string_lossy().to_string()][..]));
    assert_eq!(std::fs::read(kept.join("Alpha (2001).mkv")).unwrap(), b"another film", "nothing is taken from a place Spool only reads");
    let files = w.app.db.files(alpha.id).unwrap();
    assert_eq!((files.len(), files.iter().filter(|f| f.base.is_some()).count()), (2, 1));

    // The line is written down, and a restart takes it up again, clearing away the half-made
    // copy the interrupted move left behind.
    let waits = vault.join("Beta (2002)");
    lay(waits.join("Beta (2002).mkv"), b"a third film");
    lay(waits.join(".plexmatch"), b"tmdbid: 3\n");
    let beta = film("Beta", 2002, "tt0000002", waits.clone());
    w.app.scan_all().await.unwrap();
    w.app.db.set_setting("moves_waiting", &vec![(beta.id, 1u32), (987_654i64, 1u32)]).unwrap();
    lay(shelf.join(".spool-moving-55/half.mkv"), b"half a copy");
    assert_eq!(w.app.resume_moves().await, 1, "the title that is gone is passed over");
    settled(beta.id).await;
    assert!(!shelf.join(".spool-moving-55").exists());
    assert_eq!(std::fs::read(shelf.join("Beta (2002)/.plexmatch")).unwrap(), b"tmdbid: 3\n", "a media server's hidden note goes with its title");
    for _ in 0..100 {
        if w.app.db.get_setting::<Vec<(i64, u32)>>("moves_waiting").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(w.app.db.get_setting::<Vec<(i64, u32)>>("moves_waiting").is_empty(), "nothing is left written down once the line is done");

    // A batch is checked for room as a whole: what would not fit is refused when it is asked
    // for, by name, and the rest goes ahead.
    let (big, small) = (vault.join("Gamma (2003)"), vault.join("Delta (2004)"));
    lay(big.join("Gamma (2003).mkv"), b"said to be vast");
    lay(small.join("Delta (2004).mkv"), b"small");
    let (gamma, delta) = (film("Gamma", 2003, "tt0000003", big.clone()), film("Delta", 2004, "tt0000004", small.clone()));
    w.app.scan_all().await.unwrap();
    let mut vast = w.app.db.files(gamma.id).unwrap().remove(0);
    vast.size = 1 << 60;
    w.app.db.save_file(&mut vast).unwrap();
    let (status, said) = call(&api, "POST", "/api/titles/move", json!({"ids": [gamma.id, delta.id], "location": 1})).await;
    assert_eq!((status, said["queued"].as_u64(), said["refused"].as_array().map(Vec::len)), (200, Some(1), Some(1)), "{said}");
    assert!(said["refused"][0]["title"] == "Gamma" && said["refused"][0]["why"].to_string().contains("no room"), "{said}");
    settled(delta.id).await;
    assert!(shelf.join("Delta (2004)").exists() && !shelf.join("Gamma (2003)").exists());
    let (status, refused) = call(&api, "POST", "/api/titles/move", json!({"ids": [gamma.id], "location": 1})).await;
    assert!(status == 409 && refused.to_string().contains("no room"), "{status} {refused}");

    // What cannot be moved is refused, with the reason.
    for (location, why) in [(2, "only reads"), (1, "already on Shelf"), (9, "no such location")] {
        let (status, refused) = call(&api, "POST", "/api/titles/move", json!({"ids": [heat.id], "location": location})).await;
        assert!(status == 409 && refused.to_string().contains(why), "{location}: {status} {refused}");
    }
    lay(w.root.join("Movies/Heat (1995)/leftover.txt"), b"x");
    let (status, refused) = call(&api, "POST", "/api/titles/move", json!({"ids": [heat.id], "location": 0})).await;
    assert!(status == 409 && refused.to_string().contains("already has a folder"), "{status} {refused}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_title_may_be_held_in_two_places_and_played_from_whichever_is_there() {
    use serde_json::{json, Value};
    let source = details_source().await;
    let w = World::with_sources(Mode::Active, Some((&source, &source))).await;
    let api = spool::api::router(w.app.clone());
    let lay = |path: PathBuf| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"not really video").unwrap();
    };
    // A film and the first episode of a series, in the main library.
    let mut heat = w.movie("Heat", 1995, "tt0113277");
    (heat.tmdb_id, heat.path) = (Some(949), w.root.join("Movies/Heat (1995)").to_string_lossy().to_string());
    w.app.db.save_title(&mut heat).unwrap();
    lay(w.root.join("Movies/Heat (1995)/Heat.1995.1080p.BluRay.x264-GRP.mkv"));
    let (mut show, _) = w.series("Example World", 296762, &[(1, 1, "The Original"), (1, 2, "Chestnut")]);
    show.path = w.root.join("TV Shows/Example World").to_string_lossy().to_string();
    w.app.db.save_title(&mut show).unwrap();
    lay(w.root.join("TV Shows/Example World/Season 01/Example.World.S01E01.1080p.BluRay.x264-GRP.mkv"));
    w.app.scan_all().await.unwrap();
    assert_eq!((w.app.db.files(heat.id).unwrap().len(), w.app.db.files(show.id).unwrap().len()), (1, 1));

    // Elsewhere: another copy of the film, and both episodes of the series.
    let (there, away) = (w.root.join("Elsewhere"), w.root.join("Elsewhere.away"));
    lay(there.join("films/Heat (1995)/Heat.1995.2160p.UHD.BluRay.x265-GRP.mkv"));
    lay(there.join("tv/Example World/Season 01/Example.World.S01E01.720p.HDTV.x264-GRP.mkv"));
    lay(there.join("tv/Example World/Season 01/Example.World.S01E02.720p.HDTV.x264-GRP.mkv"));
    let mut g = serde_json::to_value(w.app.settings.general()).unwrap();
    g["tmdb_api_key"] = json!("key");
    g["locations"] = json!([{"name": "Elsewhere", "movie_root": there.join("films"), "series_root": there.join("tv"), "manage": false}]);
    assert_eq!(call(&api, "PUT", "/api/settings/general", g).await.0, 200);
    let finished = |api: axum::Router| async move {
        for _ in 0..200 {
            let (_, f) = call(&api, "GET", "/api/locations/1/found", Value::Null).await;
            if f["running"] == false {
                return f;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the location was never finished with");
    };
    call(&api, "POST", "/api/locations/1/look", Value::Null).await;
    let found = finished(api.clone()).await;
    let notes: Vec<(String, String)> = found["found"].as_array().unwrap().iter().map(|f| (f["state"].as_str().unwrap().to_string(), f["note"].as_str().unwrap().to_string())).collect();
    assert!(notes.iter().all(|(state, note)| state == "new" && note.contains("kept as another copy")), "{notes:?}");
    let before = tree(&there);
    assert_eq!(call(&api, "POST", "/api/locations/1/add", json!({})).await.0, 200);
    finished(api.clone()).await;
    assert_eq!(tree(&there), before, "nothing on the location was touched");
    assert_eq!(w.app.db.titles(None).unwrap().len(), 2, "the same two titles, not copies of them");

    // The film has two files: its own, and the copy as a version beside it.
    let films = w.app.db.files(heat.id).unwrap();
    let copy = films.iter().find(|f| f.base.is_some()).expect("the copy");
    assert_eq!((films.len(), copy.extra, copy.base.as_deref()), (2, true, there.join("films/Heat (1995)").to_str()));
    assert!(films.iter().any(|f| f.base.is_none() && !f.extra));
    // The series takes from the copy only the episode it lacked.
    let episodes = w.app.db.episodes(show.id).unwrap();
    let files = w.app.db.files(show.id).unwrap();
    let file_of = |n: u32| files.iter().find(|f| Some(f.id) == episodes.iter().find(|e| e.episode == n).unwrap().file_id).unwrap();
    assert_eq!((files.len(), file_of(1).base.is_none(), file_of(2).base.is_some()), (2, true, true));

    // A player is offered both, the copy named for its place, and can play the copy.
    let mut g = w.app.settings.general();
    g.players_enabled = true;
    w.app.settings.set_general(&g).unwrap();
    let jf = spool::jellyfin::router(w.app.clone());
    let (_, _, body) = player(&jf, "POST", "/Users/AuthenticateByName", None, json!({"Username": "spool", "Pw": ""})).await;
    let token = serde_json::from_slice::<Value>(&body).unwrap()["AccessToken"].as_str().unwrap().to_string();
    let (_, _, body) = player(&jf, "GET", "/Items?Recursive=true&IncludeItemTypes=Movie&Fields=MediaSources", Some(&token), Value::Null).await;
    let item = serde_json::from_slice::<Value>(&body).unwrap()["Items"][0].clone();
    let sources = item["MediaSources"].as_array().unwrap().clone();
    let second = sources.iter().find(|s| s["Name"].as_str().unwrap().ends_with("· Elsewhere")).unwrap_or_else(|| panic!("{sources:?}"));
    assert_eq!(sources.len(), 2);
    let (status, _, played) = player(&jf, "GET", &format!("/Videos/{}/stream?MediaSourceId={}&Static=true", item["Id"].as_str().unwrap(), second["Id"].as_str().unwrap()), Some(&token), Value::Null).await;
    assert_eq!((status, played.as_slice()), (200, &b"not really video"[..]));

    // A copy is played and never changed.
    assert!(w.app.delete_file(copy.id).is_err());
    let (_, page) = call(&api, "GET", &format!("/api/titles/{}", heat.id), Value::Null).await;
    let shown = page["files"].as_array().unwrap().iter().find(|f| f["base"].is_string()).unwrap().clone();
    assert_eq!((shown["copy_on"]["name"].as_str(), shown["copy_on"]["online"].as_bool()), (Some("Elsewhere"), Some(true)));
    assert_eq!(call(&api, "DELETE", &format!("/api/titles/{}?delete_files=true", show.id), Value::Null).await.0, 200, "removing a title deletes its own files only");
    assert!(there.join("tv/Example World/Season 01/Example.World.S01E02.720p.HDTV.x264-GRP.mkv").exists());

    // The other place goes away: the copy is kept on record, and the film is still here.
    std::fs::rename(&there, &away).unwrap();
    w.app.watch_locations();
    w.app.scan_all().await.unwrap();
    assert_eq!(w.app.db.files(heat.id).unwrap().len(), 2, "a file that cannot be seen is not a file that has gone");
    let heat_now = w.app.db.title(heat.id).unwrap().unwrap();
    assert!(w.app.placement().playable(&heat_now) && w.app.title_online(&heat_now));
    // And when it is the main library that is away, the copy alone keeps the film playable.
    std::fs::rename(&away, &there).unwrap();
    w.app.watch_locations();
    assert!(w.app.placement().online_at(copy.base.as_deref().unwrap()));
}

#[tokio::test(flavor = "multi_thread")]
async fn what_lies_beside_a_film_is_recognised_shown_and_left_alone_by_an_upgrade() {
    use serde_json::{json, Value};
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let api = spool::api::router(w.app.clone());
    let mut movie = w.movie("Heat", 1995, "tt0113277");
    let folder = w.root.join("Movies/Heat (1995)");
    movie.path = folder.to_string_lossy().to_string();
    w.app.db.save_title(&mut movie).unwrap();

    // A folder as someone who used Plex or Jellyfin for years might have left it.
    let lay = |rel: &str, bytes: &[u8]| {
        let p = folder.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    };
    let mut poster = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut poster, 90).encode_image(&image::RgbImage::from_pixel(200, 300, image::Rgb([180, 20, 20]))).unwrap();
    lay("Heat (1995) HDTV-1080p.mkv", &[7u8; 4000]);
    lay("Heat (1995) - Directors Cut HDTV-720p.mkv", &[7u8; 1000]);
    lay("Heat (1995)-trailer.mkv", b"the trailer");
    lay("Featurettes/Making Of.mkv", b"how it was made");
    lay("heat.sample.mkv", b"a sample");
    lay("poster.jpg", &poster);
    lay("Heat (1995) HDTV-1080p-fanart.jpg", &poster);
    lay("Heat (1995) HDTV-1080p.nfo", b"<movie><title>Heat</title><uniqueid type=\"tmdb\">949</uniqueid></movie>");
    lay("theme.mp3", b"music");
    lay("holiday.jpg", &poster);
    // An earlier scan had taken the trailer for the film.
    let wrong: MediaFile = serde_json::from_value(json!({"title_id": movie.id, "rel_path": "Heat (1995)-trailer.mkv", "size": 11, "quality": {"quality": "unknown"}})).unwrap();
    w.app.db.with(|c| c.execute("INSERT INTO files(title_id, rel_path, size, data) VALUES (?1, ?2, 11, ?3)", rusqlite::params![movie.id, wrong.rel_path, serde_json::to_string(&wrong).unwrap()])).unwrap();

    let mut report = spool::catalog::ScanReport::default();
    w.app.scan_title(movie.id, &mut report).await.unwrap();
    assert_eq!((report.files_found, report.files_added, report.extras, report.files_missing), (2, 2, 1, 0));
    // The film is the larger file; the other cut is a version kept beside it.
    let files = w.app.db.files(movie.id).unwrap();
    assert_eq!(files.iter().map(|f| (f.rel_path.as_str(), f.extra)).collect::<Vec<_>>(), [("Heat (1995) - Directors Cut HDTV-720p.mkv", true), ("Heat (1995) HDTV-1080p.mkv", false)]);
    assert!(folder.join("Heat (1995)-trailer.mkv").exists(), "putting the record right touches no file");
    let local = w.app.db.title(movie.id).unwrap().unwrap().local;
    assert_eq!(local.extras.iter().map(|e| (format!("{:?}", e.kind), e.name.as_str())).collect::<Vec<_>>(), [("Featurette".into(), "Making Of"), ("Trailer".into(), "Heat (1995)"), ("Sample".into(), "heat")]);
    assert_eq!(local.art.iter().map(|a| (a.rel_path.as_str(), format!("{:?}", a.kind))).collect::<Vec<_>>(), [("Heat (1995) HDTV-1080p-fanart.jpg", "Backdrop".into()), ("poster.jpg", "Poster".into())]);
    assert_eq!((local.notes.tmdb_id, local.themes.as_slice()), (Some(949), ["theme.mp3".to_string()].as_slice()));

    // The app says what is there, and shows the folder's own poster.
    let (_, detail) = call(&api, "GET", &format!("/api/titles/{}", movie.id), Value::Null).await;
    assert_eq!(detail["local"]["extras"].as_array().map(Vec::len), Some(3));
    let (_, list) = call(&api, "GET", "/api/titles", Value::Null).await;
    assert_eq!((list[0]["trailers"].as_u64(), list[0]["extras"].as_u64(), list[0]["own_poster"].as_bool(), list[0].get("local")), (Some(1), Some(1), Some(true), None));
    let shown = w.app.art(movie.id, spool::art::Art::Poster).await.unwrap();
    assert_eq!(image::load_from_memory(&shown).unwrap().width(), 200, "the picture from the folder, not one fetched");
    assert_eq!(std::fs::read(folder.join("poster.jpg")).unwrap(), poster, "which is itself left as it was");

    // A player is offered the trailer and the featurette, and can play them.
    let mut g = w.app.settings.general();
    g.players_enabled = true;
    w.app.settings.set_general(&g).unwrap();
    let jf = spool::jellyfin::router(w.app.clone());
    let (_, _, body) = player(&jf, "POST", "/Users/AuthenticateByName", None, json!({"Username": "spool", "Pw": ""})).await;
    let token = serde_json::from_slice::<Value>(&body).unwrap()["AccessToken"].as_str().unwrap().to_string();
    let get = |jf: axum::Router, token: String, uri: String| async move {
        let (status, _, body) = player(&jf, "GET", &uri, Some(&token), Value::Null).await;
        (status, body)
    };
    let (_, body) = get(jf.clone(), token.clone(), "/Items?Recursive=true&IncludeItemTypes=Movie".into()).await;
    let item = serde_json::from_slice::<Value>(&body).unwrap()["Items"][0].clone();
    assert_eq!((item["LocalTrailerCount"].as_u64(), item["SpecialFeatureCount"].as_u64()), (Some(1), Some(1)));
    let id = item["Id"].as_str().unwrap().to_string();
    // The Movies view itself is given a picture, made of the posters of what is in it.
    let (_, body) = get(jf.clone(), token.clone(), "/UserViews".into()).await;
    let views = serde_json::from_slice::<Value>(&body).unwrap();
    let movies = views["Items"].as_array().unwrap().iter().find(|v| v["CollectionType"] == "movies").expect("the Movies view").clone();
    assert!(movies["ImageTags"]["Primary"].is_string(), "{movies}");
    let (status, picture) = get(jf.clone(), token.clone(), format!("/Items/{}/Images/Primary", movies["Id"].as_str().unwrap())).await;
    let seen = image::load_from_memory(&picture).expect("a picture");
    assert_eq!((status, seen.width(), seen.height()), (200, 1280, 720));

    let (_, body) = get(jf.clone(), token.clone(), format!("/Items/{id}/LocalTrailers")).await;
    let trailers = serde_json::from_slice::<Value>(&body).unwrap();
    assert_eq!((trailers.as_array().map(Vec::len), trailers[0]["Type"].as_str(), trailers[0]["ExtraType"].as_str()), (Some(1), Some("Trailer"), Some("Trailer")));
    let (status, body) = get(jf.clone(), token.clone(), format!("/Videos/{}/stream?Static=true", trailers[0]["Id"].as_str().unwrap())).await;
    assert_eq!((status, body.as_slice()), (200, b"the trailer".as_slice()));
    let (_, body) = get(jf.clone(), token.clone(), format!("/Items/{id}/SpecialFeatures")).await;
    let features = serde_json::from_slice::<Value>(&body).unwrap();
    assert_eq!(features.as_array().unwrap().iter().map(|f| (f["Name"].as_str().unwrap(), f["ExtraType"].as_str().unwrap())).collect::<Vec<_>>(), [("Making Of", "Featurette")]);
    let (status, body) = get(jf.clone(), token.clone(), format!("/Items/{}", features[0]["Id"].as_str().unwrap())).await;
    assert_eq!((status, serde_json::from_slice::<Value>(&body).unwrap()["MediaSources"][0]["Container"].as_str().map(str::to_string)), (200, Some("mkv".into())));

    // A better copy arrives. It takes the place of the film and of nothing else.
    w.release("Heat.1995.1080p.BluRay.x264-GRP", &[("heat.mkv", &video)]);
    let outcome = w.app.search(movie.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "{}", outcome.message);
    w.wait_state(movie.id, AcqState::Imported).await;
    let new_stem = "Heat (1995) {imdb-tt0113277} [Bluray-1080p][AAC 2.0][x264]-GRP";
    let there = tree(&folder);
    for kept in ["Heat (1995) - Directors Cut HDTV-720p.mkv", "Heat (1995)-trailer.mkv", "Featurettes/Making Of.mkv", "heat.sample.mkv", "poster.jpg", "theme.mp3", "holiday.jpg"] {
        assert!(there.iter().any(|f| f == kept), "{kept} is still there: {there:?}");
    }
    assert!(there.contains(&format!("{new_stem}.mkv")) && !there.iter().any(|f| f == "Heat (1995) HDTV-1080p.mkv"), "{there:?}");
    // What a media server named after the old file now carries the new file's name.
    assert!(there.contains(&format!("{new_stem}.nfo")) && there.contains(&format!("{new_stem}-fanart.jpg")), "{there:?}");
    let files = w.app.db.files(movie.id).unwrap();
    assert_eq!(files.iter().map(|f| f.extra).collect::<Vec<_>>(), [true, false]);
    let local = w.app.db.title(movie.id).unwrap().unwrap().local;
    assert_eq!((local.extras.len(), local.notes.rel_path.as_str(), local.notes.tmdb_id), (3, format!("{new_stem}.nfo").as_str(), Some(949)));
}

/// A stand-in for a NAS: a folder the helper sees as `nas`, which Spool sees as `mnt`, as it
/// would a share. The helper files into `films`, only reads `vault`, and downloads into its own
/// folder. Spool is told of two locations there and of the helper.
async fn with_helper(w: &World) -> (spool::helper::Helper, String, PathBuf, PathBuf) {
    use serde_json::json;
    let (nas, mnt) = (w.root.join("nas"), w.root.join("mnt"));
    for d in ["films", "vault", "downloads/complete", "downloads/incomplete"] {
        std::fs::create_dir_all(nas.join(d)).unwrap();
    }
    std::os::unix::fs::symlink(&nas, &mnt).unwrap();
    let key = "a-key-of-sufficient-length".to_string();
    let conf = spool::helper::Conf { key: key.clone(), roots: vec![nas.join("films")], read_roots: vec![nas.join("vault")], downloads: nas.join("downloads"), code: None, grants: spool::link::Grants::ALL }.checked().unwrap();
    let helper = spool::helper::Helper::start(&w.root.join("helper-data"), conf).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = helper.router();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let api = spool::api::router(w.app.clone());
    let mut g = serde_json::to_value(w.app.settings.general()).unwrap();
    g["locations"] = json!([{"name": "Shelf", "movie_root": mnt.join("films"), "manage": true}, {"name": "Vault", "movie_root": mnt.join("vault"), "manage": false}]);
    g["links"] = json!([{"name": "Nas", "url": url, "key": key, "folders": [{"here": mnt, "there": nas}]}]);
    let (status, said) = call(&api, "PUT", "/api/settings/general", g).await;
    assert_eq!(status, 200, "{said}");
    w.app.ask_remotes().await;
    (helper, url, nas, mnt)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_helper_downloads_and_files_on_its_own_disks() {
    use serde_json::{json, Value};
    let video = video_or_skip!();
    let mut w = World::new(Mode::Active).await;
    let (helper, _, nas, mnt) = with_helper(&w).await;
    let api = spool::api::router(w.app.clone());

    // Spool knows the helper, what it serves, and that it can read what the helper downloads.
    assert_eq!(call(&api, "GET", "/api/helpers", Value::Null).await.0, 404, "nothing answers to a name that is not one");
    let (_, helpers) = call(&api, "GET", "/api/links", Value::Null).await;
    assert_eq!((helpers[0]["name"].as_str(), helpers[0]["serves"].to_string().as_str(), helpers[0]["seen"]["usable"].as_bool(), helpers[0]["seen"]["downloads_seen"].as_bool()), (Some("Nas"), r#"["Shelf","Vault"]"#, Some(true), Some(true)), "{helpers}");
    // Its key is not handed back to the browser.
    let (_, general) = call(&api, "GET", "/api/settings/general", Value::Null).await;
    assert_eq!((general["links"][0]["key"].as_str(), general["links"][0]["id"].as_u64(), general["link_seq"].as_u64()), (Some("********"), Some(1), Some(1)), "{}", general["links"]);
    assert!(general.get("helpers").is_none(), "and is kept under the name it has now");
    // And the helper was given the Usenet servers to download with.
    let saved: Value = serde_json::from_slice(&std::fs::read(w.root.join("helper-data/helper.json")).unwrap()).unwrap();
    assert_eq!(saved["servers"].as_array().map(Vec::len), Some(1));

    // A film that lives on the helper's disks is downloaded by the helper, not here.
    let mut film = w.movie("Night of the Example", 2020, "tt1234567");
    film.path = mnt.join("films/Night of the Example (2020)").to_string_lossy().to_string();
    w.app.db.save_title(&mut film).unwrap();
    w.release("Night.of.the.Example.2020.1080p.BluRay.x264-GRP", &[("night.of.the.example.2020.1080p.bluray.x264-grp.mkv", &video), ("night.nfo", b"info")]);
    let outcome = w.app.search(film.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "{}", outcome.message);
    let job = outcome.grabbed[0].job_id.clone().unwrap();
    assert!(job.starts_with("@1:"), "the download is the helper's: {job}");
    assert!(w.app.engine.jobs().is_empty(), "nothing was started on this machine");
    assert_eq!(helper.engine().jobs().len(), 1);

    // It arrives in the library on the helper's disks, filed there by the helper.
    for _ in 0..600 {
        w.app.ask_remotes().await;
        if w.app.db.title_acquisitions(film.id).unwrap().iter().any(|a| a.state == AcqState::Imported) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    w.wait_state(film.id, AcqState::Imported).await;
    assert_eq!(tree(&nas.join("films")), ["Night of the Example (2020)/Night of the Example (2020) {imdb-tt1234567} [Bluray-1080p][AAC 2.0][x264]-GRP.mkv"]);
    let files = w.app.db.files(film.id).unwrap();
    assert_eq!((files.len(), files[0].size), (1, video.len() as u64));
    assert_eq!(helper.work_done().0, 1, "the helper moved the file into place itself");
    for _ in 0..60 {
        if tree(&nas.join("downloads")).is_empty() && helper.engine().jobs().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(tree(&nas.join("downloads")).is_empty() && helper.engine().jobs().is_empty(), "the helper cleared the download away: {:?}", tree(&nas.join("downloads")));
    assert!(tree(&w.root.join("downloads")).is_empty(), "and nothing passed through this machine's downloads folder");

    // The check-up of a location says what its helper sees.
    let (_, checked) = call(&api, "POST", "/api/locations/1/check", Value::Null).await;
    let line = checked.to_string();
    assert!(line.contains("Nas's helper") && line.contains("sees the same folder"), "{line}");

    // With the helper away, what it was doing is waited for, and new downloads run here.
    let mut g = serde_json::to_value(w.app.settings.general()).unwrap();
    g["links"][0]["url"] = json!("http://127.0.0.1:9");
    assert_eq!(call(&api, "PUT", "/api/settings/general", g).await.0, 200);
    w.app.ask_remotes().await;
    let (_, helpers) = call(&api, "GET", "/api/links", Value::Null).await;
    assert_eq!(helpers[0]["seen"]["reachable"].as_bool(), Some(false), "{helpers}");
    assert!(w.app.db.attention(false).unwrap().iter().any(|a| a.message.contains("Nas's helper did not answer")), "a notice says the helper is away");
    let mut waiting = w.app.db.title_acquisitions(film.id).unwrap().remove(0);
    (waiting.id, waiting.state, waiting.job_id) = (0, AcqState::Downloading, Some("@1:still-running".into()));
    w.app.db.save_acquisition(&mut waiting).unwrap();
    w.app.reconcile_all().await.unwrap();
    assert_eq!(w.app.db.acquisition(waiting.id).unwrap().unwrap().state, AcqState::Downloading, "a download on a helper that cannot be asked is not written off");

    let mut second = w.movie("Second Example", 2021, "tt7654321");
    second.path = mnt.join("films/Second Example (2021)").to_string_lossy().to_string();
    w.app.db.save_title(&mut second).unwrap();
    w.release("Second.Example.2021.1080p.BluRay.x264-GRP", &[("second.example.2021.1080p.bluray.x264-grp.mkv", &video)]);
    let outcome = w.app.search(second.id, Scope::Movie, false, true).await.unwrap();
    assert_eq!(outcome.grabbed.len(), 1, "{}", outcome.message);
    assert!(!outcome.grabbed[0].job_id.clone().unwrap().starts_with('@'), "it runs here while the helper is away");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_helper_moves_a_title_between_its_own_folders_and_touches_nothing_else() {
    use serde_json::{json, Value};
    let w = World::new(Mode::Active).await;
    let (helper, url, nas, mnt) = with_helper(&w).await;
    let api = spool::api::router(w.app.clone());
    let lay = |path: PathBuf, bytes: &[u8]| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    };

    // A film on the folder the helper only reads, moved to the one it files into: the helper
    // copies it there, and the original stays.
    let kept = nas.join("vault/Alpha (2001)");
    lay(kept.join("Alpha (2001).mkv"), b"a film on the vault");
    lay(kept.join("Alpha (2001).en.srt"), b"1\n");
    let mut alpha = w.movie("Alpha", 2001, "tt0000001");
    alpha.path = mnt.join("vault/Alpha (2001)").to_string_lossy().to_string();
    w.app.db.save_title(&mut alpha).unwrap();
    w.app.scan_all().await.unwrap();
    let (status, said) = call(&api, "POST", "/api/titles/move", json!({"ids": [alpha.id], "location": 1})).await;
    assert_eq!((status, said["queued"].as_u64()), (200, Some(1)), "{said}");
    for _ in 0..400 {
        if !w.app.moving(alpha.id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!w.app.moving(alpha.id), "the move finished");
    assert_eq!(tree(&nas.join("films")), ["Alpha (2001)/Alpha (2001).en.srt", "Alpha (2001)/Alpha (2001).mkv"], "{:?}", w.app.db.attention(false).unwrap().iter().map(|a| a.message.clone()).collect::<Vec<_>>());
    assert_eq!(std::fs::read(kept.join("Alpha (2001).mkv")).unwrap(), b"a film on the vault", "nothing is taken from a folder the helper only reads");
    assert_eq!(helper.work_done().1, 1, "the helper did the copying");
    let now = w.app.db.title(alpha.id).unwrap().unwrap();
    assert_eq!((now.path.as_str(), now.also.len()), (mnt.join("films/Alpha (2001)").to_str().unwrap(), 1));

    // The helper answers nobody without its key, and works only inside what it was given.
    let http = reqwest::Client::new();
    let ask = |key: &'static str, path: &'static str, body: Value| {
        let (http, url) = (http.clone(), url.clone());
        async move {
            let resp = http.post(format!("{url}{path}")).bearer_auth(key).json(&body).send().await.unwrap();
            (resp.status().as_u16(), resp.text().await.unwrap())
        }
    };
    const KEY: &str = "a-key-of-sufficient-length";
    assert_eq!(http.get(format!("{url}/link/work/v1/info")).send().await.unwrap().status().as_u16(), 401);
    assert_eq!(ask("not-the-key", "/link/work/v1/fs/stat", json!({"path": nas.join("films/Alpha (2001)")})).await.0, 401);
    let outside = w.root.join("Movies/Elsewhere (1999)");
    lay(outside.join("Elsewhere (1999).mkv"), b"not the helper's");
    lay(nas.join("films/Neighbour (1990)/Neighbour (1990).mkv"), b"another film");
    lay(nas.join("films/.spool-moving-7/Odd (1980).mkv"), b"a film in an oddly named folder");
    let film = nas.join("films/Alpha (2001)/Alpha (2001).mkv");
    std::os::unix::fs::symlink(&outside, nas.join("films/Escape")).unwrap();
    for (path, body, status, why) in [
        // Out of its folders, by either end.
        ("/link/work/v1/fs/move", json!({"from": film, "to": outside.join("taken.mkv")}), 403, "outside the folders"),
        ("/link/work/v1/fs/move", json!({"from": outside.join("Elsewhere (1999).mkv"), "to": nas.join("films/taken.mkv")}), 403, "outside the folders"),
        // Out of them by climbing.
        ("/link/work/v1/fs/move", json!({"from": film, "to": format!("{}/films/../../Movies/taken.mkv", nas.display())}), 400, "not a plain full path"),
        // Out of them through a link that is inside them by name, either way.
        ("/link/work/v1/fs/move", json!({"from": film, "to": nas.join("films/Escape/taken.mkv")}), 409, "is a link"),
        ("/link/work/v1/fs/move", json!({"from": nas.join("films/Escape/Elsewhere (1999).mkv"), "to": nas.join("films/taken.mkv")}), 409, "is a link"),
        // Out of the folder it only reads: nothing is moved away from it or removed.
        ("/link/work/v1/fs/move", json!({"from": kept.join("Alpha (2001).mkv"), "to": nas.join("films/taken.mkv")}), 403, "only reads"),
        ("/link/work/v1/moves", json!({"from": kept, "to": nas.join("films/Taken (2001)"), "working": nas.join("films/.spool-moving-9"), "keep_original": false}), 403, "only reads"),
        ("/link/work/v1/moves", json!({"from": nas.join("films/Alpha (2001)"), "to": nas.join("vault/Taken (2001)"), "working": nas.join("vault/.spool-moving-9"), "keep_original": true}), 403, "only reads"),
        // The working folder is cleared before use, so it is never one that holds anything:
        // not the title itself, and not its neighbour.
        ("/link/work/v1/moves", json!({"from": nas.join("films/Alpha (2001)"), "to": nas.join("films/Beta (2002)"), "working": nas.join("films/Alpha (2001)"), "keep_original": false}), 400, ".spool-moving-"),
        ("/link/work/v1/moves", json!({"from": nas.join("films/Alpha (2001)"), "to": nas.join("films/Beta (2002)"), "working": nas.join("films/Neighbour (1990)"), "keep_original": true}), 400, ".spool-moving-"),
        ("/link/work/v1/moves", json!({"from": nas.join("films/Alpha (2001)"), "to": nas.join("films/Alpha (2001)/Inside"), "working": nas.join("films/Alpha (2001)/.spool-moving-9"), "keep_original": true}), 400, "into itself"),
        // However the title's own folder is named.
        ("/link/work/v1/moves", json!({"from": nas.join("films/.spool-moving-7"), "to": nas.join("films/Beta (2002)"), "working": nas.join("films/.spool-moving-7"), "keep_original": true}), 400, "the title's own folder"),
        ("/link/work/v1/moves", json!({"from": nas.join("films"), "to": nas.join("films/Beta (2002)"), "working": nas.join("films/.spool-moving-7"), "keep_original": true}), 403, "outside the folders"),
        // Only a finished download is ever cleared away, never a library folder or a whole root.
        ("/link/work/v1/fs/remove", json!({"path": nas.join("films/Alpha (2001)")}), 403, "not a finished download"),
        ("/link/work/v1/fs/remove", json!({"path": nas.join("downloads/complete")}), 403, "not a finished download"),
        ("/link/work/v1/fs/remove", json!({"path": nas.join("films")}), 403, "outside the folders"),
    ] {
        let (got, said) = ask(KEY, path, body.clone()).await;
        assert!(got == status && said.contains(why), "{path} {body}: {got} {said}");
    }
    assert!(outside.join("Elsewhere (1999).mkv").exists() && film.exists() && kept.join("Alpha (2001).mkv").exists() && nas.join("films/Neighbour (1990)/Neighbour (1990).mkv").exists() && nas.join("films/.spool-moving-7/Odd (1980).mkv").exists(), "and nothing was touched by any of it");
    assert_eq!(ask(KEY, "/link/work/v1/fs/stat", json!({"path": film})).await.1.contains(r#""exists":true"#), true);
    std::fs::remove_file(nas.join("films/Escape")).unwrap();

    // Off the helper's disks altogether, a move is Spool's own to carry out, as before.
    let (status, said) = call(&api, "POST", "/api/titles/move", json!({"ids": [alpha.id], "location": 0})).await;
    assert_eq!((status, said["queued"].as_u64()), (200, Some(1)), "{said}");
    for _ in 0..400 {
        if !w.app.moving(alpha.id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(w.root.join("Movies/Alpha (2001)/Alpha (2001).mkv").exists() && !nas.join("films/Alpha (2001)").exists());
    assert_eq!(helper.work_done().1, 1, "the helper had no part in that one");
}

/// A request to a server on a real socket, which is how a player's address is known.
async fn fetch(http: &reqwest::Client, method: &str, url: &str, headers: &[(&str, &str)]) -> (u16, reqwest::header::HeaderMap, Vec<u8>) {
    let mut req = http.request(method.parse().unwrap(), url);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let resp = req.send().await.unwrap();
    let (status, head) = (resp.status().as_u16(), resp.headers().clone());
    (status, head, resp.bytes().await.unwrap().to_vec())
}

/// A helper started without a key is linked with a short code. Until then it does nothing
/// for anybody; a wrong code agrees nothing; and it takes only so many.
#[tokio::test(flavor = "multi_thread")]
async fn a_helper_is_linked_with_a_code_and_not_without() {
    use serde_json::{json, Value};
    let w = World::new(Mode::Active).await;
    let api = spool::api::router(w.app.clone());
    let nas = w.root.join("nas");
    for d in ["films", "downloads/complete"] {
        std::fs::create_dir_all(nas.join(d)).unwrap();
    }
    let start = |data: &'static str, code: &'static str| {
        let (nas, root) = (nas.clone(), w.root.clone());
        async move {
            let conf = spool::helper::Conf { key: String::new(), roots: vec![nas.join("films")], read_roots: vec![], downloads: nas.join("downloads"), code: Some(code.into()), grants: spool::link::Grants::ALL }.checked().unwrap();
            let helper = spool::helper::Helper::start(&root.join(data), conf).await.unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let router = helper.router();
            let serving = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            (helper, url, serving)
        }
    };
    let http = reqwest::Client::new();
    let pair = |url: String, code: &'static str| {
        let api = api.clone();
        async move { call(&api, "POST", "/api/links/pair", json!({"url": url, "code": code})).await }
    };

    // Not linked: it has no key, so nothing is the key to it, and it hands out no files.
    let (_helper, url, serving) = start("helper-a", "1234 5678").await;
    for key in ["", "anything-at-all-that-is-long"] {
        assert_eq!(http.get(format!("{url}/link")).bearer_auth(key).send().await.unwrap().status().as_u16(), 401);
        assert_eq!(http.get(format!("{url}/link/work/v1/info")).bearer_auth(key).send().await.unwrap().status().as_u16(), 401);
    }
    assert_eq!(http.get(format!("{url}/link/play/v1/file?ticket=x")).send().await.unwrap().status().as_u16(), 403);

    // A wrong code agrees nothing, and is told so.
    let (status, said) = pair(url.clone(), "1234 5679").await;
    assert_eq!((status, said["error"].as_str()), (400, Some("the code is not the one it gave")), "{said}");
    assert_eq!(pair(url.clone(), "123").await.0, 400);
    assert_eq!(http.get(format!("{url}/link")).bearer_auth("").send().await.unwrap().status().as_u16(), 401, "still not linked");

    // The right code, however it is written: the two agree a key nobody copied.
    let (status, said) = pair(url.clone(), "1234-5678").await;
    assert_eq!(status, 200, "{said}");
    let key = said["key"].as_str().unwrap().to_string();
    assert!(key.len() >= 40, "{key}");
    let hello: Value = http.get(format!("{url}/link")).bearer_auth(&key).send().await.unwrap().json().await.unwrap();
    assert_eq!(hello["grants"]["work"].as_bool(), Some(true));
    // Linked, it takes no more codes, not even the right one.
    let (status, said) = pair(url.clone(), "1234 5678").await;
    assert!(status == 400 && said["error"].as_str().unwrap().contains("linked already"), "{said}");

    // Spool uses the key as it would one that was typed in.
    let mut g = serde_json::to_value(w.app.settings.general()).unwrap();
    g["links"] = json!([{"name": "Nas", "url": url, "key": key, "folders": [{"here": nas, "there": nas}]}]);
    assert_eq!(call(&api, "PUT", "/api/settings/general", g).await.0, 200);
    w.app.ask_remotes().await;
    assert!(w.app.remotes()[0].seen().usable, "{:?}", w.app.remotes()[0].seen());

    // Started again, it is still linked, with the same key, and gives no new code.
    serving.abort();
    let (_helper, url, _serving) = start("helper-a", "1234 5678").await;
    assert_eq!(http.get(format!("{url}/link")).bearer_auth(&key).send().await.unwrap().status().as_u16(), 200);
    assert_eq!(http.get(format!("{url}/link")).bearer_auth("another-key-of-the-same-length-as-it-0000000").send().await.unwrap().status().as_u16(), 401);

    // Another helper: after five wrong codes it takes none for a while, the right one
    // included, and starting it again with the same code does not give five more.
    let (_helper, url, serving) = start("helper-b", "8765 4321").await;
    for _ in 0..5 {
        assert_eq!(pair(url.clone(), "0000 0000").await.1["error"].as_str(), Some("the code is not the one it gave"));
    }
    let (status, said) = pair(url.clone(), "8765 4321").await;
    assert!(status == 400 && said["error"].as_str().unwrap().contains("it takes none for another 10 minutes"), "{said}");
    serving.abort();
    let (_helper, url, serving) = start("helper-b", "8765 4321").await;
    assert!(pair(url.clone(), "8765 4321").await.1["error"].as_str().unwrap().contains("too many wrong codes"));
    serving.abort();
    // The while passes, and the right code is taken again: nobody on the network can
    // shut the owner out for good by guessing.
    let kept = w.root.join("helper-b/tries.json");
    let mut tries: Value = serde_json::from_slice(&std::fs::read(&kept).unwrap()).unwrap();
    assert_eq!((tries["failures"].as_u64(), tries["rests"].as_u64()), (Some(0), Some(1)), "{tries}");
    tries["until"] = json!(1);
    std::fs::write(&kept, tries.to_string()).unwrap();
    let (_helper, url, serving) = start("helper-b", "8765 4321").await;
    assert_eq!(pair(url.clone(), "0000 0000").await.1["error"].as_str(), Some("the code is not the one it gave"));
    serving.abort();
    // A new code is a new start.
    let (_helper, url, _serving) = start("helper-b", "1111 2222").await;
    assert_eq!(pair(url, "1111 2222").await.0, 200);
}

/// A helper asked with the mark of the report the asker already has keeps it waiting until
/// something changes, and then answers at once.
#[tokio::test(flavor = "multi_thread")]
async fn a_helper_answers_when_it_has_news_and_not_before() {
    use serde_json::Value;
    use std::time::{Duration, Instant};
    let w = World::new(Mode::Active).await;
    let (_helper, url, _, _) = with_helper(&w).await;
    const KEY: &str = "a-key-of-sufficient-length";
    let http = reqwest::Client::new();
    let ask = |query: String| {
        let (http, url) = (http.clone(), url.clone());
        async move { http.get(format!("{url}/link/work/v1/jobs{query}")).bearer_auth(KEY).send().await.unwrap().json::<Value>().await.unwrap() }
    };
    let first = ask(String::new()).await;
    assert_eq!(ask("?wait=20".into()).await["stamp"], first["stamp"], "asked without a mark, it answers at once");

    // Nothing changes: it answers when the wait is up, with the same report.
    // (Room on the disk is part of the report, and the other tests are using the disk: a
    // report that did change is not this case, so it is tried until one does not.)
    let mut held = false;
    for _ in 0..5 {
        let stamp = ask(String::new()).await["stamp"].as_str().unwrap().to_string();
        let began = Instant::now();
        if ask(format!("?since={stamp}&wait=2")).await["stamp"].as_str() == Some(stamp.as_str()) {
            assert!(began.elapsed() >= Duration::from_millis(1800), "{:?}", began.elapsed());
            held = true;
            break;
        }
    }
    assert!(held, "it never once waited");
    let first = ask(String::new()).await;
    let stamp = first["stamp"].as_str().unwrap().to_string();

    // Something changes while it waits: it answers then, not when the wait is up.
    let began = Instant::now();
    let waiting = tokio::spawn(ask(format!("?since={stamp}&wait=20")));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(http.post(format!("{url}/link/work/v1/queue/pause")).bearer_auth(KEY).send().await.unwrap().status().as_u16(), 200);
    let news = waiting.await.unwrap();
    assert!(began.elapsed() < Duration::from_secs(5), "{:?}", began.elapsed());
    assert_eq!((news["paused"].as_bool(), news["stamp"] != first["stamp"]), (Some(true), true), "{news}");
    // A mark it does not know, as after a restart, is answered at once.
    let began = Instant::now();
    ask("?since=0000000000000000&wait=20".into()).await;
    assert!(began.elapsed() < Duration::from_secs(2));

    // Spool, keeping in step, hears of the change without being told to ask.
    w.app.spawn_remotes();
    for _ in 0..50 {
        if w.app.remotes()[0].seen().paused {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(w.app.remotes()[0].seen().paused);
    assert_eq!(http.post(format!("{url}/link/work/v1/queue/resume")).bearer_auth(KEY).send().await.unwrap().status().as_u16(), 200);
    let began = Instant::now();
    while w.app.remotes()[0].seen().paused && began.elapsed() < Duration::from_secs(6) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!w.app.remotes()[0].seen().paused, "heard within a moment, while a request was waiting on the helper");
}

/// The linked machine reads its own folders and says what is in them. What it says is taken
/// over what the share shows, and what it cannot say is never taken for "nothing there".
#[tokio::test(flavor = "multi_thread")]
async fn a_linked_machine_looks_through_its_own_folders() {
    use serde_json::{json, Value};
    use spool::models::Kind;
    let w = World::new(Mode::Active).await;
    let (_helper, url, nas, mnt) = with_helper(&w).await;
    let api = spool::api::router(w.app.clone());
    let lay = |at: PathBuf, bytes: usize| {
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, vec![7u8; bytes]).unwrap();
    };
    lay(nas.join("films/Alpha (2001)/Alpha (2001).mkv"), 3000);
    lay(nas.join("films/Alpha (2001)/poster.jpg"), 10);
    lay(nas.join("films/Beta (2002)/Beta (2002).mkv"), 500);
    lay(nas.join("films/Beta (2002)/Beta (2002) - 1080p.mkv"), 200);
    lay(nas.join("films/Only a trailer/Trailers/teaser.mkv"), 50);

    let (_, links) = call(&api, "GET", "/api/links", Value::Null).await;
    assert_eq!(links[0]["seen"]["see"].as_bool(), Some(true), "{links}");
    let share = |name: &str| links[0]["seen"]["shares"].as_array().unwrap().iter().find(|s| s["path"].as_str().unwrap().ends_with(name)).unwrap()["id"].as_str().unwrap().to_string();

    // A library folder: the folders in it that hold a film, and how much.
    let folders = w.app.folders(&mnt.join("films"), Kind::Movie).await.unwrap();
    assert_eq!(folders.iter().map(|f| (f.name.as_str(), f.files, f.bytes)).collect::<Vec<_>>(), [("Alpha (2001)", 1, 3000), ("Beta (2002)", 2, 700)]);

    // From here on this machine sees other disks than the linked one has, as it would if
    // the share were stale. What the linked machine says is what is taken.
    let other = w.root.join("other");
    lay(other.join("films/Alpha (2001)/Seen across the share.mkv"), 1);
    std::fs::remove_file(&mnt).unwrap();
    std::os::unix::fs::symlink(&other, &mnt).unwrap();
    let alpha = mnt.join("films/Alpha (2001)");
    let found = w.app.survey(&alpha, Kind::Movie).await.unwrap();
    assert_eq!(found.videos.iter().map(|v| (v.rel.as_str(), v.size)).collect::<Vec<_>>(), [("Alpha (2001).mkv", 3000)]);
    assert_eq!(found.local.art.iter().map(|a| a.rel_path.as_str()).collect::<Vec<_>>(), ["poster.jpg"]);
    // A folder that is not there, on a disk that is, holds nothing.
    assert!(w.app.survey(&mnt.join("films/Gamma (2003)"), Kind::Movie).await.unwrap().videos.is_empty());

    // A film it tells of that cannot be read from here yet is not put on record without
    // its details, and one on record that it no longer has is taken off.
    let mut title = w.movie("Alpha", 2001, "tt0000001");
    title.path = alpha.to_string_lossy().to_string();
    w.app.db.save_title(&mut title).unwrap();
    let mut report = Default::default();
    w.app.scan_title(title.id, &mut report).await.unwrap();
    assert!(w.app.db.files(title.id).unwrap().is_empty(), "not readable from here yet");
    lay(other.join("films/Alpha (2001)/Alpha (2001).mkv"), 3000);
    w.app.scan_title(title.id, &mut report).await.unwrap();
    assert_eq!(w.app.db.files(title.id).unwrap().iter().map(|f| (f.rel_path.as_str(), f.size)).collect::<Vec<_>>(), [("Alpha (2001).mkv", 3000)]);

    // A compact version it tells of waits for its original, and is not put on record as
    // a film of its own for want of one.
    lay(nas.join("films/Beta (2002)/Beta (2002) - Compact 1080p.mkv"), 100);
    for name in ["Beta (2002) - Compact 1080p.mkv", "Beta (2002) - 1080p.mkv"] {
        lay(other.join("films/Beta (2002)").join(name), 100);
    }
    let mut beta = w.movie("Beta", 2002, "tt0000002");
    beta.path = mnt.join("films/Beta (2002)").to_string_lossy().to_string();
    w.app.db.save_title(&mut beta).unwrap();
    w.app.scan_title(beta.id, &mut report).await.unwrap();
    assert_eq!(w.app.db.files(beta.id).unwrap().iter().map(|f| f.rel_path.as_str()).collect::<Vec<_>>(), ["Beta (2002) - 1080p.mkv"], "the original cannot be read yet, so its compact version waits");
    lay(other.join("films/Beta (2002)/Beta (2002).mkv"), 500);
    w.app.scan_title(beta.id, &mut report).await.unwrap();
    let files = w.app.db.files(beta.id).unwrap();
    let original = files.iter().find(|f| f.rel_path == "Beta (2002).mkv").unwrap().id;
    assert_eq!(files.iter().find(|f| f.rel_path.contains("Compact")).unwrap().compact_of, Some(original));

    // A shared folder with nothing in it is a disk that has gone. Nothing is concluded:
    // the look fails, and the film on record stays on record.
    let (kept, emptied) = (w.root.join("kept"), nas.join("films"));
    std::fs::rename(&emptied, &kept).unwrap();
    std::fs::create_dir_all(&emptied).unwrap();
    assert!(w.app.survey(&alpha, Kind::Movie).await.is_err());
    assert!(w.app.scan_title(title.id, &mut report).await.is_err());
    assert_eq!(w.app.db.files(title.id).unwrap().len(), 1, "what could not be seen is not known to be gone");
    std::fs::remove_dir(&emptied).unwrap();
    std::fs::rename(&kept, &emptied).unwrap();

    // The linked machine follows no link, so nothing outside what it shares is told of.
    // This machine may follow one, and reads such a folder for itself.
    std::os::unix::fs::symlink(w.root.join("kept-elsewhere"), nas.join("films/Alpha (2001)/Extras")).unwrap();
    let http = reqwest::Client::new();
    let ask = |what: &'static str, key: &'static str, body: Value| {
        let (http, url) = (http.clone(), url.clone());
        async move { http.post(format!("{url}/link/see/v1/{what}")).bearer_auth(key).json(&body).send().await.unwrap().status().as_u16() }
    };
    const KEY: &str = "a-key-of-sufficient-length";
    assert_eq!(ask("survey", KEY, json!({"share": share("films"), "rel": "Alpha (2001)", "kind": "movie"})).await, 409);
    let found = w.app.survey(&alpha, Kind::Movie).await.unwrap();
    assert_eq!(found.videos.iter().map(|v| v.rel.as_str()).collect::<Vec<_>>(), ["Alpha (2001).mkv", "Seen across the share.mkv"], "read from here");
    std::fs::remove_file(nas.join("films/Alpha (2001)/Extras")).unwrap();
    // Nor on the way to a folder: one reached through a link is not looked through.
    std::os::unix::fs::symlink(nas.join("films/Beta (2002)"), nas.join("films/By another name")).unwrap();
    assert_eq!(ask("survey", KEY, json!({"share": share("films"), "rel": "By another name", "kind": "movie"})).await, 409);
    std::fs::remove_file(nas.join("films/By another name")).unwrap();

    // What is asked for is held to the shared folders, and nothing is said without the key.
    for (status, key, body) in [
        (200, KEY, json!({"share": share("films"), "rel": "Alpha (2001)", "kind": "movie"})),
        (200, KEY, json!({"share": share("films"), "kind": "movie"})),
        (401, "another-key-of-the-same-size", json!({"share": share("films"), "rel": "Alpha (2001)", "kind": "movie"})),
        (404, KEY, json!({"share": "000000000000", "rel": "Alpha (2001)", "kind": "movie"})),
        (400, KEY, json!({"share": share("films"), "rel": "../vault", "kind": "movie"})),
        (400, KEY, json!({"share": share("films"), "rel": "/etc", "kind": "movie"})),
        (503, KEY, json!({"share": share("vault"), "rel": "Alpha (2001)", "kind": "movie"})),
        (503, KEY, json!({"share": share("vault"), "kind": "movie"})),
    ] {
        assert_eq!(ask("survey", key, body.clone()).await, status, "{body}");
    }
    assert_eq!(ask("folders", KEY, json!({"share": share("vault"), "kind": "movie"})).await, 200, "an empty library folder is only empty");

    // A link finds its own folders. The same folders go by other names on this machine,
    // and nothing is taken from the names: the other machine marks each of its folders,
    // and the folder here with the mark in it is the one.
    std::fs::remove_file(&mnt).unwrap();
    std::fs::write(nas.join("vault/Kept (1999).mkv"), b"a film kept").unwrap();
    let shares = w.root.join("shares");
    std::fs::create_dir_all(&shares).unwrap();
    for (here, there) in [("Movies", "films"), ("Old films", "vault"), ("dl", "downloads")] {
        std::os::unix::fs::symlink(nas.join(there), shares.join(here)).unwrap();
    }
    let (_, mut g) = call(&api, "GET", "/api/settings/general", Value::Null).await;
    (g["links"][0]["folders"], g["links"][0]["key"]) = (json!([]), json!(KEY));
    let (status, said) = call(&api, "PUT", "/api/settings/general", g).await;
    assert_eq!(status, 200, "{said}");
    w.app.ask_remotes().await;
    let link = w.app.remotes()[0].clone();
    assert!(link.seen().problem.contains("has not been found among the shares mounted on this machine"), "{}", link.seen().problem);
    assert_eq!(link.unfound().len(), 2, "neither folder it shares has a place here yet");
    let text = |p: PathBuf| p.to_string_lossy().to_string();
    let mut found = w.app.find_folders(&link, vec![shares.join("Movies"), shares.join("Old films"), shares.join("dl"), w.root.join("other/films")]).await;
    found.sort_by(|a, b| a.there.cmp(&b.there));
    assert_eq!(found.iter().map(|f| (f.there.clone(), f.here.clone())).collect::<Vec<_>>(), [
        (text(nas.join("downloads")), text(shares.join("dl"))),
        (text(nas.join("films")), text(shares.join("Movies"))),
        // One it may only read is not written in, and is known by a file it holds.
        (text(nas.join("vault")), text(shares.join("Old films"))),
    ]);
    // Looked for from the folder above, they are found inside it.
    assert_eq!(w.app.find_folders(&link, vec![shares.clone()]).await.len(), 3);
    // Nothing is left behind in the folders that were marked.
    for folder in ["films", "downloads"] {
        assert!(std::fs::read_dir(nas.join(folder)).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".spool-here-")), "{folder}");
    }
    // Two folders here that answer to one there settle nothing, and neither is taken.
    std::os::unix::fs::symlink(nas.join("films"), shares.join("Movies again")).unwrap();
    let found = w.app.find_folders(&link, vec![shares.clone()]).await;
    assert!(found.iter().all(|f| f.there != text(nas.join("films"))) && found.len() == 2, "{found:?}");
    // Folders that are not that machine's are not found, whatever they are called.
    assert!(w.app.find_folders(&link, vec![w.root.join("other")]).await.is_empty());
    assert!(w.app.find_folders(&link, vec![]).await.is_empty());

    // Found, they are kept, and a path is turned by the folder it is in.
    std::fs::remove_file(shares.join("Movies again")).unwrap();
    let (status, said) = call(&api, "POST", "/api/links/1/find", Value::Null).await;
    assert_eq!(status, 200, "{said}");
    let link = w.app.remotes()[0].clone();
    assert_eq!(link.conf.folders.len(), 0, "nothing is mounted on a machine that only runs tests, so nothing was found to keep");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_film_on_a_linked_machine_is_played_from_it_on_a_ticket() {
    use serde_json::{json, Value};
    use spool::link::ticket::{Signer, Ticket, LIFE};
    let w = World::new(Mode::Active).await;
    let (_helper, url, nas, mnt) = with_helper(&w).await;
    let api = spool::api::router(w.app.clone());
    let film: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let folder = nas.join("vault/Alpha (2001)");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("Alpha (2001).mkv"), &film).unwrap();
    let mut alpha = w.movie("Alpha", 2001, "tt0000001");
    alpha.path = mnt.join("vault/Alpha (2001)").to_string_lossy().to_string();
    w.app.db.save_title(&mut alpha).unwrap();
    w.app.scan_all().await.unwrap();

    // The two have settled what the helper allows and what it shares.
    let (_, links) = call(&api, "GET", "/api/links", Value::Null).await;
    let seen = &links[0]["seen"];
    assert_eq!((seen["usable"].as_bool(), seen["play"].as_bool(), seen["grants"].to_string().as_str()), (Some(true), Some(true), r#"{"make":true,"play":true,"see":true,"work":true}"#), "{links}");
    let share = |name: &str| seen["shares"].as_array().unwrap().iter().find(|s| s["path"].as_str().unwrap().ends_with(name)).unwrap()["id"].as_str().unwrap().to_string();

    // Players reach Spool on a socket, and are known by the address they come from.
    let mut g = w.app.settings.general();
    g.players_enabled = true;
    w.app.settings.set_general(&g).unwrap();
    let door = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let spool_url = format!("http://{}", door.local_addr().unwrap());
    let jf = spool::jellyfin::router(w.app.clone());
    tokio::spawn(async move { axum::serve(door, jf.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    const WHO: &str = r#"MediaBrowser Client="Infuse-Direct", Device="Apple TV", DeviceId="test-device", Version="8.5""#;
    let signed_in = http.post(format!("{spool_url}/Users/AuthenticateByName")).header("x-emby-authorization", WHO).json(&json!({"Username": "spool", "Pw": ""})).send().await.unwrap().json::<Value>().await.unwrap();
    let auth = format!(r#"{WHO}, Token="{}""#, signed_in["AccessToken"].as_str().unwrap());
    let items = http.get(format!("{spool_url}/Items?Recursive=true&IncludeItemTypes=Movie")).header("x-emby-authorization", &auth).send().await.unwrap().json::<Value>().await.unwrap();
    let stream = format!("{spool_url}/Videos/{}/stream?static=true", items["Items"][0]["Id"].as_str().unwrap());
    let part = [("x-emby-authorization", auth.as_str()), ("range", "bytes=1000-1999")];

    // Until the link names the players to send, every film comes through Spool.
    let (status, head, body) = fetch(&http, "GET", &stream, &part).await;
    assert_eq!((status, head["content-range"].to_str().unwrap(), &body[..]), (206, "bytes 1000-1999/200000", &film[1000..2000]));

    // A player on the machine's own network is sent to the machine that holds the film.
    let send_from = |near: Value| {
        let (api, app) = (api.clone(), w.app.clone());
        async move {
            let (_, mut g) = call(&api, "GET", "/api/settings/general", Value::Null).await;
            g["links"][0]["play_near"] = near;
            let (status, said) = call(&api, "PUT", "/api/settings/general", g).await;
            assert_eq!(status, 200, "{said}");
            app.ask_remotes().await;
        }
    };
    send_from(json!(true)).await;
    assert!(w.app.remotes()[0].seen().usable, "the key was kept though the page only ever saw it masked");
    let (status, head, _) = fetch(&http, "GET", &stream, &part).await;
    let there = head["location"].to_str().unwrap().to_string();
    assert_eq!((status, head["cache-control"].to_str().unwrap()), (307, "private, no-store"));
    assert!(there.starts_with(&format!("{url}/link/play/v1/file?ticket=")), "{there}");
    assert!(!w.app.player_requests.lock().iter().any(|line| line.contains(there.rsplit('=').next().unwrap())), "no ticket is kept in what Spool remembers of requests");
    // What is playing, who by and on what, and that it comes from the other machine.
    let (_, activity) = call(&api, "GET", "/api/activity", Value::Null).await;
    let now = &activity["playing"][0];
    assert_eq!((now["title"].as_str(), now["from"].as_str(), now["account"].as_str(), now["device"].as_str()), (Some("Alpha"), Some("Nas"), Some("spool"), Some("Apple TV")), "{}", activity["playing"]);
    // The sitting is written down as the player says how far it has got.
    let film_id = stream.rsplit("/Videos/").next().unwrap().split('/').next().unwrap().to_string();
    for (path, body) in [("/Sessions/Playing", json!({"ItemId": film_id, "PositionTicks": 0})), ("/Sessions/Playing/Progress", json!({"ItemId": film_id, "PositionTicks": 600_000_000i64, "IsPaused": false}))] {
        assert_eq!(http.post(format!("{spool_url}{path}")).header("x-emby-authorization", &auth).json(&body).send().await.unwrap().status().as_u16(), 204);
    }
    let (_, people) = call(&api, "GET", "/api/accounts", Value::Null).await;
    let (me, sat) = (&people["accounts"][0], &people["recent"][0]);
    assert_eq!((me["totals"]["plays"].as_u64(), me["playing"][0]["title"].as_str(), sat["account"].as_str(), sat["play"]["title"].as_str(), sat["play"]["source"].as_str(), sat["play"]["device"].as_str(), sat["play"]["position"].as_f64()), (Some(1), Some("Alpha"), Some("spool"), Some("Alpha"), Some("Nas"), Some("Apple TV"), Some(60.0)), "{people}");

    // Told to send nobody, it sends nobody, and says where the film comes from now.
    send_from(json!(false)).await;
    assert_eq!(fetch(&http, "GET", &stream, &part).await.0, 206);
    let (_, activity) = call(&api, "GET", "/api/activity", Value::Null).await;
    assert_eq!(activity["playing"][0]["from"].as_str(), Some(""), "sent from here now");
    send_from(json!(true)).await;
    assert_eq!(fetch(&http, "GET", &stream, &part).await.0, 307, "the player and the machine are on one network");

    // A player is sent there only if the file opens there now. The machine will not follow a
    // link in what it shares, so when the film is one, the player has it from here instead;
    // and what the machine said stands for a little while, so it is asked again after.
    let (real, aside) = (folder.join("Alpha (2001).mkv"), w.root.join("aside.mkv"));
    std::fs::rename(&real, &aside).unwrap();
    std::os::unix::fs::symlink(&aside, &real).unwrap();
    assert_eq!(fetch(&http, "GET", &stream, &part).await.0, 307, "it opened a moment ago, and is not asked again so soon");
    w.app.opened.lock().clear();
    let (status, head, body) = fetch(&http, "GET", &stream, &part).await;
    assert_eq!((status, head["content-range"].to_str().unwrap(), &body[..]), (206, "bytes 1000-1999/200000", &film[1000..2000]), "sent from here when the machine cannot open it");
    let (_, activity) = call(&api, "GET", "/api/activity", Value::Null).await;
    assert_eq!(activity["playing"][0]["from"].as_str(), Some(""));
    std::fs::remove_file(&real).unwrap();
    std::fs::rename(&aside, &real).unwrap();
    assert_eq!(fetch(&http, "GET", &stream, &part).await.0, 206, "and that it could not stands for a little while too");
    w.app.opened.lock().clear();
    assert_eq!(fetch(&http, "GET", &stream, &part).await.0, 307);

    // The film comes from the helper, with no key and no sign-in, and not by way of Spool:
    // the share is taken away from Spool's machine and the film still plays.
    std::fs::remove_file(&mnt).unwrap();
    let (status, head, body) = fetch(&http, "GET", &there, &[("range", "bytes=1000-1999")]).await;
    assert_eq!((status, head["content-range"].to_str().unwrap(), head["content-type"].to_str().unwrap(), &body[..]), (206, "bytes 1000-1999/200000", "video/x-matroska", &film[1000..2000]));
    assert_eq!(head["cache-control"], "private, no-store");
    // Skipping about asks again with the same ticket.
    let (status, head, body) = fetch(&http, "GET", &there, &[("range", "bytes=199990-")]).await;
    assert_eq!((status, head["content-range"].to_str().unwrap(), &body[..]), (206, "bytes 199990-199999/200000", &film[199990..]));
    let (status, _, body) = fetch(&http, "GET", &there, &[]).await;
    assert_eq!((status, body.len(), body == film), (200, 200_000, true));
    let (status, head, body) = fetch(&http, "HEAD", &there, &[("range", "bytes=0-9")]).await;
    assert_eq!((status, head["content-length"].to_str().unwrap(), head.get("content-range").is_none(), body.len()), (200, "200000", true, 0), "asking what is there says nothing of parts");
    let (status, _, body) = fetch(&http, "GET", &there, &[("range", "bytes=0-9, 20-29")]).await;
    assert_eq!((status, body.len()), (200, 200_000), "several parts at once are answered with the whole");
    let (status, head, _) = fetch(&http, "GET", &there, &[("range", "bytes=200000-")]).await;
    assert_eq!((status, head["content-range"].to_str().unwrap()), (416, "bytes */200000"));

    // Nothing but a ticket of Spool's own gets a file, and a ticket gets one file.
    let status_of = |url: String| {
        let http = http.clone();
        async move { fetch(&http, "GET", &url, &[]).await.0 }
    };
    let base = format!("{url}/link/play/v1/file");
    let ticket = there.rsplit('=').next().unwrap().to_string();
    let mut changed = ticket.clone().into_bytes();
    let last = changed.len() - 1;
    changed[last] = if changed[last] == b'A' { b'B' } else { b'A' };
    assert_eq!(status_of(format!("{base}?ticket={}", String::from_utf8(changed).unwrap())).await, 401);
    assert_eq!(status_of(base.clone()).await, 400);
    assert_eq!(status_of(format!("{base}?ticket={ticket}&ticket={ticket}")).await, 400, "one ticket, so there is no doubt which file is meant");
    assert_eq!(status_of(format!("{base}?ticket={ticket}&path=/etc/passwd")).await, 200, "and nothing else in the address chooses the file");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let signer = Signer::new("a-key-of-sufficient-length");
    let made = |share: String, rel: &str, at: u64| signer.sign(&Ticket { share, rel: rel.into(), made: at, until: at + LIFE });
    assert_eq!(status_of(format!("{base}?ticket={}", made(share("vault"), "Alpha (2001)/Alpha (2001).mkv", now))).await, 200);
    assert_eq!(status_of(format!("{base}?ticket={}", made(share("vault"), "Alpha (2001)/Alpha (2001).mkv", now - LIFE - 3600))).await, 410, "a ticket runs out");
    assert_eq!(status_of(format!("{base}?ticket={}", Signer::new("some-other-key-entirely").sign(&Ticket { share: share("vault"), rel: "Alpha (2001)/Alpha (2001).mkv".into(), made: now, until: now + LIFE }))).await, 401);
    assert_eq!(status_of(format!("{base}?ticket={}", made(share("vault"), "Alpha (2001)/Missing.mkv", now))).await, 404);
    assert_eq!(status_of(format!("{base}?ticket={}", made("not-a-share".into(), "Alpha (2001)/Alpha (2001).mkv", now))).await, 404);
    assert_eq!(status_of(format!("{base}?ticket={}", made(share("vault"), "Alpha (2001)", now))).await, 404, "a folder is not a file");
    // A link planted in a shared folder leads nowhere, whether it points out of the share or
    // at something else within reach.
    std::fs::write(w.root.join("private.txt"), b"not for players").unwrap();
    std::os::unix::fs::symlink(w.root.join("private.txt"), folder.join("Leak.mkv")).unwrap();
    assert_eq!(status_of(format!("{base}?ticket={}", made(share("vault"), "Alpha (2001)/Leak.mkv", now))).await, 403);
    std::os::unix::fs::symlink(nas.join("downloads"), nas.join("vault/Way out")).unwrap();
    std::fs::write(nas.join("downloads/half.mkv"), b"half a download").unwrap();
    assert_eq!(status_of(format!("{base}?ticket={}", made(share("vault"), "Way out/half.mkv", now))).await, 403);

    // With the share back, and Spool told to send nobody, the film comes through Spool.
    std::os::unix::fs::symlink(&nas, &mnt).unwrap();
    send_from(json!(false)).await;
    let (status, _, body) = fetch(&http, "GET", &stream, &part).await;
    assert_eq!((status, &body[..]), (206, &film[1000..2000]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_does_only_what_it_was_started_allowing() {
    use serde_json::{json, Value};
    use spool::link::Grants;
    let w = World::new(Mode::Active).await;
    const KEY: &str = "a-key-of-sufficient-length";
    let http = reqwest::Client::new();
    let start = |name: &'static str, grants: Grants| {
        let root = w.root.join(name);
        async move {
            std::fs::create_dir_all(root.join("films/Alpha (2001)")).unwrap();
            std::fs::create_dir_all(root.join("downloads/complete")).unwrap();
            std::fs::write(root.join("films/Alpha (2001)/Alpha (2001).mkv"), b"a film").unwrap();
            let conf = spool::helper::Conf { key: KEY.into(), roots: vec![root.join("films")], read_roots: vec![], downloads: root.join("downloads"), code: None, grants }.checked().unwrap();
            let helper = spool::helper::Helper::start(&root.join("data"), conf).await.unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            (helper, listener, url, root)
        }
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let ticket_for = |hello: &Value| spool::link::ticket::Signer::new(KEY).issue(hello["shares"][0]["id"].as_str().unwrap_or("none"), "Alpha (2001)/Alpha (2001).mkv", now);

    // One that may be seen and played from, and does no work: it says so, and holds to it.
    let (helper, listener, url, root) = start("friend", Grants { see: true, play: true, ..Default::default() }).await;
    let router = helper.router();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    assert_eq!(http.get(format!("{url}/link")).send().await.unwrap().status().as_u16(), 401, "it says nothing of itself without the key");
    let hello: Value = http.get(format!("{url}/link")).bearer_auth(KEY).send().await.unwrap().json().await.unwrap();
    assert_eq!((hello["grants"].to_string().as_str(), hello["protocols"].to_string().as_str()), (r#"{"make":false,"play":true,"see":true,"work":false}"#, r#"{"make":[],"play":[1],"see":[1],"work":[]}"#), "{hello}");
    for (method, path) in [("GET", "/link/work/v1/info"), ("GET", "/link/work/v1/jobs"), ("POST", "/link/work/v1/fs/remove"), ("POST", "/link/work/v1/moves"), ("PUT", "/link/work/v1/config")] {
        let resp = http.request(method.parse().unwrap(), format!("{url}{path}")).bearer_auth(KEY).json(&json!({"path": root.join("films/Alpha (2001)")})).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{method} {path}");
    }
    assert!(root.join("films/Alpha (2001)/Alpha (2001).mkv").exists());
    assert_eq!(http.get(format!("{url}/link/play/v1/file?ticket={}", ticket_for(&hello))).send().await.unwrap().text().await.unwrap(), "a film");

    // One that works and hands nothing to players.
    let (helper, listener, url, _) = start("worker", Grants { work: true, ..Default::default() }).await;
    let router = helper.router();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let hello: Value = http.get(format!("{url}/link")).bearer_auth(KEY).send().await.unwrap().json().await.unwrap();
    assert_eq!(hello["protocols"].to_string(), r#"{"make":[],"play":[],"see":[],"work":[1]}"#);
    let asked = http.post(format!("{url}/link/see/v1/survey")).bearer_auth(KEY).json(&json!({"share": hello["shares"][0]["id"], "rel": "Alpha (2001)", "kind": "movie"})).send().await.unwrap();
    assert_eq!(asked.status().as_u16(), 403, "it was not started to say what is in its folders");
    assert_eq!(http.get(format!("{url}/link/work/v1/info")).bearer_auth(KEY).send().await.unwrap().status().as_u16(), 200);
    assert_eq!(http.get(format!("{url}/link/play/v1/file?ticket={}", ticket_for(&hello))).send().await.unwrap().status().as_u16(), 403);

    // Spool takes one that only works for what it is: nobody is sent to it for a film.
    let root = w.root.join("worker");
    let api = spool::api::router(w.app.clone());
    let (_, mut g) = call(&api, "GET", "/api/settings/general", Value::Null).await;
    g["locations"] = json!([{"name": "Worker", "movie_root": root.join("films"), "manage": true}]);
    g["links"] = json!([{"name": "Worker", "url": url, "key": KEY, "folders": [{"here": root, "there": root}], "play_near": true}]);
    let (status, said) = call(&api, "PUT", "/api/settings/general", g).await;
    assert_eq!(status, 200, "{said}");
    w.app.ask_remotes().await;
    let seen = w.app.remotes()[0].seen();
    assert_eq!((seen.usable, seen.play, seen.see, seen.downloads_seen, seen.grants), (true, false, false, true, Grants { work: true, ..Default::default() }), "{}", seen.problem);
    assert!(w.app.play_direct(&root.join("films/Alpha (2001)/Alpha (2001).mkv"), &["127.0.0.1".parse().unwrap()]).is_none(), "nobody is sent to a helper that does not hand out files");

    // Two links are never given the same folder, and a number once given is not given again.
    let (_, mut g) = call(&api, "GET", "/api/settings/general", Value::Null).await;
    let mut second = g["links"][0].clone();
    (second["id"], second["name"], second["key"], second["folders"]) = (json!(0), json!("Twin"), json!(KEY), json!([{"here": root.join("films"), "there": root.join("films")}]));
    g["links"].as_array_mut().unwrap().push(second);
    let (status, said) = call(&api, "PUT", "/api/settings/general", g).await;
    assert!(status == 400 && said["error"].as_str().unwrap().contains("each folder is one helper's"), "{status} {said}");
    let (_, mut g) = call(&api, "GET", "/api/settings/general", Value::Null).await;
    let kept = g["links"][0].clone();
    g["links"] = json!([]);
    assert_eq!(call(&api, "PUT", "/api/settings/general", g).await.0, 200);
    let (_, mut g) = call(&api, "GET", "/api/settings/general", Value::Null).await;
    // Sent with the number the removed link had, as a page left open since before would.
    let mut again = kept;
    again["key"] = json!(KEY);
    assert_eq!(again["id"], 1);
    g["links"] = json!([again]);
    assert_eq!(call(&api, "PUT", "/api/settings/general", g).await.0, 200);
    let g = w.app.settings.general();
    assert_eq!((g.links[0].id, g.link_seq), (2, 2), "a download that ran on the link that was removed is never taken for the new one's");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_report_that_cannot_be_read_and_an_answer_that_is_lost_cost_nothing() {
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicBool, Ordering};
    let w = World::new(Mode::Active).await;
    let root = w.root.join("nas");
    std::fs::create_dir_all(root.join("downloads/complete")).unwrap();

    // A stand-in for a helper that reports one download and then, as one a release ahead
    // of this Spool might, reports it in a way this Spool cannot read.
    let garbled = Arc::new(AtomicBool::new(false));
    let refusing = Arc::new(AtomicBool::new(false));
    let job = json!({"id": "j1", "name": "A.Film.2020", "state": "downloading", "total_bytes": 100, "done_bytes": 10, "speed": 5, "eta_secs": 18, "missing_articles": 0, "damaged_articles": 0, "message": "", "error": null, "output_path": null, "added_at": 1});
    let (flag, no, complete) = (garbled.clone(), refusing.clone(), root.join("downloads/complete"));
    let stand_in = axum::Router::new()
        .route("/link", axum::routing::get(|| async { axum::Json(json!({"boot": "b1", "version": "9.9.9", "protocols": {"work": [1]}, "grants": {"work": true}, "shares": []})) }))
        .route("/link/work/v1/info", axum::routing::get(move || { let complete = complete.clone(); async move { axum::Json(json!({"version": "9.9.9", "boot": "b1", "complete": complete})) } }))
        .route("/link/work/v1/config", axum::routing::put(|| async { axum::Json(json!({"ok": true})) }))
        .route("/link/work/v1/jobs", axum::routing::get(move || {
            let (flag, no, job) = (flag.clone(), no.clone(), job.clone());
            async move {
                if no.load(Ordering::Relaxed) {
                    return (axum::http::StatusCode::FORBIDDEN, axum::Json(json!({"error": "this machine was not started to do work for another"})));
                }
                (axum::http::StatusCode::OK, axum::Json(if flag.load(Ordering::Relaxed) { json!({"boot": "b1", "jobs": [{"id": "j1", "state": "a state from the future"}]}) } else { json!({"boot": "b1", "jobs": [job], "paused": false}) }))
            }
        // It takes a download and answers without saying what it called it.
        }).post(|| async { axum::Json(json!({"taken": true})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, stand_in).await.unwrap() });
    let api = spool::api::router(w.app.clone());
    let (_, mut g) = call(&api, "GET", "/api/settings/general", Value::Null).await;
    g["links"] = json!([{"name": "Nas", "url": url, "key": "a-key-of-sufficient-length", "folders": [{"here": root, "there": root}]}]);
    assert_eq!(call(&api, "PUT", "/api/settings/general", g).await.0, 200);
    w.app.ask_remotes().await;
    assert!(matches!(w.app.job_lookup("@1:j1"), spool::remote::Found::Job(_)));
    assert!(matches!(w.app.job_lookup("@1:never"), spool::remote::Found::Gone), "a helper that answers plainly is believed about what it does not have");

    garbled.store(true, Ordering::Relaxed);
    w.app.ask_remotes().await;
    assert!(matches!(w.app.job_lookup("@1:j1"), spool::remote::Found::Job(_)), "what was last known of a download is kept");
    assert!(matches!(w.app.job_lookup("@1:never"), spool::remote::Found::Unknown), "and a report that could not be read is not taken to say a download has gone");
    assert!(w.app.remotes()[0].seen().problem.contains("cannot read"), "{}", w.app.remotes()[0].seen().problem);
    garbled.store(false, Ordering::Relaxed);
    w.app.ask_remotes().await;
    assert!(w.app.remotes()[0].seen().problem.is_empty() && matches!(w.app.job_lookup("@1:never"), spool::remote::Found::Gone), "and it is believed again once it can be read");

    // A helper that may have taken a download, and did not say so in a way that can be
    // read, is not doubled up on: the download is not started here as well.
    let mut film = w.movie("A Film", 2020, "tt7654321");
    film.path = root.join("films/A Film (2020)").to_string_lossy().to_string();
    w.app.db.save_title(&mut film).unwrap();
    let mut post = Post::new();
    post.add_file("a.film.2020.mkv", b"not much of a film", 30_000);
    let refused = w.app.job_add(&film, &post.nzb(), "A.Film.2020").unwrap_err().to_string();
    assert!(refused.contains("may have taken the download") && w.app.engine.jobs().is_empty(), "{refused}");

    // One that starts refusing the work is no longer what it was when it was met: nothing
    // is concluded from what it does not report, and it is not used until met again.
    refusing.store(true, Ordering::Relaxed);
    w.app.ask_remotes().await;
    let seen = w.app.remotes()[0].seen();
    assert!(!seen.usable && !seen.play && seen.problem.contains("not started to do work"), "{}", seen.problem);
    assert!(matches!(w.app.job_lookup("@1:never"), spool::remote::Found::Unknown));
    refusing.store(false, Ordering::Relaxed);
    w.app.ask_remotes().await;
    assert!(w.app.remotes()[0].seen().usable && w.app.remotes()[0].seen().problem.is_empty(), "and it is used again once it does the work again");

    // Asked twice for one download, as happens when the first answer is lost on the way, a
    // real helper starts it once, and has written down that it did.
    let w2 = World::new(Mode::Active).await;
    let (helper, url, _, _) = with_helper(&w2).await;
    let mut post = Post::new();
    post.add_file("a.film.2020.mkv", b"not much of a film", 30_000);
    let nzb = post.nzb();
    let http = reqwest::Client::new();
    let mut ids = vec![];
    for _ in 0..2 {
        let said: Value = http.post(format!("{url}/link/work/v1/jobs?name=A.Film.2020&rid=one-request")).bearer_auth("a-key-of-sufficient-length").body(nzb.clone()).send().await.unwrap().json().await.unwrap();
        ids.push(said["id"].as_str().unwrap().to_string());
    }
    assert_eq!((ids[0] == ids[1], helper.engine().jobs().len()), (true, 1));
    let kept: Value = serde_json::from_slice(&std::fs::read(w2.root.join("helper-data/taken.json")).unwrap()).unwrap();
    assert_eq!(kept["one-request"][0].as_str(), Some(ids[0].as_str()), "so a restart between the two would change nothing: {kept}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_linked_machine_makes_compact_versions_and_hands_them_back() {
    use serde_json::{json, Value};
    use spool::link::Grants;
    let video = video_or_skip!();
    let w = World::new(Mode::Active).await;
    let api = spool::api::router(w.app.clone());
    let http = reqwest::Client::new();
    const KEY: &str = "a-key-of-sufficient-length";

    // Spool answers on the network, since the other machine reads each film from it.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let here = format!("http://{}", listener.local_addr().unwrap());
    w.app.port.set(listener.local_addr().unwrap().port()).unwrap();
    let served = api.clone();
    tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });

    // A machine with none of the library on it, started with no folders at all.
    assert!(spool::helper::Conf { key: KEY.into(), roots: vec![], read_roots: vec![], downloads: Default::default(), code: None, grants: Grants { make: true, see: true, ..Default::default() } }.checked().is_err(), "folders are needed to show what is in them");
    let conf = spool::helper::Conf { key: KEY.into(), roots: vec![], read_roots: vec![], downloads: Default::default(), code: None, grants: Grants { make: true, ..Default::default() } }.checked().unwrap();
    let helper = spool::helper::Helper::start(&w.root.join("maker-data"), conf).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = helper.router();
    tokio::spawn(async move { axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
    let hello: Value = http.get(format!("{url}/link")).bearer_auth(KEY).send().await.unwrap().json().await.unwrap();
    assert_eq!((hello["protocols"].to_string().as_str(), hello["shares"].to_string().as_str()), (r#"{"make":[1],"play":[],"see":[],"work":[]}"#, "[]"), "{hello}");
    assert_eq!(http.get(format!("{url}/link/work/v1/info")).bearer_auth(KEY).send().await.unwrap().status().as_u16(), 403);
    assert_eq!(http.get(format!("{url}/link/make/v1/info")).send().await.unwrap().status().as_u16(), 401, "nothing is said without the key");
    // It reads a film only from where the app says films are read, on the machine that asked.
    for path in ["/api/settings/general", "/link/source/1?ticket=x y", "http://elsewhere/link/source/1"] {
        let asked = http.post(format!("{url}/link/make/v1/jobs")).bearer_auth(KEY).json(&json!({"id": "one", "port": 1, "path": path, "compact": {}, "info": {}, "bytes": 0})).send().await.unwrap();
        assert_eq!(asked.status().as_u16(), 400, "{path}");
    }
    assert_eq!(http.post(format!("{url}/link/make/v1/jobs")).bearer_auth(KEY).json(&json!({"id": "../one", "port": 1, "path": "/link/source/1?ticket=x", "compact": {}, "info": {}, "bytes": 0})).send().await.unwrap().status().as_u16(), 400);

    // Two films, as they would lie in a library.
    let mut films = vec![];
    for (name, year, imdb) in [("Heat", 1995, "tt0113277"), ("Ronin", 1998, "tt0122690")] {
        let home = w.root.join(format!("Movies/{name} ({year})"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(format!("{name} ({year}) [Bluray-1080p].mkv")), &video).unwrap();
        let mut t = w.movie(name, year, imdb);
        t.path = home.to_string_lossy().to_string();
        w.app.db.save_title(&mut t).unwrap();
        films.push(t);
    }
    w.app.scan_all().await.unwrap();
    let rate = w.app.db.files(films[0].id).unwrap()[0].media_info.as_ref().unwrap().bit_rate;
    let mut g = w.app.settings.general();
    g.compact = spool::compact::Compact { height: 120, video_kbps: (rate / 4000).max(10) as u32, audio_kbps: 0, ..Default::default() };
    w.app.settings.set_general(&g).unwrap();

    // Linked, it is known for what it is: it works on nothing and holds nothing, and makes
    // compact versions if it has video hardware to make them on.
    let (_, mut g) = call(&api, "GET", "/api/settings/general", Value::Null).await;
    g["links"] = json!([{"name": "Maker", "url": url, "key": KEY, "folders": [], "play_near": false, "make": "never"}]);
    let (status, said) = call(&api, "PUT", "/api/settings/general", g).await;
    assert_eq!(status, 200, "{said}");
    w.app.ask_remotes().await;
    let r = w.app.remotes()[0].clone();
    let seen = r.seen();
    assert_eq!((seen.reachable, seen.usable, seen.play, seen.see, seen.problem.as_str()), (true, false, false, false, ""), "{seen:?}");
    assert!(!r.makes(), "it is given none while its settings say never");
    let source = format!("{here}/link/source/{}?ticket=", r.conf.id);
    let file = w.app.db.files(films[0].id).unwrap()[0].id;
    assert_eq!(http.get(format!("{source}{}", r.source_ticket(file))).send().await.unwrap().status().as_u16(), 410, "a film is handed over only while that machine is making it");
    assert_eq!(http.get(format!("{source}not-a-ticket")).send().await.unwrap().status().as_u16(), 401);
    if !seen.make {
        assert_ne!(seen.make_means, "hardware");
        eprintln!("no video hardware here, so a linked machine is given nothing to make; the rest is skipped");
        return;
    }

    let (_, mut g) = call(&api, "GET", "/api/settings/general", Value::Null).await;
    g["links"][0]["make"] = json!("always");
    assert_eq!(call(&api, "PUT", "/api/settings/general", g).await.0, 200);
    w.app.ask_remotes().await;
    assert!(w.app.remotes()[0].makes());
    let (status, said) = call(&api, "POST", "/api/compact", json!({"title_ids": [films[0].id, films[1].id]})).await;
    assert_eq!((status, said["queued"].as_u64()), (200, Some(2)), "{said}");
    // Each machine takes its film as soon as it gets to it.
    let mut activity = Value::Null;
    for _ in 0..100 {
        activity = call(&api, "GET", "/api/activity", Value::Null).await.1;
        if activity["compact"]["now"].as_array().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let by: Vec<&str> = activity["compact"]["now"].as_array().unwrap().iter().filter_map(|n| n["by"].as_str()).collect();
    assert!(by.contains(&"Maker") && by.contains(&""), "one is made there and one here, at once: {}", activity["compact"]);
    let here = activity["compact"]["here"].as_str().unwrap();
    assert!(!here.is_empty() && activity["compact"]["now"].as_array().unwrap().iter().all(|n| n["kept_on"].as_str() == Some(here) && n["kept"].is_string()), "each says where its original is kept: {}", activity["compact"]);
    for _ in 0..1200 {
        if films.iter().all(|t| w.app.db.files(t.id).unwrap().iter().any(|f| f.compact_of.is_some())) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut made_by = vec![];
    for t in &films {
        let files = w.app.db.files(t.id).unwrap();
        let compact = files.iter().find(|f| f.compact_of.is_some()).unwrap_or_else(|| panic!("{} has no compact version: {:?}", t.title, w.app.db.history(Some(t.id), 10).unwrap().iter().map(|h| h.data.to_string()).collect::<Vec<_>>()));
        let on_disk = std::path::Path::new(&t.path).join(&compact.rel_path);
        assert!(on_disk.exists() && compact.size == std::fs::metadata(&on_disk).unwrap().len() && compact.media_info.as_ref().is_some_and(|m| m.height <= 120), "{compact:?}");
        let history = w.app.db.history(Some(t.id), 10).unwrap();
        made_by.push(history.iter().find(|h| h.kind == "compact_made").unwrap().data["by"].as_str().unwrap().to_string());
    }
    made_by.sort();
    assert_eq!(made_by, ["", "Maker"]);
    // A film's page carries where it can be played from without signing in, on a ticket for that file.
    let (_, page) = call(&api, "GET", &format!("/api/titles/{}", films[0].id), Value::Null).await;
    let play = page["play"]["path"].as_str().unwrap_or_else(|| panic!("{}", page["play"])).to_string();
    let jf = spool::jellyfin::router(w.app.clone());
    let door = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let door_url = format!("http://{}", door.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(door, jf.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
    let got = http.get(format!("{door_url}{play}")).header("Range", "bytes=0-99").send().await.unwrap();
    assert_eq!((got.status().as_u16(), got.bytes().await.unwrap().as_ref()), (206, &video[..100]));
    assert_eq!(http.get(format!("{door_url}/spool/play/x.mkv?ticket=nothing")).send().await.unwrap().status().as_u16(), 401);
    assert_eq!(http.get(format!("{door_url}/spool/play/x.mkv?ticket={}", w.app.remotes()[0].source_ticket(file))).send().await.unwrap().status().as_u16(), 401, "a ticket for one thing is not a ticket for another");

    // Held, the line is left where it is; let go, it is taken up.
    assert_eq!(call(&api, "POST", "/api/compact/hold", json!({"hold": true})).await.0, 200);
    for t in &films {
        let compact = w.app.db.files(t.id).unwrap().into_iter().find(|f| f.compact_of.is_some()).unwrap();
        w.app.delete_compact(compact.id).unwrap();
    }
    // Some of the library can be asked for instead of all of it, and what is in line is
    // not counted again as more to do.
    let would = |report: Value| report["would"]["files"].as_u64();
    assert_eq!(would(call(&api, "GET", "/api/compact", Value::Null).await.1), Some(2));
    let (_, said) = call(&api, "POST", "/api/compact/everything", json!({"count": 1})).await;
    assert_eq!(said["queued"].as_u64(), Some(1), "{said}");
    assert_eq!(would(call(&api, "GET", "/api/compact", Value::Null).await.1), Some(1));
    let (_, said) = call(&api, "POST", "/api/compact/everything", json!({"count": 5})).await;
    assert_eq!(said["queued"].as_u64(), Some(1), "asking again takes the next: {said}");
    let (_, said) = call(&api, "POST", "/api/compact/cancel", json!({})).await;
    assert_eq!(said["cancelled"].as_u64(), Some(2), "{said}");
    let (_, said) = call(&api, "POST", "/api/compact", json!({"title_ids": [films[0].id]})).await;
    assert_eq!(said["queued"].as_u64(), Some(1), "{said}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (_, activity) = call(&api, "GET", "/api/activity", Value::Null).await;
    let (_, busy) = call(&api, "GET", "/api/busy", Value::Null).await;
    assert_eq!((activity["compact"]["now"].as_array().map(Vec::len), activity["compact"]["waiting_count"].as_u64(), activity["compact"]["held"].as_bool(), busy["making"].as_u64()), (Some(0), Some(1), Some(true), Some(0)), "{}", activity["compact"]);
    assert_eq!(call(&api, "POST", "/api/compact/hold", json!({"hold": false})).await.0, 200);
    for _ in 0..1200 {
        if w.app.db.files(films[0].id).unwrap().iter().any(|f| f.compact_of.is_some()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(w.app.db.files(films[0].id).unwrap().iter().any(|f| f.compact_of.is_some()), "let go, the line is taken up");
    for _ in 0..100 {
        if !w.app.compacting(w.app.db.files(films[0].id).unwrap()[0].id) && std::fs::read_dir(w.root.join("maker-data/making")).map(|d| d.count()).unwrap_or(0) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Nothing is left on the machine that made it, nor in the film's folder but the two files.
    assert_eq!(std::fs::read_dir(w.root.join("maker-data/making")).map(|d| d.count()).unwrap_or(0), 0);
    assert_eq!(films.iter().map(|t| std::fs::read_dir(&t.path).unwrap().count()).collect::<Vec<_>>(), [2, 1]);

    // Films kept on a linked machine's disks are read from them one at a time, however many
    // machines there are to make them: two at once keep a NAS too busy to answer anyone else.
    let (_, mut g) = call(&api, "GET", "/api/settings/general", Value::Null).await;
    g["links"][0]["folders"] = json!([{"here": w.root.join("Movies").to_string_lossy(), "there": "/mnt/movies"}]);
    let (status, said) = call(&api, "PUT", "/api/settings/general", g).await;
    assert_eq!(status, 200, "{said}");
    w.app.ask_remotes().await;
    let compact = w.app.db.files(films[0].id).unwrap().into_iter().find(|f| f.compact_of.is_some()).unwrap();
    w.app.delete_compact(compact.id).unwrap();
    let (_, said) = call(&api, "POST", "/api/compact", json!({"title_ids": [films[0].id, films[1].id]})).await;
    assert_eq!(said["queued"].as_u64(), Some(2), "{said}");
    let mut most = 0;
    for _ in 0..2400 {
        let (_, activity) = call(&api, "GET", "/api/activity", Value::Null).await;
        most = most.max(activity["compact"]["now"].as_array().map_or(0, Vec::len));
        if films.iter().all(|t| w.app.db.files(t.id).unwrap().iter().any(|f| f.compact_of.is_some())) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(films.iter().all(|t| w.app.db.files(t.id).unwrap().iter().any(|f| f.compact_of.is_some())), "both are made in the end");
    assert_eq!(most, 1, "one at a time from the one machine's disks");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_what_arrives_is_put_in_line_and_turning_that_off_takes_it_out() {
    use serde_json::{json, Value};
    let video = video_or_skip!();
    let w = World::new(Mode::Active).await;
    let api = spool::api::router(w.app.clone());
    let mut films = vec![];
    for (name, imdb) in [("Heat", "tt0113277"), ("Ronin", "tt0122690"), ("Thief", "tt0083190")] {
        let home = w.root.join(format!("Movies/{name} (1995)"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(format!("{name} (1995) [Bluray-1080p].mkv")), &video).unwrap();
        let mut t = w.movie(name, 1995, imdb);
        t.path = home.to_string_lossy().to_string();
        w.app.db.save_title(&mut t).unwrap();
        films.push(t);
    }
    w.app.scan_all().await.unwrap();
    let file = |i: usize| w.app.db.files(films[i].id).unwrap()[0].id;
    let rate = w.app.db.files(films[0].id).unwrap()[0].media_info.as_ref().unwrap().bit_rate;
    // Held, so that what is in line stays there to be looked at.
    assert_eq!(call(&api, "POST", "/api/compact/hold", json!({"hold": true})).await.0, 200);
    let (_, mut g) = call(&api, "GET", "/api/settings/general", Value::Null).await;
    g["compact"] = json!({"height": 120, "video_kbps": (rate / 4000).max(10), "audio_kbps": 0, "auto": true});
    assert_eq!(call(&api, "PUT", "/api/settings/general", g.clone()).await.0, 200);
    let waiting = || async {
        let (_, a) = call(&api, "GET", "/api/activity", Value::Null).await;
        let mut ids: Vec<i64> = a["compact"]["waiting"].as_array().map(|w| w.iter().filter_map(|n| n["file_id"].as_i64()).collect()).unwrap_or_default();
        ids.sort();
        ids
    };

    // Heat arrives; Ronin is asked for by hand; Thief, already there, is left alone.
    w.app.compact_arrivals(films[0].id, &[file(0)]);
    assert_eq!(waiting().await, [file(0)], "only what arrived");
    assert_eq!(call(&api, "POST", "/api/compact", json!({"file_ids": [file(1)]})).await.0, 200);
    // Asked for by hand and then arriving, it is still the one asked for.
    w.app.compact_arrivals(films[1].id, &[file(1)]);

    // Turned off, what it put in line leaves, and what was asked for stays.
    g["compact"]["auto"] = json!(false);
    assert_eq!(call(&api, "PUT", "/api/settings/general", g).await.0, 200);
    assert_eq!(waiting().await, [file(1)]);
    w.app.compact_arrivals(films[2].id, &[file(2)]);
    assert_eq!(waiting().await, [file(1)], "nothing more while it is off");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_compact_version_is_made_beside_the_original_and_kept_as_a_version_of_it() {
    use serde_json::{json, Value};
    let video = video_or_skip!();
    let w = World::new(Mode::Active).await;
    let api = spool::api::router(w.app.clone());
    let made = |file: i64| {
        let app = w.app.clone();
        async move {
            for _ in 0..600 {
                if !app.compacting(file) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            panic!("the compact version was never finished");
        }
    };

    // A film and one episode of a series, as they would lie in a library.
    let home = w.root.join("Movies/Heat (1995)");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("Heat (1995) [Bluray-1080p].mkv"), &video).unwrap();
    let mut heat = w.movie("Heat", 1995, "tt0113277");
    heat.path = home.to_string_lossy().to_string();
    w.app.db.save_title(&mut heat).unwrap();
    let (mut show, _) = w.series("Example World", 296762, &[(1, 1, "The Original")]);
    let season = w.root.join("TV Shows/Example World/Season 01");
    std::fs::create_dir_all(&season).unwrap();
    std::fs::write(season.join("Example World - S01E01 - The Original.mkv"), &video).unwrap();
    show.path = w.root.join("TV Shows/Example World").to_string_lossy().to_string();
    w.app.db.save_title(&mut show).unwrap();
    w.app.scan_all().await.unwrap();
    let film = w.app.db.files(heat.id).unwrap().remove(0);
    let rate = film.media_info.as_ref().unwrap().bit_rate;

    // At the usual settings so small a file is refused, with the reason.
    let (status, said) = call(&api, "POST", "/api/compact", json!({"title_ids": [heat.id]})).await;
    assert!(status == 409 && said.to_string().contains("small already"), "{status} {said}");
    let (_, page) = call(&api, "GET", &format!("/api/titles/{}", heat.id), Value::Null).await;
    assert_eq!(page["files"][0]["compact"]["can"].as_bool(), Some(false), "{}", page["files"][0]["compact"]);

    // Asked for something a good deal smaller than it is, it is made.
    let mut g = w.app.settings.general();
    g.compact = spool::compact::Compact { height: 120, video_kbps: (rate / 4000).max(10) as u32, audio_kbps: 0, ..Default::default() };
    w.app.settings.set_general(&g).unwrap();
    let (status, said) = call(&api, "POST", "/api/compact", json!({"title_ids": [heat.id, show.id]})).await;
    assert_eq!((status, said["queued"].as_u64()), (200, Some(2)), "{said}");
    let (_, activity) = call(&api, "GET", "/api/activity", Value::Null).await;
    assert!(activity["compact"]["now"].as_array().is_some_and(|now| !now.is_empty()) || activity["compact"]["waiting_count"].as_u64() > Some(0), "Activity shows what is being made: {}", activity["compact"]);
    made(film.id).await;
    let episode_file = w.app.db.episodes(show.id).unwrap()[0].file_id.unwrap();
    made(episode_file).await;
    for _ in 0..100 {
        if w.app.db.get_setting::<Vec<i64>>("compact_waiting").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let notices: Vec<String> = w.app.db.attention(false).unwrap().iter().map(|a| a.message.clone()).collect();
    assert_eq!(tree(&home), ["Heat (1995) [Bluray-1080p] - Compact 120p.mkv", "Heat (1995) [Bluray-1080p].mkv"], "{notices:?}");
    let files = w.app.db.files(heat.id).unwrap();
    let (original, compact) = (files.iter().find(|f| f.compact_of.is_none()).unwrap(), files.iter().find(|f| f.compact_of.is_some()).unwrap());
    assert_eq!((original.id, original.extra, compact.compact_of, compact.extra), (film.id, false, Some(film.id), true), "the original is still the film");
    let info = compact.media_info.as_ref().unwrap();
    // (Not its size: two seconds of test pattern is already smaller than any encoder will go.)
    assert!(info.height == 120 && info.video_codec.to_lowercase().contains("265") && compact.size > 0, "{}x{} {} {} bytes", info.width, info.height, info.video_codec, compact.size);
    assert_eq!(std::fs::read(home.join("Heat (1995) [Bluray-1080p].mkv")).unwrap(), video, "the original is untouched");
    assert!(w.app.db.history(Some(heat.id), 10).unwrap().iter().any(|h| h.kind == "compact_made"));
    // And Activity lists it among what has finished, with what it came to.
    let (_, activity) = call(&api, "GET", "/api/activity", Value::Null).await;
    let done = activity["done"].as_array().unwrap().iter().find(|d| d["kind"] == "compact_made" && d["title"] == "Heat").cloned().unwrap_or_else(|| panic!("{}", activity["done"]));
    assert!(done["data"]["bytes"].as_u64() > Some(0) && done["data"]["from_bytes"].as_u64() == Some(video.len() as u64), "{done}");
    assert!(w.app.db.get_setting::<Vec<i64>>("compact_waiting").is_empty(), "nothing is left written down once the line is done");

    // An episode's compact version is a version of its file; the episode's file is still the original.
    assert_eq!(tree(&season), ["Example World - S01E01 - The Original - Compact 120p.mkv", "Example World - S01E01 - The Original.mkv"], "{notices:?}");
    assert_eq!(w.app.db.episodes(show.id).unwrap()[0].file_id, Some(episode_file));

    // The library says which titles have one, and a second is refused.
    let (_, listed) = call(&api, "GET", "/api/titles", Value::Null).await;
    let row = listed.as_array().unwrap().iter().find(|t| t["id"] == heat.id).unwrap().clone();
    assert_eq!(row["compact_count"].as_u64(), Some(1), "{row}");
    let (status, said) = call(&api, "POST", "/api/compact", json!({"title_ids": [heat.id]})).await;
    assert!(status == 409 && said.to_string().contains("already has a compact version"), "{status} {said}");
    let (_, report) = call(&api, "GET", "/api/compact", Value::Null).await;
    assert_eq!((report["made"]["files"].as_u64(), report["would"]["files"].as_u64()), (Some(2), Some(0)), "{report}");

    // Players are offered it as a version, of the film and of the episode.
    let mut g = w.app.settings.general();
    g.players_enabled = true;
    w.app.settings.set_general(&g).unwrap();
    let jf = spool::jellyfin::router(w.app.clone());
    let (_, _, body) = player(&jf, "POST", "/Users/AuthenticateByName", None, json!({"Username": "spool", "Pw": ""})).await;
    let token = serde_json::from_slice::<Value>(&body).unwrap()["AccessToken"].as_str().unwrap().to_string();
    for kind in ["Movie", "Episode"] {
        let (_, _, body) = player(&jf, "GET", &format!("/Items?Recursive=true&IncludeItemTypes={kind}&Fields=MediaSources"), Some(&token), Value::Null).await;
        let item = serde_json::from_slice::<Value>(&body).unwrap()["Items"][0].clone();
        let names: Vec<String> = item["MediaSources"].as_array().unwrap().iter().map(|s| s["Name"].as_str().unwrap_or_default().to_string()).collect();
        assert!(names.len() == 2 && names[1] == "Compact" && names[0] != "Compact", "{kind}: {names:?}");
        let small = &item["MediaSources"][1];
        let (status, _, played) = player(&jf, "GET", &format!("/Videos/{}/stream?MediaSourceId={}&Static=true", item["Id"].as_str().unwrap(), small["Id"].as_str().unwrap()), Some(&token), Value::Null).await;
        assert_eq!((status, played.len() as u64), (200, small["Size"].as_u64().unwrap()), "{kind}: the compact version is what is sent");
    }

    // Reading the folders again changes nothing; and with its record lost, a compact version
    // is known again by its name and tied to the file it was made from.
    w.app.scan_all().await.unwrap();
    assert_eq!(w.app.db.files(heat.id).unwrap().len(), 2);
    w.app.db.with(|c| c.execute("DELETE FROM files WHERE id = ?1", [compact.id])).unwrap();
    w.app.scan_all().await.unwrap();
    let again = w.app.db.files(heat.id).unwrap().into_iter().find(|f| f.rel_path.contains("Compact")).unwrap();
    assert_eq!((again.compact_of, again.extra), (Some(film.id), true));
    assert_eq!(w.app.db.episodes(show.id).unwrap()[0].file_id, Some(episode_file), "a scan does not take the compact version for the episode's file");

    // Keeping only the compact version lets the original go, and the compact one is then the film.
    let (status, said) = call(&api, "POST", &format!("/api/files/{}/keep-compact", again.id), Value::Null).await;
    assert_eq!(status, 200, "{said}");
    assert_eq!(tree(&home), ["Heat (1995) [Bluray-1080p] - Compact 120p.mkv"]);
    let files = w.app.db.files(heat.id).unwrap();
    assert_eq!((files.len(), files[0].compact_of, files[0].extra), (1, None, false));
    w.app.scan_all().await.unwrap();
    let files = w.app.db.files(heat.id).unwrap();
    assert_eq!((files.len(), files[0].id, files[0].compact_of, files[0].extra), (1, again.id, None, false), "and it stays the film when the folder is read again");

    // When an episode's file goes, the compact version made from it goes too.
    let small = w.app.db.files(show.id).unwrap().into_iter().find(|f| f.compact_of.is_some()).unwrap();
    assert_eq!(call(&api, "DELETE", &format!("/api/files/{episode_file}"), Value::Null).await.0, 200);
    assert!(w.app.db.file(small.id).unwrap().is_none() && !season.join("Example World - S01E01 - The Original - Compact 120p.mkv").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_title_on_a_location_spool_only_reads_gets_its_compact_version_somewhere_else() {
    use serde_json::{json, Value};
    let video = video_or_skip!();
    let w = World::new(Mode::Active).await;
    let api = spool::api::router(w.app.clone());
    let (shelf, vault) = (w.root.join("Shelf/films"), w.root.join("Vault/films"));
    for d in [&shelf, &vault] {
        std::fs::create_dir_all(d).unwrap();
    }
    let mut g = serde_json::to_value(w.app.settings.general()).unwrap();
    g["locations"] = json!([{"name": "Shelf", "movie_root": shelf, "manage": true}, {"name": "Vault", "movie_root": vault, "manage": false}]);
    let (status, said) = call(&api, "PUT", "/api/settings/general", g).await;
    assert_eq!(status, 200, "{said}");
    let kept = vault.join("Alpha (2001)");
    std::fs::create_dir_all(&kept).unwrap();
    std::fs::write(kept.join("Alpha (2001).mkv"), &video).unwrap();
    let mut alpha = w.movie("Alpha", 2001, "tt0000001");
    alpha.path = kept.to_string_lossy().to_string();
    w.app.db.save_title(&mut alpha).unwrap();
    w.app.scan_all().await.unwrap();
    let film = w.app.db.files(alpha.id).unwrap().remove(0);
    let rate = film.media_info.as_ref().unwrap().bit_rate;
    let mut g = w.app.settings.general();
    g.compact = spool::compact::Compact { height: 120, video_kbps: (rate / 4000).max(10) as u32, audio_kbps: 0, ..Default::default() };
    w.app.settings.set_general(&g).unwrap();

    // With nowhere chosen for them, there is to be none, and the reason says what to do.
    let (status, said) = call(&api, "POST", "/api/compact", json!({"title_ids": [alpha.id]})).await;
    assert!(status == 409 && said.to_string().contains("no location is chosen for compact versions"), "{status} {said}");

    // With a location chosen, it is made there, and that folder becomes one more place the title is held.
    let mut g = w.app.settings.general();
    g.compact.location = 1;
    w.app.settings.set_general(&g).unwrap();
    let (status, said) = call(&api, "POST", "/api/compact", json!({"file_ids": [film.id]})).await;
    assert_eq!((status, said["queued"].as_u64()), (200, Some(1)), "{said}");
    for _ in 0..600 {
        if !w.app.compacting(film.id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let notices: Vec<String> = w.app.db.attention(false).unwrap().iter().map(|a| a.message.clone()).collect();
    assert_eq!(tree(&shelf), ["Alpha (2001)/Alpha (2001) - Compact 120p.mkv"], "{notices:?}");
    assert_eq!(tree(&vault), ["Alpha (2001)/Alpha (2001).mkv"], "nothing is written where Spool only reads");
    let now = w.app.db.title(alpha.id).unwrap().unwrap();
    assert_eq!(now.also, [shelf.join("Alpha (2001)").to_string_lossy().to_string()]);
    let compact = w.app.db.files(alpha.id).unwrap().into_iter().find(|f| f.compact_of.is_some()).unwrap();
    assert_eq!(compact.base.as_deref(), now.also.first().map(String::as_str));
    w.app.scan_all().await.unwrap();
    assert_eq!(w.app.db.files(alpha.id).unwrap().len(), 2, "reading both folders again finds the same two files");

    // Taken away again, the folder that held nothing else is no longer one of the title's.
    assert_eq!(call(&api, "DELETE", &format!("/api/files/{}", compact.id), Value::Null).await.0, 200);
    assert!(tree(&shelf).is_empty() && w.app.db.title(alpha.id).unwrap().unwrap().also.is_empty());
    assert!(kept.join("Alpha (2001).mkv").exists());
    let _ = Value::Null;
}

#[tokio::test]
async fn a_clip_is_cut_from_a_film_or_an_episode_and_can_be_played_saved_and_removed() {
    use serde_json::{json, Value};
    use tower::ServiceExt;
    let video = video_or_skip!();
    let w = World::new(Mode::Active).await;
    let api = spool::api::router(w.app.clone());
    let ready = |id: i64| {
        let app = w.app.clone();
        async move {
            for _ in 0..600 {
                match app.db.clip(id).unwrap() {
                    Some(c) if c.state == "making" => tokio::time::sleep(Duration::from_millis(50)).await,
                    Some(c) => return c,
                    None => panic!("the clip went"),
                }
            }
            panic!("the clip was never finished");
        }
    };
    let raw = |uri: String, range: Option<&'static str>| {
        let api = api.clone();
        async move {
            let mut req = axum::http::Request::builder().uri(uri);
            if let Some(r) = range {
                req = req.header("range", r);
            }
            let resp = api.oneshot(req.body(axum::body::Body::empty()).unwrap()).await.unwrap();
            let (status, headers) = (resp.status().as_u16(), resp.headers().clone());
            (status, headers, axum::body::to_bytes(resp.into_body(), 64 << 20).await.unwrap().to_vec())
        }
    };

    let home = w.root.join("Movies/Heat (1995)");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("Heat (1995) [Bluray-1080p].mkv"), &video).unwrap();
    std::fs::write(home.join("Heat (1995) [Bluray-1080p].en.srt"), "1\n00:00:00,600 --> 00:00:01,400\n<i>Hello</i> there\n").unwrap();
    let mut heat = w.movie("Heat", 1995, "tt0113277");
    heat.path = home.to_string_lossy().to_string();
    w.app.db.save_title(&mut heat).unwrap();
    let (mut show, _) = w.series("Example World", 296762, &[(1, 1, "The Original")]);
    let season = w.root.join("TV Shows/Example World/Season 01");
    std::fs::create_dir_all(&season).unwrap();
    std::fs::write(season.join("Example World - S01E01 - The Original.mkv"), &video).unwrap();
    show.path = w.root.join("TV Shows/Example World").to_string_lossy().to_string();
    w.app.db.save_title(&mut show).unwrap();
    w.app.scan_all().await.unwrap();
    // The film's sound says no language, so by itself it gets no subtitles; asked, it gets the file beside it.
    let (_, asked) = call(&api, "POST", "/api/clips", json!({"title_id": heat.id, "start": 0, "seconds": 1.5, "subtitles": "on"})).await;
    let with_words = ready(asked["id"].as_i64().unwrap_or_else(|| panic!("{asked}"))).await;
    assert_eq!((with_words.state.as_str(), with_words.subtitles_shown.as_str()), ("ready", "English"), "{}", with_words.error);
    assert!((with_words.seconds - 1.5).abs() < 0.2, "it runs as long as asked, subtitles or not: {}", with_words.seconds);
    let frame = |at: f64, of: std::path::PathBuf| async move {
        let out = std::process::Command::new("ffmpeg").args(["-v", "error", "-ss", &at.to_string(), "-i"]).arg(&of).args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "pipe:1"]).output().unwrap();
        out.stdout
    };
    // The foot of the picture differs while the line is up, and not before it.
    let plain = w.app.clip_video(with_words.id);
    let (before, during) = (frame(0.2, plain.clone()).await, frame(1.0, plain.clone()).await);
    assert_eq!(before.len(), 320 * 240);
    let foot = |f: &[u8]| f[200 * 320..230 * 320].iter().filter(|p| **p > 235).count();
    assert!(foot(&during) > foot(&before) + 20, "white letters appear: {} then {}", foot(&before), foot(&during));
    assert!(!w.app.data_dir.join(format!("clips/.{}-subtitles", with_words.id)).exists(), "the drawings are cleared away");
    call(&api, "DELETE", &format!("/api/clips/{}", with_words.id), Value::Null).await;

    // What cannot be cut is refused with the reason.
    for (body, why) in [
        (json!({"title_id": heat.id, "start": "soon"}), "not a time"),
        (json!({"title_id": heat.id, "start": "5m"}), "nothing at 5:00"),
        (json!({"title_id": heat.id, "start": 0, "seconds": 900}), "at most 10 minutes"),
        (json!({"title_id": show.id, "start": 0}), "which episode"),
        (json!({"title_id": show.id, "season": 1, "episode": 9, "start": 0}), "no S01E09"),
        (json!({"title_id": show.id, "season": 1, "episode": 1, "start": 0, "subtitles": "french"}), "no french subtitles"),
    ] {
        let (status, said) = call(&api, "POST", "/api/clips", body).await;
        assert!(status == 400 && said.to_string().contains(why), "{why}: {status} {said}");
    }

    // A second of the film, from half a second in, named by the title and the time.
    let (status, asked) = call(&api, "POST", "/api/clips", json!({"title_id": heat.id, "start": "0.5", "seconds": 1})).await;
    assert_eq!(status, 200, "{asked}");
    assert_eq!((asked["state"].as_str(), asked["name"].as_str(), asked["from"].as_str()), (Some("making"), Some("Heat at 0:01"), Some("Heat (1995)")), "{asked}");
    let id = asked["id"].as_i64().unwrap();
    let clip = ready(id).await;
    assert_eq!(clip.state, "ready", "{}", clip.error);
    assert!((clip.seconds - 1.0).abs() < 0.2 && clip.size > 0 && clip.height == 240, "{clip:?}");
    let info = w.app.probe(&w.app.clip_video(id), "").await.unwrap().unwrap();
    assert!(info.video_codec.to_lowercase().contains("264") && info.audio_codec.to_lowercase().contains("aac"), "{} {}", info.video_codec, info.audio_codec);
    assert!(w.app.clip_still(id).is_file(), "a still is kept to show it by");

    // It plays in parts, and is saved under its name.
    let (status, headers, bytes) = raw(format!("/api/clips/{id}/video"), Some("bytes=0-99")).await;
    assert_eq!((status, bytes.len(), headers["content-type"].to_str().unwrap()), (206, 100, "video/mp4"));
    let (status, headers, _) = raw(format!("/api/clips/{id}/video?download"), None).await;
    assert_eq!(status, 200);
    assert!(headers["content-disposition"].to_str().unwrap().contains("filename=\"Heat at 0 01.mp4\""), "{:?}", headers["content-disposition"]);
    // Its link plays it to whoever it is sent to, and a link made up plays nothing.
    let link = w.app.db.clip(id).unwrap().unwrap().link;
    assert!(link.len() >= 32, "{link}");
    let (status, _, bytes) = raw(format!("/clip/{link}.mp4"), None).await;
    assert_eq!((status, bytes.len() as u64), (200, clip.size));
    assert_eq!(raw("/clip/0000.mp4".into(), None).await.0, 404);
    let (status, headers, _) = raw(format!("/api/clips/{id}/still"), None).await;
    assert_eq!((status, headers["content-type"].to_str().unwrap()), (200, "image/jpeg"));

    // An episode, by its season and number, with a name of its own; past the end it stops at the end.
    let (_, asked) = call(&api, "POST", "/api/clips", json!({"title_id": show.id, "season": 1, "episode": 1, "start": "1s", "seconds": "30", "name": "The bit"})).await;
    let episode_clip = ready(asked["id"].as_i64().unwrap()).await;
    assert_eq!((episode_clip.state.as_str(), episode_clip.name.as_str(), episode_clip.from.as_str()), ("ready", "The bit", "Example World S01E01 · The Original"), "{}", episode_clip.error);
    assert!(episode_clip.seconds <= 1.2, "{}", episode_clip.seconds);

    // Listed newest first; removing one takes its files with it. The film is untouched.
    let (_, listed) = call(&api, "GET", "/api/clips", Value::Null).await;
    assert_eq!(listed.as_array().unwrap().iter().map(|c| c["id"].as_i64().unwrap()).collect::<Vec<_>>(), [episode_clip.id, id]);
    assert_eq!(listed[0]["yours"].as_bool(), Some(true));
    let (status, _) = call(&api, "DELETE", &format!("/api/clips/{id}"), Value::Null).await;
    assert_eq!(status, 200);
    assert!(!w.app.clip_video(id).exists() && !w.app.clip_still(id).exists());
    assert_eq!(raw(format!("/api/clips/{id}/video"), None).await.0, 404);
    assert_eq!(std::fs::read(home.join("Heat (1995) [Bluray-1080p].mkv")).unwrap(), video);

    // A clip outlives the title it was cut from.
    w.app.db.delete_title(show.id).unwrap();
    let kept = w.app.db.clip(episode_clip.id).unwrap().unwrap();
    assert_eq!((kept.title_id, kept.state.as_str()), (None, "ready"));
}

#[tokio::test]
async fn subtitles_inside_a_file_are_read_for_the_clip_in_their_own_time() {
    let video = video_or_skip!();
    let dir = tempfile::tempdir().unwrap();
    let (plain, srt, with) = (dir.path().join("v.mkv"), dir.path().join("v.srt"), dir.path().join("with.mkv"));
    std::fs::write(&plain, &video).unwrap();
    std::fs::write(&srt, "1\n00:00:00,500 --> 00:00:01,600\nAlready up\n\n2\n00:00:01,700 --> 00:00:01,900\n{\\an8}<b>Next</b>\n").unwrap();
    let ok = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&plain)
        .arg("-i")
        .arg(&srt)
        .args(["-map", "0", "-map", "1", "-c", "copy", "-c:s", "srt", "-metadata:s:s:0", "language=eng"])
        .arg(&with)
        .status()
        .unwrap();
    assert!(ok.success());
    let stop = std::sync::atomic::AtomicBool::new(false);
    let tool = spool::subs::ffmpeg_installed().unwrap();
    // From a second in, for half a second: the line already up is there from the start, the next where it falls.
    let cues = spool::captions::cues(&tool, &with, &spool::captions::Source::Text(2), 1.0, 0.95, &stop).await.unwrap();
    let got: Vec<(i64, i64, &str)> = cues.iter().map(|c| ((c.start * 100.0).round() as i64, (c.end * 100.0).round() as i64, c.text.as_str())).collect();
    assert_eq!(got, [(0, 60, "Already up"), (70, 90, "Next")]);
    // Called off, it stops.
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(spool::captions::cues(&tool, &with, &spool::captions::Source::Text(2), 1.0, 0.95, &stop).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn what_is_said_across_the_library_is_found_with_the_lines_around_it() {
    use serde_json::{json, Value};
    let video = video_or_skip!();
    let w = World::new(Mode::Active).await;
    // No model to be had here: searches go by the words alone.
    w.app.dialogue.take_model_from("http://127.0.0.1:9");
    let api = spool::api::router(w.app.clone());
    let srt = |lines: &[(&str, &str, &str)]| lines.iter().enumerate().map(|(i, (a, b, t))| format!("{}\n{a} --> {b}\n{t}\n", i + 1)).collect::<Vec<_>>().join("\n");
    let film = |name: &str, year: u32, imdb: &str| {
        let home = w.root.join(format!("Movies/{name} ({year})"));
        std::fs::create_dir_all(&home).unwrap();
        let mut t = w.movie(name, year, imdb);
        t.path = home.to_string_lossy().to_string();
        w.app.db.save_title(&mut t).unwrap();
        (t, home.join(format!("{name} ({year}) [Bluray-1080p].mkv")))
    };
    // One with its subtitles inside, one with them beside it, one with none to read.
    let (city, city_file) = film("Dark City", 1998, "tt0118929");
    let (inside, plain) = (w.root.join("inside.srt"), w.root.join("plain.mkv"));
    std::fs::write(&plain, &video).unwrap();
    std::fs::write(
        &inside,
        srt(&[
            ("00:00:01,000", "00:00:02,000", "Where am I?"),
            ("00:00:02,500", "00:00:03,500", "Nobody knows."),
            ("00:05:00,000", "00:05:02,000", "<i>Wake up.</i>"),
            ("00:05:02,500", "00:05:04,000", "They come when you sleep."),
            ("00:05:04,500", "00:05:06,000", "Shell Beach."),
        ]),
    )
    .unwrap();
    let ok = std::process::Command::new("ffmpeg").args(["-v", "error", "-y", "-i"]).arg(&plain).arg("-i").arg(&inside).args(["-map", "0", "-map", "1", "-c", "copy", "-c:s", "srt", "-metadata:s:s:0", "language=eng"]).arg(&city_file).status().unwrap();
    assert!(ok.success());
    let (heat, heat_file) = film("Heat", 1995, "tt0113277");
    std::fs::write(&heat_file, &video).unwrap();
    let beside = heat_file.with_file_name("Heat (1995) [Bluray-1080p].en.srt");
    std::fs::write(&beside, srt(&[("00:10:00,000", "00:10:02,000", "Don't let yourself get attached."), ("01:00:00,000", "01:00:02,000", "The proxy has awakened.")])).unwrap();
    let (_, quiet_file) = film("Quiet", 2001, "tt0000001");
    std::fs::write(&quiet_file, &video).unwrap();
    w.app.scan_all().await.unwrap();
    for _ in 0..5 {
        if !w.app.dialogue_round(false).await.unwrap() {
            break;
        }
    }
    let c = w.app.dialogue.coverage();
    assert_eq!((c.files, c.read, c.unreadable, c.lines, c.can_fetch), (3, 2, 1, 7, false), "{c:?}");
    assert!(c.problems.iter().any(|p| p.contains("model")), "without the model it says so, and goes by the words: {c:?}");

    // The words by their stems, with the lines either side; the tags of how to show them gone.
    let found = w.app.search_dialogue("the awakening", 10, None).await.unwrap();
    assert!(!found.by_meaning);
    assert_eq!(found.moments.len(), 1, "{found:?}");
    let m = &found.moments[0];
    assert_eq!((m.from.as_str(), m.start, m.lines[0].text.as_str(), m.before.len(), m.words), ("Heat (1995)", 3600.0, "The proxy has awakened.", 1, true));
    let found = w.app.search_dialogue("wake up; Shell Beach", 10, None).await.unwrap();
    let said: Vec<(&str, &str)> = found.moments.iter().map(|m| (m.from.as_str(), m.lines[0].text.as_str())).collect();
    assert_eq!(said, [("Dark City (1998)", "Wake up.")], "lines said together are one moment: {found:?}");
    assert_eq!((found.moments[0].start, found.moments[0].end, found.moments[0].before.len()), (300.0, 306.0, 2));
    assert!(w.app.search_dialogue("wake up", 10, Some(heat.id)).await.unwrap().moments.is_empty(), "only in the title asked");
    let across = w.app.search_dialogue("sleep beach", 10, Some(city.id)).await.unwrap();
    assert_eq!(across.moments.len(), 1, "words on lines said together are found together: {across:?}");

    // Over the API, for anyone signed in to choose clips from.
    let (status, got) = call(&api, "GET", "/api/dialogue?q=attached", Value::Null).await;
    assert_eq!((status, got["moments"][0]["title_id"].as_i64(), got["coverage"]["read"].as_u64()), (200, Some(heat.id), Some(2)), "{got}");
    let (status, _) = call(&api, "GET", "/api/dialogue?q=", Value::Null).await;
    assert_eq!(status, 400);

    // Subtitles taken away are forgotten; a title gone from the library goes from the index.
    std::fs::remove_file(&beside).unwrap();
    for f in w.app.db.files(heat.id).unwrap() {
        w.app.find_sidecars(f.id).unwrap();
    }
    w.app.dialogue_round(false).await.unwrap();
    assert!(w.app.search_dialogue("attached", 10, None).await.unwrap().moments.is_empty());
    let (status, _) = call(&api, "DELETE", &format!("/api/titles/{}", city.id), json!({})).await;
    assert_eq!(status, 200);
    w.app.dialogue_round(false).await.unwrap();
    let c = w.app.dialogue.coverage();
    assert_eq!((c.files, c.read, c.lines), (2, 0, 0), "{c:?}");
}

/// With the model at hand (SPOOL_TEST_MODEL, a folder of its files), what is meant is found
/// when it is put another way.
#[tokio::test(flavor = "multi_thread")]
async fn what_is_meant_is_found_when_it_is_put_another_way() {
    let Ok(model) = std::env::var("SPOOL_TEST_MODEL") else {
        eprintln!("SPOOL_TEST_MODEL not set; skipping");
        return;
    };
    let video = video_or_skip!();
    let w = World::new(Mode::Active).await;
    w.app.dialogue.take_model_from(&model);
    let home = w.root.join("Movies/Dark City (1998)");
    std::fs::create_dir_all(&home).unwrap();
    let mut t = w.movie("Dark City", 1998, "tt0118929");
    t.path = home.to_string_lossy().to_string();
    w.app.db.save_title(&mut t).unwrap();
    std::fs::write(home.join("Dark City (1998).mkv"), &video).unwrap();
    let lines = ["Pass the salt, please.", "Whatever you do, don't fall asleep.", "The bank closes at five.", "I keep thinking about the ocean.", "Nobody has a key to that door."];
    let srt: String = lines.iter().enumerate().map(|(i, l)| format!("{}\n00:{:02}:00,000 --> 00:{:02}:02,000\n{l}\n\n", i + 1, i * 5, i * 5)).collect();
    std::fs::write(home.join("Dark City (1998).en.srt"), srt).unwrap();
    w.app.scan_all().await.unwrap();
    while w.app.dialogue_round(false).await.unwrap() {}
    let c = w.app.dialogue.coverage();
    assert_eq!((c.read, c.passages, c.placed), (1, 5, 5), "{c:?}");
    let found = w.app.search_dialogue("stay awake", 10, None).await.unwrap();
    assert!(found.by_meaning);
    assert_eq!(found.moments.first().map(|m| m.lines[0].text.as_str()), Some("Whatever you do, don't fall asleep."), "{found:?}");
    assert!(!found.moments[0].words);
    assert!(found.moments.iter().all(|m| m.lines[0].text != "The bank closes at five."), "what means nothing like it is left out: {found:?}");
}
