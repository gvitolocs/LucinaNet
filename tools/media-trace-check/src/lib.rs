/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `media-trace-check` reads the stderr of a LucinaNet `--render` run with `LUCINANET_MEDIA_TRACE` set and asks one question:
//! did the media backend keep its invariants? It reports the first property that broke, with the lines around it, or says
//! which properties today's trace is too thin to judge.
//!
//! Every shape matched here is the shape the crate actually prints (`crates/lucinanet-media/src/{mixer,mse_play,player}.rs`,
//! read on the `mse-audio-stall` worktree on 2026-11-18, and the logs under `<work-dir>/lucinanet/` they wrote):
//!
//! - `media elem event=… paused=… ended=… seeking=… potentially_playing=… player_playing=… cmd=…` — the element's own view, to
//!   be emitted by the fork. The format is fixed here so the fork has something to aim at; no log taken before trace-format has one,
//!   and on those the four `MEDIA-ELEM-*` properties are reported as *not observed*.
//! - `show pts=… now=Some(µs)|None sound=Some(µs)|None` (player.rs) — the clock, once per picture shown.
//! - `starving queued=… eof=… now=…`, `played VP9 seconds=…`, `clock source: …`, `screen hold on|off`.
//! - `audio cmd Start pts=… paused=… running=…`, `audio cmd Paused true|false`, `audio cmd Stop` (mse_play.rs) — commands as the
//!   sound thread dequeues them. A `Paused true` here is not proof the page paused: opening a sound mirrors the player's
//!   `paused` onto the new stream (player.rs `Audio::set_paused` from its open path) and the seek landing sends a
//!   `Paused false` the page never asked for, so only the player's own commands say what the page wants.
//! - `audio wait start|frame|room ENTER: <fields>`, `… still (N ms): <fields>`, `… LEAVE…`, with `<fields>` =
//!   `queued=… playing=… [position_us=…] cursor_pts=… start_at=… frames=… buffered_end_us=… generation=… track=… buffers=…
//!   ended=… seeks=…`.
//! - `mixer stream N playing A -> B held=… [queued=…] from file:line`, `mixer stream N set_playing(true) …, latch H released
//!   from file:line`, and the 2 s heartbeat `mixer stream N: taken=… heard=… held=… queued=… playing=… | device written=…
//!   played=…` (mixer.rs). `held` is the latch: the frame the position stands at while the stream is paused, and `Some` beside
//!   `playing=true` is case F4.
//! - `player play|pause|resume|suspend|mute|seek … on THREAD mirror(paused=…,can_resume=…)` (player.rs) — the command as the
//!   page reached the backend.
//! - The lines that begin a player or a document: `opened: yes|no` (`player.rs:750`, in the player thread's `fn run`, the
//!   line after `open(&source, &shared)` returns — and before the `mse_attached` branch, so it prints for a MediaSource too),
//!   `no pictures: …` and `no sound: …` (`player.rs:1138`, `:1148`, both inside `fn open`), `MSE sound …`
//!   (`mse_play.rs:203`, `:211`, inside `MseSound::open`), `first frame after N ms` (`player.rs:1034`, the first picture out
//!   of the player that just opened) and the engine's `phase <mono> load started`
//!   (`crates/lucinanet-servo/src/engine.rs:312` — no `lucinanet-media:` mark, so it is matched on the raw line, after any
//!   `[t+ …]` stamp). clock-contract's state is per player and resets at every one of them, and element-contract-1 stops looking for
//!   the player's answer there: a navigation leaves the old element's clock standing at its last value and the new player
//!   starts at 0, which is two clocks and not a step backwards, and the old element's `potentially_playing` is a belief about
//!   a player that no longer exists — consent-navigation. Resetting at anything else is how a rule stops meaning what it says, so
//!   `MSE sound stopped:` (a decoder giving up mid-run, same player) and the other `phase` names reset nothing:
//!   `load complete` and `first frame painted` report the document that just ended rather than the one starting, and a load
//!   that is aborted prints neither, so a rule keyed on them would come back as a false positive exactly when the page is
//!   mid-change.
//!
//! A line that carries none of these is counted and ignored: context for a violation, never evidence for one.

use std::fmt::Write as _;

/// What every trace line carries, after the `[t+ …]` prefix and after any `log` envelope.
const MARK: &str = "lucinanet-media: ";

/// Every property this tool knows, in the order trace-format gives them.
pub const IDS: [&str; 8] = [
    "element-contract-1",
    "element-contract-2",
    "element-contract-3",
    "element-contract-4",
    "MEDIA-STR",
    "MEDIA-STR-RING",
    "clock-contract",
    "MEDIA-CUR",
];

/// How long the clock may stand still while the player plays with audio buffered, and how long a `play()` may take to reach
/// the player. Seconds; `--clock-window` and `--play-window` move them.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub clock_window: f64,
    pub play_window: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config { clock_window: 2.0, play_window: 1.0 }
    }
}

/// A value the trace writes as `Some(123)` or `None`, the `{:?}` of an `Option<i64>` of microseconds.
type Opt = Option<i64>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    /// `audio cmd Start`: the feed is rebuilt from `pts`. `paused` is what the sound thread believed at the time.
    Start {
        paused: bool,
    },
    Paused(bool),
    /// `audio cmd Stop`: a seek took the feed down.
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Player {
    Play,
    Pause,
    Resume,
    Suspend,
    Seek,
    Other,
}

/// A line that says a new player, or a new document, has begun: whatever clock the old one ran is not this one's clock, and
/// the seconds the old one showed must not be held against the new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    /// `opened: yes|no` (`player.rs:750`, in the player thread's `fn run`, after `open()` returns) — a player has been made
    /// and pointed at a source. The MSE branch follows this line, so it prints for a MediaSource-backed player too.
    Opened,
    /// `first frame after N ms` (player.rs) — the first picture out of a player that just started.
    FirstFrame,
    /// `no pictures: …`, `no sound: …` (`player.rs`, inside `fn open`), `MSE sound …` (`mse_play.rs`, inside
    /// `MseSound::open`) — one branch of an open failing. `MSE sound stopped:` is the same thread's decoder giving up
    /// mid-stream, which is the same player and the same clock, so it is not one of these.
    OpenFailure,
    /// `phase <mono> load started` (`crates/lucinanet-servo/src/engine.rs`) — the engine began a navigation. The runner stamps
    /// this line in some logs and not in others, so it is the one event read from the raw line rather than after the mark.
    LoadStarted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mixer {
    /// `playing A -> B held=H`. Reaching `true` goes through `set_playing`, which takes the latch, so `held` beside it is the
    /// line that says whether the mixer really cleared it.
    Transition { to: bool, held: Opt },
    /// `set_playing(true) …, latch H released`: a resume that found the latch set and took it down.
    Resume,
    /// The mixer's 2 s heartbeat, one line per open stream.
    Heartbeat { held: Opt, playing: bool, queued: i64, ring: Ring },
}

/// The ring counts, for MEDIA-STR-RING. Today's heartbeat reports `taken`/`heard` in frames and `queued` in samples and never
/// says what was pushed in, so the identity has nothing to stand on until a line carries `in=` and `out=` beside `queued=`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ring {
    pub pushed: Opt,
    pub out: Opt,
    pub queued: Opt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elem {
    /// `play`, `pause`, `seek-start`, `seek-end`, `waiting`, `ended`, `load`, `play-redundant`.
    pub event: String,
    pub paused: Option<bool>,
    pub ended: Option<bool>,
    pub seeking: Option<bool>,
    /// The element's own potentially-playing: it has data, is not paused, not ended and not seeking.
    pub potentially_playing: Option<bool>,
    pub player_playing: Option<bool>,
    /// `none`, `play` or `pause`: the command this event sent the player.
    pub cmd: Option<String>,
}

/// One trace line, as far as the properties care.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Elem(Elem),
    Show {
        now: Opt,
        sound: Opt,
    },
    Starving {
        eof: bool,
        now: Opt,
    },
    Cmd(Cmd),
    Wait {
        kind: String,
        playing: Option<bool>,
        queued: Option<i64>,
        position_us: Opt,
        cursor_pts: Opt,
        frames: Option<i64>,
        buffered_end_us: Opt,
        ended: bool,
    },
    Mixer(Mixer),
    Player(Player),
    Hold(bool),
    ClockSource,
    /// `played VP9 seconds=…`: the player's summary, printed as it closes. The run's media is over here.
    Summary,
    /// A line that begins a player or a document. No property of the old one carries across it.
    Session(Session),
}

#[derive(Debug, Clone)]
pub struct Line {
    /// 1-based position in the input, so a report leads back to the file.
    pub n: usize,
    /// Seconds from the `[t+ …]` prefix; `None` on a line the runner did not stamp.
    pub t: Option<f64>,
    pub raw: String,
    pub event: Option<Event>,
}

/// A property broken at a line of the log. `PartialEq` only: the timestamp is a `f64` read off the runner's stamp, and two
/// runs that printed the same seconds are the same fact even if the bits differ.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub id: &'static str,
    pub line: usize,
    pub t: Option<f64>,
    pub reason: String,
}

/// What one run of the checker decided.
#[derive(Debug, Clone)]
pub struct Report {
    pub lines: usize,
    /// Earliest first: the line number, then the id, so a tie goes to the property listed first.
    pub violations: Vec<Violation>,
    pub checked: Vec<&'static str>,
    /// Properties this trace said nothing about, and what was missing. Not counted as passes.
    pub not_observed: Vec<(&'static str, String)>,
}

impl Report {
    /// The text a run prints: the `ok` line, or the first violation with five lines before and after.
    pub fn render(&self, log: &[Line]) -> String {
        let mut out = String::new();
        if let Some(v) = self.violations.first() {
            let _ = writeln!(out, "{}: violated at {}, line {}: {}", v.id, stamp(v.t), v.line, v.reason);
            let _ = writeln!(out, "  5 lines before and after:");
            for l in self.around(log, v.line) {
                let _ = writeln!(out, "{} {}", if l.n == v.line { "=>" } else { "  " }, l.raw);
            }
            if self.violations.len() > 1 {
                let rest: Vec<String> =
                    self.violations[1..].iter().map(|v| format!("{} at {}", v.id, stamp(v.t))).collect();
                let _ = writeln!(out, "also violated ({}): {}", rest.len(), rest.join(", "));
            }
            return out;
        }
        let _ = write!(out, "ok: no violation ({} lines, properties checked: {})", self.lines, names(&self.checked));
        for (id, why) in &self.not_observed {
            let _ = write!(out, "\nnot observed: {id}: {why}");
        }
        out.push('\n');
        out
    }

    fn around<'a>(&self, log: &'a [Line], line: usize) -> Vec<&'a Line> {
        let Some(i) = log.iter().position(|l| l.n == line) else { return Vec::new() };
        let from = i.saturating_sub(5);
        log[from..(i + 6).min(log.len())].iter().collect()
    }

    /// False while a property went unjudged; `--strict` turns that into a non-zero exit.
    pub fn complete(&self) -> bool {
        self.not_observed.is_empty()
    }
}

fn stamp(t: Option<f64>) -> String {
    match t {
        Some(t) => format!("t+{t:.3}"),
        None => "(no timestamp)".to_string(),
    }
}

fn names(v: &[&'static str]) -> String {
    if v.is_empty() {
        "(none)".to_string()
    } else {
        v.join(", ")
    }
}

/// What one property check returns: its id, the lines that broke it, and — when it broke nothing — why it could not judge.
type Check = (&'static str, Vec<Violation>, Option<String>);

/// Read a trace and check every property.
pub fn check(text: &str, cfg: &Config) -> Report {
    let log = parse(text);
    check_log(&log, cfg)
}

pub fn check_log(log: &[Line], cfg: &Config) -> Report {
    let mut out = Report { lines: log.len(), violations: Vec::new(), checked: Vec::new(), not_observed: Vec::new() };
    for (id, hits, missing) in [
        elem_state(log),
        elem_play_command(log),
        elem_pause_justified(log),
        elem_play_reaches_player(log, cfg),
        latch(log),
        ring(log),
        clock(log, cfg),
        cursor(log),
    ] {
        if hits.is_empty() {
            match missing {
                Some(why) => out.not_observed.push((id, why)),
                None => out.checked.push(id),
            }
        } else {
            out.violations.extend(hits);
        }
    }
    out.violations.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.id.cmp(b.id)));
    out
}

// --------------------------------------------------------------------------------------------- parsing

/// Split the input into lines with their timestamp and, where the line is one of the backend's, its event.
pub fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .enumerate()
        .map(|(i, raw)| Line {
            n: i + 1,
            t: stamp_of(raw),
            raw: raw.to_string(),
            event: message(raw).and_then(event_of).or_else(|| engine_event(raw)),
        })
        .collect()
}

fn stamp_of(raw: &str) -> Option<f64> {
    let at = raw.find("[t+")? + 3;
    raw[at..].split(']').next()?.trim().parse().ok()
}

fn message(raw: &str) -> Option<&str> {
    let at = raw.find(MARK)?;
    Some(raw[at + MARK.len()..].trim())
}

/// What a message is, or `None` for one no property reads: a `log::info!` note, a backtrace frame, anything unknown.
fn event_of(m: &str) -> Option<Event> {
    if let Some(rest) = m.strip_prefix("media elem ") {
        let event = field(rest, "event")?;
        return Some(Event::Elem(Elem {
            event: event.to_string(),
            paused: bool_field(rest, "paused"),
            ended: bool_field(rest, "ended"),
            seeking: bool_field(rest, "seeking"),
            potentially_playing: bool_field(rest, "potentially_playing"),
            player_playing: bool_field(rest, "player_playing"),
            cmd: field(rest, "cmd").map(str::to_string),
        }));
    }
    if let Some(rest) = m.strip_prefix("show ") {
        return Some(Event::Show { now: opt_field(rest, "now"), sound: opt_field(rest, "sound") });
    }
    if let Some(rest) = m.strip_prefix("starving ") {
        return Some(Event::Starving { eof: flag(rest, "eof"), now: opt_field(rest, "now") });
    }
    if let Some(rest) = m.strip_prefix("audio cmd ") {
        let mut words = rest.split_whitespace();
        return match words.next()? {
            "Stop" => Some(Event::Cmd(Cmd::Stop)),
            "Start" => Some(Event::Cmd(Cmd::Start { paused: bool_field(rest, "paused").unwrap_or(false) })),
            "Paused" => Some(Event::Cmd(Cmd::Paused(word_bool(rest.split_whitespace().nth(1)?)?))),
            _ => None,
        };
    }
    if let Some(rest) = m.strip_prefix("audio wait ") {
        let kind = rest.split_whitespace().next()?.to_string();
        // ENTER and the `still` heartbeat name the state; a LEAVE says only how long the wait lasted.
        let (fields, named) = match rest.split_once(": ") {
            Some((head, f)) => (f, head.contains("ENTER") || head.contains("still")),
            None => ("", false),
        };
        if !named {
            return Some(Event::Wait {
                kind,
                playing: None,
                queued: None,
                position_us: None,
                cursor_pts: None,
                frames: None,
                buffered_end_us: None,
                ended: false,
            });
        }
        return Some(Event::Wait {
            kind,
            playing: bool_field(fields, "playing"),
            queued: int_field(fields, "queued"),
            position_us: opt_field(fields, "position_us"),
            cursor_pts: opt_field(fields, "cursor_pts"),
            frames: int_field(fields, "frames"),
            buffered_end_us: opt_field(fields, "buffered_end_us"),
            ended: flag(fields, "ended"),
        });
    }
    if let Some(rest) = m.strip_prefix("mixer stream ") {
        return mixer_event(rest);
    }
    if let Some(rest) = m.strip_prefix("player ") {
        return Some(Event::Player(match rest.split_whitespace().next()? {
            "play" => Player::Play,
            "pause" => Player::Pause,
            "resume" => Player::Resume,
            "suspend" => Player::Suspend,
            "seek" => Player::Seek,
            _ => Player::Other,
        }));
    }
    if let Some(rest) = m.strip_prefix("screen hold ") {
        return Some(Event::Hold(rest.trim() == "on"));
    }
    if m.starts_with("clock source:") {
        return Some(Event::ClockSource);
    }
    if m.starts_with("played ") {
        return Some(Event::Summary);
    }
    if let Some(rest) = m.strip_prefix("opened:") {
        // `yes` or `no`: either way, a player has just been made and pointed at a source.
        let _ = rest.trim();
        return Some(Event::Session(Session::Opened));
    }
    if m.starts_with("first frame after") {
        return Some(Event::Session(Session::FirstFrame));
    }
    if m.starts_with("no pictures:")
        || m.starts_with("no sound:")
        || (m.starts_with("MSE sound") && !m.starts_with("MSE sound stopped"))
    {
        // `MSE sound stopped:` is the sound thread's decoder giving up: the same player, the same clock, nothing to reset.
        return Some(Event::Session(Session::OpenFailure));
    }
    None
}

/// The engine's own boundary lines, which carry no `lucinanet-media:` mark. A navigation makes a new document, and the media
/// element of the old one is not the media element of the new: the engine's `load started` is the earliest such line in a log.
/// The runner stamps `[t+ …]` in front of it, so `phase` is a word somewhere in the line — and `netphase` spells the same
/// letters, which is why the token has to start after a space.
fn engine_event(raw: &str) -> Option<Event> {
    let rest = match raw.strip_prefix("phase ") {
        Some(rest) => rest,
        None => raw.split_once(" phase ")?.1,
    };
    // `phase <mono> <what happened>`: the mono is the engine's own clock reading, not part of the name.
    match rest.split_once(' ')?.1.trim() {
        "load started" => Some(Event::Session(Session::LoadStarted)),
        _ => None,
    }
}

fn mixer_event(rest: &str) -> Option<Event> {
    // `0: taken=…` is the heartbeat; `0 playing true -> false …` and `0 set_playing(true) …` name the stream first.
    let first = rest.split_whitespace().next()?;
    if first.ends_with(':') {
        let f = rest.split_once(':')?.1;
        return Some(Event::Mixer(Mixer::Heartbeat {
            held: opt_field(f, "held"),
            playing: bool_field(f, "playing").unwrap_or(false),
            queued: int_field(f, "queued").unwrap_or(0),
            ring: Ring { pushed: count_field(f, "in"), out: count_field(f, "out"), queued: count_field(f, "queued") },
        }));
    }
    if rest.contains("set_playing(true)") {
        return Some(Event::Mixer(Mixer::Resume));
    }
    let (_, after) = rest.split_once(" -> ")?;
    Some(Event::Mixer(Mixer::Transition { to: word_bool(after)?, held: opt_field(after, "held") }))
}

/// `true`, `false`, `1`, `0`: the element lines spell their flags one way, the mixer and the sound thread the other.
fn word_bool(s: &str) -> Option<bool> {
    match s.trim().split_whitespace().next() {
        Some("true") | Some("1") => Some(true),
        Some("false") | Some("0") => Some(false),
        _ => None,
    }
}

/// The value of a standalone `key=` token, up to the next space.
fn field<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let bytes = s.as_bytes();
    let mut from = 0;
    loop {
        let at = s[from..].find(key)? + from;
        let after = at + key.len();
        let whole_word = at == 0 || bytes[at - 1].is_ascii_whitespace();
        if whole_word
            && bytes.get(after) == Some(&b'=')
            && bytes.get(after + 1).is_some_and(|c| !c.is_ascii_whitespace())
        {
            return Some(s[after + 1..].split_whitespace().next()?);
        }
        from = after;
    }
}

/// `Some(123)`; `None` answers `None`, which is what the absence of a value looks like to the properties.
fn opt_field(s: &str, key: &str) -> Opt {
    match field(s, key)? {
        "None" => None,
        v => v.strip_prefix("Some(")?.strip_suffix(')')?.parse().ok(),
    }
}

fn int_field(s: &str, key: &str) -> Option<i64> {
    field(s, key)?.parse().ok()
}

/// A count, spelled as `queued=96000` is today or as the `Some(…)` of a microseconds field is.
fn count_field(s: &str, key: &str) -> Option<i64> {
    int_field(s, key).or_else(|| opt_field(s, key))
}

fn bool_field(s: &str, key: &str) -> Option<bool> {
    word_bool(field(s, key)?)
}

fn flag(s: &str, key: &str) -> bool {
    bool_field(s, key).unwrap_or(false)
}

// --------------------------------------------------------------------------------------------- properties

/// element-contract-1: what the element believes about the player has to be what the player next reports. It answers with the next
/// element line's `player_playing`, or with the effect of the next `player …` command on the player's mirror — and only up to
/// the point where a new player or document begins, because past that line the reports are not about the player the element
/// was talking to at all.
fn elem_state(log: &[Line]) -> Check {
    const ID: &str = "element-contract-1";
    if !log.iter().any(|l| matches!(l.event, Some(Event::Elem(_)))) {
        return (ID, Vec::new(), Some(no_elem()));
    }
    let hits = log
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let Some(Event::Elem(e)) = &l.event else { return None };
            let want = e.potentially_playing?;
            let (n, t, got) = next_player_report(log, i)?;
            if got == want {
                return None;
            }
            let want_bit = u8::from(want);
            let ans = if got { "playing" } else { "not playing" };
            Some(Violation {
                id: ID,
                line: l.n,
                t: l.t,
                reason: format!(
                    "the element says potentially_playing={want_bit} where the player is {ans}, seen at {} (line {n})",
                    stamp(t)
                ),
            })
        })
        .collect();
    (ID, hits, None)
}

/// The next line that says what the player itself is doing, after index `i` — or nothing, once a line begins a new player or a
/// new document. The belief at `i` is about the player that existed then, so the reports past that boundary answer a different
/// element and cannot say the old one was wrong: consent-navigation was a consent navigation mid-run, where the last
/// `potentially_playing=1` of the old document was answered by the new one's first `player_playing=0`. It is the same boundary
/// `clock` forgets the old player's time at, for the same reason.
fn next_player_report(log: &[Line], i: usize) -> Option<(usize, Option<f64>, bool)> {
    log.iter()
        .skip(i + 1)
        .take_while(|l| !matches!(l.event, Some(Event::Session(_))))
        .find_map(|l| {
            let playing = match &l.event {
                Some(Event::Elem(e)) => e.player_playing?,
                Some(Event::Player(Player::Play | Player::Resume)) => true,
                Some(Event::Player(Player::Pause | Player::Suspend)) => false,
                _ => return None,
            };
            Some((l.n, l.t, playing))
        })
}

/// element-contract-2: a `play()` the element sends while the player is already playing is a command the player does not need, and
/// it is the path that lets `update_media_state` pause a player it just found playing. The element that finds the player
/// playing has to stay quiet: `cmd=none`.
fn elem_play_command(log: &[Line]) -> Check {
    const ID: &str = "element-contract-2";
    if !log.iter().any(|l| matches!(l.event, Some(Event::Elem(_)))) {
        return (ID, Vec::new(), Some(no_elem()));
    }
    let hits = log
        .iter()
        .filter_map(|l| {
            let Some(Event::Elem(e)) = &l.event else { return None };
            if e.event != "play" || e.player_playing != Some(true) {
                return None;
            }
            let cmd = e.cmd.as_deref().filter(|c| *c != "none")?;
            Some(Violation {
                id: ID,
                line: l.n,
                t: l.t,
                reason: format!("event=play found the player playing and sent cmd={cmd} anyway"),
            })
        })
        .collect();
    (ID, hits, None)
}

/// element-contract-3: `cmd=pause` belongs to an element that is paused, ended or seeking. Anywhere else the pause came from
/// something other than the element's own state — and the element, seeing nothing of it, will not call `play()` to undo it.
fn elem_pause_justified(log: &[Line]) -> Check {
    const ID: &str = "element-contract-3";
    if !log.iter().any(|l| matches!(l.event, Some(Event::Elem(_)))) {
        return (ID, Vec::new(), Some(no_elem()));
    }
    let hits = log
        .iter()
        .filter_map(|l| {
            let Some(Event::Elem(e)) = &l.event else { return None };
            if e.cmd.as_deref() != Some("pause")
                || e.paused == Some(true)
                || e.ended == Some(true)
                || e.seeking == Some(true)
            {
                return None;
            }
            Some(Violation {
                id: ID,
                line: l.n,
                t: l.t,
                reason: format!(
                    "cmd=pause with paused={} ended={} seeking={}: the element asked for none of those",
                    bit(e.paused),
                    bit(e.ended),
                    bit(e.seeking)
                ),
            })
        })
        .collect();
    (ID, hits, None)
}

fn bit(v: Option<bool>) -> String {
    match v {
        Some(true) => "1".into(),
        Some(false) => "0".into(),
        None => "?".into(),
    }
}

fn no_elem() -> String {
    "the log has no `media elem` line: the element's own view of its state is not traced yet".into()
}

/// element-contract-4: `event=play` with data to play has to be followed, within the window, by a line that says the player plays.
/// Silence there is the YouTube freeze: the page called `play()`, the element had 30 s buffered, and nothing reached the mixer.
fn elem_play_reaches_player(log: &[Line], cfg: &Config) -> Check {
    const ID: &str = "element-contract-4";
    if !log.iter().any(|l| matches!(l.event, Some(Event::Elem(_)))) {
        return (ID, Vec::new(), Some(no_elem()));
    }
    let mut hits = Vec::new();
    for (i, l) in log.iter().enumerate() {
        let Some(Event::Elem(e)) = &l.event else { continue };
        if e.event != "play" || e.potentially_playing != Some(true) {
            continue;
        }
        let Some(from) = l.t else { continue };
        let witness = log.iter().enumerate().skip(i + 1).find(|(_, f)| player_playing(f));
        match witness {
            // A witness with no timestamp says the player plays, just not when; the wait is then unmeasurable, not broken.
            Some((_, f)) if f.t.is_none() => {},
            Some((_, f)) => {
                let took = f.t.unwrap_or(from) - from;
                if took > cfg.play_window {
                    hits.push(Violation {
                        id: ID,
                        line: l.n,
                        t: Some(from),
                        reason: format!(
                            "event=play with data buffered at t+{from:.3}; the player was only seen playing at {}, {:.3} s later",
                            stamp(f.t),
                            took
                        ),
                    });
                }
            },
            None => hits.push(Violation {
                id: ID,
                line: l.n,
                t: Some(from),
                reason: format!(
                    "event=play with data buffered at t+{from:.3}; no later line of the trace shows the player playing ({} lines on)",
                    log.len() - i - 1
                ),
            }),
        }
    }
    (ID, hits, None)
}

/// Whether this line says the player is playing. A command *asked for* is not this: see `requests_playing`.
fn player_playing(l: &Line) -> bool {
    match &l.event {
        Some(Event::Elem(e)) => e.player_playing == Some(true),
        Some(Event::Player(Player::Play | Player::Resume)) => true,
        Some(Event::Cmd(Cmd::Paused(false))) | Some(Event::Mixer(Mixer::Resume)) => true,
        Some(Event::Mixer(Mixer::Transition { to: true, .. } | Mixer::Heartbeat { playing: true, .. })) => true,
        _ => false,
    }
}

/// MEDIA-STR: the mixer's latch. `held` is the frame the position stands at while a stream is paused, and while it is set
/// `position_us` answers that frame for ever — so a resume has to take it down (mixer.rs `set_playing`) and a stream that
/// plays with it set is case F4.
fn latch(log: &[Line]) -> Check {
    const ID: &str = "MEDIA-STR";
    if !log.iter().any(|l| matches!(l.event, Some(Event::Mixer(_)))) {
        return (ID, Vec::new(), Some("the log has no `mixer stream` line, so the latch is not traced here".into()));
    }
    let mut hits: Vec<Violation> = log
        .iter()
        .filter_map(|l| {
            let Some(Event::Mixer(m)) = &l.event else { return None };
            let (playing, held, what) = match m {
                Mixer::Transition { to, held } => (*to, *held, "transition"),
                Mixer::Heartbeat { playing, held, .. } => (*playing, *held, "heartbeat"),
                Mixer::Resume => return None,
            };
            if playing && held.is_some() {
                return Some(Violation {
                    id: ID,
                    line: l.n,
                    t: l.t,
                    reason: format!(
                        "the {what} says the stream plays with the latch still set: held={}",
                        held.unwrap_or(0)
                    ),
                });
            }
            None
        })
        .collect();
    // The same failure from the other end: playing was asked for and the mixer never once reported the latch down. A pause
    // the page asked for explains the silence; nothing else in the trace does.
    hits.extend(log.iter().enumerate().filter_map(|(i, l)| {
        if !requests_playing(l) {
            return None;
        }
        let mut cleared = false;
        // The page took the request back before the mixer answered it, so there is no latch left to clear: the pause that
        // withdrew it is what the stream is meant to stand at.
        let mut withdrawn = false;
        for f in log.iter().skip(i + 1) {
            match &f.event {
                Some(Event::Mixer(Mixer::Resume)) | Some(Event::Mixer(Mixer::Transition { to: true, .. })) => {
                    cleared = true;
                    break;
                }
                Some(Event::Mixer(Mixer::Heartbeat { playing: true, held: None, .. })) => {
                    cleared = true;
                    break;
                }
                Some(Event::Player(Player::Pause | Player::Suspend)) => {
                    withdrawn = true;
                    break;
                }
                Some(Event::Elem(e)) if e.cmd.as_deref() == Some("pause") || e.paused == Some(true) => {
                    withdrawn = true;
                    break;
                }
                _ => {}
            }
        }
        if cleared || withdrawn || !log.iter().skip(i + 1).any(|f| matches!(f.event, Some(Event::Mixer(_)))) {
            return None;
        }
        Some(Violation {
            id: ID,
            line: l.n,
            t: l.t,
            reason: format!("playing requested at {} and no later mixer line reports the latch cleared", stamp(l.t)),
        })
    }));
    hits.sort_by_key(|v| v.line);
    (ID, hits, None)
}

/// A line that asks for the stream to play. A mixer transition to `true` is not one: it is the mixer's own report, and the
/// latch beside it is what rule (a) reads — asking it to be answered again by a later line would call every short pause a
/// violation, because the next heartbeat is two seconds away.
fn requests_playing(l: &Line) -> bool {
    match &l.event {
        Some(Event::Cmd(Cmd::Paused(false))) => true,
        Some(Event::Player(Player::Play | Player::Resume)) => true,
        Some(Event::Elem(e)) => e.cmd.as_deref() == Some("play"),
        _ => false,
    }
}

/// MEDIA-STR-RING: what went into a stream's ring equals what left it plus what still waits. The heartbeat counts in two units
/// — `taken`/`heard` in frames, `queued` in samples — and says nothing about what was pushed, so until a line carries `in=`
/// and `out=` beside `queued=` the identity cannot be judged from the trace at all.
fn ring(log: &[Line]) -> Check {
    const ID: &str = "MEDIA-STR-RING";
    let beats: Vec<&Line> =
        log.iter().filter(|l| matches!(l.event, Some(Event::Mixer(Mixer::Heartbeat { .. })))).collect();
    if beats.is_empty() {
        return (ID, Vec::new(), Some("the log has no 2 s mixer heartbeat, so the ring counts are not traced".into()));
    }
    if !beats.iter().any(|l| ring_fields(l).is_some()) {
        return (
            ID,
            Vec::new(),
            Some(
                "the heartbeat carries taken=/heard= (frames) and queued= (samples) but no in= and out=, so in = out + queued \
                 cannot be checked across the two units"
                    .into(),
            ),
        );
    }
    let hits = beats
        .into_iter()
        .filter_map(|l| {
            let (pushed, out, queued) = ring_fields(l)?;
            (pushed != out + queued).then(|| Violation {
                id: ID,
                line: l.n,
                t: l.t,
                reason: format!(
                    "the ring loses counts: in={pushed} but out={out} plus queued={queued} is {}",
                    out + queued
                ),
            })
        })
        .collect();
    (ID, hits, None)
}

fn ring_fields(l: &Line) -> Option<(i64, i64, i64)> {
    let Some(Event::Mixer(Mixer::Heartbeat { ring, .. })) = &l.event else { return None };
    let (pushed, out, queued) = (ring.pushed?, ring.out?, ring.queued?);
    Some((pushed, out, queued))
}

/// clock-contract: the clock the page reads. It does not run backwards outside a seek, and it does not stand still beyond the
/// window while the player plays with audio buffered.
fn clock(log: &[Line], cfg: &Config) -> Check {
    const ID: &str = "clock-contract";
    if !log.iter().any(|l| matches!(l.event, Some(Event::Show { .. }))) {
        return (
            ID,
            Vec::new(),
            Some("the log has no `show pts=… now=…` line, so the clock is not traced here".into()),
        );
    }
    let mut hits = Vec::new();
    let mut last: Option<(usize, Option<f64>, i64)> = None;
    // A seek re-bases the clock, so the first `now` after one says nothing about direction. The sound's open prints a Start
    // too, which re-bases it the same way; a stream that stops with nothing buffered is starvation, not a broken clock.
    let mut re_based = true;
    for l in log {
        match &l.event {
            Some(Event::Player(Player::Seek))
            | Some(Event::Cmd(Cmd::Stop | Cmd::Start { .. }))
            | Some(Event::ClockSource) => {
                re_based = true;
            }
            // A new player shows a clock that starts over, so the old one's last value is forgotten instead of compared. The
            // acceptance trace of a YouTube navigation is the case: 7348250 µs at the old element, then `opened: no`, then
            // `show pts=0 now=Some(0)` from the new one — two clocks, not a step backwards.
            Some(Event::Session(_)) => {
                last = None;
                re_based = true;
            }
            Some(Event::Show { now: Some(v), .. }) => {
                if let Some((pn, pt, pv)) = last {
                    if *v < pv && !re_based {
                        hits.push(Violation {
                            id: ID,
                            line: l.n,
                            t: l.t,
                            reason: format!(
                                "the clock ran backwards from {pv} µs at line {pn} {} to {v} µs with no seek between",
                                stamp(pt)
                            ),
                        });
                    }
                }
                last = Some((l.n, l.t, *v));
                re_based = false;
            }
            _ => {}
        }
    }
    if let Some(v) = clock_stall(log, cfg) {
        hits.push(v);
    }
    hits.sort_by_key(|v| v.line);
    (ID, hits, None)
}

/// The still-standing clock. `playing` follows the page and the player, never the sound thread: `audio cmd Paused true` is
/// also what a freshly opened sound mirrors, so it cannot on its own excuse a clock that stopped.
fn clock_stall(log: &[Line], cfg: &Config) -> Option<Violation> {
    let mut playing = false;
    // Where the window stands: the time of the last advance, and the clock value it reached.
    let mut since: Option<(f64, Opt)> = None;
    let mut last_now: Opt = None;
    // What the producer last said about the buffer it is reading.
    let mut buffer: Option<(i64, Opt)> = None;
    for l in log {
        // A navigation begins a new clock at 0: the standing of the old one, and the buffer it had, are not this player's
        // facts. The reset happens before the stamp test because a log without `[t+ …]` prefixes still has to end the session.
        if matches!(l.event, Some(Event::Session(_))) {
            playing = false;
            since = None;
            last_now = None;
            buffer = None;
            continue;
        }
        let Some(t) = l.t else { continue };
        match &l.event {
            Some(Event::Show { now, .. }) => {
                if let Some(v) = now {
                    // The window restarts on an advance only: a clock that keeps answering the same frame has not moved.
                    if last_now.is_none_or(|p| *v > p) {
                        since = Some((t, Some(*v)));
                    }
                    last_now = Some(*v);
                }
            }
            Some(Event::Player(Player::Play | Player::Resume)) | Some(Event::Mixer(Mixer::Resume)) => {
                playing = true;
                since = Some((t, last_now));
            }
            Some(Event::Cmd(Cmd::Paused(false))) => {
                playing = true;
                since = Some((t, last_now));
            }
            Some(Event::Mixer(Mixer::Transition { to: true, .. } | Mixer::Heartbeat { playing: true, .. })) => {
                playing = true;
                if since.is_none() {
                    since = Some((t, last_now));
                }
            }
            Some(Event::Elem(e)) => {
                if e.player_playing == Some(true) || e.cmd.as_deref() == Some("play") {
                    playing = true;
                    since = Some((t, last_now));
                }
                if e.cmd.as_deref() == Some("pause") || e.paused == Some(true) {
                    playing = false;
                }
            }
            Some(Event::Player(Player::Pause | Player::Suspend)) => playing = false,
            // A seek holds the clock by design; it gets the window again from here.
            Some(Event::Player(Player::Seek)) | Some(Event::Cmd(Cmd::Stop | Cmd::Start { .. })) => {
                since = Some((t, last_now))
            }
            Some(Event::Summary) => playing = false,
            Some(Event::Wait { queued, buffered_end_us, .. }) => buffer = Some((queued.unwrap_or(0), *buffered_end_us)),
            _ => {}
        }
        if !playing {
            continue;
        }
        // Nothing to measure against yet: no window, or a trace that never said what the buffer holds, or a clock that has
        // not reported a value. Saying nothing is not a pass, and the property's other half still runs.
        let Some((from, at)) = since else { continue };
        if t - from < cfg.clock_window {
            continue;
        }
        let Some((queued, end)) = buffer else { continue };
        let Some(clock_at) = at.or(last_now) else { continue };
        let ahead = end.is_some_and(|e| e > clock_at);
        if !ahead && queued <= 0 {
            continue;
        }
        let mut why = Vec::new();
        if ahead {
            why.push(format!("audio buffered to {:.3} s", end.unwrap_or(0) as f64 / 1e6));
        }
        if queued > 0 {
            why.push(format!("{queued} samples still queued"));
        }
        return Some(Violation {
            id: "clock-contract",
            line: l.n,
            t: Some(t),
            reason: format!(
                "the clock stood at {clock_at} µs from t+{from:.3} to t+{t:.3} ({:.3} s against a {:.1} s window) with the \
                 player playing and {} ahead of it",
                t - from,
                cfg.clock_window,
                why.join(" and "),
            ),
        });
    }
    None
}

/// MEDIA-CUR: the producer waits for the frame after its cursor while its own line says the track holds frames beyond it.
/// That is frozen-play face P1: a reader parked on ids that name nothing.
fn cursor(log: &[Line]) -> Check {
    const ID: &str = "MEDIA-CUR";
    let waits: Vec<&Line> =
        log.iter().filter(|l| matches!(&l.event, Some(Event::Wait { kind, .. }) if kind == "frame")).collect();
    if waits.is_empty() {
        return (
            ID,
            Vec::new(),
            Some("no `audio wait frame` line in this log: the producer never parked on the cursor, so this is not traced".into()),
        );
    }
    let hits = waits
        .into_iter()
        .filter_map(|l| {
            let Some(Event::Wait { cursor_pts, frames, buffered_end_us, .. }) = &l.event else { return None };
            let frames = frames.unwrap_or(0);
            if frames <= 0 {
                return None;
            }
            let beyond = match (cursor_pts, buffered_end_us) {
                (Some(c), Some(e)) if e > c => Some(format!("the cursor sits at {c} µs, the buffer ends at {e} µs")),
                (None, Some(e)) => Some(format!("the cursor names no frame while the buffer ends at {e} µs")),
                _ => None,
            }?;
            Some(Violation {
                id: ID,
                line: l.n,
                t: l.t,
                reason: format!("waits for a frame after the cursor while {frames} are in the track: {beyond}"),
            })
        })
        .collect();
    (ID, hits, None)
}

#[cfg(test)]
mod unit {
    use super::*;

    fn event(line: &str) -> Option<Event> {
        message(line).and_then(event_of).or_else(|| engine_event(line))
    }

    #[test]
    fn a_real_heartbeat_reads_apart() {
        let line = "[t+   9.757] lucinanet-media: mixer stream 0: taken=173734 heard=173734 held=Some(173734) queued=96000 playing=false | device written=178560 played=178560";
        let l = &parse(line)[0];
        assert_eq!(l.t, Some(9.757));
        assert!(matches!(
            l.event,
            Some(Event::Mixer(Mixer::Heartbeat { held: Some(173734), playing: false, queued: 96000, .. }))
        ));
    }

    #[test]
    fn a_real_transition_reads_apart_with_and_without_the_queue() {
        let yt = "[t+   9.115] lucinanet-media: mixer stream 0 playing true -> false held=Some(173734) from crates/lucinanet-media/src/mse_play.rs:395";
        assert!(matches!(event(yt), Some(Event::Mixer(Mixer::Transition { to: false, held: Some(173734) }))));
        let fix = "[t+   2.340] lucinanet-media: mixer stream 0 playing true -> false held=Some(96259) queued=96000 from crates/lucinanet-media/src/mse_play.rs:418";
        assert!(matches!(event(fix), Some(Event::Mixer(Mixer::Transition { to: false, held: Some(96259) }))));
        let resume = "[t+   5.493] lucinanet-media: mixer stream 0 playing false -> true held=None from crates/lucinanet-media/src/mse_play.rs:395";
        assert!(matches!(event(resume), Some(Event::Mixer(Mixer::Transition { to: true, held: None }))));
        let set = "[t+   0.329] lucinanet-media: mixer stream 0 set_playing(true) resumed, latch 0 released from crates/lucinanet-media/src/mse_play.rs:418";
        assert_eq!(event(set), Some(Event::Mixer(Mixer::Resume)));
    }

    #[test]
    fn a_real_wait_reads_apart_and_a_leave_says_nothing_about_state() {
        let enter = "[t+   9.107] lucinanet-media: audio wait room ENTER: queued=96000 playing=true position_us=Some(3614395) cursor_pts=Some(4721000) start_at=None frames=1500 buffered_end_us=Some(30001000) generation=1 track=1 buffers=2 ended=false seeks=0";
        assert!(matches!(
            event(enter),
            Some(Event::Wait { queued: Some(96000), buffered_end_us: Some(30001000), frames: Some(1500), .. })
        ));
        let start = "[t+   3.747] lucinanet-media: audio wait start ENTER: queued=0 playing=false cursor_pts=None start_at=None frames=0 buffered_end_us=None generation=1 track=1 buffers=2 ended=false seeks=0";
        assert!(
            matches!(event(start), Some(Event::Wait { kind, cursor_pts: None, frames: Some(0), .. }) if kind == "start")
        );
        let leave = "[t+   9.107] lucinanet-media: audio wait room LEAVE: progress, 10 ms";
        assert!(matches!(event(leave), Some(Event::Wait { queued: None, frames: None, .. })));
    }

    #[test]
    fn the_sound_commands_read_apart() {
        assert_eq!(event("lucinanet-media: audio cmd Paused true"), Some(Event::Cmd(Cmd::Paused(true))));
        assert_eq!(event("lucinanet-media: audio cmd Paused false"), Some(Event::Cmd(Cmd::Paused(false))));
        assert_eq!(event("lucinanet-media: audio cmd Stop"), Some(Event::Cmd(Cmd::Stop)));
        assert_eq!(
            event("lucinanet-media: audio cmd Start pts=0 paused=true running=false"),
            Some(Event::Cmd(Cmd::Start { paused: true }))
        );
    }

    #[test]
    fn an_element_line_reads_every_field() {
        let line = "[t+   9.115] lucinanet-media: media elem event=play paused=0 ended=0 seeking=0 potentially_playing=1 player_playing=0 cmd=none";
        let Some(Event::Elem(e)) = event(line) else { panic!("not read as an element line") };
        assert_eq!(e.event, "play");
        assert_eq!((e.paused, e.ended, e.seeking), (Some(false), Some(false), Some(false)));
        assert_eq!((e.potentially_playing, e.player_playing), (Some(true), Some(false)));
        assert_eq!(e.cmd.as_deref(), Some("none"));
    }

    #[test]
    fn the_clock_and_the_player_commands_read_apart() {
        assert!(matches!(
            event("[t+   9.084] lucinanet-media: show pts=3587000 now=Some(3591312) sound=Some(3591312)"),
            Some(Event::Show { now: Some(3591312), sound: Some(3591312) })
        ));
        assert!(matches!(
            event("[t+   3.814] lucinanet-media: show pts=0 now=Some(0) sound=None"),
            Some(Event::Show { now: Some(0), sound: None })
        ));
        assert_eq!(
            event("[t+   0.329] lucinanet-media: player play on Script#1 mirror(paused=true,can_resume=false)"),
            Some(Event::Player(Player::Play))
        );
        assert_eq!(event("[t+  13.303] lucinanet-media: played VP9 seconds=9.7 shown=87"), Some(Event::Summary));
        assert_eq!(
            event("[t+   0.395] lucinanet-media: clock source: system -> audio at 0.064 s"),
            Some(Event::ClockSource)
        );
    }

    #[test]
    fn prose_and_backtrace_are_not_events() {
        assert_eq!(event("lucinanet-media:   12: script::dom::html::embedded_content::htmlmediaelement"), None);
        assert_eq!(parse("[t+   4.112] render: complete true in 4088 ms")[0].event, None);
        assert_eq!(parse("phase 584066.098360 load complete")[0].event, None);
    }

    /// The lines that begin a player or a document, and nothing else.
    #[test]
    fn the_lines_that_begin_a_player_or_a_document_are_sessions() {
        for (line, want) in [
            ("lucinanet-media: opened: no", Session::Opened),
            ("lucinanet-media: opened: yes", Session::Opened),
            (
                "[2026-11-18T17:10:10Z INFO  lucinanet_media::player] lucinanet-media: first frame after 116 ms",
                Session::FirstFrame,
            ),
            ("lucinanet-media: no pictures: cannot read: the player closed", Session::OpenFailure),
            ("lucinanet-media: no sound: not a format this plays: unsupported feature", Session::OpenFailure),
            ("lucinanet-media: MSE sound: the opus decoder is not here", Session::OpenFailure),
            ("phase 584066.098360 load started", Session::LoadStarted),
            // The runner stamps the engine's line like any other, and the acceptance logs do not stamp it at all.
            ("[t+  13.242] phase 569779.636813 load started", Session::LoadStarted),
        ] {
            assert_eq!(event(line), Some(Event::Session(want)), "{line}");
        }
        // A navigation's other lines are not boundaries: only the start of a load is. And a sound thread giving up is the
        // same player, while `netphase` spells the engine's word inside a fetch line.
        for line in [
            "phase 584066.483988 load complete",
            "phase 584066.126646 first frame painted",
            "lucinanet-media: MSE sound stopped: decode error",
            "[t+  11.668] netphase 569778.066908 request https://www.youtube.com/api/stats 0.0 ms",
        ] {
            assert_eq!(event(line), None, "{line}");
        }
    }

    /// A page navigation leaves the old element's clock standing and starts the new one at 0. That is two clocks, not a
    /// step backwards — and without the boundary line the same pair still has to be called a backwards clock.
    #[test]
    fn a_new_player_resets_the_clock_and_a_missing_marker_still_reports_the_step_back() {
        let old_then_new = "lucinanet-media: show pts=7341000 now=Some(7348250) sound=Some(7348250)\n\
                            lucinanet-media: opened: no\n\
                            lucinanet-media: show pts=0 now=Some(0) sound=None\n";
        assert!(
            check(old_then_new, &Config::default()).violations.is_empty(),
            "the open of a new player is a new clock"
        );

        let same_two_values_without_a_boundary =
            "lucinanet-media: show pts=7341000 now=Some(7348250) sound=Some(7348250)\n\
                                                  lucinanet-media: show pts=0 now=Some(0) sound=None\n";
        let r = check(same_two_values_without_a_boundary, &Config::default());
        let v = &r.violations[0];
        assert_eq!(
            (v.id, v.line),
            ("clock-contract", 2),
            "two values and no boundary is a clock that went backwards: {:?}",
            r.violations
        );
        assert!(v.reason.contains("no seek between"), "{}", v.reason);
    }

    /// What the element believed is about the player it was talking to: past a line that begins a new player or a new document
    /// there is nothing to answer it — and the same two lines without that boundary is still the report consent-navigation wanted.
    #[test]
    fn the_element_state_resets_at_a_new_player_and_still_bites_without_the_marker() {
        let navigation = "lucinanet-media: media elem event=play-redundant paused=0 ended=0 seeking=0 potentially_playing=1 player_playing=1 cmd=none\n\
                          phase 655617.191074 load started\n\
                          lucinanet-media: media elem event=play paused=0 ended=0 seeking=0 potentially_playing=0 player_playing=0 cmd=play\n";
        assert!(
            check(navigation, &Config::default()).violations.is_empty(),
            "a navigation is a new element, not a wrong belief"
        );

        let same_pair_without_a_boundary =
            "lucinanet-media: media elem event=play-redundant paused=0 ended=0 seeking=0 potentially_playing=1 player_playing=1 cmd=none\n\
                                                  lucinanet-media: media elem event=play paused=0 ended=0 seeking=0 potentially_playing=0 player_playing=0 cmd=play\n";
        let r = check(same_pair_without_a_boundary, &Config::default());
        let v = &r.violations[0];
        assert_eq!((v.id, v.line), ("element-contract-1", 1), "{:?}", r.violations);
        assert!(
            v.reason.contains("line 2"),
            "the report has to name the line that answers it: {}",
            v.reason
        );
    }

    #[test]
    fn the_log_envelope_still_carries_its_message() {
        let line = "[t+   9.115] [2026-11-18T13:11:57Z INFO  lucinanet_media::hold] lucinanet-media: screen hold off";
        assert_eq!(event(line), Some(Event::Hold(false)));
    }

    #[test]
    fn a_field_name_inside_another_word_does_not_count() {
        // `device written=178560` must not read as `in=`.
        let line = "[t+   9.757] lucinanet-media: mixer stream 0: taken=1 heard=1 held=None queued=0 playing=true | device written=178560 played=1";
        let Some(Event::Mixer(Mixer::Heartbeat { ring, .. })) = event(line) else { panic!("not a heartbeat") };
        assert_eq!(ring.pushed, None);
    }
}
