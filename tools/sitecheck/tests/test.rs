//! Tests for the sitecheck library: line parsing, median, verdicts,
//! Markdown and JSON output, and the sample logs in tests/data.

use std::path::Path;

use serde_json::Value;
use sitecheck::{parse_line, print_json, print_table, scroll_median, verdict, Site};

fn site_from_file(path: impl AsRef<Path>) -> Site {
    let text = std::fs::read_to_string(&path).unwrap();
    let mut site = Site::default();
    for line in text.lines() {
        parse_line(&mut site, line);
    }
    if site.url.is_empty() {
        site.url = path.as_ref().to_string_lossy().to_string();
    }
    site
}

#[test]
fn parses_each_known_line() {
    let mut site = Site::default();

    parse_line(&mut site, "# url https://example.com/");
    assert_eq!(site.url, "https://example.com/");

    parse_line(
        &mut site,
        "render: complete true in 5310 ms, rss 170816 KiB",
    );
    assert!(site.loaded);
    assert_eq!(site.load_ms, Some(5310));

    parse_line(
        &mut site,
        "render: complete false in 120000 ms, rss 200000 KiB",
    );
    assert!(!site.loaded);
    assert_eq!(site.load_ms, Some(120000));

    parse_line(
        &mut site,
        "lucinanet: load Complete in 1532 ms, rss 170484 KiB",
    );
    assert_eq!(site.page_load_ms, Some(1532));

    // Down steps are kept in whole ms; up steps are ignored.
    parse_line(&mut site, "render: down to frame 82123 us");
    parse_line(&mut site, "render: up to frame 4512 us");
    assert_eq!(site.scroll_steps, vec![82]);

    parse_line(&mut site, "console Error: Test error");
    assert_eq!(site.console_errors, vec!["Test error".to_string()]);

    parse_line(&mut site, "lucinanet: signal 11 at pc 0x12345678, ...");
    assert!(site.crashed);
    assert_eq!(site.crash_reason, Some("reason=Crash".to_string()));

    let mut stub = Site::default();
    parse_line(
        &mut stub,
        "mozjs_sys (Rust): JS_TransplantObject is not implemented",
    );
    assert!(stub.crashed);
    assert_eq!(
        stub.crash_reason,
        Some("shim=JS_TransplantObject reason=Crash".to_string())
    );

    let mut fallback = Site::default();
    parse_line(
        &mut fallback,
        "phase navigation Ok(String(\"[[\"https://fallback.example.com\",true])\")",
    );
    assert_eq!(fallback.url, "https://fallback.example.com");
}

#[test]
fn url_header_wins_over_navigation_line() {
    let mut site = Site::default();
    parse_line(&mut site, "# url https://header.example.com/");
    parse_line(
        &mut site,
        "phase navigation Ok(String(\"[[\"https://other.example.com\",true])\")",
    );
    assert_eq!(site.url, "https://header.example.com/");
}

#[test]
fn console_errors_capped_and_truncated() {
    let mut site = Site::default();
    let long: String = std::iter::repeat('x').take(200).collect();
    for text in ["one", "two", long.as_str(), "four"] {
        parse_line(&mut site, &format!("console Error: {}", text));
    }
    assert_eq!(site.console_errors.len(), 3);
    assert_eq!(site.console_errors[2].len(), 120);
}

#[test]
fn median_of_odd_and_even_counts() {
    assert_eq!(scroll_median(&[]), 0);
    assert_eq!(scroll_median(&[501, 406, 530]), 501); // odd: middle step
    assert_eq!(scroll_median(&[12, 15, 14, 18]), 14); // even: mean of the two middle steps
}

#[test]
fn verdicts_follow_the_spec() {
    let mut pass = Site::default();
    pass.loaded = true;
    pass.load_ms = Some(5310);
    pass.scroll_steps = vec![40, 45, 50];
    assert_eq!(verdict(&pass, 15000, 50), "PASS");

    let mut not_loaded = Site::default();
    not_loaded.loaded = false;
    assert_eq!(verdict(&not_loaded, 15000, 50), "FAIL load");

    let mut slow_load = pass.clone();
    slow_load.load_ms = Some(20000);
    assert_eq!(verdict(&slow_load, 15000, 50), "FAIL load");
    // ...unless the threshold is raised above it.
    assert_eq!(verdict(&slow_load, 25000, 50), "PASS");

    let mut slow_scroll = pass.clone();
    slow_scroll.scroll_steps = vec![406, 501, 530];
    assert_eq!(verdict(&slow_scroll, 15000, 50), "FAIL scroll");
    // ...unless the threshold is raised above the median.
    assert_eq!(verdict(&slow_scroll, 15000, 600), "PASS");

    let mut no_scroll = pass.clone();
    no_scroll.scroll_steps.clear();
    assert_eq!(verdict(&no_scroll, 15000, 50), "FAIL scroll");

    let mut crashed = pass.clone();
    crashed.crashed = true;
    crashed.crash_reason = Some("shim=JS_TransplantObject reason=Crash".to_string());
    assert_eq!(verdict(&crashed, 15000, 50), "CRASH");
}

#[test]
fn markdown_table_rows_and_crash_footnote() {
    let sites = vec![
        Site {
            url: "https://example.com/".to_string(),
            loaded: true,
            load_ms: Some(5310),
            page_load_ms: Some(1532),
            scroll_steps: vec![12, 15, 14, 18],
            console_errors: vec![],
            crashed: false,
            crash_reason: None,
        },
        Site {
            url: "https://crash.example.com/".to_string(),
            loaded: true,
            load_ms: Some(4000),
            page_load_ms: None,
            scroll_steps: vec![],
            console_errors: vec![],
            crashed: true,
            crash_reason: Some("shim=JS_TransplantObject reason=Crash".to_string()),
        },
    ];
    let table = print_table(&sites, 15000, 50);

    let expected_row = "| https://example.com/ | true | 5310 | 1532 | 14 | 18 | 0 | PASS |";
    assert!(table.contains(expected_row), "table:\n{}", table);
    assert!(table.contains("CRASH"), "table:\n{}", table);
    assert!(
        table.contains("1. https://crash.example.com/: shim=JS_TransplantObject reason=Crash"),
        "table:\n{}",
        table
    );
}

#[test]
fn json_output_has_one_object_per_site() {
    let sites = vec![Site {
        url: "https://example.com/".to_string(),
        loaded: true,
        load_ms: Some(5310),
        page_load_ms: Some(1532),
        scroll_steps: vec![406, 501, 530],
        console_errors: vec!["Test error".to_string()],
        crashed: false,
        crash_reason: None,
    }];
    let json = print_json(&sites);

    let value: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["url"], "https://example.com/");
    assert_eq!(value["loaded"], true);
    assert_eq!(value["load_ms"], 5310);
    assert_eq!(value["page_load_ms"], 1532);
    assert_eq!(value["scroll_steps"], Value::from(vec![406u64, 501, 530]));
    assert_eq!(value["console_errors"][0], "Test error");
    assert_eq!(value["crashed"], false);
    assert!(value.get("crash_reason").map_or(true, |v| v.is_null()));
}

#[test]
fn sample_pass_site_parses_and_passes() {
    let site = site_from_file(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/pass.log"));
    assert_eq!(site.url, "https://example.com/");
    assert!(site.loaded);
    assert_eq!(site.load_ms, Some(5310));
    assert_eq!(site.page_load_ms, Some(1532));
    assert_eq!(site.scroll_steps, vec![12, 15, 14, 18]);
    assert!(!site.crashed);
    assert_eq!(verdict(&site, 15000, 50), "PASS");
}

#[test]
fn sample_slow_scroll_site_fails_scroll() {
    let site =
        site_from_file(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/slow-scroll.log"));
    assert_eq!(site.url, "https://slow.example.com/");
    assert!(site.loaded);
    assert_eq!(site.scroll_steps, vec![406, 501, 530]);
    assert_eq!(scroll_median(&site.scroll_steps), 501);
    assert_eq!(verdict(&site, 15000, 50), "FAIL scroll");
}

#[test]
fn sample_crash_site_is_a_crash() {
    let site = site_from_file(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/crash.log"));
    assert_eq!(site.url, "https://crash.example.com/");
    assert!(site.crashed);
    assert_eq!(
        site.crash_reason.as_deref(),
        Some("shim=JS_TransplantObject reason=Crash")
    );
    assert_eq!(verdict(&site, 15000, 50), "CRASH");
}
