# Element ↔ player contracts

The contracts `media_command.rs::decide` keeps, in the shape the media crates use: one requirement
sentence per id, one property, the test that exercises it, the assumptions it holds under.
Behaviour is
taken from the HTML Standard (cited by section anchor), not from what another browser does.

`decide` is the element-to-player decision, split out of `HTMLMediaElement::update_media_state`
(`htmlmediaelement.rs:508`) so the contracts can be checked over every state there is rather than over
whatever sequence of algorithm runs happens to reach the moment. The element builds a snapshot
(`media_state_snapshot`, `htmlmediaelement.rs:543`), `decide` answers with a command and a `Reason`,
and `update_media_state` carries the command out and emits the trace line. Nothing else in the element
tells the player to run or to stop.

The rule, in one line: **the player is commanded by the element's stops — `paused`, ended playback,
stopped due to errors, a block for content or interaction — and want of data is not a stop.** An
engine running under a block is left running; an engine stopped under a block the page still wants
played is started. That is finding 1 (a pause sent into a block silences the only signal that can end
it) and finding 2 (the specification's `#blocked-media-element` has no seeking term, so a seek out of
data is the same state as an underrun) at once.

## The inputs

| field | where the snapshot reads it | spec |
| --- | --- | --- |
| `paused` | `self.paused` | `#dom-media-paused` |
| `ended` | `ended_playback(LoopCondition::Included)` | `#ended-playback` |
| `seeking` | `self.seeking` | `#dom-media-seeking` |
| `potentially_playing` | `is_potentially_playing()` (`htmlmediaelement.rs:2004`) | `#potentially-playing` |
| `ready_state` | `ReadyLevel::from(self.ready_state)` | `#ready-states` |
| `player_playing` | `!player.paused()` | — the player's command mirror |
| `buffered_at_position` | the position lying inside a `player.buffered()` range | `#dom-media-buffered` |
| `error` | `self.error.get().is_some()` | `#stopped-due-to-errors` |

Four derived predicates, each one a specification clause and not a fourth of a mixture
(finding 5):

* `blocked_for_want_of_data()` = `!ready_state.has_future_data()` — the data term of
  `#blocked-media-element`, and the same term `is_potentially_playing()` reads. The `buffered` ranges
  are *not* in it: they belong to the `waiting` event and to element-contract-4's "data buffered", and a
  high ready state with a hole at the position is potentially playing.
* `waits_for_data()` = `blocked_for_want_of_data() && (seeking || !buffered_at_position)` — the
  `waiting` event's own condition (`#event-media-waiting`), which the element does not use for any
  command.
* `wants_playback()` = `!paused && !ended && !error` — the three conditions of `#potentially-playing`
  that are stops of the element's own, inverted. This is what the command answers.
* `blocked_by_content_or_interaction()` = `wants_playback() && !blocked_for_want_of_data() &&
  !potentially_playing` — `#blocked-media-element`'s other two conditions, which the snapshot has no
  field for because the element folds them into `potentially_playing`. Both predicates are constant
  `false` in this fork (`htmlmediaelement.rs:2023`, `:2030`), so the case is unreachable today; it is
  written down so that the answer is not wrong the day someone fills them in.

The event is the algorithm step that ran, not the cause the decision settles on:

| event | emitted by | |
| --- | --- | --- |
| `play` | `internal_play_steps`, `paused` true on entry (`:1029`, entry capture `:947`) | `#internal-play-steps` |
| `play-redundant` | the same call where `paused` was already false (`:1029`) | `#internal-play-steps` step 4 |
| `pause` | `internal_pause_steps` (`:1082`) | `#internal-pause-steps` |
| `seek-start` | seek step 4 (`:2437`) | `#dom-media-seek` |
| `seek-end` | seek step 14 (`:2468`) | `#dom-media-seek` |
| `waiting` | `change_ready_state` (`:1249`, `:1286`) and the stall in `playback_state_changed` (`:3333`) | `#ready-states` |
| `ended` | `end_of_playback_in_backwards_direction` (`:2800`) | `#reaches-the-end` |
| `load` | step 6 of `media_data_processing_fatal_steps` (`:2327`) — the fixed vocabulary has no `error` event, and the fatal steps are steps of the load algorithm; the load algorithm itself stops the player in `reset_media_player` (`:2644`) and never reaches the decision | `#media-data-processing-steps-list` |

The forwards end of playback reaches the decision through its `change_ready_state` call, so its line
is stamped `waiting`; the `ended=1` field is what answers the pause, and no contract reads the event
name as a cause.

## The output

`PlayerCommand::{None, Play, Pause}` plus a `Reason`. Nine names, each one cause:
`play-found-player-running`, `potentially-playing-and-player-stopped`,
`blocked-for-want-of-data-and-playback-was-ordered`, `paused-by-the-element-or-the-page`,
`ended-playback`, `playback-stopped-due-to-error`,
`paused-for-user-interaction-or-in-band-content`, `blocked-for-want-of-data`,
`player-already-stopped`, `player-already-answers-the-element`.

## The trace line

One line per decision, to stderr, when `LUCINANET_MEDIA_TRACE` is set (`trace`, `media_command.rs`);
the switch is read once into a `OnceLock`, like the media crates read theirs, and no URL is on the
line:

    lucinanet-media: media elem event=<play|pause|seek-start|seek-end|waiting|ended|load|play-redundant> \
    paused=<0|1> ended=<0|1> seeking=<0|1> potentially_playing=<0|1> player_playing=<0|1> cmd=<none|play|pause>

`tools/media-trace-check` (LucinaNet) parses it after `strip_prefix("media elem ")` and judges
element-contract-1…04 from it. `the_trace_line_carries_the_fixed_fields_in_the_fixed_order` pins the exact
text; `every_event_names_itself_the_way_the_checker_reads_it` pins the vocabulary.

## The tests, and how findings 3 and 4 are tested at all

33 tests, all in `media_command.rs`'s `mod tests`. The decision-side properties run over the
enumeration (7 flags × 5 ready levels = 640 snapshots, 2560 of them states an element can actually be
in) and over seeded random event sequences (xorshift64\*, `Rng::next_u64`, no new dependency) driving
a model player.

Findings 3 and 4 are about `htmlmediaelement.rs`, and the setup used could not compile it: `servo-script`
depends on `servo-script-bindings`, whose build script needs a Python that setup lacks (see
`REPORT.md`). The rules those lines implement are therefore transcribed in the test module —
`Stage::{Before, RoundOne, After}` with `buffering`, `playing`, `waiting_fires`, `ready_state_events`,
`backwards_end`, `fatal_steps`, each naming the working-tree line it stands for — and every contract
that lives in the element side is asserted twice, against the old transcription and the new. Those
transcriptions are the evidence; the build gate has to confirm them against the code, and
`REPORT-round2.md` says which claim rests on that. A transcription cannot fail on a deleted call
site, so the last assumption of 07 and of 08 states which half of those two contracts only the
run-time trace carries.

---

## element-contract-1

The player is playing if and only if the element is potentially playing, except where the element is
blocked for want of data and the page still wants playback.

*Property.* For every event and every state an element can be in, after the command the mirror answers
what the element is — `player_playing == potentially_playing` once the command is applied to a model
player — unless `wants_playback() && blocked_for_want_of_data()`.

*Test.* `media_elem_01_holds_for_every_state` (the enumeration, with the one window of assumption 3
skipped), `media_elem_01_starts_a_stopped_player_under_a_potentially_playing_element`,
`media_elem_01_holds_over_random_event_sequences`.

*Spec.* `#potentially-playing` — "A media element is said to be potentially playing when its `paused`
attribute is false, the element has not ended playback, playback has not stopped due to errors, and the
element is not a blocked media element." `#playing-the-media-resource` — "When a media element is
potentially playing, its audio data played must be synchronized with the current playback position…"
and "When a media element is not potentially playing, audio must not play for the element."

*Assumptions.*
1. `player_playing` is `!player.paused()`, which mirrors the last command the element gave
   (`components/media/backends/gstreamer/player.rs:226-239`) and not frame advancement. It is the only
   thing the element can read, so 01 is a contract about commands.
2. `potentially_playing` is the element's own `is_potentially_playing()`, not a recomputation, so the
   snapshot cannot disagree with the rest of the element. The enumeration keeps only the snapshots the
   element's arithmetic can produce (`element_can_be`); a state with `paused` true and
   `potentially_playing` true is not a state the property is asked to keep.
3. The one window is beyond any command's reach from here, and it cuts both ways. A blocked element
   whose engine is running prints `potentially_playing=0 … cmd=none`; a blocked element whose engine
   the page asked to play prints `potentially_playing=0 … cmd=play`. Pausing into agreement is what
   froze frozen-play and what finding 1's seek did to this branch's own round one, because a paused engine
   sends no `PlaybackState::Playing` — the fork's only signal that raises the ready state above
   `HAVE_CURRENT_DATA` (`playback_state_changed`'s Playing arm). 05 owns the window; the checker's
   reading of it is at the end of this file.
4. A block for user interaction or in-band content would hold `potentially_playing` false over a state
   that looks clear to the other six fields, and 01 then holds (the command is a pause). Both
   predicates are constant `false` in this fork, so the tests cannot reach that case.

## element-contract-2

A `play()` that finds the player already running, on an element that is in none of the stops, issues
no command.

*Property.* `event=play` with `player_playing=1` and `wants_playback()=1` ⇒ `cmd=none`; and no play,
redundant or not, ever commands a stop over an element that is potentially playing.

*Test.* `media_elem_02_a_play_on_a_running_element_issues_no_command`,
`media_elem_02_no_play_pauses_a_potentially_playing_element`,
`media_elem_02_a_play_on_an_ended_element_stops_the_player_the_state_had_stopped`,
`media_elem_02_holds_over_random_event_sequences`. It **fails against the pre-fix decision**:
`media_elem_02_fails_for_the_pre_fix_decision` and
`the_random_sequences_break_media_elem_02_before_the_fix` — the first state that breaks it is
`event=play` over `potentially_playing=1, player_playing=1` returning `Pause`, which is first-fix's bug
in one line.

*Spec.* `#internal-play-steps` step 3 runs "If the media element's `paused` attribute is true"; step 4
covers the rest — "Otherwise … take pending play promises and … resolve them with the result. *The
media element is already playing.*" A player that is already running has nothing to be told.

*Assumptions.*
1. The precondition is part of the id (finding 6). Round 1 wrote 02 as the checker's bare shape,
   `event=play && player_playing=1 ⇒ cmd=none`, and the function does not keep that without it: a play
   over an element that has *ended* playback, or whose playback stopped due to an error, answers with
   the pause those states already asked for. That pause is element-contract-3's subject, not the play
   steps', and the play steps themselves contain no pause at all.
2. The `tools/media-trace-check` 02 predicate has no such precondition, so a
   `event=play … player_playing=1 cmd=pause` line would be flagged by the tool even though 03 owns it.
   The shape needs the play steps to reach the decision with the mirror still running, which the end of
   this file records as a divergence rather than a workaround.
3. `play-redundant` (the same call with `paused` already false) is stamped distinctly so the log can
   show the second `play()` of the first-fix sequence; the tool's 02 reads `event=play` only, and the
   redundant line answers `cmd=none` as well.

## element-contract-3

A pause command is issued only for a pause the element or the user ordered: `paused`, ended playback,
playback stopped due to an error, or a block for user interaction or in-band content.

*Property.* `cmd=pause` ⇒ one of those four in the snapshot. Want of data is not on the list, in a
seek or out of one; every other state answers `cmd=none`.

*Test.* `media_elem_03_every_pause_names_a_cause` (the reachable enumeration),
`media_elem_03_want_of_data_issues_no_command`,
`media_elem_03_a_seek_that_left_nothing_to_decode_pauses_nothing`,
`media_elem_03_holds_over_random_event_sequences`, with `ordered_pause()` as the list stated over the
snapshot. **Pre-fix it fails**: `media_elem_03_fails_for_the_pre_fix_decision`,
`the_random_sequences_break_media_elem_03_before_the_fix`, and the same test's red half against this
branch's own round one, which paused all three seek events over a seek with nothing decoded.

*Spec.* the four causes: `#internal-pause-steps` ("Set the `paused` attribute to true" — the page or
the element asked); `#ended-playback` with `#reaches-the-end`; `#stopped-due-to-errors` with
"audio must not play" from `#playing-the-media-resource`; and `#blocked-media-element`'s two content
conditions. `#dom-media-seek` is deliberately *not* a cause: step 12 says "Wait until the user agent
has established whether or not the media data for the new playback position is available, and, if it
is, until it has decoded enough data to play back that position", which is a wait for data, not a stop
for the engine, and step 11's note points at the `waiting` event — the same ready-state step as a plain
stall — for what happens instead (finding 2).

*Assumptions.*
1. Round 1's third cause ("a seek the specification pauses for") is withdrawn. `#blocked-media-element`
   has no seeking term, and the pause that went with it is the freeze finding 1 names.
2. A block for user interaction or in-band content is a cause, prints
   `paused=0 ended=0 seeking=0 cmd=pause`, and is unreachable today because both predicates are
   constant `false`; `element_can_be` documents what that does to the enumeration, and the tool's
   exemption list is the other half of the same gap.
3. `ordered_pause()` in the test module is the property as a predicate. It is deliberately written from
   the specification's four conditions rather than from `stop_cause`, so a new cause has to be argued
   for there before it is allowed here.

## element-contract-4

After every event sequence that ends with a play and data buffered at the position, the player is
playing.

*Property.* Run any sequence of events over the model player; where the last event is a play, the
element is in none of the stops and the data is at the position, the model mirror reads playing
afterwards — including the state where the element is blocked in its ready state, because a play the
page ordered outranks the block.

*Test.* `media_elem_04_holds_for_every_state_ending_with_a_play`,
`play_with_data_leaves_the_player_running` and `media_elem_04_holds_over_random_event_sequences`;
`finding_1_a_seek_that_runs_out_of_data_starts_the_engine_again` is the same property as a round trip.
**Pre-fix it fails**: `the_random_sequences_break_media_elem_04_before_the_fix` prints the trail of the
sequence it breaks in, ending `play pp=1 … buffered=1 mirror=1 -> pause`;
`the_play2_trace_before_and_after` prints the first-fix pair, and
`the_nav05_play2_sequence_keeps_the_player_running` keeps that as a named regression. The WPT is
`tests/wpt/mozilla/tests/mozilla/play-while-playing.html`.

*Spec.* `#internal-play-steps` step 3: setting `paused` false, firing `play`, and — with `readyState`
at HAVE_FUTURE_DATA or above — "notify about playing for the element", whose steps fire `playing` and
resolve the pending play promises. A player left stopped answers none of that.

*Assumptions.*
1. "Data buffered" is `buffered_at_position` together with the element not being in a stop. The trace
   line has no buffer field, so the tool's 04 reads `event=play potentially_playing=1` and then looks
   for a later line saying the player plays, within its window; an unstamped witness line is skipped,
   never flagged.
2. The sequence tests drive `decide` and a model player, not a real pipeline: 04 says the element
   issues the command. That the command reaches the mixer is the crates' own contracts (MEDIA-STR,
   clock-contract).
3. A play the specification itself refuses, or one that the internal play steps begin by seeking
   (ended playback, forwards), is still a play the element issues; the precondition "data at the
   position" is what excludes the wait after that seek from this property, and 03's answer there is
   `cmd=none` rather than a stop.

## element-contract-5

A stall — `PlaybackState::Buffering` — or a seek that leaves too little data drops the ready state
below HAVE_FUTURE_DATA, fires `waiting`, and leaves the element not potentially playing; and neither
the drop nor its answer may touch an element whose playback has ended or has stopped due to errors.

*Property.* Over every state blocked for want of data with the element in no stop: no pause, and no
engine left stopped under a page that wants playback. The two ready-state arms write nothing at all
when `ended_playback()` or an error holds.

*Test.* Decision side: `media_elem_05_a_block_of_data_is_neither_a_pause_nor_a_stop` (and its red half,
which counts the 12 want-of-data states round 1 left with a stopped engine, against 0 now). Element
side, as transcriptions with red runs: `media_elem_05_the_stall_dropped_nothing_before_the_fix` — the
fork's Buffering arm wrote nothing and queued nothing, the fix drops to HAVE_CURRENT_DATA and queues
`timeupdate` and `waiting` — and
`finding_4_the_ready_state_arms_leave_an_ended_or_errored_element_alone`, which shows the first round's
unguarded raise taking an ended element to HAVE_ENOUGH_DATA and queueing
`["canplay", "playing", "canplaythrough"]` at it. In the element: the Buffering arm
(`htmlmediaelement.rs:3312-3336`), the Playing arm (`:3291-3311`) and `can_hold_future_data`
(`:3278-3280`). The raise *ends* the block and is not the restart: the restart is the `cmd=play` that
answered the block while the mirror stood still (`BlockedButAskedToPlay`), and the decision the raise
itself ends in prints `none`, because on that path the engine never stopped (the recheck's finding 1).

*Spec.* `#blocked-media-element` — "A media element is a blocked media element if its `readyState`
attribute is in the HAVE_NOTHING state, the HAVE_METADATA state, or the HAVE_CURRENT_DATA state, or if
the element has paused for user interaction or paused for in-band content"; `#ready-states`, the
HAVE_CURRENT_DATA and HAVE_FUTURE_DATA definitions and the ready-state change steps — "If the previous
ready state was HAVE_FUTURE_DATA or more, and the new ready state is HAVE_CURRENT_DATA or less … fire
an event named `timeupdate` … and … `waiting`" — and the same section's "The user agent cannot be in
this state if playback has ended, as the current playback position can never advance in this case";
`#stopped-due-to-errors`; `#event-media-waiting`.

*Assumptions.*
1. `change_ready_state` returns early when the ready state does not change (`:1129`), which is why the
   Buffering arm runs the decision itself where it has nothing to lower.
2. The engine's `PlaybackState::Buffering` is the only stall signal the player sends, and the Playing
   arm is the only thing in the fork that lifts the ready state out of HAVE_CURRENT_DATA — hence the
   rule that a want-of-data block must never pause the engine, and hence finding 1.
3. The guard is `!ended && !error`, not `!paused`: a paused element that runs out of data still has to
   say so. The sibling raise in the Paused arm (HAVE_METADATA → HAVE_ENOUGH_DATA, upstream's own) is
   *not* guarded, so an ended element that is reported `Paused` can still be raised; that is out of
   scope for this change and is left as a known residual (finding 4's third case).
4. Nothing else in the fork lifts `readyState` out of HAVE_CURRENT_DATA, so the drop and the raise are
   one round trip and must be read together ("a fix follows its cause").

## element-contract-6

The `waiting` event fires only where playback has not stopped due to errors, the element has not ended
playback, and it is not paused for user interaction or in-band content.

*Property.* An errored state is answered as the error's stop whatever the data says, and a plain stall
as nobody's stop; and the ready-state clause that fires the event is satisfiable exactly in the stall.

*Test.* `media_elem_06_an_error_is_a_stop_and_a_stall_is_not` (decision side) and
`media_elem_06_the_waiting_clause_was_inverted_before_the_fix` (the clause, transcribed over all 16
rows). The red run is harsher than round 1's summary: `was_potentially_playing` is
`is_potentially_playing()`, whose third condition is that the element has *no* error, so the fork's
`!self.error.get().is_none()` did not fire `waiting` at an error — it made the whole substep
unsatisfiable, and the `waiting` and `timeupdate` of that ready-state branch were unreachable code.

*Spec.* `#ready-states`, the HAVE_FUTURE_DATA-or-more → HAVE_CURRENT_DATA-or-less substep: "If the
media element was potentially playing before its `readyState` attribute changed to a value lower than
HAVE_FUTURE_DATA, and the element has not ended playback, and playback has not stopped due to errors,
paused for user interaction, or paused for in-band content, the user agent must queue a media element
task … to fire an event named `timeupdate` at the element, and queue a media element task … to fire an
event named `waiting`". `#stopped-due-to-errors` defines the clause the fix restores.

*Assumptions.*
1. `was_potentially_playing` is read *before* the new ready state is stored (`:1121`), as "was
   potentially playing before … changed" requires.
2. 06 is about the event, which the pure function cannot fire; what the function owns is the
   distinction the clause draws — an error is not a want of data. The event half is the transcription,
   with its red run.
3. `is_paused_for_user_interaction()` and `is_paused_for_in_band_content()` are constant `false` here,
   so their two clauses are carried and untested.

## element-contract-7

Reaching the earliest possible position with the direction of playback backwards pauses the player.

*Property.* `decide(ended, {ended=1, paused=0, player_playing=1})` is `pause` with
`Reason::EndedPlayback`, and the step that reaches it reaches the decision.

*Test.* `media_elem_07_the_backwards_end_of_playback_pauses_the_player` (decision side) and
`media_elem_07_the_backwards_end_reached_no_decision_before_the_fix` (red: the old body queued its
`timeupdate`, refreshed the keep-alive and stopped nothing, so the model engine is still running over an
element that has ended). In the element: `end_of_playback_in_backwards_direction`
(`htmlmediaelement.rs:2785-2801`).

*Spec.* `#reaches-the-end` — "When the current playback position reaches the earliest possible position
of the media resource when the direction of playback is backwards, then the user agent must only queue
a media element task given the media element to fire an event named `timeupdate` at the element", and
`#ended-playback`'s second bullet is that position with that direction; `#playing-the-media-resource` —
"When a media element is not potentially playing, audio must not play for the element."

*Assumptions.*
1. The "must only" bounds the *events* that algorithm queues, not the player: the state change itself
   (ended, therefore not potentially playing) is what requires the silence, so the decision has to run.
2. `paused` stays false here — only the forwards arm sets it (`:2763`) — so `ended=1` is the field that
   justifies the pause to element-contract-3 and to the checker.
3. `playback_end` returns early while `seeking` (`:2806`), so a backwards seek to the earliest position
   is the seek's own path, and the fixed 03 answers nothing there for want of data.
4. **These tests cover the decision, not the step that reaches it.**
   `media_elem_07_the_backwards_end_reached_no_decision_before_the_fix` compares `decide` against a
   transcription of the old body (`media_command.rs:1244-1254`); deleting the
   `update_media_state(MediaEvent::Ended)` call at `htmlmediaelement.rs:2800` fails no test in this file,
   and the harness cannot build `servo-script` to drive a real element (see "The tests, and how findings
   3 and 4 are tested at all"). The call site is checked at run time, by the element's own trace line and
   `tools/media-trace-check`: with the call present, the step prints
   `event=ended paused=0 ended=1 seeking=0 potentially_playing=0 player_playing=1 cmd=pause` and the
   backend then prints its `player pause … on … mirror(…)` line; with the call gone neither line
   appears and the pipeline runs on over an element that has ended, so the next element line prints
   `potentially_playing=0` under a mirror that still reports playing — which is what `elem_state`
   (`tools/media-trace-check/src/lib.rs:517-546`) reports as a element-contract-1 break. That check is a read
   over pairs of lines, so a log that prints nothing after the end reports 01 as *not observed* rather
   than broken: the witness needs an acceptance trace that reaches the earliest position backwards and
   then does something else (seeks, or the page plays), not one that stops at the end.

## element-contract-8

A fatal media error pauses the player.

*Property.* With `error=1` and `player_playing=1`, `decide` is `pause` with `Reason::StoppedDueToError`
at every ready state and whether or not data is buffered; and the step that learns of the error reaches
the decision.

*Test.* `media_elem_08_a_fatal_media_error_pauses_the_player` (decision side) and
`media_elem_08_the_fatal_steps_reached_no_decision_before_the_fix` (red: the old step 6 called
`update_keep_alive` and the model engine keeps playing a resource that has just died). In the element:
step 6 of `media_data_processing_fatal_steps` (`htmlmediaelement.rs:2295-2327`).

*Spec.* `#media-data-processing-steps-list`, "If the media data is corrupted" ("Fatal errors in
decoding the media data that occur after the user agent has established whether the current media
resource is usable … must cause the user agent to execute the following steps: … Set the `error`
attribute to … MEDIA_ERR_DECODE"), and the dedicated media source failure steps;
`#potentially-playing`, whose third condition is "playback has not stopped due to errors";
`#playing-the-media-resource`, "audio must not play"; and the note on `play()` — "This means that the
dedicated media source failure steps have run. Playback is not possible until the media element load
algorithm clears the `error` attribute."

*Assumptions.*
1. The fixed event vocabulary has no `error`; the line is stamped `load`, because the fatal steps are
   steps of the load algorithm. The consequence for the checker is at the end of this file.
2. The `load()` method's own teardown does not come this way: it goes through `reset_media_player`
   (`htmlmediaelement.rs:2651`), which stops the player and drops it, and a fresh element reports
   `player_playing=0` afterwards.
3. `error` here is the element's `error` attribute being non-null, the same read
   `is_potentially_playing()` makes, so snapshot and element cannot disagree — and, as 06 notes, that
   is also why the fork's inverted `waiting` clause could never fire.
4. As in 07's assumption 4, what these tests hold is the decision and not the call: deleting
   `update_media_state(MediaEvent::Load)` (`htmlmediaelement.rs:2327`) fails no test in this file, since
   `media_elem_08_the_fatal_steps_reached_no_decision_before_the_fix` compares `decide` with a
   transcription of the old step 6 (`media_command.rs:1256-1266`). Here the run-time witness is the only
   one there is, step 6 being the only thing that stops the pipeline: steps 2 to 4 change none of
   `paused`, the ready state or the playback position, and `load()`'s own teardown goes the other way
   (assumption 2). So a trace that dies mid-playback owes
   `event=load paused=0 ended=0 seeking=0 potentially_playing=0 player_playing=1 cmd=pause` with the
   backend's `player pause …` line under it; without the call both are absent, the audio of a dead
   resource plays on, and the next element line prints `potentially_playing=0` over a mirror that still
   reports playing — the element-contract-1 break `elem_state` (`tools/media-trace-check/src/lib.rs:517-546`)
   names, with the limit 07 states: a trace that stops at the `error` event leaves the property not
   observed rather than broken.

---

## What the checker cannot see from these lines

Three consequences of the trace format the task fixes, reported rather than worked around.
`tools/media-trace-check` lives in LucinaNet and is not this branch's file to change.

1. **element-contract-1 and the want-of-data window.** A stalled, non-seeking element prints
   `event=waiting potentially_playing=0 player_playing=1 cmd=none`; a stalled element whose engine the
   page asked to play prints `event=play potentially_playing=0 player_playing=0 cmd=play`.
   `elem_state` (`tools/media-trace-check/src/lib.rs:517-546`) compares `potentially_playing` with the
   next player report, so both are called breaks. Nothing the element can command fixes either one
   while the mirror is a command mirror: pausing into agreement is the frozen-play freeze and finding 1's
   freeze, and it also silences the signal that would end the block. Either the property exempts
   `wants_playback && !potentially_playing` — which the line cannot express, since it carries no
   data-state field — or the line gains one field, `blocked=<0|1>`, which `MediaState` already computes
   at every decision.
2. **element-contract-3 and the error pause.** The element-contract-8 pause prints
   `event=load paused=0 ended=0 seeking=0 cmd=pause`, and `elem_pause_justified` (`:590-620`) exempts
   `paused|ended|seeking` only, so an error stop is flagged. The cause is a `MediaState` field with no
   place on the line. An `error=<0|1>` field is the fix, and the same exemption list has to grow when
   `is_paused_for_user_interaction()` and `is_paused_for_in_band_content()` stop returning `false`
   (assumption 2 of 03). The seek term in that list is now dead weight: the element pauses inside a seek
   in no state (finding 2).
3. **element-contract-2's precondition.** `elem_play_command` (`:564-586`) flags any
   `event=play player_playing=1 cmd!=none`, while 02's own sentence (assumption 1) scopes it to an
   element in none of the stops. A play that lands on an ended or errored element with the engine still
   running is 03's pause and the tool's break; reaching the play steps in that state needs the
   forwards-end interval of finding 4, so it is a shape to watch in a real log rather than one the
   tests can produce.

## Running the tests

    cargo test -p servo-script --lib media_command

In the setup used that command cannot run at all: building `servo-script` requires
`servo-script-bindings`, whose build script probes `uv run python`, `python3` and `python`, and the
setup has none of them. The module was therefore compiled and run
standalone, with `#[path]` including this file into a throwaway crate — 33 tests, all passing, printed
evidence in `<work-dir>/tmp/evidence-round2.txt`.
