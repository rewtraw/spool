//! Quality profiles: which qualities are wanted, in what order, and when to stop upgrading.

use crate::quality::{Quality, QualityModel};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// One rung of a profile: a single quality, or a named group of qualities treated as equal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileItem {
    #[serde(default)]
    pub name: Option<String>,
    pub qualities: Vec<Quality>,
    pub allowed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QualityProfile {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    /// "movie" or "tv".
    pub kind: String,
    pub upgrade_allowed: bool,
    /// Any quality in the rung where upgrading stops.
    pub cutoff: Quality,
    /// Worst to best.
    pub items: Vec<ProfileItem>,
    /// Size the profile aims for, as (smallest, largest) in gigabytes for the whole release.
    /// A preference, not a filter: a release over the top end is still taken when nothing
    /// that fits exists, but it never replaces a file already in the library.
    #[serde(default)]
    pub target_size_gb: Option<(f64, f64)>,
    /// Between otherwise equal releases, prefer H.265 or AV1 for their smaller files.
    #[serde(default)]
    pub prefer_efficient_codec: bool,
    /// Favour releases a streaming box plays without conversion: H.265 video, Dolby Digital Plus
    /// audio, Dolby Vision that also carries HDR10. AV1 becomes a last resort. A preference
    /// between releases of equal quality, except for AV1.
    #[serde(default)]
    pub prefer_direct_play: bool,
}

impl QualityProfile {
    pub fn index(&self, q: Quality) -> Option<usize> {
        self.items.iter().position(|i| i.qualities.contains(&q))
    }

    pub fn allows(&self, q: Quality) -> bool {
        self.items.iter().any(|i| i.allowed && i.qualities.contains(&q))
    }

    pub fn cutoff_index(&self) -> usize {
        self.index(self.cutoff).unwrap_or(self.items.len().saturating_sub(1))
    }

    /// Compare two qualities by their position in this profile. Unlisted qualities sort lowest.
    pub fn compare(&self, a: Quality, b: Quality) -> Ordering {
        let rank = |q| self.index(q).map(|i| i as i64).unwrap_or(-1);
        rank(a).cmp(&rank(b))
    }

    pub fn cutoff_met(&self, q: &QualityModel) -> bool {
        self.index(q.quality).is_some_and(|i| i >= self.cutoff_index())
    }

    /// Largest size, in bytes, that still counts as fitting this profile's target.
    pub fn size_ceiling(&self) -> Option<u64> {
        self.target_size_gb.map(|(_, hi)| (hi * 1024.0 * 1024.0 * 1024.0) as u64)
    }

    pub fn allowed_names(&self, flavor: crate::quality::Flavor) -> Vec<&'static str> {
        self.items.iter().filter(|i| i.allowed).flat_map(|i| i.qualities.iter().map(move |q| q.name(flavor))).collect()
    }

    /// A reasonable starting profile when nothing has been migrated.
    pub fn default_hd(kind: &str) -> QualityProfile {
        let single = |q: Quality, allowed: bool| ProfileItem { name: None, qualities: vec![q], allowed };
        let group = |n: &str, qs: &[Quality], allowed: bool| ProfileItem { name: Some(n.into()), qualities: qs.to_vec(), allowed };
        QualityProfile {
            id: 0,
            name: "HD-1080p".into(),
            kind: kind.into(),
            upgrade_allowed: true,
            cutoff: Quality::Bluray1080p,
            items: vec![
                single(Quality::Sdtv, false),
                single(Quality::Dvd, false),
                single(Quality::Hdtv720p, false),
                group("WEB 720p", &[Quality::Webdl720p, Quality::Webrip720p], false),
                single(Quality::Bluray720p, false),
                single(Quality::Hdtv1080p, true),
                group("WEB 1080p", &[Quality::Webdl1080p, Quality::Webrip1080p], true),
                single(Quality::Bluray1080p, true),
                single(Quality::Remux1080p, false),
            ],
            target_size_gb: None,
            prefer_efficient_codec: false,
            prefer_direct_play: false,
        }
    }
}

/// Size limits for one quality, in megabytes per minute of runtime.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SizeLimit {
    pub min: f64,
    pub max: Option<f64>,
    pub preferred: Option<f64>,
}
