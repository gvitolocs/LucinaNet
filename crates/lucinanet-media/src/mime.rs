/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `canPlayType` and `<source type>`: what this backend can really decode, and nothing more, so a page picks a format that plays.
//!
//! The original is the GStreamer backend's `can_play_type` (servo-media `registry_scanner.rs`): a container it has a demuxer for,
//! and every codec in `codecs=` one it has a decoder for, is "probably"; a known container with no `codecs=` is "maybe"; anything else
//! "". Here the lists are the decoders LucinaOS has (`lucina_video_play::decode`, `lucina_audio_decode`), measured on the laptop
//! (`docs/research/juno-media.md`), and Opus through opus1. H.265 is only the video block's (Venus), so it is offered only where
//! `/dev/lucina-venus` exists.
//!
//! Formats above Juno's measured smooth limits are refused as well, so that YouTube's format choice stays within them: it asks
//! `MediaSource.isTypeSupported` for every format it is offered, and this answers for each. The codec level carried in `codecs=` (RFC 6381
//! `avc1.64002a` is level 42, `vp09.00.40.08` is level 40, `av01.0.08M.08` is seq level 8) and any `width=`/`height=`/`framerate=`
//! parameters are checked against `Caps`. The limits are the `Caps::default_for` defaults until laptop measurements replace them;
//! `LUCINANET_MEDIA_CAPS` overrides them for measurement runs, e.g. `LUCINANET_MEDIA_CAPS=avc1=42,fps=60`.

use crate::rules::caps::{self as caps_rules, Limits};
use servo_media::SupportsMediaType;

/// What Juno plays smoothly: the highest codec level per video codec, and the largest picture and frame rate.
///
/// `can_play_type_with` refuses anything above these, so a page (YouTube) never picks a format that would drop frames.
/// The limits and the decisions over them are `caps_rules::Limits`, which Verus checks.
pub type Caps = Limits;

impl Caps {
    /// No limits: every level, any size, any frame rate.
    pub fn unlimited() -> Caps {
        Caps {
            avc_level: 255,
            vp9_level: 255,
            av1_level: Some(255),
            max_width: 1_000_000,
            max_height: 1_000_000,
            max_fps_milli: u64::MAX / 2,
        }
    }

    /// The defaults for this machine: with the Venus video block, without it.
    pub fn default_for(venus: bool) -> Caps {
        caps_rules::defaults(venus)
    }

    /// `base` with a comma-separated `key=value` list applied: `avc1`, `vp09`, `av01` (value `none` clears AV1), `w`, `h`, `fps`.
    /// Unknown keys and unparsable values are ignored.
    pub fn parse_override(s: &str, base: Caps) -> Caps {
        let mut c = base;
        for part in s.split(',') {
            let Some((k, v)) = part.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "avc1" => {
                    if let Ok(n) = v.parse::<u8>() {
                        c.avc_level = n;
                    }
                },
                "vp09" => {
                    if let Ok(n) = v.parse::<u8>() {
                        c.vp9_level = n;
                    }
                },
                "av01" => {
                    if v.eq_ignore_ascii_case("none") {
                        c.av1_level = None;
                    } else if let Ok(n) = v.parse::<u8>() {
                        c.av1_level = Some(n);
                    }
                },
                "w" => {
                    if let Ok(n) = v.parse::<u32>() {
                        c.max_width = n;
                    }
                },
                "h" => {
                    if let Ok(n) = v.parse::<u32>() {
                        c.max_height = n;
                    }
                },
                "fps" => {
                    if let Ok(n) = v.parse::<f64>() {
                        c.max_fps_milli = (n * 1000.0).round() as u64;
                    }
                },
                _ => {},
            }
        }
        c
    }

    /// The caps in force now: the defaults, overridden by `LUCINANET_MEDIA_CAPS` if it is set (read once).
    pub fn current(venus: bool) -> Caps {
        static RAW: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        let base = Caps::default_for(venus);
        match RAW.get_or_init(|| std::env::var("LUCINANET_MEDIA_CAPS").ok()) {
            Some(s) => Caps::parse_override(s, base),
            None => base,
        }
    }
}

/// The containers, and whether each carries pictures.
fn container(essence: &str) -> Option<Kind> {
    Some(match essence {
        "video/mp4" | "video/quicktime" => Kind::Mp4Video,
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" | "audio/aac" | "audio/x-aac" => Kind::Mp4Audio,
        "video/webm" | "video/x-matroska" => Kind::WebmVideo,
        "audio/webm" | "audio/x-matroska" => Kind::WebmAudio,
        "audio/mpeg" | "audio/mp3" | "audio/x-mpeg" => Kind::Mp3,
        "audio/ogg" | "application/ogg" => Kind::Ogg,
        "audio/flac" | "audio/x-flac" => Kind::Flac,
        "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => Kind::Wav,
        _ => return None,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Mp4Video,
    Mp4Audio,
    WebmVideo,
    WebmAudio,
    Mp3,
    Ogg,
    Flac,
    Wav,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Codec {
    Video,
    Audio,
}

/// What one entry of `codecs=` is, if this can decode it. RFC 6381 names for MP4, WebM's own for Matroska.
fn codec(name: &str, venus: bool) -> Option<Codec> {
    let n = name.trim().to_ascii_lowercase();
    let head = n.split('.').next().unwrap_or("");
    match head {
        "avc1" | "avc3" | "h264" => Some(Codec::Video),
        "hvc1" | "hev1" | "h265" | "hevc" if venus => Some(Codec::Video),
        "vp09" | "vp9" | "av01" | "av1" => Some(Codec::Video),
        // AAC: LC (40.2), HE (40.5) and HE v2 (40.29) through syom; MP3 in MP4 (40.34, 69, 6b).
        "mp4a" => match n.as_str() {
            "mp4a" | "mp4a.40" | "mp4a.40.2" | "mp4a.40.02" | "mp4a.40.5" | "mp4a.40.05" | "mp4a.40.29" | "mp4a.40.34" | "mp4a.69"
            | "mp4a.6b" => Some(Codec::Audio),
            _ => None,
        },
        "mp3" | "vorbis" | "flac" | "alac" | "opus" => Some(Codec::Audio),
        // WAV's format tags: 1 is PCM.
        "1" => Some(Codec::Audio),
        _ => None,
    }
}

fn allowed(kind: Kind, c: Codec, name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    match kind {
        Kind::Mp4Video => true,
        Kind::Mp4Audio => c == Codec::Audio,
        Kind::WebmVideo => {
            n.starts_with("vp09") || n == "vp9" || n.starts_with("av01") || n == "av1" || n == "vorbis" || n == "opus" || n.starts_with("avc1")
                || n.starts_with("hev1") || n.starts_with("hvc1")
        },
        Kind::WebmAudio => n == "vorbis" || n == "opus",
        Kind::Mp3 => n == "mp3" || n == "mp4a.40.34" || n == "mp4a.69" || n == "mp4a.6b",
        Kind::Ogg => n == "vorbis" || n == "flac" || n == "opus",
        Kind::Flac => n == "flac",
        Kind::Wav => n == "1",
    }
}

/// RFC 6381 `avc1.PPCCLL`: the level_idc is the last two of the six hex digits after the dot (`64002a` -> 42).
/// Anything without exactly six hex digits carries no level here.
fn avc_level_idc(n: &str) -> Option<u8> {
    let hex = n.split_once('.')?.1;
    if hex.len() != 6 {
        return None;
    }
    let mut digits = [0u8; 6];
    digits.copy_from_slice(hex.as_bytes());
    caps_rules::avc_level_from_hex(digits)
}

/// `vp09.PP.LL.DD...`: LL, the second field after the name, is the level in decimal (`vp09.00.40.08` -> 40).
fn vp9_level(n: &str) -> Option<u8> {
    let mut fields = n.split('.');
    match fields.next() {
        Some("vp09") | Some("vp9") => {},
        _ => return None,
    }
    fields.next()?;
    fields.next()?.parse::<u8>().ok()
}

/// `av01.P.LLT.DD...`: LLT, the second field after the name, is two decimal digits then a tier letter (`08M` -> 8).
fn av1_seq_level_idx(n: &str) -> Option<u8> {
    let mut fields = n.split('.');
    match fields.next() {
        Some("av01") | Some("av1") => {},
        _ => return None,
    }
    fields.next()?;
    let llt = fields.next()?;
    let b = llt.as_bytes();
    if b.len() < 2 {
        return None;
    }
    caps_rules::two_decimal_digits(b[0], b[1])
}

/// Whether a video codec's name is within `caps`: its level, where the name carries one, is not above the cap.
/// Names without a level (bare `avc1`, `vp9`, `av1`) are always within it, except AV1, which needs a cap at all.
fn video_within_caps(name: &str, caps: &Caps) -> bool {
    let n = name.trim().to_ascii_lowercase();
    let head = n.split('.').next().unwrap_or("");
    match head {
        "avc1" | "avc3" | "h264" => caps_rules::level_within(caps_rules::VideoCodec::Avc, avc_level_idc(&n), caps),
        "vp09" | "vp9" => caps_rules::level_within(caps_rules::VideoCodec::Vp9, vp9_level(&n), caps),
        "av01" | "av1" => caps_rules::level_within(caps_rules::VideoCodec::Av1, av1_seq_level_idx(&n), caps),
        _ => true,
    }
}

pub fn can_play_type_with(mime: &str, venus: bool, caps: &Caps) -> SupportsMediaType {
    let mut parts = mime.split(';');
    let essence = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let Some(kind) = container(&essence) else {
        return SupportsMediaType::No;
    };
    let mut codecs = None;
    let mut width = None;
    let mut height = None;
    let mut framerate = None;
    for p in parts {
        let Some((k, v)) = p.split_once('=') else { continue };
        let v = v.trim().trim_matches('"');
        if k.trim().eq_ignore_ascii_case("codecs") {
            codecs = Some(v.to_owned());
        } else if k.trim().eq_ignore_ascii_case("width") {
            if let Ok(n) = v.parse::<u64>() {
                width = Some(n);
            }
        } else if k.trim().eq_ignore_ascii_case("height") {
            if let Ok(n) = v.parse::<u64>() {
                height = Some(n);
            }
        } else if k.trim().eq_ignore_ascii_case("framerate") {
            if let Ok(n) = v.parse::<f64>() {
                framerate = Some((n * 1000.0).round() as u64);
            }
        }
    }
    // Size and frame rate caps: a picture too big to decode smoothly is refused whatever the codec says.
    if !caps_rules::size_within(width, height, caps) || !caps_rules::fps_within(framerate, caps) {
        return SupportsMediaType::No;
    }
    let Some(list) = codecs else {
        return SupportsMediaType::Maybe;
    };
    let mut any = false;
    for name in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        any = true;
        match codec(name, venus) {
            Some(c) if allowed(kind, c, name) => {
                if c == Codec::Video && !video_within_caps(name, caps) {
                    return SupportsMediaType::No;
                }
            },
            _ => return SupportsMediaType::No,
        }
    }
    if any { SupportsMediaType::Probably } else { SupportsMediaType::Maybe }
}

pub fn can_play_type(mime: &str, venus: bool) -> SupportsMediaType {
    can_play_type_with(mime, venus, &Caps::current(venus))
}

/// Whether the video block is there to take H.265.
pub fn venus_present() -> bool {
    std::path::Path::new("/dev/lucina-venus").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use servo_media::SupportsMediaType::{Maybe, No, Probably};

    #[test]
    fn what_youtube_and_common_pages_ask() {
        assert_eq!(can_play_type_with("video/mp4; codecs=\"avc1.42E01E, mp4a.40.2\"", false, &Caps::unlimited()), Probably);
        assert_eq!(can_play_type_with("video/mp4; codecs=\"avc1.640028\"", false, &Caps::unlimited()), Probably);
        assert_eq!(can_play_type_with("audio/mp4; codecs=\"mp4a.40.5\"", false, &Caps::unlimited()), Probably);
        assert_eq!(can_play_type_with("video/webm; codecs=\"vp9\"", false, &Caps::unlimited()), Probably);
        assert_eq!(can_play_type_with("video/webm; codecs=\"vp09.00.10.08\"", false, &Caps::unlimited()), Probably);
        assert_eq!(can_play_type_with("video/mp4; codecs=\"av01.0.05M.08\"", false, &Caps::unlimited()), Probably);
        assert_eq!(can_play_type_with("audio/mpeg", false, &Caps::unlimited()), Maybe);
        assert_eq!(can_play_type_with("video/mp4", false, &Caps::unlimited()), Maybe);
        assert_eq!(can_play_type_with("audio/ogg; codecs=vorbis", false, &Caps::unlimited()), Probably);
        // Opus through opus1: what YouTube offers in WebM.
        assert_eq!(can_play_type_with("audio/webm; codecs=\"opus\"", false, &Caps::unlimited()), Probably);
        assert_eq!(can_play_type_with("video/webm; codecs=\"vp9, opus\"", false, &Caps::unlimited()), Probably);
        assert_eq!(can_play_type_with("audio/mp4; codecs=\"opus\"", false, &Caps::unlimited()), Probably);
        assert_eq!(can_play_type_with("audio/ogg; codecs=opus", false, &Caps::unlimited()), Probably);
    }

    #[test]
    fn what_cannot_be_decoded_here_is_refused() {
        assert_eq!(can_play_type_with("video/webm; codecs=\"vp8, opus\"", false, &Caps::unlimited()), No, "no VP8 decoder");
        assert_eq!(can_play_type_with("video/webm; codecs=\"vp8, vorbis\"", false, &Caps::unlimited()), No);
        assert_eq!(can_play_type_with("video/mp4; codecs=\"hvc1.1.6.L93.B0\"", false, &Caps::unlimited()), No);
        assert_eq!(can_play_type_with("video/mp4; codecs=\"hvc1.1.6.L93.B0\"", true, &Caps::unlimited()), Probably);
        assert_eq!(can_play_type_with("video/ogg; codecs=theora", false, &Caps::unlimited()), No);
        assert_eq!(can_play_type_with("audio/mp4; codecs=\"avc1.42E01E\"", false, &Caps::unlimited()), No, "no pictures in an audio type");
        assert_eq!(can_play_type_with("video/mp4; codecs=\"ec-3\"", false, &Caps::unlimited()), No);
        assert_eq!(can_play_type_with("application/x-mpegurl", false, &Caps::unlimited()), No);
        assert_eq!(can_play_type_with("", false, &Caps::unlimited()), No);
    }

    #[test]
    fn parameters_are_read_as_mime_types_are() {
        assert_eq!(can_play_type_with("VIDEO/MP4 ; CODECS=avc1.4d401f", false, &Caps::unlimited()), Probably);
        assert_eq!(can_play_type_with("video/mp4; codecs=\"\"", false, &Caps::unlimited()), Maybe);
        assert_eq!(can_play_type_with("audio/wav; codecs=1", false, &Caps::unlimited()), Probably);
    }

    #[test]
    fn youtube_formats_on_juno() {
        let caps = Caps::default_for(true);
        let c = |mime: &str| can_play_type_with(mime, true, &caps);
        // itag 299, 1080p60: level 42 is above the Venus cap of 41.
        assert_eq!(c("video/mp4; codecs=\"avc1.64002a\""), No);
        // itag 137, 1080p30: level 40.
        assert_eq!(c("video/mp4; codecs=\"avc1.640028\""), Probably);
        // itag 298, 720p60: level 32.
        assert_eq!(c("video/mp4; codecs=\"avc1.640020\""), Probably);
        // itag 136, 720p30: level 31.
        assert_eq!(c("video/mp4; codecs=\"avc1.4d401f\""), Probably);
        assert_eq!(c("video/webm; codecs=\"vp09.00.40.08\""), Probably);
        assert_eq!(c("video/webm; codecs=\"vp09.00.41.08\""), No);
        assert_eq!(c("video/webm; codecs=\"vp9\""), Probably);
        // No AV1 decoder cap on Venus: any av01 is refused.
        assert_eq!(c("video/mp4; codecs=\"av01.0.08M.08\""), No);
        assert_eq!(c("video/mp4; codecs=\"av01\""), No);
        assert_eq!(c("audio/webm; codecs=\"opus\""), Probably);
        assert_eq!(c("audio/mp4; codecs=\"mp4a.40.2\""), Probably);
        // Size and frame rate parameters.
        assert_eq!(c("video/webm; codecs=\"vp9\"; width=3840; height=2160; framerate=60"), No);
        assert_eq!(c("video/webm; codecs=\"vp9\"; width=1920; height=1080; framerate=30"), Probably);
        assert_eq!(c("video/mp4; codecs=\"avc1.640028\"; framerate=\"29.97\""), Probably);
        assert_eq!(c("video/mp4; codecs=\"avc1.640028\"; framerate=60"), No);
    }

    #[test]
    fn without_venus() {
        let caps = Caps::default_for(false);
        let c = |mime: &str| can_play_type_with(mime, false, &caps);
        assert_eq!(c("video/mp4; codecs=\"avc1.640028\""), No);
        assert_eq!(c("video/mp4; codecs=\"avc1.4d401f\""), Probably);
        assert_eq!(c("video/webm; codecs=\"vp09.00.31.08\""), Probably);
        assert_eq!(c("video/webm; codecs=\"vp09.00.40.08\""), No);
    }

    #[test]
    fn overrides() {
        let c = Caps::parse_override("avc1=42, fps=60, av01=8, w=2560, junk=1, vp09=x", Caps::default_for(true));
        assert_eq!(c.avc_level, 42);
        assert_eq!(c.max_fps_milli, 60000);
        assert_eq!(c.av1_level, Some(8));
        assert_eq!(c.max_width, 2560);
        assert_eq!(c.vp9_level, 40);
        assert_eq!(c.max_height, 1088);
        assert_eq!(Caps::parse_override("av01=none", Caps::unlimited()).av1_level, None);
    }
}
