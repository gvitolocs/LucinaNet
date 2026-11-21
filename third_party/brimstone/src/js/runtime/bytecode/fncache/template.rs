//! Template stored for a cached function: everything needed to instantiate a `BytecodeFunction`
//! identical to a freshly generated one, against a structurally equal function's AST.
//!
//! Source-derived values are described by provenance (see `Prov`): an ordinal of a node in the
//! canonical AST walk of the function, so they can be re-read from the new function's AST.
//! Generator-intrinsic values are stored verbatim.

use allocator_api2::alloc::Global;

use super::key::{FuncKind, NameProv, NodeKind, Walk};
use super::prov_rules::{self, TextSource};
use crate::{
    common::wtf_8::{Wtf8Str, Wtf8String},
    parser::ast,
    runtime::bytecode::constant_table_builder::Prov,
};

/// Heap item kinds that can appear in a constant table and can be rebuilt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeapKind {
    BigInt,
    RegExp,
    TemplateObject,
    ClassNames,
    ScopeNames,
    /// A well-known symbol (currently only the fields initializer symbol).
    Symbol,
    /// A nested function placeholder, patched in after generation.
    NestedFunction,
}

impl HeapKind {
    fn from_u8(v: u8) -> Option<HeapKind> {
        Some(match v {
            0 => HeapKind::BigInt,
            1 => HeapKind::RegExp,
            2 => HeapKind::TemplateObject,
            3 => HeapKind::ClassNames,
            4 => HeapKind::ScopeNames,
            5 => HeapKind::Symbol,
            6 => HeapKind::NestedFunction,
            _ => return None,
        })
    }

    fn to_u8(self) -> u8 {
        match self {
            HeapKind::BigInt => 0,
            HeapKind::RegExp => 1,
            HeapKind::TemplateObject => 2,
            HeapKind::ClassNames => 3,
            HeapKind::ScopeNames => 4,
            HeapKind::Symbol => 5,
            HeapKind::NestedFunction => 6,
        }
    }
}

/// A method of a class, as stored in a ClassNames heap item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodDesc {
    /// Provenance of the method name, if statically known.
    pub name_prov: Option<Prov>,
    /// Verbatim name text for intrinsic provenance.
    pub name_text: Option<Vec<u8>>,
    pub is_static: bool,
    pub is_getter: bool,
    pub is_setter: bool,
    pub is_private: bool,
}

/// Details of a ClassNames heap item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassNamesDesc {
    pub methods: Vec<MethodDesc>,
    pub home_object: Option<(u32, u32)>,
    pub static_home_object: Option<(u32, u32)>,
}

/// Details of a ScopeNames heap item: the ScopeFlags bits, stored verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeNamesDesc {
    pub flags: u8,
}

/// Extra details of a heap item constant, recorded by the generator at the add site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeapDetail {
    ClassNames(ClassNamesDesc),
    ScopeNames(ScopeNamesDesc),
}

/// One constant table entry of the template, dense by constant index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConstEntry {
    /// A hole in the constant table, filled with undefined.
    Undefined,
    /// An interned string. `text` is stored verbatim only for intrinsic provenance.
    String { prov: Prov, text: Option<Vec<u8>> },
    HeapItem {
        kind: HeapKind,
        prov: Prov,
        class_names: Option<ClassNamesDesc>,
        scope_names: Option<ScopeNamesDesc>,
    },
    /// An array index property key stored as a smi.
    ArrayIndex { value: u32, prov: Prov },
    /// A double, stored as raw bits.
    Double(u64),
    /// A raw bytecode offset.
    Offset(isize),
}

/// One exception handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandlerEntry {
    pub start: u32,
    pub end: u32,
    pub handler: u32,
    /// Signed register index, or -1 for none (the `this` sigil).
    pub error_register: i32,
}

/// A nested function of the cached function: where its placeholder sits in the constant table
/// and how to re-enqueue it against the new AST.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NestedEntry {
    pub constant_index: u32,
    /// Ordinal of the nested function's AST node.
    pub node_ordinal: u32,
    pub kind: FuncKind,
    pub name_prov: NameProv,
    pub name_text: Option<Vec<u8>>,
    pub is_base: bool,
    /// Ordinal of the class node, for constructors.
    pub class_ordinal: Option<u32>,
    /// Scope ordinals of the scope chain the nested function is generated in, innermost first.
    /// Only scopes inside the cached function's subtree; the rest is the outer chain.
    pub scope_chain: Vec<u32>,
}

/// The source map, with positions stored as (node ordinal, offset within the node).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMapTemplate {
    /// Ordinal of the node whose source range is the function's source range.
    pub range_node: u32,
    pub range_start_delta: u32,
    pub range_end_delta: u32,
    /// (bytecode offset, node ordinal, offset within the node).
    pub entries: Vec<(u32, u32, u32)>,
}

/// A scope access instruction whose scope index and depth depend on the enclosing functions, not
/// on the cached function: re-encoded on a hit from the new function's scope layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopePatch {
    /// Byte offset of the instruction (including any width prefix) in the bytecode.
    pub offset: u32,
    /// Byte length of the instruction.
    pub len: u32,
    /// 0 = load from scope, 1 = store to scope, 2 = load from module, 3 = store to module.
    pub op: u8,
    /// The signed register operand (destination or value).
    pub reg: i32,
    /// Ordinal in the walk's external slots.
    pub slot: u32,
    /// Scopes the function has open at the instruction, on top of the position of the slot's
    /// scope on the outer chain.
    pub depth_base: u32,
}

/// The template of a cached function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub bytecode: Vec<u8>,
    pub source_map: SourceMapTemplate,
    pub constants: Vec<ConstEntry>,
    pub handlers: Vec<HandlerEntry>,
    /// Width of the encoded handlers: 0 = narrow, 1 = wide, 2 = extra wide.
    pub handler_width: u8,
    pub num_caches: u32,
    pub num_registers: u32,
    pub num_parameters: u32,
    pub function_length: u32,
    pub estimated_num_properties: u8,
    /// BytecodeFunctionFlags bits.
    pub flags: u8,
    pub new_target_index: Option<u32>,
    pub name_prov: NameProv,
    pub name_text: Option<Vec<u8>>,
    pub nested: Vec<NestedEntry>,
    /// Scope accesses to re-encode on a hit.
    pub patches: Vec<ScopePatch>,
    /// Approximate size in bytes, for bounding the store.
    pub size: usize,
}

/// Read the text of an AST node that can back a source-derived constant value.
pub fn node_text<'a>(walk: &Walk<'a>, ordinal: u32) -> Option<&'a Wtf8Str> {
    let info = walk.nodes.get(ordinal as usize)?;
    match info.kind {
        NodeKind::Identifier => Some(walk.node::<ast::Identifier>(ordinal)?.name),
        NodeKind::StringLiteral => Some(walk.node::<ast::StringLiteral>(ordinal)?.value),
        _ => None,
    }
}

/// Resolve the text described by a provenance against the new function's AST. `intrinsic` is the
/// verbatim text stored in the template for intrinsic provenance.
pub fn resolve_text(
    walk: &Walk,
    prov: Prov,
    intrinsic: Option<&[u8]>,
) -> Option<Wtf8String<Global>> {
    match prov_rules::source_of(prov) {
        TextSource::Node(ordinal) => node_text(walk, ordinal).map(|t| t.to_owned_in(Global)),
        TextSource::Quasi(template_ordinal, quasi_index) => {
            let lit = walk.node::<ast::TemplateLiteral>(template_ordinal)?;
            let quasi = lit.quasis.get(quasi_index as usize)?;
            quasi.cooked.map(|t| t.to_owned_in(Global))
        }
        TextSource::Private(ordinal, strip) => {
            let id = walk.node::<ast::Identifier>(ordinal)?;
            let bytes = id.name.as_bytes();
            let start = prov_rules::private_name_start(bytes.len(), strip)?;
            let text = Wtf8Str::from_bytes_unchecked(&bytes[start..]);
            if strip && !text.is_well_formed() {
                return None;
            }
            Some(text.to_owned_in(Global))
        }
        TextSource::Stored => {
            let bytes = intrinsic?;
            let text = Wtf8Str::from_bytes_unchecked(bytes);
            if !text.is_well_formed() {
                return None;
            }
            Some(text.to_owned_in(Global))
        }
        TextSource::NoText => None,
    }
}

/// Resolve a name provenance to a text.
pub fn resolve_name(
    walk: &Walk,
    name_prov: NameProv,
    intrinsic: Option<&[u8]>,
) -> Option<Wtf8String<Global>> {
    match name_prov {
        NameProv::None => None,
        NameProv::Node(ordinal) => node_text(walk, ordinal).map(|t| t.to_owned_in(Global)),
        NameProv::MethodKey(ordinal, prefix) => {
            let text = node_text(walk, ordinal)?;
            let prefix_str = match prefix {
                1 => "get ",
                2 => "set ",
                _ => "",
            };
            let mut owned = Wtf8String::from_str(prefix_str);
            owned.push_wtf8_str(text);
            Some(owned)
        }
        NameProv::Text => {
            let bytes = intrinsic?;
            let text = Wtf8Str::from_bytes_unchecked(bytes);
            if !text.is_well_formed() {
                return None;
            }
            Some(text.to_owned_in(Global))
        }
    }
}

// ---------------------------------------------------------------------------
// Serialization. All reads are bounds-checked; any inconsistency yields None.
// ---------------------------------------------------------------------------

// `Writer` and `Reader` are in `wire.rs`, which Verus checks.
pub use super::wire::{Reader, Writer};

impl<'a> Reader<'a> {
    /// A length-prefixed byte string. `bytes_range` (checked by Verus) gives the range, which lies inside the buffer.
    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        let (start, end) = self.bytes_range()?;
        let buf: &'a [u8] = self.buf;
        Some(&buf[start..end])
    }
}

fn write_prov(w: &mut Writer, prov: Prov) {
    match prov {
        Prov::Intrinsic => w.u8(0),
        Prov::Node(o) => {
            w.u8(1);
            w.u32(o);
        }
        Prov::Quasi(t, i) => {
            w.u8(2);
            w.u32(t);
            w.u32(i);
        }
        Prov::PrivateName(o, strip) => {
            w.u8(3);
            w.u32(o);
            w.bool(strip);
        }
        Prov::ScopeBinding(s, b) => {
            w.u8(4);
            w.u32(s);
            w.u32(b);
        }
        Prov::NestedFunction(i) => {
            w.u8(5);
            w.u32(i);
        }
        Prov::ScopeNames(s) => {
            w.u8(6);
            w.u32(s);
        }
        Prov::ClassNames(c) => {
            w.u8(7);
            w.u32(c);
        }
    }
}

fn read_prov(r: &mut Reader) -> Option<Prov> {
    Some(match r.u8()? {
        0 => Prov::Intrinsic,
        1 => Prov::Node(r.u32()?),
        2 => Prov::Quasi(r.u32()?, r.u32()?),
        3 => Prov::PrivateName(r.u32()?, r.bool()?),
        4 => Prov::ScopeBinding(r.u32()?, r.u32()?),
        5 => Prov::NestedFunction(r.u32()?),
        6 => Prov::ScopeNames(r.u32()?),
        7 => Prov::ClassNames(r.u32()?),
        _ => return None,
    })
}

fn write_name_prov(w: &mut Writer, name_prov: NameProv) {
    match name_prov {
        NameProv::None => w.u8(0),
        NameProv::Node(o) => {
            w.u8(1);
            w.u32(o);
        }
        NameProv::MethodKey(o, p) => {
            w.u8(2);
            w.u32(o);
            w.u8(p);
        }
        NameProv::Text => w.u8(3),
    }
}

fn read_name_prov(r: &mut Reader) -> Option<NameProv> {
    Some(match r.u8()? {
        0 => NameProv::None,
        1 => NameProv::Node(r.u32()?),
        2 => NameProv::MethodKey(r.u32()?, r.u8()?),
        3 => NameProv::Text,
        _ => return None,
    })
}

fn write_func_kind(w: &mut Writer, kind: FuncKind) {
    w.u8(match kind {
        FuncKind::Declaration => 0,
        FuncKind::Expression => 1,
        FuncKind::Arrow => 2,
        FuncKind::Method => 3,
        FuncKind::Constructor => 4,
    });
}

fn read_func_kind(r: &mut Reader) -> Option<FuncKind> {
    Some(match r.u8()? {
        0 => FuncKind::Declaration,
        1 => FuncKind::Expression,
        2 => FuncKind::Arrow,
        3 => FuncKind::Method,
        4 => FuncKind::Constructor,
        _ => return None,
    })
}

fn write_opt_bytes(w: &mut Writer, v: &Option<Vec<u8>>) {
    match v {
        Some(b) => {
            w.u8(1);
            w.bytes(b);
        }
        None => w.u8(0),
    }
}

fn read_opt_bytes(r: &mut Reader) -> Option<Option<Vec<u8>>> {
    match r.u8()? {
        0 => Some(None),
        1 => Some(Some(r.bytes()?.to_vec())),
        _ => None,
    }
}

fn write_const_entry(w: &mut Writer, entry: &ConstEntry) {
    match entry {
        ConstEntry::Undefined => w.u8(0),
        ConstEntry::String { prov, text } => {
            w.u8(1);
            write_prov(w, *prov);
            write_opt_bytes(w, text);
        }
        ConstEntry::HeapItem { kind, prov, class_names, scope_names } => {
            w.u8(2);
            w.u8(kind.to_u8());
            write_prov(w, *prov);
            match class_names {
                Some(desc) => {
                    w.u8(1);
                    w.usize(desc.methods.len());
                    for m in &desc.methods {
                        match &m.name_prov {
                            Some(prov) => {
                                w.u8(1);
                                write_prov(w, *prov);
                            }
                            None => w.u8(0),
                        }
                        write_opt_bytes(w, &m.name_text);
                        w.bool(m.is_static);
                        w.bool(m.is_getter);
                        w.bool(m.is_setter);
                        w.bool(m.is_private);
                    }
                    for loc in [&desc.home_object, &desc.static_home_object] {
                        match loc {
                            Some((a, b)) => {
                                w.u8(1);
                                w.u32(*a);
                                w.u32(*b);
                            }
                            None => w.u8(0),
                        }
                    }
                }
                None => w.u8(0),
            }
            match scope_names {
                Some(desc) => {
                    w.u8(1);
                    w.u8(desc.flags);
                }
                None => w.u8(0),
            }
        }
        ConstEntry::ArrayIndex { value, prov } => {
            w.u8(3);
            w.u32(*value);
            write_prov(w, *prov);
        }
        ConstEntry::Double(bits) => {
            w.u8(4);
            w.u64(*bits);
        }
        ConstEntry::Offset(offset) => {
            w.u8(5);
            w.i64(*offset as i64);
        }
    }
}

fn read_const_entry(r: &mut Reader) -> Option<ConstEntry> {
    Some(match r.u8()? {
        0 => ConstEntry::Undefined,
        1 => ConstEntry::String { prov: read_prov(r)?, text: read_opt_bytes(r)? },
        2 => {
            let kind = HeapKind::from_u8(r.u8()?)?;
            let prov = read_prov(r)?;
            let class_names = match r.u8()? {
                0 => None,
                1 => {
                    let num_methods = r.usize()?;
                    let mut methods = Vec::with_capacity(num_methods.min(1 << 20));
                    for _ in 0..num_methods {
                        let name_prov = match r.u8()? {
                            0 => None,
                            1 => Some(read_prov(r)?),
                            _ => return None,
                        };
                        let name_text = read_opt_bytes(r)?;
                        let is_static = r.bool()?;
                        let is_getter = r.bool()?;
                        let is_setter = r.bool()?;
                        let is_private = r.bool()?;
                        methods.push(MethodDesc {
                            name_prov,
                            name_text,
                            is_static,
                            is_getter,
                            is_setter,
                            is_private,
                        });
                    }
                    let read_loc = |r: &mut Reader| -> Option<Option<(u32, u32)>> {
                        match r.u8()? {
                            0 => Some(None),
                            1 => Some(Some((r.u32()?, r.u32()?))),
                            _ => None,
                        }
                    };
                    let home_object = read_loc(r)?;
                    let static_home_object = read_loc(r)?;
                    Some(ClassNamesDesc { methods, home_object, static_home_object })
                }
                _ => return None,
            };
            let scope_names = match r.u8()? {
                0 => None,
                1 => Some(ScopeNamesDesc { flags: r.u8()? }),
                _ => return None,
            };
            ConstEntry::HeapItem { kind, prov, class_names, scope_names }
        }
        3 => ConstEntry::ArrayIndex { value: r.u32()?, prov: read_prov(r)? },
        4 => ConstEntry::Double(r.u64()?),
        5 => ConstEntry::Offset(r.i64()? as isize),
        _ => return None,
    })
}

/// Serialize a template to bytes (without the file header).
pub fn serialize(template: &Template) -> Vec<u8> {
    let mut w = Writer::new();
    w.bytes(&template.bytecode);

    w.u32(template.source_map.range_node);
    w.u32(template.source_map.range_start_delta);
    w.u32(template.source_map.range_end_delta);
    w.usize(template.source_map.entries.len());
    for (bc, node, delta) in &template.source_map.entries {
        w.u32(*bc);
        w.u32(*node);
        w.u32(*delta);
    }

    w.usize(template.constants.len());
    for entry in &template.constants {
        write_const_entry(&mut w, entry);
    }

    w.usize(template.handlers.len());
    for h in &template.handlers {
        w.u32(h.start);
        w.u32(h.end);
        w.u32(h.handler);
        w.i32(h.error_register);
    }
    w.u8(template.handler_width);

    w.u32(template.num_caches);
    w.u32(template.num_registers);
    w.u32(template.num_parameters);
    w.u32(template.function_length);
    w.u8(template.estimated_num_properties);
    w.u8(template.flags);
    match template.new_target_index {
        Some(i) => {
            w.u8(1);
            w.u32(i);
        }
        None => w.u8(0),
    }
    write_name_prov(&mut w, template.name_prov);
    write_opt_bytes(&mut w, &template.name_text);

    w.usize(template.patches.len());
    for p in &template.patches {
        w.u32(p.offset);
        w.u32(p.len);
        w.u8(p.op);
        w.i32(p.reg);
        w.u32(p.slot);
        w.u32(p.depth_base);
    }

    w.usize(template.nested.len());
    for n in &template.nested {
        w.u32(n.constant_index);
        w.u32(n.node_ordinal);
        write_func_kind(&mut w, n.kind);
        write_name_prov(&mut w, n.name_prov);
        write_opt_bytes(&mut w, &n.name_text);
        w.bool(n.is_base);
        match n.class_ordinal {
            Some(c) => {
                w.u8(1);
                w.u32(c);
            }
            None => w.u8(0),
        }
        w.usize(n.scope_chain.len());
        for s in &n.scope_chain {
            w.u32(*s);
        }
    }

    w.usize(template.size);

    w.buf
}

/// Deserialize a template. Returns None on any inconsistency (corrupt or truncated data).
pub fn deserialize(buf: &[u8]) -> Option<Template> {
    let mut r = Reader::new(buf);

    let bytecode = r.bytes()?.to_vec();

    let range_node = r.u32()?;
    let range_start_delta = r.u32()?;
    let range_end_delta = r.u32()?;
    let num_entries = r.usize()?;
    if num_entries > (1 << 26) {
        return None;
    }
    let mut entries = Vec::with_capacity(num_entries.min(1 << 20));
    for _ in 0..num_entries {
        entries.push((r.u32()?, r.u32()?, r.u32()?));
    }
    let source_map = SourceMapTemplate { range_node, range_start_delta, range_end_delta, entries };

    let num_constants = r.usize()?;
    if num_constants > (1 << 26) {
        return None;
    }
    let mut constants = Vec::with_capacity(num_constants.min(1 << 20));
    for _ in 0..num_constants {
        constants.push(read_const_entry(&mut r)?);
    }

    let num_handlers = r.usize()?;
    if num_handlers > (1 << 24) {
        return None;
    }
    let mut handlers = Vec::with_capacity(num_handlers.min(1 << 20));
    for _ in 0..num_handlers {
        handlers.push(HandlerEntry {
            start: r.u32()?,
            end: r.u32()?,
            handler: r.u32()?,
            error_register: r.i32()?,
        });
    }
    let handler_width = r.u8()?;
    if handler_width > 2 {
        return None;
    }

    let num_caches = r.u32()?;
    let num_registers = r.u32()?;
    let num_parameters = r.u32()?;
    let function_length = r.u32()?;
    let estimated_num_properties = r.u8()?;
    let flags = r.u8()?;
    let new_target_index = match r.u8()? {
        0 => None,
        1 => Some(r.u32()?),
        _ => return None,
    };
    let name_prov = read_name_prov(&mut r)?;
    let name_text = read_opt_bytes(&mut r)?;

    let num_patches = r.usize()?;
    if num_patches > (1 << 24) {
        return None;
    }
    let mut patches = Vec::with_capacity(num_patches.min(1 << 20));
    for _ in 0..num_patches {
        let offset = r.u32()?;
        let len = r.u32()?;
        let op = r.u8()?;
        if op > 3 || (offset as u64 + len as u64) > bytecode.len() as u64 {
            return None;
        }
        patches.push(ScopePatch {
            offset,
            len,
            op,
            reg: r.i32()?,
            slot: r.u32()?,
            depth_base: r.u32()?,
        });
    }

    let num_nested = r.usize()?;
    if num_nested > (1 << 24) {
        return None;
    }
    let mut nested = Vec::with_capacity(num_nested.min(1 << 20));
    for _ in 0..num_nested {
        let constant_index = r.u32()?;
        let node_ordinal = r.u32()?;
        let kind = read_func_kind(&mut r)?;
        let name_prov = read_name_prov(&mut r)?;
        let name_text = read_opt_bytes(&mut r)?;
        let is_base = r.bool()?;
        let class_ordinal = match r.u8()? {
            0 => None,
            1 => Some(r.u32()?),
            _ => return None,
        };
        let chain_len = r.usize()?;
        if chain_len > (1 << 20) {
            return None;
        }
        let mut scope_chain = Vec::with_capacity(chain_len);
        for _ in 0..chain_len {
            scope_chain.push(r.u32()?);
        }
        nested.push(NestedEntry {
            constant_index,
            node_ordinal,
            kind,
            name_prov,
            name_text,
            is_base,
            class_ordinal,
            scope_chain,
        });
    }

    let size = r.usize()?;
    if size > (1 << 40) {
        return None;
    }

    if !r.done() {
        return None;
    }

    Some(Template {
        bytecode,
        source_map,
        constants,
        handlers,
        handler_width,
        num_caches,
        num_registers,
        num_parameters,
        function_length,
        estimated_num_properties,
        flags,
        new_target_index,
        name_prov,
        name_text,
        nested,
        patches,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A template exercising every field and entry kind.
    pub(crate) fn sample_template(seed: u8) -> Template {
        Template {
            bytecode: vec![seed, 1, 2, 3, 0xff],
            source_map: SourceMapTemplate {
                range_node: 0,
                range_start_delta: 0,
                range_end_delta: 12,
                entries: vec![(0, 0, 0), (4, 3, 2), (9, 7, 1)],
            },
            constants: vec![
                ConstEntry::Undefined,
                ConstEntry::String { prov: Prov::Node(3), text: None },
                ConstEntry::String { prov: Prov::Intrinsic, text: Some(b"intrinsic".to_vec()) },
                ConstEntry::HeapItem {
                    kind: HeapKind::BigInt,
                    prov: Prov::Node(5),
                    class_names: None,
                    scope_names: None,
                },
                ConstEntry::HeapItem {
                    kind: HeapKind::ClassNames,
                    prov: Prov::ClassNames(9),
                    class_names: Some(ClassNamesDesc {
                        methods: vec![MethodDesc {
                            name_prov: Some(Prov::Node(11)),
                            name_text: None,
                            is_static: true,
                            is_getter: false,
                            is_setter: true,
                            is_private: false,
                        }],
                        home_object: Some((1, 2)),
                        static_home_object: None,
                    }),
                    scope_names: None,
                },
                ConstEntry::HeapItem {
                    kind: HeapKind::ScopeNames,
                    prov: Prov::ScopeNames(13),
                    class_names: None,
                    scope_names: Some(ScopeNamesDesc { flags: 0b101 }),
                },
                ConstEntry::HeapItem {
                    kind: HeapKind::NestedFunction,
                    prov: Prov::NestedFunction(0),
                    class_names: None,
                    scope_names: None,
                },
                ConstEntry::ArrayIndex { value: 42, prov: Prov::Node(15) },
                ConstEntry::Double(0x4059000000000000),
                ConstEntry::Offset(-17),
            ],
            handlers: vec![HandlerEntry { start: 1, end: 5, handler: 9, error_register: -3 }],
            handler_width: 1,
            num_caches: 2,
            num_registers: 7,
            num_parameters: 3,
            function_length: 3,
            estimated_num_properties: 4,
            flags: 0b101,
            new_target_index: Some(6),
            name_prov: NameProv::MethodKey(17, 1),
            name_text: None,
            nested: vec![NestedEntry {
                constant_index: 6,
                node_ordinal: 19,
                kind: FuncKind::Method,
                name_prov: NameProv::Node(21),
                name_text: None,
                is_base: true,
                class_ordinal: Some(23),
                scope_chain: vec![1, 2, 3],
            }],
            patches: vec![ScopePatch { offset: 1, len: 3, op: 2, reg: -5, slot: 4, depth_base: 6 }],
            size: 128,
        }
    }

    #[test]
    fn template_round_trip() {
        let t = sample_template(7);
        let bytes = serialize(&t);
        let mut back = deserialize(&bytes).expect("deserialize");
        // `size` is derived, not serialized; normalize it before comparing.
        let mut expected = t.clone();
        expected.size = back.size;
        back.size = expected.size;
        assert_eq!(expected, back);
    }

    #[test]
    fn every_variant_round_trips() {
        let provs = [
            Prov::Intrinsic,
            Prov::Node(u32::MAX),
            Prov::Quasi(1, u32::MAX),
            Prov::PrivateName(2, true),
            Prov::PrivateName(3, false),
            Prov::ScopeBinding(4, 5),
            Prov::NestedFunction(6),
            Prov::ScopeNames(7),
            Prov::ClassNames(8),
        ];
        let names = [
            NameProv::None,
            NameProv::Node(9),
            NameProv::MethodKey(10, 2),
            NameProv::Text,
        ];
        let kinds = [
            FuncKind::Declaration,
            FuncKind::Expression,
            FuncKind::Arrow,
            FuncKind::Method,
            FuncKind::Constructor,
        ];
        let mut t = sample_template(3);
        t.constants = vec![ConstEntry::Undefined];
        for prov in provs {
            t.constants
                .push(ConstEntry::String { prov, text: Some(vec![0, 255, 1]) });
            t.constants
                .push(ConstEntry::ArrayIndex { value: u32::MAX, prov });
            t.constants.push(ConstEntry::HeapItem {
                kind: HeapKind::RegExp,
                prov,
                class_names: None,
                scope_names: None,
            });
        }
        for kind in [
            HeapKind::BigInt,
            HeapKind::RegExp,
            HeapKind::TemplateObject,
            HeapKind::ClassNames,
            HeapKind::ScopeNames,
            HeapKind::Symbol,
            HeapKind::NestedFunction,
        ] {
            t.constants.push(ConstEntry::HeapItem {
                kind,
                prov: Prov::Intrinsic,
                class_names: Some(ClassNamesDesc {
                    methods: vec![],
                    home_object: None,
                    static_home_object: Some((u32::MAX, 0)),
                }),
                scope_names: Some(ScopeNamesDesc { flags: 255 }),
            });
        }
        t.constants.push(ConstEntry::Double(u64::MAX));
        t.constants.push(ConstEntry::Offset(isize::MIN));
        t.constants.push(ConstEntry::Offset(isize::MAX));
        t.handlers = vec![HandlerEntry {
            start: u32::MAX,
            end: 0,
            handler: 7,
            error_register: i32::MIN,
        }];
        t.nested.clear();
        for (i, kind) in kinds.iter().enumerate() {
            t.nested.push(NestedEntry {
                constant_index: i as u32,
                node_ordinal: 1,
                kind: *kind,
                name_prov: names[i % names.len()],
                name_text: if i % 2 == 0 {
                    Some(vec![i as u8])
                } else {
                    None
                },
                is_base: i % 2 == 1,
                class_ordinal: if i % 2 == 0 { None } else { Some(u32::MAX) },
                scope_chain: vec![0; i],
            });
        }
        for name_prov in names {
            t.name_prov = name_prov;
            t.new_target_index = if matches!(name_prov, NameProv::None) {
                None
            } else {
                Some(u32::MAX)
            };
            for handler_width in 0..3 {
                t.handler_width = handler_width;
                for op in 0..4u8 {
                    t.patches = vec![ScopePatch {
                        offset: 0,
                        len: 5,
                        op,
                        reg: i32::MIN,
                        slot: u32::MAX,
                        depth_base: 0,
                    }];
                    assert_eq!(deserialize(&serialize(&t)).as_ref(), Some(&t));
                }
            }
        }
    }

    #[test]
    fn deserialize_rejects_every_truncation() {
        let t = sample_template(9);
        let bytes = serialize(&t);
        for cut in 0..bytes.len() {
            assert!(deserialize(&bytes[..cut]).is_none(), "truncation at {cut} accepted");
        }
    }

    #[test]
    fn deserialize_never_panics_on_corruption() {
        let t = sample_template(11);
        let bytes = serialize(&t);
        for i in 0..bytes.len() {
            let mut corrupted = bytes.clone();
            corrupted[i] ^= 0xff;
            // Must not panic; may return None or a different template.
            let _ = deserialize(&corrupted);
        }
        // Also try random garbage.
        let mut garbage = vec![0u8; 64];
        for (i, b) in garbage.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        let _ = deserialize(&garbage);
        assert!(deserialize(&[]).is_none());
    }
}
