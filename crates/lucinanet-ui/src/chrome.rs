//! Which top bar the window draws (`LUCINANET_CHROME`), how much of its left end the window's own buttons claim, and the
//! rule for when the blurred backdrop is given up for a plain tinted capsule. The bar's geometry is not here: that is
//! [`crate::onebar`], which both chromes place from; what the field says about an address is not either, and lives in
//! [`crate::address`]. Pure arithmetic; no drawing and no toolkit, so it is all unit-tested and both chromes read the same
//! answers.

/// Which top bar the window draws. Read once at startup (`LUCINANET_CHROME`), never again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The toolkit's own plain bar (the default except on the macOS desktop build).
    Classic,
    /// The lucina-glass bar: one row of glass capsules.
    Glass,
}

/// The default per platform: glass on the macOS desktop build (the window its user looks at), classic on Juno and every other
/// build until the laptop measurement says otherwise.
pub const DEFAULT_MACOS: Mode = Mode::Glass;
pub const DEFAULT_OTHER: Mode = Mode::Classic;

/// The default for a build: `macos` is `cfg!(target_os = "macos")`, injected so the rule is plain testable code.
pub fn default_mode(macos: bool) -> Mode {
    if macos { DEFAULT_MACOS } else { DEFAULT_OTHER }
}

/// `glass` or `classic` (any case, spaces round it are ignored); unset or anything else is the platform's default.
pub fn parse_mode(value: Option<&str>, macos: bool) -> Mode {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("glass") => Mode::Glass,
        Some("classic") => Mode::Classic,
        _ => default_mode(macos),
    }
}

/// The most room, in points, the left inset may claim (window buttons are far narrower).
pub const MAX_LEFT_INSET: f32 = 400.0;

/// `LUCINANET_CHROME_LEFT_INSET`: points kept free at the bar's left end, for window buttons. Unset, not a number or negative
/// is 0; a number past [`MAX_LEFT_INSET`] is that.
pub fn parse_inset(value: Option<&str>) -> f32 {
    match value.and_then(|v| v.trim().parse::<f32>().ok()) {
        Some(v) if v.is_finite() && v > 0.0 => v.min(MAX_LEFT_INSET),
        _ => 0.0,
    }
}

/// What the window reads at startup.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub mode: Mode,
    /// Points free at the bar's left end, for window buttons.
    pub left_inset: f32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { mode: default_mode(cfg!(target_os = "macos")), left_inset: 0.0 }
    }
}

impl Settings {
    pub fn from_env() -> Settings {
        Settings {
            mode: parse_mode(std::env::var("LUCINANET_CHROME").ok().as_deref(), cfg!(target_os = "macos")),
            left_inset: parse_inset(std::env::var("LUCINANET_CHROME_LEFT_INSET").ok().as_deref()),
        }
    }
}

/// How long a blur may take, in milliseconds, before the bar is drawn on a plain tinted capsule instead (one frame at 60 Hz is
/// 16.7 ms; the bar has to leave room for the page). A starting value, unmeasured on Juno: see the glass chrome spec.
pub const BLUR_BUDGET_MS: f32 = 12.0;
/// After a blur ran over budget the next ones are skipped, and one in this many page changes tries again.
pub const REPROBE_EVERY: u32 = 8;

/// Whether to blur the backdrop now: always until one blur was over budget; then only every [`REPROBE_EVERY`]th change.
pub fn blur_now(last_blur_ms: Option<f32>, changes_since_slow: u32) -> bool {
    match last_blur_ms {
        None => true,
        Some(ms) if ms <= BLUR_BUDGET_MS => true,
        Some(_) => changes_since_slow >= REPROBE_EVERY,
    }
}

/// Whether the last blur is shown through the glass: only when it was within budget.
pub fn use_blur(last_blur_ms: Option<f32>) -> bool {
    last_blur_ms.is_some_and(|ms| ms <= BLUR_BUDGET_MS)
}

/// Rows the address bar's dropdown shows at once: `one-bar.md` stops the list at eight.
pub const DROPDOWN_ROWS: usize = 8;

/// What the row shows of an address while nobody is typing in it, for the tab chips and the site disc: the host, with its
/// port when there is one, and nothing that can be left off (`docs/design/one-bar.md`, "What the field shows"). An address
/// with no host - `about:`, `file:`, what the person typed that is not a URL - is shown whole.
///
/// The *field* no longer uses this: it shows `lucinanet-omnibox`'s display of the address, with the registrable domain the
/// engine supplies emphasised inside it ([`crate::address::field_display`]). These two are the two different questions the row
/// asks of an address: what site is this tab (a chip's initial, a disc's first letter) against what page is this field.
pub fn host_of(address: &str) -> String {
    match url::Url::parse(address).ok().filter(|u| u.host_str().is_some()) {
        Some(u) => match u.port() {
            Some(port) => format!("{}:{port}", u.host_str().unwrap_or("")),
            None => u.host_str().unwrap_or("").to_owned(),
        },
        None => address.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_and_inset_parse() {
        for macos in [false, true] {
            assert_eq!(parse_mode(Some("glass"), macos), Mode::Glass, "an explicit choice wins on both platforms");
            assert_eq!(parse_mode(Some(" Glass "), macos), Mode::Glass);
            assert_eq!(parse_mode(Some("CLASSIC"), macos), Mode::Classic);
            assert_eq!(parse_mode(None, macos), default_mode(macos));
            assert_eq!(parse_mode(Some(""), macos), default_mode(macos));
            assert_eq!(parse_mode(Some("frosted"), macos), default_mode(macos), "unknown is the platform default");
        }
        assert_eq!(default_mode(true), Mode::Glass, "macOS desktop: glass");
        assert_eq!(default_mode(false), Mode::Classic, "Juno and every other build: classic");
        assert_eq!(parse_inset(None), 0.0);
        assert_eq!(parse_inset(Some("76")), 76.0);
        assert_eq!(parse_inset(Some(" 76.5 ")), 76.5);
        assert_eq!(parse_inset(Some("-5")), 0.0);
        assert_eq!(parse_inset(Some("wide")), 0.0);
        assert_eq!(parse_inset(Some("NaN")), 0.0);
        assert_eq!(parse_inset(Some("1e9")), MAX_LEFT_INSET);
        assert_eq!(Settings::default().mode, default_mode(cfg!(target_os = "macos")), "the build's own default");
    }

    #[test]
    fn an_over_budget_blur_is_skipped_until_a_change_is_due_a_reprobe() {
        assert!(blur_now(None, 0), "the first blur always runs");
        assert!(blur_now(Some(9.0), 0), "one within budget runs");
        assert!(!blur_now(Some(30.0), 0), "the change after a slow one waits");
        assert!(!blur_now(Some(30.0), REPROBE_EVERY - 1));
        assert!(blur_now(Some(30.0), REPROBE_EVERY), "one in {REPROBE_EVERY} tries again");
        assert!(use_blur(Some(9.0)), "a blur within budget is shown");
        assert!(!use_blur(Some(30.0)), "one over it is not");
        assert!(!use_blur(None), "no blur is nothing to show");
    }
}
