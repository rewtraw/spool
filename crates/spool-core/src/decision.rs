//! Release selection: every candidate gets a list of plain-language reasons it was rejected, or
//! none if it is acceptable. The rules follow Radarr's and Sonarr's decision specifications.

use crate::profile::{QualityProfile, SizeLimit};
use crate::quality::{Flavor, QualityModel};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProperPolicy {
    /// Grab a proper or repack to replace the same quality.
    PreferAndUpgrade,
    /// Prefer them when choosing, but never replace an existing file for one.
    DoNotUpgrade,
    /// Treat them like any other release.
    DoNotPrefer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rejection {
    pub code: String,
    pub message: String,
}

fn reject(code: &str, message: impl Into<String>) -> Rejection {
    Rejection { code: code.into(), message: message.into() }
}

/// What the decision needs to know about one release.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub quality: QualityModel,
    pub size_bytes: u64,
    pub age_days: f64,
    pub languages: Vec<String>,
    pub release_group: Option<String>,
    pub indexer_priority: i32,
    /// Episodes this release covers (1 for a movie).
    pub item_count: u32,
    /// Names a language that is neither wanted nor the title's own: a dub, usually with that
    /// language as the default track even when the original audio is included.
    pub dubbed: bool,
    pub full_season: bool,
    pub multi_season: bool,
    /// The name says H.265, HEVC or AV1.
    pub efficient_codec: bool,
    /// What the name says about how the release will play. See [`PlaybackTraits`].
    pub playback: PlaybackTraits,
}

/// Properties of a release, read from its name, that decide whether a typical streaming box
/// plays it as is or the media server has to convert it on the fly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaybackTraits {
    /// AV1 video. Efficient, but many players, the Apple TV 4K among them, cannot decode it.
    pub av1: bool,
    /// H.265 / HEVC video.
    pub hevc: bool,
    /// Dolby Vision with no HDR10 layer named alongside it. Players without Dolby Vision support
    /// for that profile have nothing to fall back to.
    pub dolby_vision_only: bool,
    /// 2 for Dolby Digital Plus (carries Atmos to devices that cannot pass lossless audio
    /// through), 1 for other common formats or none named, 0 for TrueHD and DTS variants, which
    /// such devices need converted.
    pub audio_rank: u8,
}

impl PlaybackTraits {
    pub fn from_title(title: &str) -> PlaybackTraits {
        let l = format!(" {} ", title.to_lowercase().replace(['.', '_', '-', '[', ']', '(', ')'], " "));
        let has = |needles: &[&str]| needles.iter().any(|n| l.contains(n));
        let dv = has(&[" dv ", " dovi ", " dolby vision ", " dolbyvision "]);
        let hdr = has(&[" hdr ", " hdr10 ", " hdr10+ ", " hdr10plus ", " hdr10p ", " hlg "]);
        let audio_rank = if has(&[" truehd", " dts ", " dts hd", " dtshd", " dts x", " dtsx", " dts ma", " dts es"]) {
            0
        } else if has(&[" ddp", " dd+", " eac3", " e ac 3", " eac 3", " ddplus", " dd p"]) {
            2
        } else {
            1
        };
        PlaybackTraits {
            av1: has(&[" av1 "]),
            hevc: has(&[" x265 ", " h265 ", " h 265 ", " hevc "]),
            dolby_vision_only: dv && !hdr,
            audio_rank,
        }
    }

    /// Higher plays more directly. Compared only between releases of the same quality.
    fn score(&self) -> (bool, bool, u8) {
        (self.hevc, !self.dolby_vision_only, self.audio_rank)
    }
}

#[derive(Clone, Debug)]
pub struct ExistingFile {
    pub quality: QualityModel,
    pub release_group: Option<String>,
    /// Size on disk; zero when not known.
    pub size_bytes: u64,
}

pub struct Context<'a> {
    pub flavor: Flavor,
    pub profile: &'a QualityProfile,
    pub size_limit: Option<SizeLimit>,
    /// Total runtime the release should cover, in minutes. Zero skips the size check.
    pub runtime_minutes: u32,
    /// Files already on disk for the wanted items.
    pub existing: &'a [ExistingFile],
    /// Qualities of downloads already in progress for the same items.
    pub queued: &'a [QualityModel],
    pub blocklisted: bool,
    /// Looking for a smaller copy of what is on disk: a release is wanted if it fits the
    /// profile's size target and is clearly smaller, whether or not it is a quality upgrade.
    pub compact: bool,
    pub monitored: bool,
    /// Movie: released per its minimum availability. Episode: has aired.
    pub available: bool,
    /// Season packs only: every episode of the season has aired.
    pub season_complete: bool,
    pub retention_days: u32,
    pub proper_policy: ProperPolicy,
    /// Language the user wants, lower-case, or None to accept any.
    pub wanted_language: Option<&'a str>,
    /// The language the title was made in, lower-case, when known.
    pub original_language: Option<&'a str>,
    /// True for a search the user started by hand: monitoring and availability are not enforced.
    pub user_invoked: bool,
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn fmt_size(bytes: u64) -> String {
    let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", mb(bytes))
    }
}

/// Why replacing `current` with `new` is or is not an upgrade.
fn upgrade_rejection(ctx: &Context, new: &QualityModel, new_group: &Option<String>, current: &ExistingFile, what: &str) -> Option<Rejection> {
    let p = ctx.profile;
    let name = |q: &QualityModel| q.full_name(ctx.flavor);
    match p.compare(new.quality, current.quality.quality) {
        Ordering::Less => Some(reject("not_an_upgrade", format!("{what} is {}, which is better than {}", name(&current.quality), name(new)))),
        Ordering::Equal => {
            let better_revision = new.revision.rank() > current.quality.revision.rank();
            if !better_revision {
                return Some(reject("not_an_upgrade", format!("{what} is already {}", name(&current.quality))));
            }
            if ctx.proper_policy != ProperPolicy::PreferAndUpgrade {
                return Some(reject("proper_upgrades_off", format!("{what} is already {} and proper/repack upgrades are off", name(&current.quality))));
            }
            if new.revision.is_repack {
                if let (Some(a), Some(b)) = (new_group, &current.release_group) {
                    if !a.eq_ignore_ascii_case(b) {
                        return Some(reject("repack_group_mismatch", format!("repack is from {a}, but {what} is from {b}")));
                    }
                }
            }
            None
        }
        Ordering::Greater => {
            if !p.upgrade_allowed {
                return Some(reject("upgrades_not_allowed", format!("{what} exists as {} and the profile does not allow upgrades", name(&current.quality))));
            }
            if p.cutoff_met(&current.quality) {
                return Some(reject("cutoff_met", format!("{what} is {}, which already meets the profile cutoff", name(&current.quality))));
            }
            None
        }
    }
}

/// Evaluate one candidate. An empty result means it is acceptable.
pub fn evaluate(c: &Candidate, ctx: &Context) -> Vec<Rejection> {
    let mut out = Vec::new();
    let qname = c.quality.quality.name(ctx.flavor);

    if !ctx.profile.allows(c.quality.quality) {
        out.push(reject("quality_not_wanted", format!("{qname} is not wanted in profile \"{}\"", ctx.profile.name)));
    }

    if let (Some(limit), true) = (ctx.size_limit, ctx.runtime_minutes > 0) {
        let runtime = ctx.runtime_minutes as f64;
        let size = mb(c.size_bytes);
        if c.size_bytes > 0 && size < limit.min * runtime {
            out.push(reject("too_small", format!("{} is smaller than the {:.0} MB minimum for {qname} at {} minutes", fmt_size(c.size_bytes), limit.min * runtime, ctx.runtime_minutes)));
        }
        if let Some(max) = limit.max {
            if max > 0.0 && size > max * runtime {
                out.push(reject("too_large", format!("{} is larger than the {:.1} GB maximum for {qname} at {} minutes", fmt_size(c.size_bytes), max * runtime / 1024.0, ctx.runtime_minutes)));
            }
        }
    }

    let compacting = ctx.compact && !ctx.existing.is_empty();
    if compacting {
        let on_disk: u64 = ctx.existing.iter().map(|e| e.size_bytes).sum();
        match ctx.profile.size_ceiling() {
            None => out.push(reject("no_size_target", format!("profile \"{}\" has no size target to compact to", ctx.profile.name))),
            Some(ceiling) if c.size_bytes > ceiling * c.item_count.max(1) as u64 => {
                out.push(reject("over_target", format!("{} is over this profile's {:.0} GB target", fmt_size(c.size_bytes), ctx.profile.target_size_gb.map(|t| t.1).unwrap_or(0.0))));
            }
            // Replacing a file to save a sliver is not worth a download.
            Some(_) if c.size_bytes == 0 || c.size_bytes as u128 * 10 > on_disk as u128 * 8 => {
                out.push(reject("not_smaller", format!("{} would not save at least a fifth of the {} on disk", fmt_size(c.size_bytes), fmt_size(on_disk))));
            }
            Some(_) => {}
        }
        if !ctx.queued.is_empty() {
            out.push(reject("already_queued", "a replacement is already in the queue"));
        }
    } else {
        for existing in ctx.existing {
            if let Some(r) = upgrade_rejection(ctx, &c.quality, &c.release_group, existing, "the file on disk") {
                out.push(r);
                break;
            }
        }

        // A release bigger than the profile aims for is a last resort when there is nothing in the
        // library. It is never a reason to replace a file that is already there.
        if let (Some(ceiling), false) = (ctx.profile.size_ceiling(), ctx.existing.is_empty()) {
            if c.size_bytes > ceiling * c.item_count.max(1) as u64 && !out.iter().any(|r| r.code == "not_an_upgrade") {
                out.push(reject("too_large_to_upgrade", format!("{} is over this profile's {:.0} GB target, and a file is already in the library", fmt_size(c.size_bytes), ctx.profile.target_size_gb.map(|t| t.1).unwrap_or(0.0))));
            }
        }

        for queued in ctx.queued {
            let as_file = ExistingFile { quality: *queued, release_group: None, size_bytes: 0 };
            if let Some(mut r) = upgrade_rejection(ctx, &c.quality, &c.release_group, &as_file, "a download already in the queue") {
                r.code = "already_queued".into();
                out.push(r);
                break;
            }
        }
    }

    if ctx.blocklisted {
        out.push(reject("blocklisted", "this release failed before and is blocklisted"));
    }

    if ctx.retention_days > 0 && c.age_days > ctx.retention_days as f64 {
        out.push(reject("beyond_retention", format!("posted {:.0} days ago, beyond the {} day retention", c.age_days, ctx.retention_days)));
    }

    if let Some(wanted) = ctx.wanted_language {
        let stated: Vec<&str> = c.languages.iter().map(String::as_str).collect();
        // A film is always welcome in the language it was made in. Regional variants count
        // ("spanish_latino" for a Spanish original).
        let original = ctx.original_language.filter(|o| *o != wanted);
        let ok = stated.is_empty() || stated.contains(&wanted) || stated.contains(&"original") || original.is_some_and(|o| stated.iter().any(|s| s.starts_with(o)));
        if !ok {
            let also = original.map(|o| format!(" or {o} (its original language)")).unwrap_or_default();
            out.push(reject("wrong_language", format!("release is {}, wanted {wanted}{also}", stated.join(", "))));
        }
    }

    if !ctx.user_invoked {
        if !ctx.monitored {
            out.push(reject("not_monitored", "not monitored"));
        }
        if !ctx.available {
            out.push(reject("not_available", if ctx.flavor == Flavor::Movie { "the movie has not reached its minimum availability" } else { "the episode has not aired" }));
        }
    }

    if c.multi_season {
        out.push(reject("multi_season", "multi-season packs are not supported"));
    }
    if c.full_season && !ctx.season_complete {
        out.push(reject("season_not_complete", "season pack, but not every episode of the season has aired"));
    }

    out
}

/// Order acceptable candidates best first.
///
/// Without a size target: quality, then proper/repack, indexer priority, coverage, age, size.
/// With one, anything that fits the target outranks anything that does not, so a 60 GB remux is
/// only chosen when no smaller release is acceptable. Among releases that fit, quality still
/// leads, then the codec preference, then how close the size is to the target.
pub fn compare(a: &Candidate, b: &Candidate, profile: &QualityProfile, policy: ProperPolicy) -> Ordering {
    let age_bucket = |d: f64| if d < 1.0 { 0 } else { (d.log10().round() as i64) + 1 };
    let size_bucket = |s: u64| s / (200 * 1024 * 1024);
    let gb = |c: &Candidate| c.size_bytes as f64 / (1024.0 * 1024.0 * 1024.0) / c.item_count.max(1) as f64;
    let quality = profile.compare(b.quality.quality, a.quality.quality);
    let revision = if policy == ProperPolicy::DoNotPrefer { Ordering::Equal } else { b.quality.revision.rank().cmp(&a.quality.revision.rank()) };
    let codec = if profile.prefer_direct_play {
        b.playback.score().cmp(&a.playback.score())
    } else if profile.prefer_efficient_codec {
        b.efficient_codec.cmp(&a.efficient_codec)
    } else {
        Ordering::Equal
    };
    // A release the household's players cannot decode is a last resort, like one that is too big.
    // So is a dub, when a release in the wanted or original language is on offer.
    let unplayable = |c: &Candidate| (profile.prefer_direct_play && c.playback.av1, c.dubbed);
    let tail = a.indexer_priority.cmp(&b.indexer_priority).then_with(|| b.item_count.cmp(&a.item_count)).then_with(|| age_bucket(a.age_days).cmp(&age_bucket(b.age_days)));

    match profile.target_size_gb {
        None => unplayable(a).cmp(&unplayable(b)).then(quality).then(revision).then(codec).then(tail).then_with(|| size_bucket(b.size_bytes).cmp(&size_bucket(a.size_bytes))),
        Some((lo, hi)) => {
            // Unknown sizes are treated as fitting; indexers nearly always report one.
            let over = |c: &Candidate| c.size_bytes > 0 && gb(c) > hi;
            // Distance from the target range, in whole gigabytes so near-equal sizes tie.
            let distance = |c: &Candidate| {
                let g = gb(c);
                (if g < lo { lo - g } else if g > hi { g - hi } else { 0.0 }).round() as i64
            };
            unplayable(a).cmp(&unplayable(b)).then(over(a).cmp(&over(b))).then_with(|| {
                if over(a) {
                    // Both too big: the smaller overshoot wins before quality is considered.
                    distance(a).cmp(&distance(b)).then(quality).then(revision).then(codec).then(tail)
                } else {
                    quality.then(revision).then(codec).then_with(|| distance(a).cmp(&distance(b))).then(tail)
                }
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ProfileItem;
    use crate::quality::{Quality, Revision};

    fn cand(q: Quality) -> Candidate {
        Candidate { quality: QualityModel::new(q), size_bytes: 8 << 30, age_days: 2.0, languages: vec![], release_group: Some("GRP".into()), indexer_priority: 25, item_count: 1, dubbed: false, full_season: false, multi_season: false, efficient_codec: false, playback: PlaybackTraits::default() }
    }

    fn ctx<'a>(p: &'a QualityProfile, existing: &'a [ExistingFile]) -> Context<'a> {
        Context { flavor: Flavor::Movie, profile: p, size_limit: None, runtime_minutes: 110, existing, queued: &[], blocklisted: false, compact: false, monitored: true, available: true, season_complete: true, retention_days: 0, proper_policy: ProperPolicy::PreferAndUpgrade, wanted_language: Some("english"), original_language: None, user_invoked: false }
    }

    fn codes(r: Vec<Rejection>) -> Vec<String> {
        r.into_iter().map(|r| r.code).collect()
    }

    #[test]
    fn accepts_wanted_quality_and_rejects_unwanted() {
        let p = QualityProfile::default_hd("movie");
        assert!(evaluate(&cand(Quality::Bluray1080p), &ctx(&p, &[])).is_empty());
        assert_eq!(codes(evaluate(&cand(Quality::Bluray720p), &ctx(&p, &[]))), ["quality_not_wanted"]);
        assert_eq!(codes(evaluate(&cand(Quality::Remux1080p), &ctx(&p, &[]))), ["quality_not_wanted"]);
    }

    #[test]
    fn upgrade_rules() {
        let p = QualityProfile::default_hd("movie");
        let have = |q| [ExistingFile { quality: QualityModel::new(q), release_group: Some("GRP".into()), size_bytes: 0 }];
        // below cutoff: upgrade accepted
        assert!(evaluate(&cand(Quality::Bluray1080p), &ctx(&p, &have(Quality::Hdtv1080p))).is_empty());
        // same quality: not an upgrade
        assert_eq!(codes(evaluate(&cand(Quality::Webdl1080p), &ctx(&p, &have(Quality::Webrip1080p)))), ["not_an_upgrade"]);
        // at cutoff: nothing replaces it
        assert_eq!(codes(evaluate(&cand(Quality::Hdtv1080p), &ctx(&p, &have(Quality::Bluray1080p)))), ["not_an_upgrade"]);
        // upgrades off
        let mut fixed = p.clone();
        fixed.upgrade_allowed = false;
        assert_eq!(codes(evaluate(&cand(Quality::Bluray1080p), &ctx(&fixed, &have(Quality::Hdtv1080p)))), ["upgrades_not_allowed"]);
    }

    #[test]
    fn propers_and_repacks() {
        let p = QualityProfile::default_hd("movie");
        let have = [ExistingFile { quality: QualityModel::new(Quality::Bluray1080p), release_group: Some("GRP".into()), size_bytes: 0 }];
        let mut proper = cand(Quality::Bluray1080p);
        proper.quality.revision = Revision { version: 2, real: 0, is_repack: false };
        assert!(evaluate(&proper, &ctx(&p, &have)).is_empty());
        let mut c = ctx(&p, &have);
        c.proper_policy = ProperPolicy::DoNotUpgrade;
        assert_eq!(codes(evaluate(&proper, &c)), ["proper_upgrades_off"]);
        let mut repack = proper.clone();
        repack.quality.revision.is_repack = true;
        repack.release_group = Some("OTHER".into());
        assert_eq!(codes(evaluate(&repack, &ctx(&p, &have))), ["repack_group_mismatch"]);
    }

    #[test]
    fn size_language_and_misc() {
        let p = QualityProfile::default_hd("movie");
        let mut c = ctx(&p, &[]);
        c.size_limit = Some(SizeLimit { min: 5.0, max: Some(50.0), preferred: None });
        let mut small = cand(Quality::Bluray1080p);
        small.size_bytes = 100 << 20;
        assert_eq!(codes(evaluate(&small, &c)), ["too_small"]);
        let mut big = cand(Quality::Bluray1080p);
        big.size_bytes = 40 << 30;
        assert_eq!(codes(evaluate(&big, &c)), ["too_large"]);
        let mut german = cand(Quality::Bluray1080p);
        german.languages = vec!["german".into()];
        assert_eq!(codes(evaluate(&german, &ctx(&p, &[]))), ["wrong_language"]);
        german.languages.push("original".into());
        assert!(evaluate(&german, &ctx(&p, &[])).is_empty());
        let mut c = ctx(&p, &[]);
        c.monitored = false;
        c.blocklisted = true;
        assert_eq!(codes(evaluate(&cand(Quality::Bluray1080p), &c)), ["blocklisted", "not_monitored"]);
        c.user_invoked = true;
        assert_eq!(codes(evaluate(&cand(Quality::Bluray1080p), &c)), ["blocklisted"]);
    }

    fn sized(q: Quality, gb: u64, hevc: bool) -> Candidate {
        let mut c = cand(q);
        c.size_bytes = gb << 30;
        c.efficient_codec = hevc;
        c
    }

    fn best(mut v: Vec<Candidate>, p: &QualityProfile) -> Candidate {
        v.sort_by(|a, b| compare(a, b, p, ProperPolicy::PreferAndUpgrade));
        v.remove(0)
    }

    fn compact() -> QualityProfile {
        let mut p = QualityProfile::default_hd("movie");
        for i in &mut p.items {
            i.allowed = true;
        }
        p.items.push(ProfileItem { name: None, qualities: vec![Quality::Webdl2160p], allowed: true });
        p.items.push(ProfileItem { name: None, qualities: vec![Quality::Remux2160p], allowed: true });
        p.cutoff = Quality::Webdl2160p;
        p.target_size_gb = Some((15.0, 25.0));
        p.prefer_efficient_codec = true;
        p
    }

    #[test]
    fn size_target_prefers_what_fits_without_filtering() {
        let p = compact();
        // The remux is the highest quality but far over the target; the 4K web release fits.
        let pick = best(vec![sized(Quality::Remux2160p, 62, true), sized(Quality::Webdl2160p, 20, true), sized(Quality::Bluray1080p, 12, false)], &p);
        assert_eq!((pick.quality.quality, pick.size_bytes >> 30), (Quality::Webdl2160p, 20));
        // Nothing fits: the smallest overshoot is still taken rather than nothing.
        let pick = best(vec![sized(Quality::Remux2160p, 62, true), sized(Quality::Remux1080p, 31, false)], &p);
        assert_eq!(pick.quality.quality, Quality::Remux1080p);
        assert!(evaluate(&sized(Quality::Remux2160p, 62, true), &ctx(&p, &[])).is_empty(), "an oversize release is acceptable when the library has nothing");
        // Same quality: H.265 first, then the size nearest the target.
        let pick = best(vec![sized(Quality::Bluray1080p, 14, false), sized(Quality::Bluray1080p, 6, true), sized(Quality::Bluray1080p, 16, true)], &p);
        assert_eq!((pick.size_bytes >> 30, pick.efficient_codec), (16, true));
        // The codec is a preference only: a better quality in H.264 still wins.
        let pick = best(vec![sized(Quality::Webdl1080p, 9, true), sized(Quality::Bluray1080p, 13, false)], &p);
        assert_eq!(pick.quality.quality, Quality::Bluray1080p);
    }

    fn named(q: Quality, gb: u64, title: &str) -> Candidate {
        let mut c = sized(q, gb, false);
        c.playback = PlaybackTraits::from_title(title);
        c.efficient_codec = c.playback.hevc || c.playback.av1;
        c
    }

    #[test]
    fn reads_playback_traits_from_names() {
        let t = PlaybackTraits::from_title("Backrooms.2026.2160p.iT.WEB-DL.DDP5.1.Atmos.DV.HDR.H.265-BYNDR");
        assert_eq!((t.hevc, t.av1, t.dolby_vision_only, t.audio_rank), (true, false, false, 2));
        let t = PlaybackTraits::from_title("Backrooms.2026.2160p.UHD.BluRay.Remux.HEVC.DoVi.TrueHD.Atmos.7.1-playBD");
        assert_eq!((t.hevc, t.dolby_vision_only, t.audio_rank), (true, true, 0));
        let t = PlaybackTraits::from_title("Backrooms.2026.1080p.WEBRip.Ds4k.Av1.DDP5.1-DAV1NCI");
        assert_eq!((t.av1, t.hevc, t.audio_rank), (true, false, 2));
        let t = PlaybackTraits::from_title("Movie.2020.1080p.BluRay.DTS-HD.MA.5.1.x264-GRP");
        assert_eq!((t.hevc, t.audio_rank), (false, 0));
        let t = PlaybackTraits::from_title("Movie.2020.2160p.WEB-DL.DV.HDR10+.H.265-GRP");
        assert!(!t.dolby_vision_only);
        assert_eq!(PlaybackTraits::from_title("Movie.2020.1080p.BluRay.x264-GRP").audio_rank, 1);
    }

    #[test]
    fn direct_play_preference_orders_equal_quality_releases() {
        let mut p = compact();
        p.prefer_direct_play = true;
        let q = Quality::Webdl2160p;
        // Same quality and size: Dolby Digital Plus beats TrueHD, HDR fallback beats Dolby Vision alone.
        let pick = best(vec![named(q, 20, "M.2020.2160p.WEB-DL.TrueHD.7.1.DV.HDR.H.265-A"), named(q, 20, "M.2020.2160p.WEB-DL.DDP5.1.Atmos.DV.H.265-B"), named(q, 20, "M.2020.2160p.WEB-DL.DDP5.1.Atmos.DV.HDR.H.265-C")], &p);
        assert_eq!(pick.playback, PlaybackTraits::from_title("M.2020.2160p.WEB-DL.DDP5.1.Atmos.DV.HDR.H.265-C"));
        // AV1 drops below everything playable, even a lower quality, but is still there as a last resort.
        let ranked = {
            let mut v = vec![named(q, 12, "M.2020.2160p.WEB-DL.AV1.DDP5.1-A"), named(Quality::Bluray1080p, 12, "M.2020.1080p.BluRay.DD5.1.x264-B")];
            v.sort_by(|a, b| compare(a, b, &p, ProperPolicy::PreferAndUpgrade));
            v
        };
        assert_eq!(ranked[0].quality.quality, Quality::Bluray1080p);
        assert!(ranked[1].playback.av1);
        assert!(evaluate(&ranked[1], &ctx(&p, &[])).is_empty());
        // Quality still leads among playable releases: H.264 Blu-ray over H.265 web at 1080p.
        let pick = best(vec![named(Quality::Webdl1080p, 6, "M.2020.1080p.WEB-DL.DDP5.1.H.265-A"), named(Quality::Bluray1080p, 12, "M.2020.1080p.BluRay.DTS.x264-B")], &p);
        assert_eq!(pick.quality.quality, Quality::Bluray1080p);
    }

    #[test]
    fn oversize_releases_never_replace_an_existing_file() {
        let p = compact();
        let have = [ExistingFile { quality: QualityModel::new(Quality::Bluray1080p), release_group: None, size_bytes: 0 }];
        assert_eq!(codes(evaluate(&sized(Quality::Remux2160p, 62, true), &ctx(&p, &have))), ["too_large_to_upgrade"]);
        assert!(evaluate(&sized(Quality::Webdl2160p, 20, true), &ctx(&p, &have)).is_empty(), "a fitting upgrade is still taken");
        let at_cutoff = [ExistingFile { quality: QualityModel::new(Quality::Webdl2160p), release_group: None, size_bytes: 0 }];
        assert_eq!(codes(evaluate(&sized(Quality::Remux2160p, 22, true), &ctx(&p, &at_cutoff))), ["cutoff_met"]);
    }

    #[test]
    fn ranking_prefers_quality_then_revision_then_indexer() {
        let p = QualityProfile::default_hd("movie");
        let web = cand(Quality::Webdl1080p);
        let blu = cand(Quality::Bluray1080p);
        let mut proper = cand(Quality::Bluray1080p);
        proper.quality.revision.version = 2;
        let mut v = vec![web.clone(), blu.clone(), proper.clone()];
        v.sort_by(|a, b| compare(a, b, &p, ProperPolicy::PreferAndUpgrade));
        assert_eq!(v[0].quality.revision.version, 2);
        assert_eq!(v[2].quality.quality, Quality::Webdl1080p);
    }

    #[test]
    fn compacting_takes_a_smaller_copy_that_fits_even_at_lower_quality() {
        let mut p = QualityProfile::default_hd("movie");
        p.target_size_gb = Some((10.0, 20.0));
        let have = [ExistingFile { quality: QualityModel::new(Quality::Remux1080p), release_group: None, size_bytes: 37 << 30 }];
        let sized = |q, gb: u64| Candidate { size_bytes: gb << 30, ..cand(q) };
        // Ordinarily a lower quality is no upgrade.
        assert_eq!(codes(evaluate(&sized(Quality::Bluray1080p, 12), &ctx(&p, &have))), ["not_an_upgrade"]);
        let mut c = ctx(&p, &have);
        c.compact = true;
        assert!(evaluate(&sized(Quality::Bluray1080p, 12), &c).is_empty());
        assert_eq!(codes(evaluate(&sized(Quality::Bluray1080p, 24), &c)), ["over_target"]);
        assert_eq!(codes(evaluate(&sized(Quality::Bluray720p, 6), &c)), ["quality_not_wanted"]);
        // On a file that is already small, a slightly smaller one is not worth fetching.
        let small = [ExistingFile { quality: QualityModel::new(Quality::Bluray1080p), release_group: None, size_bytes: 13 << 30 }];
        let mut c = ctx(&p, &small);
        c.compact = true;
        assert_eq!(codes(evaluate(&sized(Quality::Webdl1080p, 12), &c)), ["not_smaller"]);
    }

    #[test]
    fn the_original_language_is_always_acceptable() {
        let p = QualityProfile::default_hd("movie");
        let said = |langs: &[&str]| Candidate { languages: langs.iter().map(|l| l.to_string()).collect(), ..cand(Quality::Bluray1080p) };
        let mut c = ctx(&p, &[]);
        assert_eq!(codes(evaluate(&said(&["german"]), &c)), ["wrong_language"]);
        c.original_language = Some("german");
        assert!(evaluate(&said(&["german"]), &c).is_empty());
        assert!(evaluate(&said(&[]), &c).is_empty());
        let r = evaluate(&said(&["french"]), &c);
        assert!(r[0].message.contains("wanted english or german"), "{}", r[0].message);
        c.original_language = Some("spanish");
        assert!(evaluate(&said(&["spanish_latino"]), &c).is_empty());
    }

    #[test]
    fn a_dub_is_chosen_only_when_nothing_else_is_on_offer() {
        let p = QualityProfile::default_hd("movie");
        let plain = cand(Quality::Webdl1080p);
        let dub = Candidate { dubbed: true, ..cand(Quality::Bluray1080p) };
        // Better quality does not rescue it.
        assert_eq!(compare(&plain, &dub, &p, ProperPolicy::PreferAndUpgrade), Ordering::Less);
        assert!(evaluate(&dub, &ctx(&p, &[])).is_empty(), "still acceptable");
    }
}
