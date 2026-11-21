//! Re-parsing one function of a program whose AST has been dropped (SpiderMonkey's
//! delazification).
//!
//! A lazy function (a stub that has not run yet) used to keep the whole program's AST alive. Once
//! its script has run, the AST is dropped and the stub keeps only its source range and the id of
//! its scope node in the program's scope tree, which stays alive in its own arena. On the first
//! call the function's own text is parsed again into a temporary arena, with the parser and the
//! analyzer put back into the state they were in when they reached the function in the full parse
//! (recorded here, in the function's scope node). Names the function uses from outside resolve
//! through the kept scope tree, whose bindings already carry their final analysis (captured,
//! TDZ checks, VM locations), so the bytecode is the same as generation from the full AST.

use crate::parser::{loc::Pos, scope_tree::ScopeNodeId};

/// How a lazy function appears in the source, which decides the parser entry point used to parse
/// it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReparseKind {
    /// `[async] function [*] name (...) {...}`, starting at `async` or `function`.
    Declaration,
    /// A function expression, starting at `async` or `function`.
    Expression,
    /// An arrow function, starting at `async` or its parameters.
    Arrow,
    /// An object or class method, getter or setter. The function's range starts at the property
    /// (modifiers and key), its parameters at `params_pos`.
    Method {
        kind: MethodKind,
        params_pos: Pos,
        is_async: bool,
        is_generator: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodKind {
    Init,
    Get,
    Set,
}

/// The parser state at the start of the function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParserFlags {
    pub in_strict_mode: bool,
    pub allow_await: bool,
    pub allow_yield: bool,
    pub allow_in: bool,
    pub is_module: bool,
}

/// One entry of the analyzer's function stack, as bits.
pub type FunctionEntryBits = u8;

/// Most enclosing function entries kept for an arrow function: the chain of enclosing arrow
/// functions up to and including the nearest non-arrow function. A deeper chain is not re-parsed
/// (its stub is generated before the AST is dropped).
pub const MAX_FUNCTION_CHAIN: usize = super::reparse_rules::MAX_CHAIN;

/// The analyzer state at the start of the function, as far as the analysis of the function
/// depends on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalyzerSnapshot {
    /// Whether the enclosing context is strict.
    pub in_strict_mode_context: bool,
    /// The top of the analyzer's function stack, bottom first: the enclosing arrow functions and
    /// the nearest enclosing non-arrow function. Empty at the top level.
    pub function_chain: [FunctionEntryBits; MAX_FUNCTION_CHAIN],
    pub function_chain_len: u8,
    /// The innermost "super member allowed" entry: None if the stack is empty, Some(None) for
    /// Disallow, Some(Some((is_static, home_object_scope))) for Allow.
    pub allow_super: Option<Option<(bool, Option<ScopeNodeId>)>>,
    pub allow_arguments: bool,
    /// Whether the function is inside a class (private names may be used).
    pub in_class: bool,
    /// The analyzer arguments the function was visited with.
    pub is_arrow_function: bool,
    pub is_method: bool,
    pub is_static_method: bool,
}

/// What a function's scope node records so that the function can be parsed again on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReparseInfo {
    pub kind: ReparseKind,
    pub parser: ParserFlags,
    /// For a declaration, the scope node its name binding was added to (the name is not added
    /// again when the function is parsed again).
    pub decl_name_scope: Option<ScopeNodeId>,
    /// Set by the analyzer. None if the analyzer did not reach the function, or its context is not
    /// one that can be restored.
    pub analyzer: Option<AnalyzerSnapshot>,
}

impl ReparseInfo {
    /// Whether the function can be parsed again on its own.
    pub fn is_complete(&self) -> bool {
        self.analyzer.is_some()
            && (self.kind != ReparseKind::Declaration || self.decl_name_scope.is_some())
    }
}
