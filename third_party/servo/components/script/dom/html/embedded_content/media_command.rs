/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The element-to-player decision, split out of `HTMLMediaElement::update_media_state`.
//!
//! `update_media_state` is the one place the media element tells its player to run or to stop, and
//! several algorithms reach it: the internal play steps, the internal pause steps, the ready-state
//! steps, the seek algorithm, the end-of-playback steps and the media element load algorithm.
//! Before first-fix they all got the same two branches, and one of those answers was the frozen-play
//! freeze: a `play()` that arrived while the pipeline was running paused it.
//!
//! This module holds the answer as one pure function of (event, element state). It imports nothing
//! from the DOM: the element builds a [`MediaState`] from its own cells and applies the
//! [`Decision`] to its player. The properties the function keeps are written down in
//! `contracts-media-elem.md` beside it, and the tests below hold them over every state and over
//! random event sequences.
//!
//! The rule the contract-rewrite review settled: the player is commanded by what the element *is* — paused,
//! ended, errored, blocked for content — and want of data is not one of those. Two reasons, and the
//! second is the one that bites. The specification's `#blocked-media-element` has no seeking term,
//! so a seek that has nothing to decode and a mid-playback underrun are the same state; and the
//! engine's command mirror is the only thing this element can read, so pausing it for want of data
//! stops the very signal (`PlaybackState::Playing`, which raises the ready state back above
//! `HAVE_CURRENT_DATA`) that would end the block. That is finding 1 of the review: a seek that runs
//! out of data paused the player and never started it again. Want of data therefore commands
//! nothing when the engine is already running, and commands a `play()` when the page wants playback
//! and the engine is stopped — which is the same answer in both directions: the engine runs whenever
//! the page wants it to, and the data question is the engine's to answer.

/// The element's ready state, as the decision needs it. Mirrors the `ReadyState` of
/// `htmlmediaelement.rs`, whose discriminants come from the WebIDL constants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReadyLevel {
    #[default]
    HaveNothing,
    HaveMetadata,
    HaveCurrentData,
    HaveFutureData,
    HaveEnoughData,
}

impl ReadyLevel {
    /// `HAVE_FUTURE_DATA` or more: there is data ahead of the position to play.
    pub fn has_future_data(self) -> bool {
        self >= ReadyLevel::HaveFutureData
    }
}

/// What the element and its player look like at the moment the decision is made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MediaState {
    /// The `paused` IDL attribute. Only the internal pause steps and the end-of-playback steps
    /// write it, so it is the page's or the element's own order to stop.
    pub paused: bool,
    /// `ended_playback(LoopCondition::Included)`, forwards or backwards.
    pub ended: bool,
    /// The `seeking` IDL attribute. It is read for the trace and for the `waiting` event's data
    /// condition, not by the command: the seek algorithm does not touch the playing state, and
    /// `#blocked-media-element` has no seeking term.
    pub seeking: bool,
    /// <https://html.spec.whatwg.org/multipage/#potentially-playing>
    pub potentially_playing: bool,
    pub ready_state: ReadyLevel,
    /// Whether the player reports playing: `!player.paused()`, which is the mirror of the last
    /// `play()`/`pause()` the element sent, not a measurement of frames advancing.
    pub player_playing: bool,
    /// Whether the player's buffered ranges cover the current playback position.
    pub buffered_at_position: bool,
    /// Whether the error attribute is set: playback has stopped due to an error.
    pub error: bool,
}

impl MediaState {
    /// <https://html.spec.whatwg.org/multipage/#blocked-media-element>'s data term and only that: the
    /// ready state. Its two other conditions, paused for user interaction and paused for in-band
    /// content, are [`MediaState::blocked_by_content_or_interaction`].
    ///
    /// The `buffered` ranges are deliberately not here. They belong to the `waiting` event —
    /// "readyState is less than or equal to HAVE_CURRENT_DATA, and paused is false. Either seeking is
    /// true, or the current playback position is not contained in any of the ranges in buffered"
    /// (<https://html.spec.whatwg.org/multipage/#event-media-waiting>) — and to element-contract-4's "data
    /// buffered". The element's own `is_potentially_playing()` does not read them, so a high ready
    /// state with a hole at the position is potentially playing (contract-rewrite review, finding 5).
    pub fn blocked_for_want_of_data(&self) -> bool {
        !self.ready_state.has_future_data()
    }

    /// The `waiting` event's own data condition, which is narrower than the block: the position has
    /// to be the one with nothing to decode, because a seek moved it there or because it fell in a
    /// hole in the buffered ranges.
    pub fn waits_for_data(&self) -> bool {
        self.blocked_for_want_of_data() && (self.seeking || !self.buffered_at_position)
    }

    /// What the page and the specification want, before the data question: the three conditions of
    /// potentially playing that are not the ready state
    /// (<https://html.spec.whatwg.org/multipage/#potentially-playing>). Each of the three is a stop
    /// somebody ordered, or a stop the element cannot leave; want of data is neither.
    pub fn wants_playback(&self) -> bool {
        !self.paused && !self.ended && !self.error
    }

    /// A blocked media element for a reason that is not want of data: paused for user interaction, or
    /// paused for in-band content. The snapshot carries no field for them because the element folds
    /// both into `potentially_playing`, so this is the only reading left when the three stops above
    /// are clear, the ready state has future data, and the element still says it is not potentially
    /// playing. Both predicates are constant `false` in this fork today (the FIXMEs in
    /// `is_blocked_media_element`), so the case does not occur yet; it is here so the answer is not
    /// wrong on the day someone fills them in.
    pub fn blocked_by_content_or_interaction(&self) -> bool {
        self.wants_playback() && !self.blocked_for_want_of_data() && !self.potentially_playing
    }
}

/// The algorithm that brought the element to `update_media_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaEvent {
    /// The internal play steps, on an element whose `paused` attribute was true.
    Play,
    /// The internal play steps, on an element that was already playing: `play()` answering a
    /// running element, which is what YouTube's state sync does and what froze frozen-play.
    RedundantPlay,
    /// The internal pause steps.
    Pause,
    /// The seek algorithm, while `seeking` is set.
    SeekStart,
    /// The seek algorithm, once the new position is established.
    SeekEnd,
    /// The ready-state steps: the element became blocked, or the block cleared.
    Waiting,
    /// End of playback, forwards or backwards.
    Ended,
    /// A stop of the resource, stamped `load` for want of a better name in the fixed vocabulary: step
    /// 6 of the fatal steps of the media data processing list, which is the only thing in the element
    /// that answers with it. The trace format has no `error=` field and no `error` event, and these
    /// are steps of the load algorithm — the fetch is cancelled and the resource selection algorithm
    /// aborts, which is what the name says. The media element load algorithm itself never reaches the
    /// decision: it stops the previous player outright, in `reset_media_player`.
    Load,
}

impl MediaEvent {
    /// The `event=` name of the trace line, fixed by `tools/media-trace-check` in LucinaNet.
    pub fn trace_name(self) -> &'static str {
        match self {
            MediaEvent::Play => "play",
            MediaEvent::RedundantPlay => "play-redundant",
            MediaEvent::Pause => "pause",
            MediaEvent::SeekStart => "seek-start",
            MediaEvent::SeekEnd => "seek-end",
            MediaEvent::Waiting => "waiting",
            MediaEvent::Ended => "ended",
            MediaEvent::Load => "load",
        }
    }

    /// Whether the event is one of the play steps.
    pub fn is_play(self) -> bool {
        matches!(self, MediaEvent::Play | MediaEvent::RedundantPlay)
    }
}

pub const ALL_EVENTS: [MediaEvent; 8] = [
    MediaEvent::Play,
    MediaEvent::RedundantPlay,
    MediaEvent::Pause,
    MediaEvent::SeekStart,
    MediaEvent::SeekEnd,
    MediaEvent::Waiting,
    MediaEvent::Ended,
    MediaEvent::Load,
];

/// What to tell the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerCommand {
    None,
    Play,
    Pause,
}

impl PlayerCommand {
    /// The `cmd=` name of the trace line.
    pub fn trace_name(self) -> &'static str {
        match self {
            PlayerCommand::None => "none",
            PlayerCommand::Play => "play",
            PlayerCommand::Pause => "pause",
        }
    }
}

/// Why the decision is what it is, for the tests and for whoever reads a trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// element-contract-2: the play steps found a running player and left it alone.
    PlayAlreadyRunning,
    /// The element is potentially playing and the player is not: start it.
    PotentiallyPlayingButStopped,
    /// Want of data with the engine stopped and the page wanting playback: start it anyway, because
    /// the running engine is the only thing that can end the block (contract-rewrite review, finding 1).
    BlockedButAskedToPlay,
    /// The page or the element paused: `paused` is true.
    OrderedPause,
    /// <https://html.spec.whatwg.org/multipage/#ended-playback>, in either direction.
    EndedPlayback,
    /// Playback stopped due to an error: the element is not potentially playing.
    StoppedDueToError,
    /// Blocked for user interaction or in-band content, with data to play.
    PausedForContentOrInteraction,
    /// element-contract-3's answer in the want-of-data window: nothing ordered a stop, and the engine is
    /// already commanded to run, so there is nothing to send. A pause commanded there is a stop that
    /// nothing answers — the element stays not-potentially-playing until the data returns, and the
    /// engine that is paused reports nothing — which is the shape of the frozen-play freeze and of finding
    /// 1.
    Starved,
    /// The player is already stopped and nothing asks it to run.
    NothingToStop,
    /// The player already answers what the element is, and the event is not a play: the running
    /// engine under a potentially playing element, with nothing to send either way.
    InAgreement,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::PlayAlreadyRunning => "play-found-player-running",
            Reason::PotentiallyPlayingButStopped => "potentially-playing-and-player-stopped",
            Reason::BlockedButAskedToPlay => "blocked-for-want-of-data-and-playback-was-ordered",
            Reason::OrderedPause => "paused-by-the-element-or-the-page",
            Reason::EndedPlayback => "ended-playback",
            Reason::StoppedDueToError => "playback-stopped-due-to-error",
            Reason::PausedForContentOrInteraction => {
                "paused-for-user-interaction-or-in-band-content"
            },
            Reason::Starved => "blocked-for-want-of-data",
            Reason::NothingToStop => "player-already-stopped",
            Reason::InAgreement => "player-already-answers-the-element",
        }
    }
}

/// The answer: a command and the cause of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub command: PlayerCommand,
    pub reason: Reason,
}

impl Decision {
    fn none(reason: Reason) -> Decision {
        Decision {
            command: PlayerCommand::None,
            reason,
        }
    }
}

/// The cause that orders a stop, if the element's state names one.
///
/// > A media element is said to be potentially playing when its paused attribute is false, the
/// > element has not ended playback, playback has not stopped due to errors, and the element is
/// > not a blocked media element. — <https://html.spec.whatwg.org/multipage/#potentially-playing>
///
/// The first three are stops: the page or the element ordered one, the playback has reached an end,
/// the data has died. Of the fourth, a blocked media element
/// (<https://html.spec.whatwg.org/multipage/#blocked-media-element>), only the two content pauses are
/// a stop of the element's; want of data is not, and `#blocked-media-element` says so by never
/// mentioning `seeking`: the seek algorithm's step 12 is a wait for decoded data, not an instruction
/// to the engine. A running engine is also the only thing that can end a want-of-data block, which
/// is why the pause is not merely unauthorized but fatal (contract-rewrite review, findings 1 and 2).
fn stop_cause(state: &MediaState) -> Option<Reason> {
    if state.paused {
        Some(Reason::OrderedPause)
    } else if state.ended {
        Some(Reason::EndedPlayback)
    } else if state.error {
        Some(Reason::StoppedDueToError)
    } else if state.blocked_by_content_or_interaction() {
        Some(Reason::PausedForContentOrInteraction)
    } else {
        None
    }
}

/// The element-to-player decision. Pure: it reads the snapshot and answers, and touches nothing.
///
/// One rule, then the two that follow from it:
///
/// * The command answers the element's own state, read as stops: `paused`, ended playback, stopped
///   due to errors, a block for content or interaction. Anything else — want of data, a seek, a hole
///   in the buffered ranges — is the engine's condition to report and to clear, not the element's to
///   command. So: `play()` when none of them holds and the player is stopped, `pause()` when one
///   holds and the player is running, nothing when the player already agrees.
/// * Because want of data is not a stop, a blocked element whose engine the page still wants running
///   is *started*, and a blocked element whose engine is running is left running. The mirror then
///   says playing while the element is not potentially playing, which is element-contract-1's one window
///   and the price of never freezing: the engine rendering nothing with nothing to render, and
///   reporting the frame that clears the block, is the fork's only route back to a ready state above
///   `HAVE_CURRENT_DATA` (`playback_state_changed`'s `Playing` arm).
/// * the internal play steps contain no pause at all
///   (<https://html.spec.whatwg.org/multipage/#internal-play-steps>): they clear `paused`, seek back
///   if the element had ended, and otherwise only ask the player to run. So the answer to a play
///   comes from the rule alone — on an element that is not in a stop the player is either already
///   running, and no command goes out, or stopped, and the command is `play`. That is first-fix stated
///   as a rule instead of a branch: element-contract-2.
pub fn decide(event: MediaEvent, state: &MediaState) -> Decision {
    if let Some(reason) = stop_cause(state) {
        return if state.player_playing {
            Decision {
                command: PlayerCommand::Pause,
                reason,
            }
        } else {
            Decision::none(Reason::NothingToStop)
        };
    }

    if state.player_playing {
        // Nothing here has to change. Say which kind of nothing it is, for the trace.
        return Decision::none(if event.is_play() {
            Reason::PlayAlreadyRunning
        } else if state.blocked_for_want_of_data() {
            Reason::Starved
        } else {
            Reason::InAgreement
        });
    }

    Decision {
        command: PlayerCommand::Play,
        reason: if state.potentially_playing {
            Reason::PotentiallyPlayingButStopped
        } else {
            // Blocked for want of data, with the page's wish standing. A `play()` the first round
            // left unanswered, and the engine then reported nothing at all: finding 1.
            Reason::BlockedButAskedToPlay
        },
    }
}

/// The element's one trace line per decision, in the format `tools/media-trace-check` parses.
pub fn trace_line(event: MediaEvent, state: &MediaState, decision: &Decision) -> String {
    let bit = |v: bool| if v { '1' } else { '0' };
    format!(
        "media elem event={} paused={} ended={} seeking={} potentially_playing={} player_playing={} cmd={}",
        event.trace_name(),
        bit(state.paused),
        bit(state.ended),
        bit(state.seeking),
        bit(state.potentially_playing),
        bit(state.player_playing),
        decision.command.trace_name(),
    )
}

/// The line above, on stderr, when `LUCINANET_MEDIA_TRACE` is set. Read once, like the media
/// crates read their own switch, so a decision costs one atomic load afterwards.
pub fn trace(event: MediaEvent, state: &MediaState, decision: &Decision) {
    // LucinaNet: one line per element-to-player decision, for `tools/media-trace-check`.
    static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *LOG.get_or_init(|| std::env::var_os("LUCINANET_MEDIA_TRACE").is_some()) {
        eprintln!("lucinanet-media: {}", trace_line(event, state, decision));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decision as it stood at the parent of the first-fix fix (`5c43b1520`), kept here only so
    /// these tests can show which contracts the old code broke. Nothing reaches it from the
    /// element. The old `update_media_state` read only the ready-state branch and the mirror:
    ///
    /// ```text
    /// if potentially_playing && !is_playing { play() } else if is_playing { pause() }
    /// ```
    fn decide_pre_first_fix(_event: MediaEvent, state: &MediaState) -> Decision {
        if state.potentially_playing && !state.player_playing {
            return Decision {
                command: PlayerCommand::Play,
                reason: Reason::PotentiallyPlayingButStopped,
            };
        }
        if state.player_playing {
            return Decision {
                command: PlayerCommand::Pause,
                reason: Reason::NothingToStop,
            };
        }
        Decision::none(Reason::NothingToStop)
    }

    /// The rule as first-fix left it: the guard added, still with no event and no cause behind the
    /// pause. Used to show where splitting the event out changes the answer.
    fn decide_first_fix(_event: MediaEvent, state: &MediaState) -> Decision {
        if state.potentially_playing && !state.player_playing {
            return Decision {
                command: PlayerCommand::Play,
                reason: Reason::PotentiallyPlayingButStopped,
            };
        }
        if !state.potentially_playing && state.player_playing {
            return Decision {
                command: PlayerCommand::Pause,
                reason: Reason::NothingToStop,
            };
        }
        Decision::none(Reason::NothingToStop)
    }

    /// The same answer, told as a command: the tests below read better with it.
    fn pause(reason: Reason) -> Decision {
        Decision {
            command: PlayerCommand::Pause,
            reason,
        }
    }

    /// The decision as the first round of contract-rewrite left it (`e441b99b2`), before the cross-family
    /// review: want of data outside a seek issued no command, but want of data *inside* one paused
    /// the player for the duration of the seek. Nothing reaches it from the element; it is here
    /// because finding 1 of the review is that this is the pause nothing answers, and the red runs
    /// below have to run against the code this branch actually wrote, not only against upstream's.
    fn decide_pre_contract_rewrite(event: MediaEvent, state: &MediaState) -> Decision {
        if state.potentially_playing {
            return if state.player_playing {
                Decision::none(if event.is_play() {
                    Reason::PlayAlreadyRunning
                } else {
                    Reason::NothingToStop
                })
            } else {
                Decision {
                    command: PlayerCommand::Play,
                    reason: Reason::PotentiallyPlayingButStopped,
                }
            };
        }
        if !state.player_playing {
            return Decision::none(Reason::NothingToStop);
        }
        if state.paused {
            return pause(Reason::OrderedPause);
        }
        if state.ended {
            return pause(Reason::EndedPlayback);
        }
        if state.error {
            return pause(Reason::StoppedDueToError);
        }
        if !state.ready_state.has_future_data() || !state.buffered_at_position {
            return if state.seeking {
                Decision {
                    command: PlayerCommand::Pause,
                    reason: Reason::Starved,
                }
            } else {
                Decision::none(Reason::Starved)
            };
        }
        pause(Reason::PausedForContentOrInteraction)
    }

    /// A model player: what the real player's `paused()` mirror reports is the last command the
    /// element gave it.
    #[derive(Clone, Copy, Debug, Default)]
    struct ModelPlayer {
        playing: bool,
    }

    impl ModelPlayer {
        fn apply(&mut self, command: PlayerCommand) {
            match command {
                PlayerCommand::Play => self.playing = true,
                PlayerCommand::Pause => self.playing = false,
                PlayerCommand::None => {},
            }
        }
    }

    fn state_of(booleans: u8, ready_state: ReadyLevel) -> MediaState {
        let flag = |bit: u32| (booleans >> bit & 1) == 1;
        MediaState {
            paused: flag(0),
            ended: flag(1),
            seeking: flag(2),
            potentially_playing: flag(3),
            player_playing: flag(4),
            buffered_at_position: flag(5),
            error: flag(6),
            ready_state,
        }
    }

    const LEVELS: [ReadyLevel; 5] = [
        ReadyLevel::HaveNothing,
        ReadyLevel::HaveMetadata,
        ReadyLevel::HaveCurrentData,
        ReadyLevel::HaveFutureData,
        ReadyLevel::HaveEnoughData,
    ];

    /// Every state the snapshot can hold: the seven flags crossed with the five ready levels,
    /// 640 of them.
    fn all_states() -> Vec<MediaState> {
        let mut out = Vec::with_capacity(128 * LEVELS.len());
        for flags in 0..=127u8 {
            for level in LEVELS {
                out.push(state_of(flags, level));
            }
        }
        out
    }

    /// The states an element can actually be in: `potentially_playing` is not an independent flag,
    /// it is the name for none of the four conditions holding. The snapshot sees three of them plus
    /// want of data; a block for user interaction or in-band content would hold `potentially_playing`
    /// false over a state that looks clear, and both of those predicates are constant `false` in this
    /// fork (see the FIXMEs in `is_blocked_media_element`), so no reachable state does.
    ///
    /// > A media element is said to be potentially playing when its paused attribute is false, the
    /// > element has not ended playback, playback has not stopped due to errors, and the element is
    /// > not a blocked media element.
    fn element_can_be(state: &MediaState) -> bool {
        state.potentially_playing
            == !(state.paused || state.ended || state.error || state.blocked_for_want_of_data())
    }

    /// The states an element can actually be in, out of the 640.
    fn reachable_states() -> Vec<MediaState> {
        all_states()
            .into_iter()
            .filter(|s| element_can_be(s))
            .collect()
    }

    /// The first state and event `want` names a break for, over every state an element can be in.
    fn first_break<F>(decide_fn: fn(MediaEvent, &MediaState) -> Decision, want: F) -> Option<String>
    where
        F: Fn(MediaEvent, &MediaState, Decision) -> Option<String>,
    {
        for state in reachable_states() {
            for event in ALL_EVENTS {
                if let Some(break_) = want(event, &state, decide_fn(event, &state)) {
                    return Some(break_);
                }
            }
        }
        None
    }

    // --------------------------------------------------------------------- element-contract-1

    /// element-contract-1: the player is playing if and only if the element is potentially playing.
    /// Over every state and event, the command leaves the mirror answering what the element is.
    ///
    /// Over the states an element can actually be in: the snapshot's `potentially_playing` is the
    /// element's own answer, so a flag combination its predicate forbids — `paused` true with
    /// `potentially_playing` true, say — is not a state the property is asked to keep, and
    /// `element_can_be` says why. The count is printed by
    /// `the_split_differs_from_first_fix_only_where_a_contract_says_it_must`.
    #[test]
    fn media_elem_01_holds_for_every_state() {
        for state in reachable_states() {
            for event in ALL_EVENTS {
                let decision = decide(event, &state);
                let mut player = ModelPlayer {
                    playing: state.player_playing,
                };
                player.apply(decision.command);
                if state.wants_playback()
                    && state.blocked_for_want_of_data()
                    && !state.potentially_playing
                {
                    // The one window 01 cannot close from here, and the review's finding 2 says the
                    // specification never asked it to: the element is blocked for want of data, so it
                    // is not potentially playing, while the mirror says the last command was `play()`
                    // — or says nothing, and the element starts the engine anyway because a stopped
                    // engine reports nothing and the block would never end (finding 1). Pausing into
                    // agreement is the freeze; this is the window element-contract-5 owns, seek or not.
                    continue;
                }
                assert_eq!(
                    player.playing, state.potentially_playing,
                    "{event:?} {state:?} -> {decision:?}"
                );
            }
        }
    }

    /// A potentially playing element with a stopped player is always started, whatever brought the
    /// element to the decision.
    #[test]
    fn media_elem_01_starts_a_stopped_player_under_a_potentially_playing_element() {
        let stopped = MediaState {
            potentially_playing: true,
            ready_state: ReadyLevel::HaveEnoughData,
            buffered_at_position: true,
            ..MediaState::default()
        };
        for event in ALL_EVENTS {
            assert_eq!(
                decide(event, &stopped),
                Decision {
                    command: PlayerCommand::Play,
                    reason: Reason::PotentiallyPlayingButStopped,
                },
                "{event:?}"
            );
        }
    }

    // --------------------------------------------------------------------- element-contract-2

    /// element-contract-2: `play()` on a playing element issues no command.
    #[test]
    fn media_elem_02_a_play_on_a_running_element_issues_no_command() {
        let running = MediaState {
            potentially_playing: true,
            player_playing: true,
            ready_state: ReadyLevel::HaveEnoughData,
            buffered_at_position: true,
            ..MediaState::default()
        };
        for event in [MediaEvent::Play, MediaEvent::RedundantPlay] {
            assert_eq!(
                decide(event, &running),
                Decision {
                    command: PlayerCommand::None,
                    reason: Reason::PlayAlreadyRunning,
                },
                "{event:?}"
            );
        }
    }

    /// The strong form the fix needs, over every state an element can be in: a play never pauses an
    /// element that is potentially playing. This is the branch that froze frozen-play — `play()` ->
    /// `internal_play_steps` -> `update_media_state` -> `player.pause()`.
    #[test]
    fn media_elem_02_no_play_pauses_a_potentially_playing_element() {
        for state in reachable_states() {
            if !state.potentially_playing {
                continue;
            }
            for event in [MediaEvent::Play, MediaEvent::RedundantPlay] {
                assert_ne!(
                    decide(event, &state).command,
                    PlayerCommand::Pause,
                    "{event:?} over {state:?}"
                );
            }
        }
    }

    /// How far 02 reaches: the play *steps* hold no pause, so a play only stops the player when the
    /// element's own state had already ordered the stop and the play could not undo it. The spec's
    /// step 2 seeks an ended element back to the start; where the media cannot be seeked the element
    /// stays ended, and an ended element must not play audio (#potentially-playing,
    /// #playing-the-media-resource) — so that play answers with the pause the ended state asked for,
    /// not with a pause the play steps ordered.
    #[test]
    fn media_elem_02_a_play_on_an_ended_element_stops_the_player_the_state_had_stopped() {
        let ended_and_unseekable = MediaState {
            ended: true,
            potentially_playing: false,
            player_playing: true,
            ready_state: ReadyLevel::HaveEnoughData,
            buffered_at_position: true,
            ..MediaState::default()
        };
        for event in [MediaEvent::Play, MediaEvent::RedundantPlay] {
            assert_eq!(
                decide(event, &ended_and_unseekable),
                Decision {
                    command: PlayerCommand::Pause,
                    reason: Reason::EndedPlayback,
                },
                "{event:?}"
            );
        }
    }

    /// The same search over the pre-fix decision finds the running element and stops it.
    #[test]
    fn media_elem_02_fails_for_the_pre_fix_decision() {
        let break_ = first_break(decide_pre_first_fix, |event, state, decision| {
            (event.is_play()
                && state.potentially_playing
                && decision.command == PlayerCommand::Pause)
                .then(|| format!("element-contract-2 broken: {event:?} over {state:?} -> {decision:?}"))
        });
        let Some(break_) = break_ else {
            panic!("the pre-first-fix decision was expected to break element-contract-2 and did not");
        };
        eprintln!("{break_}");

        // The frozen-play state on its own: potentially playing, data buffered, player running, and the
        // page calls play() a second time.
        let nav05 = MediaState {
            potentially_playing: true,
            player_playing: true,
            ready_state: ReadyLevel::HaveEnoughData,
            buffered_at_position: true,
            ..MediaState::default()
        };
        assert_eq!(
            decide_pre_first_fix(MediaEvent::RedundantPlay, &nav05).command,
            PlayerCommand::Pause,
            "the pre-fix decision paused the element the page had just asked to play"
        );
    }

    // --------------------------------------------------------------------- element-contract-3

    /// element-contract-3: every pause names a cause the element, the page or the specification
    /// ordered, and no pause happens under an element that is potentially playing.
    ///
    /// The causes are `paused` (the page or the element), ended playback, playback stopped due to an
    /// error, and a block for user interaction or in-band content (#blocked-media-element), which is
    /// also a stop the specification orders. Want of data is not on the list at all — not as a plain
    /// underrun and not as a seek that has nothing to decode, because `#blocked-media-element` writes
    /// no seeking term — and a pause under an element that is potentially playing never is.
    #[test]
    fn media_elem_03_every_pause_names_a_cause() {
        for state in reachable_states() {
            for event in ALL_EVENTS {
                let decision = decide(event, &state);
                if decision.command != PlayerCommand::Pause {
                    continue;
                }
                assert!(
                    !state.potentially_playing,
                    "{event:?}: a pause under an element that is potentially playing: {state:?}"
                );
                assert!(
                    ordered_pause(&state),
                    "{event:?}: {:?} is not a cause that was ordered: {state:?}",
                    decision.reason
                );
            }
        }
    }

    /// The list element-contract-3 allows, stated over the snapshot: the stops. The order is the
    /// specification's — #potentially-playing writes its four conditions, and the first three are
    /// `paused`, ended playback and stopped due to errors; the fourth, a blocked media element, is a
    /// stop only for its two content conditions.
    ///
    /// `seeking` was on this list until the review's findings 1 and 2 took it off: #blocked-media-element
    /// has no seeking term, so a seek that has nothing to decode and a mid-playback underrun are the
    /// same state, and pausing either one is the stop nothing answers — a paused engine emits no
    /// `PlaybackState::Playing`, which is the only thing that raises the ready state back above
    /// `HAVE_CURRENT_DATA`.
    fn ordered_pause(state: &MediaState) -> bool {
        state.paused || state.ended || state.error || state.blocked_by_content_or_interaction()
    }

    /// Want of data is not a pause anybody ordered, and the element cannot answer one later: it is
    /// not paused, so nothing ever runs the play steps again. That is the frozen-play freeze.
    #[test]
    fn media_elem_03_want_of_data_issues_no_command() {
        let starved = MediaState {
            player_playing: true,
            ready_state: ReadyLevel::HaveCurrentData,
            buffered_at_position: false,
            ..MediaState::default()
        };
        for event in ALL_EVENTS {
            if event.is_play() {
                continue;
            }
            assert_eq!(
                decide(event, &starved),
                Decision {
                    command: PlayerCommand::None,
                    reason: Reason::Starved,
                },
                "{event:?} over a starved element"
            );
        }
    }

    /// The pre-fix decision stops a starved element as well: `is_playing` alone opened the branch.
    #[test]
    fn media_elem_03_fails_for_the_pre_fix_decision() {
        let break_ = first_break(decide_pre_first_fix, |event, state, decision| {
            (decision.command == PlayerCommand::Pause && !ordered_pause(state))
                .then(|| format!("element-contract-3 broken: {event:?} over {state:?} -> {decision:?}"))
        });
        assert!(
            break_.is_some(),
            "expected a pre-fix pause with no cause behind it"
        );
    }

    /// The seek that the first round of this change paused, and why that was wrong: a seek into a
    /// hole is `#blocked-media-element` with no more and no less — the seek algorithm's step 12 waits
    /// for the data, it does not tell the element to stop the engine — and a paused engine sends no
    /// `PlaybackState::Playing`, which is the only thing that raises the ready state out of
    /// `HAVE_CURRENT_DATA`. So the pause the round-one decision issued there was never answered. The
    /// answer now is the same as for a plain underrun: nothing, and the engine's own answer arrives as
    /// a ready-state change that restarts the element if anything ever stopped it.
    #[test]
    fn media_elem_03_a_seek_that_left_nothing_to_decode_pauses_nothing() {
        let seek_into_a_hole = MediaState {
            seeking: true,
            player_playing: true,
            ready_state: ReadyLevel::HaveCurrentData,
            buffered_at_position: false,
            ..MediaState::default()
        };
        for event in [
            MediaEvent::SeekStart,
            MediaEvent::Waiting,
            MediaEvent::SeekEnd,
        ] {
            assert_eq!(
                decide(event, &seek_into_a_hole),
                Decision::none(Reason::Starved),
                "{event:?} over a seek that has nothing to decode",
            );
        }
        // Red first, against this branch's own previous answer: the same snapshot, and the round-one
        // decision stops the engine the wait is waiting for.
        for event in [
            MediaEvent::SeekStart,
            MediaEvent::Waiting,
            MediaEvent::SeekEnd,
        ] {
            assert_eq!(
                decide_pre_contract_rewrite(event, &seek_into_a_hole).command,
                PlayerCommand::Pause,
                "the round-one decision was expected to pause here ({event:?})"
            );
        }
        eprintln!(
            "element-contract-3 before round 2: a seek with nothing to decode answered pause to all \
             three seek events; now none of them command anything"
        );

        // And the mirror of the same window with the engine already stopped: the page wants playback,
        // so the engine is started, which is what turns the wait into an answer (finding 1).
        let stopped_mid_seek = MediaState {
            player_playing: false,
            ..seek_into_a_hole
        };
        assert_eq!(
            decide(MediaEvent::SeekEnd, &stopped_mid_seek),
            Decision {
                command: PlayerCommand::Play,
                reason: Reason::BlockedButAskedToPlay,
            },
        );
    }

    // --------------------------------------------------------------------- element-contract-4

    /// element-contract-4: a state that ends with `play()` over an element with data buffered at the
    /// position leaves the player playing. Over every state; the sequence form is below. The
    /// precondition is the specification's — a play the element is allowed to make and data to play —
    /// and it now includes the blocked element whose engine stands still, which is the half round one
    /// left frozen (finding 1).
    #[test]
    fn media_elem_04_holds_for_every_state_ending_with_a_play() {
        for state in all_states() {
            for event in [MediaEvent::Play, MediaEvent::RedundantPlay] {
                if !state.wants_playback() || !state.buffered_at_position {
                    continue;
                }
                if state.blocked_by_content_or_interaction() {
                    // A stop the content holds, not the data: element-contract-3 answers it with a pause.
                    continue;
                }
                let decision = decide(event, &state);
                let mut player = ModelPlayer {
                    playing: state.player_playing,
                };
                player.apply(decision.command);
                assert!(
                    player.playing,
                    "{event:?} over {state:?} leaves the player stopped: {decision:?}"
                );
            }
        }
    }

    // --------------------------------------------------------------------- element-contract-5

    /// element-contract-5: an element blocked for want of data is not potentially playing
    /// (`#blocked-media-element` is one of the four conditions of `#potentially-playing`), and the
    /// answer to that block is the engine's own: never a pause, and never an engine left stopped
    /// under a page that wants playback. Both halves matter, and the second is the review's finding 1:
    /// the first round issued no command at all for a starved element whose engine stood still, and a
    /// standstill engine reports nothing ever again — the ready state cannot rise, so the element
    /// cannot become potentially playing, so nothing plays it.
    #[test]
    fn media_elem_05_a_block_of_data_is_neither_a_pause_nor_a_stop() {
        for state in reachable_states() {
            if !state.blocked_for_want_of_data() {
                continue;
            }
            for event in ALL_EVENTS {
                let decision = decide(event, &state);
                if state.paused || state.ended || state.error {
                    // Want of data is not the only thing holding, and the other condition is what
                    // answers (element-contract-3's first three causes).
                    assert_ne!(
                        decision.command,
                        PlayerCommand::Play,
                        "{event:?} started the player of an element in a stop: {state:?}"
                    );
                    continue;
                }
                assert_ne!(
                    decision.command,
                    PlayerCommand::Pause,
                    "{event:?} paused an element for want of data, which is the stop nothing \
                     answers: {state:?}"
                );
                let mut player = ModelPlayer {
                    playing: state.player_playing,
                };
                player.apply(decision.command);
                assert!(
                    player.playing,
                    "{event:?} over {state:?} leaves the engine standing still: {decision:?}"
                );
            }
        }

        // Red first: how many of those windows the first round left stopped, and what answers them
        // now. This is the freeze finding 1 names, counted rather than argued.
        let left_stopped = reachable_states()
            .into_iter()
            .filter(|state| {
                state.blocked_for_want_of_data()
                    && state.wants_playback()
                    && !state.player_playing
                    && decide_pre_contract_rewrite(MediaEvent::Play, state).command != PlayerCommand::Play
            })
            .count();
        assert!(
            left_stopped > 0,
            "the round-one decision was expected to leave a starved element's engine stopped"
        );
        let now_stopped = reachable_states()
            .into_iter()
            .filter(|state| {
                state.blocked_for_want_of_data()
                    && state.wants_playback()
                    && !state.player_playing
                    && decide(MediaEvent::Play, state).command != PlayerCommand::Play
            })
            .count();
        assert_eq!(now_stopped, 0, "the restart half of element-contract-5 broke");
        eprintln!(
            "element-contract-5 before round 2: {left_stopped} want-of-data states left the engine \
             stopped with the page asking for playback; now {now_stopped}"
        );

        // And the snapshot's own arithmetic: with the three other conditions clear, want of data on
        // its own makes the element not potentially playing. That is what the element must hand over,
        // so `is_potentially_playing()` cannot be true here; see the `waiting` half of
        // `change_ready_state` and the fix in `playback_state_changed`'s Buffering arm.
        let starved = MediaState {
            ready_state: ReadyLevel::HaveCurrentData,
            buffered_at_position: false,
            ..MediaState::default()
        };
        assert!(!element_can_be(&MediaState {
            potentially_playing: true,
            ..starved
        }));
        assert!(!starved.potentially_playing);
        // The engine running under the block: nothing to do, and nothing to stop.
        assert_eq!(
            decide(
                MediaEvent::Waiting,
                &MediaState {
                    player_playing: true,
                    ..starved
                }
            ),
            Decision::none(Reason::Starved),
        );
        // The engine stopped under the same block, with the page wanting playback: the restart
        // finding 1 asked for, and the half the first round left frozen.
        assert_eq!(
            decide(MediaEvent::Waiting, &starved),
            Decision {
                command: PlayerCommand::Play,
                reason: Reason::BlockedButAskedToPlay,
            },
        );
    }

    // --------------------------------------------------------------------- element-contract-6

    /// element-contract-6: `waiting` stands for want of data that is expected to come back, so the
    /// ready-state steps fire it only where "playback has not stopped due to errors" holds. The
    /// decision reads the same distinction from its side: an errored state is the error's stop
    /// whatever the data says, a plain stall is nobody's stop.
    #[test]
    fn media_elem_06_an_error_is_a_stop_and_a_stall_is_not() {
        // "playback has not stopped due to errors" is the third condition of potentially playing, so
        // no state with an error may claim it, and the `waiting` that an errored element does answer
        // is the error's, not the data's.
        let errored = MediaState {
            ready_state: ReadyLevel::HaveEnoughData,
            buffered_at_position: true,
            player_playing: true,
            error: true,
            ..MediaState::default()
        };
        assert!(
            !element_can_be(&MediaState {
                potentially_playing: true,
                ..errored
            }),
            "an element whose playback has stopped due to errors is not potentially playing, \
             however much data it has"
        );
        assert_eq!(
            decide(MediaEvent::Waiting, &errored),
            Decision {
                command: PlayerCommand::Pause,
                reason: Reason::StoppedDueToError,
            },
        );

        // The plain stall the fork's inverted clause kept silent: same shape, no error, and the
        // answer is a player left running for the data to arrive, not a stop.
        let stalled = MediaState {
            ready_state: ReadyLevel::HaveCurrentData,
            buffered_at_position: false,
            player_playing: true,
            ..MediaState::default()
        };
        assert!(element_can_be(&stalled));
        assert_eq!(
            decide(MediaEvent::Waiting, &stalled),
            Decision::none(Reason::Starved),
        );
    }

    // --------------------------------------------------------------------- element-contract-7

    /// element-contract-7: the backwards end of playback leaves `paused` false and the element at the
    /// earliest position, so `ended` is the only thing that stops the player.
    #[test]
    fn media_elem_07_the_backwards_end_of_playback_pauses_the_player() {
        let backwards = MediaState {
            ended: true,
            ready_state: ReadyLevel::HaveEnoughData,
            player_playing: true,
            buffered_at_position: true,
            ..MediaState::default()
        };
        assert!(
            !backwards.paused,
            "backwards end of playback does not set paused (#reaches-the-end)"
        );
        assert_eq!(
            decide(MediaEvent::Ended, &backwards),
            Decision {
                command: PlayerCommand::Pause,
                reason: Reason::EndedPlayback,
            }
        );
    }

    // --------------------------------------------------------------------- element-contract-8

    /// element-contract-8: playback stopped due to an error is not potentially playing, whatever the
    /// ready state says — the error outranks the want-of-data exemption.
    #[test]
    fn media_elem_08_a_fatal_media_error_pauses_the_player() {
        for ready_state in [
            ReadyLevel::HaveNothing,
            ReadyLevel::HaveCurrentData,
            ReadyLevel::HaveEnoughData,
        ] {
            for buffered in [true, false] {
                let errored = MediaState {
                    player_playing: true,
                    ready_state,
                    buffered_at_position: buffered,
                    error: true,
                    ..MediaState::default()
                };
                assert_eq!(
                    decide(MediaEvent::Load, &errored),
                    Decision {
                        command: PlayerCommand::Pause,
                        reason: Reason::StoppedDueToError,
                    },
                    "{ready_state:?} buffered={buffered}"
                );
            }
        }
    }

    // ------------------------------------------- the element-side steps, transcribed (review 1, 3, 4)
    //
    // Findings 3 and 4 are about `htmlmediaelement.rs`, which could not be compiled in the setup used: its build
    // gate needs a Python for `servo-script-bindings`, which that setup does not have (see the review
    // report). The rules those lines implement are transcribed here, before and after, so the
    // contracts that live in them can be run red and green instead of argued. Each function names the
    // line it stands for, in the working tree; the transcription itself is what the build gate has to
    // confirm against the code.

    /// The element as the fixes left it, against the fork at `fd3fd2cef`.
    #[derive(Clone, Copy, PartialEq)]
    enum Stage {
        /// Before this change: the `Buffering` arm returned without touching the ready state, the
        /// `waiting` clause read `!self.error.get().is_none()`, the backwards end of playback and the
        /// fatal steps ended at `update_keep_alive`, and neither ready-state arm asked whose playback
        /// had ended or errored.
        Before,
        /// The first round of this change: the arms were widened as finding 1 needs them to be, but
        /// neither had the guard finding 4 asked for, and the decision still paused a seek that had
        /// nothing to decode.
        RoundOne,
        /// This branch.
        After,
    }

    /// `htmlmediaelement.rs:3278-3280`, which the Playing raise (`:3305`) and the Buffering guard
    /// (`:3323`) share. An element that has ended playback cannot hold `HAVE_FUTURE_DATA` or
    /// `HAVE_ENOUGH_DATA` — "#ready-states": *"The user agent cannot be in this state if playback
    /// has ended, as the current playback position can never advance in this case"* — and an
    /// element whose playback has stopped due to errors
    /// (<https://html.spec.whatwg.org/multipage/#stopped-due-to-errors>) is not able to play the
    /// content at the position, which is the same absence written the other way (finding 4). The
    /// `paused` attribute is not part of it: an element the page paused still has to say in its
    /// ready state when the data runs out, because that is the element's data question and not the
    /// page's order.
    fn can_hold_future_data(state: &MediaState) -> bool {
        !state.ended && !state.error
    }

    /// The `waiting` clause of the ready-state steps (`htmlmediaelement.rs:1200-1207`), which is
    /// finding 3 in one line: before the fix the error term was inverted, so the event fired exactly
    /// where it must not and stayed silent exactly where it must. `was_potentially_playing` is the
    /// element's own answer *before* the new ready state is written (`:1121`), which the transcription
    /// derives from the old level and the three stops.
    fn waiting_fires(stage: Stage, state: &MediaState, old: ReadyLevel) -> bool {
        let was_potentially_playing =
            !state.paused && !state.ended && !state.error && old.has_future_data();
        was_potentially_playing
            && !state.ended
            && match stage {
                Stage::Before => state.error,
                Stage::RoundOne | Stage::After => !state.error,
            }
    }

    /// `change_ready_state`'s step 1 (`htmlmediaelement.rs:1134-1286`), as far as these findings need
    /// it: the events a transition queues. `waiting` and `timeupdate` are the clause above; `playing`
    /// is `notify_about_playing`, which the ready-state steps run for an element whose `paused`
    /// attribute is false.
    fn ready_state_events(stage: Stage, state: &MediaState, new: ReadyLevel) -> Vec<&'static str> {
        let old = state.ready_state;
        let mut out = Vec::new();
        if old == new {
            return out;
        }
        if old == ReadyLevel::HaveNothing && new == ReadyLevel::HaveMetadata {
            out.push("loadedmetadata");
            return out;
        }
        if old == ReadyLevel::HaveMetadata && new >= ReadyLevel::HaveCurrentData {
            out.push("loadeddata");
            if new < ReadyLevel::HaveFutureData {
                return out;
            }
        }
        if old >= ReadyLevel::HaveFutureData
            && new <= ReadyLevel::HaveCurrentData
            && waiting_fires(stage, state, old)
        {
            out.push("timeupdate");
            out.push("waiting");
        }
        if old <= ReadyLevel::HaveCurrentData && new >= ReadyLevel::HaveFutureData {
            out.push("canplay");
            if !state.paused {
                out.push("playing");
            }
            if new == ReadyLevel::HaveEnoughData {
                out.push("canplaythrough");
            }
        }
        out
    }

    /// `PlaybackState::Buffering` (`htmlmediaelement.rs:3312-3336`): what the arm writes, and what
    /// that queues.
    fn buffering(stage: Stage, state: &MediaState) -> (Option<ReadyLevel>, Vec<&'static str>) {
        if stage == Stage::Before {
            return (None, Vec::new());
        }
        if stage == Stage::After && !can_hold_future_data(state) {
            return (None, Vec::new());
        }
        if state.ready_state > ReadyLevel::HaveCurrentData {
            let new = ReadyLevel::HaveCurrentData;
            let events = ready_state_events(stage, state, new);
            (Some(new), events)
        } else {
            (None, Vec::new())
        }
    }

    /// `PlaybackState::Playing` (`htmlmediaelement.rs:3291-3311`): the raise out of a want-of-data
    /// block, and what it queues. It ends the block; it is not the restart, which was commanded
    /// earlier, by the decision that found the mirror already stopped (finding 1).
    fn playing(stage: Stage, state: &MediaState) -> (Option<ReadyLevel>, Vec<&'static str>) {
        let raises = match stage {
            // `fd3fd2cef` raised out of `HAVE_METADATA` only.
            Stage::Before => state.ready_state == ReadyLevel::HaveMetadata,
            // The first round widened the arm for every want-of-data level, ended or not.
            Stage::RoundOne => {
                state.ready_state >= ReadyLevel::HaveMetadata
                    && state.ready_state < ReadyLevel::HaveFutureData
            },
            Stage::After => {
                state.ready_state >= ReadyLevel::HaveMetadata
                    && state.ready_state < ReadyLevel::HaveFutureData
                    && can_hold_future_data(state)
            },
        };
        if !raises {
            return (None, Vec::new());
        }
        let new = ReadyLevel::HaveEnoughData;
        (Some(new), ready_state_events(stage, state, new))
    }

    /// `end_of_playback_in_backwards_direction` (`htmlmediaelement.rs:2778-2795`). Before the fix the
    /// body queued its `timeupdate` and refreshed the keep-alive, and reached no decision at all.
    fn backwards_end(
        stage: Stage,
        state: &MediaState,
        decide_fn: fn(MediaEvent, &MediaState) -> Decision,
    ) -> Option<Decision> {
        match stage {
            Stage::Before => None,
            Stage::RoundOne | Stage::After => Some(decide_fn(MediaEvent::Ended, state)),
        }
    }

    /// Step 6 of `media_data_processing_fatal_steps` (`htmlmediaelement.rs:2295-2327`), same shape.
    fn fatal_steps(
        stage: Stage,
        state: &MediaState,
        decide_fn: fn(MediaEvent, &MediaState) -> Decision,
    ) -> Option<Decision> {
        match stage {
            Stage::Before => None,
            Stage::RoundOne | Stage::After => Some(decide_fn(MediaEvent::Load, state)),
        }
    }

    /// Finding 3, 05: the stall changed nothing in the fork, so the element kept a ready state that
    /// said "data ahead" over a position with no data, and the page never heard `waiting`.
    #[test]
    fn media_elem_05_the_stall_dropped_nothing_before_the_fix() {
        let stalled = MediaState {
            ready_state: ReadyLevel::HaveEnoughData,
            buffered_at_position: true,
            player_playing: true,
            potentially_playing: true,
            ..MediaState::default()
        };
        let (before_ready, before_events) = buffering(Stage::Before, &stalled);
        let (after_ready, after_events) = buffering(Stage::After, &stalled);
        assert_eq!(before_ready, None, "the fork's Buffering arm wrote nothing");
        assert!(
            before_events.is_empty(),
            "and so queued no event: {before_events:?}"
        );
        assert_eq!(
            after_ready,
            Some(ReadyLevel::HaveCurrentData),
            "the stall has to say so in the ready state"
        );
        assert_eq!(
            after_events,
            ["timeupdate", "waiting"],
            "and that transition is what the ready-state steps owe the page"
        );
        eprintln!(
            "element-contract-5 before: ready state {before_ready:?}, events {before_events:?}; \
             after: {after_ready:?}, {after_events:?}"
        );
    }

    /// Finding 3, 06: the clause table, run over both readings of the error term. The transcription is
    /// harsher than the first round's summary of the bug: `was_potentially_playing` is
    /// `is_potentially_playing()`, whose third condition is that the element has *no* error, so the
    /// fork's `!self.error.get().is_none()` made the whole condition unsatisfiable — the `waiting` (and
    /// the paired `timeupdate`) of that ready-state step could not fire in any state at all.
    #[test]
    fn media_elem_06_the_waiting_clause_was_inverted_before_the_fix() {
        let mut flipped = 0;
        for old in [ReadyLevel::HaveCurrentData, ReadyLevel::HaveEnoughData] {
            for paused in [false, true] {
                for ended in [false, true] {
                    for error in [false, true] {
                        let potentially_playing =
                            !paused && !ended && !error && old.has_future_data();
                        let state = MediaState {
                            paused,
                            ended,
                            error,
                            potentially_playing,
                            ready_state: old,
                            player_playing: true,
                            buffered_at_position: true,
                            ..MediaState::default()
                        };
                        let before = waiting_fires(Stage::Before, &state, old);
                        let after = waiting_fires(Stage::After, &state, old);
                        assert!(
                            !before,
                            "the fork's clause was unsatisfiable, yet it fired over {state:?}"
                        );
                        assert_eq!(
                            after,
                            potentially_playing && !ended && !error,
                            "the specification's clause, over {state:?}"
                        );
                        if before != after {
                            flipped += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(
            flipped, 1,
            "exactly the row a live stall sits in: potentially playing, nothing ended, no error"
        );
        eprintln!(
            "element-contract-6 before: the fork's `waiting` clause could not fire in any of the 16 rows \
             (was_potentially_playing needs no error, the inverted term demands one); after: it fires \
             in the {flipped} row a plain stall lives in"
        );
    }

    /// Finding 3, 07: the backwards end reached the player only through the fix.
    #[test]
    fn media_elem_07_the_backwards_end_reached_no_decision_before_the_fix() {
        let backwards = MediaState {
            ended: true,
            ready_state: ReadyLevel::HaveEnoughData,
            player_playing: true,
            buffered_at_position: true,
            ..MediaState::default()
        };
        assert_eq!(
            backwards_end(Stage::Before, &backwards, decide),
            None,
            "the old body ended at update_keep_alive"
        );
        let mut engine = ModelPlayer {
            playing: backwards.player_playing,
        };
        engine.apply(
            backwards_end(Stage::After, &backwards, decide)
                .expect("the fixed step runs the decision")
                .command,
        );
        assert!(
            !engine.playing,
            "an element at the earliest position, backwards, with the engine running"
        );
        eprintln!(
            "element-contract-7 before: no command at all, the engine still running; after: pause, \
             engine stopped"
        );
    }

    /// Finding 3, 08: same, for the fatal steps.
    #[test]
    fn media_elem_08_the_fatal_steps_reached_no_decision_before_the_fix() {
        let errored = MediaState {
            error: true,
            ready_state: ReadyLevel::HaveEnoughData,
            player_playing: true,
            buffered_at_position: true,
            ..MediaState::default()
        };
        assert_eq!(fatal_steps(Stage::Before, &errored, decide), None);
        let mut engine = ModelPlayer {
            playing: errored.player_playing,
        };
        engine.apply(
            fatal_steps(Stage::After, &errored, decide)
                .expect("step 6 runs the decision now")
                .command,
        );
        assert!(
            !engine.playing,
            "the audio of a resource that has died kept playing"
        );
        eprintln!(
            "element-contract-8 before: the fatal steps stopped the fetch and never stopped the engine; \
             after: pause"
        );
    }

    /// Finding 4: neither the raise nor the drop may touch an element whose playback has ended or has
    /// stopped due to errors. The forwards end of playback leaves the ready state at
    /// `HAVE_CURRENT_DATA` while `paused` is still false and the task that sets it is queued, so an
    /// engine signal in that interval is the case that matters: the raise would queue `canplay`,
    /// `playing` and `canplaythrough` at an element the ends-of-playback steps forbid them for.
    #[test]
    fn finding_4_the_ready_state_arms_leave_an_ended_or_errored_element_alone() {
        // The forwards end, before the queued task sets `paused`: #reaches-the-end step 3.2 wrote
        // HAVE_CURRENT_DATA at htmlmediaelement.rs:2774.
        let ended_before_the_task_runs = MediaState {
            ended: true,
            ready_state: ReadyLevel::HaveCurrentData,
            player_playing: true,
            buffered_at_position: true,
            ..MediaState::default()
        };
        let (ready, events) = playing(Stage::After, &ended_before_the_task_runs);
        assert_eq!(ready, None, "an ended element cannot hold HAVE_ENOUGH_DATA");
        assert!(
            events.is_empty(),
            "and no `canplay`/`playing`/`canplaythrough` may be queued at it: {events:?}"
        );
        // The first round's arm, unguarded, is the red half: it raises, and queues three events the
        // ends-of-playback steps leave out on purpose.
        let unguarded = playing(Stage::RoundOne, &ended_before_the_task_runs);
        assert_eq!(unguarded.0, Some(ReadyLevel::HaveEnoughData));
        assert!(
            unguarded.1.contains(&"canplaythrough"),
            "the first round queued {:?}",
            unguarded.1
        );

        // The drop is the other half: a Buffering signal after a fatal error would otherwise write a
        // ready state the fatal steps did not ask for, and queue the drop's events at it.
        let errored = MediaState {
            error: true,
            ready_state: ReadyLevel::HaveEnoughData,
            player_playing: true,
            buffered_at_position: false,
            ..MediaState::default()
        };
        assert_eq!(
            buffering(Stage::After, &errored),
            (None, Vec::new()),
            "the error already said what a stall would"
        );
        assert_eq!(
            buffering(Stage::RoundOne, &errored).0,
            Some(ReadyLevel::HaveCurrentData),
            "and the first round wrote it anyway"
        );
        assert_eq!(playing(Stage::After, &errored).0, None);

        // While the element is clear, both arms still work: that is element-contract-5's round trip.
        let clear = MediaState {
            potentially_playing: true,
            ready_state: ReadyLevel::HaveEnoughData,
            player_playing: true,
            buffered_at_position: true,
            ..MediaState::default()
        };
        assert_eq!(
            buffering(Stage::After, &clear).0,
            Some(ReadyLevel::HaveCurrentData)
        );
        assert_eq!(
            buffering(Stage::Before, &clear).0,
            None,
            "which is what finding 3 asked to see failing"
        );
        let stalled = MediaState {
            ready_state: ReadyLevel::HaveCurrentData,
            potentially_playing: false,
            buffered_at_position: false,
            ..clear
        };
        let (raised, raised_events) = playing(Stage::After, &stalled);
        assert_eq!(
            raised,
            Some(ReadyLevel::HaveEnoughData),
            "the raise ends a want-of-data block on an element in no stop"
        );
        assert!(
            raised_events.contains(&"playing"),
            "{raised_events:?}: notify_about_playing is what tells the page it is playing again"
        );
        eprintln!(
            "finding 4: the raise and the drop now ask whose playback ended or errored; the first \
             round raised an ended element to HAVE_ENOUGH_DATA ({:?}) and dropped an errored one to \
             HAVE_CURRENT_DATA",
            unguarded.1
        );
    }

    /// Finding 1, the round trip: a seek that runs out of data. The engine's rule is the fork's own —
    /// `PlaybackState::Playing` is what a *running* engine reports, and `player.paused()` mirrors the
    /// last command, so an engine the element paused reports nothing at all
    /// (`components/media/backends/gstreamer/player.rs:226-239`). That is why the pause the first round
    /// sent inside a seek was the last command anybody would ever send.
    #[test]
    fn finding_1_a_seek_that_runs_out_of_data_starts_the_engine_again() {
        let variants: [(&str, fn(MediaEvent, &MediaState) -> Decision); 4] = [
            ("pre-first-fix", decide_pre_first_fix),
            ("first-fix", decide_first_fix),
            ("pre-contract-rewrite (round 1)", decide_pre_contract_rewrite),
            ("contract-rewrite", decide),
        ];
        let mut frozen_names = Vec::new();
        for (name, decide_fn) in variants {
            let (state, player, trail) = seek_out_of_data(decide_fn);
            let frozen = !player.playing && !state.potentially_playing && !state.paused;
            eprintln!("finding 1, {name}:\n{}", trail.join("\n"));
            if frozen {
                frozen_names.push(name);
            }
            if name == "contract-rewrite" {
                assert!(!frozen, "the fixed decision still freezes the seek");
                assert!(player.playing, "{trail:?}");
                assert!(state.potentially_playing, "{trail:?}");
                assert_eq!(state.ready_state, ReadyLevel::HaveEnoughData, "{trail:?}");
            }
        }
        assert_eq!(
            frozen_names,
            ["pre-first-fix", "first-fix", "pre-contract-rewrite (round 1)"],
            "every decision before this fix was expected to freeze the seek"
        );
    }

    /// The element's own arithmetic, transcribed from `is_potentially_playing()`
    /// (`htmlmediaelement.rs:2000-2008`): the three stops and the ready state. Not the buffered
    /// ranges, which the element does not read (finding 5) — that is why a seek with nothing decoded
    /// yet is still potentially playing at step 4, and why the block only arrives with the stall.
    fn as_the_element_sees_it(state: &mut MediaState) {
        state.potentially_playing = state.wants_playback() && state.ready_state.has_future_data();
    }

    /// The round trip itself: the element's arms as `Stage::After` writes them, and the decision under
    /// test. The seek runs out of data and the data comes back; the trail is the trace lines each step
    /// would print.
    fn seek_out_of_data(
        decide_fn: fn(MediaEvent, &MediaState) -> Decision,
    ) -> (MediaState, ModelPlayer, Vec<String>) {
        let mut state = MediaState {
            potentially_playing: true,
            ready_state: ReadyLevel::HaveEnoughData,
            player_playing: true,
            buffered_at_position: true,
            ..MediaState::default()
        };
        let mut player = ModelPlayer { playing: true };
        let mut trail: Vec<String> = Vec::new();
        let mut record = |label: &'static str,
                          event: MediaEvent,
                          state: &MediaState,
                          player: &mut ModelPlayer| {
            let decision = decide_fn(event, state);
            player.apply(decision.command);
            trail.push(format!(
                "  {label:<22} {} ({})",
                trace_line(event, state, &decision),
                decision.reason.as_str()
            ));
        };

        // Seek steps 4 and 11: `seeking` true, the position moved, and the flush has not established
        // anything yet. The ready state still says there is more to play, so the element is still
        // potentially playing and the running engine is left alone.
        state.seeking = true;
        state.buffered_at_position = false;
        as_the_element_sees_it(&mut state);
        record("seek start", MediaEvent::SeekStart, &state, &mut player);

        // The engine's answer to the flush is a stall: the Buffering arm drops the ready state
        // (finding 4's guard passes, nothing has ended or errored) and `change_ready_state` ends with
        // the decision, still inside the seek window.
        let (dropped, _) = buffering(Stage::After, &state);
        if let Some(new) = dropped {
            state.ready_state = new;
            as_the_element_sees_it(&mut state);
        }
        record("buffering", MediaEvent::Waiting, &state, &mut player);

        // Seek step 14: `seeking` false, and step 12's wait has been answered — the data at the new
        // position is there now. The ready state only rises when the engine says so.
        state.seeking = false;
        state.buffered_at_position = true;
        as_the_element_sees_it(&mut state);
        record("seek end", MediaEvent::SeekEnd, &state, &mut player);

        // And the engine's rule, which is the whole of finding 1: `PlaybackState::Playing` is what an
        // engine that is *running* reports, so an engine the element paused reports nothing, the ready
        // state cannot rise, and the element never becomes potentially playing again.
        if player.playing {
            let (raised, _) = playing(Stage::After, &state);
            if let Some(new) = raised {
                state.ready_state = new;
                as_the_element_sees_it(&mut state);
            }
            record(
                "playing, data back",
                MediaEvent::Waiting,
                &state,
                &mut player,
            );
        } else {
            trail.push(
                "  (no Playing signal: a paused engine reports nothing, so nothing raises the \
                 ready state and the element sits blocked with paused=false)"
                    .to_owned(),
            );
        }
        (state, player, trail)
    }

    #[test]
    fn the_trace_line_carries_the_fixed_fields_in_the_fixed_order() {
        let running = MediaState {
            potentially_playing: true,
            player_playing: true,
            ready_state: ReadyLevel::HaveEnoughData,
            buffered_at_position: true,
            ..MediaState::default()
        };
        let decision = decide(MediaEvent::RedundantPlay, &running);
        assert_eq!(
            trace_line(MediaEvent::RedundantPlay, &running, &decision),
            "media elem event=play-redundant paused=0 ended=0 seeking=0 potentially_playing=1 \
             player_playing=1 cmd=none"
        );

        let paused = MediaState {
            paused: true,
            player_playing: true,
            ready_state: ReadyLevel::HaveEnoughData,
            buffered_at_position: true,
            ..MediaState::default()
        };
        let decision = decide(MediaEvent::Pause, &paused);
        assert_eq!(
            trace_line(MediaEvent::Pause, &paused, &decision),
            "media elem event=pause paused=1 ended=0 seeking=0 potentially_playing=0 \
             player_playing=1 cmd=pause"
        );

        let backwards = MediaState {
            ended: true,
            seeking: true,
            player_playing: true,
            ready_state: ReadyLevel::HaveEnoughData,
            buffered_at_position: true,
            ..MediaState::default()
        };
        let decision = decide(MediaEvent::Ended, &backwards);
        assert_eq!(
            trace_line(MediaEvent::Ended, &backwards, &decision),
            "media elem event=ended paused=0 ended=1 seeking=1 potentially_playing=0 \
             player_playing=1 cmd=pause"
        );
    }

    /// Every event name the format allows, so the checker's list is covered from this side.
    #[test]
    fn every_event_names_itself_the_way_the_checker_reads_it() {
        let names: Vec<&str> = ALL_EVENTS.iter().map(|event| event.trace_name()).collect();
        assert_eq!(
            names,
            [
                "play",
                "play-redundant",
                "pause",
                "seek-start",
                "seek-end",
                "waiting",
                "ended",
                "load"
            ]
        );
    }

    /// The line the frozen-play `play2` run printed the moment it froze, as the pre-fix decision would
    /// have traced it, and the line the fixed decision traces in its place.
    #[test]
    fn the_play2_trace_before_and_after() {
        let playing = MediaState {
            potentially_playing: true,
            player_playing: true,
            ready_state: ReadyLevel::HaveEnoughData,
            buffered_at_position: true,
            ..MediaState::default()
        };
        let before = decide_pre_first_fix(MediaEvent::RedundantPlay, &playing);
        let after = decide(MediaEvent::RedundantPlay, &playing);
        eprintln!(
            "pre-first-fix: {}\nfixed:      {}",
            trace_line(MediaEvent::RedundantPlay, &playing, &before),
            trace_line(MediaEvent::RedundantPlay, &playing, &after),
        );
        assert_eq!(before.command, PlayerCommand::Pause);
        assert_eq!(after.command, PlayerCommand::None);
    }

    // ------------------------------------------------------------------------ property tests
    //
    // Seeded xorshift64*, twelve lines and no dependency: the sequences are random and
    // reproducible. `search` runs them through the decision and a model player, and answers with
    // the whole trail of the first sequence that breaks the property it is given.

    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: u64) -> usize {
            (self.next_u64() % n) as usize
        }

        fn flag(&mut self) -> bool {
            self.below(2) == 0
        }
    }

    /// The flags the element's own algorithms write, and the two facts about its data.
    #[derive(Clone, Copy, Debug)]
    struct Element {
        paused: bool,
        ended: bool,
        seeking: bool,
        error: bool,
        /// Whether the resource has data ahead of the position: readyState ≥ HAVE_FUTURE_DATA.
        future_data: bool,
        /// Whether the buffered ranges cover the position.
        buffered: bool,
    }

    impl Element {
        /// What the element's state is, and therefore the snapshot the element would build, after
        /// running `event`. The play steps clear `paused` and seek an ended element back to the
        /// earliest position; the pause steps set it; a seek sets `seeking` and clears it; the load
        /// algorithm resets the element. End of playback sets `ended` and — forwards only —
        /// `paused`, which the model cannot tell apart, so it leaves `paused` as it was and the
        /// generator supplies both shapes.
        fn after(&self, event: MediaEvent, player: &ModelPlayer) -> (Element, MediaState) {
            let mut elem = *self;
            match event {
                MediaEvent::Play | MediaEvent::RedundantPlay => {
                    elem.paused = false;
                    if elem.ended {
                        elem.seeking = false;
                        elem.ended = false;
                    }
                },
                MediaEvent::Pause => elem.paused = true,
                MediaEvent::SeekStart => elem.seeking = true,
                MediaEvent::SeekEnd => elem.seeking = false,
                MediaEvent::Waiting => {},
                MediaEvent::Ended => elem.ended = true,
                MediaEvent::Load => {
                    elem.paused = true;
                    elem.ended = false;
                    elem.seeking = false;
                },
            }
            // The element's own arithmetic: the three stops and the ready state. The buffered ranges
            // are not in it — `is_potentially_playing()` never reads them, and neither does
            // `#blocked-media-element` (finding 5).
            let potentially_playing =
                !elem.paused && !elem.ended && !elem.error && elem.future_data;
            let state = MediaState {
                paused: elem.paused,
                ended: elem.ended,
                seeking: elem.seeking,
                potentially_playing,
                ready_state: if elem.future_data {
                    ReadyLevel::HaveEnoughData
                } else {
                    ReadyLevel::HaveCurrentData
                },
                player_playing: player.playing,
                buffered_at_position: elem.buffered,
                error: elem.error,
            };
            (elem, state)
        }
    }

    /// `rounds` sequences of `steps` events through `decide_fn` and the model player. `want` is the
    /// property, and answers with the break it found.
    fn search<F>(
        decide_fn: fn(MediaEvent, &MediaState) -> Decision,
        rounds: usize,
        steps: usize,
        want: &F,
    ) -> Option<String>
    where
        F: Fn(MediaEvent, &MediaState, Decision, &ModelPlayer) -> Option<String>,
    {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for seed in 0..rounds {
            let mut elem = Element {
                paused: rng.flag(),
                ended: rng.flag(),
                seeking: rng.flag(),
                error: rng.flag(),
                future_data: rng.flag(),
                buffered: rng.flag(),
            };
            let mut player = ModelPlayer {
                playing: rng.flag(),
            };
            let mut trail = Vec::new();
            for _ in 0..steps {
                let event = ALL_EVENTS[rng.below(ALL_EVENTS.len() as u64)];
                let (next, state) = elem.after(event, &player);
                let decision = decide_fn(event, &state);
                trail.push(step(event, &state, decision));
                let broke = want(event, &state, decision, &player);
                player.apply(decision.command);
                elem = next;
                // The data situation is the resource's, not the element's: it drifts.
                elem.future_data = rng.below(4) != 0 && elem.future_data || rng.flag();
                elem.buffered = rng.below(4) != 0 && elem.buffered || rng.flag();
                if let Some(why) = broke {
                    return Some(format!(
                        "sequence {seed} broke:\n  {}\n  {why}",
                        trail.join("\n  ")
                    ));
                }
            }
        }
        None
    }

    fn step(event: MediaEvent, state: &MediaState, decision: Decision) -> String {
        format!(
            "{:>13}  pp={} paused={} ended={} seeking={} error={} ready={:?} buffered={} mirror={} -> {} ({})",
            event.trace_name(),
            state.potentially_playing as u8,
            state.paused as u8,
            state.ended as u8,
            state.seeking as u8,
            state.error as u8,
            state.ready_state,
            state.buffered_at_position as u8,
            state.player_playing as u8,
            decision.command.trace_name(),
            decision.reason.as_str(),
        )
    }

    /// element-contract-2 as a property over sequences: no play, redundant or not, ever commands a stop
    /// over an element that is potentially playing.
    fn play_never_pauses(
        event: MediaEvent,
        state: &MediaState,
        decision: Decision,
        _player: &ModelPlayer,
    ) -> Option<String> {
        (event.is_play() && state.potentially_playing && decision.command == PlayerCommand::Pause)
            .then(|| "element-contract-2: a play commanded a pause over a playing element".to_owned())
    }

    /// element-contract-4 as a property over sequences: one that ends with `play()` over an element with
    /// data buffered at the position leaves the player playing.
    fn play_with_data_leaves_the_player_running(
        event: MediaEvent,
        state: &MediaState,
        decision: Decision,
        player: &ModelPlayer,
    ) -> Option<String> {
        if !event.is_play() || !state.potentially_playing || !state.buffered_at_position {
            return None;
        }
        let mut after = *player;
        after.apply(decision.command);
        (!after.playing).then(|| {
            "element-contract-4: the sequence ends with play() and data buffered, and the player stands stopped".to_owned()
        })
    }

    #[test]
    fn media_elem_02_holds_over_random_event_sequences() {
        let break_ = search(decide, 4000, 12, &play_never_pauses);
        assert_eq!(break_, None, "the decision breaks element-contract-2");
    }

    #[test]
    fn media_elem_04_holds_over_random_event_sequences() {
        let break_ = search(decide, 4000, 12, &play_with_data_leaves_the_player_running);
        assert_eq!(break_, None, "the decision breaks element-contract-4");
    }

    #[test]
    fn media_elem_03_holds_over_random_event_sequences() {
        fn pause_needs_a_cause(
            event: MediaEvent,
            state: &MediaState,
            decision: Decision,
            _player: &ModelPlayer,
        ) -> Option<String> {
            if decision.command != PlayerCommand::Pause {
                return None;
            }
            if state.potentially_playing {
                return Some(
                    "element-contract-3: a pause under an element that is potentially playing"
                        .to_owned(),
                );
            }
            (!ordered_pause(state))
                .then(|| format!("element-contract-3: a pause nobody ordered ({event:?})"))
        }
        let break_ = search(decide, 4000, 12, &pause_needs_a_cause);
        assert_eq!(break_, None, "the decision breaks element-contract-3");
    }

    #[test]
    fn media_elem_01_holds_over_random_event_sequences() {
        fn mirror_matches_the_element(
            _event: MediaEvent,
            state: &MediaState,
            decision: Decision,
            player: &ModelPlayer,
        ) -> Option<String> {
            let mut after = *player;
            after.apply(decision.command);
            let starved = state.wants_playback() && state.blocked_for_want_of_data();
            (after.playing != state.potentially_playing && !starved).then(|| {
                format!(
                    "element-contract-1: the element says potentially_playing={}, the player ends {}",
                    state.potentially_playing as u8,
                    if after.playing { "playing" } else { "stopped" }
                )
            })
        }
        let break_ = search(decide, 4000, 12, &mirror_matches_the_element);
        assert_eq!(break_, None, "the decision breaks element-contract-1");
    }

    /// The before half of element-contract-2 and element-contract-4, on the same seeded sequences: the old
    /// decision has to break both, or first-fix changed nothing these contracts are about.
    #[test]
    fn the_random_sequences_break_media_elem_02_before_the_fix() {
        let break_ = search(decide_pre_first_fix, 4000, 12, &play_never_pauses);
        let Some(break_) = break_ else {
            panic!(
                "the pre-first-fix decision was expected to break element-contract-2 over the sequences and did not"
            );
        };
        eprintln!("element-contract-2 over random sequences, pre-first-fix:\n{break_}");
    }

    #[test]
    fn the_random_sequences_break_media_elem_04_before_the_fix() {
        let break_ = search(
            decide_pre_first_fix,
            4000,
            12,
            &play_with_data_leaves_the_player_running,
        );
        let Some(break_) = break_ else {
            panic!(
                "the pre-first-fix decision was expected to break element-contract-4 over the sequences and did not"
            );
        };
        eprintln!("element-contract-4 over random sequences, pre-first-fix:\n{break_}");
    }

    #[test]
    fn the_random_sequences_break_media_elem_03_before_the_fix() {
        fn pause_needs_a_cause(
            _event: MediaEvent,
            state: &MediaState,
            decision: Decision,
            _player: &ModelPlayer,
        ) -> Option<String> {
            (decision.command == PlayerCommand::Pause && !ordered_pause(state))
                .then(|| "element-contract-3: a pause nobody ordered".to_owned())
        }
        let break_ = search(decide_pre_first_fix, 4000, 12, &pause_needs_a_cause);
        assert!(
            break_.is_some(),
            "the pre-first-fix decision was expected to break element-contract-3 over the sequences and did not"
        );
    }

    /// Where splitting the event out changes the answer against the code first-fix left, and why it is
    /// allowed to: one window only, want of data outside a seek, where first-fix still stopped the
    /// player although no cause on element-contract-3's list ordered a stop (and the stop was the pause
    /// nothing answers). Everywhere else the two agree, which is the "behaviour after first-fix
    /// unchanged" of this task.
    /// Where this branch's decision differs from the one first-fix merged, and why each difference is
    /// allowed. One window only: want of data while the page still wants playback. Inside it the two
    /// commands first-fix gave are both wrong — a running engine it paused (the stop nothing answers,
    /// which is the frozen-play freeze and review finding 1) and a stopped engine it left stopped, which is
    /// finding 1's other half: a paused engine reports no `PlaybackState::Playing`, so nothing ever
    /// raises the ready state and the element cannot become potentially playing again. Everywhere else
    /// the two agree, which is the "behaviour after first-fix unchanged" of this task.
    #[test]
    fn the_split_differs_from_first_fix_only_where_a_contract_says_it_must() {
        let mut pause_taken_out = 0;
        let mut stop_taken_out = 0;
        let mut agree = 0;
        for state in reachable_states() {
            for event in ALL_EVENTS {
                let mine = decide(event, &state).command;
                let theirs = decide_first_fix(event, &state).command;
                if mine == theirs {
                    agree += 1;
                    continue;
                }
                assert!(
                    state.wants_playback() && state.blocked_for_want_of_data(),
                    "{event:?} {state:?}: {} vs {} differs with no contract behind it",
                    mine.trace_name(),
                    theirs.trace_name()
                );
                if state.player_playing {
                    assert_eq!(mine, PlayerCommand::None, "{event:?} {state:?}");
                    assert_eq!(theirs, PlayerCommand::Pause, "{event:?} {state:?}");
                    pause_taken_out += 1;
                } else {
                    assert_eq!(mine, PlayerCommand::Play, "{event:?} {state:?}");
                    assert_eq!(theirs, PlayerCommand::None, "{event:?} {state:?}");
                    stop_taken_out += 1;
                }
            }
        }
        let pairs = reachable_states().len() * ALL_EVENTS.len();
        eprintln!(
            "{}/{} state-and-event pairs are ones an element can actually be in; {agree} agree with \
             first-fix and {} differ, all of them in the want-of-data window: {pause_taken_out} \
             pauses taken out (an engine running under nothing to play) and {stop_taken_out} starts \
             put in (an engine stopped under a page that wants playback)",
            pairs,
            all_states().len() * ALL_EVENTS.len(),
            pause_taken_out + stop_taken_out,
        );
        assert!(pause_taken_out > 0 && stop_taken_out > 0, "both halves");
    }

    /// The frozen `play2` case of frozen-play §1c as one run of decisions: load, the data arrives, the
    /// page plays, and two seconds later the page's state sync plays again with no seek anywhere
    /// near it. The player has to still be running at the end of it.
    #[test]
    fn the_nav05_play2_sequence_keeps_the_player_running() {
        let mut elem = Element {
            paused: true,
            ended: false,
            seeking: false,
            error: false,
            future_data: false,
            buffered: false,
        };
        let mut player = ModelPlayer::default();
        let run = |event, elem: &mut Element, player: &mut ModelPlayer| {
            let (next, state) = elem.after(event, player);
            let decision = decide(event, &state);
            eprintln!("  {}", step(event, &state, decision));
            player.apply(decision.command);
            *elem = next;
            decision
        };

        eprintln!("first-fix-fixed decision:");
        run(MediaEvent::Load, &mut elem, &mut player);
        elem.future_data = true;
        elem.buffered = true;
        run(MediaEvent::Waiting, &mut elem, &mut player);
        run(MediaEvent::Play, &mut elem, &mut player);
        assert!(player.playing, "the first play() left the player stopped");
        let second = run(MediaEvent::RedundantPlay, &mut elem, &mut player);
        assert_eq!(second.command, PlayerCommand::None, "{second:?}");
        assert!(
            player.playing,
            "the redundant play() stopped a running player"
        );

        // The same four events from the decision at the parent commit.
        eprintln!("pre-first-fix decision:");
        let mut old_elem = elem;
        old_elem.paused = true;
        let mut old_player = ModelPlayer::default();
        let (next, state) = old_elem.after(MediaEvent::Load, &old_player);
        old_player.apply(decide_pre_first_fix(MediaEvent::Load, &state).command);
        old_elem = next;
        old_elem.future_data = true;
        old_elem.buffered = true;
        let (next, state) = old_elem.after(MediaEvent::Waiting, &old_player);
        old_player.apply(decide_pre_first_fix(MediaEvent::Waiting, &state).command);
        old_elem = next;
        let (next, state) = old_elem.after(MediaEvent::Play, &old_player);
        old_player.apply(decide_pre_first_fix(MediaEvent::Play, &state).command);
        old_elem = next;
        let (_next, state) = old_elem.after(MediaEvent::RedundantPlay, &old_player);
        let frozen = decide_pre_first_fix(MediaEvent::RedundantPlay, &state);
        eprintln!(
            "  {}\n  -> pre-first-fix ends with cmd={} on an element the page just played",
            step(MediaEvent::RedundantPlay, &state, frozen),
            frozen.command.trace_name()
        );
        assert_eq!(frozen.command, PlayerCommand::Pause);
    }
}
