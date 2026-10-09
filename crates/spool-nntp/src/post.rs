//! After the last article: verify, repair, extract, tidy, and hand the result over.

use crate::engine::{job_dir_files, safe_dir_name, FetchOutcome, Inner, Job, JobState};
use crate::par2;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock};

fn find_tool(configured: &str, names: &[&str]) -> Option<PathBuf> {
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        return p.is_file().then_some(p);
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let dirs = path.split(':').map(PathBuf::from).chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].iter().map(PathBuf::from));
    for d in dirs {
        for n in names {
            let p = d.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn fail(job: &Job, why: impl Into<String>) {
    let why = why.into();
    tracing::warn!(job = %job.id, reason = %why, "job failed");
    let mut r = job.rec.lock();
    r.state = JobState::Failed;
    r.message = "Failed".into();
    r.error = Some(why);
}

struct Par2Set {
    index: PathBuf,
    info: par2::Par2Info,
}

/// Give every PAR2 file in the directory a name `par2` will recognise.
///
/// Obfuscated posts deliver recovery files under random names with no extension. The helper only
/// picks up an index called `<base>.par2` and volumes called `<base>.<anything>.par2`, so files
/// are renamed to that shape, grouped by the recovery set they belong to. Safe to repeat: files
/// already named correctly are left alone.
fn normalize_par2_names(dir: &Path) {
    let mut by_set: Vec<([u8; 16], Vec<(u64, PathBuf)>)> = Vec::new();
    for p in job_dir_files(dir) {
        let Some(id) = par2::set_id_of(&p) else { continue };
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        match by_set.iter_mut().find(|(s, _)| *s == id) {
            Some((_, files)) => files.push((size, p)),
            None => by_set.push((id, vec![(size, p)])),
        }
    }
    for (n, (_, mut files)) in by_set.into_iter().enumerate() {
        files.sort();
        // The smallest file is the index. Keep its base name if it already has a proper one.
        let index_name = file_name(&files[0].1);
        let lower = index_name.to_lowercase();
        let base = if lower.ends_with(".par2") && !par2::is_recovery_volume(&index_name) {
            index_name[..index_name.len() - 5].to_string()
        } else {
            format!("spool-recovery-{n}")
        };
        let prefix = format!("{}.", base.to_lowercase());
        for (i, (_, path)) in files.iter().enumerate() {
            let name = file_name(path);
            let l = name.to_lowercase();
            let wanted = if i == 0 {
                format!("{base}.par2")
            } else if l.starts_with(&prefix) && l.ends_with(".par2") {
                continue;
            } else {
                format!("{base}.vol{i:03}.par2")
            };
            if name != wanted && !dir.join(&wanted).exists() {
                let _ = std::fs::rename(path, dir.join(&wanted));
            }
        }
    }
}

/// PAR2 files present in the job directory, one entry per recovery set, smallest file as index.
fn par2_sets(dir: &Path) -> Vec<Par2Set> {
    normalize_par2_names(dir);
    let mut candidates: Vec<(u64, PathBuf)> = job_dir_files(dir)
        .into_iter()
        .filter(|p| par2::is_par2(&file_name(p)) || par2::has_magic(p))
        .filter_map(|p| std::fs::metadata(&p).ok().map(|m| (m.len(), p)))
        .collect();
    candidates.sort();
    let mut sets: Vec<Par2Set> = Vec::new();
    for (_, p) in candidates {
        let Ok(info) = par2::parse_file(&p) else { continue };
        if info.files.is_empty() {
            continue;
        }
        match sets.iter_mut().find(|s| s.info.set_id == info.set_id) {
            Some(existing) => {
                for f in info.files {
                    if !existing.info.files.iter().any(|e| e.name == f.name) {
                        existing.info.files.push(f);
                    }
                }
            }
            None => sets.push(Par2Set { index: p, info }),
        }
    }
    sets
}

/// Give obfuscated files the names the recovery set records, matching on the first 16 KiB.
fn rename_from_par2(dir: &Path, sets: &[Par2Set]) {
    let wanted: Vec<&par2::FileDesc> = sets.iter().flat_map(|s| s.info.files.iter()).filter(|f| !dir.join(&f.name).exists()).collect();
    if wanted.is_empty() {
        return;
    }
    let known: Vec<&str> = sets.iter().flat_map(|s| s.info.files.iter()).map(|f| f.name.as_str()).collect();
    for path in job_dir_files(dir) {
        let name = file_name(&path);
        if known.contains(&name.as_str()) || par2::is_par2(&name) || par2::has_magic(&path) {
            continue;
        }
        let Ok(h) = par2::hash_16k(&path) else { continue };
        if let Some(desc) = wanted.iter().find(|d| d.md5_16k == h) {
            if let Some(safe) = crate::engine::sanitize_name(&desc.name) {
                let target = dir.join(&safe);
                if !target.exists() && std::fs::rename(&path, &target).is_ok() {
                    tracing::info!(from = %name, to = %safe, "restored file name from recovery data");
                }
            }
        }
    }
}

enum Verify {
    Ok,
    Repairable,
    NeedBlocks(u32),
    Hopeless(String),
}

static NEED_BLOCKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"You need (\d+) more recovery blocks").unwrap());

fn run_par2(tool: &Path, mode: &str, dir: &Path, set: &Par2Set) -> (i32, String) {
    // Listing every other file lets par2 find data that is misnamed or split differently.
    let extras: Vec<String> = job_dir_files(dir).iter().map(|p| file_name(p)).filter(|n| !par2::is_par2(n)).collect();
    let mut cmd = Command::new(tool);
    cmd.arg(mode).arg("-q");
    if mode == "repair" {
        // Purge the damaged originals par2 would otherwise leave behind as "name.1".
        cmd.arg("-p");
    }
    let out = cmd.arg(file_name(&set.index)).args(&extras).current_dir(dir).output();
    match out {
        Ok(o) => (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => (-1, format!("could not run par2: {e}")),
    }
}

fn verify(tool: &Path, dir: &Path, set: &Par2Set) -> Verify {
    // Recovery volumes fetched since the last look need proper names too.
    normalize_par2_names(dir);
    let (code, text) = run_par2(tool, "verify", dir, set);
    if code == 0 {
        return Verify::Ok;
    }
    if let Some(c) = NEED_BLOCKS.captures(&text) {
        return Verify::NeedBlocks(c[1].parse().unwrap_or(u32::MAX));
    }
    if code == 1 || text.contains("Repair is possible") {
        return Verify::Repairable;
    }
    Verify::Hopeless(text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("verification failed").trim().to_string())
}

/// True when every file the set protects is present at its recorded size.
fn sizes_match(dir: &Path, set: &Par2Set) -> bool {
    set.info.files.iter().all(|f| std::fs::metadata(dir.join(&f.name)).map(|m| m.len() == f.length).unwrap_or(false))
}

static RAR_PART: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?P<base>.+)\.part(?P<n>\d+)\.rar$").unwrap());
static RAR_OLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?P<base>.+)\.(rar|[r-z]\d\d)$").unwrap());
static SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?P<base>.+)\.(?P<n>\d{3})$").unwrap());

struct Archive {
    first: PathBuf,
    members: Vec<PathBuf>,
    /// A raw split file (name.mkv.001, .002, ...) that only needs joining.
    join_to: Option<String>,
}

fn find_archives(dir: &Path) -> Vec<Archive> {
    let files = job_dir_files(dir);
    let names: Vec<String> = files.iter().map(|p| file_name(p)).collect();
    let mut out = Vec::new();
    let mut claimed: Vec<bool> = vec![false; files.len()];

    // name.partNN.rar sets
    let mut bases: Vec<String> = names.iter().filter_map(|n| RAR_PART.captures(n).map(|c| c["base"].to_lowercase())).collect();
    bases.sort();
    bases.dedup();
    for base in bases {
        let mut members: Vec<(u32, usize)> = names
            .iter()
            .enumerate()
            .filter_map(|(i, n)| RAR_PART.captures(n).filter(|c| c["base"].to_lowercase() == base).map(|c| (c["n"].parse().unwrap_or(0), i)))
            .collect();
        members.sort();
        for (_, i) in &members {
            claimed[*i] = true;
        }
        out.push(Archive { first: files[members[0].1].clone(), members: members.iter().map(|(_, i)| files[*i].clone()).collect(), join_to: None });
    }
    // name.rar + name.r00 ...
    for (i, n) in names.iter().enumerate() {
        if claimed[i] || !n.to_lowercase().ends_with(".rar") {
            continue;
        }
        let base = n[..n.len() - 4].to_lowercase();
        let members: Vec<usize> = names
            .iter()
            .enumerate()
            .filter(|(j, m)| !claimed[*j] && RAR_OLD.captures(m).is_some_and(|c| c["base"].to_lowercase() == base))
            .map(|(j, _)| j)
            .collect();
        for j in &members {
            claimed[*j] = true;
        }
        out.push(Archive { first: files[i].clone(), members: members.iter().map(|j| files[*j].clone()).collect(), join_to: None });
    }
    // .7z, .zip, and .7z.001 / raw .001 splits
    for (i, n) in names.iter().enumerate() {
        if claimed[i] {
            continue;
        }
        let lower = n.to_lowercase();
        if lower.ends_with(".7z") || lower.ends_with(".zip") {
            claimed[i] = true;
            out.push(Archive { first: files[i].clone(), members: vec![files[i].clone()], join_to: None });
        } else if let Some(c) = SPLIT.captures(n).filter(|c| &c["n"] == "001") {
            let base = c["base"].to_string();
            let mut members: Vec<(u32, usize)> = names
                .iter()
                .enumerate()
                .filter_map(|(j, m)| SPLIT.captures(m).filter(|c| c["base"] == base).map(|c| (c["n"].parse().unwrap_or(0), j)))
                .collect();
            members.sort();
            for (_, j) in &members {
                claimed[*j] = true;
            }
            let is_archive = base.to_lowercase().ends_with(".7z") || base.to_lowercase().ends_with(".zip");
            out.push(Archive {
                first: files[i].clone(),
                members: members.iter().map(|(_, j)| files[*j].clone()).collect(),
                join_to: (!is_archive).then_some(base),
            });
        }
    }
    out
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..).map(|n| dir.join(format!("{stem}.{n}{ext}"))).find(|p| !p.exists()).unwrap()
}

/// Move everything under `from` into `to`, flattening nothing: directories keep their shape.
fn move_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let name = file_name(&src);
        if src.is_dir() {
            let dst = to.join(&name);
            if dst.exists() {
                move_tree(&src, &dst)?;
                let _ = std::fs::remove_dir(&src);
            } else {
                std::fs::rename(&src, &dst)?;
            }
        } else {
            std::fs::rename(&src, unique_path(to, &name))?;
        }
    }
    Ok(())
}

fn extract(tool: Option<&Path>, dir: &Path, password: Option<&str>) -> Result<bool, String> {
    let archives = find_archives(dir);
    if archives.is_empty() {
        return Ok(false);
    }
    for a in archives {
        if let Some(target) = &a.join_to {
            let out_path = unique_path(dir, target);
            let mut out = std::fs::File::create(&out_path).map_err(|e| e.to_string())?;
            for m in &a.members {
                let mut f = std::fs::File::open(m).map_err(|e| e.to_string())?;
                std::io::copy(&mut f, &mut out).map_err(|e| format!("joining {}: {e}", file_name(m)))?;
            }
            for m in &a.members {
                let _ = std::fs::remove_file(m);
            }
            continue;
        }
        let tool = tool.ok_or("the download is an archive but 7zz is not installed (brew install sevenzip)")?;
        let tmp = dir.join("_unpack");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let out = Command::new(tool)
            .arg("x")
            .arg("-y")
            .arg("-bd")
            .arg("-bb0")
            .arg(format!("-p{}", password.unwrap_or("")))
            .arg(format!("-o{}", tmp.display()))
            .arg(&a.first)
            .output()
            .map_err(|e| format!("could not run 7zz: {e}"))?;
        if !out.status.success() {
            let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            let _ = std::fs::remove_dir_all(&tmp);
            let reason = if text.contains("Wrong password") || text.contains("Cannot open encrypted archive") {
                if password.is_some() { "the archive password is wrong".to_string() } else { "the archive is password protected and no password was supplied".to_string() }
            } else {
                text.lines().filter(|l| l.contains("ERROR") || l.contains("Error")).next_back().unwrap_or("extraction failed").trim().to_string()
            };
            return Err(format!("{}: {reason}", file_name(&a.first)));
        }
        for m in &a.members {
            let _ = std::fs::remove_file(m);
        }
        move_tree(&tmp, dir).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(&tmp);
    }
    Ok(true)
}

static HEXISH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-fA-F0-9]{24,}$|^[a-zA-Z0-9]{30,}$|^[a-f0-9.]{40,}$|^abc\.xyz").unwrap());

fn looks_obfuscated(stem: &str) -> bool {
    if HEXISH.is_match(stem) {
        return true;
    }
    // Real names have separators and several letters; random strings rarely do.
    let letters = stem.chars().filter(|c| c.is_alphabetic()).count();
    let seps = stem.chars().filter(|c| matches!(c, '.' | ' ' | '_' | '-')).count();
    stem.len() >= 16 && seps == 0 && letters < stem.len()
}

/// If the main file still carries a meaningless name, name it after the job.
fn deobfuscate(dir: &Path, job_name: &str) {
    let mut files: Vec<(u64, PathBuf)> = job_dir_files(dir).into_iter().filter_map(|p| std::fs::metadata(&p).ok().map(|m| (m.len(), p))).collect();
    files.sort();
    let Some((size, biggest)) = files.pop() else { return };
    if files.last().is_some_and(|(s, _)| *s * 2 > size) {
        return; // no single dominant file
    }
    let name = file_name(&biggest);
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    if looks_obfuscated(stem) {
        let target = dir.join(format!("{job_name}{ext}"));
        if !target.exists() {
            let _ = std::fs::rename(&biggest, target);
        }
    }
}

fn cleanup(dir: &Path) {
    for p in job_dir_files(dir) {
        let n = file_name(&p).to_lowercase();
        let junk = n.ends_with(".par2") || n.ends_with(".sfv") || n.ends_with(".nzb") || n.ends_with(".par2.1") || n.ends_with(".1") && par2::has_magic(&p) || par2::has_magic(&p);
        if junk {
            let _ = std::fs::remove_file(p);
        }
    }
}

pub(crate) async fn process(inner: &Arc<Inner>, job: &Arc<Job>) {
    let cfg = inner.cfg.read().clone();
    let dir = job.dir.clone();
    let par2_tool = find_tool(&cfg.par2_path, &["par2"]);
    let sevenzip = find_tool(&cfg.sevenzip_path, &["7zz", "7z"]);
    let (damage, password, name) = {
        let r = job.rec.lock();
        (r.files.iter().map(|f| f.missing.len() + f.damaged.len()).sum::<usize>(), r.password.clone(), r.name.clone())
    };

    // ---- verify and repair
    let d = dir.clone();
    let sets = tokio::task::spawn_blocking(move || {
        let sets = par2_sets(&d);
        rename_from_par2(&d, &sets);
        sets
    })
    .await
    .unwrap_or_default();

    // Recovery files stay until extraction has succeeded, so a failed extraction can still be repaired.
    let mut force = false;
    // Unpacked while downloading: every part was checked against the archive's own checksums
    // as it was copied out, so there is nothing left to verify or extract.
    let already_unpacked = job.rec.lock().direct_done;
    loop {
        if already_unpacked {
            break;
        }
        let mut skipped = false;
        if sets.is_empty() {
            if damage > 0 {
                return fail(job, format!("{damage} articles were missing or damaged and the post has no recovery data"));
            }
        } else {
            for set in &sets {
                // Every article passed its checksum, so matching sizes almost always means the files
                // are whole. The full check is skipped unless extraction later says otherwise.
                if !force && damage == 0 && sizes_match(&dir, set) {
                    skipped = true;
                    continue;
                }
                let Some(tool) = par2_tool.clone() else {
                    return fail(job, "the download needs repair but par2 is not installed (brew install par2)");
                };
                let mut rounds = 0;
                loop {
                    rounds += 1;
                    job.set_state(JobState::Verifying, "Verifying with recovery data");
                    inner.publish(job);
                    let (t, d2, idx) = (tool.clone(), dir.clone(), Par2Set { index: set.index.clone(), info: set.info.clone() });
                    let result = tokio::task::spawn_blocking(move || verify(&t, &d2, &idx)).await.unwrap_or(Verify::Hopeless("verify crashed".into()));
                    match result {
                        Verify::Ok => break,
                        Verify::Repairable => {
                            job.set_state(JobState::Repairing, "Repairing");
                            inner.publish(job);
                            let (t, d2, idx) = (tool.clone(), dir.clone(), Par2Set { index: set.index.clone(), info: set.info.clone() });
                            let (code, text) = tokio::task::spawn_blocking(move || run_par2(&t, "repair", &d2, &idx)).await.unwrap_or((-1, String::new()));
                            if code != 0 {
                                return fail(job, format!("repair failed: {}", text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error").trim()));
                            }
                            break;
                        }
                        Verify::NeedBlocks(need) if rounds <= 3 => {
                            // Pick deferred recovery volumes, biggest first, until they cover the need.
                            let mut volumes: Vec<(u32, usize)> = {
                                let r = job.rec.lock();
                                r.files
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, f)| f.deferred && !f.complete())
                                    .filter_map(|(i, _)| job.nzb.files[i].subject_filename().and_then(|n| par2::blocks_from_name(&n)).map(|b| (b, i)))
                                    .collect()
                            };
                            volumes.sort_by(|a, b| b.cmp(a));
                            let available: u32 = volumes.iter().map(|v| v.0).sum();
                            if available < need {
                                return fail(job, format!("not repairable: {need} more recovery blocks are needed and only {available} exist"));
                            }
                            // Smallest set that covers the need: take big ones, then swap the last for the smallest that still fits.
                            let mut picked: Vec<usize> = Vec::new();
                            let mut have = 0;
                            for (blocks, i) in &volumes {
                                if have >= need {
                                    break;
                                }
                                if have + blocks > need {
                                    if let Some((b, j)) = volumes.iter().rev().find(|(b, j)| have + b >= need && !picked.contains(j)) {
                                        picked.push(*j);
                                        have += b;
                                        break;
                                    }
                                }
                                picked.push(*i);
                                have += blocks;
                            }
                            {
                                let mut r = job.rec.lock();
                                let extra: u64 = picked.iter().map(|i| job.nzb.files[*i].bytes()).sum();
                                r.total_bytes += extra;
                                for i in &picked {
                                    r.files[*i].deferred = false;
                                }
                                r.state = JobState::Downloading;
                                r.message = format!("Fetching {need} recovery blocks");
                            }
                            inner.publish(job);
                            match inner.fetch_files(job, &picked).await {
                                FetchOutcome::Complete => job.close_handles(),
                                FetchOutcome::Stopped => {
                                    job.close_handles();
                                    let mut r = job.rec.lock();
                                    if !r.state.is_terminal() && r.state != JobState::Paused {
                                        r.state = JobState::Queued;
                                    }
                                    return;
                                }
                                FetchOutcome::Unreachable(why) => return fail(job, format!("could not fetch recovery data: {why}")),
                            }
                        }
                        Verify::NeedBlocks(need) => return fail(job, format!("not repairable: still {need} recovery blocks short")),
                        Verify::Hopeless(why) => return fail(job, format!("verification failed: {why}")),
                    }
                }
            }
        }

        // ---- extract
        job.set_state(JobState::Extracting, "Extracting");
        inner.publish(job);
        let (d, pw, tool) = (dir.clone(), password.clone(), sevenzip.clone());
        let extracted = tokio::task::spawn_blocking(move || {
            // Archives inside archives are rare but real; two passes covers them.
            let first = extract(tool.as_deref(), &d, pw.as_deref())?;
            if first {
                extract(tool.as_deref(), &d, pw.as_deref())?;
            }
            Ok::<bool, String>(first)
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        match extracted {
            Ok(_) => break,
            // The quick size check let a bad file through. The recovery data is still here: use it.
            Err(why) if skipped && !force => {
                tracing::info!(job = %job.id, reason = %why, "extraction failed after the quick check; verifying in full");
                force = true;
            }
            Err(why) if force && !sets.is_empty() => return fail(job, format!("extraction failed: {why}. The recovery data says the download matches what was posted, so the post itself is damaged")),
            Err(why) => return fail(job, format!("extraction failed: {why}")),
        }
    }
    let d = dir.clone();
    let _ = tokio::task::spawn_blocking(move || cleanup(&d)).await;

    // ---- finish
    job.set_state(JobState::Finishing, "Moving to completed");
    inner.publish(job);
    let (d, n, complete_root) = (dir.clone(), name.clone(), cfg.complete_dir.clone());
    let moved = tokio::task::spawn_blocking(move || -> Result<PathBuf, String> {
        deobfuscate(&d, &n);
        if job_dir_files(&d).is_empty() && std::fs::read_dir(&d).map(|r| r.flatten().filter(|e| e.path().is_dir()).count()).unwrap_or(0) == 0 {
            return Err("nothing was left after processing".into());
        }
        std::fs::create_dir_all(&complete_root).map_err(|e| e.to_string())?;
        let mut target = complete_root.join(safe_dir_name(&n));
        let mut i = 1;
        while target.exists() {
            target = complete_root.join(format!("{}.{i}", safe_dir_name(&n)));
            i += 1;
        }
        std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(&d).map_err(|e| e.to_string())?.flatten() {
            let p = entry.path();
            let fname = file_name(&p);
            if matches!(fname.as_str(), "job.json" | "job.json.tmp" | "source.nzb") {
                continue;
            }
            let dst = target.join(&fname);
            if std::fs::rename(&p, &dst).is_err() {
                // Different volume: copy, then remove the source.
                if p.is_dir() {
                    copy_dir(&p, &dst).map_err(|e| e.to_string())?;
                    let _ = std::fs::remove_dir_all(&p);
                } else {
                    std::fs::copy(&p, &dst).map_err(|e| e.to_string())?;
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        Ok(target)
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));

    match moved {
        Ok(path) => {
            let mut r = job.rec.lock();
            r.state = JobState::Completed;
            r.message = "Completed".into();
            r.output_path = Some(path);
        }
        Err(why) => fail(job, why),
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obfuscation_heuristic() {
        assert!(looks_obfuscated("b082fa0beaa644d3aa01045d5b8d0b36"));
        assert!(looks_obfuscated("0675e29e9abfd2.f7d069dab0b853283cc1b069a25f82.6547"));
        assert!(looks_obfuscated("abc.xyz.a4c567edbcbf27.BLURAY"));
        assert!(!looks_obfuscated("Death.Becomes.Her.1992.1080p.REMASTERED.BluRay.REMUX"));
        assert!(!looks_obfuscated("Westworld S01E01"));
    }

    #[test]
    fn groups_archive_volumes() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a.part01.rar", "a.part02.rar", "b.rar", "b.r00", "b.r01", "c.mkv.001", "c.mkv.002", "d.7z", "notes.nfo"] {
            std::fs::write(dir.path().join(n), b"x").unwrap();
        }
        let found = find_archives(dir.path());
        let summary: Vec<(String, usize, bool)> = found.iter().map(|a| (file_name(&a.first), a.members.len(), a.join_to.is_some())).collect();
        assert!(summary.contains(&("a.part01.rar".into(), 2, false)));
        assert!(summary.contains(&("b.rar".into(), 3, false)));
        assert!(summary.contains(&("c.mkv.001".into(), 2, true)));
        assert!(summary.contains(&("d.7z".into(), 1, false)));
        assert_eq!(found.len(), 4);
    }
}
