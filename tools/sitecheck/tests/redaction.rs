//! The panic message belongs to the session log and to the crash page, so nothing sitecheck writes carries it:
//! a crash is recorded and printed as `thread=`, `location=`, `shim=` and `reason=` only. The line shapes here are
//! the ones the engine and the runner really print.

use serde_json::Value;
use sitecheck::{crash_record, parse_line, print_json, print_table, CrashReason, Site};

/// A site from the given crash lines, on a page that otherwise loaded and scrolled well.
fn site_of(lines: &[&str]) -> Site {
    let mut site = Site::default();
    parse_line(&mut site, "# url https://crash.example.com/");
    parse_line(
        &mut site,
        "render: complete true in 4000 ms, rss 160000 KiB",
    );
    parse_line(&mut site, "render: down to frame 40000 us");
    parse_line(&mut site, "render: down to frame 45000 us");
    for line in lines {
        parse_line(&mut site, line);
    }
    site
}

#[test]
fn a_panic_line_keeps_thread_location_and_reason_only() {
    // The engine prints the location on this line and the message on the next one.
    let site = site_of(&[
        "thread 'Script#3' (1) panicked at csp.rs:225:22:",
        "Must always be able to parse document origin: RelativeUrlWithoutBase",
    ]);
    assert!(site.crashed);
    assert_eq!(
        site.crash_reason.as_deref(),
        Some("thread=Script#3 location=csp.rs:225:22 reason=Panic")
    );
    assert_eq!(
        site.scroll_steps,
        vec![40, 45],
        "counting of everything else is untouched"
    );
}

#[test]
fn the_message_never_reaches_the_table_or_the_json() {
    let site = site_of(&[
        "thread 'Script#3' (1) panicked at csp.rs:225:22:",
        "Must always be able to parse document origin: RelativeUrlWithoutBase https://leak.example.net/a:b",
    ]);
    let printed = [
        print_table(&[site.clone()], 15000, 50),
        print_json(&[site.clone()]),
    ];
    for text in printed {
        assert!(!text.contains("Must always"), "{text}");
        assert!(!text.contains("RelativeUrlWithoutBase"), "{text}");
        assert!(!text.contains("leak.example.net"), "{text}");
    }
    // The record itself holds no URL at all, so a message cannot hide inside its location.
    assert!(!site.crash_reason.unwrap().contains("://"));
}

#[test]
fn a_message_with_a_url_and_a_colon_glued_onto_the_line_is_cut() {
    // What a harness that joined the panic's two lines with `tr '\n' ' '` gives a tool to read.
    let glued = "thread 'main' (42) panicked at csp.rs:225:22: refused https://leak.example.net/a:b for that origin";
    let record = crash_record(glued);
    assert_eq!(record, "thread=main location=csp.rs:225:22 reason=Panic");
    assert!(
        !record.contains("://") && !record.contains("refused"),
        "{record}"
    );
    assert_eq!(
        site_of(&[glued]).crash_reason.as_deref(),
        Some("thread=main location=csp.rs:225:22 reason=Panic")
    );
}

#[test]
fn a_crash_or_an_abort_without_a_location_carries_only_the_reason() {
    assert_eq!(
        crash_record("lucinanet: signal 11 at pc 0x12345678, lr 0x23456789, sp 0x34567890"),
        "reason=Crash"
    );
    assert_eq!(crash_record("Aborted"), "reason=Crash");
    assert_eq!(crash_record("CRASHED: signal 11"), "reason=Crash");
    assert_eq!(
        site_of(&["Aborted"]).crash_reason.as_deref(),
        Some("reason=Crash")
    );
    // A line that does name a shim and a location names them, and still nothing else.
    assert_eq!(
        crash_record("CRASHED: OutOfMemory at raster/core/src/pool.rs:304 shim=timeout-shim"),
        "location=raster/core/src/pool.rs:304 shim=timeout-shim reason=OutOfMemory"
    );
}

#[test]
fn reasons_are_the_three_the_rule_names() {
    assert_eq!(
        CrashReason::of("thread 'x' panicked at a.rs:1:1:"),
        CrashReason::Panic
    );
    assert_eq!(
        CrashReason::of(
            "thread 'x' panicked at alloc.rs:1:1: memory allocation of 123456789 bytes failed"
        ),
        CrashReason::OutOfMemory
    );
    assert_eq!(
        CrashReason::of("CRASHED: OutOfMemory"),
        CrashReason::OutOfMemory
    );
    assert_eq!(CrashReason::of("Aborted"), CrashReason::Crash);
}

#[test]
fn json_keeps_the_field_and_holds_the_record() {
    let site = site_of(&[
        "thread 'Script#3' (1) panicked at csp.rs:225:22:",
        "Must always be able to parse document origin: RelativeUrlWithoutBase",
    ]);
    let value: Value = serde_json::from_str(&print_json(&[site])).unwrap();
    assert_eq!(value["crashed"], true);
    assert_eq!(
        value["crash_reason"],
        "thread=Script#3 location=csp.rs:225:22 reason=Panic"
    );
}
