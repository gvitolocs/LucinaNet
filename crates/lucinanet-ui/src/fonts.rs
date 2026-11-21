/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Where the engines find the system's fonts, so that a page's characters have a face to be drawn in.
//!
//! Both engines take their whole font universe from one colon-separated variable, `LUCINANET_FONT_DIRS`, and neither
//! is good at guessing it. Servo's Linux list (`components/fonts/platform/freetype/font_list.rs` in the fork) walks
//! each directory recursively, but its built-in default names `/usr/share/fonts/dejavu` and `/usr/share/fonts/noto`,
//! and a Debian-layout system puts nothing there: an engine started without the variable sees the dozen DejaVu faces
//! and nothing else. Blitz reads the same variable but does not descend, so handed `/usr/share/fonts` it finds no file
//! at all.
//!
//! The names a character falls back to are, for the most part, already right. Servo tries "Noto Color Emoji" for a
//! character that wants emoji presentation, "Noto Sans" and a per-script "Noto Sans <Script>", "Noto Sans CJK
//! JP/SC/KR", FreeSerif and Symbola; on macOS CoreText resolves Apple's names (Apple Color Emoji, Hiragino Sans,
//! Songti SC, Apple SD Gothic Neo, Geeza Pro). But a family that no scanned file declares cannot be chosen, and so a
//! missing directory and a missing glyph look identical on screen: one box per character. Naming Noto Color Emoji is
//! worth nothing on a host that has no emoji font installed anywhere in reach, which is what the build host is - the crab in
//! a YouTube title is U+1F980, and no face in `/usr/share/fonts` has it. What this module can decide is which
//! directories get scanned; what the distribution ships is not ours to choose.
//!
//! So the embedder's job is the directory list, and it has to be in place before the font thread reads it, for every
//! way into the program: `--render`, the desktop window, and Juno, where the `desktop` feature is not even compiled.
//! See docs/fonts.md.

use std::path::{Path, PathBuf};

/// The directories both engines scan, colon-separated - also what the reftest and raster-trace runners set by hand.
pub const FONT_DIRS_ENV: &str = "LUCINANET_FONT_DIRS";
/// Appended to whatever list is in effect, for a font being tried out or a prefix this OS keeps outside the usual
/// places. It only ever adds, so it is safe to leave set.
pub const FONT_DIRS_EXTRA_ENV: &str = "LUCINANET_FONT_DIRS_EXTRA";
/// The toolkit bar's font directory (`lucina-app`'s first choice), which is one directory rather than a list.
pub const FONT_DIR_ENV: &str = "LUCINA_FONT_DIR";
/// The face the bar is drawn in, and what it looks like when the directory is right.
pub const TOOLKIT_FACE: &str = "DejaVuSans.ttf";

/// The operating system's own font directories, most general first. `os` is "macos" or anything else - Linux, and
/// Juno, whose LucinaOS tree puts its Noto script faces in `/usr/share/fonts/truetype/noto` and the CJK collection in
/// `/usr/share/fonts/noto`, both reached by the recursive scan of `/usr/share/fonts`. `carried` is where the packager
/// ships fonts beside the binary, if it ships any.
///
/// A directory that does not exist stays in the answer: this list is a policy about where fonts live on this kind of
/// host, and the scan decides which of them are really there.
pub fn system_font_dirs(os: &str, home: Option<&Path>, carried: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if os == "macos" {
        // CoreText's four search domains, as Apple documents them. Unverified on this host; docs/fonts.md says so.
        dirs.push("/System/Library/Fonts".into());
        dirs.push("/System/Library/Fonts/Supplemental".into());
        dirs.push("/Library/Fonts".into());
        dirs.extend(home.map(|home| home.join("Library/Fonts")));
    } else {
        // fontconfig's own list, plus the per-user XDG directory that packagers and users fill.
        dirs.push("/usr/share/fonts".into());
        dirs.push("/usr/local/share/fonts".into());
        dirs.extend(home.map(|home| home.join(".local/share/fonts")));
        dirs.extend(home.map(|home| home.join(".fonts")));
    }
    dirs.extend(carried.map(PathBuf::from));
    dirs
}

/// Split a colon-separated font directory list the way the engines read it, without the empty fields a trailing
/// colon leaves behind.
pub fn split_dirs(list: &str) -> Vec<PathBuf> {
    list.split(':').filter(|dir| !dir.is_empty()).map(PathBuf::from).collect()
}

/// Join a list the way the engines read it.
pub fn join_dirs(dirs: &[PathBuf]) -> String {
    let mut out = String::new();
    for dir in dirs {
        if !out.is_empty() {
            out.push(':');
        }
        out.push_str(&dir.to_string_lossy());
    }
    out
}

/// The list to hand the engines. `wanted` is what the caller already set in `LUCINANET_FONT_DIRS`, and when it is set
/// it is the whole list: the reftest and raster-trace runners name a directory precisely so that no other face can
/// take part and change a metric. `extra` goes on the end either way, without repeats.
pub fn font_dirs(
    os: &str,
    home: Option<&Path>,
    carried: Option<&Path>,
    wanted: Option<&str>,
    extra: Option<&str>,
) -> Vec<PathBuf> {
    let mut dirs = match wanted {
        Some(wanted) => split_dirs(wanted),
        None => system_font_dirs(os, home, carried),
    };
    for dir in split_dirs(extra.unwrap_or("")) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// The directories worth looking in for one face, in order: each scanned directory's conventional DejaVu corners
/// first, then the directory itself, which is what a flat bundle or a hand-made test directory is.
pub fn face_dir_candidates(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in dirs {
        for child in ["truetype/dejavu", "dejavu", "noto"] {
            out.push(dir.join(child));
        }
        out.push(dir.clone());
    }
    out
}

/// The toolkit bar's font directory: the first of `dirs` that really holds `face`, judged by `have` so that the answer
/// is the same on the host that runs the test and the one the list is about.
pub fn toolkit_font_dir(dirs: &[PathBuf], face: &str, have: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    face_dir_candidates(dirs).into_iter().find(|dir| have(&dir.join(face)))
}

/// Where a packager would ship fonts beside this binary: `<prefix>/share/lucinanet/fonts` for an install tree whose
/// `bin` holds the executable, and `Contents/Resources/fonts` for a macOS app bundle, whose executable sits one
/// level under `Contents` too but names its level `MacOS`. No script in this repository fills either directory
/// today, so this is a question the layout answers, not a promise about a bundle: a missing directory is skipped.
pub fn install_font_dir(exe: Option<&Path>) -> Option<PathBuf> {
    let bin = exe?.parent()?;
    let prefix = bin.parent()?;
    if bin.file_name() == Some("MacOS".as_ref()) {
        return Some(prefix.join("Resources/fonts"));
    }
    Some(prefix.join("share/lucinanet/fonts"))
}

/// The directories this process should scan, from its own environment and this host.
pub fn default_font_dirs() -> Vec<PathBuf> {
    let os = if cfg!(target_os = "macos") { "macos" } else { "linux" };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let exe = std::env::current_exe().ok();
    let wanted = std::env::var_os(FONT_DIRS_ENV);
    let extra = std::env::var_os(FONT_DIRS_EXTRA_ENV);
    font_dirs(
        os,
        home.as_deref(),
        install_font_dir(exe.as_deref()).as_deref(),
        wanted.as_deref().and_then(|list| list.to_str()),
        extra.as_deref().and_then(|list| list.to_str()),
    )
}


/// Point the engines at the system's fonts. Every entry point does this first: the font thread reads the variable
/// once, never asks again, and a directory it missed is a page of boxes. A `LUCINANET_FONT_DIRS` the caller set is
/// kept as the whole list, so a test that names its own fonts still gets just those.
pub fn ensure_font_env() {
    let dirs = default_font_dirs();
    if dirs.is_empty() {
        return;
    }
    // SAFETY: every caller runs this before starting a thread, which is the only moment a reader exists.
    unsafe {
        std::env::set_var(FONT_DIRS_ENV, join_dirs(&dirs));
        if std::env::var_os(FONT_DIR_ENV).is_none() {
            let has_face = |path: &Path| path.is_file();
            if let Some(dir) = toolkit_font_dir(&dirs, TOOLKIT_FACE, has_face) {
                std::env::set_var(FONT_DIR_ENV, dir);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(dirs: &[PathBuf]) -> Vec<String> {
        dirs.iter().map(|dir| dir.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn linux_gets_the_whole_system_not_a_dejavu_guess() {
        assert_eq!(
            strings(&font_dirs("linux", Some(Path::new("/home/u")), None, None, None)),
            ["/usr/share/fonts", "/usr/local/share/fonts", "/home/u/.local/share/fonts", "/home/u/.fonts"]
        );
    }

    #[test]
    fn macos_gets_apples_four_domains_and_the_bundle() {
        assert_eq!(
            strings(&font_dirs(
                "macos",
                Some(Path::new("/Users/u")),
                Some(Path::new("/Apps/LucinaNet.app/Contents/Resources/fonts")),
                None,
                None
            )),
            [
                "/System/Library/Fonts",
                "/System/Library/Fonts/Supplemental",
                "/Library/Fonts",
                "/Users/u/Library/Fonts",
                "/Apps/LucinaNet.app/Contents/Resources/fonts"
            ]
        );
    }

    #[test]
    fn a_home_is_optional() {
        assert_eq!(strings(&font_dirs("linux", None, None, None, None)), ["/usr/share/fonts", "/usr/local/share/fonts"]);
    }

    #[test]
    fn a_named_list_is_the_whole_list() {
        // What `tests/wpt/run-reftests.sh` and `raster/capture-traces.sh` count on: their fonts, and nothing else.
        assert_eq!(strings(&font_dirs("linux", Some(Path::new("/home/u")), None, Some("/named/fonts:"), None)), ["/named/fonts"]);
    }

    #[test]
    fn extra_appends_to_whichever_list_is_in_effect() {
        assert_eq!(
            strings(&font_dirs("linux", None, None, None, Some("/opt/fonts"))),
            ["/usr/share/fonts", "/usr/local/share/fonts", "/opt/fonts"]
        );
        assert_eq!(
            strings(&font_dirs("linux", None, None, Some("/named/fonts"), Some("/opt/fonts:/usr/share/fonts"))),
            ["/named/fonts", "/opt/fonts", "/usr/share/fonts"],
            "what the caller named comes first, and the system directories it excluded stay excluded only if not asked for"
        );
        assert_eq!(
            strings(&font_dirs("linux", None, None, None, Some("/usr/local/share/fonts"))),
            ["/usr/share/fonts", "/usr/local/share/fonts"],
            "an already-listed directory is not listed twice"
        );
    }

    #[test]
    fn split_and_join_are_inverse() {
        assert_eq!(strings(&split_dirs("a::b:/x")), ["a", "b", "/x"]);
        assert!(split_dirs("").is_empty() && split_dirs(":").is_empty());
        let dirs = split_dirs("/a:/b");
        assert_eq!(join_dirs(&dirs), "/a:/b");
        assert_eq!(join_dirs(&[]), "");
    }

    #[test]
    fn the_bar_font_follows_the_face_that_is_actually_there() {
        // Synthetic hosts, because the real ones differ: Debian keeps DejaVu two levels under the scan root,
        // Juno's LucinaOS tree one, and a macOS bundle flat beside the executable.
        let at = |wanted: &'static str| move |path: &Path| path == Path::new(wanted);
        let root = split_dirs("/usr/share/fonts");
        assert_eq!(
            toolkit_font_dir(&root, "DejaVuSans.ttf", at("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")),
            Some(PathBuf::from("/usr/share/fonts/truetype/dejavu"))
        );
        assert_eq!(
            toolkit_font_dir(&root, "DejaVuSans.ttf", at("/usr/share/fonts/dejavu/DejaVuSans.ttf")),
            Some(PathBuf::from("/usr/share/fonts/dejavu"))
        );
        assert_eq!(
            toolkit_font_dir(&split_dirs("/App/fonts:/usr/share/fonts"), "DejaVuSans.ttf", at("/App/fonts/DejaVuSans.ttf")),
            Some(PathBuf::from("/App/fonts")),
            "a flat directory answers to the bare name, and is chosen in list order"
        );
        assert_eq!(toolkit_font_dir(&root, "DejaVuSans.ttf", at("/nowhere/DejaVuSans.ttf")), None);
        assert_eq!(toolkit_font_dir(&[], "DejaVuSans.ttf", |_| true), None, "nothing to choose from");
    }

    #[test]
    fn install_font_dir_reads_the_two_layouts() {
        assert_eq!(
            install_font_dir(Some(Path::new("/opt/lucinanet/bin/lucinanet-servo"))).map(|d| d.to_string_lossy().into_owned()),
            Some("/opt/lucinanet/share/lucinanet/fonts".into())
        );
        assert_eq!(
            install_font_dir(Some(Path::new("/Apps/LucinaNet.app/Contents/MacOS/lucinanet-servo")))
                .map(|d| d.to_string_lossy().into_owned()),
            Some("/Apps/LucinaNet.app/Contents/Resources/fonts".into())
        );
        assert_eq!(install_font_dir(None), None);
        assert_eq!(install_font_dir(Some(Path::new("lucinanet"))), None, "no parent, no prefix");
    }

}
