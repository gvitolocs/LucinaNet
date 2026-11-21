//! The eight properties, each on a fixture that passes and on one with a violation planted in it, and then on real traces.
//!
//! `fixtures/healthy-all.log` is the passing fixture: every property is judged there and none of them breaks. Each file in
//! `fixtures/planted/` is that same log with one fact changed or one line taken away, named after the id it has to produce,
//! so a property going blind shows up as the wrong id rather than as a passing test. The four real excerpts came out of
//! actual `--render` runs; see `fixtures/README.md` for where each was cut from and what it is worth.

use media_trace_check::{check, parse, Config, Report, IDS};

fn healthy() -> &'static str {
    include_str!("fixtures/healthy-all.log")
}

fn planted(name: &str) -> &'static str {
    match name {
        "element-contract-1" => include_str!("fixtures/planted/element-contract-1.log"),
        "element-contract-2" => include_str!("fixtures/planted/element-contract-2.log"),
        "element-contract-3" => include_str!("fixtures/planted/element-contract-3.log"),
        "element-contract-4" => include_str!("fixtures/planted/element-contract-4.log"),
        "MEDIA-STR-held" => include_str!("fixtures/planted/MEDIA-STR-held.log"),
        "MEDIA-STR-uncleared" => include_str!("fixtures/planted/MEDIA-STR-uncleared.log"),
        "MEDIA-STR-RING" => include_str!("fixtures/planted/MEDIA-STR-RING.log"),
        "clock-contract-backward" => include_str!("fixtures/planted/clock-contract-backward.log"),
        "clock-contract-stall" => include_str!("fixtures/planted/clock-contract-stall.log"),
        "MEDIA-CUR" => include_str!("fixtures/planted/MEDIA-CUR.log"),
        "yt-f4-freeze" => include_str!("fixtures/yt-f4-freeze.log"),
        "fix-play2-rec1-healthy" => include_str!("fixtures/fix-play2-rec1-healthy.log"),
        "nav05-120-rec1-resume" => include_str!("fixtures/nav05-120-rec1-resume.log"),
        "yt2-rec0-new-player" => include_str!("fixtures/yt2-rec0-new-player.log"),
        "yt-rec0-navigation" => include_str!("fixtures/yt-rec0-navigation.log"),
        "prefix-play2-M-frozen" => include_str!("fixtures/prefix-play2-M-frozen.log"),
        other => panic!("no fixture named {other}"),
    }
}

/// What the checker had to say about a fixture, with the default windows.
fn said(text: &str) -> Report {
    check(text, &Config::default())
}

fn first(text: &str) -> (&'static str, usize, f64) {
    let r = said(text);
    let v = r.violations.first().unwrap_or_else(|| panic!("expected a violation, got {}", r.render(&parse(text))));
    (v.id, v.line, v.t.unwrap_or(f64::NAN))
}

/// A fixture where nothing breaks: no violation, and no property that got to say nothing either.
#[test]
fn healthy_log_passes_every_property_and_observes_all_of_them() {
    let r = said(healthy());
    assert!(r.violations.is_empty(), "violations: {:?}", r.violations);
    assert!(r.not_observed.is_empty(), "not observed: {:?}", r.not_observed);
    assert!(r.complete());
    assert_eq!(r.checked.to_vec(), IDS.to_vec(), "every property should have been judged");
    assert!(r.render(&parse(healthy())).starts_with("ok: no violation (17 lines, properties checked: element-contract-1"));
}

/// The planted fixtures, each named after the id it must produce, must produce it as the FIRST violation.
#[test]
fn each_planted_violation_is_named_first() {
    for (fixture, id, line, t) in [
        ("element-contract-1", "element-contract-1", 1, 0.100),
        ("element-contract-2", "element-contract-2", 1, 0.100),
        ("element-contract-3", "element-contract-3", 1, 0.100),
        ("element-contract-4", "element-contract-4", 1, 0.100),
        ("MEDIA-STR-held", "MEDIA-STR", 6, 0.105),
        ("MEDIA-STR-uncleared", "MEDIA-STR", 1, 0.100),
        ("MEDIA-STR-RING", "MEDIA-STR-RING", 11, 2.100),
        ("clock-contract-backward", "clock-contract", 12, 2.400),
        ("clock-contract-stall", "clock-contract", 13, 5.400),
        ("MEDIA-CUR", "MEDIA-CUR", 8, 0.900),
    ] {
        let text = planted(fixture);
        assert_eq!(first(text), (id, line, t), "fixture {fixture} should break {id} at line {line}, t+{t}");
    }
}

/// The report has to lead a reader back into the log: the line, its timestamp, a reason, five lines before and after.
#[test]
fn a_violation_shows_its_lines_and_the_reason() {
    let text = planted("MEDIA-STR-held");
    let out = said(text).render(&parse(text));
    let want =
        "MEDIA-STR: violated at t+0.105, line 6: the transition says the stream plays with the latch still set: \
                held=144048";
    assert!(out.contains(want), "output was:\n{out}");
    assert!(out.contains("  5 lines before and after:"), "output was:\n{out}");
    assert!(
        out.contains("=> [t+   0.105] lucinanet-media: mixer stream 0 playing false -> true held=Some(144048)"),
        "output was:\n{out}"
    );
    // Three lines before the offender and one after it are still inside the excerpt of five.
    assert!(out.contains("   [t+   0.102] lucinanet-media: mixer stream 0 set_playing(true)"), "output was:\n{out}");
    assert!(out.contains("   [t+   0.300] lucinanet-media: show pts=300000"), "output was:\n{out}");
    assert!(!out.contains("[t+   2.400]"), "line 12 is nine lines away and should not be printed:\n{out}");
}

/// The window is a knob, not a hard-coded fact: what stalls in 2 s is only slow if you allow 2 s.
#[test]
fn the_clock_window_is_configurable() {
    let text = planted("clock-contract-stall");
    let loose = Config { clock_window: 5.0, ..Config::default() };
    let r = check(text, &loose);
    assert!(r.violations.is_empty(), "3 s standing still is inside a 5 s window: {:?}", r.violations);
    assert!(r.complete(), "{:?}", r.not_observed);

    let quick = Config { clock_window: 0.25, ..Config::default() };
    let r = check(healthy(), &quick);
    let v = r
        .violations
        .iter()
        .find(|v| v.id == "clock-contract")
        .expect("0.6 s of standing clock is a stall in a 0.25 s window");
    assert_eq!((v.line, v.t), (8, Some(0.9)));
}

/// A short play window makes the healthy log's own `event=play` look like a freeze; a long one lets the planted late reply go.
#[test]
fn the_play_window_is_configurable() {
    let text = planted("element-contract-4");
    let loose = Config { play_window: 5.0, ..Config::default() };
    assert!(
        check(text, &loose).violations.iter().all(|v| v.id != "element-contract-4"),
        "2 s to reach the player is fine in a 5 s window"
    );
    let quick = Config { play_window: 0.0005, ..Config::default() };
    assert!(
        check(healthy(), &quick).violations.iter().any(|v| v.id == "element-contract-4"),
        "1 ms is not an answer in half a millisecond"
    );
}

/// Lines the checker does not read are ignored, not guessed at — but they stay in the log, so the numbers still lead back to
/// the file and the excerpt around a violation still shows them.
#[test]
fn lines_that_are_not_trace_events_are_only_neighbours() {
    let note = "0.16s  WARN lucinanet: this line is not a media trace event";
    let mut text = String::new();
    for (i, l) in planted("MEDIA-STR-held").lines().enumerate() {
        text.push_str(l);
        text.push('\n');
        if i == 3 {
            text.push_str(note);
            text.push('\n');
        }
    }
    let log = parse(&text);
    assert_eq!(log.len(), 18, "the note is a line of the log too");
    assert!(log[4].event.is_none(), "and it is not an event: {:?}", log[4]);
    assert_eq!(log[4].t, None, "a note the runner did not stamp has no time");

    let r = said(&text);
    assert_eq!(
        (r.violations[0].id, r.violations[0].line, r.violations[0].t),
        ("MEDIA-STR", 7, Some(0.105)),
        "{:?}",
        r.violations
    );
    assert!(r.render(&log).contains(note), "the neighbourhood should still print it:\n{}", r.render(&log));
}

// ------------------------------------------------------------------------------------------- the real traces

/// frozen-play F4 on YouTube: the page pressed play, the mixer kept its latch, and the position stopped answering. The first
/// thing the checker names has to be the clock standing still, and the element properties have to say they were not seen.
#[test]
fn the_real_youtube_freeze_is_caught_by_the_clock() {
    let text = planted("yt-f4-freeze");
    let r = said(text);
    let v = r.violations.first().expect("the freeze has to be named");
    assert_eq!(v.id, "clock-contract");
    // The window expires 2 s after the last thing that asked for a running clock: the page paused at t+9.115, resumed at
    // t+9.556 and was paused again 2 ms later, and the first line that proves the clock stood the whole window is the room
    // wait at t+11.578.
    assert_eq!(v.t, Some(11.578), "the first thing that is provably wrong is at t+11.578: {v:?}");
    assert!(v.reason.contains("from t+9.565"), "the reason has to say where the standing started: {}", v.reason);
    assert!(v.reason.contains("30.001 s"), "the reason should say what was buffered: {}", v.reason);
    for id in ["element-contract-1", "element-contract-2", "element-contract-3", "element-contract-4", "MEDIA-STR-RING", "MEDIA-CUR"] {
        assert!(
            r.not_observed.iter().any(|(n, _)| *n == id),
            "{id} cannot be seen in today's trace and must not be called a pass"
        );
    }
    assert!(r.checked.contains(&"MEDIA-STR"), "the latch is traced here and it held its ground: {:?}", r.checked);
    assert!(!r.complete());
}

/// The same page with the fixed crate, cut from a run that played: clean, with the same properties still untraced.
#[test]
fn the_real_healthy_excerpt_is_clean() {
    for name in ["fix-play2-rec1-healthy", "nav05-120-rec1-resume"] {
        let text = planted(name);
        let r = said(text);
        assert!(r.violations.is_empty(), "{name} should be clean, found {:?}", r.violations);
        assert!(r.checked.contains(&"MEDIA-STR"), "{name} should judge the latch");
        assert!(r.checked.contains(&"clock-contract"), "{name} should judge the clock");
        assert!(r.not_observed.iter().any(|(n, _)| n == &"element-contract-1"), "{name}: element lines are not traced yet");
    }
}

/// A page navigation in the middle of a run: the old element's clock stops at 7348250 µs and the new player starts at 0.
/// Those are two clocks. The acceptance trace said so and the checker called the second one a clock that ran backwards.
#[test]
fn a_navigation_is_a_new_clock_not_a_step_backwards() {
    let text = planted("yt2-rec0-new-player");
    // The fixture has to keep both ends of the pair, or this test proves nothing.
    assert!(text.contains("now=Some(7348250)"), "the old player's last value has to stay in the cut");
    assert!(text.contains("show pts=0 now=Some(0)"), "the new player's first value has to stay in the cut");
    assert!(text.contains("lucinanet-media: opened: no"), "and the line that says a new player opened has to stay too");

    let r = said(text);
    assert!(r.violations.is_empty(), "the navigation is not a violation: {:?}", r.violations);
    assert!(r.checked.contains(&"clock-contract"), "the clock was still judged over both players: {:?}", r.checked);
    assert!(r.checked.contains(&"MEDIA-STR"), "and so was the latch");
    for id in ["element-contract-1", "element-contract-2", "element-contract-3", "element-contract-4", "MEDIA-STR-RING", "MEDIA-CUR"] {
        assert!(r.not_observed.iter().any(|(n, _)| *n == id), "{id} is still not traced: {:?}", r.not_observed);
    }
}

/// The same navigation one property later: the old document's element closed believing the player plays, and the checker
/// answered it with the new document's first `player_playing=0` — consent-navigation. The element's belief ends where a new player or
/// document begins, so this cut has to come back clean, and clean without being blind: take the boundary lines out and the
/// same two element reports are the violation again.
#[test]
fn a_navigation_ends_the_element_the_state_was_about() {
    let text = planted("yt-rec0-navigation");
    // The fixture has to keep both ends, or this test proves nothing: the last belief of the old document, the first report
    // of the new one, and the engine line that says they are two documents.
    assert!(
        text.contains("potentially_playing=1 player_playing=1 cmd=none"),
        "the old element's belief has to stay"
    );
    assert!(
        text.contains("media elem event=play paused=0 ended=0 seeking=0 potentially_playing=0 player_playing=0"),
        "so does the new document's first element line"
    );
    assert!(text.contains("phase 655617.191074 load started"), "and the engine line between them");

    let r = said(text);
    assert!(r.violations.is_empty(), "the navigation is not a violation: {:?}", r.violations);
    assert!(r.checked.contains(&"element-contract-1"), "the element was still judged: {:?}", r.checked);

    // Take the boundary out and the two documents are read as one element again: the reset is what makes the first half of
    // this test a pass, not a property that stopped looking.
    let kept: Vec<&str> = text.lines().filter(|l| !l.starts_with("phase ")).collect();
    let r = said(&kept.join("\n"));
    let Some(v) = r.violations.first() else { panic!("the same two reports without the boundary break element-contract-1") };
    assert_eq!(v.id, "element-contract-1", "{:?}", r.violations);
}

/// A run with no media trace at all judges nothing and says so eight times over; that is not a pass, and `--strict` knows it.
#[test]
fn a_log_without_media_lines_observes_nothing() {
    let text = planted("prefix-play2-M-frozen");
    let r = said(text);
    assert!(r.violations.is_empty());
    assert!(r.checked.is_empty(), "nothing should be called a pass: {:?}", r.checked);
    assert_eq!(r.not_observed.len(), IDS.len(), "every property should say why it could not judge");
    assert!(!r.complete());
    let out = r.render(&parse(text));
    assert!(out.starts_with("ok: no violation ("), "output was:\n{out}");
    assert!(out.contains("not observed: clock-contract:"), "output was:\n{out}");
}
