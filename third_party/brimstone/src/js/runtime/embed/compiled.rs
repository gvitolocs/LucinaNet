//! Compiling scripts without running them, and parsing them off the context's thread.
//!
//! `compile_script` parses, analyzes and generates bytecode, reporting syntax errors at compile
//! time, and returns a `CompiledScript` heap item. `execute_compiled` runs it, any number of times.
//!
//! `parse_script_off_thread` runs the parser and the analyzer, which do not need a Context, on any
//! thread. The result owns the AST arena together with every `Rc` the AST refers to, so it can be
//! sent to the context's thread as one unit, where `compile_parsed_script` generates bytecode.

use std::{cell::Cell, rc::Rc, time::Duration, time::Instant};

use crate::{
    common::{options::Options, wtf_8::Wtf8String},
    parser::{ParsedProgram, source::Source},
    runtime::{
        Context, EvalResult, Handle, HeapItemKind, HeapPtr, Realm, Value,
        bytecode::{
            function::BytecodeFunction,
            generator::{BytecodeProgramGenerator, BytecodeScript},
        },
        embed::with_realm,
        error::{syntax_error, syntax_parse_error},
        gc::{HeapItem, HeapVisitor},
        global_names::GlobalNames,
        shape::Shape,
    },
};

/// A script compiled to bytecode but not yet run (SpiderMonkey's `JSScript` from a stencil).
/// A GC heap item: hold it in a handle or report it from a root.
#[repr(C)]
pub struct CompiledScript {
    shape: HeapPtr<Shape>,
    script_function: HeapPtr<BytecodeFunction>,
    global_names: HeapPtr<GlobalNames>,
    /// `BytecodeScript::program`: the address of the script's parsed program (not a pointer to
    /// follow), whose AST is dropped once the script has run.
    program: usize,
}

impl CompiledScript {
    fn new(cx: Context, script: BytecodeScript) -> EvalResult<Handle<CompiledScript>> {
        let mut compiled = cx.alloc_uninit::<CompiledScript>()?;

        compiled.shape = cx.shapes.get(HeapItemKind::CompiledScript);
        compiled.script_function = *script.script_function;
        compiled.global_names = *script.global_names;
        compiled.program = script.program;

        Ok(compiled.to_handle())
    }

    /// The realm the script was compiled for and runs in.
    pub fn realm(&self) -> Handle<Realm> {
        self.script_function.realm()
    }

    fn bytecode_script(&self) -> BytecodeScript {
        BytecodeScript {
            script_function: self.script_function.to_handle(),
            global_names: self.global_names.to_handle(),
            program: self.program,
        }
    }
}

impl HeapItem for CompiledScript {
    fn byte_size(_: HeapPtr<Self>) -> usize {
        size_of::<CompiledScript>()
    }

    fn visit_pointers(mut compiled: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        visitor.visit_pointer(&mut compiled.shape);
        visitor.visit_pointer(&mut compiled.script_function);
        visitor.visit_pointer(&mut compiled.global_names);
    }
}

/// Source text with the line number of its first line applied, by prefixing newlines.
fn source_with_line(source: &str, line: u32) -> String {
    let mut text = "\n".repeat(line.saturating_sub(1) as usize);
    text.push_str(source);
    text
}

/// Generate bytecode for a parsed script in a realm.
fn generate(
    cx: Context,
    realm: Handle<Realm>,
    program: &Rc<ParsedProgram>,
) -> EvalResult<Handle<CompiledScript>> {
    with_realm(cx, realm, |cx| {
        let script =
            match BytecodeProgramGenerator::generate_from_parsed_script_with_completion_value(
                cx, program, realm,
            ) {
                Ok(script) => script,
                Err(error) => return syntax_error(cx, &error.to_string()),
            };

        CompiledScript::new(cx, script)
    })
}

/// Time spent compiling the last script on this thread: parsing plus scope analysis, and bytecode
/// generation (the script's top level and its eagerly generated functions). For a script parsed off
/// the thread, `parse` is the time the parse took there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompileTimes {
    pub parse: Duration,
    pub generate: Duration,
}

thread_local! {
    static LAST_COMPILE_TIMES: Cell<CompileTimes> = const {
        Cell::new(CompileTimes { parse: Duration::ZERO, generate: Duration::ZERO })
    };
}

/// The compile times of the last script compiled on this thread (`compile_script`,
/// `compile_parsed_script`, `evaluate_script`).
pub fn last_compile_times() -> CompileTimes {
    LAST_COMPILE_TIMES.with(|times| times.get())
}

fn record_compile_times(parse: Duration, generate: Duration) {
    LAST_COMPILE_TIMES.with(|times| times.set(CompileTimes { parse, generate }));
}

/// Parse and compile a script for a realm without running it. Syntax errors are thrown here as
/// SyntaxError objects. `line` is the line number of the first line of `source`.
pub fn compile_script(
    cx: Context,
    realm: Handle<Realm>,
    source: &str,
    file_name: &str,
    line: u32,
) -> EvalResult<Handle<CompiledScript>> {
    // Errors are created in the target realm
    with_realm(cx, realm, |cx| {
        let text = Wtf8String::from_string(source_with_line(source, line));
        let source = match Source::new_for_string(file_name, text) {
            Ok(source) => Rc::new(source),
            Err(error) => return syntax_parse_error(cx, &error),
        };

        let parse_start = Instant::now();
        let program =
            match ParsedProgram::parse(source, cx.options.clone(), /* is_module */ false) {
                Ok(program) => Rc::new(program),
                Err(error) => return syntax_parse_error(cx, &error),
            };
        let parse = parse_start.elapsed();

        let generate_start = Instant::now();
        let script = generate(cx, realm, &program);
        record_compile_times(parse, generate_start.elapsed());
        script
    })
}

/// Run a compiled script in its realm and return its completion value. Can be called any number
/// of times; each run declares the script's globals again, like re-running the same source.
pub fn execute_compiled(cx: Context, script: Handle<CompiledScript>) -> EvalResult<Handle<Value>> {
    let realm = script.realm();
    // The VM drops the AST of the script's program once it has run (`VM::execute_script`)
    with_realm(cx, realm, |mut cx| cx.vm().execute_script(script.bytecode_script()))
}

/// A syntax error found while parsing off the context's thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OffThreadSyntaxError {
    /// The message without the "SyntaxError:" prefix, including the location.
    pub message: String,
}

/// A script parsed and analyzed on any thread, ready for bytecode generation on the context's
/// thread with `compile_parsed_script`.
pub struct ParsedScript {
    program: ParsedProgram,
    annex_b: bool,
    parse_time: Duration,
}

/// Parse and analyze a script without a Context, e.g. on a background thread. Annex B follows the
/// engine's default; `compile_parsed_script` parses again on the context's thread if the context
/// was configured differently.
pub fn parse_script_off_thread(
    source: &str,
    file_name: &str,
    line: u32,
) -> Result<ParsedScript, OffThreadSyntaxError> {
    let parse_start = Instant::now();
    let options = Rc::new(Options::default());
    let annex_b = options.annex_b;

    let text = Wtf8String::from_string(source_with_line(source, line));
    let source = Source::new_for_string(file_name, text)
        .map_err(|error| OffThreadSyntaxError { message: error.to_string_without_name() })?;

    let program = ParsedProgram::parse(Rc::new(source), options, /* is_module */ false)
        .map_err(|error| OffThreadSyntaxError { message: error.to_string_without_name() })?;

    Ok(ParsedScript { program, annex_b, parse_time: parse_start.elapsed() })
}

impl ParsedScript {
    fn source_text(&self) -> String {
        String::from_utf8_lossy(self.program.source().contents.as_bytes()).into_owned()
    }
}

/// Generate bytecode for a script parsed with `parse_script_off_thread`. Must be called on the
/// context's thread.
pub fn compile_parsed_script(
    cx: Context,
    realm: Handle<Realm>,
    parsed: ParsedScript,
) -> EvalResult<Handle<CompiledScript>> {
    if parsed.annex_b != cx.options.annex_b {
        // The context parses with different options, so parse again with them
        let file_name = parsed.program.source().file_path().to_owned();
        let text = parsed.source_text();
        return compile_script(cx, realm, &text, &file_name, 1);
    }

    let parse = parsed.parse_time;
    let generate_start = Instant::now();
    let script = generate(cx, realm, &Rc::new(parsed.program));
    record_compile_times(parse, generate_start.elapsed());
    script
}
