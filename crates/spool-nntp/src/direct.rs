//! Unpacking while downloading.
//!
//! When a post is a set of RAR volumes packed without compression, each volume's payload is
//! copied to the finished file as soon as the volume is complete, checked against the checksum
//! the archive carries for that part, and the volume is then deleted. Extraction costs no extra
//! time at the end, and the download never needs room for both the archive and its contents.
//!
//! A volume is only deleted after its part has been verified. What is not payload (headers,
//! recovery records) is kept aside, so if anything later turns out wrong the deleted volumes are
//! put back exactly and the job goes down the ordinary verify, repair and extract path.

use crate::engine::Job;
use crate::rar::{self, Entry};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::LazyLock;

const WORK: &str = "_direct";

static RAR_PART: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)^(?P<base>.+)\.part(?P<n>\d+)\.rar$").unwrap());
static RAR_OLD: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)^(?P<base>.+)\.(?P<ext>rar|[r-z]\d\d)$").unwrap());

/// The job's RAR volumes as indexes into its file list, in archive order. None unless the post
/// is one unambiguous set.
pub(crate) fn plan(job: &Job) -> Option<Vec<usize>> {
    let names: Vec<Option<String>> = job.nzb.files.iter().map(|f| f.subject_filename()).collect();
    let mut set: Vec<(String, u32, usize)> = vec![];
    for (i, name) in names.iter().enumerate() {
        let Some(n) = name else { continue };
        if let Some(c) = RAR_PART.captures(n) {
            set.push((c["base"].to_lowercase(), c["n"].parse().ok()?, i));
        } else if let Some(c) = RAR_OLD.captures(n) {
            // name.rar comes first, then .r00, .r01 ... .s00 ...
            let ext = c["ext"].to_lowercase();
            let order = if ext == "rar" { 0 } else { 1 + (ext.as_bytes()[0] - b'r') as u32 * 100 + ext[1..].parse::<u32>().ok()? };
            set.push((c["base"].to_lowercase(), order, i));
        }
    }
    if set.is_empty() || set.iter().any(|s| s.0 != set[0].0) {
        return None;
    }
    set.sort_by_key(|s| s.1);
    // Two files claiming the same place means the names cannot be trusted.
    if set.windows(2).any(|w| w[0].1 == w[1].1) {
        return None;
    }
    Some(set.into_iter().map(|s| s.2).collect())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Piece {
    /// Bytes kept in the volume's side file.
    Kept { len: u64 },
    /// Bytes that now live in an output file.
    Data { out: usize, at: u64, len: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Removed {
    name: String,
    pieces: Vec<Piece>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Output {
    /// Path inside the archive, already made safe.
    name: String,
    written: u64,
    /// Running checksum of everything written so far.
    crc: u32,
    complete: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    /// Volumes handled, counted along the plan.
    done: usize,
    outputs: Vec<Output>,
    /// The output a file split across volumes is still being written to.
    open: Option<usize>,
    removed: Vec<Removed>,
}

pub(crate) enum Outcome {
    /// Everything is unpacked into the job folder and the volumes are gone.
    Unpacked,
    /// Not finished; state is saved and a later run carries on.
    Interrupted,
    /// Not possible for this job. Any deleted volumes have been put back.
    GaveUp(String),
}

fn work(dir: &Path) -> PathBuf {
    dir.join(WORK)
}

fn load(dir: &Path) -> State {
    std::fs::read(work(dir).join("state.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(dir: &Path, state: &State) -> std::io::Result<()> {
    let tmp = work(dir).join("state.json.tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(&serde_json::to_vec(state).map_err(std::io::Error::other)?)?;
    f.sync_data()?;
    std::fs::rename(tmp, work(dir).join("state.json"))
}

/// A path from inside an archive, kept inside the folder it is unpacked to.
fn safe(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            p if p.contains('\0') => return None,
            p => out.push(p),
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn out_path(dir: &Path, name: &str) -> PathBuf {
    work(dir).join("out").join(name)
}

fn copy_range(src: &mut File, at: u64, len: u64, dst: &mut File, mut each: impl FnMut(&[u8])) -> std::io::Result<()> {
    src.seek(SeekFrom::Start(at))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len;
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        src.read_exact(&mut buf[..want])?;
        dst.write_all(&buf[..want])?;
        each(&buf[..want]);
        left -= want as u64;
    }
    Ok(())
}

/// Take one complete volume into the outputs. On success the volume has been deleted.
fn take_volume(dir: &Path, state: &mut State, file_name: &str) -> Result<(), String> {
    let path = dir.join(file_name);
    let vol = rar::scan(&path)?;
    let files: Vec<&Entry> = vol.entries.iter().filter(|e| !e.is_dir).collect();
    if files.is_empty() && state.done == 0 {
        return Err("the first volume holds no files".into());
    }
    for e in &vol.entries {
        if e.encrypted {
            return Err("the archive is password protected".into());
        }
        if !e.is_dir && (!e.stored || e.crc.is_none()) {
            return Err("the archive is compressed".into());
        }
    }
    let io = |e: std::io::Error| format!("unpacking {file_name}: {e}");
    std::fs::create_dir_all(work(dir).join("out")).map_err(io)?;
    let before = state.clone();
    let mut src = File::open(&path).map_err(io)?;
    let mut pieces: Vec<Piece> = vec![];
    let mut kept = File::create(work(dir).join(format!("{}.kept", state.done))).map_err(io)?;
    let mut pos = 0u64;
    let result: Result<(), String> = (|| {
        for e in &vol.entries {
            if e.is_dir {
                if let Some(p) = safe(&e.name) {
                    std::fs::create_dir_all(out_path(dir, &p.to_string_lossy())).map_err(io)?;
                }
                continue;
            }
            let name = safe(&e.name).ok_or("the archive holds a file with an unsafe name")?.to_string_lossy().to_string();
            let idx = if e.split_before {
                let i = state.open.ok_or("a volume continues a file that was never started")?;
                if state.outputs[i].name != name {
                    return Err("the volumes are out of order".into());
                }
                i
            } else {
                if state.open.is_some() {
                    return Err("a file was left unfinished by the previous volume".into());
                }
                if state.outputs.iter().any(|o| o.name == name) {
                    return Err("the archive holds the same file twice".into());
                }
                state.outputs.push(Output { name: name.clone(), written: 0, crc: 0, complete: false });
                state.outputs.len() - 1
            };
            // What sits between the last payload and this one is kept for putting the volume back.
            if e.data_offset < pos {
                return Err("the volume's headers overlap".into());
            }
            copy_range(&mut src, pos, e.data_offset - pos, &mut kept, |_| {}).map_err(io)?;
            pieces.push(Piece::Kept { len: e.data_offset - pos });

            let target = out_path(dir, &name);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            let mut out = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&target).map_err(io)?;
            let at = state.outputs[idx].written;
            // A crash can leave more on disk than the saved state knows about.
            out.set_len(at).map_err(io)?;
            out.seek(SeekFrom::Start(at)).map_err(io)?;
            let mut part = crc32fast::Hasher::new();
            let mut whole = crc32fast::Hasher::new_with_initial_len(state.outputs[idx].crc, at);
            copy_range(&mut src, e.data_offset, e.data_len, &mut out, |b| {
                part.update(b);
                whole.update(b);
            })
            .map_err(io)?;
            out.sync_data().map_err(io)?;
            let (part, whole) = (part.finalize(), whole.finalize());
            // The archive stores a checksum of this volume's share while the file continues, and
            // of the whole file where it ends.
            let good = if e.split_after { e.crc == Some(part) } else { e.crc == Some(whole) };
            if !good {
                return Err(format!("{file_name} does not match its checksum"));
            }
            pieces.push(Piece::Data { out: idx, at, len: e.data_len });
            let o = &mut state.outputs[idx];
            o.written = at + e.data_len;
            o.crc = whole;
            o.complete = !e.split_after;
            state.open = if e.split_after { Some(idx) } else { None };
            pos = e.data_offset + e.data_len;
        }
        copy_range(&mut src, pos, vol.len - pos, &mut kept, |_| {}).map_err(io)?;
        pieces.push(Piece::Kept { len: vol.len - pos });
        kept.sync_data().map_err(io)?;
        Ok(())
    })();
    if let Err(why) = result {
        // Leave everything as it was before this volume: the volume itself is untouched.
        for o in &before.outputs {
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(out_path(dir, &o.name)) {
                let _ = f.set_len(o.written);
            }
        }
        for o in state.outputs.iter().skip(before.outputs.len()) {
            let _ = std::fs::remove_file(out_path(dir, &o.name));
        }
        let _ = std::fs::remove_file(work(dir).join(format!("{}.kept", before.done)));
        *state = before;
        return Err(why);
    }
    state.removed.push(Removed { name: file_name.to_string(), pieces });
    state.done += 1;
    // Saved before the volume goes: after a crash the state says it is done, and it is deleted then.
    save(dir, state).map_err(io)?;
    std::fs::remove_file(&path).map_err(io)?;
    Ok(())
}

/// Put every deleted volume back, byte for byte, and remove the working folder.
fn restore(dir: &Path, state: &State) -> Result<(), String> {
    for (i, r) in state.removed.iter().enumerate() {
        let io = |e: std::io::Error| format!("putting {} back: {e}", r.name);
        let target = dir.join(&r.name);
        let tmp = dir.join(format!("{}.restoring", r.name));
        let mut out = File::create(&tmp).map_err(io)?;
        let mut kept = File::open(work(dir).join(format!("{i}.kept"))).map_err(io)?;
        let mut kept_at = 0u64;
        for p in &r.pieces {
            match p {
                Piece::Kept { len } => {
                    copy_range(&mut kept, kept_at, *len, &mut out, |_| {}).map_err(io)?;
                    kept_at += len;
                }
                Piece::Data { out: o, at, len } => {
                    let mut src = File::open(out_path(dir, &state.outputs[*o].name)).map_err(io)?;
                    copy_range(&mut src, *at, *len, &mut out, |_| {}).map_err(io)?;
                }
            }
        }
        out.sync_data().map_err(io)?;
        std::fs::rename(&tmp, &target).map_err(io)?;
    }
    std::fs::remove_dir_all(work(dir)).map_err(|e| e.to_string())
}

/// Move the unpacked files into the job folder.
fn finish(dir: &Path, state: &State) -> Result<(), String> {
    let out = work(dir).join("out");
    for entry in std::fs::read_dir(&out).map_err(|e| e.to_string())?.flatten() {
        let mut target = dir.join(entry.file_name());
        if target.exists() {
            target = dir.join(format!("unpacked_{}", entry.file_name().to_string_lossy()));
        }
        std::fs::rename(entry.path(), target).map_err(|e| e.to_string())?;
    }
    let _ = state;
    std::fs::remove_dir_all(work(dir)).map_err(|e| e.to_string())
}

/// Follow a download, taking each volume as it completes. Returns when the archive is fully
/// unpacked, when `stop` is raised, or when unpacking this way turns out not to be possible.
/// `downloaded` says that no more data is coming.
pub(crate) async fn run(job: Arc<Job>, volumes: Vec<usize>, stop: Arc<AtomicBool>, downloaded: Arc<AtomicBool>) -> Outcome {
    let dir = job.dir.clone();
    let d = dir.clone();
    let mut state = tokio::task::spawn_blocking(move || load(&d)).await.unwrap_or_default();
    let give_up = |state: State, why: String| {
        let d = dir.clone();
        async move {
            let put_back = tokio::task::spawn_blocking(move || if work(&d).exists() { restore(&d, &state) } else { Ok(()) }).await.unwrap_or_else(|e| Err(e.to_string()));
            match put_back {
                Ok(()) => Outcome::GaveUp(why),
                Err(e) => Outcome::GaveUp(format!("{why}; and the volumes could not be put back: {e}")),
            }
        }
    };
    loop {
        if state.done == volumes.len() {
            if state.open.is_some() || state.outputs.iter().any(|o| !o.complete) || state.outputs.is_empty() {
                return give_up(state, "the archive ends before its last file does".into()).await;
            }
            let (d, s) = (dir.clone(), state.clone());
            return match tokio::task::spawn_blocking(move || finish(&d, &s)).await.unwrap_or_else(|e| Err(e.to_string())) {
                Ok(()) => Outcome::Unpacked,
                Err(e) => Outcome::GaveUp(e),
            };
        }
        if stop.load(Ordering::Relaxed) {
            return Outcome::Interrupted;
        }
        let index = volumes[state.done];
        // (name on disk, complete, undamaged)
        let (name, complete, clean) = {
            let r = job.rec.lock();
            let f = &r.files[index];
            (f.name.clone(), f.complete(), f.missing.is_empty() && f.damaged.is_empty())
        };
        if complete && !clean {
            // Repair needs every volume as posted, so this job takes the ordinary path.
            return give_up(state, "a volume arrived damaged".into()).await;
        }
        let Some(name) = name.filter(|_| complete) else {
            if downloaded.load(Ordering::Relaxed) {
                return give_up(state, "a volume is missing".into()).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            continue;
        };
        // A crash between saving the state and deleting the volume leaves a handled volume behind.
        let d = dir.clone();
        let mut s = state.clone();
        let taken = tokio::task::spawn_blocking(move || take_volume(&d, &mut s, &name).map(|()| s)).await.unwrap_or_else(|e| Err(e.to_string()));
        match taken {
            Ok(s) => state = s,
            Err(why) => return give_up(state, why).await,
        }
    }
}

/// Clear away a handled volume that a crash left on disk, so a resumed job is consistent.
pub(crate) fn tidy(job: &Job) {
    let state = load(&job.dir);
    for r in &state.removed {
        let _ = std::fs::remove_file(job.dir.join(&r.name));
    }
}
