/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The per-site zoom memory: which level a host was left at, and the default for the sites that
//! never had one.
//!
//! The key is the host (see [`site_key`]), so `http://` and `https://` and every port of a host share
//! one level, as in Chromium's host zoom map. The store keeps no rows for pages that cannot remember
//! a zoom, and no row whose level is the default, so nothing stale is left behind (PORTING.md).

use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::levels::{self, Percent, Profile, DEFAULT};

/// The file format this crate writes; `from_json` refuses anything else, per entry being lenient and
/// the file itself strict.
const VERSION: u32 = 1;

/// The key a site's zoom is stored under: the URL's host, lower-case, in IDNA (ASCII) form, with no
/// scheme and no port, and no `www.` stripping.
///
/// `None` for the pages that never remember a zoom: `about:`, `data:`, `file:` and `blob:`, and hosts
/// that are empty (Chromium keys those by the whole URL spec and Firefox has no domain to group them
/// under; either way there is no site to remember them by).
pub fn site_key(url: &url::Url) -> Option<String> {
    if matches!(url.scheme(), "about" | "data" | "file" | "blob") {
        return None;
    }
    let host = url.host_str()?;
    if host.is_empty() {
        return None;
    }
    // The URL parser has already lower-cased the host and turned non-ASCII labels into punycode, so
    // `host_str()` is the key as it is stored; two spellings of one host cannot reach this and differ.
    Some(host.to_owned())
}

/// Why a zoom store did not load.
#[derive(Debug, PartialEq, Eq)]
pub enum ZoomError {
    /// The text is not the file at all: unreadable JSON, or a missing or wrongly typed top-level field.
    Syntax(String),
    /// A file of another format version, which this code must not guess at.
    Version(u32),
}

/// The remembered zoom of each site, and the default for the ones that have none.
///
/// Levels are [`levels::clamp`]ed to the profile's range as they arrive, so a store built under one
/// profile and read under another never hands the engine a level it would refuse.
#[derive(Clone, Debug)]
pub struct SiteZoom {
    profile: Profile,
    default: Percent,
    sites: BTreeMap<String, Percent>,
}

/// The shape of [`SiteZoom::to_json`]: the format version, the default zoom and the host rows.
#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    default: Percent,
    /// Read as [`Value`] so one bad row costs that row and not the file.
    sites: BTreeMap<String, Value>,
}

impl SiteZoom {
    /// An empty store for `profile`, whose default zoom is [`DEFAULT`].
    pub fn new(profile: Profile) -> SiteZoom {
        SiteZoom { profile, default: DEFAULT, sites: BTreeMap::new() }
    }

    /// The remembered zoom of the site of `url`, or the default zoom setting for a site nobody has
    /// zoomed and for a page that cannot remember a zoom at all.
    pub fn get(&self, url: &url::Url) -> Percent {
        site_key(url).and_then(|host| self.sites.get(&host).copied()).unwrap_or(self.default)
    }

    /// Remember `percent` as the zoom of the site of `url`. Setting a site to the default level
    /// removes its row instead, as both browsers do: Chromium erases the host when the level equals
    /// the default (`content/browser/host_zoom_map_impl.cc` `SetZoomLevelForHostInternal` @e9a4673),
    /// Firefox drops the content preference on reset (`browser-fullZoom.js` `reset`/`_removePref`).
    /// A page with no site key remembers nothing.
    pub fn set(&mut self, url: &url::Url, percent: Percent) {
        let Some(host) = site_key(url) else {
            return;
        };
        let percent = levels::clamp(self.profile, percent);
        if percent == self.default {
            self.sites.remove(&host);
        } else {
            self.sites.insert(host, percent);
        }
    }

    /// Forget the zoom of one site, which sends it back to the default (Firefox's reset, Chromium's
    /// `RemoveZoomLevelForHost`).
    pub fn reset_site(&mut self, url: &url::Url) {
        if let Some(host) = site_key(url) {
            self.sites.remove(&host);
        }
    }

    /// Forget every site. The default zoom setting is not part of the rows and stays as it is.
    pub fn clear(&mut self) {
        self.sites.clear();
    }

    /// The "default zoom" setting, into the profile's range: the level of every site without a row.
    /// Rows already stored are left alone - Firefox applies a new global value only to sites with no
    /// preference of their own (`browser-fullZoom.js` `_onContentPrefChanged`), as Chromium's default
    /// zoom level does (`SetDefaultZoomLevel`).
    pub fn set_default(&mut self, percent: Percent) {
        self.default = levels::clamp(self.profile, percent);
    }

    /// The current default zoom setting. Ctrl 0 in the UI goes here, not to a bare 100, because the
    /// user may have set a different default.
    pub fn default_percent(&self) -> Percent {
        self.default
    }

    /// How many sites have a remembered zoom.
    pub fn len(&self) -> usize {
        self.sites.len()
    }

    /// Whether no site has one.
    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }

    /// The remembered sites, sorted by host.
    pub fn entries(&self) -> impl Iterator<Item = (&str, Percent)> {
        self.sites.iter().map(|(host, percent)| (host.as_str(), *percent))
    }

    /// The store as JSON: `{"version":1,"default":100,"sites":{"example.org":125}}`, keys sorted, so
    /// two stores holding the same thing write the same bytes and the profile file does not change
    /// when nothing else did.
    pub fn to_json(&self) -> String {
        let file = StoreFile {
            version: VERSION,
            default: self.default,
            sites: self
                .sites
                .iter()
                .map(|(host, percent)| (host.clone(), Value::from(*percent)))
                .collect(),
        };
        // Integers and host keys cannot fail to serialise; if serde_json ever does, an empty string
        // is what `from_json` refuses as `Syntax` - a lost file is loud, a panic in the UI thread is not.
        serde_json::to_string(&file).unwrap_or_default()
    }

    /// Read a store back, for `profile`. The file is strict - unreadable JSON is
    /// [`ZoomError::Syntax`], another version is [`ZoomError::Version`] - and each row is lenient: a
    /// level that is not an integer, that is 0, or whose host is not a lower-case ASCII host is
    /// dropped rather than failing the file, since one broken row says nothing about the rest.
    /// Levels are clamped to the profile, so a row from a wider profile lands inside this one.
    pub fn from_json(profile: Profile, text: &str) -> Result<SiteZoom, ZoomError> {
        let file: StoreFile =
            serde_json::from_str(text).map_err(|e| ZoomError::Syntax(e.to_string()))?;
        if file.version != VERSION {
            return Err(ZoomError::Version(file.version));
        }
        let mut store = SiteZoom::new(profile);
        store.set_default(file.default);
        for (host, value) in file.sites {
            if !host_is_storable(&host) {
                continue;
            }
            let Some(percent) = read_percent(profile, &value) else {
                continue;
            };
            store.sites.insert(host, percent);
        }
        Ok(store)
    }
}

/// Whether a stored host is what [`site_key`] writes: non-empty, lower-case, and only the characters
/// a serialised host holds - letters, digits, `.`, `-`, and the brackets and colons of an IPv6
/// literal. Anything else came from a hand-edited or damaged file, not from a URL.
fn host_is_storable(host: &str) -> bool {
    !host.is_empty()
        && host.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b':' | b'[' | b']')
        })
}

/// A row's level: a whole number that is not 0, clamped into the profile's range.
fn read_percent(profile: Profile, value: &Value) -> Option<Percent> {
    let whole = value.as_u64()?;
    if whole == 0 {
        return None;
    }
    if whole > Percent::MAX as u64 {
        return Some(levels::max(profile));
    }
    Some(levels::clamp(profile, whole as Percent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("the test URL is valid")
    }

    #[test]
    fn site_key_is_lowercase_idna_host() {
        // The host only: upper case folded, the port gone, and the scheme gone, so http and https of
        // one host share a row (Chromium's host map is keyed the same way).
        assert_eq!(site_key(&url("https://EXAMPLE.org:8443/x")).as_deref(), Some("example.org"));
        assert_eq!(site_key(&url("http://example.org/")).as_deref(), Some("example.org"));
        assert_eq!(
            site_key(&url("https://EXAMPLE.org:8443/x")),
            site_key(&url("http://example.org/"))
        );
        // IDNA: the parsed host is the ASCII form, which is what both browsers store.
        assert_eq!(site_key(&url("https://münchen.de/")).as_deref(), Some("xn--mnchen-3ya.de"));
        assert_eq!(site_key(&url("https://xn--mnchen-3ya.de/")).as_deref(), Some("xn--mnchen-3ya.de"));
    }

    #[test]
    fn site_key_none_for_about_data_file_blob() {
        for text in [
            "about:blank",
            "about:preferences",
            "data:text/html,<p>hi</p>",
            "file:///home/user/index.html",
            "blob:https://example.org/8a7c1f30-0000-0000-0000-000000000001",
        ] {
            assert_eq!(site_key(&url(text)), None, "{text} remembers no zoom");
        }
        // And nothing to key a hostless URL by at all.
        assert_eq!(site_key(&url("mailto:someone@example.org")), None);
    }

    #[test]
    fn site_key_keeps_www_distinct() {
        // No eTLD+1 folding: `www.example.org` is a host of its own, as in Chromium's map. Firefox's
        // content preferences would group it with `example.org` (PORTING.md).
        assert_eq!(site_key(&url("https://www.example.org/")).as_deref(), Some("www.example.org"));
        assert_ne!(site_key(&url("https://www.example.org/")), site_key(&url("https://example.org/")));
    }

    #[test]
    fn site_key_ip_literals_work() {
        // The IPv6 form keeps its brackets, which are what separates it from a label, and the parser
        // canonicalises it: lower case, longest run of zeros collapsed. Port 80 is http's default, so
        // it is not in the key either way.
        assert_eq!(site_key(&url("http://[::1]:80/")).as_deref(), Some("[::1]"));
        assert_eq!(site_key(&url("http://127.0.0.1/")).as_deref(), Some("127.0.0.1"));
        assert_eq!(site_key(&url("http://[2001:DB8::1]:8080/")).as_deref(), Some("[2001:db8::1]"));
        assert_ne!(site_key(&url("http://127.0.0.1/")), site_key(&url("http://[::1]/")));
    }

    #[test]
    fn site_zoom_set_get_across_schemes_and_ports() {
        let mut store = SiteZoom::new(Profile::Firefox);
        store.set(&url("https://example.org:8443/a"), 125);
        assert_eq!(store.get(&url("http://example.org/")), 125);
        assert_eq!(store.get(&url("https://example.org:9999/deep/path")), 125);
        assert_eq!(store.len(), 1);
        // A level outside the profile's range is clamped on the way in, never stored as it is.
        store.set(&url("https://wide.example/"), 9999);
        assert_eq!(store.get(&url("https://wide.example/")), 500);
        store.set(&url("https://narrow.example/"), 5);
        assert_eq!(store.get(&url("https://narrow.example/")), 20);
    }

    #[test]
    fn site_zoom_default_when_unset() {
        let store = SiteZoom::new(Profile::Chromium);
        assert_eq!(store.get(&url("https://never-zoomed.example/")), DEFAULT);
        assert_eq!(store.get(&url("https://never-zoomed.example/")), store.default_percent());
        assert!(store.is_empty());
        // A page with no key takes the default too, and gets no row.
        assert_eq!(store.get(&url("about:blank")), DEFAULT);
    }

    #[test]
    fn site_zoom_setting_the_default_removes_entry() {
        let mut store = SiteZoom::new(Profile::Firefox);
        store.set(&url("https://example.org/"), 125);
        assert_eq!(store.len(), 1);
        // Back to the default: no row is left holding a level the default already gives.
        store.set(&url("https://example.org/other"), 100);
        assert!(store.is_empty());
        assert_eq!(store.get(&url("https://example.org/")), 100);
        // The same against a default that is not 100.
        store.set_default(110);
        store.set(&url("https://example.org/"), 150);
        store.set(&url("https://example.org/"), 110);
        assert!(store.is_empty());
        assert_eq!(store.get(&url("https://example.org/")), 110);
    }

    #[test]
    fn site_zoom_changed_default_applies_to_unset_sites_only() {
        let mut store = SiteZoom::new(Profile::Chromium);
        store.set(&url("https://zoomed.example/"), 150);
        store.set_default(125);
        // The site with a row keeps it; the ones without follow the setting.
        assert_eq!(store.get(&url("https://zoomed.example/")), 150);
        assert_eq!(store.get(&url("https://untouched.example/")), 125);
        assert_eq!(store.default_percent(), 125);
        assert_eq!(store.len(), 1);
        // A default outside the range is clamped, and the rows are untouched by the change.
        store.set_default(9999);
        assert_eq!(store.default_percent(), 500);
        assert_eq!(store.get(&url("https://zoomed.example/")), 150);
    }

    #[test]
    fn site_zoom_reset_site() {
        let mut store = SiteZoom::new(Profile::Firefox);
        store.set(&url("https://one.example/"), 125);
        store.set(&url("https://two.example/"), 150);
        store.reset_site(&url("https://one.example/"));
        assert_eq!(store.get(&url("https://one.example/")), 100);
        assert_eq!(store.get(&url("https://two.example/")), 150);
        assert_eq!(store.len(), 1);
        // Forgetting a site that has nothing to forget changes nothing.
        store.reset_site(&url("about:blank"));
        store.reset_site(&url("https://three.example/"));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn site_zoom_clear() {
        let mut store = SiteZoom::new(Profile::Chromium);
        store.set(&url("https://one.example/"), 125);
        store.set(&url("https://two.example/"), 150);
        store.set_default(110);
        store.clear();
        assert!(store.is_empty());
        assert_eq!(store.get(&url("https://one.example/")), 110);
        // The default zoom setting is not a site row, so it survives the clear.
        assert_eq!(store.default_percent(), 110);
    }

    #[test]
    fn site_zoom_json_roundtrip() {
        let mut store = SiteZoom::new(Profile::Firefox);
        store.set(&url("https://example.org/"), 125);
        store.set(&url("https://www.example.org/"), 80);
        store.set(&url("https://münchen.de/"), 150);
        store.set_default(110);
        let text = store.to_json();
        let back = SiteZoom::from_json(Profile::Firefox, &text).expect("our own file loads");
        assert_eq!(back.default_percent(), 110);
        assert_eq!(back.get(&url("http://example.org:8080/")), 125);
        assert_eq!(back.get(&url("https://www.example.org/")), 80);
        assert_eq!(back.get(&url("https://xn--mnchen-3ya.de/")), 150);
        assert_eq!(back.get(&url("https://unset.example/")), 110);
        assert_eq!(back.entries().count(), 3);
    }

    #[test]
    fn site_zoom_json_is_deterministic() {
        // The same rows, inserted in the opposite order, and the same bytes out: keys sorted, fields
        // in the order of the format, so rewriting the file when nothing changed changes nothing.
        let mut forward = SiteZoom::new(Profile::Chromium);
        forward.set(&url("https://example.org/"), 125);
        forward.set(&url("https://aaa.example/"), 150);
        forward.set(&url("https://zzz.example/"), 90);
        let mut backward = SiteZoom::new(Profile::Chromium);
        backward.set(&url("https://zzz.example/"), 90);
        backward.set(&url("https://aaa.example/"), 150);
        backward.set(&url("https://example.org/"), 125);
        let expected = r#"{"version":1,"default":100,"sites":{"aaa.example":150,"example.org":125,"zzz.example":90}}"#;
        assert_eq!(forward.to_json(), expected);
        assert_eq!(backward.to_json(), expected);
    }

    #[test]
    fn site_zoom_json_drops_bad_entries() {
        // The file is fine; these rows are not, and each costs only itself.
        let text = r#"{"version":1,"default":100,"sites":{
            "good.example":125,
            "BAD.example":110,
            "zero.example":0,
            "fraction.example":1.5,
            "text.example":"125",
            "":90,
            "two words.example":150,
            "ünencoded.example":110
        }}"#;
        let store = SiteZoom::from_json(Profile::Firefox, text).expect("the file itself is valid");
        assert_eq!(store.default_percent(), 100);
        assert_eq!(store.len(), 1);
        assert_eq!(store.get(&url("https://good.example/")), 125);
        assert_eq!(store.get(&url("https://zero.example/")), 100);
        assert_eq!(store.get(&url("https://BAD.example/")), 100);
    }

    #[test]
    fn site_zoom_json_rejects_unknown_version() {
        let text = r#"{"version":2,"default":100,"sites":{"example.org":125}}"#;
        assert_eq!(
            SiteZoom::from_json(Profile::Firefox, text).err(),
            Some(ZoomError::Version(2)),
            "a file of another version is not this format"
        );
        // A version that is not a number at all cannot be reported as a version, so it is the syntax.
        assert!(matches!(
            SiteZoom::from_json(Profile::Firefox, r#"{"version":"two","default":100,"sites":{}}"#),
            Err(ZoomError::Syntax(_))
        ));
    }

    #[test]
    fn site_zoom_json_rejects_garbage() {
        for text in ["", "not json at all", "{\"version\":1,", "{}"] {
            assert!(
                matches!(SiteZoom::from_json(Profile::Firefox, text), Err(ZoomError::Syntax(_))),
                "{text:?} is not the file"
            );
        }
    }

    #[test]
    fn site_zoom_values_clamped_on_load() {
        let text =
            r#"{"version":1,"default":100,"sites":{"high.example":9999,"low.example":1,"far.example":70000}}"#;
        // Firefox allows 20..500, so a wide row lands on the top of it and a thin one on the bottom.
        let store = SiteZoom::from_json(Profile::Firefox, text).expect("the file is valid");
        assert_eq!(store.get(&url("https://high.example/")), 500);
        assert_eq!(store.get(&url("https://low.example/")), 20);
        assert_eq!(store.get(&url("https://far.example/")), 500);
        // The same file under Chromium, whose bottom is 25.
        let chromium = SiteZoom::from_json(Profile::Chromium, text).expect("the file is valid");
        assert_eq!(chromium.get(&url("https://low.example/")), 25);
        assert_eq!(chromium.get(&url("https://high.example/")), 500);
    }
}
