//! The aggregator end to end over the checked-in logs.
//!
//! Four of them are the real stderr of `lucinanet --render` runs, copied whole from
//! `<work-dir>/lucinanet/example-site-run/sites` (2026-11-12, the engine before the Intl pin, which is why
//! they still say `Intl` is not defined). The fifth, `probe.missing`, is a synthetic
//! `LUCINANET_JS_MISSING_LOG` file: no shipped binary has written one, because the glue that does it is
//! this branch, so its own channel needs a fixture the run cannot yet give it.

use lucinanet_sitelog::{
    aggregate, collect_logs, newest_stamp, parse_line, read_events, render_md, render_tsv, site_of, sites_per_kind, Args, Event, Kind, Row, Summary,
};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sites")
}

fn events() -> Vec<(String, Event)> {
    read_events(&collect_logs(&[fixtures()]).expect("the fixture directory reads"))
}

fn row<'a>(rows: &'a [Row], message: &str) -> &'a Row {
    rows.iter().find(|r| r.message == message).unwrap_or_else(|| panic!("no row for {message:?}"))
}

#[test]
fn a_directory_contributes_its_log_files_by_path_and_its_stems_are_the_sites() {
    let paths = collect_logs(&[fixtures()]).expect("the fixture directory reads");
    let names: Vec<String> = paths.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["bbc.err", "example-site.err", "github.err", "google-signin.err", "probe.missing"], "sorted, so the example never depends on argument order");
    assert_eq!(site_of(Path::new("x/sites/youtube.err")), "youtube");
    assert_eq!(site_of(Path::new("x/sites/youtube.missing")), "youtube", "the two channels of one site are one site");
    // A flagless path that is neither file nor directory is the user's mistake, not an empty table.
    assert!(collect_logs(&[PathBuf::from("tests/fixtures/no-such-run")]).is_err());
}

#[test]
fn one_site_said_nothing_and_the_rest_did() {
    let events = events();
    let sites: std::collections::BTreeSet<String> = events.iter().map(|(s, _)| s.clone()).collect();
    assert_eq!(sites.len(), 4, "bbc logged phases and network only, which is not a missing feature");
    assert!(!sites.contains("bbc"));
    assert_eq!(events.len(), 14, "8 Intl (6 + 1 + 1 from the glue log) + 1 stack + 1 warn + 2 glue errors + 2 glue misses");
    assert!(newest_stamp(&[fixtures().join("bbc.err").as_path()]).is_some(), "the table knows its own date");
}

#[test]
fn the_glue_log_and_the_console_lines_of_the_same_message_are_one_row() {
    let rows = aggregate(&events());
    let intl = row(&rows, "ReferenceError: Intl is not defined");
    assert_eq!(intl.kind, Kind::Error);
    assert_eq!((intl.sites, intl.hits), (3, 8), "github, example-site and the glue log, 6 + 1 + 1 lines");
    // The first hit that names a location wins: example-site's rejections name none, github's does.
    assert_eq!((intl.example_site.as_str(), intl.example_location.as_str()), ("github", "https://github.githubassets.com/assets/code-view-a0e205571facc60b.js:2"));
    let stack = row(&rows, "TypeError: cannot set property stack");
    assert_eq!((stack.sites, stack.hits), (1, 1));
    let warn = row(&rows, "No valid entry type provided to observe().");
    assert_eq!(warn.kind, Kind::Warn);
    assert_eq!((warn.sites, warn.example_site.as_str()), (1, "github"));
    assert_eq!(
        rows[0].message,
        "ReferenceError: Intl is not defined",
        "the most-hit row ranks first; the rest tie on one site and one hit and order by kind then text"
    );
    let missing: Vec<&str> = rows.iter().filter(|r| r.kind == Kind::Missing).map(|r| r.message.as_str()).collect();
    assert_eq!(missing, ["JS_DefineDebuggerObject is not implemented", "glue::NewExternalArrayBuffer is not implemented"]);
    assert_eq!(sites_per_kind(&events()), std::collections::BTreeMap::from([(Kind::Error, 4), (Kind::Missing, 1), (Kind::Warn, 1)]));
}

#[test]
fn the_summary_is_short_the_tables_are_stable_and_both_carry_the_provenance() {
    let paths = collect_logs(&[fixtures()]).expect("the fixture directory reads");
    let given: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let events = read_events(&paths);
    let rows = aggregate(&events);
    let kind_sites = sites_per_kind(&events);
    let summary = Summary {
        rows: &rows,
        kind_sites: &kind_sites,
        files: paths.len(),
        sites: 5,
        silent: 1,
        events: events.len(),
        stamp: newest_stamp(&given),
        build: "the probe binary of 2026-11-13 12:28 UTC",
    };
    let md = render_md(&summary);
    assert!(md.lines().count() < 80, "{} lines", md.lines().count());
    assert!(md.lines().all(|l| l.chars().count() < 200), "the summary is readable: no line of a whole bundle URL");
    assert!(md.contains("…") && md.contains("/m=_b,_tp:105"), "a long location keeps its tail, where the file and the line are");
    assert!(!md.contains("gstatic"), "and loses its head here, though not in the TSV");
    let tsv = render_tsv(&summary);
    assert!(tsv.contains("https://www.gstatic.com"), "the TSV keeps the whole location");
    assert!(md.contains("\nBuild: the probe binary of 2026-11-13 12:28 UTC\n"), "what produced the logs travels with them");
    assert!(md.contains("\nengine\tIntl\t"), "Intl is a standard name, so the row is marked engine");
    assert!(md.contains("\nweb-engine\tIntersectionObserver\t"), "a Web name is marked web-engine");
    assert!(md.contains("\nengine\tglue::NewExternalArrayBuffer\t"), "a not-implemented line is the glue's own");
    assert!(!md.contains("cannot set property stack\n"), "a row that names nothing is not in the built-in table");
    assert_eq!(tsv.lines().count(), 9 + 1 + rows.len());
    for line in tsv.lines().filter(|l| !l.starts_with('#')) {
        assert_eq!(line.split('\t').count(), 6, "six columns in every line: {line}");
    }
    assert_eq!(render_tsv(&summary), tsv, "the same input renders the same bytes");
    // And the whole run, twice, into the target directory.
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("out-{}", std::process::id()));
    let args = Args { logs: vec![fixtures()], out_dir: out.clone(), build: "test".into(), help: false };
    lucinanet_sitelog::run(&args).expect("a run writes both files");
    let once = std::fs::read_to_string(out.join("missing-runtime.tsv")).expect("the tsv");
    lucinanet_sitelog::run(&args).expect("a second run too");
    assert_eq!(once, std::fs::read_to_string(out.join("missing-runtime.tsv")).expect("the tsv again"), "deterministic between runs");
    assert!(out.join("missing-runtime.md").is_file());
}

#[test]
fn a_stray_binary_in_the_run_directory_is_skipped_not_fatal() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("mixed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary run directory");
    std::fs::write(dir.join("shot.png"), [0u8, 159, 10, 26]).expect("a screenshot");
    std::fs::write(dir.join("a.err"), "console Warn: only a warning\nphase 12 3 main\n").expect("a log");
    std::fs::write(dir.join("a.mlog"), "1794600000000\tmissing\tJS_GetPropertyWithLength\n").expect("the glue log of the same site");
    let paths = collect_logs(&[dir.clone()]).expect("the directory reads");
    assert_eq!(paths, vec![dir.join("a.err"), dir.join("a.mlog")], "only a log extension is taken; `.mlog` is the stem the 30-site run writes");
    let events = read_events(&paths);
    assert_eq!(events.len(), 2, "the console line and the glue line, one site each");
    assert_eq!(events.iter().map(|(site, _)| site.as_str()).collect::<Vec<_>>(), ["a", "a"], "so the two channels are still one site");
    assert_eq!(parse_line("console Warn: only a warning").map(|e| e.kind), Some(Kind::Warn));
    std::fs::remove_dir_all(&dir).expect("the temporary directory goes away");
}
