//! Media properties used for naming and verification, derived from ffprobe output.
//! The formatting rules follow Radarr's `MediaInfoFormatter` (GPLv3) so names match existing files.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaInfo {
    pub video_codec: String,
    pub audio_codec: String,
    pub audio_channels: f64,
    pub dynamic_range_type: String,
    pub bit_depth: u32,
    pub width: u32,
    pub height: u32,
    pub runtime_secs: f64,
    pub audio_languages: Vec<String>,
    pub subtitles: Vec<String>,
    pub is_3d: bool,
}

fn scene_match(scene_name: &str, tokens: &[&str]) -> String {
    let scene = crate::parser::remove_file_extension(scene_name).to_lowercase();
    tokens.iter().find(|t| scene.contains(&t.to_lowercase())).unwrap_or(tokens.last().unwrap()).to_string()
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn tag<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get("tags")
        .and_then(|t| t.as_object())
        .and_then(|t| t.iter().find(|(key, _)| key.eq_ignore_ascii_case(k)))
        .and_then(|(_, x)| x.as_str())
        .unwrap_or("")
}

pub fn format_video_codec(format: &str, codec_id: &str, scene_name: &str) -> String {
    let id_upper = codec_id.to_uppercase();
    match format {
        "" => String::new(),
        _ if codec_id == "x264" => "x264".into(),
        "h264" => scene_match(scene_name, &["AVC", "x264", "h264"]),
        _ if codec_id == "x265" => "x265".into(),
        "hevc" => scene_match(scene_name, &["HEVC", "x265", "h265"]),
        "mpeg2video" => "MPEG2".into(),
        "mpeg1video" => "MPEG".into(),
        f if f == "mpeg4" || f.contains("msmpeg4") => {
            if id_upper == "XVID" {
                "XviD".into()
            } else if matches!(id_upper.as_str(), "DIV3" | "DX50" | "DIVX") {
                "DivX".into()
            } else {
                String::new()
            }
        }
        "vc1" => "VC1".into(),
        "av1" => "AV1".into(),
        f if f.contains("vp6") => "VP6".into(),
        "vp7" | "vp8" | "vp9" => format.to_uppercase(),
        "wmv1" | "wmv2" | "wmv3" => "WMV".into(),
        "qtrle" | "rpza" | "rv10" | "rv20" | "rv30" | "rv40" | "cinepak" | "rawvideo" | "msvideo1" => String::new(),
        other => other.to_string(),
    }
}

pub fn format_audio_codec(format: &str, codec_id: &str, profile: &str) -> String {
    match format {
        "" => String::new(),
        _ if codec_id == "thd+" => "TrueHD Atmos".into(),
        "truehd" => "TrueHD".into(),
        "flac" => "FLAC".into(),
        "dts" => match profile {
            "DTS:X" => "DTS-X",
            "DTS-HD MA" => "DTS-HD MA",
            "DTS-ES" => "DTS-ES",
            "DTS-HD HRA" => "DTS-HD HRA",
            "DTS Express" => "DTS Express",
            "DTS 96/24" => "DTS 96/24",
            _ => "DTS",
        }
        .into(),
        _ if codec_id == "ec+3" => "EAC3 Atmos".into(),
        "eac3" => "EAC3".into(),
        "ac3" => "AC3".into(),
        "aac" => if codec_id == "A_AAC/MPEG4/LC/SBR" { "HE-AAC" } else { "AAC" }.into(),
        "mp3" => "MP3".into(),
        "mp2" => "MP2".into(),
        "opus" => "Opus".into(),
        f if f.starts_with("pcm_") || f.starts_with("adpcm_") => "PCM".into(),
        "vorbis" => "Vorbis".into(),
        "wmav1" | "wmav2" | "wmapro" => "WMA".into(),
        other => other.to_string(),
    }
}

/// "5.1" style channel count from an ffprobe channel layout, falling back to the raw count.
fn channels(stream: &Value) -> f64 {
    let layout = str_of(stream, "channel_layout");
    let count = stream.get("channels").and_then(|c| c.as_f64()).unwrap_or(0.0);
    let lfe = layout.contains("LFE") || layout.contains(".1") || layout.contains("(side)") && count == 6.0;
    if let Some(n) = layout.split(['(', ' ']).next().and_then(|s| s.parse::<f64>().ok()) {
        return n;
    }
    match (count as u32, lfe) {
        (0, _) => 0.0,
        (n, true) if n > 1 => (n - 1) as f64 + 0.1,
        (n, _) => match layout {
            "stereo" => 2.0,
            "mono" => 1.0,
            _ if n == 6 => 5.1,
            _ if n == 8 => 7.1,
            _ => n as f64,
        },
    }
}

fn dynamic_range(video: &Value, frame_side_data: &[Value]) -> String {
    let bits = bit_depth(video);
    let transfer = str_of(video, "color_transfer");
    let primaries = str_of(video, "color_primaries");
    let side = |needle: &str| {
        frame_side_data
            .iter()
            .chain(video.get("side_data_list").and_then(|s| s.as_array()).into_iter().flatten())
            .find(|s| str_of(s, "side_data_type").contains(needle))
    };
    if let Some(dovi) = side("DOVI configuration record") {
        let compat = dovi.get("dv_bl_signal_compatibility_id").and_then(|x| x.as_i64()).unwrap_or(0);
        let hdr10_plus = side("HDR Dynamic Metadata SMPTE2094-40").is_some();
        return match compat {
            1 if hdr10_plus => "DV HDR10Plus",
            1 => "DV HDR10",
            2 => "DV SDR",
            4 => "DV HLG",
            6 => "DV HDR10",
            _ => "DV",
        }
        .into();
    }
    if bits < 10 {
        return String::new();
    }
    if primaries != "bt2020" && !transfer.contains("2084") && transfer != "arib-std-b67" {
        return String::new();
    }
    if transfer == "arib-std-b67" {
        return "HLG".into();
    }
    if transfer == "smpte2084" {
        if side("HDR Dynamic Metadata SMPTE2094-40").is_some() {
            return "HDR10Plus".into();
        }
        if side("Mastering display metadata").is_some() || side("Content light level metadata").is_some() {
            return "HDR10".into();
        }
        return "PQ".into();
    }
    String::new()
}

fn bit_depth(video: &Value) -> u32 {
    if let Some(b) = str_of(video, "bits_per_raw_sample").parse::<u32>().ok().filter(|b| *b > 0) {
        return b;
    }
    let pix = str_of(video, "pix_fmt");
    if pix.contains("p10") {
        10
    } else if pix.contains("p12") {
        12
    } else {
        8
    }
}

impl MediaInfo {
    /// Build from `ffprobe -print_format json -show_format -show_streams` output, optionally with
    /// `-show_frames -read_intervals %+#1` so HDR side data is present.
    pub fn from_ffprobe(probe: &Value, scene_name: &str) -> Option<MediaInfo> {
        let streams = probe.get("streams")?.as_array()?;
        let is_cover = |s: &Value| s.get("disposition").and_then(|d| d.get("attached_pic")).and_then(|x| x.as_i64()) == Some(1);
        let video = streams.iter().find(|s| str_of(s, "codec_type") == "video" && !is_cover(s))?;
        let audios: Vec<&Value> = streams.iter().filter(|s| str_of(s, "codec_type") == "audio").collect();
        let audio = audios
            .iter()
            .find(|s| s.get("disposition").and_then(|d| d.get("default")).and_then(|x| x.as_i64()) == Some(1))
            .or(audios.first())
            .copied();
        let frames: Vec<Value> = probe
            .get("frames")
            .and_then(|f| f.as_array())
            .and_then(|f| f.iter().find(|fr| str_of(fr, "media_type") == "video"))
            .and_then(|f| f.get("side_data_list"))
            .and_then(|s| s.as_array())
            .cloned()
            .unwrap_or_default();

        let video_codec_id = {
            let enc = tag(video, "encoder").to_lowercase();
            let lib = tag(video, "ENCODER").to_lowercase();
            let opts = tag(video, "ENCODER_OPTIONS").to_lowercase();
            let tag_str = str_of(video, "codec_tag_string");
            if enc.contains("x264") || lib.contains("x264") || opts.contains("x264") {
                "x264".to_string()
            } else if enc.contains("x265") || lib.contains("x265") || opts.contains("x265") {
                "x265".to_string()
            } else {
                tag_str.to_string()
            }
        };

        let duration = probe
            .get("format")
            .map(|f| str_of(f, "duration"))
            .and_then(|d| d.parse::<f64>().ok())
            .unwrap_or(0.0);

        let (audio_codec, audio_channels) = match audio {
            Some(a) => {
                let profile = str_of(a, "profile");
                let mut codec_id = str_of(a, "codec_tag_string").to_string();
                if profile.contains("Atmos") {
                    codec_id = if str_of(a, "codec_name") == "truehd" { "thd+".into() } else { "ec+3".into() };
                }
                (format_audio_codec(str_of(a, "codec_name"), &codec_id, profile), channels(a))
            }
            None => (String::new(), 0.0),
        };

        Some(MediaInfo {
            video_codec: format_video_codec(str_of(video, "codec_name"), &video_codec_id, scene_name),
            audio_codec,
            audio_channels,
            dynamic_range_type: dynamic_range(video, &frames),
            bit_depth: bit_depth(video),
            width: video.get("width").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            height: video.get("height").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            runtime_secs: duration,
            audio_languages: audios.iter().map(|a| tag(a, "language").to_string()).filter(|l| !l.is_empty()).collect(),
            subtitles: streams
                .iter()
                .filter(|s| str_of(s, "codec_type") == "subtitle")
                .map(|s| tag(s, "language").to_string())
                .filter(|l| !l.is_empty())
                .collect(),
            is_3d: false,
        })
    }

    pub fn audio_channels_formatted(&self) -> String {
        if self.audio_channels > 0.0 {
            format!("{:.1}", self.audio_channels)
        } else {
            String::new()
        }
    }
}
