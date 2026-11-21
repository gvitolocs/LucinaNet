//! The byte-for-byte test: the tool over the committed fixture, against what the retired Python printed.
//!
//! The fixture is ten runs from the the build host matrices - four of the pre-rewrite driver, two of the paced
//! re-run, three of the fills, one of the unpaced re-run - and a job log excerpt whose clocks have been
//! moved to a fixed epoch so that `mtimes.txt` can put every run's json back inside the window of the
//! block it belongs to. git carries no mtimes, so the test touches the copies before it runs anything.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const TOOL: &str = env!("CARGO_BIN_EXE_scroll-bench-tables");

/// The fixture, copied to a scratch directory with its mtimes set, and taken away again - including
/// when the test fails, so a red run leaves no tree behind for the next one to trip over.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn prepare(name: &str) -> Fixture {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fixture-{name}"));
        let _ = fs::remove_dir_all(&dir);
        copy(&Self::source(), &dir).expect("the fixture is readable");
        let fixture = Fixture { dir };
        fixture.set_times();
        fixture
    }

    fn source() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture")
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    /// Puts every run's json at the second `mtimes.txt` names for it, which is what makes the clock
    /// matching in the job log a test rather than an accident of when the tree was checked out.
    fn set_times(&self) {
        for line in fs::read_to_string(Self::source().join("mtimes.txt")).unwrap().lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let (Some(run), Some(secs)) = (fields.next(), fields.next()) else {
                panic!("mtimes.txt names a run and a second on every line, not {line:?}");
            };
            let path = self.dir.join(run).join("json");
            let json = fs::read_dir(path)
                .unwrap_or_else(|e| panic!("{run}: {e}"))
                .map(|e| e.unwrap().path())
                .find(|p| p.extension().is_some_and(|e| e == "json"))
                .unwrap_or_else(|| panic!("{run} holds no json"));
            let when = SystemTime::UNIX_EPOCH + Duration::from_secs(secs.trim().parse().unwrap());
            fs::File::options()
                .write(true)
                .open(&json)
                .and_then(|f| f.set_times(fs::FileTimes::new().set_modified(when)))
                .unwrap_or_else(|e| panic!("{}: {e}", json.display()));
        }
    }

    fn output(&self, args: &[&str]) -> (Vec<u8>, String) {
        let made = Command::new(TOOL).arg("--base").arg(self.dir()).args(args).output().unwrap();
        (made.stdout, String::from_utf8_lossy(&made.stderr).into_owned())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in from.read_dir()? {
        let entry = entry?;
        let name = entry.file_name();
        let target = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[test]
fn the_tables_are_the_pythons_byte_for_byte() {
    let fixture = Fixture::prepare("tables");
    let want = fs::read(Fixture::source().join("analyse.expected.txt")).unwrap();
    let (stdout, stderr) = fixture.output(&["m1", "m3", "m4", "matrix.log"]);
    assert_eq!(stdout, want, "the tables differ from the retired Python's, byte for byte");
    // Two runs print a dash in those columns - the one whose json falls outside every block, and the one
    // whose block names a different cell - and both are said once, on stderr, where they cannot change a
    // byte of the tables.
    assert_eq!(
        stderr.trim(),
        "scroll-bench-tables: 2 of 10 runs have no load line, so their load and wall columns read -: \
         m1/wiki-s2-rust-vsync-r2, m4/yt-s2-swgl-vsync-r2"
    );
}

#[test]
fn a_job_log_can_sit_anywhere_in_the_arguments_but_the_directories_cannot() {
    let fixture = Fixture::prepare("order");
    let (one, _) = fixture.output(&["m1", "m3", "m4", "matrix.log"]);
    let (two, _) = fixture.output(&["matrix.log", "m1", "m3", "m4"]);
    assert_eq!(one, two, "a log is only a pool of blocks; where it was named cannot show in a table");

    // The runs of one cell are pooled in the order their directories were read, and two columns report that
    // order rather than a statistic: the `frames per run` list of section 2 and the async line section 4
    // quotes from the first run of the cell. The analysis works the same way, so this is not a difference to
    // fix but the reason the reproduce command names its matrices in the order it does.
    let (three, _) = fixture.output(&["m4", "m1", "m3", "matrix.log"]);
    assert_ne!(one, three, "the order of the directories is the order the rows pool them in");

    // The one row that carries that order, taken from the Python's own file rather than typed here.
    let cell = "| long | 1 | rust | fast | 2 | m3,m4 |";
    let line = |text: &[u8]| {
        String::from_utf8(text.to_vec())
            .unwrap()
            .lines()
            .find(|l| l.starts_with(cell))
            .unwrap_or_else(|| panic!("no section 2 row for {cell}"))
            .to_string()
    };
    let want = fs::read_to_string(Fixture::source().join("analyse.expected.txt")).unwrap();
    let want = want.lines().find(|l| l.starts_with(cell)).unwrap().to_string();
    assert_eq!(line(&one), want, "m3 before m4, as the arguments named them");
    assert_eq!(line(&two), want, "and the log's position changes nothing");
    assert_eq!(
        line(&three).replace("m4:60,m3:60", "m3:60,m4:60"),
        want,
        "the reversed arguments give the same row with only its per-run order turned round"
    );
    assert_ne!(line(&three), want);
}

#[test]
fn a_missing_run_directory_is_an_error_and_says_so() {
    let fixture = Fixture::prepare("missing");
    let made = Command::new(TOOL)
        .arg("--base")
        .arg(fixture.dir())
        .arg("m9")
        .output()
        .unwrap();
    assert!(!made.status.success(), "a typo in an argument must not print an empty report");
    assert!(
        String::from_utf8_lossy(&made.stderr).contains("no run directory at"),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
}

/// The shape the Mac leaves behind: one directory, a `json/` in it, and no job log beside it at all.
#[test]
fn a_run_dir_of_its_own_is_read_and_reports_no_load() {
    let fixture = Fixture::prepare("flat");
    let flat = fixture.dir().join("mac-out");
    fs::create_dir_all(flat.join("json")).unwrap();
    let one = fixture.dir().join("m4/yt-s2-swgl-vsync-r2/json");
    let source = fs::read_dir(one)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .unwrap();
    fs::copy(&source, flat.join("json/yt-s2-swgl-vsync-r2.json")).unwrap();

    let (stdout, stderr) = fixture.output(&["mac-out"]);
    let text = String::from_utf8(stdout).unwrap();
    let first = text.lines().next().unwrap();
    assert_eq!(first, "1 runs read, 0 matched to a load line by the clock, 0 whose block disagreed with the directory");
    assert!(stderr.contains("1 of 1 runs have no load line"), "{stderr}");
    // The row is the yt cell's, named by the json file rather than a directory around it.
    let row = text.lines().find(|l| l.starts_with("| yt |")).expect("the yt row is in the tables");
    let cells: Vec<&str> = row.split('|').map(str::trim).collect();
    assert_eq!(
        (cells[1], cells[2], cells[3], cells[4], cells[21], cells[22], cells[23]),
        ("yt", "2", "swgl", "vsync", "-", "-", "mac-out")
    );
}

#[test]
fn help_is_printed_and_nothing_else_happens() {
    let made = Command::new(TOOL).arg("--help").output().unwrap();
    assert!(made.status.success());
    let text = String::from_utf8(made.stdout).unwrap();
    assert!(text.contains("USAGE:") && text.contains("--base DIR"), "{text}");
    assert!(String::from_utf8_lossy(&made.stderr).is_empty());
}
