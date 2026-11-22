#!/usr/bin/env bash
# Shows that Verus can fail on our shipped rule files and on the registry model: each line plants one bug in a copy, and Verus must
# refuse it. As LucinaOS verification/*/mutants.sh. Run on a Linux desktop from the LucinaNet root, through
# the verification script:
#   verification/mutants.sh
set -uo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
servo=${SERVO:-$root/third_party/servo}
source "$root/scripts/verus-pin.sh"   # resolves `verus`, prints it, and refuses a version other than verification/verus.version
work=$(mktemp -d "${TMPDIR:-/tmp}/lucinanet-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local file=$1 from=$2 to=$3 shipped=$4
    local text; text=$(<"$file")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND in $(basename "$file"): $from"; fail=1; return ;;
    esac
    printf '%s\n' "${text/"$from"/"$to"}" > "$work/mutant.rs"
    if [ "$shipped" = shipped ]; then
        local body; body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$work/mutant.rs")
        printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body" > "$work/check.rs"
    else
        cp "$work/mutant.rs" "$work/check.rs"
    fi
    if "$verus" "$work/check.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $(basename "$file"): $from -> $to"; fail=1
    else
        echo "refused: $(basename "$file"): $from -> $to"
    fi
}
b=$servo/components/profile_store/rules/bounds.rs
e=$servo/components/profile_store/rules/evict.rs
w=$servo/components/zstd/window.rs
m=$root/verification/registry/model.rs
# Records: a fixed field past the end, a variable field one byte too long, a field over the limit, a record grown past its limit.
try "$b" 'if at > len || len - at < FIXED {' 'if at > len || len - at < FIXED - 1 {' shipped
try "$b" 'if room < n as usize {' 'if room + 1 < n as usize {' shipped
try "$b" '|| n > MAX_FIELD {' '|| n > MAX_FIELD + 1 {' shipped
try "$b" 'len <= MAX_RECORD && add <= MAX_RECORD - len' 'len <= MAX_RECORD && add <= MAX_RECORD - len + 1' shipped
# Eviction: one entry too many, an underflowing subtraction, skipping an entry.
try "$e" 'while dropped < sizes.len() && left > max' 'while dropped < sizes.len() && left >= max' shipped
try "$e" 'left = if left >= size { left - size } else { 0 };' 'left = left - size;' shipped
try "$e" 'dropped = dropped + 1;' 'dropped = dropped + 2;' shipped
# zstd: twice the window, a window read from the wrong byte, a header taken as whole one byte early, the dictionary check gone,
# the high half of an 8-byte content size ignored, a mantissa read as 4 bits.
try "$w" 'if window > MAX_WINDOW {' 'if window > MAX_WINDOW * 2 {' shipped
try "$w" 'window_size(buf[5])' 'window_size(buf[4])' shipped
try "$w" 'if buf.len() < len {' 'if buf.len() + 1 < len {' shipped
try "$w" 'if buf[after_wd + i] != 0 {' 'if buf[after_wd + i] == 255 {' shipped
try "$w" 'hi * 4294967296 + lo' 'lo' shipped
try "$w" 'let mantissa = (wd & 7u8) as u64;' 'let mantissa = (wd & 15u8) as u64;' shipped
try "$w" 'buf[at] as u64 + 256 * (buf[at + 1] as u64) + 256' 'buf[at] as u64 + 256 * (buf[at + 1] as u64)' shipped
# Registry model: Servo's own order (row before directory), and a delete that removes the directory first.
try "$m" 'seq![Step::MakeDir(p), Step::CommitRow(p)]' 'seq![Step::CommitRow(p), Step::MakeDir(p)]' model
try "$m" 'seq![Step::CommitRemoval(p), Step::RemoveDir(p)]' 'seq![Step::RemoveDir(p), Step::CommitRemoval(p)]' model
# Media: a push taken while full, Enough signalled one byte early, Need without falling to the low mark, a read waiting past the
# slack, a push overrunning the next held run; a stream position past its block, one going back, time from frames off by a factor.
f=$root/crates/lucinanet-media/src/rules/flow.rs
c=$root/crates/lucinanet-media/src/rules/clock.rs
try "$f" 'Flow::Full => false,' 'Flow::Full => true,' shipped
try "$f" 'if ahead >= HIGH {' 'if ahead + 1 >= HIGH {' shipped
try "$f" 'if ahead <= LOW {' 'if ahead <= LOW + 1 {' shipped
try "$f" 'fetch_at <= pos && pos - fetch_at <= SLACK' 'fetch_at <= pos' shipped
try "$f" 'if len <= room { len } else { room }' 'len' shipped
try "$c" '} else if played - dev_start >= taken {' '} else if played - dev_start > taken + 1 {' shipped
try "$c" '        stream_start + (played - dev_start)
    }' '        stream_start + (played - dev_start) / 2
    }' shipped
try "$c" '((frames * 1000000) / rate) as i64' '((frames * 1000) / rate) as i64' shipped
# Picture handover model: an id's pins let go while a lock is open, a lock that pins nothing, a lock that found nothing left
# uncounted, publish into a pinned buffer, a lock that does not point its texture, a dropped surface taking the pins with it.
h=$root/verification/handover/model.rs
try "$h" 'Step::Unlock => if s.count == 1 {' 'Step::Unlock => if s.count <= 2 {' model
try "$h" 'held: s.held.insert(s.current->Some_0),' 'held: s.held,' model
try "$h" 'Step::LockNothing => State { count: s.count + 1, opened: s.opened + 1, ..s },' 'Step::LockNothing => State { opened: s.opened + 1, ..s },' model
try "$h" 's.current != Some(b) && s.frame != Some(b) && !s.held.contains(b)' 's.current != Some(b) && s.frame != Some(b)' model
try "$h" 'tex: s.tex.insert(k, s.current->Some_0),' 'tex: s.tex,' model
try "$h" 'Step::DropSurface => State { current: None, ..s },' 'Step::DropSurface => State { current: None, held: Set::empty(), ..s },' model
# MSE buffered ranges: an empty range kept, the wrong index advanced, a start from the earlier of two, a merged range that shrinks,
# a new range ending past its frame, the merge test without a range before it, the quota off by one byte, eviction one second
# behind the playback position, eviction at a key frame at the cut itself.
r=$root/crates/lucinanet-media/src/rules/buffered.rs
try "$r" 'if s < e {' 'if s <= e {' shipped
try "$r" 'if a[i].1 < b[j].1 { i = i + 1 } else { j = j + 1 }' 'if a[i].1 > b[j].1 { i = i + 1 } else { j = j + 1 }' shipped
try "$r" 'let s = if a[i].0 > b[j].0 { a[i].0 } else { b[j].0 };' 'let s = if a[i].0 < b[j].0 { a[i].0 } else { b[j].0 };' shipped
try "$r" 'out.push((ls, if e > le { e } else { le }));' 'out.push((ls, e));' shipped
try "$r" '            out.push((s, e));
            //@ proof {
            //@     owner = owner.push(n as int);' '            out.push((s, e + 1));
            //@ proof {
            //@     owner = owner.push(n as int);' shipped
try "$r" 'if n > 0 && s <= out[n - 1].1 + (e - s) {' 'if s <= out[n - 1].1 + (e - s) {' shipped
try "$r" 'bytes <= quota && incoming <= quota - bytes' 'bytes <= quota && incoming <= quota - bytes + 1' shipped
try "$r" '{ current_us - 10_000_000 }' '{ current_us - 1_000_000 }' shipped
try "$r" 'if p < until {' 'if p <= until {' shipped
# Pacing: a frame a full gap later still counted in the run, the fourth frame paced as if it were the third, a wait from
# the request instead of the last start, the 16 ms floor dropped, a 2x paint made 1x, an input hold a millisecond longer,
# the future-input check gone, a run not reset, an unpaced frame that waits.
p=$root/crates/lucinanet-servo/src/rules/pacing.rs
try "$p" 'Some(elapsed) if elapsed < RUN_GAP_US =>' 'Some(elapsed) if elapsed <= RUN_GAP_US =>' shipped
try "$p" 'if run.saturating_add(1) < UNPACED_FRAMES {' 'if run.saturating_add(1) <= UNPACED_FRAMES {' shipped
try "$p" '(next_us.saturating_sub(elapsed), run.saturating_add(1))' '(next_us, run.saturating_add(1))' shipped
try "$p" 'FASTEST_FRAME_US.max(last_paint_us.saturating_mul(2))' 'last_paint_us.saturating_mul(2)' shipped
try "$p" 'FASTEST_FRAME_US.max(last_paint_us.saturating_mul(2))' 'FASTEST_FRAME_US.max(last_paint_us)' shipped
try "$p" 'now_ms.saturating_sub(INPUT_HOLD_MS - 2)' 'now_ms.saturating_sub(INPUT_HOLD_MS - 1)' shipped
try "$p" '&& last_input_ms <= now_ms.saturating_add(1)' '' shipped
try "$p" '_ => (0, 0),' '_ => (0, run),' shipped
try "$p" '(0, run.saturating_add(1))' '(1, run.saturating_add(1))' shipped
# Caps: AV1 allowed with no cap, VP9 checked against the H.264 cap, a level one above the cap accepted, the Venus H.264
# default raised to 42, the Venus AV1 default given a level, the side cap one too large, the area check dropped, the
# half-step frame-rate slack doubled, the wrong hex digit read for the H.264 level, a non-digit taken as an AV1 digit.
c=$root/crates/lucinanet-media/src/rules/caps.rs
try "$c" '            None => false,
            Some(max) => match level {' '            None => true,
            Some(max) => match level {' shipped
try "$c" '        (VideoCodec::Vp9, Some(n)) => n <= l.vp9_level,
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

//@ proof' '        (VideoCodec::Vp9, Some(n)) => n <= l.avc_level,
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

//@ proof' shipped
try "$c" '        (VideoCodec::Avc, Some(n)) => n <= l.avc_level,
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

//@ proof' '        (VideoCodec::Avc, Some(n)) => n <= l.avc_level.saturating_add(1),
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

//@ proof' shipped
try "$c" '    if venus {
        Limits {
            avc_level: 41,' '    if venus {
        Limits {
            avc_level: 42,' shipped
try "$c" '    if venus {
        Limits {
            avc_level: 41,
            vp9_level: 40,
            av1_level: None,' '    if venus {
        Limits {
            avc_level: 41,
            vp9_level: 40,
            av1_level: Some(8),' shipped
try "$c" '        if w > cap {' '        if w > cap.saturating_add(1) {' shipped
try "$c" '            w * h <= l.max_width as u64 * l.max_height as u64' '            true' shipped
try "$c" 'Some(m) => m <= l.max_fps_milli.saturating_add(500),' 'Some(m) => m <= l.max_fps_milli.saturating_add(1000),' shipped
try "$c" 'Some(16 * hex_digit(hex[4]) + hex_digit(hex[5]))' 'Some(16 * hex_digit(hex[3]) + hex_digit(hex[5]))' shipped
try "$c" '    if a < '\''0'\'' as u8 || a > '\''9'\'' as u8 || b < '\''0'\'' as u8 || b > '\''9'\'' as u8 {
        return None;' '    if a < '\''0'\'' as u8 || a > '\''9'\'' as u8 || b < '\''0'\'' as u8 {
        return None;' shipped
exit "$fail"
