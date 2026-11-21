//! Inventory tests over a tiny checked-in fixture (tests/fixtures/inventory.jsonl).

use lucinanet_inventory::{
    area_of, group_by_area, group_by_feature, normalize_message, ranked, read_rows, relative_path,
    render_areas, render_features, render_failures, render_markdown, top_failures, totals, Args,
    Counts,
};
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inventory.jsonl")
}

fn rows() -> Vec<lucinanet_inventory::Row> {
    let mut rows = read_rows(&fixture()).expect("fixture reads");
    rows.sort_by(|a, b| a.path.cmp(&b.path).then(a.variant.cmp(&b.variant)));
    rows
}

/// The data rows of a rendered table, keyed by the first column.
fn data_row(text: &str, key: &str) -> Vec<String> {
    let found: Vec<Vec<String>> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .skip(1)
        .map(|l| l.split('\t').map(|c| c.to_string()).collect())
        .filter(|cells: &Vec<String>| cells.first().map(|c| c == key).unwrap_or(false))
        .collect();
    assert_eq!(found.len(), 1, "{key:?} appears {} times", found.len());
    found[0].clone()
}

fn counts_of(map: &std::collections::BTreeMap<String, Counts>, key: &str) -> Counts {
    *map.get(key).unwrap_or_else(|| panic!("no group {key:?}"))
}

#[test]
fn variants_are_counted_separately_and_results_classified() {
    let features = group_by_feature(&rows());
    // One test, two variants: pass and fail.
    assert_eq!(
        counts_of(&features, "WeakRef"),
        Counts { total: 2, pass: 1, fail: 1, skipped: 0 }
    );
    // timeout and crash count as failures, only "skipped" does not.
    assert_eq!(
        counts_of(&features, "arrow-function"),
        Counts { total: 1, pass: 0, fail: 1, skipped: 0 }
    );
    assert_eq!(counts_of(&features, "WeakRef").status(), "partial");
    assert_eq!(counts_of(&features, "arrow-function").status(), "none");
    assert_eq!(counts_of(&features, "Intl").status(), "complete");
    assert_eq!(counts_of(&features, "Intl").pass_pct(), 100.0);
    assert_eq!(
        counts_of(&features, "WeakRef").pass_pct().round(),
        50.0
    );
}

#[test]
fn a_test_with_several_features_counts_under_each() {
    let features = group_by_feature(&rows());
    for name in ["destructuring-binding", "iterable-array-like"] {
        assert_eq!(
            counts_of(&features, name),
            Counts { total: 2, pass: 1, fail: 1, skipped: 0 },
            "{name}"
        );
    }
}

#[test]
fn everything_skipped_is_unsupported_by_runner() {
    let features = group_by_feature(&rows());
    for name in ["Symbol.iterator", "IsHTMLDDA"] {
        let c = counts_of(&features, name);
        assert_eq!((c.total, c.skipped), (2, 2), "{name}");
        assert_eq!(c.status(), "unsupported-by-runner", "{name}");
    }
    // A feature whose variants are all skipped, whatever the skip reason.
    assert_eq!(counts_of(&features, "await").status(), "unsupported-by-runner");
}

#[test]
fn tests_without_features_are_counted_in_areas_only() {
    let rows = rows();
    assert!(group_by_feature(&rows).keys().all(|k| !k.is_empty()));
    assert_eq!(
        counts_of(&group_by_feature(&rows), "Temporal").total,
        2,
        "the fixture declares features for Temporal only there"
    );
    let areas = group_by_area(&rows, None);
    // language/statements/const/redefine.js declares no feature.
    assert_eq!(
        counts_of(&areas, "language/statements"),
        Counts { total: 2, pass: 1, fail: 1, skipped: 0 }
    );
    let area_total: usize = areas.values().map(|c| c.total).sum();
    assert_eq!(area_total, rows.len(), "areas cover every variant");
}

#[test]
fn areas_are_the_test262_directories_under_test() {
    let areas = group_by_area(&rows(), None);
    for name in [
        "built-ins/WeakRef",
        "built-ins/Temporal/PlainDate",
        "built-ins/Object",
        "annexB/built-ins",
        "language/expressions",
        "intl402/DateTimeFormat",
        "staging/await",
    ] {
        assert!(areas.contains_key(name), "missing area {name:?}");
    }
    assert_eq!(area_of("intl402/Temporal/Duration/x.js"), "intl402/Temporal/Duration");
    assert_eq!(area_of("intl402/NumberFormat/x.js"), "intl402/NumberFormat");
    assert_eq!(area_of("built-ins/Intl/DateTimeFormat/x.js"), "built-ins/Intl/DateTimeFormat");
    assert_eq!(
        area_of("built-ins/RegExp/prototype/test/y.js"),
        "built-ins/RegExp"
    );
    assert_eq!(area_of("language/expressions/x/y.js"), "language/expressions");
    // A test directly in a directory names the directory, not itself.
    assert_eq!(
        area_of("intl402/supportedLocalesOf-default-locale.js"),
        "intl402"
    );
    assert_eq!(area_of("language/types/null.js"), "language/types");
    assert_eq!(area_of("README.md"), "(root)");
    assert_eq!(area_of(""), "(root)");
}

#[test]
fn paths_become_relative() {
    assert_eq!(
        relative_path("/corpus/test/built-ins/Array/x.js", Some("/corpus")),
        "built-ins/Array/x.js"
    );
    assert_eq!(
        relative_path("/elsewhere/test/built-ins/Array/x.js", Some("/corpus")),
        "built-ins/Array/x.js",
        "without a matching root the first /test/ is the boundary"
    );
    assert_eq!(
        relative_path("/c/test/built-ins/RegExp/prototype/test/y.js", Some("/c")),
        "built-ins/RegExp/prototype/test/y.js"
    );
    assert_eq!(
        relative_path("/corpus/tool.js", Some("/corpus")),
        "/corpus/tool.js"
    );
    assert_eq!(relative_path("bare.js", None), "bare.js");
}

#[test]
fn equal_causes_group_after_normalisation() {
    assert_eq!(
        normalize_message("Expected SameValue(«1», «0») to be true"),
        "Expected SameValue(X, X) to be true"
    );
    assert_eq!(
        normalize_message("uncaught undefined: #1: scope.x === 2. Actual: undefined"),
        "uncaught undefined: #N: scope.x === N. Actual: undefined"
    );
    assert_eq!(
        normalize_message("uncaught TypeError: `x` is not defined"),
        "uncaught TypeError: X is not defined"
    );
    assert_eq!(
        normalize_message("uncaught TypeError: \"y\" is not defined"),
        "uncaught TypeError: X is not defined"
    );
    assert_eq!(normalize_message("exceeded 30000 ms"), "exceeded N ms");
    assert_eq!(
        normalize_message("  a\n\t b "),
        "a b",
        "whitespace collapses"
    );
    assert_eq!(normalize_message("5 and 5"), "N and N");
    assert_eq!(normalize_message("unbalanced ` quote"), "unbalanced ` quote");
}

#[test]
fn failure_table_keeps_count_feature_and_example() {
    let rows = rows();
    let failures = top_failures(&rows, 200, None);
    let same = failures
        .iter()
        .find(|f| f.message == "uncaught undefined: Expected SameValue(X, X) to be true")
        .expect("the two SameValue messages group");
    assert_eq!(same.count, 2, "one per failing variant");
    // Equal feature counts resolve to the first name, and paths resolve the example.
    assert_eq!(same.feature, "Object");
    assert_eq!(same.example, "built-ins/Object/assign/numbers.js [sloppy]");
    assert_eq!(
        failures.first().map(|f| f.count),
        Some(2),
        "most frequent first"
    );
    assert!(
        failures.iter().all(|f| f.count >= 1 && !f.message.is_empty()),
        "skips are not failures: {:?}",
        failures.iter().map(|f| &f.message).collect::<Vec<_>>()
    );
    assert!(!failures.iter().any(|f| f.message.starts_with("feature-not-supported")));
    assert!(!failures.iter().any(|f| f.message == "module-unsupported"));
}

#[test]
fn tables_are_ranked_by_non_passes_then_name() {
    let rows = rows();
    let text = render_features(&ranked(&group_by_feature(&rows)), "fixture");
    let first = text.lines().filter(|l| !l.starts_with('#')).nth(1).unwrap();
    let weak = data_row(&text, "WeakRef");
    assert_eq!(
        weak,
        vec![
            "WeakRef".to_string(),
            "2".to_string(),
            "1".to_string(),
            "1".to_string(),
            "0".to_string(),
            "50.00".to_string(),
            "partial".to_string()
        ]
    );
    assert!(first.starts_with("IsHTMLDDA\t"), "got {first:?}");
    assert!(text.starts_with("# Generated by `lucinanet-inventory`"));
    assert!(text.contains("# Sources: fixture\n"));
    let areas = render_areas(&ranked(&group_by_area(&rows, None)), "fixture");
    assert_eq!(
        data_row(&areas, "annexB/built-ins")[6],
        "unsupported-by-runner"
    );
    assert_eq!(data_row(&areas, "built-ins/Object")[6], "none");
    assert_eq!(data_row(&areas, "built-ins/WeakRef")[5], "50.00");
    let failures = render_failures(&top_failures(&rows, 200, None), "fixture");
    assert!(failures.contains("message\tcount\tfeature\texample\n"));
    assert_eq!(data_row(&failures, "uncaught TypeError: X is not defined")[1], "1");
}

#[test]
fn summary_reports_the_same_numbers_as_the_tables() {
    let rows = rows();
    let features = group_by_feature(&rows);
    let areas = group_by_area(&rows, None);
    let overall = totals(&rows);
    assert_eq!(
        overall,
        Counts { total: 14, pass: 5, fail: 6, skipped: 3 }
    );
    let md = render_markdown(&features, &areas, &overall, "fixture");
    let areas_text = render_areas(&ranked(&areas), "fixture");
    let mut sums = Counts::default();
    for line in areas_text.lines().filter(|l| !l.starts_with('#')).skip(1) {
        let c: Vec<&str> = line.split('\t').collect();
        sums.total += c[1].parse::<usize>().unwrap();
        sums.pass += c[2].parse::<usize>().unwrap();
        sums.fail += c[3].parse::<usize>().unwrap();
        sums.skipped += c[4].parse::<usize>().unwrap();
    }
    assert_eq!(sums, overall, "the area table adds up to the totals");
    assert!(
        md.contains(&format!(
            "{} variants: {} pass ({:.2}%), {} fail, {} skipped.",
            overall.total, overall.pass, overall.pass_pct(), overall.fail, overall.skipped
        )),
        "{md}"
    );
    // The same rows the feature table has, in the same order, in the summary.
    for line in render_features(&ranked(&features), "fixture")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .skip(1)
        .take(25)
    {
        let c: Vec<&str> = line.split('\t').collect();
        let non_passes: usize = c[1].parse::<usize>().unwrap() - c[2].parse::<usize>().unwrap();
        assert!(
            md.contains(&format!("{}\t{}\t{}\t{}\t{}\n", c[0], c[1], non_passes, c[5], c[6])),
            "{} is in the feature table but not in the summary",
            c[0]
        );
    }
    assert!(md.contains("built-ins/Object"), "areas below 90% are listed: {md}");
    assert!(md.contains("IsHTMLDDA"), "the skip list is present: {md}");
    assert!(md.lines().count() < 120, "summary is {} lines", md.lines().count());
}

#[test]
fn run_writes_deterministic_files_and_merges_the_intl_run() {
    let out = std::env::temp_dir().join(format!(
        "lucinanet-inventory-test-{}-{}",
        std::process::id(),
        "merge"
    ));
    let _ = std::fs::remove_dir_all(&out);
    let intl = out.join("intl.jsonl");
    std::fs::create_dir_all(&out).expect("temp dir");
    std::fs::write(
        &intl,
        "{\"features\":[\"Intl\"],\"message\":\"\",\"path\":\"/corpus/test/intl402/NumberFormat/x.js\",\"result\":\"fail\",\"variant\":\"sloppy\"}\n",
    )
    .expect("write intl fixture");
    let args = Args {
        results: fixture(),
        intl_results: Some(intl.clone()),
        out_dir: out.clone(),
        test262: Some("/corpus".to_string()),
        help: false,
    };
    lucinanet_inventory::run(&args).expect("first run");
    let first = read_all(&out);
    lucinanet_inventory::run(&args).expect("second run");
    assert_eq!(read_all(&out), first, "output must be byte-identical");

    let text = std::fs::read_to_string(out.join("missing-test262-features.tsv")).unwrap();
    assert_eq!(
        data_row(&text, "Intl"),
        vec![
            "Intl", "2", "1", "1", "0", "50.00", "partial"
        ],
        "the intl run merges into the same groups"
    );
    assert!(text.contains("# Sources: "), "{text}");
    let areas = std::fs::read_to_string(out.join("missing-test262-areas.tsv")).unwrap();
    assert_eq!(data_row(&areas, "intl402/NumberFormat")[1], "1");
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(&intl).ok();
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

#[test]
fn the_command_line_needs_a_results_file_and_an_out_dir() {
    let out = std::env::temp_dir().join(format!("lucinanet-inventory-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let fixture = fixture().to_string_lossy().into_owned();
    let dir = out.to_string_lossy().into_owned();
    lucinanet_inventory::run_args(&argv(&["--results", &fixture, "--out-dir", &dir]))
        .expect("cli run without --test262, paths fall back to the first /test/");
    let text = std::fs::read_to_string(out.join("missing-test262-features.tsv")).unwrap();
    assert!(data_row(&text, "WeakRef")[1] == "2");
    assert!(std::fs::read_to_string(out.join("missing-test262.md"))
        .unwrap()
        .contains("14 variants:"));
    std::fs::remove_dir_all(&out).ok();

    lucinanet_inventory::run_args(&argv(&["--help"])).expect("--help is not an error");
    let problem = lucinanet_inventory::run_args(&argv(&[])).unwrap_err();
    assert!(problem.contains("--results <jsonl> is required"), "{problem}");
    assert!(problem.contains("--out-dir <dir>"), "{problem}");
    assert!(lucinanet_inventory::run_args(&argv(&["--results"])).unwrap_err().contains("needs a value"));
    assert!(lucinanet_inventory::run_args(&argv(&["--nope"]))
        .unwrap_err()
        .contains("unknown argument: --nope"));
}

fn read_all(dir: &Path) -> Vec<(String, String)> {
    ["missing-test262-features.tsv",
     "missing-test262-areas.tsv",
     "missing-test262-top-failures.tsv",
     "missing-test262.md"]
        .iter()
        .map(|n| {
            let p = dir.join(n);
            (
                n.to_string(),
                std::fs::read_to_string(&p)
                    .unwrap_or_else(|e| panic!("{}: {e}", p.display())),
            )
        })
        .collect()
}
