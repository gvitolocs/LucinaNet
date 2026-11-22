//! What this build says about the machine it runs on: one table, keyed by the build target (`target_os`,
//! `target_arch`), read by the User-Agent and by the fork's `navigator.platform` (preference `navigator_platform`,
//! set in `lucinanet-servo`'s `engine.rs`). Nothing else in the tree may spell a platform token (the scan test below).
//!
//! The User-Agent keeps Chromium's shape ("Mozilla/5.0 (<token>) AppleWebKit/537.36 ... Safari/537.36 LucinaNet/0.1");
//! only the token is the machine's own. macOS uses Chromium's frozen macOS conventions ("Macintosh; Intel Mac OS X
//! 10_15_7", platform "MacIntel"), which describe macOS on both Intel and Apple silicon; Linux gives the arch.
//! A target not in the table gets a truthful fallback built from its own os and arch names, never "Mac" or "Linux".

use std::sync::LazyLock;

/// The pieces of identity derived from the OS and CPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformIdentity {
    /// The parenthesised platform token of the User-Agent.
    pub ua_token: String,
    /// `navigator.platform`.
    pub navigator_platform: String,
    /// The `accessKeyLabel` prefix for the accesskey chord (the fork derives it from `target_os` as well).
    pub access_key_chord: &'static str,
}

/// The identity of the target `os`/`arch` (the values of `std::env::consts::OS` and `ARCH`).
pub fn platform_identity(os: &str, arch: &str) -> PlatformIdentity {
    match os {
        "macos" => PlatformIdentity {
            ua_token: "Macintosh; Intel Mac OS X 10_15_7".to_owned(),
            navigator_platform: "MacIntel".to_owned(),
            access_key_chord: "\u{2303}\u{2325}",
        },
        "linux" => PlatformIdentity {
            ua_token: format!("X11; Linux {arch}"),
            navigator_platform: format!("Linux {arch}"),
            access_key_chord: "Alt+Shift+",
        },
        other => PlatformIdentity {
            ua_token: format!("{other}; {arch}"),
            navigator_platform: format!("{other} {arch}"),
            access_key_chord: "Alt+Shift+",
        },
    }
}

/// The User-Agent for `os`/`arch`.
pub fn user_agent_for(os: &str, arch: &str) -> String {
    let id = platform_identity(os, arch);
    format!("Mozilla/5.0 ({}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 LucinaNet/0.1", id.ua_token)
}

/// This build's identity.
pub fn host_identity() -> PlatformIdentity {
    platform_identity(std::env::consts::OS, std::env::consts::ARCH)
}

/// What both engines say they are. Chromium's form, because sites serve Chromium's form the working page.
pub static USER_AGENT: LazyLock<String> = LazyLock::new(|| user_agent_for(std::env::consts::OS, std::env::consts::ARCH));

#[cfg(test)]
mod tests {
    use super::*;

    const SUPPORTED: [(&str, &str); 4] = [("macos", "aarch64"), ("macos", "x86_64"), ("linux", "aarch64"), ("linux", "x86_64")];

    #[test]
    fn every_supported_target_agrees_with_itself_and_with_the_pair() {
        for (os, arch) in SUPPORTED {
            let id = platform_identity(os, arch);
            let ua = user_agent_for(os, arch);
            assert!(ua.contains(&format!("({})", id.ua_token)), "{os}/{arch}: {ua}");
            match os {
                "macos" => {
                    assert_eq!(id.ua_token, "Macintosh; Intel Mac OS X 10_15_7");
                    assert_eq!(id.navigator_platform, "MacIntel");
                    assert_eq!(id.access_key_chord, "\u{2303}\u{2325}");
                },
                _ => {
                    assert_eq!(id.ua_token, format!("X11; Linux {arch}"));
                    assert_eq!(id.navigator_platform, format!("Linux {arch}"));
                    assert_eq!(id.access_key_chord, "Alt+Shift+");
                },
            }
        }
    }

    #[test]
    fn an_unlisted_target_is_never_called_mac_or_linux() {
        let id = platform_identity("freebsd", "riscv64");
        assert_eq!(id.ua_token, "freebsd; riscv64");
        assert_eq!(id.navigator_platform, "freebsd riscv64");
        let ua = user_agent_for("freebsd", "riscv64");
        assert!(!ua.contains("Mac") && !ua.contains("Linux"), "{ua}");
    }

    #[test]
    fn the_shipped_user_agent_is_the_functions_answer_for_the_hosts_own_target() {
        assert_eq!(*crate::USER_AGENT, user_agent_for(std::env::consts::OS, std::env::consts::ARCH));
        assert_eq!(host_identity(), platform_identity(std::env::consts::OS, std::env::consts::ARCH));
    }

    /// A platform token typed anywhere but the table is a second source that can disagree with it.
    #[test]
    fn no_source_file_spells_a_platform_token_outside_the_table() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut seen = 0;
        for krate in ["lucinanet-ui", "lucinanet-servo"] {
            let mut stack = vec![crates.join(krate).join("src")];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("lucinanet-ui/src/platform.rs") {
                        let text = std::fs::read_to_string(&path).unwrap();
                        seen += 1;
                        for token in ["X11; Linux", "Macintosh;", "Intel Mac OS X", "\"MacIntel\""] {
                            assert!(!text.contains(token), "{} spells {token:?}; it belongs in platform.rs", path.display());
                        }
                    }
                }
            }
        }
        assert!(seen > 10, "the scan saw {seen} files");
    }
}
