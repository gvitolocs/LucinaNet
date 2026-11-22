//! The per-site zoom the engine keeps: `lucinanet-zoom`'s rules over the profile's `zoom.json`
//! (the shortcuts/zoom wiring spec). Nothing here touches Servo, so the glue is tested on its own; the engine
//! asks [`ZoomState`] which level a page gets and applies it with `WebView::set_page_zoom`.

use std::io::Write;
use std::path::{Path, PathBuf};

use lucinanet_ui::ZoomStep;
use lucinanet_zoom::{Percent, Profile, SiteZoom};
use url::Url;

/// Firefox's table and rules, as the shortcuts follow Firefox's keys (`lucinanet_ui::keymap::PROFILE`).
pub const PROFILE: Profile = Profile::Firefox;

/// The most sites remembered. A site past it still zooms, for as long as its tab stays there; it is just not kept.
/// Firefox and Chromium keep every row; a 4 GB laptop with no swap keeps the file small and the load bounded.
pub const MAX_SITES: usize = 2000;

/// The largest `zoom.json` read. 2000 rows are about 60 KB; a bigger file is not ours.
const MAX_FILE: u64 = 1 << 20;

/// The file's name in the profile directory.
const FILE: &str = "zoom.json";

#[derive(Clone)]
pub struct ZoomState {
    sites: SiteZoom,
    /// `<profile>/zoom.json`; None with `LUCINANET_PROFILE_DIR=none`, and then nothing is ever written.
    file: Option<PathBuf>,
}

impl ZoomState {
    /// The remembered zooms of the profile in `dir`, or none if there is no profile or no usable file. A file that
    /// cannot be read is left where it is and replaced by the next change; the browser starts at 100 %.
    pub fn load(dir: Option<&Path>) -> ZoomState {
        let file = dir.map(|dir| dir.join(FILE));
        let sites = file.as_deref().and_then(read).unwrap_or_else(|| SiteZoom::new(PROFILE));
        ZoomState { sites: bounded(sites), file }
    }

    /// A state that keeps nothing on disk, for tests and `LUCINANET_PROFILE_DIR=none`.
    pub fn in_memory() -> ZoomState {
        ZoomState { sites: SiteZoom::new(PROFILE), file: None }
    }

    /// The level a page at `url` is shown at: its host's row, else the default.
    pub fn percent_for(&self, url: &Url) -> Percent {
        self.sites.get(url)
    }

    /// One zoom step on a page at `url` that is shown at `current` now; returns the new level. In and out step from
    /// `current` by the Firefox table; reset goes to the default and forgets the site (`SiteZoom::default_percent`,
    /// not 100: the default is a setting). The row is saved when it changed.
    pub fn step(&mut self, url: &Url, current: Percent, step: ZoomStep) -> Percent {
        let before = self.sites.clone();
        let new = match step {
            ZoomStep::In => lucinanet_zoom::zoom_in(PROFILE, current),
            ZoomStep::Out => lucinanet_zoom::zoom_out(PROFILE, current),
            ZoomStep::Reset => self.sites.default_percent(),
        };
        match step {
            ZoomStep::Reset => self.sites.reset_site(url),
            _ if self.has_room_for(url) => self.sites.set(url, new),
            _ => {},
        }
        if self.sites.to_json() != before.to_json() {
            if let Err(error) = self.save() {
                eprintln!("lucinanet: zoom not saved: {error}");
            }
        }
        new
    }

    /// Whether `url`'s host can have a row: it has one already, or the file is not full.
    fn has_room_for(&self, url: &Url) -> bool {
        self.sites.len() < MAX_SITES || self.sites.get(url) != self.sites.default_percent()
    }

    /// The rows now, as the file would hold them.
    pub fn len(&self) -> usize {
        self.sites.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }

    fn save(&self) -> std::io::Result<()> {
        match &self.file {
            Some(path) => write_atomic(path, self.sites.to_json().as_bytes()),
            None => Ok(()),
        }
    }
}

/// The store from `path`, or None when there is no file, it is too big, or it is not a zoom file.
fn read(path: &Path) -> Option<SiteZoom> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        eprintln!("lucinanet: {} ignored: not a file of at most {MAX_FILE} bytes", path.display());
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    match SiteZoom::from_json(PROFILE, &text) {
        Ok(sites) => Some(sites),
        Err(error) => {
            eprintln!("lucinanet: {} ignored: {error:?}", path.display());
            None
        },
    }
}

/// The first `MAX_SITES` rows (by host), for a file somebody filled past what the engine writes.
fn bounded(sites: SiteZoom) -> SiteZoom {
    if sites.len() <= MAX_SITES {
        return sites;
    }
    let mut kept = SiteZoom::new(PROFILE);
    kept.set_default(sites.default_percent());
    for (host, percent) in sites.entries().take(MAX_SITES) {
        // Stored hosts are the URL parser's own output, so this parses; one that did not is dropped.
        if let Ok(url) = Url::parse(&format!("http://{host}/")) {
            kept.set(&url, percent);
        }
    }
    kept
}

/// Write `bytes` to `path` so a crash leaves the old file or the new one, never half of either: a temp file in the
/// same directory, fsynced, renamed over, and the directory fsynced so the rename survives power loss (the
/// convention of Juno's ext4 patches 0033/0034: fsync the file, then its directory).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| FILE.into());
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        std::fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    /// A fresh directory under the target dir's temp area, removed at the end by the test.
    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Dir {
            let path = std::env::temp_dir().join(format!("lucinanet-zoom-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Dir(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn steps_follow_the_firefox_table_and_reset_forgets_the_site() {
        let mut zoom = ZoomState::in_memory();
        let page = url("https://example.org/a");
        assert_eq!(zoom.percent_for(&page), 100);
        assert_eq!(zoom.step(&page, 100, ZoomStep::In), 110);
        assert_eq!(zoom.percent_for(&page), 110);
        // Another page of the host, another scheme and port: the same row.
        assert_eq!(zoom.percent_for(&url("http://example.org:8080/b")), 110);
        assert_eq!(zoom.percent_for(&url("https://other.org/")), 100);
        assert_eq!(zoom.step(&page, 110, ZoomStep::Out), 100);
        // Back at the default there is no row left.
        assert!(zoom.is_empty());
        assert_eq!(zoom.step(&page, 100, ZoomStep::Out), 90);
        assert_eq!(zoom.step(&page, 90, ZoomStep::Reset), 100);
        assert_eq!(zoom.percent_for(&page), 100);
        assert!(zoom.is_empty());
        // The ends: nothing past 500 or below 20.
        assert_eq!(zoom.step(&page, 500, ZoomStep::In), 500);
        assert_eq!(zoom.step(&page, 20, ZoomStep::Out), 20);
    }

    #[test]
    fn pages_that_remember_nothing_still_step_but_leave_no_row() {
        let mut zoom = ZoomState::in_memory();
        for page in ["about:blank", "file:///tmp/a.html", "data:text/plain,hi"] {
            assert_eq!(zoom.step(&url(page), 100, ZoomStep::In), 110, "{page}");
            assert_eq!(zoom.percent_for(&url(page)), 100, "{page}");
        }
        assert!(zoom.is_empty());
    }

    #[test]
    fn a_change_survives_a_restart_and_leaves_one_file() {
        let dir = Dir::new("round-trip");
        let mut zoom = ZoomState::load(Some(&dir.0));
        assert!(zoom.is_empty());
        assert!(files(&dir.0).is_empty(), "loading writes nothing");
        zoom.step(&url("https://example.org/"), 100, ZoomStep::In);
        zoom.step(&url("https://wikipedia.org/x"), 100, ZoomStep::Out);
        assert_eq!(files(&dir.0), ["zoom.json"], "no temp file is left behind");

        let again = ZoomState::load(Some(&dir.0));
        assert_eq!(again.percent_for(&url("https://example.org/other")), 110);
        assert_eq!(again.percent_for(&url("https://wikipedia.org/")), 90);
        assert_eq!(again.len(), 2);

        // Reset removes the host's row from the file too.
        let mut zoom = again;
        zoom.step(&url("https://example.org/"), 110, ZoomStep::Reset);
        let third = ZoomState::load(Some(&dir.0));
        assert_eq!(third.percent_for(&url("https://example.org/")), 100);
        assert_eq!(third.percent_for(&url("https://wikipedia.org/")), 90);
        assert_eq!(third.len(), 1);
    }

    #[test]
    fn no_change_writes_nothing() {
        let dir = Dir::new("no-change");
        let mut zoom = ZoomState::load(Some(&dir.0));
        // Reset on a site with no row: the store does not change, so no file appears.
        zoom.step(&url("https://example.org/"), 100, ZoomStep::Reset);
        assert!(files(&dir.0).is_empty());
        // At the bottom of the table, out again changes nothing: the file is the same bytes (and not rewritten).
        let page = url("https://small.example/");
        let mut level = 100;
        while level > 20 {
            level = zoom.step(&page, level, ZoomStep::Out);
        }
        let path = dir.0.join("zoom.json");
        let before = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(zoom.step(&page, 20, ZoomStep::Out), 20);
        assert!(!path.exists(), "an unchanged store is not written again");
        std::fs::write(&path, &before).unwrap();
        assert_eq!(ZoomState::load(Some(&dir.0)).percent_for(&page), 20);
    }

    #[test]
    fn without_a_profile_nothing_is_written() {
        // `LUCINANET_PROFILE_DIR=none` makes the engine pass None.
        let mut zoom = ZoomState::load(None);
        zoom.step(&url("https://example.org/"), 100, ZoomStep::In);
        assert_eq!(zoom.percent_for(&url("https://example.org/")), 110);
        assert!(zoom.file.is_none());
        // And the working directory has no zoom.json either.
        assert!(!Path::new("zoom.json").exists());
    }

    #[test]
    fn the_rows_are_bounded_in_memory_and_on_load() {
        let mut zoom = ZoomState::in_memory();
        for i in 0..MAX_SITES + 25 {
            zoom.step(&url(&format!("https://site{i}.example/")), 100, ZoomStep::In);
        }
        assert_eq!(zoom.len(), MAX_SITES);
        // A site past the bound still zooms this time, and is not kept.
        let late = url("https://late.example/");
        assert_eq!(zoom.step(&late, 100, ZoomStep::In), 110);
        assert_eq!(zoom.percent_for(&late), 100);
        // A host that has a row can still change when the store is full.
        let first = url("https://site0.example/");
        assert_eq!(zoom.step(&first, 110, ZoomStep::In), 120);
        assert_eq!(zoom.percent_for(&first), 120);
        assert_eq!(zoom.len(), MAX_SITES);

        // A file somebody filled past the bound loads cut to it.
        let dir = Dir::new("bounded");
        let mut big = SiteZoom::new(PROFILE);
        for i in 0..MAX_SITES + 500 {
            big.set(&url(&format!("https://h{i:05}.example/")), 150);
        }
        std::fs::write(dir.0.join("zoom.json"), big.to_json()).unwrap();
        let loaded = ZoomState::load(Some(&dir.0));
        assert_eq!(loaded.len(), MAX_SITES);
        assert_eq!(loaded.percent_for(&url("https://h00000.example/")), 150);
    }

    #[test]
    fn a_bad_or_oversized_file_starts_empty_and_is_replaced_by_the_next_change() {
        let dir = Dir::new("bad");
        let path = dir.0.join("zoom.json");
        std::fs::write(&path, "not json").unwrap();
        let mut zoom = ZoomState::load(Some(&dir.0));
        assert!(zoom.is_empty());
        std::fs::write(&path, vec![b' '; (MAX_FILE + 1) as usize]).unwrap();
        assert!(ZoomState::load(Some(&dir.0)).is_empty());
        zoom.step(&url("https://example.org/"), 100, ZoomStep::In);
        assert_eq!(ZoomState::load(Some(&dir.0)).percent_for(&url("https://example.org/")), 110);
        // A directory where the file should be is not a file.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(ZoomState::load(Some(&dir.0)).is_empty());
    }

    #[test]
    fn write_atomic_replaces_and_cleans_up() {
        let dir = Dir::new("atomic");
        let path = dir.0.join("f.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(files(&dir.0), ["f.json"]);
        // A directory that does not exist is an error and leaves nothing.
        assert!(write_atomic(&dir.0.join("missing").join("f.json"), b"x").is_err());
        assert_eq!(files(&dir.0), ["f.json"]);
    }
}
