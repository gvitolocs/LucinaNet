/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// Juno's video caps as pure decisions over integers: the highest codec level per video codec, the largest picture,
// the smoothest frame rate. `crates/lucinanet-media/src/mime.rs` keeps the MIME and `codecs=` string parsing and calls
// these (its `Caps` is this `Limits`). Verus checks this very file (scripts/verify-shipped.sh), proofs in `//@` lines
//.
//
// Proved: AV1 is refused under the defaults for every level and for no level; with the Venus video block H.264 is
// accepted exactly up to level 41 and VP9 exactly up to 40, without it both exactly up to 31; a lower level is
// accepted whenever a higher one is; a picture is never accepted with a side or an area above the limits, for any
// width or height, and `None`/`None` is accepted; a frame rate is accepted exactly up to the cap plus half a frame
// step, in frames per second times 1000, and `None`; the level out of an RFC 6381 `avc1.PPCCLL` name and out of an
// AV1 `LLT` field are the right digits, and no other name is given a level.

/// What Juno plays smoothly: the highest codec level per video codec, and the largest picture and frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Limits {
    pub avc_level: u8,
    pub vp9_level: u8,
    pub av1_level: Option<u8>,
    pub max_width: u32,
    pub max_height: u32,
    /// The smoothest frame rate, in frames per second times 1000 (29.97 fps is 29970).
    pub max_fps_milli: u64,
}

/// A video codec that carries a level in its `codecs=` name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum VideoCodec {
    Avc,
    Vp9,
    Av1,
}

//@ pub open spec fn spec_limits(venus: bool) -> Limits {
//@     if venus {
//@         Limits {
//@             avc_level: 41,
//@             vp9_level: 40,
//@             av1_level: None,
//@             max_width: 1920,
//@             max_height: 1088,
//@             max_fps_milli: 30000,
//@         }
//@     } else {
//@         Limits {
//@             avc_level: 31,
//@             vp9_level: 31,
//@             av1_level: None,
//@             max_width: 1280,
//@             max_height: 720,
//@             max_fps_milli: 30000,
//@         }
//@     }
//@ }

/// The defaults for this machine: with the Venus video block, without it.
pub fn defaults(venus: bool) -> Limits //@ret
//@ ensures ret == spec_limits(venus),
{
    if venus {
        Limits {
            avc_level: 41,
            vp9_level: 40,
            av1_level: None,
            max_width: 1920,
            max_height: 1088,
            max_fps_milli: 30000,
        }
    } else {
        Limits {
            avc_level: 31,
            vp9_level: 31,
            av1_level: None,
            max_width: 1280,
            max_height: 720,
            max_fps_milli: 30000,
        }
    }
}

//@ pub open spec fn spec_level_within(codec: VideoCodec, level: Option<u8>, l: &Limits) -> bool {
//@     match (codec, level) {
//@         (VideoCodec::Avc, Some(n)) => n <= l.avc_level,
//@         (VideoCodec::Avc, None) => true,
//@         (VideoCodec::Vp9, Some(n)) => n <= l.vp9_level,
//@         (VideoCodec::Vp9, None) => true,
//@         (VideoCodec::Av1, _) => match l.av1_level {
//@             None => false,
//@             Some(max) => match level {
//@                 Some(n) => n <= max,
//@                 None => true,
//@             },
//@         },
//@     }
//@ }

/// Whether a codec's level, where the name carries one, is not above the cap. Names without a level (bare `avc1`,
/// `vp9`, `av1`) are always within the caps, except AV1, which needs a cap at all.
pub fn level_within(codec: VideoCodec, level: Option<u8>, l: &Limits) -> bool //@ret
//@ ensures ret == spec_level_within(codec, level, l),
{
    match (codec, level) {
        (VideoCodec::Avc, Some(n)) => n <= l.avc_level,
        (VideoCodec::Avc, None) => true,
        (VideoCodec::Vp9, Some(n)) => n <= l.vp9_level,
        (VideoCodec::Vp9, None) => true,
        (VideoCodec::Av1, _) => match l.av1_level {
            None => false,
            Some(max) => match level {
                Some(n) => n <= max,
                None => true,
            },
        },
    }
}

//@ proof fn level_av1_refused_at_defaults(venus: bool, level: Option<u8>)
//@     ensures !spec_level_within(VideoCodec::Av1, level, &spec_limits(venus)),
//@ {}

//@ proof fn level_venus_avc_exactly_up_to_41(level: u8)
//@     ensures spec_level_within(VideoCodec::Avc, Some(level), &spec_limits(true)) <==> level <= 41,
//@ {}

//@ proof fn level_venus_vp9_exactly_up_to_40(level: u8)
//@     ensures spec_level_within(VideoCodec::Vp9, Some(level), &spec_limits(true)) <==> level <= 40,
//@ {}

//@ proof fn level_without_venus_exactly_up_to_31(level: u8)
//@     ensures spec_level_within(VideoCodec::Avc, Some(level), &spec_limits(false)) <==> level <= 31,
//@     spec_level_within(VideoCodec::Vp9, Some(level), &spec_limits(false)) <==> level <= 31,
//@ {}

/// Monotone: a lower level is accepted whenever a higher one is, whatever the caps.
//@ proof fn level_within_monotone(codec: VideoCodec, lower: u8, higher: u8, l: &Limits)
//@     requires lower <= higher,
//@     ensures spec_level_within(codec, Some(higher), l) ==> spec_level_within(codec, Some(lower), l),
//@ {}

/// The largest side a picture may have: the bigger of the two size caps.
//@ pub open spec fn dim_cap(l: &Limits) -> int { if l.max_width >= l.max_height { l.max_width as int } else { l.max_height as int } }

/// The largest area a picture may have, in the limits' own units: `max_width * max_height`, never overflows.
//@ pub open spec fn area_cap(l: &Limits) -> int { l.max_width as int * l.max_height as int }

/// Whether a picture fits the caps: no side above the larger size cap, and when both sides are known, the area not
/// above the limits' area. A side that is not known is not against the caps.
pub fn size_within(width: Option<u64>, height: Option<u64>, l: &Limits) -> bool //@ret
//@ ensures
//@     ret <==> match (width, height) {
//@         (Some(w), Some(h)) => w <= dim_cap(l) && h <= dim_cap(l) && w * h <= area_cap(l),
//@         (Some(w), None) => w <= dim_cap(l),
//@         (None, Some(h)) => h <= dim_cap(l),
//@         (None, None) => true,
//@     },
{
    let cap = if l.max_width >= l.max_height { l.max_width as u64 } else { l.max_height as u64 };
    if let Some(w) = width {
        if w > cap {
            return false;
        }
    }
    if let Some(h) = height {
        if h > cap {
            return false;
        }
    }
    match (width, height) {
        (Some(w), Some(h)) => {
            // Both sides are at most a u32 cap here, so neither product overflows a u64.
            //@ assert(w * h <= 0xffff_ffff * 0xffff_ffff) by (nonlinear_arith) requires w <= 0xffff_ffff, h <= 0xffff_ffff;
            //@ assert(l.max_width as int * l.max_height as int <= 0xffff_ffff * 0xffff_ffff) by (nonlinear_arith)
            //@     requires l.max_width <= 0xffff_ffff, l.max_height <= 0xffff_ffff;
            w * h <= l.max_width as u64 * l.max_height as u64
        },
        _ => true,
    }
}

/// Whether a frame rate, in frames per second times 1000, is within the caps: up to half a frame step (500) above
/// the cap, and an unknown rate is not against them.
pub fn fps_within(fps_milli: Option<u64>, l: &Limits) -> bool //@ret
//@ ensures ret <==> match fps_milli {
//@     Some(m) => m as int <= l.max_fps_milli as int + 500,
//@     None => true,
//@ },
{
    match fps_milli {
        Some(m) => m <= l.max_fps_milli.saturating_add(500),
        None => true,
    }
}

/// A byte's value if it is an ASCII hex digit, else 16: only the first case is ever read.
fn hex_digit(b: u8) -> u8 //@ret
//@ ensures ret as int == spec_hex_digit(b),
{
    if b >= '0' as u8 && b <= '9' as u8 {
        b - '0' as u8
    } else if b >= 'a' as u8 && b <= 'f' as u8 {
        b - 'a' as u8 + 10
    } else if b >= 'A' as u8 && b <= 'F' as u8 {
        b - 'A' as u8 + 10
    } else {
        16
    }
}

/// Whether a byte is an ASCII hex digit, upper or lower case.
fn is_hex_digit(b: u8) -> bool //@ret
//@ ensures ret == spec_is_hex_digit(b),
{
    b >= '0' as u8 && b <= '9' as u8 || b >= 'a' as u8 && b <= 'f' as u8 || b >= 'A' as u8 && b <= 'F' as u8
}

//@ pub open spec fn spec_hex_digit(b: u8) -> int {
//@     if b >= '0' as u8 && b <= '9' as u8 {
//@         b as int - '0' as int
//@     } else if b >= 'a' as u8 && b <= 'f' as u8 {
//@         b as int - 'a' as int + 10
//@     } else if b >= 'A' as u8 && b <= 'F' as u8 {
//@         b as int - 'A' as int + 10
//@     } else {
//@         16
//@     }
//@ }

//@ pub open spec fn spec_is_hex_digit(b: u8) -> bool {
//@     (b >= '0' as u8 && b <= '9' as u8) || (b >= 'a' as u8 && b <= 'f' as u8) || (b >= 'A' as u8 && b <= 'F' as u8)
//@ }

//@ pub open spec fn spec_all_hex_digits(hex: [u8; 6]) -> bool {
//@     spec_is_hex_digit(hex[0]) && spec_is_hex_digit(hex[1]) && spec_is_hex_digit(hex[2])
//@         && spec_is_hex_digit(hex[3]) && spec_is_hex_digit(hex[4]) && spec_is_hex_digit(hex[5])
//@ }

/// The level out of an RFC 6381 `avc1.PPCCLL` name: the level_idc is the last two of the six hex digits
/// (`64002a` -> 42). `None` if any of the six is not an ASCII hex digit.
pub fn avc_level_from_hex(hex: [u8; 6]) -> Option<u8> //@ret
//@ ensures match (ret, spec_all_hex_digits(hex)) {
//@     (Some(l), true) => l as int == 16 * spec_hex_digit(hex[4]) + spec_hex_digit(hex[5]),
//@     (None, false) => true,
//@     _ => false,
//@ },
{
    if !(is_hex_digit(hex[0]) && is_hex_digit(hex[1]) && is_hex_digit(hex[2]) && is_hex_digit(hex[3]) && is_hex_digit(hex[4])
        && is_hex_digit(hex[5]))
    {
        return None;
    }
    Some(16 * hex_digit(hex[4]) + hex_digit(hex[5]))
}

/// The AV1 level out of an `LLT` field: its two leading decimal digits (`08M` -> 8), `None` if they are not digits.
pub fn two_decimal_digits(a: u8, b: u8) -> Option<u8> //@ret
//@ ensures match (ret, a, b) {
//@     (Some(v), a, b) if a >= '0' as u8 && a <= '9' as u8 && b >= '0' as u8 && b <= '9' as u8 =>
//@         v as int == 10 * (a as int - '0' as int) + (b as int - '0' as int),
//@     (None, _, _) => a < '0' as u8 || a > '9' as u8 || b < '0' as u8 || b > '9' as u8,
//@     _ => false,
//@ },
{
    if a < '0' as u8 || a > '9' as u8 || b < '0' as u8 || b > '9' as u8 {
        return None;
    }
    Some(10 * (a - '0' as u8) + (b - '0' as u8))
}

#[cfg(test)] //@omit
mod tests { //@omit
    use super::*; //@omit

    #[test] //@omit
    fn youtube_levels() { //@omit
        // itag 299, 1080p60: level 42, above the Venus cap of 41. //@omit
        assert_eq!(avc_level_from_hex(*b"64002a"), Some(42)); //@omit
        assert!(!level_within(VideoCodec::Avc, Some(42), &defaults(true))); //@omit
        // itag 136, 720p30: level 31, within both sets of defaults. //@omit
        assert_eq!(avc_level_from_hex(*b"4d401f"), Some(31)); //@omit
        assert!(level_within(VideoCodec::Avc, Some(31), &defaults(true))); //@omit
        assert!(level_within(VideoCodec::Avc, Some(31), &defaults(false))); //@omit
        // vp09.00.40.08 -> level 40, within Venus; vp09.00.41.08 -> 41, above it, and above 31 without Venus. //@omit
        assert!(level_within(VideoCodec::Vp9, Some(40), &defaults(true))); //@omit
        assert!(!level_within(VideoCodec::Vp9, Some(41), &defaults(true))); //@omit
        assert!(!level_within(VideoCodec::Vp9, Some(41), &defaults(false))); // 31 without Venus //@omit
        // av01.0.08M.08 -> seq level 8, but AV1 has no cap under the defaults. //@omit
        assert_eq!(two_decimal_digits(b'0', b'8'), Some(8)); //@omit
        assert!(!level_within(VideoCodec::Av1, Some(8), &defaults(true))); //@omit
        assert!(!level_within(VideoCodec::Av1, None, &defaults(false))); //@omit
    } //@omit

    #[test] //@omit
    fn boundaries() { //@omit
        let v = defaults(true); //@omit
        assert!(level_within(VideoCodec::Avc, Some(41), &v)); //@omit
        assert!(!level_within(VideoCodec::Avc, Some(42), &v)); //@omit
        assert!(level_within(VideoCodec::Vp9, Some(40), &v)); //@omit
        assert!(!level_within(VideoCodec::Vp9, Some(41), &v)); //@omit
        assert!(level_within(VideoCodec::Avc, None, &v)); //@omit
        // A rate at the cap plus half a frame step is the last one accepted. //@omit
        assert!(fps_within(Some(30000), &v)); //@omit
        assert!(fps_within(Some(30500), &v)); //@omit
        assert!(!fps_within(Some(30501), &v)); //@omit
        assert!(fps_within(None, &v)); //@omit
        // The last picture accepted on Venus: 1920 x 1080, area below 1920 * 1088. //@omit
        assert!(size_within(Some(1920), Some(1080), &v)); //@omit
        assert!(!size_within(Some(3840), Some(2160), &v)); //@omit
        assert!(!size_within(Some(2000), Some(60), &v)); //@omit
        assert!(size_within(None, None, &v)); //@omit
        // Uppercase hex, as in `avc1.42E01E`; the level is the last two digits. //@omit
        assert_eq!(avc_level_from_hex(*b"42E01E"), Some(30)); //@omit
        assert_eq!(avc_level_from_hex(*b"42E01g"), None); //@omit
    } //@omit
} //@omit
