//! Tests for the context's mocked clock (see `Context::current_unix_time_nanos`).

use crate::runtime::{
    Context, ContextBuilder, Handle, Realm, context::MOCKED_UNIX_TIME_STEP_NANOS, embed,
};

/// The unix time that the mocked clocks of these tests start at, in nanoseconds.
const BASE_NANOS: u128 = 1_000_000_000_000_000_000;

/// Nanoseconds in a millisecond, the unit `Date.now()` reports in.
const NANOS_PER_MILLI: u128 = 1_000_000;

/// Evaluate a script that completes with a number.
fn eval_number(cx: Context, realm: Handle<Realm>, source: &str) -> f64 {
    match embed::evaluate_script(cx, realm, source, "clock_test.js", 1) {
        Ok(value) => value.as_number(),
        Err(error) => panic!("script failed: {error:?}"),
    }
}

/// Read the mocked clock of a fresh context `reads` times, nanoseconds and milliseconds in turn.
/// Both accessors read the same clock, so each call is one step further than the last.
fn clock_reads(base: u128, reads: u128) -> Vec<(u128, u128)> {
    let cx = ContextBuilder::new()
        .mock_unix_time_nanos(base)
        .build()
        .unwrap();

    let times = (0..reads)
        .map(|_| (cx.current_unix_time_nanos(), cx.current_unix_time_millis()))
        .collect();

    cx.drop();

    times
}

#[test]
fn mocked_clock_advances_on_every_read() {
    let times = clock_reads(BASE_NANOS, 5);

    for (index, (nanos, millis)) in times.iter().enumerate() {
        let ticks = index as u128 * 2;
        assert_eq!(*nanos, BASE_NANOS + ticks * MOCKED_UNIX_TIME_STEP_NANOS);
        assert_eq!(
            *millis,
            (BASE_NANOS + (ticks + 1) * MOCKED_UNIX_TIME_STEP_NANOS) / NANOS_PER_MILLI
        );
    }

    for pair in times.windows(2) {
        assert!(pair[1].0 > pair[0].0, "the nanosecond clock did not advance");
        assert!(pair[1].1 > pair[0].1, "the millisecond clock did not advance");
    }
}

#[test]
fn mocked_clock_is_the_same_in_every_context() {
    assert_eq!(clock_reads(BASE_NANOS, 8), clock_reads(BASE_NANOS, 8));
    assert_eq!(clock_reads(BASE_NANOS + 1, 8), clock_reads(BASE_NANOS + 1, 8));
}

/// A test that waits on `Date.now()` used to wait forever, the mocked clock being frozen. It now
/// finishes after a fixed number of reads, whatever the machine it runs on.
#[test]
fn js_busy_wait_on_the_clock_terminates() {
    let cx = ContextBuilder::new()
        .mock_unix_time_nanos(BASE_NANOS)
        .build()
        .unwrap();
    let realm = cx.initial_realm();

    // Every read of `Date.now()` is a millisecond later than the last.
    assert_eq!(eval_number(cx, realm, "Date.now()"), BASE_NANOS as f64 / NANOS_PER_MILLI as f64);
    assert_eq!(
        eval_number(cx, realm, "Date.now()"),
        BASE_NANOS as f64 / NANOS_PER_MILLI as f64 + 1.0
    );

    // The shape of the test262 busy-wait that used to hang the suite (see
    // language/expressions/dynamic-import/await-import-evaluation_FIXTURE.js): it breaks once the
    // clock has moved past 100 ms, so exactly 101 reads of `Date.now()` happen and no more.
    let elapsed = eval_number(
        cx,
        realm,
        "var startTime = Date.now(); var endTime = 0;
         while (true) { endTime = Date.now() - startTime; if (endTime > 100) break; } endTime",
    );
    assert_eq!(elapsed, 101.0);

    // A counted wait is the same shape: the condition reads the clock 5 times, 4 of them inside.
    let spins = eval_number(
        cx,
        realm,
        "var start = Date.now(); var spins = 0; while (Date.now() - start < 5) { spins++; } spins",
    );
    assert_eq!(spins, 4.0);

    cx.drop();
}
