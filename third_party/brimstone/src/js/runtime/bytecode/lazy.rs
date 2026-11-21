//! Lazy bytecode generation.
//!
//! Nested functions of a script or module get a stub `BytecodeFunction` when their parent is
//! generated. A stub carries everything needed before the function first runs (flags, name,
//! length, realm, source range) but no bytecode. Its body is generated from the AST on the first
//! call, and the stub then forwards to the generated function.
//!
//! The AST stays alive for as long as some stub of its program may still be generated: every
//! registry entry holds an `Rc` of the program's parse arena. An entry is removed when its stub is
//! generated or collected, so the arena is freed once the last stub of the program is gone.

use std::time::Instant;

use crate::parser::ParsedProgram;
use crate::runtime::{
    Context, Handle, HeapPtr,
    bytecode::{
        function::BytecodeFunction,
        generator::{BytecodeProgramGenerator, LazyFunctionInfo},
    },
    error::syntax_error,
    eval_result::EvalResult,
    gc::AnyHeapItem,
};

/// Counts of the functions compiled by a context, and the source bytes they span.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompileStats {
    /// Functions whose bytecode was generated when their program was compiled, including the
    /// toplevel of each program.
    pub eager_functions: u64,
    pub eager_source_bytes: u64,
    /// Functions created as lazy stubs.
    pub stub_functions: u64,
    pub stub_source_bytes: u64,
    /// Stubs whose bytecode was generated later, on their first call.
    pub lazy_generated_functions: u64,
    pub lazy_generated_source_bytes: u64,
    /// Stubs that have neither been generated nor collected yet.
    pub pending_stub_functions: u64,
}

struct LazyEntry {
    /// The stub. Held weakly: the entry is dropped when the stub is collected.
    stub: HeapPtr<BytecodeFunction>,
    info: LazyFunctionInfo,
}

/// What dropping ASTs and re-parsing lazy functions did (`Options::lazy_reparse`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReparseStats {
    /// Stubs detached from their AST.
    pub detached: u64,
    /// Stubs generated before their AST was dropped because they cannot be parsed again on
    /// their own.
    pub generated_before_drop: u64,
    /// Programs whose AST was dropped.
    pub programs_dropped: u64,
    /// Functions parsed again on their first call, the time it took, and failures.
    pub reparsed: u64,
    pub reparse_ns: u64,
    pub reparse_failed: u64,
    /// Verify mode: re-parsed functions whose bytecode differed from the original's.
    pub verify_mismatches: u64,
    /// Scope tree compactions.
    pub compactions: u64,
}

/// The stubs of a context that have not been generated yet, with what is needed to generate them.
#[derive(Default)]
pub struct LazyFunctions {
    entries: Vec<Option<LazyEntry>>,
    free_ids: Vec<u32>,
    pub stats: CompileStats,
    pub reparse: ReparseStats,
    /// Ids of stubs inserted while a re-parsed function is generated (`begin_collect`).
    collected: Vec<u32>,
    collecting: usize,
}

impl LazyFunctions {
    /// Register a stub and return its id, stored in the stub.
    pub(crate) fn insert(
        &mut self,
        stub: HeapPtr<BytecodeFunction>,
        info: LazyFunctionInfo,
    ) -> u32 {
        let entry = Some(LazyEntry { stub, info });

        let id = if let Some(id) = self.free_ids.pop() {
            self.entries[id as usize] = entry;
            id
        } else {
            self.entries.push(entry);
            (self.entries.len() - 1) as u32
        };
        if self.collecting > 0 {
            self.collected.push(id);
        }
        id
    }

    /// Start collecting the ids of inserted stubs; returns the mark to pass to `detach_collected`.
    pub(crate) fn begin_collect(&mut self) -> usize {
        self.collecting += 1;
        self.collected.len()
    }

    /// Remove the entry of a stub, returning its info.
    fn take(&mut self, id: u32) -> LazyFunctionInfo {
        let entry = self.entries[id as usize].take().unwrap();
        self.free_ids.push(id);
        entry.info
    }

    /// Number of stubs that have neither been generated nor collected.
    pub fn num_pending(&self) -> usize {
        self.entries.len() - self.free_ids.len()
    }

    /// The compile statistics of the context so far.
    pub fn compile_stats(&self) -> CompileStats {
        CompileStats {
            pending_stub_functions: self.num_pending() as u64,
            ..self.stats
        }
    }

    /// Update the stub pointers after a GC moved the heap, dropping the entries of collected stubs.
    /// `resolve` returns the new location of a live item, or None if it was collected.
    pub fn sweep(
        &mut self,
        resolve: impl Fn(HeapPtr<AnyHeapItem>) -> Option<HeapPtr<AnyHeapItem>>,
    ) {
        for (id, slot) in self.entries.iter_mut().enumerate() {
            if let Some(entry) = slot {
                match resolve(entry.stub.cast()) {
                    Some(new_location) => entry.stub = new_location.cast(),
                    None => {
                        *slot = None;
                        self.free_ids.push(id as u32);
                    }
                }
            }
        }
    }

    /// The programs kept alive by pending stubs, each once, with the bytes of their parse arenas
    /// that trimming can free: the whole arena, or with `reparse` only programs that still have
    /// their AST, and only its bytes (the scope tree stays).
    fn retained_programs(&self, reparse: bool) -> Vec<(*const ParsedProgram, usize)> {
        let mut programs: Vec<(*const ParsedProgram, usize)> = Vec::new();
        for entry in self.entries.iter().flatten() {
            let program = entry.info.program();
            let ptr = std::rc::Rc::as_ptr(program);
            if !programs.iter().any(|(p, _)| *p == ptr) {
                let bytes = if reparse {
                    program.ast_arena_bytes()
                } else {
                    program.arena_bytes()
                };
                programs.push((ptr, bytes));
            }
        }
        if reparse {
            programs.retain(|(_, bytes)| *bytes > 0);
        }
        programs
    }

    /// Bytes of the parse arenas that pending stubs keep alive and trimming can free (see
    /// `retained_programs`), each program counted once.
    pub fn retained_parse_bytes(&self) -> usize {
        self.retained_programs(false)
            .iter()
            .map(|(_, bytes)| bytes)
            .sum()
    }

    /// Like `retained_parse_bytes`, for a context that re-parses: only ASTs count.
    pub fn retained_ast_bytes(&self) -> usize {
        self.retained_programs(true)
            .iter()
            .map(|(_, bytes)| bytes)
            .sum()
    }

    /// Bytes of the scope tree arenas of programs with pending stubs.
    pub fn retained_scope_bytes(&self) -> usize {
        let mut seen: Vec<*const ParsedProgram> = Vec::new();
        let mut bytes = 0;
        for entry in self.entries.iter().flatten() {
            let program = entry.info.program();
            let ptr = std::rc::Rc::as_ptr(program);
            if !seen.contains(&ptr) {
                seen.push(ptr);
                bytes += program.scope_arena_bytes();
            }
        }
        bytes
    }

    /// The ids of the pending stubs of one program that are still attached to an AST.
    fn attached_ids_of(&self, program: *const ParsedProgram) -> Vec<u32> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(id, slot)| match slot {
                Some(entry)
                    if entry.info.is_attached()
                        && std::rc::Rc::as_ptr(entry.info.program()) == program =>
                {
                    Some(id as u32)
                }
                _ => None,
            })
            .collect()
    }

    /// The programs of pending stubs that still have their AST, each once.
    pub(crate) fn programs_with_ast(&self) -> Vec<*const ParsedProgram> {
        let mut programs = Vec::new();
        for entry in self.entries.iter().flatten() {
            let ptr = std::rc::Rc::as_ptr(entry.info.program());
            if entry.info.program().has_ast() && !programs.contains(&ptr) {
                programs.push(ptr);
            }
        }
        programs
    }

    /// The program with the given address, if a pending stub belongs to it.
    fn program_rc(&self, program: *const ParsedProgram) -> Option<std::rc::Rc<ParsedProgram>> {
        self.entries
            .iter()
            .flatten()
            .find(|entry| std::rc::Rc::as_ptr(entry.info.program()) == program)
            .map(|entry| entry.info.program().clone())
    }

    /// The ids of the pending stubs of one program.
    fn pending_ids_of(&self, program: *const ParsedProgram) -> Vec<u32> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(id, slot)| match slot {
                Some(entry) if std::rc::Rc::as_ptr(entry.info.program()) == program => {
                    Some(id as u32)
                }
                _ => None,
            })
            .collect()
    }

    /// The stub registered under `id`, if it still belongs to `program`.
    fn stub_of(&self, id: u32, program: *const ParsedProgram) -> Option<HeapPtr<BytecodeFunction>> {
        match self.entries.get(id as usize) {
            Some(Some(entry)) if std::rc::Rc::as_ptr(entry.info.program()) == program => {
                Some(entry.stub)
            }
            _ => None,
        }
    }
}

/// What a call of `trim_retained_programs` did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrimResult {
    /// Stubs generated by this call.
    pub generated: u64,
    /// Programs whose last stub was generated, so their parse arena was freed.
    pub freed_programs: u64,
    /// Parse arena bytes still kept alive by pending stubs when the call returned.
    pub retained_bytes: usize,
    /// True when the retained bytes are within the budget; false when the deadline came first.
    pub within_budget: bool,
}

/// Bound the memory that lazy stubs keep alive: while the parse arenas of pending stubs exceed
/// `budget_bytes`, generate every pending stub of the largest such program (its arena is freed with
/// its last stub), checking `deadline` between batches. Meant for idle time after a page load:
/// generation costs what lazy generation saved, so it should not run on the load's critical path.
/// A stub whose generation fails is left pending (its first call reports the error), so a program
/// with one is not freed; the loop moves on to the next program.
pub fn trim_retained_programs(
    cx: Context,
    budget_bytes: usize,
    deadline: Instant,
) -> EvalResult<TrimResult> {
    let mut result = TrimResult::default();
    let mut failed_programs: Vec<*const ParsedProgram> = Vec::new();

    let reparse = reparse_enabled(cx);
    loop {
        let programs = cx.lazy_functions.retained_programs(reparse);
        result.retained_bytes = programs.iter().map(|(_, bytes)| bytes).sum();
        if result.retained_bytes <= budget_bytes {
            result.within_budget = true;
            return Ok(result);
        }
        if Instant::now() >= deadline {
            return Ok(result);
        }

        let Some(&(program, _)) = programs
            .iter()
            .filter(|(p, _)| !failed_programs.contains(p))
            .max_by_key(|(_, bytes)| *bytes)
        else {
            return Ok(result);
        };

        // Dropping a program's AST frees it without generating anything
        if reparse {
            detach_program(cx, program).map_err(|error| {
                crate::runtime::error::syntax_error::<()>(cx, &error.to_string()).unwrap_err()
            })?;
            if cx
                .lazy_functions
                .retained_programs(true)
                .iter()
                .any(|(p, _)| *p == program)
            {
                failed_programs.push(program);
            } else {
                result.freed_programs += 1;
            }
            continue;
        }

        // Generating a stub creates stubs for its nested functions in the same program, so repeat
        // until the program has none left.
        let mut failed = false;
        loop {
            let ids = cx.lazy_functions.pending_ids_of(program);
            if ids.is_empty() {
                break;
            }
            let mut progressed = false;
            for (i, id) in ids.into_iter().enumerate() {
                // A GC during generation may move or collect stubs: look each one up again
                let Some(stub) = cx.lazy_functions.stub_of(id, program) else {
                    continue;
                };
                match ensure_generated(cx, stub.to_handle()) {
                    Ok(_) => {
                        result.generated += 1;
                        progressed = true;
                    }
                    Err(_) => failed = true,
                }
                if i % 64 == 63 && Instant::now() >= deadline {
                    result.retained_bytes = cx.lazy_functions.retained_parse_bytes();
                    return Ok(result);
                }
            }
            if failed || !progressed {
                break;
            }
        }

        if failed || !cx.lazy_functions.pending_ids_of(program).is_empty() {
            failed_programs.push(program);
        } else {
            result.freed_programs += 1;
        }
    }
}

/// Total time and count of lazy stub generations on this thread (first calls and trims).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LazyGenerateTime {
    pub total: std::time::Duration,
    pub functions: u64,
}

thread_local! {
    static LAZY_GENERATE_TIME: std::cell::Cell<LazyGenerateTime> = const {
        std::cell::Cell::new(LazyGenerateTime { total: std::time::Duration::ZERO, functions: 0 })
    };
}

/// Time spent generating lazy stubs on this thread so far.
pub fn lazy_generate_time() -> LazyGenerateTime {
    LAZY_GENERATE_TIME.with(|time| time.get())
}

/// Return the generated function for a function, generating the bytecode of a lazy stub if this
/// is its first use. Functions that are not stubs are returned unchanged.
pub fn ensure_generated(
    mut cx: Context,
    mut function: Handle<BytecodeFunction>,
) -> EvalResult<Handle<BytecodeFunction>> {
    if !function.is_lazy_stub() {
        return Ok(function);
    }

    if let Some(generated) = function.lazy_generated_ptr() {
        return Ok(generated.to_handle());
    }

    let id = function.lazy_id().unwrap();
    let info = cx.lazy_functions.take(id);

    // Generation failed before the program's AST was dropped: report that error again
    if let Some(message) = info.failed_message() {
        let message = message.to_owned();
        let new_id = cx.lazy_functions.insert(*function, info);
        function.set_lazy_id(new_id);
        return syntax_error(cx, &message);
    }

    let start = Instant::now();
    let result = BytecodeProgramGenerator::generate_lazy_function(cx, function, &info);
    let elapsed = start.elapsed();
    let _ = LAZY_GENERATE_TIME.try_with(|time| {
        let mut t = time.get();
        t.total += elapsed;
        t.functions += 1;
        time.set(t);
    });

    match result {
        Ok(generated) => {
            // The generation itself counted the stats in process_generated_function.
            function.set_lazy_generated(*generated);

            Ok(generated)
        }
        Err(error) => {
            // Keep the stub so that a later call reports the same error
            let new_id = cx.lazy_functions.insert(*function, info);
            function.set_lazy_id(new_id);

            syntax_error(cx, &error.to_string())
        }
    }
}

/// Generate the bytecode of every stub that has not been generated yet, including the stubs
/// created while doing so. Used before serializing a heap, since stubs refer to Rust data.
/// Generate the stubs pending now, but not the stubs that generating them creates.
pub fn generate_pending_round(cx: Context) -> EvalResult<()> {
    let ids: Vec<u32> = cx
        .lazy_functions
        .entries
        .iter()
        .enumerate()
        .filter_map(|(id, slot)| slot.as_ref().map(|_| id as u32))
        .collect();
    for id in ids {
        let stub = match cx.lazy_functions.entries.get(id as usize) {
            Some(Some(entry)) => entry.stub,
            _ => continue,
        };
        ensure_generated(cx, stub.to_handle())?;
    }
    Ok(())
}

pub fn generate_all_lazy_functions(cx: Context) -> EvalResult<()> {
    // In rounds: every stub pending at the start of a round, then the stubs that generating them
    // created. Generation may run a GC that moves the stubs, so each one is looked up again.
    loop {
        let ids: Vec<u32> = cx
            .lazy_functions
            .entries
            .iter()
            .enumerate()
            .filter_map(|(id, slot)| slot.as_ref().map(|_| id as u32))
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        for id in ids {
            let stub = match cx.lazy_functions.entries.get(id as usize) {
                Some(Some(entry)) => entry.stub,
                _ => continue,
            };
            ensure_generated(cx, stub.to_handle())?;
        }
    }
}

/// Whether this context drops ASTs and re-parses lazy functions.
pub fn reparse_enabled(cx: Context) -> bool {
    cx.options.lazy_functions && cx.options.lazy_reparse
}

/// Generate a pending stub now, while its AST is alive. On failure the stub is marked failed, so
/// that its first call reports the error after the AST is gone.
fn generate_before_drop(mut cx: Context, id: u32) {
    let Some(Some(entry)) = cx.lazy_functions.entries.get(id as usize) else {
        return;
    };
    let mut stub = entry.stub.to_handle();
    let mut info = cx.lazy_functions.take(id);
    cx.lazy_functions.reparse.generated_before_drop += 1;
    match BytecodeProgramGenerator::generate_lazy_function(cx, stub, &info) {
        Ok(generated) => stub.set_lazy_generated(*generated),
        Err(error) => {
            info.set_failed(error.to_string());
            let new_id = cx.lazy_functions.insert(*stub, info);
            stub.set_lazy_id(new_id);
        }
    }
}

/// Detach (or generate) the stubs inserted since `mark` (see `LazyFunctions::begin_collect`): the
/// stubs of functions nested in a re-parsed function, whose AST is about to be freed.
pub(crate) fn detach_collected(
    mut cx: Context,
    mark: usize,
    keep_original: bool,
) -> Result<(), crate::runtime::bytecode::generator::EmitError> {
    while cx.lazy_functions.collected.len() > mark {
        let id = cx.lazy_functions.collected.pop().unwrap();
        let lazy = &mut cx.lazy_functions;
        let Some(Some(entry)) = lazy.entries.get_mut(id as usize) else {
            continue;
        };
        if !entry.info.is_attached() {
            continue;
        }
        if entry.info.detach(keep_original) {
            lazy.reparse.detached += 1;
        } else {
            // Generating it may insert more stubs, which are collected too
            generate_before_drop(cx, id);
        }
    }
    cx.lazy_functions.collecting -= 1;
    Ok(())
}

/// Drop the AST of a program whose script has run: its pending stubs are detached (or generated,
/// for the few that cannot be parsed again on their own) and the AST arena is freed, keeping the
/// scope tree. Does nothing unless the context re-parses, or if no stub of the program is pending.
pub fn detach_program(
    mut cx: Context,
    program: *const ParsedProgram,
) -> Result<(), crate::runtime::bytecode::generator::EmitError> {
    if !reparse_enabled(cx) {
        return Ok(());
    }
    let Some(program_rc) = cx.lazy_functions.program_rc(program) else {
        return Ok(());
    };
    if !program_rc.has_ast() {
        return Ok(());
    }
    let keep_original = cx.options.lazy_reparse_verify;

    loop {
        let ids = cx.lazy_functions.attached_ids_of(program);
        if ids.is_empty() {
            break;
        }
        for id in ids {
            let lazy = &mut cx.lazy_functions;
            let Some(Some(entry)) = lazy.entries.get_mut(id as usize) else {
                continue;
            };
            if !entry.info.is_attached() || std::rc::Rc::as_ptr(entry.info.program()) != program {
                continue;
            }
            if entry.info.detach(keep_original) {
                lazy.reparse.detached += 1;
            } else {
                generate_before_drop(cx, id);
            }
        }
    }

    if !keep_original {
        // SAFETY: every pending stub of the program is detached, and no generation is running
        // (this is called between scripts or when idle).
        unsafe { program_rc.drop_ast() };
        cx.lazy_functions.reparse.programs_dropped += 1;
        compact_program(cx, &program_rc);
    }
    Ok(())
}

/// Free the parts of a program's scope tree that no pending stub is nested in. Called once the
/// AST is dropped, between generations (no reference into the tree is in use).
pub(crate) fn compact_program(mut cx: Context, program: &std::rc::Rc<ParsedProgram>) {
    let ptr = std::rc::Rc::as_ptr(program);
    let enclosing: Vec<_> = cx
        .lazy_functions
        .entries
        .iter()
        .flatten()
        .filter(|entry| std::rc::Rc::as_ptr(entry.info.program()) == ptr)
        .filter_map(|entry| entry.info.detached_enclosing_scope())
        .collect();
    // SAFETY: the AST is dropped (callers check), every pending stub of the program is detached
    // and refers to its enclosing scope and that scope's ancestors only, and no generation or
    // re-parse is running.
    unsafe { program.compact_scope_tree(&enclosing) };
    cx.lazy_functions.reparse.compactions += 1;
}
