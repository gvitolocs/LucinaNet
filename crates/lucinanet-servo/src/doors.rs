//! the doors the environment could open in the shipped browser, now compiled in only with the `harness` feature
//! (docs/harness-doors.md). Each reads the environment through `get`, a lookup, so a test can stand in for it without
//! setting a variable. A build without the feature has no code that names these variables: the function is a constant `None`.

use std::path::PathBuf;

/// `LUCINANET_AX=<socket path>`: where the accessibility socket opens off the device. On Juno the socket opens without it,
/// at `/run/lucina/ax/<pid>.sock` (engine.rs `ax_wanted`); that stays.
#[cfg(feature = "harness")]
pub(crate) fn ax_socket_override(
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    get("LUCINANET_AX").map(PathBuf::from)
}

/// Without `harness` the variable is not read: the name is not in the program.
#[cfg(not(feature = "harness"))]
pub(crate) fn ax_socket_override(
    _get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    None
}

/// `LUCINANET_PREFS=name=value,name=value`: engine preferences set by name, as servoshell's `--pref`.
#[cfg(feature = "harness")]
pub(crate) fn extra_prefs(
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<(String, String)> {
    let text = get("LUCINANET_PREFS")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_default();
    text.split(',')
        .filter(|p| !p.is_empty())
        .map(|pref| {
            let (name, value) = pref.split_once('=').unwrap_or((pref, "true"));
            (name.to_owned(), value.to_owned())
        })
        .collect()
}

#[cfg(not(feature = "harness"))]
pub(crate) fn extra_prefs(
    _get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<(String, String)> {
    Vec::new()
}

/// `LUCINANET_USER_CSS=<file>` and `LUCINANET_USER_JS=<file>`: a user style sheet and a user script on every page. The
/// style sheet first, then the script.
#[cfg(feature = "harness")]
pub(crate) fn user_files(
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> (Option<String>, Option<String>) {
    let text = |name: &str| get(name).and_then(|v| v.into_string().ok());
    (text("LUCINANET_USER_CSS"), text("LUCINANET_USER_JS"))
}

#[cfg(not(feature = "harness"))]
pub(crate) fn user_files(
    _get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> (Option<String>, Option<String>) {
    (None, None)
}

/// The real environment, as a lookup.
pub(crate) fn environment(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variable set, to anything.
    #[cfg(not(feature = "harness"))]
    fn everything(_: &str) -> Option<std::ffi::OsString> {
        Some("/nonexistent/x".into())
    }

    fn only(
        name: &'static str,
        value: &'static str,
    ) -> impl Fn(&str) -> Option<std::ffi::OsString> {
        move |asked| (asked == name).then(|| value.into())
    }

    /// F4: `LUCINANET_AX`.
    #[cfg(not(feature = "harness"))]
    #[test]
    fn without_harness_the_ax_variable_opens_no_socket_path() {
        assert_eq!(ax_socket_override(everything), None);
    }

    /// F3: `LUCINANET_PREFS`.
    #[cfg(not(feature = "harness"))]
    #[test]
    fn without_harness_the_prefs_variable_sets_nothing() {
        assert!(
            extra_prefs(only("LUCINANET_PREFS", "dom_web_animations_enabled=true,x")).is_empty()
        );
        assert!(extra_prefs(everything).is_empty());
    }

    /// F1 and F2: `LUCINANET_USER_JS`, `LUCINANET_USER_CSS`.
    #[cfg(not(feature = "harness"))]
    #[test]
    fn without_harness_no_user_script_or_style_sheet_is_read() {
        assert_eq!(user_files(everything), (None, None));
        assert_eq!(
            user_files(only("LUCINANET_USER_JS", "planted/a.js")),
            (None, None)
        );
        assert_eq!(
            user_files(only("LUCINANET_USER_CSS", "planted/a.css")),
            (None, None)
        );
    }

    #[cfg(feature = "harness")]
    #[test]
    fn with_harness_the_ax_variable_names_the_socket() {
        assert_eq!(
            ax_socket_override(only("LUCINANET_AX", "/run/user/1/ax.sock")),
            Some("/run/user/1/ax.sock".into())
        );
        assert_eq!(ax_socket_override(|_| None), None);
    }

    #[cfg(feature = "harness")]
    #[test]
    fn with_harness_the_prefs_variable_sets_each_name() {
        let got = extra_prefs(only(
            "LUCINANET_PREFS",
            "dom_web_animations_enabled=false,flag,,a=b=c",
        ));
        assert_eq!(
            got,
            vec![
                ("dom_web_animations_enabled".to_owned(), "false".to_owned()),
                ("flag".to_owned(), "true".to_owned()),
                ("a".to_owned(), "b=c".to_owned()),
            ]
        );
        assert!(extra_prefs(|_| None).is_empty());
    }

    #[cfg(feature = "harness")]
    #[test]
    fn with_harness_the_user_files_are_named() {
        assert_eq!(
            user_files(only("LUCINANET_USER_JS", "planted/a.js")),
            (None, Some("planted/a.js".to_owned()))
        );
        assert_eq!(
            user_files(only("LUCINANET_USER_CSS", "planted/a.css")),
            (Some("planted/a.css".to_owned()), None)
        );
        assert_eq!(user_files(|_| None), (None, None));
    }
}
