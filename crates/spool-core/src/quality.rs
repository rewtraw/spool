//! Quality model and release-name quality parsing.
//!
//! The parsing logic is a port of `QualityParser.cs` from Radarr and Sonarr (GPLv3).
//! The two projects differ slightly, so the parser takes a [`Flavor`].

use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Flavor {
    Movie,
    Tv,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Source {
    Unknown,
    Workprint,
    Cam,
    Telesync,
    Telecine,
    Dvd,
    Tv,
    WebRip,
    WebDl,
    Bluray,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Modifier {
    None,
    Regional,
    Screener,
    RawHd,
    BrDisk,
    Remux,
}

macro_rules! qualities {
    ($( $v:ident, $key:literal, $movie:literal, $tv:literal, $src:ident, $res:literal, $mod:ident, $w:literal; )*) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub enum Quality { $( $v ),* }
        impl Quality {
            pub const ALL: &'static [Quality] = &[ $( Quality::$v ),* ];
            /// Stable identifier used in the database and API.
            pub fn key(self) -> &'static str { match self { $( Quality::$v => $key ),* } }
            pub fn movie_name(self) -> &'static str { match self { $( Quality::$v => $movie ),* } }
            pub fn tv_name(self) -> &'static str { match self { $( Quality::$v => $tv ),* } }
            pub fn source(self) -> Source { match self { $( Quality::$v => Source::$src ),* } }
            pub fn resolution(self) -> u32 { match self { $( Quality::$v => $res ),* } }
            pub fn modifier(self) -> Modifier { match self { $( Quality::$v => Modifier::$mod ),* } }
            /// Default ordering weight (Radarr's defaults). Profiles carry their own order.
            pub fn default_weight(self) -> u32 { match self { $( Quality::$v => $w ),* } }
        }
    };
}

qualities! {
    Unknown,     "unknown",      "Unknown",      "Unknown",            Unknown,   0,    None,     1;
    Workprint,   "workprint",    "WORKPRINT",    "WORKPRINT",          Workprint, 0,    None,     2;
    Cam,         "cam",          "CAM",          "CAM",                Cam,       0,    None,     3;
    Telesync,    "telesync",     "TELESYNC",     "TELESYNC",           Telesync,  0,    None,     4;
    Telecine,    "telecine",     "TELECINE",     "TELECINE",           Telecine,  0,    None,     5;
    Regional,    "regional",     "REGIONAL",     "REGIONAL",           Dvd,       480,  Regional, 6;
    Dvdscr,      "dvdscr",       "DVDSCR",       "DVDSCR",             Dvd,       480,  Screener, 7;
    Sdtv,        "sdtv",         "SDTV",         "SDTV",               Tv,        480,  None,     8;
    Dvd,         "dvd",          "DVD",          "DVD",                Dvd,       0,    None,     9;
    DvdR,        "dvd-r",        "DVD-R",        "DVD-R",              Dvd,       480,  Remux,    10;
    Webdl480p,   "webdl-480p",   "WEBDL-480p",   "WEBDL-480p",         WebDl,     480,  None,     11;
    Webrip480p,  "webrip-480p",  "WEBRip-480p",  "WEBRip-480p",        WebRip,    480,  None,     11;
    Bluray480p,  "bluray-480p",  "Bluray-480p",  "Bluray-480p",        Bluray,    480,  None,     12;
    Bluray576p,  "bluray-576p",  "Bluray-576p",  "Bluray-576p",        Bluray,    576,  None,     13;
    Hdtv720p,    "hdtv-720p",    "HDTV-720p",    "HDTV-720p",          Tv,        720,  None,     14;
    Webdl720p,   "webdl-720p",   "WEBDL-720p",   "WEBDL-720p",         WebDl,     720,  None,     15;
    Webrip720p,  "webrip-720p",  "WEBRip-720p",  "WEBRip-720p",        WebRip,    720,  None,     15;
    Bluray720p,  "bluray-720p",  "Bluray-720p",  "Bluray-720p",        Bluray,    720,  None,     16;
    Hdtv1080p,   "hdtv-1080p",   "HDTV-1080p",   "HDTV-1080p",         Tv,        1080, None,     17;
    Webdl1080p,  "webdl-1080p",  "WEBDL-1080p",  "WEBDL-1080p",        WebDl,     1080, None,     18;
    Webrip1080p, "webrip-1080p", "WEBRip-1080p", "WEBRip-1080p",       WebRip,    1080, None,     18;
    Bluray1080p, "bluray-1080p", "Bluray-1080p", "Bluray-1080p",       Bluray,    1080, None,     19;
    Remux1080p,  "remux-1080p",  "Remux-1080p",  "Bluray-1080p Remux", Bluray,    1080, Remux,    20;
    Hdtv2160p,   "hdtv-2160p",   "HDTV-2160p",   "HDTV-2160p",         Tv,        2160, None,     21;
    Webdl2160p,  "webdl-2160p",  "WEBDL-2160p",  "WEBDL-2160p",        WebDl,     2160, None,     22;
    Webrip2160p, "webrip-2160p", "WEBRip-2160p", "WEBRip-2160p",       WebRip,    2160, None,     22;
    Bluray2160p, "bluray-2160p", "Bluray-2160p", "Bluray-2160p",       Bluray,    2160, None,     23;
    Remux2160p,  "remux-2160p",  "Remux-2160p",  "Bluray-2160p Remux", Bluray,    2160, Remux,    24;
    BrDisk,      "br-disk",      "BR-DISK",      "BR-DISK",            Bluray,    1080, BrDisk,   25;
    RawHd,       "raw-hd",       "Raw-HD",       "Raw-HD",             Tv,        1080, RawHd,    26;
}

impl Quality {
    pub fn from_key(key: &str) -> Option<Quality> {
        Quality::ALL.iter().copied().find(|q| q.key() == key)
    }

    /// Look a quality up by the display name either upstream project uses.
    pub fn from_name(name: &str) -> Option<Quality> {
        Quality::ALL
            .iter()
            .copied()
            .find(|q| q.movie_name().eq_ignore_ascii_case(name) || q.tv_name().eq_ignore_ascii_case(name))
    }

    pub fn name(self, flavor: Flavor) -> &'static str {
        match flavor {
            Flavor::Movie => self.movie_name(),
            Flavor::Tv => self.tv_name(),
        }
    }

    fn find(source: Source, resolution: u32, modifier: Modifier) -> Quality {
        Quality::ALL
            .iter()
            .copied()
            .find(|q| q.source() == source && q.resolution() == resolution && q.modifier() == modifier)
            .or_else(|| {
                Quality::ALL
                    .iter()
                    .copied()
                    .filter(|q| q.source() == source && q.modifier() == modifier)
                    .min_by_key(|q| q.resolution().abs_diff(resolution))
            })
            .unwrap_or(Quality::Unknown)
    }
}

impl Serialize for Quality {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.key())
    }
}

impl<'de> Deserialize<'de> for Quality {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Quality::from_key(&s).ok_or_else(|| serde::de::Error::custom(format!("unknown quality {s}")))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Revision {
    pub version: u32,
    pub real: u32,
    pub is_repack: bool,
}

impl Default for Revision {
    fn default() -> Self {
        Revision { version: 1, real: 0, is_repack: false }
    }
}

impl Revision {
    /// Ordering key: REAL beats version, as upstream does.
    pub fn rank(&self) -> (u32, u32) {
        (self.real, self.version)
    }
    pub fn is_proper_or_repack(&self) -> bool {
        self.version > 1
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QualityModel {
    pub quality: Quality,
    #[serde(default)]
    pub revision: Revision,
}

impl QualityModel {
    pub fn new(quality: Quality) -> Self {
        QualityModel { quality, revision: Revision::default() }
    }

    /// "Bluray-1080p Proper", as `{Quality Full}` renders it.
    pub fn full_name(&self, flavor: Flavor) -> String {
        let mut s = self.quality.name(flavor).to_string();
        if self.revision.is_repack {
            s.push_str(" Repack");
        } else if self.revision.version > 1 {
            s.push_str(" Proper");
        }
        if self.revision.real > 0 {
            s.push_str(" REAL");
        }
        s
    }
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|e| panic!("bad regex {p}: {e}"))
}

struct Regexes {
    source: Regex,
    resolution: Regex,
    codec: Regex,
    version: Regex,
    remux: Regex,
}

static MOVIE: LazyLock<Regexes> = LazyLock::new(|| Regexes {
    source: re(r"(?ix)\b(?:
        (?<bluray>M?Blu[-_.\ ]?Ray|HD[-_.\ ]?DVD|BD(?!$)|UHD2?BD|BDISO|BDMux|BD25|BD50|BR[-_.\ ]?DISK)|
        (?<webdl>WEB[-_.\ ]?DL(?:mux)?|AmazonHD|AmazonSD|iTunesHD|MaxdomeHD|NetflixU?HD|WebHD|HBOMaxHD|DisneyHD|[.\ ]WEB[.\ ](?:[xh][\ .]?26[45]|AVC|HEVC|DDP?5[.\ ]1)|[.\ ](?-i:WEB)$|(?:\d{3,4}0p)[-.\ ](?:Hybrid[-_.\ ]?)?WEB[-.\ ]|[-.\ ]WEB[-.\ ]\d{3,4}0p|\b\s/\sWEB\s/\s\b|(?:AMZN|NF|DP)[.\ -]WEB[.\ -](?!Rip))|
        (?<webrip>WebRip|Web-Rip|WEBMux)|
        (?<hdtv>HDTV)|
        (?<bdrip>BDRip|BDLight|HD[-_.\ ]?DVDRip|UHDBDRip)|
        (?<brrip>BRRip)|
        (?<dvdr>\d?x?M?DVD-?[R59])|
        (?<dvd>DVD(?!-R)|DVDRip|xvidvd)|
        (?<dsr>WS[-_.\ ]DSR|DSR)|
        (?<regional>R[0-9]{1}|REGIONAL)|
        (?<scr>SCR|SCREENER|DVDSCR|DVDSCREENER)|
        (?<ts>TS[-_.\ ]|TELESYNCH?|HD-TS|HDTS|PDVD|TSRip|HDTSRip)|
        (?<tc>TC|TELECINE|HD-TC|HDTC)|
        (?<cam>CAMRIP|(?:NEW)?CAM|HD-?CAM(?:Rip)?|HQCAM)|
        (?<wp>WORKPRINT|WP)|
        (?<pdtv>PDTV)|
        (?<sdtv>SDTV)|
        (?<tvrip>TVRip)
        )(?:\b|$|[\ .])"),
    resolution: re(r"(?i)\b(?:(?<R360p>360p)|(?<R480p>480p|480i|640x480|848x480)|(?<R540p>540p)|(?<R576p>576p)|(?<R720p>720p|1280x720|960p)|(?<R1080p>1080p|1920x1080|1440p|FHD|1080i|4kto1080p)|(?<R2160p>2160p|3840x2160|4k[-_. ](?:UHD|HEVC|BD|H\.?265)|(?:UHD|HEVC|BD|H\.?265)[-_. ]4k))\b"),
    codec: re(r"(?i)\b(?:(?<x264>x264)|(?<h264>h264)|(?<xvidhd>XvidHD)|(?<xvid>X-?vid)|(?<divx>divx))\b"),
    version: re(r"(?i)\d[-._ ]?v(?<v1>\d)[-._ ]|\[v(?<v2>\d)\]|repack(?<v3>\d)|rerip(?<v4>\d)"),
    remux: re(r"(?i)(?:[_. \[]|\d{4}p-|\bHybrid-)(?<remux>(?:(BD|UHD)[-_. ]?)?Remux)\b|(?<remux2>(?:(BD|UHD)[-_. ]?)?Remux[_. ]\d{4}p)"),
});

static TV: LazyLock<Regexes> = LazyLock::new(|| Regexes {
    source: re(r"(?ix)\b(?:
        (?<bluray>BluRay|Blu-Ray|HD-?DVD|BDMux|BD(?!$))|
        (?<webdl>WEB[-_.\ ]DL(?:mux)?|WEBDL|AmazonHD|AmazonSD|iTunesHD|MaxdomeHD|NetflixU?HD|WebHD|HBOMaxHD|DisneyHD|[.\ ]WEB[.\ ](?:[xh][\ .]?26[456]|AVC|HEVC|EAC3|DDP?[\ .]?5[.\ ]1)|[.\ ](?-i:WEB)$|(?:720|1080|2160)p[-.\ ]WEB[-.\ ]|[-.\ ]WEB[-.\ ](?:720|1080|2160)p|\b\s/\sWEB\s/\s\b|(?:AMZN|ATVP|DP|NF)[.\ -]WEB[.\ -](?!Rip))|
        (?<webrip>WebRip|Web-Rip|WEBMux)|
        (?<hdtv>HDTV)|
        (?<bdrip>BDRip|BDLight)|
        (?<brrip>BRRip)|
        (?<dvd>DVD|DVDRip|NTSC|PAL|xvidvd)|
        (?<dsr>WS[-_.\ ]DSR|DSR)|
        (?<pdtv>PDTV)|
        (?<sdtv>SDTV)|
        (?<tvrip>TVRip)
        )(?:\b|$|[\ .])"),
    resolution: re(r"(?i)\b(?:(?<R360p>360p)|(?<R480p>480p|480i|640x480|848x480)|(?<R540p>540p)|(?<R576p>576p)|(?<R720p>720p|1280x720|960p)|(?<R1080p>1080p|1920x1080|1440p|FHD|1080i|4kto1080p)|(?<R2160p>2160p|3840x2160|4k[-_. ](?:UHD|HEVC|BD|H26[56])|(?:UHD|HEVC|BD|H26[56])[-_. ]4k))\b"),
    codec: re(r"(?i)\b(?:(?<x264>x264)|(?<h264>h264)|(?<xvidhd>XvidHD)|(?<xvid>Xvid)|(?<divx>divx))\b"),
    version: re(r"(?i)\d[-._ ]?v(?<v1>\d)[-._ ]|\[v(?<v2>\d)\]|repack(?<v3>\d)|rerip(?<v4>\d)|(?:480|576|720|1080|2160)p[._ ]v(?<v5>\d)"),
    remux: re(r"(?i)(?:[_. ]|\d{4}p-|\bHybrid-)(?<remux>(?:(BD|UHD)[-_. ]?)?Remux)\b|(?<remux2>(?:(BD|UHD)[-_. ]?)?Remux[_. ]\d{4}p)"),
});

static RAW_HD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(?<rawhd>RawHD|Raw[-_. ]HD)\b"));
static MPEG2: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?<mpeg2>MPEG[-_. ]?2)\b"));
static BRDISK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^(?!.*\b((?<!HD[._ -]|HD)DVD|BDRip|720p|MKV|XviD|WMV|d3g|(BD)?REMUX|^(?=.*1080p)(?=.*HEVC)|[xh][-_. ]?26[45]|German.*[DM]L|((?<=\d{4}).*German.*([DM]L)?)(?=.*\b(AVC|HEVC|VC[-_. ]?1|MVC|MPEG[-_. ]?2)\b))\b)(((?=.*\b(Blu[-_. ]?ray|BD|HD[-_. ]?DVD)\b)(?=.*\b(AVC|HEVC|VC[-_. ]?1|MVC|MPEG[-_. ]?2|BDMV|ISO)\b))|^((?=.*\b(((?=.*\b((.*_)?COMPLETE.*|Dis[ck])\b)(?=.*(Blu[-_. ]?ray|HD[-_. ]?DVD)))|3D[-_. ]?BD|BR[-_. ]?DISK|Full[-_. ]?Blu[-_. ]?ray|^((?=.*((BD|UHD)[-_. ]?(25|50|66|100|ISO)))))))).*")
});
static PROPER: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(?<proper>proper)\b"));
static REPACK: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(?<repack>repack\d?|rerip\d?)\b"));
static REAL: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?<real>REAL)\b"));
static ALT_RESOLUTION: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(?<R2160p>UHD)\b|(?<R2160p2>\[4K\])"));
static OTHER_SOURCE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(?<hdtv>HD[-_. ]TV)|(?<sdtv>SD[-_. ]TV)"));
static ANIME_BLURAY: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)bd(?:720|1080|2160)|(?<=[-_. (\[])bd(?=[-_. )\]])"));
static ANIME_WEBDL: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\[WEB\]|[\[\(]WEB[ .]"));
static HIGH_DEF_PDTV: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)hr[-_. ]ws"));
static GERMAN_REMUX: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)((?<=\d{4}).*German.*([DM]L)?)(?=.*\b(AVC|HEVC|VC[_. -]?1|MVC|MPEG[_. -]?2))(?=.*Blu-?ray)")
});

fn is_match(r: &Regex, s: &str) -> bool {
    r.is_match(s).unwrap_or(false)
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

/// Quality implied by a media file extension, as each upstream project maps it.
pub fn quality_for_extension(ext: &str, flavor: Flavor) -> Quality {
    let tv = flavor == Flavor::Tv;
    match ext.to_ascii_lowercase().as_str() {
        ".webm" => Quality::Unknown,
        ".img" | ".iso" | ".vob" => Quality::Dvd,
        ".mkv" if tv => Quality::Hdtv720p,
        ".ts" | ".wtv" if tv => Quality::Hdtv720p,
        ".mkv" | ".mk3d" => Quality::Webdl720p,
        ".m2ts" => Quality::Bluray720p,
        e if crate::parser::common::MEDIA_EXTENSIONS.contains(&e) => Quality::Sdtv,
        _ => Quality::Unknown,
    }
}

fn path_extension(name: &str) -> &str {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.rfind('.') {
        Some(i) if i > 0 => &base[i..],
        _ => "",
    }
}

fn resolution(rx: &Regexes, name: &str) -> u32 {
    let m = crate::parser::common::caps(&rx.resolution, name).ok().flatten();
    let alt = is_match(&ALT_RESOLUTION, name);
    if let Some(c) = &m {
        for (g, r) in [("R360p", 360), ("R480p", 480), ("R540p", 540), ("R576p", 576), ("R720p", 720), ("R1080p", 1080), ("R2160p", 2160)] {
            if c.name(g).is_some() {
                return r;
            }
        }
    }
    if alt {
        2160
    } else {
        0
    }
}

fn modifiers(rx: &Regexes, name: &str, normalized: &str) -> Revision {
    let mut rev = Revision::default();
    let version = crate::parser::common::caps(&rx.version, normalized).ok().flatten().and_then(|c| {
        ["v1", "v2", "v3", "v4", "v5"]
            .iter()
            .find_map(|g| c.name(g))
            .and_then(|m| m.as_str().parse::<u32>().ok())
    });
    if let Some(v) = version {
        rev.version = v;
    }
    if is_match(&PROPER, normalized) {
        rev.version = version.map(|v| v + 1).unwrap_or(2);
    }
    if is_match(&REPACK, normalized) {
        rev.version = version.map(|v| v + 1).unwrap_or(2);
        rev.is_repack = true;
    }
    let real = crate::parser::common::find_iter(&REAL, name).filter(|m| m.is_ok()).count() as u32;
    if real > 0 {
        rev.real = real;
    }
    rev
}

/// Parse quality from a release or file name, falling back to the file extension.
pub fn parse_quality(name: &str, flavor: Flavor) -> QualityModel {
    let name = name.trim();
    if name.is_empty() {
        return QualityModel::new(Quality::Unknown);
    }
    let mut result = parse_quality_name(name, flavor);
    if result.quality == Quality::Unknown {
        result.quality = quality_for_extension(path_extension(name), flavor);
    }
    result
}

/// Parse quality from the name alone.
pub fn parse_quality_name(name: &str, flavor: Flavor) -> QualityModel {
    let rx: &Regexes = match flavor {
        Flavor::Movie => &MOVIE,
        Flavor::Tv => &TV,
    };
    let movie = flavor == Flavor::Movie;
    let normalized = name.replace('_', " ");
    let normalized = normalized.trim();
    let revision = modifiers(rx, name, normalized);
    let done = |q: Quality| QualityModel { quality: q, revision };

    let br_disk = movie && is_match(&BRDISK, normalized);
    if is_match(&RAW_HD, normalized) && !br_disk {
        return done(Quality::RawHd);
    }

    let source = crate::parser::common::caps_iter(&rx.source, normalized).filter_map(|c| c.ok()).last();
    let res = resolution(rx, normalized);
    let codec = crate::parser::common::caps(&rx.codec, normalized).ok().flatten();
    let codec_is = |g: &str| codec.as_ref().is_some_and(|c| c.name(g).is_some());
    let remux = is_match(&rx.remux, normalized) || (movie && is_match(&GERMAN_REMUX, normalized));

    if let Some(src) = &source {
        let g = |n: &str| src.name(n).is_some();
        if g("bluray") {
            if br_disk {
                return done(Quality::BrDisk);
            }
            if codec_is("xvid") || codec_is("divx") {
                return done(Quality::Bluray480p);
            }
            return done(match res {
                2160 => if remux { Quality::Remux2160p } else { Quality::Bluray2160p },
                1080 => if remux { Quality::Remux1080p } else { Quality::Bluray1080p },
                720 if movie => Quality::Bluray720p,
                576 => Quality::Bluray576p,
                360 | 480 | 540 => Quality::Bluray480p,
                // A remux without a resolution is treated as 1080p; a 720p remux stays Bluray-720p.
                _ if remux && res != 720 => Quality::Remux1080p,
                _ => Quality::Bluray720p,
            });
        }
        if g("webdl") {
            return done(match res {
                2160 => Quality::Webdl2160p,
                1080 => Quality::Webdl1080p,
                720 => Quality::Webdl720p,
                _ if name.contains("[WEBDL]") => Quality::Webdl720p,
                _ => Quality::Webdl480p,
            });
        }
        if g("webrip") {
            return done(match res {
                2160 => Quality::Webrip2160p,
                1080 => Quality::Webrip1080p,
                720 => Quality::Webrip720p,
                _ => Quality::Webrip480p,
            });
        }
        if movie {
            if g("scr") {
                return done(Quality::Dvdscr);
            }
            if g("cam") {
                return done(Quality::Cam);
            }
            if g("ts") {
                return done(Quality::Telesync);
            }
            if g("tc") {
                return done(Quality::Telecine);
            }
            if g("wp") {
                return done(Quality::Workprint);
            }
            if g("regional") {
                return done(Quality::Regional);
            }
        }
        if g("hdtv") {
            if is_match(&MPEG2, normalized) {
                return done(Quality::RawHd);
            }
            return done(match res {
                2160 => Quality::Hdtv2160p,
                1080 => Quality::Hdtv1080p,
                720 => Quality::Hdtv720p,
                _ if name.contains("[HDTV]") => Quality::Hdtv720p,
                _ => Quality::Sdtv,
            });
        }
        if g("bdrip") || g("brrip") {
            return done(match res {
                720 => Quality::Bluray720p,
                1080 => Quality::Bluray1080p,
                2160 => Quality::Bluray2160p,
                576 if movie => Quality::Bluray576p,
                _ => Quality::Bluray480p,
            });
        }
        if movie && g("dvdr") {
            return done(Quality::DvdR);
        }
        if g("dvd") {
            return done(Quality::Dvd);
        }
        if g("pdtv") || g("sdtv") || g("dsr") || g("tvrip") {
            if res == 1080 || contains_ci(normalized, "1080p") {
                return done(Quality::Hdtv1080p);
            }
            if res == 720 || contains_ci(normalized, "720p") {
                return done(Quality::Hdtv720p);
            }
            if is_match(&HIGH_DEF_PDTV, normalized) {
                return done(Quality::Hdtv720p);
            }
            return done(Quality::Sdtv);
        }
    }

    if source.is_none() && remux && res != 0 {
        match res {
            480 => return done(Quality::Bluray480p),
            720 => return done(Quality::Bluray720p),
            2160 => return done(Quality::Remux2160p),
            1080 => return done(Quality::Remux1080p),
            _ => {}
        }
    }

    if is_match(&ANIME_BLURAY, normalized) {
        if matches!(res, 360 | 480 | 540 | 576) || contains_ci(normalized, "480p") {
            return done(Quality::Dvd);
        }
        if res == 1080 || contains_ci(normalized, "1080p") {
            return done(if remux { Quality::Remux1080p } else { Quality::Bluray1080p });
        }
        if res == 2160 || contains_ci(normalized, "2160p") {
            return done(if remux { Quality::Remux2160p } else { Quality::Bluray2160p });
        }
        if remux && res != 720 {
            return done(Quality::Remux1080p);
        }
        return done(Quality::Bluray720p);
    }

    if is_match(&ANIME_WEBDL, normalized) {
        if matches!(res, 360 | 480 | 540 | 576) || contains_ci(normalized, "480p") {
            return done(Quality::Webdl480p);
        }
        if res == 1080 || contains_ci(normalized, "1080p") {
            return done(Quality::Webdl1080p);
        }
        if res == 2160 || contains_ci(normalized, "2160p") {
            return done(Quality::Webdl2160p);
        }
        return done(Quality::Webdl720p);
    }

    if res != 0 {
        let (src, modifier) = if remux {
            (Source::Bluray, Modifier::Remux)
        } else {
            let q = quality_for_extension(path_extension(name), flavor);
            (q.source(), Modifier::None)
        };
        let pick = |r: u32, fallback: Quality| {
            if src == Source::Unknown {
                fallback
            } else {
                Quality::find(src, r, modifier)
            }
        };
        match res {
            2160 => return done(pick(2160, Quality::Hdtv2160p)),
            1080 => return done(pick(1080, Quality::Hdtv1080p)),
            720 => return done(pick(720, Quality::Hdtv720p)),
            360 | 480 | 540 | 576 => return done(pick(480, Quality::Sdtv)),
            _ => {}
        }
    }

    if codec_is("x264") {
        return done(Quality::Sdtv);
    }
    if contains_ci(normalized, "848x480") {
        return done(if normalized.contains("dvd") {
            Quality::Dvd
        } else if contains_ci(normalized, "bluray") {
            Quality::Bluray480p
        } else {
            Quality::Sdtv
        });
    }
    if contains_ci(normalized, "1280x720") {
        return done(if contains_ci(normalized, "bluray") { Quality::Bluray720p } else { Quality::Hdtv720p });
    }
    if contains_ci(normalized, "1920x1080") {
        return done(if contains_ci(normalized, "bluray") { Quality::Bluray1080p } else { Quality::Hdtv1080p });
    }
    if contains_ci(normalized, "bluray720p") {
        return done(Quality::Bluray720p);
    }
    if contains_ci(normalized, "bluray1080p") {
        return done(Quality::Bluray1080p);
    }
    if contains_ci(normalized, "bluray2160p") {
        return done(Quality::Bluray2160p);
    }
    if let Ok(Some(c)) = crate::parser::common::caps(&OTHER_SOURCE, normalized) {
        if c.name("sdtv").is_some() {
            return done(Quality::Sdtv);
        }
        if c.name("hdtv").is_some() {
            return done(Quality::Hdtv720p);
        }
    }
    done(Quality::Unknown)
}
