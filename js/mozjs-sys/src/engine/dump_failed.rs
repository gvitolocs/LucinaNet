/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `LUCINANET_JS_DUMP_FAILED=<dir>`: the source of every script the glue compiles or runs that fails with a SyntaxError, in
//! `<dir>/<pid>-<n>.js`, with a `<dir>/<pid>-<n>.txt` beside it saying what the glue knows about that script (its file, the
//! options' line and column, the message, whether it failed compiling or running, and the stack or position).
//!
//! Why: a real page reports `SyntaxError: redeclaration of \`searchParams\`` at ":0:0". The check is the fork's global
//! declaration instantiation (`realm.rs`'s `can_declare_lexical_names`, `can_declare_var_names`), which runs when the script
//! starts, so by the time Servo reports the error the compile that produced it is long gone and nothing names the script.
//!
//! The directory is read when a context is created (`cx::JS_NewContext`), like the other switches there. Unset or empty turns
//! everything here off: the entry points start with one atomic load, and no source text is ever written.
//!
//! Who reports: `script.rs` (`JS::Compile`, `JS::Evaluate`, `JS_ExecuteScript`, an off-thread stencil and its frontend error),
//! `env_chain.rs` (`JS::CompileFunction`) and `module.rs` (`JS::CompileModule`). `JS_ExecuteScript` is handed only a
//! `JSScript*`, so `record_compiled` keeps the text of the last few scripts compiled or instantiated on the thread and the run
//! path matches by address, as `script.rs`'s timing does; the collector moves scripts, and a script that has left the kept set
//! is reported with its error and no source. `eval` and the `Function` constructor compile inside the engine, which never hands
//! the text to the glue, so their syntax errors are not covered. A JSON module's error is `JSON.parse`'s, not a compile either.
//!
//! Caps: a script over `MAX_SOURCE_BYTES` keeps its `.js` unwritten (the `.txt` says so), and `MAX_DUMPS` dumps per process.
//! `<pid>-<n>` keeps the browser's content processes from overwriting each other.

use ::std::borrow::Cow;
use ::std::path::PathBuf;
use ::std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use ::std::sync::{LazyLock, Mutex};

use brimstone_core::runtime::embed;
use brimstone_core::runtime::eval_result::EvalError;
use brimstone_core::runtime::gc::{Escapable, HandleScope};
use brimstone_core::runtime::Context as Bs;

use crate::jsapi::*;

/// A script over this keeps its text unwritten: the flag is for a page's own scripts, not for a minified bundle's megabytes.
const MAX_SOURCE_BYTES: usize = 8 << 20;
/// Dumps per process, so a page that fails in a loop cannot fill the disk.
const MAX_DUMPS: usize = 200;
/// Compiled scripts whose text is kept for the call that runs them.
const KEEP_COMPILED: usize = 4;
/// The stack the `.txt` reports is cut here.
const STACK_BYTES: usize = 200;

/// Whether the flag names a directory: the switch read at context creation, kept for the one load that gates everything.
static ON: AtomicBool = AtomicBool::new(false);
static DIR: LazyLock<Mutex<Option<PathBuf>>> = LazyLock::new(Default::default);
/// This process's dump counter, which is also its cap.
static DUMPS: AtomicUsize = AtomicUsize::new(0);

/// Whether the failure was compiling the script or running it: the redeclaration of a global name is only found when it runs.
#[derive(Clone, Copy)]
pub enum Phase {
    Compile,
    Run,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Compile => "compile",
            Phase::Run => "run",
        }
    }
}

/// What the compile options said about the script.
#[derive(Clone)]
pub struct Where<'a> {
    pub file: Cow<'a, str>,
    pub line: u32,
    pub column: u32,
}

/// The options as a `Where`.
pub unsafe fn at<'a>(options: *const JS::ReadOnlyCompileOptions) -> Where<'a> {
    let (file, line) = unsafe { super::script::file_and_line(options) };
    Where { file, line, column: column_of(options) }
}

impl<'a> Where<'a> {
    /// The same, owning its file name: what a script keeps between being compiled and being run.
    pub fn owned(&self) -> Where<'static> {
        Where { file: Cow::Owned(self.file.as_ref().to_owned()), line: self.line, column: self.column }
    }
}

fn column_of(options: *const JS::ReadOnlyCompileOptions) -> u32 {
    if options.is_null() {
        return 0;
    }
    let column = unsafe { (*options).column };
    column._base
}

/// What is kept about a compiled script, so that `JS_ExecuteScript` can name what it is about to run.
#[derive(Clone)]
struct Kept {
    script: usize,
    text: String,
    at: Where<'static>,
}

thread_local! {
    static KEPT: ::std::cell::RefCell<Vec<Kept>> = const { ::std::cell::RefCell::new(Vec::new()) };
}

/// The `LUCINANET_JS_DUMP_FAILED` directory, read when a context is created (`cx::JS_NewContext`). An empty value is off.
pub fn configure() {
    let dir = ::std::env::var_os("LUCINANET_JS_DUMP_FAILED").filter(|d| !d.is_empty()).map(PathBuf::from);
    ON.store(dir.is_some(), Ordering::Relaxed);
    *DIR.lock().unwrap_or_else(|e| e.into_inner()) = dir;
}

/// Whether anything here can write; the callers that would keep text for later ask first.
#[inline]
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Keeps the text of a script that was just compiled or instantiated, next to `script::note_compiled`, so that the call
/// which runs it can name it.
pub unsafe fn record_compiled(script: *mut JSScript, text: &str, at: &Where) {
    if !enabled() || script.is_null() || text.len() > MAX_SOURCE_BYTES {
        return;
    }
    let (at, text) = (at.owned(), text.to_owned());
    let _ = KEPT.try_with(move |kept| {
        let mut kept = kept.borrow_mut();
        kept.retain(|k| k.script != script as usize);
        kept.push(Kept { script: script as usize, text, at });
        while kept.len() > KEEP_COMPILED {
            kept.remove(0);
        }
    });
}

fn kept(script: *mut JSScript) -> Option<Kept> {
    KEPT.try_with(|kept| kept.borrow().iter().find(|k| k.script == script as usize).cloned()).ok().flatten()
}

/// What a failing script's error says.
struct Failing {
    /// As the report reads it: "SyntaxError: redeclaration of `x`".
    message: String,
    /// The newest frame captured with the error, `file:line:column`, if it has one.
    position: String,
    /// The stack captured when the error was made, newest frame first.
    stack: String,
}

/// `e`, if it is a SyntaxError, as the report would give it.
unsafe fn failing(bs: Bs, e: EvalError) -> Option<Failing> {
    // Escaped into the caller's handle scope: describing the error allocates, and a collection moves the thrown value.
    let v = embed::exception_value_opt(e)?.escape(bs);
    if !v.is_object() {
        return None;
    }
    let scope = HandleScope::enter(bs);
    let d = super::errors::describe(bs, v.as_raw_bits(), true);
    let frames = embed::error_stack_frames(bs, v.as_object(), 0).ok().flatten();
    scope.exit();
    let d = d?;
    if d.exn_type != crate::jsapi::JSExnType::JSEXN_SYNTAXERR as i16 {
        return None;
    }
    let position = if d.line == 0 && d.file.is_empty() { String::new() } else { format!("{}:{}:{}", d.file, d.line, d.column) };
    Some(Failing { message: d.message, position, stack: frames.map_or_else(String::new, |f| format_frames(&f)) })
}

/// The captured frames, as SpiderMonkey writes a stack: one per line, `name (file:line:column)`.
fn format_frames(frames: &[embed::CapturedStackFrame]) -> String {
    let mut out = String::new();
    for f in frames {
        let at = match (&f.file_name, f.line) {
            (Some(file), line) => format!("{}:{}:{}", file, line.unwrap_or(0), f.column.unwrap_or(0)),
            _ => "<native>".to_owned(),
        };
        out.push_str(&format!("{} ({at})\n", f.function_name.as_deref().unwrap_or("")));
    }
    out
}

/// A script the glue compiled or ran from `text` failed with `e`: dump it, if that is a SyntaxError.
pub unsafe fn report(bs: Bs, text: &str, at: &Where, phase: Phase, e: EvalError) {
    if !enabled() {
        return;
    }
    let Some(f) = (unsafe { failing(bs, e) }) else { return };
    write(Some(text), Some(at), phase, &f);
}

/// The same for a compiled script that failed as it ran, whose text is what `record_compiled` kept for it.
pub unsafe fn report_script(bs: Bs, script: *mut JSScript, phase: Phase, e: EvalError) {
    if !enabled() {
        return;
    }
    let Some(f) = (unsafe { failing(bs, e) }) else { return };
    match kept(script) {
        Some(k) => write(Some(&k.text), Some(&k.at), phase, &f),
        None => write(None, None, phase, &f),
    }
}

/// A script compiled off the thread (`script.rs`'s stencil path): its parse error is a message, not a thrown value, and the
/// engine only reports it later, when `JS::ConvertFrontendErrorsToRuntimeErrors` runs.
pub fn report_frontend(text: &str, file: &str, line: u32, column: u32, message: &str) {
    if !enabled() {
        return;
    }
    let f = Failing { message: message.to_owned(), position: String::new(), stack: String::new() };
    write(Some(text), Some(&Where { file: Cow::Borrowed(file), line, column }), Phase::Compile, &f);
}

/// `s` cut at `max` bytes, ending on a char boundary.
fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

/// One line, for the stderr report.
fn one_line(s: &str) -> String {
    s.replace(['\n', '\r', '\0'], " ")
}

/// Writes a dump: the `.js`, unless the source is unknown or over the cap, then the `.txt`, then the stderr line.
fn write(text: Option<&str>, at: Option<&Where>, phase: Phase, f: &Failing) {
    let n = DUMPS.fetch_add(1, Ordering::Relaxed);
    if n >= MAX_DUMPS {
        return;
    }
    let Some(dir) = DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
    let _ = ::std::fs::create_dir_all(&dir);
    let stem = dir.join(format!("{}-{}", ::std::process::id(), n));
    let (js, txt) = (stem.with_extension("js"), stem.with_extension("txt"));
    let (source, wrote_js) = match text {
        Some(t) if t.len() <= MAX_SOURCE_BYTES => match ::std::fs::write(&js, t) {
            Ok(()) => (format!("{} ({} bytes)", js.display(), t.len()), true),
            Err(e) => {
                eprintln!("lucinanet-js: LUCINANET_JS_DUMP_FAILED: cannot write {}: {e}", js.display());
                return;
            },
        },
        Some(t) => (format!("not written: {} bytes, over the cap of {} MiB", t.len(), MAX_SOURCE_BYTES >> 20), false),
        None => ("not written: the glue never saw this source (compiled elsewhere, or a collection moved the script)".to_owned(), false),
    };
    let file = at.map_or("(none)", |a| if a.file.is_empty() { "(none)" } else { a.file.as_ref() });
    let line = at.map_or_else(|| "-".to_owned(), |a| a.line.to_string());
    // A compile with the options' column unset is the normal case: the parser reports its own position, not the options.
    let column = at.map_or_else(|| "-".to_owned(), |a| if a.column == 0 { "0 (unset in the compile options)".to_owned() } else { a.column.to_string() });
    let position = if f.position.is_empty() { "(none)".to_owned() } else { f.position.clone() };
    // A compile-time error has no captured stack, only the position the parser stopped at.
    let stack = if f.stack.is_empty() { position.clone() } else { cut(&f.stack, STACK_BYTES) };
    let report = format!(
        "file: {file}\nline: {line}\ncolumn: {column}\nfailure: {}\nerror: {}\nsource: {source}\nposition: {position}\nstack: {stack}\n",
        phase.label(),
        f.message,
    );
    if let Err(e) = ::std::fs::write(&txt, report) {
        eprintln!("lucinanet-js: LUCINANET_JS_DUMP_FAILED: cannot write {}: {e}", txt.display());
        return;
    }
    eprintln!(
        "lucinanet-js: dumped failed script {} ({})",
        if wrote_js { js.display() } else { txt.display() },
        one_line(&f.message),
    );
}