use std::{
    cell::{Cell, UnsafeCell},
    rc::Rc,
};

use bumpalo::Bump;

use crate::{
    common::options::Options,
    parser::{
        LocalizedParseError, ParseContext,
        analyze::{AnalyzedProgramResult, analyze, analyze_reparsed_function},
        ast::{AstBox, AstPtr, Function},
        loc::Loc,
        parse_module, parse_script,
        parser::reparse_function,
        reparse::ReparseInfo,
        scope_tree::{ScopeNodeId, ScopeTree},
        source::Source,
    },
};

/// A parsed and analyzed program bundled with the arena that owns its AST and scope tree.
///
/// Bytecode generation borrows the AST. A program kept in an `Rc<ParsedProgram>` lets functions
/// whose bytecode is generated lazily, on their first call, keep the AST alive after the toplevel
/// has been generated. The arena is freed when the last `Rc` is dropped.
pub struct ParsedProgram {
    // Field order matters: the analyzed result borrows from the parse context's arena, so it is
    // dropped first.
    // In an UnsafeCell: a re-parse adds nodes to the scope tree through a shared ParsedProgram.
    analyzed_result: UnsafeCell<AnalyzedProgramResult<'static>>,
    pcx: Box<ParseContext>,
    /// Whether a re-parse currently has the scope tree (it is moved into the parser meanwhile).
    reparsing: Cell<bool>,
    /// Bytes in use in the scope tree arena right after the last compaction (0 before any).
    compacted_scope_bytes: Cell<usize>,
}

// SAFETY: A ParsedProgram owns the AST arena, the AST and every `Rc` (the Source and the Options)
// and `Cell` the AST refers to. None of them are reachable from outside the ParsedProgram until it
// is borrowed, so moving the whole bundle to another thread cannot race on a reference count or a
// cell. It is not Sync.
unsafe impl Send for ParsedProgram {}

impl ParsedProgram {
    /// Parse and analyze a program in a new parse context for `source`. `build` runs the parser
    /// and analyzer on the parse context, and its result is kept together with the arena.
    pub fn new<E>(
        source: Rc<Source>,
        build: impl for<'a> FnOnce(&'a ParseContext) -> Result<AnalyzedProgramResult<'a>, E>,
    ) -> Result<ParsedProgram, E> {
        let pcx = Box::new(ParseContext::new(source));

        // SAFETY: The parse context is boxed, so neither it nor the arena the AST is allocated in
        // move, and it is stored in the ParsedProgram next to the AST and dropped after it.
        let pcx_ref: &'static ParseContext = unsafe { &*(&*pcx as *const ParseContext) };

        let analyzed_result = build(pcx_ref)?;

        Ok(ParsedProgram {
            analyzed_result: UnsafeCell::new(analyzed_result),
            pcx,
            reparsing: Cell::new(false),
            compacted_scope_bytes: Cell::new(0),
        })
    }

    /// Parse and analyze a script or module, returning the first syntax error if there is one.
    pub fn parse(
        source: Rc<Source>,
        options: Rc<Options>,
        is_module: bool,
    ) -> Result<ParsedProgram, LocalizedParseError> {
        Self::new(source, |pcx: &ParseContext| {
            let parse_result = if is_module {
                parse_module(pcx, options)?
            } else {
                parse_script(pcx, options)?
            };

            analyze(parse_result).map_err(|mut errors| errors.errors.swap_remove(0))
        })
    }

    /// The analyzed program, borrowed for as long as the ParsedProgram is. Not available once the
    /// AST is dropped (`drop_ast`).
    pub fn analyzed_result(&self) -> &AnalyzedProgramResult<'_> {
        assert!(self.has_ast(), "the program's AST was dropped");
        assert!(!self.reparsing.get());
        // SAFETY: The AST lives in the arena owned by `self`, so shortening the 'static lifetime of
        // the bundle's internal borrow to the borrow of `self` is sound.
        unsafe {
            std::mem::transmute::<&AnalyzedProgramResult<'static>, &AnalyzedProgramResult<'_>>(
                &*self.analyzed_result.get(),
            )
        }
    }

    pub fn source(&self) -> &Rc<Source> {
        // SAFETY: the source is never changed after construction.
        unsafe { &(*self.analyzed_result.get()).source }
    }

    /// Bytes allocated for the arena holding the AST and scope tree, including arena metadata and
    /// space allocated but not yet used.
    pub fn arena_bytes(&self) -> usize {
        self.ast_arena_bytes() + self.scope_arena_bytes()
    }

    /// Bytes allocated for the AST arena alone, zero once the AST is dropped.
    pub fn ast_arena_bytes(&self) -> usize {
        self.pcx.ast_arena_bytes()
    }

    /// Whether the AST still exists.
    pub fn has_ast(&self) -> bool {
        self.pcx.has_ast_arena()
    }

    /// The program's scope tree, which outlives the AST.
    pub fn scope_tree(&self) -> &ScopeTree<'_> {
        assert!(!self.reparsing.get());
        // SAFETY: as in `analyzed_result`: the scope tree lives in the scope arena owned by `self`.
        unsafe {
            std::mem::transmute::<&ScopeTree<'static>, &ScopeTree<'_>>(
                &(*self.analyzed_result.get()).scope_tree,
            )
        }
    }

    /// Free the AST, keeping the scope tree: afterwards lazy functions of the program are parsed
    /// again on their own (`reparse_function`).
    ///
    /// # Safety
    /// No reference into the AST may be used afterwards: every lazy stub of the program must have
    /// been detached from its AST node, and no generation of the program may be running.
    pub unsafe fn drop_ast(&self) {
        unsafe { self.pcx.drop_ast_arena() };
    }

    /// Parse and analyze one function of the program again on its own, into `arena`: the function
    /// whose scope node in the full parse was `scope_id`, child of `enclosing_scope`, spanning
    /// `loc`. The function's new scope nodes are added to the kept scope tree. The returned node
    /// lives in `arena` (and refers to the scope tree), so it may only be used while both are.
    pub fn reparse_function(
        &self,
        arena: &Bump,
        options: Rc<Options>,
        info: &ReparseInfo,
        enclosing_scope: ScopeNodeId,
        loc: Loc,
    ) -> Result<AstPtr<Function<'static>>, LocalizedParseError> {
        assert!(!self.reparsing.replace(true), "nested re-parse of one program");
        let result = self.reparse_function_inner(arena, options, info, enclosing_scope, loc);
        self.reparsing.set(false);
        result
    }

    /// Keep only the scopes that lazy functions still to be parsed again are nested in:
    /// `enclosing` holds the enclosing scope of each such function, and every ancestor of one is
    /// kept; the rest of the scope tree is freed (node ids do not change).
    ///
    /// # Safety
    /// The AST must be dropped, and no reference into the scope tree may be in use: no generation
    /// or re-parse of the program is running, and no lazy function of the program refers to a
    /// scope node outside `enclosing` and their ancestors.
    pub unsafe fn compact_scope_tree(&self, enclosing: &[ScopeNodeId]) {
        assert!(!self.has_ast() && !self.reparsing.get());
        // SAFETY: by the caller's guarantee nothing else uses the analyzed result meanwhile.
        let result = unsafe { &mut *self.analyzed_result.get() };

        let old: &ScopeTree<'static> = &result.scope_tree;
        let mut live = vec![false; old.num_ast_nodes()];
        for &id in enclosing {
            let mut cur = Some(id);
            while let Some(node_id) = cur {
                if live[node_id] {
                    break;
                }
                live[node_id] = true;
                cur = old.ast_node(node_id).parent();
            }
        }

        let arena = Box::new(Bump::new());
        // SAFETY: the new arena is owned by the parse context from here on, next to the tree.
        let arena_ref: &'static Bump = unsafe { &*(&*arena as *const Bump) };
        let tree = old.compact_into(arena_ref, &live);
        result.scope_tree = AstBox::new_in(tree, arena_ref);

        // SAFETY: the tree was copied out of the old arena, and nothing refers into it any more.
        let old_arena = unsafe { self.pcx.replace_scope_arena(arena) };
        drop(old_arena);
        self.compacted_scope_bytes.set(self.used_arena_bytes().1);
    }

    /// Whether re-parses have grown the scope tree arena enough since the last compaction to
    /// compact again: more than twice what it held then, and at least 4 MiB more.
    pub fn scope_tree_wants_compaction(&self) -> bool {
        let used = self.used_arena_bytes().1;
        let after_last = self.compacted_scope_bytes.get();
        !self.has_ast() && used > after_last.saturating_mul(2) && used - after_last > (4 << 20)
    }

    fn reparse_function_inner(
        &self,
        arena: &Bump,
        options: Rc<Options>,
        info: &ReparseInfo,
        enclosing_scope: ScopeNodeId,
        loc: Loc,
    ) -> Result<AstPtr<Function<'static>>, LocalizedParseError> {
        let source = self.source().clone();
        // SAFETY: `reparsing` is set, so no other reference to the analyzed result is in use
        // (`analyzed_result` and `scope_tree` assert that it is not set).
        let result = unsafe { &mut *self.analyzed_result.get() };
        let tree_ptr: *mut ScopeTree<'static> = &mut *result.scope_tree;

        // SAFETY: the scope tree's arena is owned by `self` and outlives `arena`'s use here; the
        // lifetimes are unified so that the re-parsed function's scope nodes (in the scope arena)
        // can refer to its AST (in `arena`). Those references are only followed while generating
        // the function, which happens while `arena` lives (see `LazyFunctionInfo`).
        let arena: &'static Bump = unsafe { std::mem::transmute::<&Bump, &'static Bump>(arena) };
        let source_ref: &'static Rc<Source> =
            unsafe { std::mem::transmute::<&Rc<Source>, &'static Rc<Source>>(&source) };

        if !info.is_complete() {
            return Err(reparse_error(&source, loc));
        }
        let info = *info;
        let snapshot = info.analyzer.unwrap();

        // Move the scope tree into the parser, and back. `reparsing` keeps every other use out.
        // SAFETY: the tree is read out and written back before anything else can see it.
        let tree = unsafe { std::ptr::read(tree_ptr) };
        let first_new_node = tree.num_ast_nodes();
        let (tree, parsed) = reparse_function(
            source_ref,
            options.clone(),
            arena,
            tree,
            enclosing_scope,
            &info,
            loc.start,
        );
        unsafe { std::ptr::write(tree_ptr, tree) };
        let mut func = parsed?;
        if func.loc != loc {
            return Err(reparse_error(&source, loc));
        }

        // SAFETY: as above; the box only aliases the tree for the duration of the analysis.
        let tree_box = unsafe { AstBox::from_raw(tree_ptr) };
        let enclosing = unsafe { &*tree_ptr }.get_ast_node_ptr(enclosing_scope);
        let (_, analyzed) = analyze_reparsed_function(
            source.clone(),
            options,
            tree_box,
            enclosing,
            first_new_node,
            &mut func,
            &snapshot,
        );
        if let Err(mut errors) = analyzed {
            return Err(errors.errors.swap_remove(0));
        }

        Ok(AstPtr::from_ref(func.as_ref()))
    }

    /// Bytes in use in the AST arena and in the scope tree arena (allocated chunks minus their free
    /// tails), for measurements.
    pub fn used_arena_bytes(&self) -> (usize, usize) {
        fn used(bump: &bumpalo::Bump) -> usize {
            // SAFETY: only the lengths of the chunks are read, no allocation is made meanwhile.
            unsafe { bump.iter_allocated_chunks_raw().map(|(_, len)| len).sum() }
        }
        (self.pcx.alloc_if_alive().map_or(0, used), used(self.pcx.scope_alloc()))
    }

    /// Bytes allocated for the scope tree arena alone.
    pub fn scope_arena_bytes(&self) -> usize {
        self.pcx.scope_alloc().allocated_bytes_including_metadata()
    }
}

/// The error of a function that cannot be parsed again on its own (it parsed in its program, so
/// this is an engine bug).
fn reparse_error(source: &Rc<Source>, loc: Loc) -> LocalizedParseError {
    LocalizedParseError {
        error: crate::parser::ParseError::new_unknown_token(
            "(engine: lazy function could not be parsed again)".to_owned(),
        ),
        source_loc: Some((loc, source.clone())),
    }
}
