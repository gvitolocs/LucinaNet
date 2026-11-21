//! Structural key computation for the per-function bytecode cache.
//!
//! The key of a function is a 128-bit hash of its AST where every identifier name and
//! string/template literal text is replaced by a symbol numbered by first occurrence of that text
//! (so the key fixes which names are equal, but not the names themselves), and where each nested
//! function is a placeholder carrying only its kind and flags. Numbers, regexp sources/flags,
//! operators, punctuation and statement/expression structure are hashed verbatim. The key also
//! includes everything outside the function that changes its bytecode: strict mode, the function
//! kind and flags, and for every reference that leaves the function the scope analysis result
//! (global/dynamic lookup, or scope hops and slot index, or captured this/arguments/new.target/home
//! object), plus the function's own scope tree structure.

use std::hash::Hasher;
use std::rc::Rc;

use rustc_hash::FxHashMap;
use siphasher::sip128::{Hasher128, SipHasher13};

use super::secret::KeySecret;
use crate::{
    common::wtf_8::{Wtf8Str, Wtf8String},
    parser::{
        ast::{self, AstPtr},
        ast_visitor::*,
        loc::{Loc, Pos},
        scope_tree::{
            ARGUMENTS_NAME, AstScopeNode, Binding, BindingKind, DERIVED_CONSTRUCTOR_BINDING_NAME,
            HOME_OBJECT_BINDING_NAME, NEW_TARGET_BINDING_NAME, STATIC_HOME_OBJECT_BINDING_NAME,
            ScopeNodeKind, ScopeTree, THIS_NAME, VMLocation,
        },
    },
    runtime::bytecode::generator::ScopeStackNode,
};

/// Number of independently hashed sections of a key.
pub const KEY_SECTIONS: usize = 4;
/// Section of the function's own AST (structure, symbols, scope references inside the function).
const SECTION_AST: usize = 0;
/// Section of the scope subtree the function owns.
const SECTION_OWN_SCOPES: usize = 1;
/// Section of everything the function reads from outside itself.
const SECTION_EXTERNAL: usize = 2;
/// Section of the source layout (gaps between nodes).
const SECTION_LAYOUT: usize = 3;

/// The key hash, kept in sections so that a miss can be attributed to the part that differs.
///
/// Every word of the walk goes through SipHash-1-3 with 128-bit output, keyed with the cache's
/// secret: without the secret, two different inputs cannot be made to collide. (An unkeyed
/// pre-mix would not do: its collisions would carry through the keyed hash.)
pub struct Sections {
    cur: usize,
    secret: KeySecret,
    parts: [SipHasher13; KEY_SECTIONS],
}

impl Sections {
    fn new(secret: &KeySecret) -> Self {
        Sections {
            cur: SECTION_AST,
            secret: *secret,
            parts: std::array::from_fn(|_| SipHasher13::new_with_key(secret)),
        }
    }

    #[inline]
    fn word(&mut self, v: u64) {
        self.parts[self.cur].write_u64(v);
    }

    #[inline]
    fn u8(&mut self, v: u8) {
        self.word(v as u64);
    }

    #[inline]
    fn u16(&mut self, v: u16) {
        self.word(v as u64);
    }

    #[inline]
    fn u32(&mut self, v: u32) {
        self.word(v as u64);
    }

    #[inline]
    fn u64(&mut self, v: u64) {
        self.word(v);
    }

    #[inline]
    fn usize(&mut self, v: usize) {
        self.word(v as u64);
    }

    #[inline]
    fn bool(&mut self, v: bool) {
        self.word(v as u64);
    }

    /// The per-section hashes and the key combining them.
    fn finish(self) -> (u128, [u128; KEY_SECTIONS]) {
        let sections: [u128; KEY_SECTIONS] = self.parts.map(|p| p.finish128().as_u128());
        let mut all = SipHasher13::new_with_key(&self.secret);
        for section in sections {
            all.write_u64(section as u64);
            all.write_u64((section >> 64) as u64);
        }
        (all.finish128().as_u128(), sections)
    }
}

/// Why a function cannot be cached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UncacheableReason {
    /// A reference resolved to a scope outside the function that is not on the known outer chain.
    ExternalScopeRef,
    /// A nested function of an unsupported kind (class fields/static initializer, default
    /// constructor with fields).
    UnsupportedNestedFunction,
    /// A constant table entry was deduplicated from two different provenances.
    ProvenanceConflict,
    /// A heap item of a kind that cannot be described.
    UnknownHeapItem,
    /// A name could not be traced back to an AST node.
    NameNotFound,
    /// A module scope patch, which only the toplevel can have.
    ModuleScopePatch,
    /// A source map position could not be traced back to an AST node.
    UnmappedPosition,
    /// The function is too large to cache.
    TooLarge,
    /// The source range recorded by the generator starts before the node it is relative to.
    RangeBeforeNode,
    /// A hit whose template could not be instantiated against the new function.
    InstantiateFailed,
    /// A hit whose patched scope access needs a different instruction width.
    PatchWidthChanged,
    /// The walk found something else that cannot be keyed.
    Other,
}

impl UncacheableReason {
    pub const COUNT: usize = 12;

    /// All reasons, in index order.
    pub const ALL: [UncacheableReason; Self::COUNT] = [
        UncacheableReason::ExternalScopeRef,
        UncacheableReason::UnsupportedNestedFunction,
        UncacheableReason::ProvenanceConflict,
        UncacheableReason::UnknownHeapItem,
        UncacheableReason::NameNotFound,
        UncacheableReason::ModuleScopePatch,
        UncacheableReason::UnmappedPosition,
        UncacheableReason::TooLarge,
        UncacheableReason::RangeBeforeNode,
        UncacheableReason::InstantiateFailed,
        UncacheableReason::PatchWidthChanged,
        UncacheableReason::Other,
    ];

    pub fn index(self) -> usize {
        match self {
            UncacheableReason::ExternalScopeRef => 0,
            UncacheableReason::UnsupportedNestedFunction => 1,
            UncacheableReason::ProvenanceConflict => 2,
            UncacheableReason::UnknownHeapItem => 3,
            UncacheableReason::NameNotFound => 4,
            UncacheableReason::ModuleScopePatch => 5,
            UncacheableReason::UnmappedPosition => 6,
            UncacheableReason::TooLarge => 7,
            UncacheableReason::RangeBeforeNode => 8,
            UncacheableReason::InstantiateFailed => 9,
            UncacheableReason::PatchWidthChanged => 10,
            UncacheableReason::Other => 11,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            UncacheableReason::ExternalScopeRef => "external scope ref",
            UncacheableReason::UnsupportedNestedFunction => "unsupported nested function",
            UncacheableReason::ProvenanceConflict => "provenance conflict",
            UncacheableReason::UnknownHeapItem => "unknown heap item",
            UncacheableReason::NameNotFound => "name not found",
            UncacheableReason::ModuleScopePatch => "module scope patch",
            UncacheableReason::UnmappedPosition => "unmapped position",
            UncacheableReason::TooLarge => "too large",
            UncacheableReason::RangeBeforeNode => "range before node",
            UncacheableReason::InstantiateFailed => "instantiate failed",
            UncacheableReason::PatchWidthChanged => "patched scope access changed width",
            UncacheableReason::Other => "other",
        }
    }
}

/// The kind of function being keyed, mirroring the generator's `PendingFunctionNode` variants that
/// are cacheable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuncKind {
    Declaration,
    Expression,
    Arrow,
    Method,
    Constructor,
}

/// Provenance of a function's `name` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameProv {
    None,
    /// The name of the identifier node with this ordinal.
    Node(u32),
    /// A method key node ordinal plus a prefix: 0 = none, 1 = "get ", 2 = "set ".
    MethodKey(u32, u8),
    /// Verbatim text, stored in the template.
    Text,
}

/// Information about the function being keyed, beyond its AST node.
pub struct FuncInfo<'a> {
    pub kind: FuncKind,
    /// The function's AST node. Always present for cacheable functions.
    pub node: AstPtr<ast::Function<'a>>,
    /// The class node, for constructors.
    pub class: Option<AstPtr<ast::Class<'a>>>,
    pub is_class_constructor: bool,
    pub is_base_constructor: bool,
    pub is_anonymous_default_export: bool,
    /// Whether the function's source range is the class's range (constructors).
    pub source_range_is_class: bool,
}

/// What kind of node an ordinal refers to, for re-reading source-derived values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Other,
    Identifier,
    StringLiteral,
    BigIntLiteral,
    RegExpLiteral,
    TemplateLiteral,
    Class,
    Function,
}

/// Flag in an anchored position offset: the position is `end + offset` of its node rather than
/// `start + offset`.
pub const POS_FROM_END: u32 = 1 << 31;

/// Resolve an anchored position offset against a node's range.
pub fn resolve_pos(loc: Loc, anchored: u32) -> Option<Pos> {
    let offset = (anchored & !POS_FROM_END) as usize;
    if anchored & POS_FROM_END != 0 {
        loc.end.checked_add(offset)
    } else {
        loc.start.checked_add(offset)
    }
}

pub struct NodeInfo {
    pub ptr: *const u8,
    pub loc: Loc,
    pub kind: NodeKind,
}

/// The result of walking a function: its structural key plus everything needed to re-read
/// source-derived values from the same (or a structurally equal) function's AST.
pub struct Walk<'a> {
    pub key: u128,
    /// The digest of the partition (top-level site) the key was made for.
    pub partition: u128,
    /// The hashes of the key's sections, to attribute a miss to the part that differs.
    pub sections: [u128; KEY_SECTIONS],
    pub func_info: FuncInfo<'a>,
    pub scope_tree: &'a ScopeTree<'a>,
    /// AST node pointer -> ordinal.
    /// Built on first use from `nodes` (first visit of a node wins): only a miss needs it.
    node_ordinals: std::cell::OnceCell<FxHashMap<*const u8, u32>>,
    /// Ordinal -> node info.
    pub nodes: Vec<NodeInfo>,
    /// AST scope id -> ordinal.
    pub scope_ordinals: FxHashMap<usize, u32>,
    /// Ordinal -> scope node.
    pub scopes: Vec<AstPtr<AstScopeNode<'a>>>,
    /// VM scope id -> AST scope ordinal (for scopes in the function's subtree).
    pub vm_ordinal: FxHashMap<usize, u32>,
    /// The outer scope chain (VM scope ids, innermost first) that the function is generated in.
    pub outer_chain: Vec<usize>,
    /// Extra source positions (operators, holes, ...) that are not node boundaries, with the
    /// node boundary they are anchored to: (position, node ordinal, anchored offset).
    pub extra_anchors: Vec<(Pos, u32, u32)>,
    /// Source position -> (node ordinal, anchored offset), see `POS_FROM_END` and `resolve_pos`.
    /// Built on first use: only a miss, which captures a template, needs it.
    pos_map: std::cell::OnceCell<FxHashMap<Pos, (u32, u32)>>,
    /// All identifier nodes with their names, for finding a name's node by pointer.
    pub identifier_names: Vec<(u32, &'a Wtf8Str)>,
    /// The external scope slots (kind: 0 scope, 1 module scope; VM scope id; slot index) that
    /// identifier references use, in order of first occurrence. The key fixes which references
    /// share a slot, not the slot itself, so bytecode that addresses them is patched on a hit.
    pub ext_slots: Vec<(u8, usize, usize)>,
    /// External slot -> its ordinal in `ext_slots`.
    pub ext_slot_ordinals: FxHashMap<(u8, usize, usize), u32>,
    /// Texts of AST nodes that can be re-read as constant table values, by pointer identity:
    /// (node ordinal, text). Covers identifier names and string literal values.
    pub text_nodes: Vec<(u32, &'a Wtf8Str)>,
    /// Cooked texts of template quasis: (template literal node ordinal, quasi index, cooked text).
    pub quasi_nodes: Vec<(u32, u32, &'a Wtf8Str)>,
    /// For each class method function node pointer: (key node ordinal, accessor prefix) where
    /// prefix is 0 = none, 1 = "get ", 2 = "set ". Used to rebuild method closure names.
    pub method_keys: FxHashMap<*const u8, (u32, u8)>,
    /// Ordinal of the node whose source range is the function's source range (the function node,
    /// or the class node for constructors).
    pub range_node: u32,
    pub uncacheable: Option<UncacheableReason>,
}

impl<'a> Walk<'a> {
    /// The anchored position of a source position: (node ordinal, anchored offset). Positions
    /// that are node boundaries map to the innermost node having the boundary; other positions
    /// that the key fixed (operators, holes) map to their anchor.
    pub fn anchored_pos(&self, pos: Pos) -> Option<(u32, u32)> {
        let map = self.pos_map.get_or_init(|| {
            let mut map: FxHashMap<Pos, (u32, u32)> = FxHashMap::default();
            let record =
                |map: &mut FxHashMap<Pos, (u32, u32)>, pos: Pos, ordinal: u32, anchored: u32| {
                    if pos == crate::parser::loc::NO_POS {
                        return;
                    }
                    // Prefer the innermost (smallest) node when several nodes share a position.
                    let replace = match map.get(&pos) {
                        Some((old, _)) => {
                            let len = |o: u32| {
                                let loc = self.nodes[o as usize].loc;
                                loc.end - loc.start
                            };
                            len(ordinal) < len(*old)
                        }
                        None => true,
                    };
                    if replace {
                        map.insert(pos, (ordinal, anchored));
                    }
                };
            for (ordinal, node) in self.nodes.iter().enumerate() {
                record(&mut map, node.loc.start, ordinal as u32, 0);
                if node.loc.end > node.loc.start {
                    record(&mut map, node.loc.end, ordinal as u32, POS_FROM_END);
                }
            }
            for (pos, ordinal, anchored) in &self.extra_anchors {
                map.insert(*pos, (*ordinal, *anchored));
            }
            map
        });
        map.get(&pos).copied()
    }

    pub fn ordinal_of(&self, ptr: *const u8) -> Option<u32> {
        let map = self.node_ordinals.get_or_init(|| {
            let mut map: FxHashMap<*const u8, u32> = FxHashMap::default();
            for (ordinal, node) in self.nodes.iter().enumerate() {
                map.entry(node.ptr).or_insert(ordinal as u32);
            }
            map
        });
        map.get(&ptr).copied()
    }

    /// The AST node with the given ordinal, cast to the expected type.
    pub fn node<T>(&self, ordinal: u32) -> Option<&'a T> {
        let ptr = self.nodes.get(ordinal as usize)?.ptr;
        // SAFETY: `ptr` was stored by the walk from a reference to an AST node that lives in the
        // parse arena for `'a`. The caller names the type through the ordinal's `NodeKind`
        // (identifier, string literal, function, class, template literal), which the walk
        // assigned at the same time as the pointer. Callers check the kind first or take the
        // ordinal from a template whose key fixed the node kinds, so `T` is the stored type.
        Some(unsafe { &*(ptr as *const T) })
    }

    pub fn scope(&self, ordinal: u32) -> Option<&'a AstScopeNode<'a>> {
        let p = self.scopes.get(ordinal as usize)?;
        Some(p.as_arena_ref())
    }

    /// Find the identifier node whose name is the given string (by pointer identity).
    pub fn find_identifier_by_name(&self, name: &Wtf8Str) -> Option<u32> {
        self.identifier_names
            .iter()
            .find(|(_, n)| std::ptr::eq(*n, name))
            .map(|(o, _)| *o)
    }

    /// Find the AST node whose text is the given string (by pointer identity), for re-reading
    /// source-derived constant values. Returns the node ordinal.
    pub fn find_text_node(&self, text: &Wtf8Str) -> Option<u32> {
        self.text_nodes
            .iter()
            .find(|(_, t)| std::ptr::eq(*t, text))
            .map(|(o, _)| *o)
    }

    /// Find the template quasi whose cooked text is the given string (by pointer identity).
    /// Returns (template literal node ordinal, quasi index).
    pub fn find_quasi(&self, text: &Wtf8Str) -> Option<(u32, u32)> {
        self.quasi_nodes
            .iter()
            .find(|(_, _, t)| std::ptr::eq(*t, text))
            .map(|(o, i, _)| (*o, *i))
    }

    /// The ordinal of an AST scope node in the function's scope subtree.
    pub fn scope_ordinal_of(&self, ast_scope_id: usize) -> Option<u32> {
        self.scope_ordinals.get(&ast_scope_id).copied()
    }
}

/// The walker that computes the key and assigns ordinals.
struct KeyWalker<'a> {
    h: Sections,
    symbols: FxHashMap<&'a Wtf8Str, u32>,
    /// Only kept when a node can be visited twice (constructors walk their class and function).
    node_ordinals: FxHashMap<*const u8, u32>,
    dedupe_nodes: bool,
    nodes: Vec<NodeInfo>,
    scope_ordinals: FxHashMap<usize, u32>,
    scopes: Vec<AstPtr<AstScopeNode<'a>>>,
    vm_ordinal: FxHashMap<usize, u32>,
    outer_chain: Vec<usize>,
    /// Extra source positions (operators, holes, ...) resolved after the walk.
    extra_positions: Vec<Pos>,
    extra_anchors: Vec<(Pos, u32, u32)>,
    /// Set while hashing the binding of an identifier that resolves outside the function: its
    /// scope slot is hashed as an ordinal, not as a value.
    slot_param: bool,
    ext_scope_ordinals: FxHashMap<usize, u32>,
    ext_slots: Vec<(u8, usize, usize)>,
    ext_slot_ordinals: FxHashMap<(u8, usize, usize), u32>,
    identifier_names: Vec<(u32, &'a Wtf8Str)>,
    text_nodes: Vec<(u32, &'a Wtf8Str)>,
    quasi_nodes: Vec<(u32, u32, &'a Wtf8Str)>,
    method_keys: FxHashMap<*const u8, (u32, u8)>,
    scope_tree: &'a ScopeTree<'a>,
    uncacheable: Option<UncacheableReason>,
}

impl<'a> KeyWalker<'a> {
    #[inline]
    fn tag(&mut self, tag: u32) {
        self.h.u32(tag);
    }

    /// The ordinal of a node visited a short while ago (a key just before its function).
    fn recent_ordinal(&self, ptr: *const u8) -> Option<u32> {
        let n = self.nodes.len();
        self.nodes[n.saturating_sub(64)..]
            .iter()
            .rposition(|node| node.ptr == ptr)
            .map(|i| (n.saturating_sub(64) + i) as u32)
    }

    /// The ordinal of an AST node, if it was visited.
    fn ordinal_of(&self, ptr: *const u8) -> Option<u32> {
        self.nodes
            .iter()
            .rposition(|n| n.ptr == ptr)
            .map(|i| i as u32)
    }

    #[inline]
    fn u8(&mut self, v: u8) {
        self.h.u8(v);
    }

    #[inline]
    fn u16(&mut self, v: u16) {
        self.h.u16(v);
    }

    #[inline]
    fn u32(&mut self, v: u32) {
        self.h.u32(v);
    }

    #[inline]
    fn u64(&mut self, v: u64) {
        self.h.u64(v);
    }

    #[inline]
    fn usize(&mut self, v: usize) {
        self.h.usize(v);
    }

    #[inline]
    fn boolean(&mut self, v: bool) {
        self.h.bool(v);
    }

    /// Number a text by first occurrence; equal texts get equal symbols.
    fn symbol(&mut self, text: &'a Wtf8Str) -> u32 {
        let next = self.symbols.len() as u32;
        *self.symbols.entry(text).or_insert(next)
    }

    fn hash_symbol(&mut self, text: &'a Wtf8Str) {
        let sym = self.symbol(text);
        self.u32(sym);
        // Properties of the text that the generator branches on, besides equality.
        let bytes = text.as_bytes();
        let flags = (bytes.is_empty() as u8)
            | ((bytes == b"eval") as u8) << 1
            | ((bytes == b"__proto__") as u8) << 2
            | (is_array_index_text(text) as u8) << 3;
        self.u8(flags);
    }

    /// Hash a text that belongs to the node with the given ordinal, recording the mapping so the
    /// text can be re-read from a structurally equal function's AST.
    fn hash_symbol_node(&mut self, text: &'a Wtf8Str, ordinal: u32) {
        self.hash_symbol(text);
        self.text_nodes.push((ordinal, text));
    }

    fn fail(&mut self, reason: UncacheableReason) {
        if self.uncacheable.is_none() {
            self.uncacheable = Some(reason);
        }
    }

    /// Assign an ordinal to a node and record its info. Idempotent: a node that already has an
    /// ordinal keeps it.
    fn ordinal(&mut self, ptr: *const u8, loc: Loc, kind: NodeKind) -> u32 {
        if !self.dedupe_nodes {
            let ordinal = self.nodes.len() as u32;
            self.nodes.push(NodeInfo { ptr, loc, kind });
            return ordinal;
        }
        if let Some(prev) = self.node_ordinals.get(&ptr) {
            return *prev;
        }
        let ordinal = self.nodes.len() as u32;
        self.node_ordinals.insert(ptr, ordinal);
        self.nodes.push(NodeInfo { ptr, loc, kind });
        ordinal
    }

    /// Record an extra source position (an operator, a hole, `super`, `in`/`of`). It is anchored
    /// to a node boundary after the walk, see `resolve_extra_positions`.
    fn record_extra_pos(&mut self, pos: Pos) {
        if pos != crate::parser::loc::NO_POS {
            self.extra_positions.push(pos);
        }
    }

    /// Mix the source layout into the key and anchor the extra positions.
    ///
    /// The generator records positions that are node boundaries or token positions between
    /// nodes. After a rename the texts have other lengths, so a position can only be rebuilt
    /// from an anchor (a node's start or end) plus a distance that does not cross a name. The
    /// gaps between a node and the end of the previous sibling (or the start of its parent), and
    /// between the last child and the end of the parent, are therefore part of the key: two
    /// functions with the same key have the same layout, and a boundary maps onto a boundary.
    fn resolve_layout(&mut self) {
        self.h.cur = SECTION_LAYOUT;
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        let mut order: Vec<u32> = (0..n as u32).collect();
        let nodes = &self.nodes;
        let before = |x: u32, y: u32| {
            let (lx, ly) = (nodes[x as usize].loc, nodes[y as usize].loc);
            lx.start
                .cmp(&ly.start)
                .then(ly.end.cmp(&lx.end))
                .then(x.cmp(&y))
        };
        // Nodes are visited in source order for almost every function.
        if !order.windows(2).all(|w| before(w[0], w[1]).is_le()) {
            order.sort_unstable_by(|&x, &y| before(x, y));
        }

        struct Frame {
            end: usize,
            cursor: usize,
            has_children: bool,
        }
        let first_start = self.nodes[order[0] as usize].loc.start;
        let mut stack: Vec<Frame> =
            vec![Frame { end: usize::MAX, cursor: first_start, has_children: false }];
        self.tag(TAG_LAYOUT);
        for &idx in &order {
            let loc = self.nodes[idx as usize].loc;
            while stack.len() > 1 && loc.end > stack.last().unwrap().end {
                let frame = stack.pop().unwrap();
                if frame.has_children {
                    self.u64(frame.end.wrapping_sub(frame.cursor) as u64);
                }
                stack.last_mut().unwrap().cursor = frame.end;
            }
            let parent = stack.last_mut().unwrap();
            let gap_in = loc.start.wrapping_sub(parent.cursor);
            parent.has_children = true;
            self.h.u64(gap_in as u64);
            stack.push(Frame { end: loc.end, cursor: loc.start, has_children: false });
        }
        while stack.len() > 1 {
            let frame = stack.pop().unwrap();
            if frame.has_children {
                self.u64(frame.end.wrapping_sub(frame.cursor) as u64);
            }
            stack.last_mut().unwrap().cursor = frame.end;
        }

        // Extra positions: anchor to the end of the last node that ends at or before them.
        self.tag(TAG_EXTRA_POSITIONS);
        let extras = std::mem::take(&mut self.extra_positions);
        if extras.is_empty() {
            return;
        }
        let mut by_end: Vec<u32> = (0..n as u32).collect();
        by_end.sort_by(|&x, &y| {
            let (lx, ly) = (self.nodes[x as usize].loc, self.nodes[y as usize].loc);
            lx.end.cmp(&ly.end).then(x.cmp(&y))
        });
        for pos in extras {
            let first_start = order.partition_point(|&i| self.nodes[i as usize].loc.start < pos);
            let is_start =
                first_start < n && self.nodes[order[first_start] as usize].loc.start == pos;
            let first_end = by_end.partition_point(|&i| self.nodes[i as usize].loc.end < pos);
            let is_end = first_end < n && self.nodes[by_end[first_end] as usize].loc.end == pos;
            if is_start || is_end || self.extra_anchors.iter().any(|(p, ..)| *p == pos) {
                self.u32(0);
                continue;
            }
            let upto = by_end.partition_point(|&i| self.nodes[i as usize].loc.end <= pos);
            if upto > 0 {
                let ordinal = by_end[upto - 1];
                let gap = pos - self.nodes[ordinal as usize].loc.end;
                self.u32(1);
                self.u64(gap as u64);
                self.extra_anchors
                    .push((pos, ordinal, POS_FROM_END | gap as u32));
            } else {
                let ordinal = order[0];
                let gap = pos.saturating_sub(self.nodes[ordinal as usize].loc.start);
                self.u32(2);
                self.u64(gap as u64);
                self.extra_anchors.push((pos, ordinal, gap as u32));
            }
        }
    }

    /// Hash a reference to an AST scope node: its ordinal if it is in the function's scope
    /// subtree, otherwise its depth on the outer chain.
    fn hash_scope_ref(&mut self, scope_id: usize) {
        if let Some(ordinal) = self.scope_ordinals.get(&scope_id).copied() {
            self.u8(0);
            self.u32(ordinal);
        } else {
            let prev = std::mem::replace(&mut self.h.cur, SECTION_EXTERNAL);
            self.hash_external_scope_ref(scope_id);
            self.h.cur = prev;
        }
    }

    /// Hash whether an external scope has a VM node, and require that it is on the outer chain.
    /// Where on the chain is patched on a hit, not part of the key.
    fn hash_external_scope_presence(&mut self, scope_id: usize) {
        let scope_tree = self.scope_tree;
        let ast_node = scope_tree.get_ast_node_ptr(scope_id);
        match ast_node.as_arena_ref().vm_scope_id() {
            Some(vm_id) => {
                if self.outer_chain.contains(&vm_id) {
                    self.u8(1);
                } else {
                    self.u8(2);
                    self.fail(UncacheableReason::ExternalScopeRef);
                }
            }
            None => self.u8(3),
        }
    }

    fn hash_external_scope_ref(&mut self, scope_id: usize) {
        {
            // Find the scope's VM node id, then its depth on the outer chain.
            let scope_tree = self.scope_tree;
            let ast_node = scope_tree.get_ast_node_ptr(scope_id);
            let vm_id = ast_node.as_arena_ref().vm_scope_id();
            match vm_id {
                Some(vm_id) => match self.outer_chain.iter().position(|id| *id == vm_id) {
                    Some(depth) => {
                        self.u8(1);
                        self.usize(depth);
                    }
                    None => {
                        self.u8(2);
                        self.fail(UncacheableReason::ExternalScopeRef);
                    }
                },
                None => {
                    self.u8(3);
                }
            }
        }
    }

    /// Hash a reference to a VM scope id.
    fn hash_vm_scope_ref(&mut self, vm_id: usize) {
        if let Some(ordinal) = self.vm_ordinal.get(&vm_id).copied() {
            self.u8(0);
            self.u32(ordinal);
        } else {
            let prev = std::mem::replace(&mut self.h.cur, SECTION_EXTERNAL);
            self.hash_external_vm_scope_ref(vm_id);
            self.h.cur = prev;
        }
    }

    fn hash_external_vm_scope_ref(&mut self, vm_id: usize) {
        {
            match self.outer_chain.iter().position(|id| *id == vm_id) {
                Some(depth) => {
                    self.u8(1);
                    self.usize(depth);
                }
                None => {
                    self.u8(2);
                    self.fail(UncacheableReason::ExternalScopeRef);
                }
            }
        }
    }

    /// Hash a slot of a scope outside the function by ordinal: the numbering by first occurrence
    /// fixes which references share a scope and a slot, without fixing the values.
    fn hash_param_slot(&mut self, kind: u8, vm_id: usize, index: usize) {
        if !self.outer_chain.contains(&vm_id) {
            self.u8(7);
            self.fail(UncacheableReason::ExternalScopeRef);
            return;
        }
        let next_scope = self.ext_scope_ordinals.len() as u32;
        let scope_ordinal = *self.ext_scope_ordinals.entry(vm_id).or_insert(next_scope);
        let slot = (kind, vm_id, index);
        let slot_ordinal = match self.ext_slot_ordinals.get(&slot) {
            Some(ordinal) => *ordinal,
            None => {
                let ordinal = self.ext_slots.len() as u32;
                self.ext_slots.push(slot);
                self.ext_slot_ordinals.insert(slot, ordinal);
                ordinal
            }
        };
        self.u8(8 + kind);
        self.u32(scope_ordinal);
        self.u32(slot_ordinal);
    }

    fn hash_vm_location(&mut self, loc: Option<VMLocation>) {
        match loc {
            None => self.u8(0),
            Some(VMLocation::Global) => self.u8(1),
            Some(VMLocation::Argument(index)) => {
                self.u8(2);
                self.usize(index);
            }
            Some(VMLocation::LocalRegister(index)) => {
                self.u8(3);
                self.usize(index);
            }
            Some(VMLocation::Scope { scope_id, index })
                if self.slot_param && !self.vm_ordinal.contains_key(&scope_id) =>
            {
                self.hash_param_slot(0, scope_id, index);
            }
            Some(VMLocation::ModuleScope { scope_id, index })
                if self.slot_param && !self.vm_ordinal.contains_key(&scope_id) =>
            {
                self.hash_param_slot(1, scope_id, index);
            }
            Some(VMLocation::Scope { scope_id, index }) => {
                self.u8(4);
                self.hash_vm_scope_ref(scope_id);
                self.usize(index);
            }
            Some(VMLocation::ModuleScope { scope_id, index }) => {
                self.u8(5);
                self.hash_vm_scope_ref(scope_id);
                self.usize(index);
            }
            Some(VMLocation::EvalVar) => self.u8(6),
            Some(VMLocation::WithVar) => self.u8(7),
        }
    }

    fn hash_binding(&mut self, binding: &Binding) {
        self.hash_vm_location(binding.vm_location());
        self.boolean(binding.needs_tdz_check());
        self.boolean(binding.is_exported());
        self.boolean(binding.has_nested_assignment());
        self.boolean(binding.is_immutable());
        self.boolean(binding.is_captured());
        self.boolean(binding.force_vm_scope());
        match binding.kind() {
            BindingKind::Var => self.u8(0),
            BindingKind::Const { .. } => self.u8(1),
            BindingKind::Let { .. } => self.u8(2),
            BindingKind::Function { is_lexical, is_expression, parameter_index, .. } => {
                self.u8(3);
                self.boolean(*is_lexical);
                self.boolean(*is_expression);
                match parameter_index {
                    Some(index) => {
                        self.u8(1);
                        self.u32(*index);
                    }
                    None => self.u8(0),
                }
            }
            BindingKind::FunctionParameter { index, .. } => {
                self.u8(4);
                self.u32(*index);
            }
            BindingKind::Class { in_body_scope, .. } => {
                self.u8(5);
                self.boolean(*in_body_scope);
            }
            BindingKind::CatchParameter { .. } => self.u8(6),
            BindingKind::Import { is_namespace } => {
                self.u8(7);
                self.boolean(*is_namespace);
            }
            BindingKind::ImplicitThis { in_derived_constructor } => {
                self.u8(8);
                self.boolean(*in_derived_constructor);
            }
            BindingKind::ImplicitArguments => self.u8(9),
            BindingKind::ImplicitNewTarget => self.u8(10),
            BindingKind::DerivedConstructor => self.u8(11),
            BindingKind::HomeObject => self.u8(12),
            BindingKind::PrivateName => self.u8(13),
            BindingKind::DefaultExportExpression => self.u8(14),
        }
    }

    /// Hash the resolution of a tagged resolved scope reference.
    fn hash_resolved_scope(&mut self, scope: &ast::TaggedResolvedScope<'a>) {
        use ast::ResolvedScope;
        match scope.kind() {
            ResolvedScope::UnresolvedGlobal => self.u8(0),
            ResolvedScope::UnresolvedDynamic => self.u8(1),
            ResolvedScope::Resolved => {
                self.u8(2);
                let ast_node = scope.unwrap_resolved();
                self.hash_scope_ref(ast_node.id());
                let own = self.scope_ordinals.contains_key(&ast_node.id());
                let prev = std::mem::replace(
                    &mut self.h.cur,
                    if own { SECTION_AST } else { SECTION_EXTERNAL },
                );
                // The generator reads the implicit bindings of the resolved scope (`this` in a
                // derived constructor needs an initialization check, a captured `this` lives in
                // a scope slot, ...), so they are part of the key.
                let names: [&Wtf8String; 6] = [
                    &THIS_NAME,
                    &NEW_TARGET_BINDING_NAME,
                    &DERIVED_CONSTRUCTOR_BINDING_NAME,
                    &HOME_OBJECT_BINDING_NAME,
                    &STATIC_HOME_OBJECT_BINDING_NAME,
                    &ARGUMENTS_NAME,
                ];
                for name in names {
                    match ast_node.get_binding_opt(name.as_str()) {
                        Some(binding) => {
                            self.u8(1);
                            self.hash_binding(binding);
                        }
                        None => self.u8(0),
                    }
                }
                self.h.cur = prev;
            }
        }
    }

    /// Walk a scope node and its owned scopes, hashing the scope structure.
    fn walk_scope(&mut self, scope: AstPtr<AstScopeNode<'a>>) {
        let prev = std::mem::replace(&mut self.h.cur, SECTION_OWN_SCOPES);
        self.walk_scope_in_section(scope);
        self.h.cur = prev;
    }

    fn walk_scope_in_section(&mut self, scope: AstPtr<AstScopeNode<'a>>) {
        let scope_tree = self.scope_tree;
        let node = scope.as_arena_ref();
        let id = node.id();
        let ordinal = self.scopes.len() as u32;
        self.scope_ordinals.insert(id, ordinal);
        self.scopes.push(scope);
        if let Some(vm_id) = node.vm_scope_id() {
            self.vm_ordinal.insert(vm_id, ordinal);
        }

        self.tag(TAG_SCOPE);
        self.u32(ordinal);
        match node.kind() {
            ScopeNodeKind::Global => self.u8(0),
            ScopeNodeKind::Module => self.u8(1),
            ScopeNodeKind::Function { is_arrow, .. } => {
                self.u8(2);
                self.boolean(is_arrow);
            }
            ScopeNodeKind::FunctionBody => self.u8(3),
            ScopeNodeKind::FunctionExpressionName => self.u8(4),
            ScopeNodeKind::Block => self.u8(5),
            ScopeNodeKind::Switch => self.u8(6),
            ScopeNodeKind::Class => self.u8(7),
            ScopeNodeKind::CatchBody => self.u8(8),
            ScopeNodeKind::With => self.u8(9),
            ScopeNodeKind::Eval { is_direct, is_strict } => {
                self.u8(10);
                self.boolean(is_direct);
                self.boolean(is_strict);
            }
            ScopeNodeKind::StaticInitializer => self.u8(11),
        }
        self.boolean(node.supports_dynamic_access());
        self.boolean(node.supports_dynamic_bindings());
        self.boolean(node.allow_empty_vm_node());
        self.boolean(node.may_dynamically_access_uninitialized_parameters());
        self.usize(node.num_local_registers());
        self.usize(node.num_bindings());
        self.boolean(node.has_duplicates());
        self.boolean(node.vm_scope_id().is_some());
        // Owner scope: itself or an ancestor, which has already been walked.
        let owner = node.owner_scope();
        if owner == id {
            self.u8(0);
        } else if let Some(owner_ordinal) = self.scope_ordinals.get(&owner).copied() {
            self.u8(1);
            self.u32(owner_ordinal);
        } else {
            self.u8(2);
            self.fail(UncacheableReason::ExternalScopeRef);
        }

        // Bindings in insertion order.
        for (name, binding) in node.iter_bindings() {
            self.tag(TAG_BINDING);
            self.hash_symbol(name);
            self.hash_binding(binding);
        }

        // Extra var names, sorted by symbol for determinism.
        let mut syms: Vec<u32> = node
            .extra_var_names()
            .iter()
            .map(|n| self.symbol(n))
            .collect();
        syms.sort_unstable();
        self.usize(syms.len());
        for sym in syms {
            self.u32(sym);
        }

        // VM scope node bindings, in slot order.
        if let Some(vm_id) = node.vm_scope_id() {
            self.tag(TAG_VM_NODE);
            let vm_node = scope_tree.get_vm_node(vm_id);
            let bindings: Vec<_> = vm_node.bindings().to_vec();
            self.usize(bindings.len());
            for name in bindings {
                self.hash_symbol(name);
            }
        }

        // Recurse into owned scopes.
        let owned: Vec<_> = node.owned_scopes().to_vec();
        for child in owned {
            self.walk_scope(scope_tree.get_ast_node_ptr(child));
        }
    }

    /// Placeholder for a nested function: kind and flags only, not its body.
    fn fn_placeholder(&mut self, func: &ast::Function<'a>, tag: u32) {
        self.tag(tag);
        self.ordinal(func as *const ast::Function as *const u8, func.loc, NodeKind::Function);
        self.u16(func.flags.bits());
        match &func.id {
            Some(id) => {
                self.u8(1);
                let id_ordinal = self.ordinal(
                    id.as_ref() as *const ast::Identifier as *const u8,
                    id.loc,
                    NodeKind::Identifier,
                );
                self.hash_symbol_node(id.name, id_ordinal);
                self.identifier_names.push((id_ordinal, id.name));
            }
            None => self.u8(0),
        }
    }
}

// Tags for node types. Arbitrary distinct constants.
const TAG_SCOPE: u32 = 1;
const TAG_BINDING: u32 = 2;
const TAG_VM_NODE: u32 = 3;
const TAG_PROGRAM: u32 = 10;
const TAG_TOPLEVEL: u32 = 11;
const TAG_STATEMENT: u32 = 12;
const TAG_EXPRESSION: u32 = 13;
const TAG_PATTERN: u32 = 14;
const TAG_OUTER_EXPRESSION: u32 = 15;
const TAG_VAR_DECL: u32 = 16;
const TAG_VAR_DECLARATOR: u32 = 17;
const TAG_FUNCTION_DECL: u32 = 18;
const TAG_FUNCTION: u32 = 19;
const TAG_FUNCTION_PARAM: u32 = 20;
const TAG_FUNCTION_BODY: u32 = 21;
const TAG_FUNCTION_BLOCK_BODY: u32 = 22;
const TAG_CLASS_DECL: u32 = 23;
const TAG_CLASS: u32 = 24;
const TAG_CLASS_ELEMENT: u32 = 25;
const TAG_CLASS_METHOD: u32 = 26;
const TAG_CLASS_PROPERTY: u32 = 27;
const TAG_BLOCK: u32 = 28;
const TAG_EXPR_STMT: u32 = 29;
const TAG_IF: u32 = 30;
const TAG_SWITCH: u32 = 31;
const TAG_SWITCH_CASE: u32 = 32;
const TAG_FOR: u32 = 33;
const TAG_FOR_INIT: u32 = 34;
const TAG_FOR_EACH: u32 = 35;
const TAG_FOR_EACH_INIT: u32 = 36;
const TAG_WHILE: u32 = 37;
const TAG_DO_WHILE: u32 = 38;
const TAG_WITH: u32 = 39;
const TAG_TRY: u32 = 40;
const TAG_CATCH: u32 = 41;
const TAG_THROW: u32 = 42;
const TAG_RETURN: u32 = 43;
const TAG_BREAK: u32 = 44;
const TAG_CONTINUE: u32 = 45;
const TAG_LABELED: u32 = 46;
const TAG_EMPTY: u32 = 47;
const TAG_DEBUGGER: u32 = 48;
const TAG_ID_EXPR: u32 = 49;
const TAG_NULL_LIT: u32 = 50;
const TAG_BOOL_LIT: u32 = 51;
const TAG_NUMBER_LIT: u32 = 52;
const TAG_STRING_LIT: u32 = 53;
const TAG_BIGINT_LIT: u32 = 54;
const TAG_REGEXP_LIT: u32 = 55;
const TAG_UNARY: u32 = 56;
const TAG_BINARY: u32 = 57;
const TAG_LOGICAL: u32 = 58;
const TAG_ASSIGN: u32 = 59;
const TAG_UPDATE: u32 = 60;
const TAG_MEMBER: u32 = 61;
const TAG_CHAIN: u32 = 62;
const TAG_CONDITIONAL: u32 = 63;
const TAG_CALL: u32 = 64;
const TAG_CALL_ARG: u32 = 65;
const TAG_NEW: u32 = 66;
const TAG_SEQUENCE: u32 = 67;
const TAG_THIS: u32 = 68;
const TAG_ARRAY: u32 = 69;
const TAG_SPREAD: u32 = 70;
const TAG_OBJECT: u32 = 71;
const TAG_PROPERTY: u32 = 72;
const TAG_FUNC_EXPR: u32 = 73;
const TAG_ARROW: u32 = 74;
const TAG_CLASS_EXPR: u32 = 75;
const TAG_AWAIT: u32 = 76;
const TAG_YIELD: u32 = 77;
const TAG_SUPER_MEMBER: u32 = 78;
const TAG_SUPER_CALL: u32 = 79;
const TAG_TEMPLATE: u32 = 80;
const TAG_TEMPLATE_ELEMENT: u32 = 81;
const TAG_TAGGED_TEMPLATE: u32 = 82;
const TAG_META_PROPERTY: u32 = 83;
const TAG_IMPORT_EXPR: u32 = 84;
const TAG_ID_PATTERN: u32 = 85;
const TAG_ARRAY_PATTERN: u32 = 86;
const TAG_REST: u32 = 87;
const TAG_OBJECT_PATTERN: u32 = 88;
const TAG_OBJECT_PATTERN_PROPERTY: u32 = 89;
const TAG_ASSIGN_PATTERN: u32 = 90;
const TAG_IDENTIFIER: u32 = 91;
const TAG_LAYOUT: u32 = 92;
const TAG_EXTRA_POSITIONS: u32 = 93;

macro_rules! walk_node {
    ($method:ident, $tag:expr, $ty:ty, $default:path, $kind:expr) => {
        fn $method(&mut self, node: &mut $ty) {
            self.tag($tag);
            self.ordinal(node as *mut _ as *const u8, node_loc(node), $kind);
            $default(self, node);
        }
    };
}

fn node_loc<T: HasLoc>(node: &T) -> Loc {
    node.loc()
}

trait HasLoc {
    fn loc(&self) -> Loc;
}

macro_rules! impl_has_loc {
    ($($ty:ty),*) => {
        $(impl HasLoc for $ty {
            fn loc(&self) -> Loc { self.loc }
        })*
    };
}

impl_has_loc!(
    ast::VariableDeclaration<'_>,
    ast::VariableDeclarator<'_>,
    ast::Function<'_>,
    ast::FunctionBlockBody<'_>,
    ast::Class<'_>,
    ast::ClassMethod<'_>,
    ast::ClassProperty<'_>,
    ast::Block<'_>,
    ast::ExpressionStatement<'_>,
    ast::IfStatement<'_>,
    ast::SwitchStatement<'_>,
    ast::SwitchCase<'_>,
    ast::ForStatement<'_>,
    ast::ForEachStatement<'_>,
    ast::WhileStatement<'_>,
    ast::DoWhileStatement<'_>,
    ast::WithStatement<'_>,
    ast::TryStatement<'_>,
    ast::CatchClause<'_>,
    ast::ThrowStatement<'_>,
    ast::ReturnStatement<'_>,
    ast::BreakStatement<'_>,
    ast::ContinueStatement<'_>,
    ast::LabeledStatement<'_>,
    ast::BooleanLiteral,
    ast::NumberLiteral,
    ast::StringLiteral<'_>,
    ast::BigIntLiteral<'_>,
    ast::RegExpLiteral<'_>,
    ast::UnaryExpression<'_>,
    ast::BinaryExpression<'_>,
    ast::LogicalExpression<'_>,
    ast::AssignmentExpression<'_>,
    ast::UpdateExpression<'_>,
    ast::MemberExpression<'_>,
    ast::ChainExpression<'_>,
    ast::ConditionalExpression<'_>,
    ast::CallExpression<'_>,
    ast::NewExpression<'_>,
    ast::SequenceExpression<'_>,
    ast::ArrayExpression<'_>,
    ast::SpreadElement<'_>,
    ast::ObjectExpression<'_>,
    ast::Property<'_>,
    ast::AwaitExpression<'_>,
    ast::YieldExpression<'_>,
    ast::SuperMemberExpression<'_>,
    ast::SuperCallExpression<'_>,
    ast::TemplateLiteral<'_>,
    ast::TemplateElement<'_>,
    ast::TaggedTemplateExpression<'_>,
    ast::MetaProperty<'_>,
    ast::ImportExpression<'_>,
    ast::ArrayPattern<'_>,
    ast::RestElement<'_>,
    ast::ObjectPattern<'_>,
    ast::ObjectPatternProperty<'_>,
    ast::AssignmentPattern<'_>,
    ast::Identifier<'_>,
    ast::Label<'_>
);

impl HasLoc for Loc {
    fn loc(&self) -> Loc {
        *self
    }
}

impl<'a> AstVisitor<'a> for KeyWalker<'a> {
    fn visit_program(&mut self, program: &mut ast::Program<'a>) {
        self.tag(TAG_PROGRAM);
        self.ordinal(program as *mut _ as *const u8, program.loc, NodeKind::Other);
        default_visit_program(self, program);
    }

    fn visit_toplevel(&mut self, toplevel: &mut ast::Toplevel<'a>) {
        self.tag(TAG_TOPLEVEL);
        match toplevel {
            ast::Toplevel::Statement(_) => self.u8(0),
            ast::Toplevel::Import(_) => self.u8(1),
            ast::Toplevel::ExportDefault(_) => self.u8(2),
            ast::Toplevel::ExportNamed(_) => self.u8(3),
            ast::Toplevel::ExportAll(_) => self.u8(4),
        }
        default_visit_toplevel(self, toplevel);
    }

    fn visit_statement(&mut self, stmt: &mut ast::Statement<'a>) {
        self.tag(TAG_STATEMENT);
        match stmt {
            ast::Statement::VarDecl(_) => self.u8(0),
            ast::Statement::FuncDecl(_) => self.u8(1),
            ast::Statement::ClassDecl(_) => self.u8(2),
            ast::Statement::Expr(_) => self.u8(3),
            ast::Statement::Block(_) => self.u8(4),
            ast::Statement::If(_) => self.u8(5),
            ast::Statement::Switch(_) => self.u8(6),
            ast::Statement::For(_) => self.u8(7),
            ast::Statement::ForEach(_) => self.u8(8),
            ast::Statement::While(_) => self.u8(9),
            ast::Statement::DoWhile(_) => self.u8(10),
            ast::Statement::With(_) => self.u8(11),
            ast::Statement::Try(_) => self.u8(12),
            ast::Statement::Throw(_) => self.u8(13),
            ast::Statement::Return(_) => self.u8(14),
            ast::Statement::Break(_) => self.u8(15),
            ast::Statement::Continue(_) => self.u8(16),
            ast::Statement::Labeled(_) => self.u8(17),
            ast::Statement::Empty(_) => self.u8(18),
            ast::Statement::Debugger(_) => self.u8(19),
        }
        match stmt {
            ast::Statement::VarDecl(v) => self.visit_variable_declaration(v),
            ast::Statement::FuncDecl(f) => self.visit_function_declaration(f),
            ast::Statement::ClassDecl(c) => self.visit_class_declaration(c),
            ast::Statement::Expr(e) => self.visit_expression_statement(e),
            ast::Statement::Block(s) => self.visit_block(s),
            ast::Statement::If(s) => self.visit_if_statement(s),
            ast::Statement::Switch(s) => self.visit_switch_statement(s),
            ast::Statement::For(s) => self.visit_for_statement(s),
            ast::Statement::ForEach(s) => self.visit_for_each_statement(s),
            ast::Statement::While(s) => self.visit_while_statement(s),
            ast::Statement::DoWhile(s) => self.visit_do_while_statement(s),
            ast::Statement::With(s) => self.visit_with_statement(s),
            ast::Statement::Try(s) => self.visit_try_statement(s),
            ast::Statement::Throw(s) => self.visit_throw_statement(s),
            ast::Statement::Return(s) => self.visit_return_statement(s),
            ast::Statement::Break(s) => self.visit_break_statement(s),
            ast::Statement::Continue(s) => self.visit_continue_statement(s),
            ast::Statement::Labeled(s) => self.visit_labeled_statement(s),
            ast::Statement::Empty(s) => self.visit_empty_statement(s),
            ast::Statement::Debugger(s) => self.visit_debugger_statement(s),
        }
    }

    fn visit_expression(&mut self, expr: &mut ast::Expression<'a>) {
        self.tag(TAG_EXPRESSION);
        match expr {
            ast::Expression::Id(_) => self.u8(0),
            ast::Expression::Null(_) => self.u8(1),
            ast::Expression::Boolean(_) => self.u8(2),
            ast::Expression::Number(_) => self.u8(3),
            ast::Expression::String(_) => self.u8(4),
            ast::Expression::BigInt(_) => self.u8(5),
            ast::Expression::RegExp(_) => self.u8(6),
            ast::Expression::Unary(_) => self.u8(7),
            ast::Expression::Binary(_) => self.u8(8),
            ast::Expression::Logical(_) => self.u8(9),
            ast::Expression::Assign(_) => self.u8(10),
            ast::Expression::Update(_) => self.u8(11),
            ast::Expression::Member(_) => self.u8(12),
            ast::Expression::Chain(_) => self.u8(13),
            ast::Expression::Conditional(_) => self.u8(14),
            ast::Expression::Call(_) => self.u8(15),
            ast::Expression::New(_) => self.u8(16),
            ast::Expression::Sequence(_) => self.u8(17),
            ast::Expression::This(_) => self.u8(18),
            ast::Expression::Array(_) => self.u8(19),
            ast::Expression::Object(_) => self.u8(20),
            ast::Expression::Function(_) => self.u8(21),
            ast::Expression::ArrowFunction(_) => self.u8(22),
            ast::Expression::Class(_) => self.u8(23),
            ast::Expression::Await(_) => self.u8(24),
            ast::Expression::Yield(_) => self.u8(25),
            ast::Expression::SuperMember(_) => self.u8(26),
            ast::Expression::SuperCall(_) => self.u8(27),
            ast::Expression::Template(_) => self.u8(28),
            ast::Expression::TaggedTemplate(_) => self.u8(29),
            ast::Expression::MetaProperty(_) => self.u8(30),
            ast::Expression::Import(_) => self.u8(31),
        }
        match expr {
            ast::Expression::Id(e) => self.visit_identifier_expression(e),
            ast::Expression::Null(e) => self.visit_null_literal(e),
            ast::Expression::Boolean(e) => self.visit_boolean_literal(e),
            ast::Expression::Number(e) => self.visit_number_literal(e),
            ast::Expression::String(e) => self.visit_string_literal(e),
            ast::Expression::BigInt(e) => self.visit_bigint_literal(e),
            ast::Expression::RegExp(e) => self.visit_regexp_literal(e),
            ast::Expression::Unary(e) => self.visit_unary_expression(e),
            ast::Expression::Binary(e) => self.visit_binary_expression(e),
            ast::Expression::Logical(e) => self.visit_logical_expression(e),
            ast::Expression::Assign(e) => self.visit_assignment_expression(e),
            ast::Expression::Update(e) => self.visit_update_expression(e),
            ast::Expression::Member(e) => self.visit_member_expression(e),
            ast::Expression::Chain(e) => self.visit_chain_expression(e),
            ast::Expression::Conditional(e) => self.visit_conditional_expression(e),
            ast::Expression::Call(e) => self.visit_call_expression(e),
            ast::Expression::New(e) => self.visit_new_expression(e),
            ast::Expression::Sequence(e) => self.visit_sequence_expression(e),
            ast::Expression::This(e) => self.visit_this_expression(e),
            ast::Expression::Array(e) => self.visit_array_expression(e),
            ast::Expression::Object(e) => self.visit_object_expression(e),
            ast::Expression::Function(e) => self.visit_function_expression(e),
            ast::Expression::ArrowFunction(e) => self.visit_arrow_function(e),
            ast::Expression::Class(e) => self.visit_class_expression(e),
            ast::Expression::Await(e) => self.visit_await_expression(e),
            ast::Expression::Yield(e) => self.visit_yield_expression(e),
            ast::Expression::SuperMember(e) => self.visit_super_member_expression(e),
            ast::Expression::SuperCall(e) => self.visit_super_call_expression(e),
            ast::Expression::Template(e) => self.visit_template_literal(e),
            ast::Expression::TaggedTemplate(e) => self.visit_tagged_template_expression(e),
            ast::Expression::MetaProperty(e) => self.visit_meta_property(e),
            ast::Expression::Import(e) => self.visit_import_expression(e),
        }
    }

    fn visit_pattern(&mut self, patt: &mut ast::Pattern<'a>) {
        self.tag(TAG_PATTERN);
        match patt {
            ast::Pattern::Id(_) => self.u8(0),
            ast::Pattern::Array(_) => self.u8(1),
            ast::Pattern::Object(_) => self.u8(2),
            ast::Pattern::Assign(_) => self.u8(3),
            ast::Pattern::Member(_) => self.u8(4),
            ast::Pattern::SuperMember(_) => self.u8(5),
            ast::Pattern::InvalidCall(_) => self.u8(6),
        }
        match patt {
            ast::Pattern::Id(e) => self.visit_identifier_pattern(e),
            ast::Pattern::Array(e) => self.visit_array_pattern(e),
            ast::Pattern::Object(e) => self.visit_object_pattern(e),
            ast::Pattern::Assign(e) => self.visit_assignment_pattern(e),
            ast::Pattern::Member(e) => self.visit_member_expression(e),
            ast::Pattern::SuperMember(e) => self.visit_super_member_expression(e),
            ast::Pattern::InvalidCall(e) => self.visit_call_expression(e),
        }
    }

    fn visit_outer_expression(&mut self, expr: &mut ast::OuterExpression<'a>) {
        self.tag(TAG_OUTER_EXPRESSION);
        self.boolean(expr.has_assign_expr);
        default_visit_outer_expression(self, expr);
    }

    walk_node!(
        visit_variable_declaration,
        TAG_VAR_DECL,
        ast::VariableDeclaration<'a>,
        default_visit_variable_declaration,
        NodeKind::Other
    );

    fn visit_variable_declarator(&mut self, decl: &mut ast::VariableDeclarator<'a>) {
        self.tag(TAG_VAR_DECLARATOR);
        self.ordinal(decl as *mut _ as *const u8, decl.loc, NodeKind::Other);
        self.boolean(decl.id_has_assign_expr);
        default_visit_variable_declarator(self, decl);
    }

    fn visit_function_declaration(&mut self, func: &mut ast::Function<'a>) {
        self.fn_placeholder(func, TAG_FUNCTION_DECL);
    }

    fn visit_function(&mut self, func: &mut ast::Function<'a>) {
        self.tag(TAG_FUNCTION);
        self.ordinal(func as *mut _ as *const u8, func.loc, NodeKind::Function);
        self.u16(func.flags.bits());
        default_visit_function(self, func);
    }

    fn visit_function_param(&mut self, param: &mut ast::FunctionParam<'a>) {
        self.tag(TAG_FUNCTION_PARAM);
        match param {
            ast::FunctionParam::Pattern { has_assign_expr, .. } => {
                self.u8(0);
                self.boolean(*has_assign_expr);
            }
            ast::FunctionParam::Rest { has_assign_expr, .. } => {
                self.u8(1);
                self.boolean(*has_assign_expr);
            }
        }
        default_visit_function_param(self, param);
    }

    fn visit_function_body(&mut self, body: &mut ast::FunctionBody<'a>) {
        self.tag(TAG_FUNCTION_BODY);
        self.ordinal(body as *mut _ as *const u8, body_loc(body), NodeKind::Other);
        match body {
            ast::FunctionBody::Block(_) => self.u8(0),
            ast::FunctionBody::Expression(_) => self.u8(1),
        }
        default_visit_function_body(self, body);
    }

    fn visit_function_block_body(&mut self, body: &mut ast::FunctionBlockBody<'a>) {
        self.tag(TAG_FUNCTION_BLOCK_BODY);
        self.ordinal(body as *mut _ as *const u8, body.loc, NodeKind::Other);
        match &body.scope {
            Some(scope) => {
                self.u8(1);
                self.hash_scope_ref(scope.as_ref().id());
            }
            None => self.u8(0),
        }
        default_visit_function_block_body(self, body);
    }

    fn visit_class_declaration(&mut self, class: &mut ast::Class<'a>) {
        self.tag(TAG_CLASS_DECL);
        self.visit_class(class);
    }

    fn visit_class(&mut self, class: &mut ast::Class<'a>) {
        self.tag(TAG_CLASS);
        self.ordinal(class as *mut _ as *const u8, class.loc, NodeKind::Class);
        match &class.id {
            Some(id) => {
                self.u8(1);
                let id_ordinal = self.ordinal(
                    id.as_ref() as *const ast::Identifier as *const u8,
                    id.loc,
                    NodeKind::Identifier,
                );
                self.hash_symbol_node(id.name, id_ordinal);
                self.identifier_names.push((id_ordinal, id.name));
            }
            None => self.u8(0),
        }
        self.hash_scope_ref(class.scope.as_ref().id());
        self.boolean(class.fields_initializer_scope.is_some());
        self.boolean(class.static_initializer_scope.is_some());
        self.boolean(class.has_assign_expr);
        self.boolean(class.constructor.is_some());
        match &mut class.super_class {
            Some(super_class) => {
                self.u8(1);
                self.visit_expression(super_class);
            }
            None => self.u8(0),
        }
        for element in class.body.iter_mut() {
            self.visit_class_element(element);
        }
    }

    fn visit_class_element(&mut self, element: &mut ast::ClassElement<'a>) {
        self.tag(TAG_CLASS_ELEMENT);
        match element {
            ast::ClassElement::Method(_) => self.u8(0),
            ast::ClassElement::Property(_) => self.u8(1),
        }
        default_visit_class_element(self, element);
    }

    fn visit_class_method(&mut self, method: &mut ast::ClassMethod<'a>) {
        self.tag(TAG_CLASS_METHOD);
        self.ordinal(method as *mut _ as *const u8, method.loc, NodeKind::Other);
        // Method kind
        match method.kind {
            ast::ClassMethodKind::Method => self.u8(0),
            ast::ClassMethodKind::Get => self.u8(1),
            ast::ClassMethodKind::Set => self.u8(2),
            ast::ClassMethodKind::Constructor => self.u8(3),
            ast::ClassMethodKind::StaticInitializer => self.u8(4),
        }
        self.boolean(method.is_computed);
        self.boolean(method.is_static);
        self.boolean(method.is_private);
        self.boolean(method.is_private_pair_start);
        // The key is evaluated inline in the parent for computed keys; named keys are hashed too.
        self.visit_expression(&mut method.key);
        // The method's function is a nested function: placeholder with kind/flags only.
        let func_ordinal = self.ordinal(
            method.value.as_ref() as *const ast::Function as *const u8,
            method.value.loc,
            NodeKind::Function,
        );
        self.u16(method.value.flags.bits());
        match &method.value.id {
            Some(id) => {
                self.u8(1);
                self.hash_symbol(id.name);
            }
            None => self.u8(0),
        }
        // Record how to rebuild this method's closure name: the key node ordinal plus an accessor
        // prefix. Only for non-computed named keys.
        if !method.is_computed {
            let key_ptr = match &method.key {
                ast::Expression::Id(id) => id.as_ref() as *const ast::Identifier as *const u8,
                ast::Expression::String(s) => s.as_ref() as *const ast::StringLiteral as *const u8,
                _ => std::ptr::null(),
            };
            if !key_ptr.is_null() {
                if let Some(key_ordinal) = self.recent_ordinal(key_ptr) {
                    let prefix = match method.kind {
                        ast::ClassMethodKind::Get => 1,
                        ast::ClassMethodKind::Set => 2,
                        _ => 0,
                    };
                    self.method_keys.insert(
                        method.value.as_ref() as *const ast::Function as *const u8,
                        (key_ordinal, prefix),
                    );
                }
            }
        }
        let _ = func_ordinal;
    }

    fn visit_class_property(&mut self, prop: &mut ast::ClassProperty<'a>) {
        self.tag(TAG_CLASS_PROPERTY);
        self.ordinal(prop as *mut _ as *const u8, prop.loc, NodeKind::Other);
        self.boolean(prop.is_computed);
        self.boolean(prop.is_static);
        self.boolean(prop.is_private);
        self.visit_expression(&mut prop.key);
        // The field initializer belongs to the fields initializer function, not this one.
        self.boolean(prop.value.is_some());
    }

    fn visit_block(&mut self, block: &mut ast::Block<'a>) {
        self.tag(TAG_BLOCK);
        self.ordinal(block as *mut _ as *const u8, block.loc, NodeKind::Other);
        self.hash_scope_ref(block.scope.as_ref().id());
        default_visit_block(self, block);
    }

    walk_node!(
        visit_expression_statement,
        TAG_EXPR_STMT,
        ast::ExpressionStatement<'a>,
        default_visit_expression_statement,
        NodeKind::Other
    );

    walk_node!(
        visit_if_statement,
        TAG_IF,
        ast::IfStatement<'a>,
        default_visit_if_statement,
        NodeKind::Other
    );

    fn visit_switch_statement(&mut self, stmt: &mut ast::SwitchStatement<'a>) {
        self.tag(TAG_SWITCH);
        self.ordinal(stmt as *mut _ as *const u8, stmt.loc, NodeKind::Other);
        self.hash_scope_ref(stmt.scope.as_ref().id());
        self.boolean(stmt.has_assign_expr);
        default_visit_switch_statement(self, stmt);
    }

    walk_node!(
        visit_switch_case,
        TAG_SWITCH_CASE,
        ast::SwitchCase<'a>,
        default_visit_switch_case,
        NodeKind::Other
    );

    fn visit_for_statement(&mut self, stmt: &mut ast::ForStatement<'a>) {
        self.tag(TAG_FOR);
        self.ordinal(stmt as *mut _ as *const u8, stmt.loc, NodeKind::Other);
        self.hash_scope_ref(stmt.scope.as_ref().id());
        default_visit_for_statement(self, stmt);
    }

    fn visit_for_init(&mut self, init: &mut ast::ForInit<'a>) {
        self.tag(TAG_FOR_INIT);
        match init {
            ast::ForInit::Expression(_) => self.u8(0),
            ast::ForInit::VarDecl(_) => self.u8(1),
        }
        default_visit_for_init(self, init);
    }

    fn visit_for_each_statement(&mut self, stmt: &mut ast::ForEachStatement<'a>) {
        self.tag(TAG_FOR_EACH);
        self.ordinal(stmt as *mut _ as *const u8, stmt.loc, NodeKind::Other);
        self.hash_scope_ref(stmt.scope.as_ref().id());
        match stmt.kind {
            ast::ForEachKind::In => self.u8(0),
            ast::ForEachKind::Of => self.u8(1),
        }
        self.boolean(stmt.is_await);
        self.record_extra_pos(stmt.in_of_pos);
        default_visit_for_each_statement(self, stmt);
    }

    fn visit_for_each_init(&mut self, init: &mut ast::ForEachInit<'a>) {
        self.tag(TAG_FOR_EACH_INIT);
        match init {
            ast::ForEachInit::VarDecl(_) => self.u8(0),
            ast::ForEachInit::Pattern { has_assign_expr, .. } => {
                self.u8(1);
                self.boolean(*has_assign_expr);
            }
        }
        default_visit_for_each_init(self, init);
    }

    walk_node!(
        visit_while_statement,
        TAG_WHILE,
        ast::WhileStatement<'a>,
        default_visit_while_statement,
        NodeKind::Other
    );
    walk_node!(
        visit_do_while_statement,
        TAG_DO_WHILE,
        ast::DoWhileStatement<'a>,
        default_visit_do_while_statement,
        NodeKind::Other
    );

    fn visit_with_statement(&mut self, stmt: &mut ast::WithStatement<'a>) {
        self.tag(TAG_WITH);
        self.ordinal(stmt as *mut _ as *const u8, stmt.loc, NodeKind::Other);
        self.hash_scope_ref(stmt.scope.as_ref().id());
        default_visit_with_statement(self, stmt);
    }

    walk_node!(
        visit_try_statement,
        TAG_TRY,
        ast::TryStatement<'a>,
        default_visit_try_statement,
        NodeKind::Other
    );

    fn visit_catch_clause(&mut self, catch: &mut ast::CatchClause<'a>) {
        self.tag(TAG_CATCH);
        self.ordinal(catch as *mut _ as *const u8, catch.loc, NodeKind::Other);
        self.hash_scope_ref(catch.scope.as_ref().id());
        self.boolean(catch.param_has_assign_expr);
        default_visit_catch_clause(self, catch);
    }

    walk_node!(
        visit_throw_statement,
        TAG_THROW,
        ast::ThrowStatement<'a>,
        default_visit_throw_statement,
        NodeKind::Other
    );
    walk_node!(
        visit_return_statement,
        TAG_RETURN,
        ast::ReturnStatement<'a>,
        default_visit_return_statement,
        NodeKind::Other
    );

    fn visit_break_statement(&mut self, stmt: &mut ast::BreakStatement<'a>) {
        self.tag(TAG_BREAK);
        self.ordinal(stmt as *mut _ as *const u8, stmt.loc, NodeKind::Other);
        match &stmt.label {
            Some(label) => {
                self.u8(1);
                self.hash_symbol(label.name);
            }
            None => self.u8(0),
        }
    }

    fn visit_continue_statement(&mut self, stmt: &mut ast::ContinueStatement<'a>) {
        self.tag(TAG_CONTINUE);
        self.ordinal(stmt as *mut _ as *const u8, stmt.loc, NodeKind::Other);
        match &stmt.label {
            Some(label) => {
                self.u8(1);
                self.hash_symbol(label.name);
            }
            None => self.u8(0),
        }
    }

    fn visit_labeled_statement(&mut self, stmt: &mut ast::LabeledStatement<'a>) {
        self.tag(TAG_LABELED);
        self.ordinal(stmt as *mut _ as *const u8, stmt.loc, NodeKind::Other);
        self.hash_symbol(stmt.label.name);
        self.u16(stmt.label.id);
        default_visit_labeled_statement(self, stmt);
    }

    fn visit_empty_statement(&mut self, loc: &mut Loc) {
        self.tag(TAG_EMPTY);
        self.ordinal(loc as *mut _ as *const u8, *loc, NodeKind::Other);
    }

    fn visit_debugger_statement(&mut self, loc: &mut Loc) {
        self.tag(TAG_DEBUGGER);
        self.ordinal(loc as *mut _ as *const u8, *loc, NodeKind::Other);
    }

    fn visit_identifier_expression(&mut self, id: &mut ast::Identifier<'a>) {
        self.tag(TAG_ID_EXPR);
        self.visit_identifier(id);
    }

    fn visit_null_literal(&mut self, loc: &mut Loc) {
        self.tag(TAG_NULL_LIT);
        self.ordinal(loc as *mut _ as *const u8, *loc, NodeKind::Other);
    }

    fn visit_boolean_literal(&mut self, lit: &mut ast::BooleanLiteral) {
        self.tag(TAG_BOOL_LIT);
        self.ordinal(lit as *mut _ as *const u8, lit.loc, NodeKind::Other);
        self.boolean(lit.value);
    }

    fn visit_number_literal(&mut self, lit: &mut ast::NumberLiteral) {
        self.tag(TAG_NUMBER_LIT);
        self.ordinal(lit as *mut _ as *const u8, lit.loc, NodeKind::Other);
        self.u64(lit.value.to_bits());
    }

    fn visit_string_literal(&mut self, lit: &mut ast::StringLiteral<'a>) {
        self.tag(TAG_STRING_LIT);
        let ordinal = self.ordinal(lit as *mut _ as *const u8, lit.loc, NodeKind::StringLiteral);
        self.hash_symbol_node(lit.value, ordinal);
        // Whether the text is a canonical array index changes how it is stored in the constant
        // table (smi vs string), so it is part of the key even though the text itself is a symbol.
        self.boolean(is_array_index_text(lit.value));
    }

    fn visit_bigint_literal(&mut self, lit: &mut ast::BigIntLiteral<'a>) {
        self.tag(TAG_BIGINT_LIT);
        self.ordinal(lit as *mut _ as *const u8, lit.loc, NodeKind::BigIntLiteral);
        let value = lit.value();
        let (sign, digits) = value.into_parts();
        self.u8(match sign {
            num_bigint::Sign::Minus => 0,
            num_bigint::Sign::NoSign => 1,
            num_bigint::Sign::Plus => 2,
        });
        let digits: Vec<u32> = digits.iter_u32_digits().collect();
        self.usize(digits.len());
        for digit in digits {
            self.u32(digit);
        }
    }

    fn visit_regexp_literal(&mut self, lit: &mut ast::RegExpLiteral<'a>) {
        self.tag(TAG_REGEXP_LIT);
        self.ordinal(lit as *mut _ as *const u8, lit.loc, NodeKind::RegExpLiteral);
        // Regexp sources and flags are part of the key verbatim.
        for byte in lit.pattern.as_bytes() {
            self.u8(*byte);
        }
        self.u8(0xFF);
        for byte in lit.flags.as_bytes() {
            self.u8(*byte);
        }
    }

    fn visit_unary_expression(&mut self, expr: &mut ast::UnaryExpression<'a>) {
        self.tag(TAG_UNARY);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        match expr.operator {
            ast::UnaryOperator::Plus => self.u8(0),
            ast::UnaryOperator::Minus => self.u8(1),
            ast::UnaryOperator::LogicalNot => self.u8(2),
            ast::UnaryOperator::BitwiseNot => self.u8(3),
            ast::UnaryOperator::TypeOf => self.u8(4),
            ast::UnaryOperator::Void => self.u8(5),
            ast::UnaryOperator::Delete => self.u8(6),
        }
        default_visit_unary_expression(self, expr);
    }

    fn visit_binary_expression(&mut self, expr: &mut ast::BinaryExpression<'a>) {
        self.tag(TAG_BINARY);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        match expr.operator {
            ast::BinaryOperator::Add => self.u8(0),
            ast::BinaryOperator::Subtract => self.u8(1),
            ast::BinaryOperator::Multiply => self.u8(2),
            ast::BinaryOperator::Divide => self.u8(3),
            ast::BinaryOperator::Remainder => self.u8(4),
            ast::BinaryOperator::Exponent => self.u8(5),
            ast::BinaryOperator::EqEq => self.u8(6),
            ast::BinaryOperator::EqEqEq => self.u8(7),
            ast::BinaryOperator::NotEq => self.u8(8),
            ast::BinaryOperator::NotEqEq => self.u8(9),
            ast::BinaryOperator::LessThan => self.u8(10),
            ast::BinaryOperator::LessThanOrEqual => self.u8(11),
            ast::BinaryOperator::GreaterThan => self.u8(12),
            ast::BinaryOperator::GreaterThanOrEqual => self.u8(13),
            ast::BinaryOperator::And => self.u8(14),
            ast::BinaryOperator::Or => self.u8(15),
            ast::BinaryOperator::Xor => self.u8(16),
            ast::BinaryOperator::ShiftLeft => self.u8(17),
            ast::BinaryOperator::ShiftRightArithmetic => self.u8(18),
            ast::BinaryOperator::ShiftRightLogical => self.u8(19),
            ast::BinaryOperator::In => self.u8(20),
            ast::BinaryOperator::InPrivate => self.u8(21),
            ast::BinaryOperator::InstanceOf => self.u8(22),
        }
        self.record_extra_pos(expr.operator_pos);
        default_visit_binary_expression(self, expr);
    }

    fn visit_logical_expression(&mut self, expr: &mut ast::LogicalExpression<'a>) {
        self.tag(TAG_LOGICAL);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        match expr.operator {
            ast::LogicalOperator::And => self.u8(0),
            ast::LogicalOperator::Or => self.u8(1),
            ast::LogicalOperator::NullishCoalesce => self.u8(2),
        }
        default_visit_logical_expression(self, expr);
    }

    fn visit_assignment_expression(&mut self, expr: &mut ast::AssignmentExpression<'a>) {
        self.tag(TAG_ASSIGN);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        match expr.operator {
            ast::AssignmentOperator::Equals => self.u8(0),
            ast::AssignmentOperator::Add => self.u8(1),
            ast::AssignmentOperator::Subtract => self.u8(2),
            ast::AssignmentOperator::Multiply => self.u8(3),
            ast::AssignmentOperator::Divide => self.u8(4),
            ast::AssignmentOperator::Remainder => self.u8(5),
            ast::AssignmentOperator::Exponent => self.u8(6),
            ast::AssignmentOperator::And => self.u8(7),
            ast::AssignmentOperator::Or => self.u8(8),
            ast::AssignmentOperator::Xor => self.u8(9),
            ast::AssignmentOperator::ShiftLeft => self.u8(10),
            ast::AssignmentOperator::ShiftRightArithmetic => self.u8(11),
            ast::AssignmentOperator::ShiftRightLogical => self.u8(12),
            ast::AssignmentOperator::LogicalAnd => self.u8(13),
            ast::AssignmentOperator::LogicalOr => self.u8(14),
            ast::AssignmentOperator::NullishCoalesce => self.u8(15),
        }
        self.boolean(expr.is_parenthesized);
        self.record_extra_pos(expr.operator_pos);
        default_visit_assignment_expression(self, expr);
    }

    fn visit_update_expression(&mut self, expr: &mut ast::UpdateExpression<'a>) {
        self.tag(TAG_UPDATE);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        match expr.operator {
            ast::UpdateOperator::Increment => self.u8(0),
            ast::UpdateOperator::Decrement => self.u8(1),
        }
        self.boolean(expr.is_prefix);
        default_visit_update_expression(self, expr);
    }

    fn visit_member_expression(&mut self, expr: &mut ast::MemberExpression<'a>) {
        self.tag(TAG_MEMBER);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        self.boolean(expr.is_computed);
        self.boolean(expr.is_optional);
        self.boolean(expr.is_private);
        self.record_extra_pos(expr.operator_pos);
        default_visit_member_expression(self, expr);
    }

    walk_node!(
        visit_chain_expression,
        TAG_CHAIN,
        ast::ChainExpression<'a>,
        default_visit_chain_expression,
        NodeKind::Other
    );
    walk_node!(
        visit_conditional_expression,
        TAG_CONDITIONAL,
        ast::ConditionalExpression<'a>,
        default_visit_conditional_expression,
        NodeKind::Other
    );

    fn visit_call_expression(&mut self, expr: &mut ast::CallExpression<'a>) {
        self.tag(TAG_CALL);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        self.boolean(expr.is_optional);
        self.boolean(expr.maybe_eval_in_function);
        self.boolean(expr.maybe_eval_in_method);
        self.boolean(expr.maybe_eval_in_static);
        self.boolean(expr.maybe_eval_in_derived_constructor);
        self.boolean(expr.maybe_eval_in_static_initializer);
        self.boolean(expr.maybe_eval_in_class_field_initializer);
        self.boolean(expr.maybe_eval_in_arrow_function);
        default_visit_call_expression(self, expr);
    }

    fn visit_call_argument(&mut self, argument: &mut ast::CallArgument<'a>) {
        self.tag(TAG_CALL_ARG);
        match argument {
            ast::CallArgument::Expression(_) => self.u8(0),
            ast::CallArgument::Spread(_) => self.u8(1),
        }
        default_visit_call_argument(self, argument);
    }

    walk_node!(
        visit_new_expression,
        TAG_NEW,
        ast::NewExpression<'a>,
        default_visit_new_expression,
        NodeKind::Other
    );
    walk_node!(
        visit_sequence_expression,
        TAG_SEQUENCE,
        ast::SequenceExpression<'a>,
        default_visit_sequence_expression,
        NodeKind::Other
    );

    fn visit_this_expression(&mut self, expr: &mut ast::ThisExpression<'a>) {
        self.tag(TAG_THIS);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        self.hash_resolved_scope(&expr.scope);
    }

    fn visit_array_expression(&mut self, expr: &mut ast::ArrayExpression<'a>) {
        self.tag(TAG_ARRAY);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        self.boolean(expr.is_parenthesized);
        for element in expr.elements.iter_mut() {
            match element {
                ast::ArrayElement::Expression(e) => {
                    self.u8(0);
                    self.visit_expression(e);
                }
                ast::ArrayElement::Spread(s) => {
                    self.u8(1);
                    self.visit_spread_element(s);
                }
                ast::ArrayElement::Hole(pos) => {
                    self.u8(2);
                    self.record_extra_pos(*pos);
                }
            }
        }
    }

    walk_node!(
        visit_spread_element,
        TAG_SPREAD,
        ast::SpreadElement<'a>,
        default_visit_spread_element,
        NodeKind::Other
    );

    fn visit_object_expression(&mut self, expr: &mut ast::ObjectExpression<'a>) {
        self.tag(TAG_OBJECT);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        self.boolean(expr.is_parenthesized);
        self.hash_scope_ref(expr.scope.as_ref().id());
        default_visit_object_expression(self, expr);
    }

    fn visit_property(&mut self, prop: &mut ast::Property<'a>) {
        self.tag(TAG_PROPERTY);
        self.ordinal(prop as *mut _ as *const u8, prop.loc, NodeKind::Other);
        self.boolean(prop.is_computed);
        self.boolean(prop.is_method);
        match &prop.kind {
            ast::PropertyKind::Init => self.u8(0),
            ast::PropertyKind::Get => self.u8(1),
            ast::PropertyKind::Set => self.u8(2),
            ast::PropertyKind::Spread(has_comma) => {
                self.u8(3);
                self.boolean(*has_comma);
            }
            ast::PropertyKind::PatternInitializer(_) => self.u8(4),
        }
        default_visit_property(self, prop);

        // How to rebuild the name of a method or accessor with a named key: the key node's
        // ordinal plus an accessor prefix.
        if prop.is_method && !prop.is_computed {
            if let Some(ast::Expression::Function(func)) = &prop.value {
                let key_ptr = match &prop.key {
                    ast::Expression::Id(id) => id.as_ref() as *const ast::Identifier as *const u8,
                    ast::Expression::String(s) => {
                        s.as_ref() as *const ast::StringLiteral as *const u8
                    }
                    _ => std::ptr::null(),
                };
                if let Some(key_ordinal) = self.recent_ordinal(key_ptr) {
                    let prefix = match prop.kind {
                        ast::PropertyKind::Get => 1,
                        ast::PropertyKind::Set => 2,
                        _ => 0,
                    };
                    self.method_keys.insert(
                        func.as_ref() as *const ast::Function as *const u8,
                        (key_ordinal, prefix),
                    );
                }
            }
        }
    }

    fn visit_function_expression(&mut self, func: &mut ast::Function<'a>) {
        self.fn_placeholder(func, TAG_FUNC_EXPR);
    }

    fn visit_arrow_function(&mut self, func: &mut ast::Function<'a>) {
        self.fn_placeholder(func, TAG_ARROW);
    }

    fn visit_class_expression(&mut self, class: &mut ast::Class<'a>) {
        self.tag(TAG_CLASS_EXPR);
        self.visit_class(class);
    }

    walk_node!(
        visit_await_expression,
        TAG_AWAIT,
        ast::AwaitExpression<'a>,
        default_visit_await_expression,
        NodeKind::Other
    );

    fn visit_yield_expression(&mut self, expr: &mut ast::YieldExpression<'a>) {
        self.tag(TAG_YIELD);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        self.boolean(expr.is_delegate);
        default_visit_yield_expression(self, expr);
    }

    fn visit_super_member_expression(&mut self, expr: &mut ast::SuperMemberExpression<'a>) {
        self.tag(TAG_SUPER_MEMBER);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        self.boolean(expr.is_computed);
        self.boolean(expr.is_static);
        self.hash_resolved_scope(&expr.this_scope);
        self.hash_resolved_scope(&expr.home_object_scope);
        self.record_extra_pos(expr.operator_pos);
        self.record_extra_pos(expr.super_.start);
        default_visit_super_member_expression(self, expr);
    }

    fn visit_super_call_expression(&mut self, expr: &mut ast::SuperCallExpression<'a>) {
        self.tag(TAG_SUPER_CALL);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        self.hash_resolved_scope(&expr.constructor_scope);
        self.hash_resolved_scope(&expr.new_target_scope);
        self.hash_resolved_scope(&expr.this_scope);
        self.record_extra_pos(expr.super_.start);
        default_visit_super_call_expression(self, expr);
    }

    fn visit_template_literal(&mut self, lit: &mut ast::TemplateLiteral<'a>) {
        self.tag(TAG_TEMPLATE);
        let ordinal = self.ordinal(lit as *mut _ as *const u8, lit.loc, NodeKind::TemplateLiteral);
        for (i, quasi) in lit.quasis.iter().enumerate() {
            if let Some(cooked) = quasi.cooked {
                self.quasi_nodes.push((ordinal, i as u32, cooked));
            }
        }
        default_visit_template_literal(self, lit);
    }

    fn visit_template_element(&mut self, elem: &mut ast::TemplateElement<'a>) {
        self.tag(TAG_TEMPLATE_ELEMENT);
        self.ordinal(elem as *mut _ as *const u8, elem.loc, NodeKind::Other);
        self.hash_symbol(elem.raw);
        match elem.cooked {
            Some(cooked) => {
                self.u8(1);
                self.hash_symbol(cooked);
            }
            None => self.u8(0),
        }
    }

    walk_node!(
        visit_tagged_template_expression,
        TAG_TAGGED_TEMPLATE,
        ast::TaggedTemplateExpression<'a>,
        default_visit_tagged_template_expression,
        NodeKind::Other
    );

    fn visit_meta_property(&mut self, expr: &mut ast::MetaProperty<'a>) {
        self.tag(TAG_META_PROPERTY);
        self.ordinal(expr as *mut _ as *const u8, expr.loc, NodeKind::Other);
        match &expr.kind {
            ast::MetaPropertyKind::NewTarget { scope } => {
                self.u8(0);
                self.hash_resolved_scope(scope);
            }
            ast::MetaPropertyKind::ImportMeta => self.u8(1),
        }
    }

    walk_node!(
        visit_import_expression,
        TAG_IMPORT_EXPR,
        ast::ImportExpression<'a>,
        default_visit_import_expression,
        NodeKind::Other
    );

    fn visit_identifier_pattern(&mut self, id: &mut ast::Identifier<'a>) {
        self.tag(TAG_ID_PATTERN);
        self.visit_identifier(id);
    }

    fn visit_array_pattern(&mut self, patt: &mut ast::ArrayPattern<'a>) {
        self.tag(TAG_ARRAY_PATTERN);
        self.ordinal(patt as *mut _ as *const u8, patt.loc, NodeKind::Other);
        for element in patt.elements.iter_mut() {
            match element {
                ast::ArrayPatternElement::Pattern(p) => {
                    self.u8(0);
                    self.visit_pattern(p);
                }
                ast::ArrayPatternElement::Rest(r) => {
                    self.u8(1);
                    self.visit_rest_element(r);
                }
                ast::ArrayPatternElement::Hole(pos) => {
                    self.u8(2);
                    self.record_extra_pos(*pos);
                }
            }
        }
    }

    walk_node!(
        visit_rest_element,
        TAG_REST,
        ast::RestElement<'a>,
        default_visit_rest_element,
        NodeKind::Other
    );
    walk_node!(
        visit_object_pattern,
        TAG_OBJECT_PATTERN,
        ast::ObjectPattern<'a>,
        default_visit_object_pattern,
        NodeKind::Other
    );

    fn visit_object_pattern_property(&mut self, prop: &mut ast::ObjectPatternProperty<'a>) {
        self.tag(TAG_OBJECT_PATTERN_PROPERTY);
        self.ordinal(prop as *mut _ as *const u8, prop.loc, NodeKind::Other);
        self.boolean(prop.is_computed);
        self.boolean(prop.is_rest);
        default_visit_object_pattern_property(self, prop);
    }

    walk_node!(
        visit_assignment_pattern,
        TAG_ASSIGN_PATTERN,
        ast::AssignmentPattern<'a>,
        default_visit_assignment_pattern,
        NodeKind::Other
    );

    fn visit_identifier(&mut self, id: &mut ast::Identifier<'a>) {
        self.tag(TAG_IDENTIFIER);
        let ordinal = self.ordinal(id as *mut _ as *const u8, id.loc, NodeKind::Identifier);
        self.identifier_names.push((ordinal, id.name));
        self.hash_symbol_node(id.name, ordinal);
        use ast::ResolvedScope;
        match id.scope.kind() {
            ResolvedScope::UnresolvedGlobal => self.u8(0),
            ResolvedScope::UnresolvedDynamic => self.u8(1),
            ResolvedScope::Resolved => {
                self.u8(2);
                let scope = id.scope.unwrap_resolved();
                if self.scope_ordinals.contains_key(&scope.id()) {
                    self.hash_scope_ref(scope.id());
                    self.hash_binding(id.get_binding());
                } else {
                    let prev = std::mem::replace(&mut self.h.cur, SECTION_EXTERNAL);
                    self.hash_external_scope_presence(scope.id());
                    self.slot_param = true;
                    self.hash_binding(id.get_binding());
                    self.slot_param = false;
                    self.h.cur = prev;
                }
            }
        }
    }
}

/// Whether a string text is a canonical array index (same rules as `parse_string_to_u32`):
/// all ASCII digits, no leading zero unless the text is exactly "0", and not u32::MAX.
fn is_array_index_text(text: &Wtf8Str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_digit() {
        return false;
    }
    if bytes[0] == b'0' && bytes.len() > 1 {
        return false;
    }
    let mut result: u64 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return false;
        }
        result = result * 10 + u64::from(b - b'0');
        if result > u32::MAX as u64 {
            return false;
        }
    }
    result != u32::MAX as u64
}

fn body_loc(body: &ast::FunctionBody) -> Loc {
    match body {
        ast::FunctionBody::Block(block) => block.loc,
        ast::FunctionBody::Expression(expr) => expr.expr.loc(),
    }
}

/// Compute the structural key of a function, along with the ordinal maps needed to re-read
/// source-derived values from its AST.
pub(crate) fn walk_function<'a>(
    func_info: FuncInfo<'a>,
    scope_tree: &'a ScopeTree<'a>,
    outer_scope: &Rc<ScopeStackNode>,
    secret: &KeySecret,
    partition: u128,
) -> Walk<'a> {
    let mut walker = KeyWalker {
        h: Sections::new(secret),
        symbols: FxHashMap::default(),
        node_ordinals: FxHashMap::default(),
        dedupe_nodes: func_info.class.is_some(),
        nodes: Vec::new(),
        scope_ordinals: FxHashMap::default(),
        scopes: Vec::new(),
        vm_ordinal: FxHashMap::default(),
        outer_chain: {
            let mut chain = Vec::new();
            let mut node = Some(Rc::clone(outer_scope));
            while let Some(n) = node {
                chain.push(n.scope_id);
                node = n.parent.clone();
            }
            chain
        },
        extra_positions: Vec::new(),
        extra_anchors: Vec::new(),
        slot_param: false,
        ext_scope_ordinals: FxHashMap::default(),
        ext_slots: Vec::new(),
        ext_slot_ordinals: FxHashMap::default(),
        identifier_names: Vec::new(),
        text_nodes: Vec::new(),
        quasi_nodes: Vec::new(),
        method_keys: FxHashMap::default(),
        scope_tree,
        uncacheable: None,
    };

    // The partition: functions of different sites never share a key.
    walker.u64(partition as u64);
    walker.u64((partition >> 64) as u64);

    // Function-level information that changes its bytecode.
    walker.tag(TAG_FUNCTION);
    walker.u8(match func_info.kind {
        FuncKind::Declaration => 0,
        FuncKind::Expression => 1,
        FuncKind::Arrow => 2,
        FuncKind::Method => 3,
        FuncKind::Constructor => 4,
    });
    walker.boolean(func_info.is_class_constructor);
    walker.boolean(func_info.is_base_constructor);
    walker.boolean(func_info.is_anonymous_default_export);
    walker.boolean(func_info.source_range_is_class);

    let func = func_info.node.as_ref();
    let func_addr = func as *const ast::Function<'a> as *const u8;
    walker.u16(func.flags.bits());
    walker.boolean(func.is_strict_mode());
    let (num_parameters, function_length) =
        crate::runtime::bytecode::generator::BytecodeFunctionGenerator::count_parameters_pub(func);
    walker.usize(num_parameters);
    walker.usize(function_length);
    walker.usize(func.scope.as_ref().num_local_registers());

    // The function's own scope subtree.
    walker.walk_scope(func.scope);

    // For constructors, walk the class node as well: the function's source range is the class's
    // range, and the class structure is part of what the constructor's bytecode depends on.
    let mut range_node: Option<u32> = None;
    if let Some(class) = func_info.class {
        // The mutable reference comes from the AstPtr, the way the generator obtains its own
        // (`AsMut`), never from a shared reference. SAFETY of the exclusivity: the walk runs
        // synchronously on this thread while the generator holds the function's AstPtrs only as
        // pointers; no other reference to the class is live, and the visitor does not write.
        let mut class_ptr = class;
        walker.visit_class(class_ptr.as_mut());
        range_node = walker.ordinal_of(class.as_arena_ref() as *const ast::Class as *const u8);
    }

    // The function's AST. The root function is walked in full; nested functions are placeholders.
    // The mutable reference comes from the AstPtr (`AsMut`), as in the generator. SAFETY of the
    // exclusivity: the walk is synchronous and no other reference to the function is live while
    // it runs (`func` above is only used as an address after it); the walker never writes.
    let mut func_ptr = func_info.node;
    walker.visit_function(func_ptr.as_mut());
    if range_node.is_none() {
        range_node = walker.ordinal_of(func_addr);
    }
    let range_node = match range_node {
        Some(ordinal) => ordinal,
        None => {
            walker.fail(UncacheableReason::Other);
            0
        }
    };

    walker.resolve_layout();
    let (key, sections) = walker.h.finish();

    Walk {
        key,
        partition,
        sections,
        func_info,
        scope_tree,
        node_ordinals: std::cell::OnceCell::new(),
        nodes: walker.nodes,
        scope_ordinals: walker.scope_ordinals,
        scopes: walker.scopes,
        vm_ordinal: walker.vm_ordinal,
        outer_chain: walker.outer_chain,
        extra_anchors: walker.extra_anchors,
        pos_map: std::cell::OnceCell::new(),
        identifier_names: walker.identifier_names,
        ext_slots: walker.ext_slots,
        ext_slot_ordinals: walker.ext_slot_ordinals,
        text_nodes: walker.text_nodes,
        quasi_nodes: walker.quasi_nodes,
        method_keys: walker.method_keys,
        range_node,
        uncacheable: walker.uncacheable,
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;
    use crate::common::options::OptionsBuilder;
    use crate::common::wtf_8::Wtf8String;
    use crate::parser::ParsedProgram;
    use crate::parser::source::Source;

    const TEST_SECRET: KeySecret = [7; 16];

    /// Recursively collect every function declaration in the statement list, with the VM scope
    /// ids of its enclosing functions (outermost first).
    fn collect_functions<'a>(
        stmts: &[&'a ast::Statement<'a>],
        stack: &mut Vec<&'a ast::Function<'a>>,
        out: &mut Vec<(&'a ast::Function<'a>, Vec<usize>)>,
    ) {
        for stmt in stmts {
            if let ast::Statement::FuncDecl(f) = *stmt {
                let func = f.as_ref();
                let chain: Vec<usize> = stack
                    .iter()
                    .filter_map(|f| f.scope.as_ref().vm_scope_id())
                    .collect();
                out.push((func, chain));
                stack.push(func);
                if let ast::FunctionBody::Block(body) = func.body.as_ref() {
                    let body_stmts: Vec<&ast::Statement> = body.body.iter().collect();
                    collect_functions(&body_stmts, stack, out);
                }
                stack.pop();
            }
        }
    }

    /// Parse a script and return the structural key of the function declaration with the given
    /// name.
    fn key_of_function(src: &str, name: &str) -> u128 {
        key_of_function_with_secret(src, name, &TEST_SECRET)
    }

    fn key_of_function_with_secret(src: &str, name: &str, secret: &KeySecret) -> u128 {
        let source = Rc::new(Source::new_for_string("test.js", Wtf8String::from_str(src)).unwrap());
        let options = Rc::new(OptionsBuilder::new().build().unwrap());
        let program = ParsedProgram::parse(source, options, /* is_module */ false).unwrap();
        let result = program.analyzed_result();
        let program_ast: &ast::Program = result.program.as_ref();
        let scope_tree: &ScopeTree = result.scope_tree.as_ref();

        let mut found: Vec<(&ast::Function, Vec<usize>)> = vec![];
        let mut stack: Vec<&ast::Function> = vec![];
        let toplevels: Vec<&ast::Statement> = program_ast
            .toplevels
            .iter()
            .filter_map(|t| match t {
                ast::Toplevel::Statement(s) => Some(s),
                _ => None,
            })
            .collect();
        collect_functions(&toplevels, &mut stack, &mut found);

        let global_vm = program_ast.scope.as_ref().vm_scope_id().unwrap();
        let (func, enclosing) = found
            .iter()
            .find(|(f, _)| {
                f.id.as_ref()
                    .map_or(false, |id| id.name.as_bytes() == name.as_bytes())
            })
            .unwrap_or_else(|| panic!("function {name} not found"));

        // Build the scope stack the generator would use: dummy root, global scope, then the
        // enclosing functions' scopes from outermost to innermost.
        let mut chain = Rc::new(ScopeStackNode { scope_id: usize::MAX, parent: None });
        chain = Rc::new(ScopeStackNode { scope_id: global_vm, parent: Some(chain) });
        for vm in enclosing {
            chain = Rc::new(ScopeStackNode { scope_id: *vm, parent: Some(chain) });
        }

        let info = FuncInfo {
            kind: FuncKind::Declaration,
            node: AstPtr::from_ref(*func),
            class: None,
            is_class_constructor: false,
            is_base_constructor: false,
            is_anonymous_default_export: false,
            source_range_is_class: false,
        };
        let walk = walk_function(info, scope_tree, &chain, secret, 0);
        assert!(walk.uncacheable.is_none(), "uncacheable: {:?}", walk.uncacheable);
        walk.key
    }

    #[test]
    fn same_function_has_different_keys_under_different_secrets() {
        let source = "function f(a, b) { return a + b; }";
        let one = key_of_function_with_secret(source, "f", &[1; 16]);
        let again = key_of_function_with_secret(source, "f", &[1; 16]);
        let two = key_of_function_with_secret(source, "f", &[2; 16]);
        assert_eq!(one, again);
        assert_ne!(one, two);
    }

    #[test]
    fn key_equal_under_renaming() {
        let a = key_of_function("function f(alpha, beta) { return alpha + beta; }", "f");
        let b = key_of_function("function g(x, y) { return x + y; }", "g");
        assert_eq!(a, b, "renaming all identifiers must not change the key");
    }

    #[test]
    fn key_equal_under_renaming_with_strings() {
        let a = key_of_function("function f(a) { return a.foo + \"hello\" + a.bar; }", "f");
        let b = key_of_function("function g(x) { return x.baz + \"world\" + x.qux; }", "g");
        assert_eq!(a, b, "renaming identifiers and string texts must not change the key");
    }

    #[test]
    fn key_differs_for_a_plus_b_vs_a_plus_a() {
        let a = key_of_function("function f(a, b) { return a + b; }", "f");
        let b = key_of_function("function f(a, b) { return a + a; }", "f");
        assert_ne!(a, b, "a+b and a+a must have different keys");
    }

    #[test]
    fn key_differs_for_strict_vs_sloppy() {
        let a = key_of_function("function f(a) { return a; }", "f");
        let b = key_of_function("\"use strict\"; function f(a) { return a; }", "f");
        assert_ne!(a, b, "strict vs sloppy must have different keys");
    }

    #[test]
    fn key_differs_for_different_captured_slots() {
        // The inner function captures a different slot of the outer function in each case.
        let a = key_of_function(
            "function outer(a, b) { function inner() { return a; } return inner; }",
            "inner",
        );
        let b = key_of_function(
            "function outer(a, b) { function inner() { return b; } return inner; }",
            "inner",
        );
        assert_ne!(a, b, "capturing different slots must change the key");
    }

    #[test]
    fn key_equal_for_same_captured_slot_under_renaming() {
        let a = key_of_function(
            "function outer(alpha, beta) { function inner() { return alpha; } return inner; }",
            "inner",
        );
        let b = key_of_function(
            "function outer(x, y) { function inner() { return x; } return inner; }",
            "inner",
        );
        assert_eq!(a, b, "renaming must not change the key of a nested function");
    }

    #[test]
    fn key_differs_for_different_structure() {
        let a = key_of_function("function f(a) { return a + 1; }", "f");
        let b = key_of_function("function f(a) { return a - 1; }", "f");
        let c = key_of_function("function f(a) { return a + 1; } function g() {}", "f");
        assert_ne!(a, b);
        assert_eq!(a, c, "extra toplevel functions must not change a function's key");
    }

    #[test]
    fn key_differs_for_different_numbers() {
        let a = key_of_function("function f() { return 1.5; }", "f");
        let b = key_of_function("function f() { return 2.5; }", "f");
        assert_ne!(a, b, "number literals are part of the key verbatim");
    }
}
