use std::{cell::UnsafeCell, rc::Rc};

use bumpalo::Bump;

use crate::parser::source::Source;

/// Context for a single invocation of the parser. Owns the entire AST.
pub struct ParseContext {
    /// The source file that is being parsed
    source: Rc<Source>,
    /// An arena allocator for the nodes parsed from the source. None once the AST is dropped
    /// (`drop_ast_arena`).
    alloc: UnsafeCell<Option<Box<Bump>>>,
    /// A separate arena for the scope tree (scope nodes, their bindings and binding names). Lazy
    /// functions keep the scope tree after the AST is dropped: a function re-parsed on its first
    /// call resolves the names it uses outside itself through it.
    scope_alloc: UnsafeCell<Box<Bump>>,
}

impl ParseContext {
    pub fn new(source: Rc<Source>) -> Self {
        Self {
            source,
            alloc: UnsafeCell::new(Some(Box::new(Bump::new()))),
            scope_alloc: UnsafeCell::new(Box::new(Bump::new())),
        }
    }

    pub fn source(&self) -> &Rc<Source> {
        &self.source
    }

    pub fn alloc(&self) -> &Bump {
        // SAFETY: the arena is only replaced by `drop_ast_arena`, whose caller guarantees that no
        // reference into it (or to it) is used afterwards.
        unsafe { (*self.alloc.get()).as_deref() }.expect("AST arena already dropped")
    }

    /// The AST arena, if it still exists.
    pub fn alloc_if_alive(&self) -> Option<&Bump> {
        // SAFETY: as in `alloc`.
        unsafe { (*self.alloc.get()).as_deref() }
    }

    /// Whether the AST arena still exists.
    pub fn has_ast_arena(&self) -> bool {
        // SAFETY: only reads whether the arena exists, see `alloc`.
        unsafe { (*self.alloc.get()).is_some() }
    }

    /// Free the AST arena, keeping the scope tree arena.
    ///
    /// # Safety
    /// Nothing allocated in the AST arena, and no reference to the arena from `alloc`, may be used
    /// after this call.
    pub unsafe fn drop_ast_arena(&self) {
        unsafe { *self.alloc.get() = None };
    }

    /// Bytes allocated for the AST arena, zero once it is dropped.
    pub fn ast_arena_bytes(&self) -> usize {
        // SAFETY: as in `alloc`.
        unsafe { (*self.alloc.get()).as_deref() }
            .map_or(0, |a| a.allocated_bytes_including_metadata())
    }

    /// The arena of the scope tree.
    pub fn scope_alloc(&self) -> &Bump {
        // SAFETY: the arena is only replaced by `replace_scope_arena`, whose caller guarantees
        // that nothing in the old arena is used after it is freed.
        unsafe { &*self.scope_alloc.get() }
    }

    /// Replace the scope tree arena, returning the old one, to free once the tree is moved out of
    /// it (scope tree compaction).
    ///
    /// # Safety
    /// No reference to the old arena (from `scope_alloc`) or into it may be used after it is freed.
    pub unsafe fn replace_scope_arena(&self, arena: Box<Bump>) -> Box<Bump> {
        unsafe { std::mem::replace(&mut *self.scope_alloc.get(), arena) }
    }

    pub fn stats(&self) -> ParseStats {
        ParseStats {
            name: self.source.display_name().to_owned(),
            source_size: self.source.contents.len(),
            total_arena_size: self.ast_arena_bytes(),
        }
    }
}

#[allow(unused)]
#[derive(Debug)]
pub struct ParseStats {
    /// Display name of the source file
    name: String,
    /// Size of the source file in bytes
    source_size: usize,
    /// Total size of the arena allocated for the AST in bytes. Includes arena metadata and space
    /// allocated for the arena but not yet used.
    total_arena_size: usize,
}
