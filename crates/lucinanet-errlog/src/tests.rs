/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::path::{Path, PathBuf};

use crate::log::log_paths;
use crate::*;

/// One fixed time, so any test that prints a date prints the same one: 2024-11-12 08:26:40 UTC.
const T: u64 = 1_731_400_000_000;

/// A directory for one test's files, emptied first. Tests run in parallel, so each one names itself.
///
/// A test that litters a home directory is not tested, it is installed: writes go to
/// `LUCINANET_ERRLOG_TEST_TMP` when that is set, and otherwise under the workspace's own build
/// directory, which is already somewhere git does not look.
fn sandbox(tag: &str) -> PathBuf {
    let root = std::env::var_os("LUCINANET_ERRLOG_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/lucinanet-errlog-tests"));
    let dir = root.join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test sandbox");
    dir
}

/// A page that is really an address with a session id in it, and a script that is really a URL with a
/// version and a fragment on it: everything a test hands in is something the log has to shrink to a
/// host and a name.
fn script(message: &str) -> Event {
    Event::script(
        T,
        1,
        2,
        "https://news.example/front/story.html?session=9f33&from=share",
        "https://cdn.example/assets/js/app.min.js?v=17#inline",
        42,
        message,
    )
}

fn lines(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join(FILE_NAME))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

// --- host_of -----------------------------------------------------------------------------

#[test]
fn a_host_is_the_authority_without_its_user_its_port_or_its_path() {
    assert_eq!(host_of("https://user:pass@Example.com:8443/a/b?q=1#f"), "example.com");
    assert_eq!(host_of("http://sub.news.example."), "sub.news.example");
    assert_eq!(host_of("https://example.com./x"), "example.com");
    assert_eq!(host_of("https://192.168.1.10:8080/x"), "192.168.1.10");
    assert_eq!(host_of("https://[::1]:8080/x"), "[::1]");
    assert_eq!(host_of("https://localhost:3000/webpack.js"), "localhost");
    assert_eq!(host_of("https://example.com:80"), "example.com");
    assert_eq!(host_of("HTTPS://EXAMPLE.COM/A"), "example.com");
    // Two `@`s: the host is what follows the last one.
    assert_eq!(host_of("https://a@b@host.example/x"), "host.example");
}

#[test]
fn a_non_ascii_host_is_lower_cased_and_left_as_it_was_written() {
    assert_eq!(host_of("https://bücher.example/x"), "bücher.example");
    assert_eq!(host_of("https://MÜNCHEN.de"), "münchen.de");
    // The punycode spelling is a different string and groups apart; that is the cost of keeping no
    // IDNA table, and it is smaller than the cost of guessing at an address.
    assert_eq!(host_of("https://xn--bcher-kva.example/x"), "xn--bcher-kva.example");
}

#[test]
fn a_host_is_empty_for_everything_that_is_not_a_web_host() {
    for url in [
        "data:text/html,<script>alert(1)</script>",
        "blob:https://example.com/uuid-1",
        "file:///home/user/x.js",
        "about:blank",
        "javascript:void(0)",
        "ws://example.com/socket",
        "example.com/path",
        "//cdn.example/a.js",
        "not a url at all",
        "",
        "https://",
        "https:example.com",
        "https://ex ample/x",
        "https://.example",
        "https://example..com",
        "https://-bad.example",
        "https://bad-.example",
        "https://[::1",
    ] {
        assert_eq!(host_of(url), "", "{url:?} should have no host");
    }
}

#[test]
fn a_host_that_is_not_a_host_is_nothing() {
    // 253 characters is the longest a real label sequence can be; past that it is text in the place
    // of a host, and text is what this log does not keep.
    let four_labels = [String::from("a").repeat(59), "b".repeat(59), "c".repeat(59), "d".repeat(59)].join(".");
    let at_the_limit = format!("https://{four_labels}.example.com/x");
    assert_eq!(host_of(&at_the_limit).chars().count(), 251);
    let over = format!("https://eeeeeeeeee.{four_labels}.example.com/x");
    assert_eq!(host_of(&over), "", "a host of 262 characters is not a host");
    // A single label longer than 63 is not a host name either, however short the whole thing is.
    assert_eq!(host_of("https://f.example/x"), "f.example");
    assert_eq!(host_of(&format!("https://{}.example/x", "g".repeat(64))), "");
}

// --- file_of -----------------------------------------------------------------------------

#[test]
fn a_file_is_the_last_segment_of_the_path() {
    assert_eq!(file_of("https://cdn.example/assets/js/app.min.js?v=17#inline"), "app.min.js");
    assert_eq!(file_of("https://example.com/a/b/../c.js?x=1"), "c.js");
    assert_eq!(file_of("file:///home/user/projects/app.js"), "app.js");
    assert_eq!(file_of("https://user:pass@example.com:8443/a/b?q=1"), "b");
    // What a stack report gives: a path with no scheme.
    assert_eq!(file_of("/assets/js/app.min.js"), "app.min.js");
    assert_eq!(file_of("./app.min.js"), "app.min.js");
    assert_eq!(file_of("https://cdn.example/a/b/c%20d.js"), "c%20d.js");
}

#[test]
fn a_file_is_empty_where_there_is_no_name() {
    assert_eq!(file_of("https://example.com/pages/"), "");
    assert_eq!(file_of("https://example.com/"), "");
    assert_eq!(file_of("https://example.com"), "");
    // The body of these is the page itself; its last "segment" is someone's text.
    assert_eq!(file_of("data:text/html,<script>alert(1)</script>"), "");
    assert_eq!(file_of("blob:https://example.com/uuid-1"), "");
    assert_eq!(file_of("javascript:void(0)"), "");
    assert_eq!(file_of("about:blank"), "");
    assert_eq!(file_of(""), "");
    // A bare word names a file as often as it names a function, so it names neither.
    assert_eq!(file_of("app.min.js"), "");
    assert_eq!(file_of("HTMLDivElement.appendChild"), "");
}

#[test]
fn a_name_that_runs_on_is_cut_and_not_split() {
    let long = "d".repeat(300);
    let name = file_of(&format!("https://cdn.example/build/{long}.js"));
    assert_eq!(name.chars().count(), FILE_MAX_CHARS);
    assert!(name.starts_with('d'));
    let unicode = file_of(&format!("https://cdn.example/{}", "é".repeat(300)));
    assert_eq!(unicode.chars().count(), FILE_MAX_CHARS);
    assert!(unicode.chars().all(|c| c == 'é'));
}

// --- redact_detail -----------------------------------------------------------------------

#[test]
fn a_url_in_a_message_loses_its_query_and_its_fragment() {
    let message = "TypeError: x is not a function (https://a.example/p.js?token=abc#x user@mail.com)";
    assert_eq!(redact_detail(message), "TypeError: x is not a function (https://a.example/p.js <email>)");
    assert_eq!(redact_detail("failed to load //x.example/a/b?sig=z#f"), "failed to load //x.example/a/b");
    assert_eq!(redact_detail("https://x.example/a#b?c"), "https://x.example/a");
    assert_eq!(redact_detail("read <https://x.example/a?q=1>."), "read <https://x.example/a>.");
}

#[test]
fn a_message_loses_its_credentials() {
    assert_eq!(
        redact_detail("cannot reach https://bob:hunter2@vault.example/invoices/9"),
        "cannot reach https://vault.example/invoices/9"
    );
    assert_eq!(redact_detail("cannot reach bob:hunter2@vault.example/invoices"), "cannot reach vault.example/invoices");
}

#[test]
fn a_message_loses_an_email() {
    assert_eq!(redact_detail("sent to a.person@corp.example, nobody else"), "sent to <email>, nobody else");
    assert_eq!(redact_detail("owner <a.person@corp.example>."), "owner <email>.");
    // A URL's user info is not an e-mail: its host is what the log keeps.
    assert_eq!(redact_detail("https://a.person@corp.example/mail"), "https://corp.example/mail");
    // Not an e-mail either: a handle with no domain to leak.
    assert_eq!(redact_detail("someone@localhost"), "someone@localhost");
}

#[test]
fn a_message_keeps_its_names_and_loses_its_identifiers() {
    assert_eq!(redact_detail("no session for 00112233445566778899aabbcc"), "no session for <id>");
    assert_eq!(
        redact_detail("key AAACVuHlqAAAAAABJcUR1aXhjNTdnQUFBQUFBQUFBdA== accepted"),
        "key <id> accepted"
    );
    assert_eq!(redact_detail("deadbeefcafebabefeedface is a git object"), "<id> is a git object");
    // The names in a TypeError are long too, and they are the whole reason to keep the message.
    for name in ["requestAnimationFrameCallbackHandler", "HTMLDivElement", "onBeforeUnloadHandler"] {
        assert_eq!(redact_detail(name), name, "{name} is a name, not an id");
    }
    assert_eq!(
        redact_detail("cannot read 'counterIncrementValue' of undefined"),
        "cannot read 'counterIncrementValue' of undefined"
    );
}

#[test]
fn redaction_is_normal_form_and_idempotent() {
    assert_eq!(redact_detail("boom\r\n\ta\tis\tbroken\u{1}  badly "), "boom a is broken badly");
    for message in [
        "TypeError: x is not a function (https://a.example/p.js?token=abc#x user@mail.com)",
        "key AAACVuHlqAAAAAABJcUR1aXhjNTdnQUFBQUFBQUFBdA== rejected for a.person@corp.example",
        "cannot reach https://bob:hunter2@vault.example/invoices/9",
    ] {
        let once = redact_detail(message);
        assert_eq!(redact_detail(&once), once, "{message} does not redact to a fixed point");
        assert!(!once.contains('\n') && !once.contains('\t'));
    }
}

#[test]
fn a_detail_is_cut_to_length_after_it_is_redacted() {
    let event = script(&format!("RangeError: {}", "x ".repeat(400)));
    assert_eq!(event.detail.chars().count(), DETAIL_MAX_CHARS);
    assert!(event.detail.starts_with("RangeError: x"));
}

// --- the events --------------------------------------------------------------------------

#[test]
fn the_kind_names_are_the_vocabulary_of_the_file() {
    for kind in [
        Kind::ScriptException,
        Kind::MissingApi,
        Kind::ResourceFailed,
        Kind::EnginePanic,
        Kind::PageNeverLoaded,
        Kind::Truncated,
    ] {
        assert_eq!(Kind::parse(kind.as_str()), Some(kind));
        assert_eq!(kind.to_string(), kind.as_str());
        assert!(!kind.label().is_empty());
    }
    assert_eq!(Kind::parse("Watermelon"), None);
    assert_eq!(Kind::parse(""), None);
}

#[test]
fn a_script_event_keeps_its_page_and_script_as_host_and_name() {
    let event = script("TypeError: x is not a function");
    assert_eq!(event.kind, Kind::ScriptException);
    assert_eq!(event.page_host, "news.example");
    assert_eq!(event.host, "cdn.example");
    assert_eq!(event.file, "app.min.js");
    assert_eq!((event.line, event.count, event.tab), (42, 1, 2));
    for piece in ["session=9f33", "from=share", "?v=17", "#inline", "story.html", "/assets/js/"] {
        assert!(!event.to_line().contains(piece), "{piece} leaked into {}", event.to_line());
    }
}

#[test]
fn each_kind_fills_the_fields_it_is_allowed() {
    let api = Event::missing_api(T, 1, 1, "https://shop.example/cart", "requestIdleCallback", 12);
    assert_eq!((api.detail.as_str(), api.count, api.host.as_str(), api.file.as_str()), ("requestIdleCallback", 12, "", ""));
    assert_eq!(api.page_host, "shop.example");

    let charge = Event::resource_failed(T, 1, 1, "https://shop.example/cart", "https://pay.example.com:443/charge?order=9#top", "404");
    assert_eq!((charge.detail.as_str(), charge.host.as_str(), charge.file.as_str()), ("404", "pay.example.com", ""));
    assert_eq!(charge.page_host, "shop.example");
    assert_eq!(Event::resource_failed(T, 1, 1, "https://shop.example/cart", "https://api.shop.example/v1/list", "dns").detail, "dns");

    let panic = Event::engine_panic(
        T,
        1,
        1,
        "https://shop.example/cart",
        "/home/user/Projects/LucinaNet/crates/lucinanet-servo/src/engine.rs",
        "index out of bounds: the length is 4 but the index is 9",
    );
    assert_eq!(panic.file, "src/engine.rs");
    assert!(!panic.to_line().contains("/home/user"), "{}", panic.to_line());
    assert_eq!(
        Event::engine_panic(T, 1, 1, "about:blank", "C:\\build\\agent\\work\\raster\\core.cc", "oops").file,
        "raster/core.cc"
    );
    assert_eq!(Event::engine_panic(T, 1, 1, "", "engine.rs", "oops").file, "engine.rs");
    assert_eq!(Event::engine_panic(T, 1, 1, "", "/////", "oops").file, "");

    let slow = Event::page_never_loaded(T, 1, 1, "https://video.example/watch?id=7", 30);
    assert_eq!((slow.detail.as_str(), slow.page_host.as_str(), slow.host.as_str()), ("30s", "video.example", ""));
}

#[test]
fn a_line_is_the_event_and_back() {
    let event = script("TypeError: x is not a function");
    let line = event.to_line();
    assert_eq!(line.lines().count(), 1);
    assert!(line.starts_with('{'));
    assert_eq!(Event::from_line(&line), Some(event));
    assert!(line.contains("\"kind\":\"ScriptException\""), "{line}");
    assert!(line.contains("\"page_host\":\"news.example\""), "{line}");
}

#[test]
fn from_line_skips_what_is_not_an_event() {
    for line in [
        "",
        "TypeError: x is not a function",
        "[1,2]",
        "{}",
        "null",
        "13",
        "{\"t_ms\":1731400000000,\"session\":1,\"kind\":\"ScriptExc", // half a line, as a crash leaves it
        "{\"t_ms\":1,\"kind\":\"Watermelon\"}",                        // a kind this build does not know
        "{\"kind\":\"ScriptException\"}",                             // no time: nothing to put it on
        "{\"t_ms\":\"soon\",\"kind\":\"ScriptException\"}",
    ] {
        assert_eq!(Event::from_line(line), None, "{line:?} should not read as an event");
    }
}

#[test]
fn from_line_loses_only_the_field_that_is_missing() {
    let event = Event::from_line("{\"t_ms\":5,\"kind\":\"MissingApi\",\"detail\":\"Intl\"}").expect("read");
    assert_eq!((event.session, event.tab, event.line, event.count), (0, 0, 0, 1));
    assert_eq!(event.page_host, "");
    assert_eq!(event.detail, "Intl");
    // A number where a string belongs costs that field, not the line.
    let odd = Event::from_line("{\"t_ms\":5,\"kind\":\"ResourceFailed\",\"detail\":404,\"count\":0}").expect("read");
    assert_eq!(odd.detail, "");
    assert_eq!(odd.count, 1);
}

// --- the writer --------------------------------------------------------------------------

#[test]
fn a_memory_log_writes_the_lines_it_would_have() {
    let mut log = Log::memory(7);
    log.record(script("one"));
    log.record(script("two"));
    let events = log.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].detail, "one");
    assert_eq!(events[1].session, 7, "the log's session number wins");
    assert!(!log.is_truncated());
}

#[test]
fn a_repeat_is_written_again_at_each_power_of_two() {
    let mut log = Log::memory(1);
    for _ in 0..20 {
        log.record(script("TypeError: x is not a function"));
    }
    let counts: Vec<u32> = log.events().iter().map(|event| event.count).collect();
    assert_eq!(counts, [1, 2, 4, 8, 16]);

    let mut log = Log::memory(1);
    for _ in 0..200 {
        log.record(script("same"));
    }
    let counts: Vec<u32> = log.events().iter().map(|event| event.count).collect();
    assert_eq!(counts, [1, 2, 4, 8, 16, 32, 64, 128]);
    assert_eq!(log.events().len(), 8, "a loop of two hundred is eight lines");
}

#[test]
fn a_tab_is_not_part_of_what_an_error_is() {
    let mut log = Log::memory(1);
    log.record(script("loop"));
    let mut other_tab = script("loop");
    other_tab.tab = 9;
    other_tab.t_ms = T + 1000;
    log.record(other_tab);
    assert_eq!(log.events().len(), 2, "the second tab is the same error, counted");
    assert_eq!(log.events()[1].count, 2);
    let mut third_tab = script("loop");
    third_tab.tab = 4;
    log.record(third_tab);
    assert_eq!(log.events().len(), 2, "three is not a power of two");
}

#[test]
fn every_part_of_the_identity_makes_its_own_line() {
    let mut log = Log::memory(1);
    log.record(script("loop"));
    let mut deeper = script("loop");
    deeper.line = 43;
    log.record(deeper);
    let mut elsewhere = script("loop");
    elsewhere.host = "other.example".to_string();
    log.record(elsewhere);
    let mut different = script("loop");
    different.page_host = "elsewhere.example".to_string();
    log.record(different);
    log.record(Event::resource_failed(T, 1, 1, "https://news.example/front/story.html", "https://cdn.example/a.css", "404"));
    assert_eq!(log.events().len(), 5);
    assert_eq!(log.events()[4].kind, Kind::ResourceFailed);
}

#[test]
fn a_session_stops_at_its_cap_with_one_marker() {
    let mut log = Log::memory(3);
    for i in 0..(MAX_LINES + 100) {
        log.record(script(&format!("error number {i}")));
    }
    assert!(log.is_truncated(), "true from the first refusal, before the line is written");
    log.finish();
    let events = log.events();
    assert_eq!(events.len(), MAX_LINES as usize + 1, "500 lines and the marker");
    let marker = events.last().expect("marker");
    assert_eq!(marker.kind, Kind::Truncated);
    assert_eq!(marker.session, 3);
    assert_eq!(marker.count, 100, "the marker says what it left out");
    assert!(marker.detail.contains("500 lines"), "{}", marker.detail);
    // The marker carries the time of the last event it is talking about, not a zero.
    assert_eq!(marker.t_ms, events[MAX_LINES as usize - 1].t_ms);
    log.record(script("late"));
    log.finish();
    assert_eq!(log.events().len(), MAX_LINES as usize + 1, "nothing at all after the marker");
    assert_eq!(log.events().iter().filter(|event| event.kind == Kind::Truncated).count(), 1);
}

#[test]
fn a_file_log_appends_one_line_per_written_event() {
    let dir = sandbox("append");
    {
        let mut log = Log::open(&dir, 1).expect("open");
        log.record(script("first"));
        log.record(script("second"));
        for _ in 0..6 {
            log.record(script("third"));
        }
        assert!(log.events().is_empty(), "a file log keeps nothing in memory");
    }
    let written = lines(&dir);
    assert_eq!(written.len(), 5, "third was written at 1, 2 and 4: {written:?}");
    assert_eq!(Event::from_line(&written[0]).expect("line").detail, "first");
    assert_eq!(Event::from_line(written.last().expect("line")).expect("line").count, 4);
    assert!(dir.join(FILE_NAME).is_file());
    assert!(!dir.join("errors.1.jsonl").exists());
}

#[test]
fn opening_the_log_makes_its_directory_and_a_second_session_appends_to_it() {
    let dir = sandbox("nested").join("deep").join("profile");
    Log::open(&dir, 1).expect("a fresh profile").record(script("from session 1"));
    let mut second = Log::open(&dir, 2).expect("the same profile again");
    assert_eq!(second.session(), 2);
    second.record(script("from session 2"));
    drop(second);
    let written = lines(&dir);
    assert_eq!(written.len(), 2);
    assert!(written[0].contains("\"session\":1"), "{}", written[0]);
    assert!(written[1].contains("\"session\":2"), "{}", written[1]);
    assert_eq!(read_sessions(&dir, 5).len(), 2);
}

/// A host of three long labels and a real suffix: legal — every label under 64, the whole thing
/// under 253 — and as long as the machine-readable addresses a CDN hands out.
fn long_host(seed: char) -> String {
    [seed.to_string().repeat(60), seed.to_string().repeat(60), seed.to_string().repeat(60), "example".to_string(), "com".to_string()].join(".")
}

/// An event whose line is long: a detail near the full length, two long hosts and a full file name.
/// Lines this long are what makes the byte cap arrive inside one session, which is what a test of the
/// rollover needs.
fn long_event(session: u32, i: u32) -> Event {
    Event::script(
        T + u64::from(session) * 1_000 + u64::from(i),
        session,
        1,
        &format!("https://{}/page", long_host('a')),
        &format!("https://{}/build/{}.js", long_host('b'), "c".repeat(120)),
        7,
        &format!("{i} {}", "x".repeat(190)),
    )
}

/// One session's worth of distinct, long events: enough to pass [`MAX_BYTES`] but short of the line
/// cap, so the rollover is the only thing happening.
fn fill_session(dir: &Path, session: u32) -> u64 {
    let mut log = Log::open(dir, session).expect("open");
    let line_len = long_event(session, 0).to_line().len() as u64;
    for i in 0..(MAX_LINES - 1) {
        log.record(long_event(session, i));
    }
    line_len
}

#[test]
fn the_file_rolls_at_its_cap_and_one_session_straddles_it() {
    let dir = sandbox("rotation");
    let line_len = fill_session(&dir, 1);
    assert!(line_len * u64::from(MAX_LINES - 1) > MAX_BYTES, "these lines cannot roll in one session: {line_len} bytes");

    let live = std::fs::metadata(dir.join(FILE_NAME)).expect("live").len();
    assert!(live <= MAX_BYTES, "the live file is {live} bytes, over the cap");
    assert!(dir.join("errors.1.jsonl").is_file(), "the rollover never came");
    assert!(!dir.join("errors.2.jsonl").exists(), "one roll is one generation");

    // The session's own lines are split across the two files, and the reader puts them back.
    let sessions = read_sessions(&dir, 5);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session, 1);
    assert_eq!(sessions[0].events.len(), (MAX_LINES - 1) as usize);
    assert_eq!(sessions[0].corrupt, 0);
    assert!(!sessions[0].truncated(), "the byte cap came first, as it should");

    // Every file ends on a whole line: a rollover never leaves a fragment behind.
    for name in [FILE_NAME, "errors.1.jsonl"] {
        let text = std::fs::read_to_string(dir.join(name)).expect("file");
        assert!(text.ends_with('\n'), "{name} does not end on a line");
        assert!(text.lines().all(|line| Event::from_line(line).is_some()), "{name} has an unreadable line");
    }
}

#[test]
fn only_three_generations_are_kept_and_the_oldest_goes() {
    let dir = sandbox("generations");
    for session in 1..=10 {
        fill_session(&dir, session);
    }
    for generation in 1..=GENERATIONS {
        let path = dir.join(format!("errors.{generation}.jsonl"));
        assert!(path.is_file(), "generation {generation} is missing");
        let size = std::fs::metadata(&path).expect("size").len();
        assert!(size <= MAX_BYTES, "{path:?} is {size} bytes, over the cap");
    }
    assert!(!dir.join("errors.4.jsonl").exists(), "a fourth generation escaped the cap");
    assert!(!dir.join("errors.0.jsonl").exists());
    assert!(std::fs::metadata(dir.join(FILE_NAME)).expect("live").len() <= MAX_BYTES);

    // The file is a window, not an archive: ten sessions of these lines do not all fit.
    let sessions = read_sessions(&dir, 100);
    let kept: usize = sessions.iter().map(|session| session.events.len()).sum();
    assert!(kept < 10 * (MAX_LINES as usize - 1), "{kept} lines still readable: nothing was dropped");
    assert_eq!(read_sessions(&dir, 1)[0].session, 10, "the newest session is the newest line");
    assert!(sessions.iter().all(|session| session.session >= 1));
}

#[test]
fn a_log_with_no_writable_directory_counts_and_carries_on() {
    // A path that cannot hold a file: `open` says so, and the wiring's fallback is a memory log,
    // which is the same log without the file.
    let dir = sandbox("unwritable");
    std::fs::write(dir.join(FILE_NAME), "in the way").expect("plant");
    assert!(Log::open(&dir.join(FILE_NAME).join("profile"), 1).is_err());

    let mut log = Log::memory(1);
    for _ in 0..3 {
        log.record(script("still counted"));
    }
    assert_eq!(log.events().len(), 2);
}

// --- the reader --------------------------------------------------------------------------

const OLD: &str = "{\"t_ms\":1000,\"session\":1,\"tab\":1,\"page_host\":\"old.example\",\"kind\":\"ScriptException\",\"detail\":\"was here\",\"host\":\"\",\"file\":\"\",\"line\":0,\"count\":1}";
const LOOP: &str = "{\"t_ms\":2000,\"session\":2,\"tab\":1,\"page_host\":\"shop.example\",\"kind\":\"ScriptException\",\"detail\":\"loop\",\"host\":\"cdn.example\",\"file\":\"app.js\",\"line\":7,\"count\":%}";

fn loop_line(count: u32, t_ms: u64) -> String {
    LOOP.replace("%", &count.to_string()).replace("\"t_ms\":2000", &format!("\"t_ms\":{t_ms}"))
}

fn plant(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("plant");
}

#[test]
fn an_empty_or_missing_directory_has_no_sessions() {
    assert_eq!(read_sessions(&sandbox("no-such").join("deep"), 5), vec![]);
    let dir = sandbox("empty-dir");
    assert_eq!(read_sessions(&dir, 5), vec![]);
    plant(&dir, FILE_NAME, "\n\n   \n");
    assert_eq!(read_sessions(&dir, 5), vec![]);
}

#[test]
fn sessions_come_back_newest_first_and_capped_in_number() {
    let dir = sandbox("newest-first");
    plant(
        &dir,
        "errors.2.jsonl",
        &format!("{OLD}\n{}\n", loop_line(1, 900).replace("\"session\":2", "\"session\":4")),
    );
    plant(&dir, FILE_NAME, &format!("{OLD}\n{}\n{}\n", loop_line(1, 3000), loop_line(1, 5000).replace("\"session\":2", "\"session\":3")));

    let sessions = read_sessions(&dir, 5);
    assert_eq!(sessions.iter().map(|session| session.session).collect::<Vec<_>>(), [3, 2, 1, 4]);
    assert_eq!(sessions[2].events.len(), 2, "session 1's line is in both generations");
    assert_eq!(sessions[3].first_t_ms(), Some(900));
    assert_eq!(sessions[0].last_t_ms(), Some(5000));
    assert_eq!(read_sessions(&dir, 2).len(), 2);
    assert_eq!(read_sessions(&dir, 1)[0].session, 3);
    assert_eq!(read_sessions(&dir, 0).len(), 0);
}

#[test]
fn corrupt_lines_are_skipped_and_counted() {
    let dir = sandbox("corrupt");
    plant(
        &dir,
        FILE_NAME,
        &format!(
            "{OLD}\nnot json at all\n{}\n\n{}\n",
            "{\"t_ms\":2,\"session\":1,\"kind\":\"Watermelon\"}",
            loop_line(1, 3000).replace("\"session\":2", "\"session\":1")
        ),
    );
    let sessions = read_sessions(&dir, 5);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].events.len(), 2, "the two readable lines");
    assert_eq!(sessions[0].corrupt, 2, "the junk and the unknown kind");
    assert!(render_html(&sessions).contains("2 lines unreadable"));
}

#[test]
fn a_line_cut_off_by_a_crash_costs_only_itself() {
    let dir = sandbox("partial");
    plant(&dir, FILE_NAME, &format!("{OLD}\n{}", &loop_line(1, 2000)[..40]));
    let sessions = read_sessions(&dir, 5);
    assert_eq!(sessions.len(), 1, "the half line belongs to no session");
    assert_eq!(sessions[0].session, 1);
    assert_eq!(sessions[0].events.len(), 1);
    assert_eq!(sessions[0].corrupt, 1);
    assert!(render_html(&sessions).contains("1 line unreadable"));
}

#[test]
fn a_file_of_only_junk_still_says_so() {
    let dir = sandbox("all-corrupt");
    plant(&dir, FILE_NAME, "nonsense\nmore nonsense\n");
    let sessions = read_sessions(&dir, 5);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session, 0);
    assert!(sessions[0].events.is_empty());
    assert_eq!(sessions[0].corrupt, 2);
    assert!(render_html(&sessions).contains("no readable lines"));
}

#[test]
fn a_key_written_at_each_power_counts_once_not_summed() {
    // The four lines of one error are that it happened eight times, not fifteen.
    let dir = sandbox("collapse");
    plant(&dir, FILE_NAME, &format!("{}\n{}\n{}\n{}\n", loop_line(1, 1000), loop_line(2, 2000), loop_line(4, 3000), loop_line(8, 4000)));
    let sessions = read_sessions(&dir, 5);
    assert_eq!(sessions[0].events.len(), 4, "the file keeps every line it wrote");

    let groups = group_by_host(&sessions[0]);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].page_host, "shop.example");
    assert_eq!(groups[0].total, 8);
    assert_eq!(groups[0].distinct, 1);
    assert_eq!(groups[0].kinds, vec![(Kind::ScriptException, 8)]);
    let detail = &groups[0].details[0];
    assert_eq!((detail.count, detail.file.as_str(), detail.line, detail.host.as_str()), (8, "app.js", 7, "cdn.example"));
}

#[test]
fn a_marker_reaches_the_page_as_a_note() {
    let dir = sandbox("marker");
    let marker = Event {
        t_ms: 9000,
        session: 1,
        tab: 0,
        page_host: String::new(),
        kind: Kind::Truncated,
        detail: "cut off at 500 lines; 12 more events counted only".to_string(),
        host: String::new(),
        file: String::new(),
        line: 0,
        count: 12,
    };
    plant(&dir, FILE_NAME, &format!("{OLD}\n{}\n", marker.to_line()));
    let sessions = read_sessions(&dir, 5);
    assert!(sessions[0].truncated());
    let groups = group_by_host(&sessions[0]);
    assert_eq!(groups[0].page_host, "", "the marker has no host, and sorts by its count");
    assert_eq!(groups[0].total, 12);
    assert_eq!(groups[0].kinds, vec![(Kind::Truncated, 12)]);
    assert!(render_html(&sessions).contains("reached the limit"));
}

#[test]
fn hosts_are_ordered_by_how_often_they_broke() {
    let dir = sandbox("order");
    let mut body = String::new();
    for (host, times) in [("many.example", 30u32), ("busy.example", 40), ("one.example", 1)] {
        for i in 0..times {
            body.push_str(&Event::resource_failed(T, 1, 1, &format!("https://{host}/"), "https://cdn.example/a.js", &format!("404 {i}")).to_line());
            body.push('\n');
        }
    }
    // The quietest host broke 41 times, in two errors.
    for (count, t_ms, detail) in [(40u32, 5000u64, "loop"), (1, 5001, "other")] {
        body.push_str(
            &loop_line(count, t_ms)
                .replace("shop.example", "quiet.example")
                .replace("\"session\":2", "\"session\":1")
                .replace("\"detail\":\"loop\"", &format!("\"detail\":\"{detail}\"")),
        );
        body.push('\n');
    }
    // Two lines, one error, on a page with no host at all: the same total as `one.example`.
    for _ in 0..2 {
        body.push_str(&Event::page_never_loaded(T, 1, 1, "about:blank", 30).to_line());
        body.push('\n');
    }
    plant(&dir, FILE_NAME, &body);

    let sessions = read_sessions(&dir, 1);
    assert_eq!(sessions.len(), 1);
    let groups = group_by_host(&sessions[0]);
    let order: Vec<&str> = groups.iter().map(|group| group.page_host.as_str()).collect();
    assert_eq!(order, ["quiet.example", "busy.example", "many.example", "", "one.example"], "{groups:?}");
    assert_eq!(groups.iter().map(|group| group.total).collect::<Vec<_>>(), [41, 40, 30, 1, 1]);
    assert_eq!(groups[3].page_host, "", "the pages with no host are a group of their own");
    assert_eq!(groups[3].distinct, 1, "its two lines are one error");
    assert_eq!(groups[3].kinds, vec![(Kind::PageNeverLoaded, 1)]);
    assert_eq!(groups[2].distinct, 30);
    assert_eq!(groups[0].details.len(), 2);
}

#[test]
fn a_hosts_top_details_are_the_frequent_ones() {
    let dir = sandbox("top-details");
    let mut body = String::new();
    for i in 0..8 {
        body.push_str(&Event::script(T, 1, 1, "https://many.example/", "https://js.example/app.js", 1, &format!("failure {i}")).to_line());
        body.push('\n');
    }
    // One error that happened more than all the others put together, on the line the log writes for it.
    let mut frequent = Event::script(T, 1, 1, "https://many.example/", "https://js.example/app.js", 2, "the frequent one");
    frequent.count = 32;
    body.push_str(&frequent.to_line());
    body.push('\n');
    plant(&dir, FILE_NAME, &body);
    let groups = group_by_host(&read_sessions(&dir, 1)[0]);
    assert_eq!(groups[0].details.len(), TOP_DETAILS, "eight failures and one more, five named");
    assert_eq!(groups[0].total, 40);
    assert_eq!(groups[0].distinct, 9);
    assert_eq!(groups[0].details[0].detail, "the frequent one");
    assert_eq!(groups[0].details[0].count, 32);
    assert_eq!(groups[0].details[1].detail, "failure 0", "the rest are in name order when they count the same");
    assert!(groups[0].details.iter().all(|detail| detail.count <= 32));
}

// --- the page ----------------------------------------------------------------------------

#[test]
fn the_page_escapes_what_a_page_wrote() {
    let events = vec![Event::script(
        T,
        4,
        1,
        "https://news.example/story",
        "https://cdn.example/app.js",
        3,
        "<script>alert(1)</script> & \"quotes\" <b>tag</b> in https://a.example/p.js?token=abc",
    )];
    let sessions = vec![SessionErrors { session: 4, events, corrupt: 0 }];
    let html = render_html(&sessions);
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"), "{html}");
    assert!(html.contains("&amp; &quot;quotes&quot;"), "{html}");
    assert!(!html.to_lowercase().contains("<script>"), "a script tag reached about:errors");
    assert!(!html.contains("token=abc"), "the message's query reached the page");
    assert!(html.contains("news.example") && html.contains("app.js:3") && html.contains("cdn.example"));
    assert!(html.contains("Session 4"));
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.ends_with("</html>\n"));
    assert!(!html.contains("<script") && !html.contains("javascript:"), "the page carries a script");
}

#[test]
fn the_page_is_one_table_per_session_and_says_what_it_keeps() {
    let dir = sandbox("page");
    plant(
        &dir,
        FILE_NAME,
        &format!(
            "{}\n{}\n{}\n",
            Event::script(T, 1, 1, "https://a.example/", "https://js.a.example/app.js", 3, "a broke").to_line(),
            Event::resource_failed(T, 1, 1, "https://b.example/", "https://img.b.example/x.png", "404").to_line(),
            Event::missing_api(T + 1000, 1, 2, "https://b.example/", "Intl", 1).to_line(),
        ),
    );
    let sessions = read_sessions(&dir, 10);
    let html = render_html(&sessions);
    assert_eq!(html.matches("<table>").count(), 1, "one session, one table");
    assert_eq!(html.matches("<h2>Session").count(), 1);
    for name in ["a.example", "b.example", "js.a.example", "app.js"] {
        assert!(html.contains(name), "{name} missing from the page");
    }
    for label in ["script error", "failed request", "missing API"] {
        assert!(html.contains(label), "{label} missing from the page");
    }
    assert!(html.contains("<style>") && html.contains("errors.jsonl"));
    assert_eq!(render_html(&[]).matches("<table>").count(), 0);
    assert!(render_html(&[]).contains("Nothing has gone wrong yet"));
}

#[test]
fn a_text_rendering_says_the_same_thing() {
    let dir = sandbox("text");
    plant(
        &dir,
        FILE_NAME,
        &format!(
            "{}\n{}\n",
            Event::script(T, 1, 1, "https://a.example/", "https://js.a.example/app.js", 3, "a broke").to_line(),
            Event::engine_panic(T + 1, 1, 1, "https://a.example/", "crates/lucinanet-servo/src/engine.rs", "index oops").to_line(),
        ),
    );
    let sessions = read_sessions(&dir, 10);
    let text = render_text(&sessions);
    assert!(text.starts_with("LucinaNet - session errors"), "{text}");
    for expected in ["Session 1", "a.example", "src/engine.rs", "app.js:3", "js.a.example", "2024-11-12 08:26:40 UTC", "a broke"] {
        assert!(text.contains(expected), "{expected} missing from:\n{text}");
    }
    assert_eq!(render_text(&[]).lines().count(), 4);
    assert!(render_text(&[]).contains("Nothing has gone wrong yet"));
    // No HTML in the text, and no escaping to undo.
    assert!(!text.contains("&lt;") && !text.contains("<td>"));
}

#[test]
fn stamps_are_utc_without_a_locale_or_a_crate() {
    assert_eq!(stamp(0), "1970-01-01 00:00:00 UTC");
    assert_eq!(stamp(T), "2024-11-12 08:26:40 UTC");
    assert_eq!(stamp(951_782_400_000), "2000-02-29 00:00:00 UTC");
    assert_eq!(stamp(1_709_251_199_000), "2024-02-29 23:59:59 UTC");
    assert_eq!(stamp(1_546_300_800_000), "2019-01-01 00:00:00 UTC");
    assert_eq!(stamp(1_739_000_000_000), "2025-02-08 07:33:20 UTC");
    assert_eq!(stamp(253_402_300_799_000), "9999-12-31 23:59:59 UTC");
    // Absurd input, and still a line: no panic on a clock that was handed a wild number.
    let far = stamp(u64::MAX);
    assert!(far.ends_with(" UTC") && far.contains('-'), "{far}");
}

// --- the whole path ----------------------------------------------------------------------

#[test]
fn from_an_onerror_hook_to_the_page() {
    let dir = sandbox("path");
    {
        let mut log = Log::open(&dir, 12).expect("open");
        for _ in 0..5 {
            log.record(Event::script(
                T,
                12,
                1,
                "https://shop.example/checkout?session=9f33",
                "https://pay.example.com/checkout.js?v=3#inline",
                18,
                "PaymentResponse is not defined https://pay.example.com/checkout.js?v=3#inline bob@shop.example",
            ));
        }
        log.record(Event::resource_failed(T + 1, 12, 1, "https://shop.example/checkout", "https://cdn.example/assets/style.css", "tls"));
        log.record(Event::missing_api(T + 2, 12, 2, "https://shop.example/checkout", "requestIdleCallback", 3));
    }
    let sessions = read_sessions(&dir, 4);
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];
    assert_eq!(session.session, 12);
    assert_eq!((session.events.len(), session.corrupt), (5, 0), "the loop wrote at 1, 2 and 4");

    let groups = group_by_host(session);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].page_host, "shop.example");
    assert_eq!(groups[0].total, 4 + 1 + 3);
    assert_eq!(groups[0].kinds, vec![(Kind::ScriptException, 4), (Kind::MissingApi, 3), (Kind::ResourceFailed, 1)]);
    let top = &groups[0].details[0];
    assert_eq!(top.detail, "PaymentResponse is not defined https://pay.example.com/checkout.js <email>");
    assert_eq!((top.file.as_str(), top.line, top.host.as_str()), ("checkout.js", 18, "pay.example.com"));

    let file = std::fs::read_to_string(dir.join(FILE_NAME)).expect("file");
    for secret in ["9f33", "?v=3", "#inline", "bob@shop.example", "checkout?session", "/assets/style.css"] {
        assert!(!file.contains(secret), "{secret} reached errors.jsonl:\n{file}");
    }
    let html = render_html(&sessions);
    assert!(html.contains("shop.example") && html.contains("pay.example.com") && html.contains("checkout.js:18"));
    assert!(!html.contains("9f33"), "the page shows a session id");
}

// --- clearing ----------------------------------------------------------------------------

/// Two hosts, two kinds, two sessions, in one directory: enough that any of the four things a clear
/// can get wrong (the wrong host, the wrong session, the wrong file, the wrong generation) shows up.
fn plant_two_hosts(dir: &Path) {
    let mut log = Log::open(dir, 1).expect("open");
    log.record(Event::script(T, 1, 1, "https://a.example/", "https://a.example/app.js", 3, "a broke"));
    log.record(Event::resource_failed(T, 1, 1, "https://b.example/", "https://b.example/x.png", "404"));
    log.record(Event::missing_api(T, 1, 2, "https://a.example/", "Intl", 1));
    // Pages with no host of their own are a row on the page, and clearable like any other.
    log.record(Event::page_never_loaded(T, 1, 3, "about:home", 30));
    drop(log);
    let mut next = Log::open(dir, 2).expect("open second session");
    next.record(Event::script(T + 1000, 2, 1, "https://b.example/", "https://b.example/app.js", 7, "b broke"));
    drop(next);
}

/// Every host group the page would show, session by session: what a clear has to get right.
fn rows(dir: &Path) -> Vec<String> { read_sessions(dir, usize::MAX).iter().flat_map(group_by_host).map(|group| group.page_host).collect() }

#[test]
fn clearing_one_host_takes_its_rows_and_leaves_every_other_one() {
    let dir = sandbox("clear_host");
    plant_two_hosts(&dir);
    let before = read_sessions(&dir, usize::MAX);
    assert_eq!(before.iter().map(|session| session.events.len()).sum::<usize>(), 5);

    let mut log = Log::open(&dir, 3).expect("open");
    let removed = log.clear(&Target::Host("a.example".to_owned())).expect("clear");
    assert_eq!(removed, 2, "two lines named a.example");

    let hosts = rows(&dir);
    assert!(!hosts.iter().any(|host| host == "a.example"), "{hosts:?}: a.example survived");
    assert!(hosts.contains(&"b.example".to_owned()) && hosts.contains(&String::new()), "{hosts:?}: something else went too");
    let sessions = read_sessions(&dir, usize::MAX);
    assert_eq!(sessions.iter().map(|s| s.events.len()).sum::<usize>(), 3, "the rest of the file is untouched");
    assert!(sessions.iter().any(|s| s.session == 2), "the older session is still there");
}

#[test]
fn clearing_the_no_host_group_leaves_the_hosts_alone() {
    let dir = sandbox("clear_none");
    plant_two_hosts(&dir);
    let mut log = Log::open(&dir, 3).expect("open");
    assert_eq!(log.clear(&Target::Host(String::new())).expect("clear"), 1);
    let hosts = rows(&dir);
    assert!(!hosts.iter().any(String::is_empty), "{hosts:?}: the host-less row survived");
    for host in ["a.example", "b.example"] {
        assert!(hosts.contains(&host.to_owned()), "{hosts:?}: {host} went with it");
    }
}

#[test]
fn a_clear_reaches_every_rolled_away_generation() {
    let dir = sandbox("clear_rolls");
    // Real rollover: long lines over two sessions, so one host ends up in more than one file.
    fill_session(&dir, 1);
    fill_session(&dir, 2);
    let generations = (1..=GENERATIONS).filter(|n| dir.join(format!("errors.{n}.jsonl")).is_file()).count();
    assert!(generations >= 2, "the rollover did not reach a second generation: {generations}");

    let host = long_host('a');
    let mut log = Log::open(&dir, 3).expect("open");
    let removed = log.clear(&Target::Host(host.clone())).expect("clear");
    assert!(removed > 0, "not one line went");
    // Every file that is left holds only lines that are not that host's.
    for path in log_paths(&dir) {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for line in text.lines() {
            let event = Event::from_line(line).expect("a readable line");
            assert_ne!(event.page_host, host, "{} still names it", path.display());
        }
    }
    assert!(rows(&dir).iter().all(|page_host| page_host != &host), "the page still shows the cleared host");

    // And the whole log, generations and all: nothing readable is left anywhere.
    log.clear(&Target::All).expect("clear all");
    for generation in 1..=GENERATIONS {
        assert!(!dir.join(format!("errors.{generation}.jsonl")).exists(), "generation {generation} outlived a clear-all");
    }
    assert_eq!(std::fs::metadata(dir.join(FILE_NAME)).expect("live").len(), 0, "the live file still holds lines");
    assert_eq!(read_sessions(&dir, usize::MAX), Vec::new(), "the page still says something");
}

#[test]
fn a_cleared_error_that_happens_again_is_written_again_from_one() {
    let dir = sandbox("clear_counts");
    let mut log = Log::open(&dir, 4).expect("open");
    let one = Event::script(T, 4, 1, "https://a.example/", "https://a.example/app.js", 3, "same");
    log.record(one.clone()); // count 1, its first line
    log.record(one.clone()); // count 2, the next power of two, so a line too
    log.record(one.clone()); // count 3, folded away
    assert_eq!(lines(&dir).len(), 2);
    assert_eq!(log.clear(&Target::Host("a.example".to_owned())).expect("clear"), 2);

    // The log's memory of what it has said is gone with the lines, so this is a first line again and
    // not a count of four nobody can see. The session cap is spent afresh too.
    log.record(one.clone());
    let kept = lines(&dir);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(Event::from_line(&kept[0]).expect("a line").count, 1, "a cleared error came back at its old count");
}

#[test]
fn a_session_cleared_of_its_cap_gets_its_cap_back() {
    let dir = sandbox("clear_cap");
    let mut log = Log::open(&dir, 1).expect("open");
    for i in 0..MAX_LINES + 5 {
        // A distinct line number each: every event is its own error, so the session really spends its
        // cap rather than folding repeats into a few lines.
        log.record(Event::script(T, 1, 1, "https://a.example/", "https://a.example/app.js", i, "boom"));
    }
    assert!(log.is_truncated(), "the cap never arrived");
    assert_eq!(lines(&dir).len(), MAX_LINES as usize, "the file does not hold what the cap says it does");
    assert_eq!(log.clear(&Target::All).expect("clear"), MAX_LINES as usize);
    assert!(!log.is_truncated(), "a cleared log is still cut off");
    log.record(Event::script(T, 1, 1, "https://b.example/", "https://b.example/app.js", 1, "new"));
    assert_eq!(lines(&dir).len(), 1, "the session had no room left after its clear");
}

#[test]
fn a_memory_log_clears_without_a_file_and_keeps_the_rest() {
    let mut log = Log::memory(9);
    log.record(Event::script(T, 9, 1, "https://a.example/", "https://a.example/app.js", 3, "a broke"));
    log.record(Event::resource_failed(T, 9, 1, "https://b.example/", "https://b.example/x.png", "404"));
    let dir = sandbox("clear_memory");
    assert!(!dir.join(FILE_NAME).exists(), "a memory log touched a file");

    assert_eq!(log.clear(&Target::Host("a.example".to_owned())).expect("memory clear"), 1);
    assert_eq!(log.events().len(), 1);
    assert_eq!(log.events()[0].page_host, "b.example");
    assert_eq!(log.clear(&Target::All).expect("memory clear all"), 1);
    assert!(log.events().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clearing_a_directory_with_no_log_is_nothing_and_makes_nothing() {
    // A profile with no file, and a directory that is not there at all: `Ok(0)` and no side effect —
    // opening the page to find nothing in it is not a request to create a profile.
    let dir = sandbox("clear_empty");
    assert_eq!(clear_files(&dir, &Target::All).expect("nothing to clear"), 0);
    assert_eq!(clear_files(&dir, &Target::Host("a.example".to_owned())).expect("nothing to clear"), 0);
    assert!(!dir.join(FILE_NAME).exists(), "a clear wrote a file that was not there");
    let missing = dir.join("deeper");
    assert_eq!(clear_files(&missing, &Target::All).expect("no directory at all"), 0);
    assert!(!missing.exists(), "a clear made a directory");
}

#[test]
fn an_unreadable_line_survives_its_hosts_clear_and_not_a_clear_all() {
    let dir = sandbox("clear_junk");
    plant(&dir, FILE_NAME, &format!("not an event at all\n{}\n", Event::script(T, 1, 1, "https://a.example/", "", 1, "a broke").to_line()));

    let mut log = Log::open(&dir, 2).expect("open");
    assert_eq!(log.clear(&Target::Host("a.example".to_owned())).expect("clear"), 1);
    let kept = std::fs::read_to_string(dir.join(FILE_NAME)).expect("file");
    assert!(kept.contains("not an event at all"), "{kept}: a line nobody can read was destroyed by a per-host clear");
    assert!(!kept.contains("a.example"), "{kept}: the host's line survived");

    assert_eq!(log.clear(&Target::All).expect("clear all"), 1, "the junk line went too, and counted");
    assert_eq!(std::fs::metadata(dir.join(FILE_NAME)).expect("live").len(), 0, "{kept}");
}

#[test]
fn the_files_are_rewritten_where_the_log_still_writes() {
    // The handle a `Log` holds is of the bytes that were there before a clear: after one, the next
    // line still has to land in the file that is on the disk now.
    let dir = sandbox("clear_handle");
    let mut log = Log::open(&dir, 1).expect("open");
    log.record(Event::script(T, 1, 1, "https://a.example/", "https://a.example/app.js", 1, "before"));
    log.clear(&Target::Host("a.example".to_owned())).expect("clear");
    log.record(Event::script(T + 1, 1, 1, "https://c.example/", "https://c.example/app.js", 1, "after"));
    drop(log);
    let kept = lines(&dir);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(Event::from_line(&kept[0]).expect("a line").page_host, "c.example");
}

// --- the page's controls -------------------------------------------------------------------

fn two_host_sessions() -> Vec<SessionErrors> {
    vec![SessionErrors {
        session: 4,
        events: vec![
            Event::script(T, 4, 1, "https://a.example/", "https://a.example/app.js", 3, "a broke"),
            Event::resource_failed(T, 4, 1, "https://b.example/", "https://b.example/x.png", "tls"),
            Event::missing_api(T, 4, 2, "about:home", "Intl", 1),
        ],
        corrupt: 0,
    }]
}

#[test]
fn the_page_puts_a_clear_beside_every_host_and_one_for_the_whole_log() {
    let links = [
        ClearLink { page_host: "a.example".to_owned(), href: "about:errors/clear?host=a.example&t=9f2".to_owned() },
        ClearLink { page_host: String::new(), href: "about:errors/clear?host=&t=1d4".to_owned() },
    ];
    let page = Page {
        clear_hosts: &links,
        clear_all: Some("about:errors/clear-all?t=77c"),
        notice: Some("Nothing was cleared."),
        lines: Lines::File("/home/user/.local/share/lucinanet"),
    };
    let html = render_html_with(&two_host_sessions(), &page);
    assert_eq!(html.matches("<a href=").count(), 3, "{html}");
    assert!(html.contains("about:errors/clear-all?t=77c"), "{html}");
    assert!(html.contains("Clear</a>"), "{html}");
    assert!(html.contains("<th>Remove</th>"), "no column of controls");
    assert!(html.contains("Nothing was cleared."), "the notice did not reach the page");
    assert!(html.contains("/home/user/.local/share/lucinanet/errors.jsonl"), "the footer did not name the file");
    // The host-less group gets its own control, and by the row it is on rather than by name. The
    // ampersand of a query arrives as `&amp;`, which is how an attribute is written.
    assert!(html.contains("about:errors/clear?host=&amp;t=1d4"), "{html}");
}

#[test]
fn the_page_names_the_setting_the_environment_and_the_control_it_tells_you_about() {
    let html = render_html_with(&two_host_sessions(), &Page::plain());
    assert!(!html.contains("<a href"), "a plain page offers a control");
    assert!(!html.contains("<th>Remove</th>"));
    let links = [ClearLink { page_host: "a.example".to_owned(), href: "about:errors/clear?host=a.example&t=9f2".to_owned() }];
    let with = render_html_with(&two_host_sessions(), &Page { clear_hosts: &links, clear_all: Some("about:errors/clear-all?t=1"), ..Page::default() });
    // The sentence that used to point at a settings page that did not exist, made true: the field,
    // the file it lives in, the variable, and the fact that the env wins.
    for words in ["error_log", "prefs.json", "privacy", "LUCINANET_ERRLOG=0", "Clear all", "ask first"] {
        assert!(with.contains(words), "{words} missing from:\n{with}");
    }
    assert!(!with.contains("in the settings"), "the page still points at a settings page");
    // A run with no profile is told the truth about its own lines, which are not in a file.
    let memory = render_html_with(&two_host_sessions(), &Page { lines: Lines::Memory, ..Page::default() });
    assert!(memory.contains("in memory and die with it"), "{memory}");
    assert!(!memory.contains("This file is in your LucinaNet profile directory"));
}

#[test]
fn the_host_of_a_clear_link_is_escaped_like_everything_else_on_the_page() {
    let odd = r#"<img src="x" onerror=alert(1)>"#;
    let links = [ClearLink { page_host: odd.to_owned(), href: format!("about:errors/clear?host={odd}&t=1") }];
    // A page host this hostile cannot come out of [`host_of`], which only accepts a real name. It
    // comes from a file that was edited by hand, which is the case the escaping is for.
    let mut event = Event::script(T, 1, 1, "https://a.example/", "", 1, "broke");
    event.page_host = odd.to_owned();
    let html = render_html_with(&[SessionErrors { session: 1, events: vec![event], corrupt: 0 }], &Page { clear_hosts: &links, ..Page::default() });
    assert!(!html.contains("<img"), "{html}");
    assert!(html.contains("&lt;img src=&quot;x&quot;"), "the host in its cell: {html}");
    assert!(html.contains("host=&lt;img"), "the host in the href: {html}");
    assert_eq!(html.matches("&lt;img src=&quot;x&quot; onerror=alert(1)&gt;").count(), 2, "cell and link, once each: {html}");
}

#[test]
fn the_ask_page_says_what_goes_and_which_link_says_yes() {
    let ask = render_clear_ask(&Target::Host("a.example".to_owned()), "about:errors/clear/confirm?host=a.example&t=ab", "about:errors");
    assert!(ask.starts_with("<!DOCTYPE html>"), "{ask}");
    assert!(ask.contains("Clear a.example?"), "{ask}");
    assert!(ask.contains("every line of <code>a.example</code>"), "{ask}");
    assert!(ask.contains("about:errors/clear/confirm?host=a.example&amp;t=ab") || ask.contains("about:errors/clear/confirm?host=a.example&t=ab"), "{ask}");
    assert!(ask.contains("about:errors"), "no way back");
    assert!(!ask.contains("<script") && !ask.contains("onclick="), "the ask page needs a script");

    let all = render_clear_ask(&Target::All, "about:errors/clear-all/confirm?t=1", "about:errors");
    assert!(all.contains("Clear the whole error log?"), "{all}");
    assert!(all.contains("generations rolled away"), "{all}");

    let none = render_clear_ask(&Target::Host(String::new()), "about:errors/clear/confirm?host=&t=1", "about:errors");
    assert!(none.contains("no host"), "{none}");
}
