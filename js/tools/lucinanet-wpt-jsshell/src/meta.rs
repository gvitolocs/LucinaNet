//! The `// META:` header of a `.any.js` test.
//!
//! WPT's own reader (`tools/manifest/sourcefile.py:42,59-72`) matches `^//\s*META:`
//! line by line and **stops at the first line that does not match**: the header is the
//! run of META comments at the very top of the file, and a file that opens with
//! anything else carries no metadata at all. `global=` is a comma-separated list
//! (`:137-149`), and `jsshell` is the variant whose file *is* the bare `.any.js`
//! (`:113`) - the only variant this runner executes.

/// The header entries this runner reads. Unknown keys are ignored, as upstream ignores
/// them (`variant=` and `timeout=` select HTML variants, which have no `.any.js`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Meta {
    /// The `global=` items, in declaration order.
    pub globals: Vec<String>,
    /// The `script=` values in declaration order. A leading `/` is relative to the WPT
    /// root, anything else to the directory of the test.
    pub scripts: Vec<String>,
    /// `title=`: the name testharness.js prefers over the filename (`get_title`).
    pub title: Option<String>,
}

impl Meta {
    /// Does the file declare the `jsshell` global? No `global=` entry means WPT's
    /// default of `window` and `dedicatedworker` (`sourcefile.py:130-134`), which is
    /// not jsshell.
    pub fn is_jsshell(&self) -> bool {
        self.globals.iter().any(|g| g == "jsshell")
    }
}

/// Read the META header of a test source.
pub fn parse(source: &str) -> Meta {
    let mut meta = Meta::default();
    for line in source.lines() {
        let Some((key, value)) = meta_line(line) else { break };
        match key {
            "global" => meta
                .globals
                .extend(value.split(',').map(str::trim).filter(|g| !g.is_empty()).map(str::to_string)),
            "script" => meta.scripts.push(value.to_string()),
            "title" => meta.title = Some(value.to_string()),
            _ => {}
        }
    }
    meta
}

/// `^//\s*META:\s*(\w*)=(.*)$` as a matcher: the key is word characters, the value runs
/// to the end of the line. `str::lines` has already dropped a trailing `\r`.
fn meta_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("//")?.trim_start();
    let rest = rest.strip_prefix("META:")?.trim_start();
    let eq = rest.find('=')?;
    let (key, value) = rest.split_at(eq);
    if !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some((key, &value[1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_the_run_of_meta_lines_at_the_top() {
        let meta = parse(
            "// META: global=window,dedicatedworker,jsshell\n\
             // META: script=/wasm/jsapi/assertions.js\n\
             \n\
             // META: script=/never/read.js\n\
             test(() => {}, \"x\");\n",
        );
        assert_eq!(meta.globals, vec!["window", "dedicatedworker", "jsshell"]);
        // The blank line ended the header, so the second `script=` is not metadata.
        assert_eq!(meta.scripts, vec!["/wasm/jsapi/assertions.js"]);
    }

    #[test]
    fn a_file_that_does_not_open_with_meta_has_no_header() {
        let meta = parse(
            "// Test that wasm: cases should fail.\n\
             promise_test(async (t) => {}, \"x\");\n\
             // META: global=jsshell\n",
        );
        assert_eq!(meta, Meta::default());
        assert!(!meta.is_jsshell());
    }

    #[test]
    fn jsshell_is_found_in_any_position() {
        assert!(parse("// META: global=window,worker,jsshell\n").is_jsshell());
        assert!(parse("// META: global=jsshell\n").is_jsshell());
        assert!(!parse("// META: global=window,dedicatedworker\n").is_jsshell());
        assert!(!parse("// META: global=\n").is_jsshell());
    }

    #[test]
    fn scripts_keep_their_order_and_slashes() {
        let meta = parse(
            "// META: global=jsshell\n\
             // META: script=/resources/WebIDLParser.js\n\
             // META: script=../resources/load_wasm.js\n\
             // META: title=Names\n",
        );
        assert_eq!(
            meta.scripts,
            vec!["/resources/WebIDLParser.js", "../resources/load_wasm.js"]
        );
        assert_eq!(meta.title.as_deref(), Some("Names"));
    }

    #[test]
    fn a_value_may_contain_an_equals_sign() {
        let meta = parse("// META: script=/x?a=b.js\n");
        assert_eq!(meta.scripts, vec!["/x?a=b.js"]);
    }
}
