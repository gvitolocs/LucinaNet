//! Tests for `sites.txt`, the list `suite.sh` walks. A malformed line is a
//! silently skipped site, so the shape of the file is worth checking.

use std::collections::HashSet;

/// `name url` pairs, comments and blank lines dropped.
fn entries() -> Vec<(String, String)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sites.txt");
    let text = std::fs::read_to_string(&path).expect("sites.txt is next to Cargo.toml");
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next().unwrap_or_default().to_string();
            let url = fields.next().unwrap_or_default().to_string();
            assert!(
                fields.next().is_none(),
                "line {} has more than a name and a url",
                line
            );
            (name, url)
        })
        .collect()
}

#[test]
fn every_entry_has_a_unique_safe_name_and_a_url() {
    let entries = entries();
    assert!(entries.len() >= 20, "expected at least 20 sites, got {}", entries.len());

    let mut names = HashSet::new();
    for (name, url) in &entries {
        assert!(
            name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "name {} must be file-name safe",
            name
        );
        assert!(names.insert(name), "site name {} appears twice", name);
        assert!(url.starts_with("https://"), "url {} must be https", url);
    }
}

#[test]
fn the_original_sites_are_still_listed() {
    let entries = entries();
    let find = |name: &str| {
        entries
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, url)| url.as_str())
    };
    assert_eq!(find("example"), Some("https://example.com"));
    assert_eq!(
        find("wikipedia"),
        Some("https://en.wikipedia.org/wiki/Asterinas")
    );
    assert_eq!(find("hn"), Some("https://news.ycombinator.com"));
    assert_eq!(find("github"), Some("https://github.com/servo/servo"));
    assert_eq!(find("bbc"), Some("https://www.bbc.com/news"));
    assert_eq!(
        find("mdn"),
        Some("https://developer.mozilla.org/en-US/docs/Web/JavaScript")
    );
    assert_eq!(find("youtube"), Some("https://www.youtube.com/"));
}
