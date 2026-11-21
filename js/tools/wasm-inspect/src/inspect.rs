/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! One parser pass over a module's bytes, and the operator scan behind the
//! feature verdict.
//!
//! Nothing here instantiates, links or runs the module: `wasmparser` walks the
//! sections and every function body, and the report is what those reads found.
//! Sizes come from a walk of the top-level section headers (an id byte and a
//! LEB128 length, so a section's own bytes are counted with its header, which
//! is what `wat2wasm`-style listings mean by a section's size).

use crate::verdict::{self, Feature, Proposal};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use wasmparser::{
    BlockType, ConstExpr, Encoding, ExternalKind, Operator, Parser, Payload, TypeRef, ValType,
};

/// Section ids, as the binary format names them. The data count section has id
/// 12 even though it must appear before code (id 10): order and numbering part
/// ways there, and matching these ids to `wasmparser`'s own constants is what
/// keeps the table honest.
const SECTION_NAMES: [&str; 14] = [
    "custom",
    "type",
    "import",
    "function",
    "table",
    "memory",
    "global",
    "export",
    "start",
    "element",
    "code",
    "data",
    "data count",
    "tag",
];

fn section_kind(id: u8) -> String {
    match SECTION_NAMES.get(id as usize) {
        Some(name) => (*name).to_string(),
        None => format!("unknown({id})"),
    }
}

/// One top-level section: what it is, where it starts, how big it is.
#[derive(Clone, Debug)]
pub struct SectionRow {
    pub id: u8,
    pub kind: String,
    /// Only for custom sections.
    pub name: Option<String>,
    pub offset: usize,
    /// id byte + length field + contents.
    pub bytes: usize,
    /// Contents only.
    pub contents: usize,
}

/// An entry of the type section, in the flat (expanded) index space.
///
/// Without GC types every recursion group holds one type, so this is also the
/// binary's own type index space; when structural types are present the two
/// index spaces part ways and the indices shown are approximate. The report
/// says so as soon as it finds a structural type.
#[derive(Clone, Debug)]
pub struct TypeRow {
    pub index: usize,
    /// `func`, `struct`, `array` or `cont`.
    pub kind: String,
    /// `(i32, i32) -> i32` for a function type.
    pub signature: String,
    pub params: usize,
    pub results: usize,
    /// Part of a recursion group with more than one type, or has a supertype.
    pub gc: bool,
}

#[derive(Clone, Debug)]
pub struct ImportRow {
    pub module: String,
    pub name: String,
    /// `func`, `table`, `memory`, `global` or `tag`.
    pub kind: String,
    /// Signature for a function, limits for a table or memory, type for a global.
    pub detail: String,
    /// For a function import, the type it names.
    pub type_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ExportRow {
    pub name: String,
    pub kind: String,
    pub index: u32,
}

#[derive(Clone, Debug)]
pub struct TableRow {
    pub index: usize,
    pub imported: bool,
    pub elem_type: String,
    pub min: u64,
    pub max: Option<u64>,
    pub shared: bool,
    pub table64: bool,
}

#[derive(Clone, Debug)]
pub struct MemoryRow {
    pub index: usize,
    pub imported: bool,
    pub min: u64,
    pub max: Option<u64>,
    pub shared: bool,
    pub memory64: bool,
    pub page_size_log2: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct GlobalRow {
    pub index: usize,
    pub ty: String,
    pub mutable: bool,
    pub shared: bool,
    pub init: String,
}

#[derive(Clone, Debug)]
pub struct ElementRow {
    pub index: usize,
    /// `active table #0`, `passive` or `declared`.
    pub kind: String,
    pub elem_type: String,
    pub count: u64,
}

#[derive(Clone, Debug)]
pub struct DataRow {
    pub index: usize,
    /// `active memory #0 at 0x4000` or `passive`.
    pub kind: String,
    pub bytes: u64,
}

#[derive(Clone, Debug)]
pub struct CustomRow {
    pub name: String,
    pub offset: usize,
    pub bytes: usize,
    /// What the section turned out to hold, e.g. the producers fields.
    pub note: String,
}

/// Everything the operator scan found.
#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub bodies: u64,
    pub operators: u64,
    /// Proposal groups seen among the operators, from the parser's tagging.
    pub usages: BTreeMap<Proposal, Usage>,
    /// Function types with more than one result.
    pub multi_value_types: u64,
    pub first_multi_value_type: Option<(u64, String)>,
    /// `block`/`loop`/`if` described by a type with more than one result or any
    /// parameters.
    pub multi_value_blocks: u64,
    pub first_multi_value_block: Option<(u64, String)>,
    /// `i32.add`/`i64.add` inside a constant expression: what the
    /// extended-const proposal allows and MVP const expressions do not.
    pub extended_const: u64,
    pub first_extended_const: Option<(u64, String)>,
    /// Tags in the tag section or imported: exception handles need them.
    pub tags: u64,
    pub passive_data: u64,
    pub passive_elements: u64,
    pub data_count: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Usage {
    pub operators: u64,
    pub first: Option<(u64, String)>,
}

impl Scan {
    fn note(&mut self, proposal: Proposal, offset: u64, variant: &str) {
        let entry = self.usages.entry(proposal).or_insert(Usage {
            operators: 0,
            first: None,
        });
        entry.operators += 1;
        if entry.first.is_none() {
            entry.first = Some((offset, variant.to_string()));
        }
    }

    pub fn operators_of(&self, proposals: &[Proposal]) -> u64 {
        proposals
            .iter()
            .filter_map(|p| self.usages.get(p))
            .map(|u| u.operators)
            .sum()
    }

    fn first_of(&self, proposals: &[Proposal]) -> Option<(u64, String)> {
        proposals
            .iter()
            .filter_map(|p| self.usages.get(p))
            .filter_map(|u| u.first.as_ref())
            .min_by_key(|(offset, _)| *offset)
            .cloned()
    }
}

/// A whole module, as read.
#[derive(Clone, Debug)]
pub struct Report {
    pub file: String,
    pub size: usize,
    /// `core wasm module`, or what the header said.
    pub format: String,
    pub version: u16,
    pub sections: Vec<SectionRow>,
    pub types: Vec<TypeRow>,
    pub imports: Vec<ImportRow>,
    pub tables: Vec<TableRow>,
    pub memories: Vec<MemoryRow>,
    pub globals: Vec<GlobalRow>,
    pub exports: Vec<ExportRow>,
    pub elements: Vec<ElementRow>,
    pub data: Vec<DataRow>,
    pub custom: Vec<CustomRow>,
    pub start: Option<u32>,
    pub funcs_imported: u64,
    pub funcs_declared: u64,
    /// Type index of each defined function, in the function section's order.
    pub declared_types: Vec<u32>,
    /// Payloads of the component model: this tool is for core modules.
    pub component_payloads: u64,
    pub producers: Vec<(String, Vec<(String, String)>)>,
    pub target_features: Vec<String>,
    pub scan: Scan,
}

impl Report {
    pub fn new(bytes: &[u8], file: impl Into<String>) -> Result<Report, String> {
        let mut report = Report {
            file: file.into(),
            size: bytes.len(),
            format: String::new(),
            version: 0,
            sections: Vec::new(),
            types: Vec::new(),
            imports: Vec::new(),
            tables: Vec::new(),
            memories: Vec::new(),
            globals: Vec::new(),
            exports: Vec::new(),
            elements: Vec::new(),
            data: Vec::new(),
            custom: Vec::new(),
            start: None,
            funcs_imported: 0,
            funcs_declared: 0,
            declared_types: Vec::new(),
            component_payloads: 0,
            producers: Vec::new(),
            target_features: Vec::new(),
            scan: Scan::default(),
        };
        if bytes.len() < 8 {
            return Err("too short to hold a WebAssembly header".to_string());
        }
        if bytes[0..4] != [0x00, 0x61, 0x73, 0x6d] {
            return Err("no \\0asm magic: not a WebAssembly binary".to_string());
        }
        report.sections = section_table(bytes)?;
        report.walk(bytes)?;
        Ok(report)
    }

    /// Total functions addressable: imports first, then the function section.
    pub fn func_space(&self) -> u64 {
        self.funcs_imported + self.funcs_declared
    }

    pub fn signature(&self, type_index: Option<usize>) -> String {
        match type_index.and_then(|i| self.types.get(i)) {
            Some(t) => t.signature.clone(),
            None => "?".to_string(),
        }
    }

    /// Where a function index lives: imported or defined, and its type.
    pub fn describe_func(&self, index: u32) -> String {
        let i = u64::from(index);
        if i < self.funcs_imported {
            match self
                .imports
                .iter()
                .filter(|row| row.kind == "func")
                .nth(i as usize)
            {
                Some(row) => format!(
                    "#{index} = import {}::{} {}",
                    row.module,
                    row.name,
                    self.signature(row.type_index)
                ),
                None => format!("#{index} = imported"),
            }
        } else if i < self.func_space() {
            let local = (i - self.funcs_imported) as usize;
            match self.declared_types.get(local) {
                Some(ty) => format!(
                    "#{index} = defined function #{local}, type {ty} {}",
                    self.signature(Some(*ty as usize))
                ),
                None => format!("#{index} = defined function #{local}"),
            }
        } else {
            format!(
                "#{index} is out of range ({} imported + {} defined)",
                self.funcs_imported, self.funcs_declared
            )
        }
    }

    fn walk(&mut self, bytes: &[u8]) -> Result<(), String> {
        let parser = Parser::new(0);
        for payload in parser.parse_all(bytes) {
            let payload = payload.map_err(describe)?;
            match payload {
                Payload::Version { num, encoding, .. } => {
                    self.version = num;
                    self.format = match encoding {
                        Encoding::Module => "core wasm module".to_string(),
                        Encoding::Component => "component (not a core module)".to_string(),
                    };
                }
                Payload::TypeSection(section) => {
                    for item in section.into_iter_with_offsets() {
                        let (offset, group) = item.map_err(describe)?;
                        self.read_one_type(offset, &group);
                    }
                }
                Payload::ImportSection(section) => {
                    for item in section {
                        let import = item.map_err(describe)?;
                        self.read_one_import(import);
                    }
                }
                Payload::FunctionSection(section) => {
                    self.funcs_declared = u64::from(section.count());
                    for item in section {
                        let index = item.map_err(describe)?;
                        self.declared_types.push(index);
                    }
                }
                Payload::TableSection(section) => {
                    for item in section {
                        let table = item.map_err(describe)?;
                        self.push_table(&table.ty, false);
                    }
                }
                Payload::MemorySection(section) => {
                    for item in section {
                        let memory = item.map_err(describe)?;
                        self.push_memory(&memory, false);
                    }
                }
                Payload::TagSection(section) => {
                    for item in section {
                        item.map_err(describe)?;
                        self.scan.tags += 1;
                    }
                }
                Payload::GlobalSection(section) => {
                    for item in section {
                        let global = item.map_err(describe)?;
                        let init = self.const_expr_text(&global.init_expr);
                        self.globals.push(GlobalRow {
                            index: self.globals.len(),
                            ty: val_type(&global.ty.content_type),
                            mutable: global.ty.mutable,
                            shared: global.ty.shared,
                            init,
                        });
                    }
                }
                Payload::ExportSection(section) => {
                    for item in section {
                        let export = item.map_err(describe)?;
                        self.exports.push(ExportRow {
                            name: export.name.to_string(),
                            kind: external_kind(&export.kind).to_string(),
                            index: export.index,
                        });
                    }
                }
                Payload::StartSection { func, .. } => {
                    self.start = Some(func);
                }
                Payload::ElementSection(section) => {
                    for item in section {
                        let element = item.map_err(describe)?;
                        self.read_one_element(&element);
                    }
                }
                Payload::DataCountSection { count, .. } => {
                    self.scan.data_count = Some(u64::from(count));
                }
                Payload::CodeSectionEntry(body) => {
                    self.scan.bodies += 1;
                    let mut reader = body.get_operators_reader().map_err(describe)?;
                    while !reader.eof() {
                        let (op, offset) = reader.read_with_offset().map_err(describe)?;
                        self.scan_one_operator(&op, offset);
                    }
                }
                Payload::DataSection(section) => {
                    for item in section {
                        let segment = item.map_err(describe)?;
                        self.read_one_data(&segment);
                    }
                }
                Payload::CustomSection(section) => {
                    self.read_one_custom(&section);
                }
                // A section id this parser does not know: already listed, with
                // its size, by the section table above.
                Payload::UnknownSection { .. }
                | Payload::CodeSectionStart { .. }
                | Payload::End(_) => {}
                _ => {
                    self.component_payloads += 1;
                }
            }
        }
        Ok(())
    }

    /// The type index space this report uses: one entry per subtype, in order.
    fn read_one_type(&mut self, offset: usize, group: &wasmparser::RecGroup) {
        let many = group.types().len() > 1;
        for subtype in group.types() {
            let gc = many
                || subtype.supertype_idx.is_some()
                || !subtype.is_final
                || !matches!(
                    subtype.composite_type.inner,
                    wasmparser::CompositeInnerType::Func(_)
                );
            let (kind, signature, params, results) = match &subtype.composite_type.inner {
                wasmparser::CompositeInnerType::Func(func) => {
                    let params = func.params();
                    let results = func.results();
                    let mut signature = String::from("(");
                    for (i, p) in params.iter().enumerate() {
                        if i > 0 {
                            signature.push_str(", ");
                        }
                        signature.push_str(&val_type(p));
                    }
                    let _ = write!(signature, ") -> {}", result_list(results));
                    (
                        "func".to_string(),
                        signature,
                        params.len() as u64,
                        results.len() as u64,
                    )
                }
                wasmparser::CompositeInnerType::Struct(_) => {
                    ("struct".to_string(), String::new(), 0, 0)
                }
                wasmparser::CompositeInnerType::Array(_) => {
                    ("array".to_string(), String::new(), 0, 0)
                }
                wasmparser::CompositeInnerType::Cont(_) => {
                    ("cont".to_string(), String::new(), 0, 0)
                }
            };
            if results > 1 {
                self.scan.multi_value_types += 1;
                if self.scan.first_multi_value_type.is_none() {
                    self.scan.first_multi_value_type = Some((
                        offset as u64,
                        format!("type {} -> {results} results", self.types.len()),
                    ));
                }
            }
            self.types.push(TypeRow {
                index: self.types.len(),
                kind,
                signature,
                params: params as usize,
                results: results as usize,
                gc,
            });
        }
    }

    fn read_one_import(&mut self, import: wasmparser::Import<'_>) {
        let detail;
        let type_index = match &import.ty {
            TypeRef::Func(index) => {
                self.funcs_imported += 1;
                detail = self.signature(Some(*index as usize));
                Some(*index as usize)
            }
            TypeRef::Table(ty) => {
                self.push_table(ty, true);
                detail = format!(
                    "{} table #{}",
                    limits(ty.initial, ty.maximum, ty.table64, ty.shared),
                    self.tables.len() - 1
                );
                None
            }
            TypeRef::Memory(ty) => {
                self.push_memory(ty, true);
                detail = format!(
                    "memory #{} {}",
                    self.memories.len() - 1,
                    limits(ty.initial, ty.maximum, ty.memory64, ty.shared)
                );
                None
            }
            TypeRef::Global(ty) => {
                detail = format!("global #{} {}", self.globals.len(), global_text(ty));
                self.globals.push(GlobalRow {
                    index: self.globals.len(),
                    ty: val_type(&ty.content_type),
                    mutable: ty.mutable,
                    shared: ty.shared,
                    init: "imported".to_string(),
                });
                None
            }
            TypeRef::Tag(ty) => {
                self.scan.tags += 1;
                detail = format!(
                    "tag ({}), exception type {:?}",
                    self.signature(Some(ty.func_type_idx as usize)),
                    ty.kind
                );
                None
            }
        };
        self.imports.push(ImportRow {
            module: import.module.to_string(),
            name: import.name.to_string(),
            kind: match &import.ty {
                TypeRef::Func(_) => "func",
                TypeRef::Table(_) => "table",
                TypeRef::Memory(_) => "memory",
                TypeRef::Global(_) => "global",
                TypeRef::Tag(_) => "tag",
            }
            .to_string(),
            detail,
            type_index,
        });
    }

    fn push_table(&mut self, ty: &wasmparser::TableType, imported: bool) {
        self.tables.push(TableRow {
            index: self.tables.len(),
            imported,
            elem_type: ty.element_type.to_string(),
            min: ty.initial,
            max: ty.maximum,
            shared: ty.shared,
            table64: ty.table64,
        });
    }

    fn push_memory(&mut self, ty: &wasmparser::MemoryType, imported: bool) {
        self.memories.push(MemoryRow {
            index: self.memories.len(),
            imported,
            min: ty.initial,
            max: ty.maximum,
            shared: ty.shared,
            memory64: ty.memory64,
            page_size_log2: ty.page_size_log2,
        });
    }

    fn read_one_element(&mut self, element: &wasmparser::Element<'_>) {
        let kind = match &element.kind {
            wasmparser::ElementKind::Passive => {
                self.scan.passive_elements += 1;
                "passive".to_string()
            }
            wasmparser::ElementKind::Active {
                table_index,
                offset_expr,
            } => {
                // An absent table index is the MVP encoding of table 0.
                let table = table_index.unwrap_or(0);
                let offset = self.const_expr_text(offset_expr);
                format!("active table #{table} at {offset}")
            }
            wasmparser::ElementKind::Declared => "declared".to_string(),
        };
        let (elem_type, count) = match &element.items {
            wasmparser::ElementItems::Functions(functions) => {
                ("funcref".to_string(), u64::from(functions.count()))
            }
            wasmparser::ElementItems::Expressions(ty, expressions) => {
                (ty.to_string(), u64::from(expressions.count()))
            }
        };
        self.elements.push(ElementRow {
            index: self.elements.len(),
            kind,
            elem_type,
            count,
        });
    }

    fn read_one_data(&mut self, segment: &wasmparser::Data<'_>) {
        let kind = match &segment.kind {
            wasmparser::DataKind::Passive => {
                self.scan.passive_data += 1;
                "passive".to_string()
            }
            wasmparser::DataKind::Active {
                memory_index,
                offset_expr,
            } => {
                let offset = self.const_expr_text(offset_expr);
                format!("active memory #{memory_index} at {offset}")
            }
        };
        self.data.push(DataRow {
            index: self.data.len(),
            kind,
            bytes: segment.data.len() as u64,
        });
    }

    fn read_one_custom(&mut self, section: &wasmparser::CustomSectionReader<'_>) {
        let range = section.range();
        let mut row = CustomRow {
            name: section.name().to_string(),
            offset: range.start,
            bytes: range.end - range.start,
            note: String::new(),
        };
        match section.name() {
            "producers" => match self.read_producers(section.data(), section.data_offset()) {
                Ok(parts) => row.note = parts.join("; "),
                Err(error) => row.note = format!("producers field unreadable: {error}"),
            },
            "target_features" => {
                let (features, exact) = parse_target_features(section.data());
                if exact {
                    self.target_features = features;
                    row.note = format!(
                        "{} feature(s): {}",
                        self.target_features.len(),
                        self.target_features.join(", ")
                    );
                } else {
                    row.note = format!(
                        "{} bytes, not the LLVM list of `+feature` entries",
                        section.data().len()
                    );
                }
            }
            "name" => row.note = "function and local names".to_string(),
            "linking" => row.note = "linking metadata (side module / emscripten)".to_string(),
            "dylink.0" => row.note = "dynamic linking metadata".to_string(),
            "metadata.code.branch_hint" => row.note = "branch hints".to_string(),
            other => {
                if other.starts_with("reloc.") {
                    row.note = "relocations".to_string();
                }
            }
        }
        self.custom.push(row);
    }

    /// The `producers` custom section: which toolchain built this module.
    fn read_producers(&mut self, data: &[u8], offset: usize) -> Result<Vec<String>, String> {
        let reader = wasmparser::BinaryReader::new(data, offset);
        let producers = wasmparser::ProducersSectionReader::new(reader).map_err(describe)?;
        let mut parts = Vec::new();
        for field in producers {
            let field = field.map_err(describe)?;
            let name = field.name.to_string();
            let mut values = Vec::new();
            for value in field.values {
                let value = value.map_err(describe)?;
                values.push((value.name.to_string(), value.version.to_string()));
            }
            parts.push(format!(
                "{name} = {}",
                values
                    .iter()
                    .map(|(n, v)| producer_text(n, v))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            self.producers.push((name, values));
        }
        Ok(parts)
    }

    /// The one operator scan every verdict is computed from.
    fn scan_one_operator(&mut self, op: &Operator<'_>, offset: usize) {
        self.scan.operators += 1;
        // The parser's own tagging says which proposal this operator is from.
        let proposal = verdict::proposal_of(op);
        self.scan
            .note(proposal, offset as u64, verdict::variant_of(op));
        if let Operator::Block { blockty } | Operator::Loop { blockty } | Operator::If { blockty } =
            op
            && let BlockType::FuncType(index) = blockty
        {
            let index = *index as usize;
            if self
                .types
                .get(index)
                .is_some_and(|t| t.params > 0 || t.results > 1)
            {
                self.scan.multi_value_blocks += 1;
                if self.scan.first_multi_value_block.is_none() {
                    self.scan.first_multi_value_block = Some((
                        offset as u64,
                        format!("block by type {index}: {}", self.signature(Some(index))),
                    ));
                }
            }
        }
    }

    /// Read a constant expression: its first operator for display, and whether
    /// it uses the extended-const form MVP forbids.
    fn const_expr_text(&mut self, expr: &ConstExpr<'_>) -> String {
        let mut reader = expr.get_operators_reader();
        let mut text = String::new();
        let mut count = 0u64;
        while !reader.eof() {
            let (op, offset) = match reader.read_with_offset() {
                Ok(pair) => pair,
                Err(error) => return format!("<unreadable: {}>", describe(error)),
            };
            if matches!(op, Operator::End) {
                // The terminator is not part of what the expression means.
                continue;
            }
            count += 1;
            if matches!(op, Operator::I32Add | Operator::I64Add) {
                self.scan.extended_const += 1;
                if self.scan.first_extended_const.is_none() {
                    self.scan.first_extended_const =
                        Some((offset as u64, verdict::variant_of(&op).to_string()));
                }
            }
            if count == 1 {
                text = operator_in_const(&op);
            }
        }
        if count == 0 {
            "<empty>".to_string()
        } else if count > 1 {
            format!("{text} … ({count} operators)")
        } else {
            text
        }
    }
}

fn operator_in_const(op: &Operator<'_>) -> String {
    match op {
        Operator::I32Const { value } => format!("i32.const {value}"),
        Operator::I64Const { value } => format!("i64.const {value}"),
        Operator::F32Const { value } => format!("f32.const 0x{:08x}", value.bits()),
        Operator::F64Const { value } => format!("f64.const 0x{:016x}", value.bits()),
        Operator::GlobalGet { global_index, .. } => format!("global.get {global_index}"),
        Operator::RefNull { hty, .. } => format!("ref.null {}", heap_type_text(hty)),
        Operator::RefFunc { function_index, .. } => format!("ref.func {function_index}"),
        other => verdict::variant_of(other).to_string(),
    }
}

fn val_type(ty: &ValType) -> String {
    ty.to_string()
}

/// `HeapType` has no `Display` in this parser version, and its `Debug` form is
/// the Rust variant name rather than the WAT one.
fn heap_type_text(hty: &wasmparser::HeapType) -> String {
    match hty {
        wasmparser::HeapType::Abstract { shared: false, ty } => format!("{ty:?}").to_lowercase(),
        wasmparser::HeapType::Abstract { shared: true, ty } => {
            format!("{} (shared)", format!("{ty:?}").to_lowercase())
        }
        wasmparser::HeapType::Concrete(index) => format!("type #{index}"),
    }
}

/// `i32`, `mut i32`, `shared externref` and so on.
fn global_text(ty: &wasmparser::GlobalType) -> String {
    let mut text = String::new();
    if ty.shared {
        text.push_str("shared ");
    }
    if ty.mutable {
        let _ = write!(text, "mut ");
    }
    text.push_str(&val_type(&ty.content_type));
    text
}

fn result_list(results: &[ValType]) -> String {
    match results.len() {
        0 => "()".to_string(),
        1 => results.first().map(|t| t.to_string()).unwrap_or_default(),
        _ => format!(
            "({})",
            results
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn external_kind(kind: &ExternalKind) -> &'static str {
    match kind {
        ExternalKind::Func => "func",
        ExternalKind::Table => "table",
        ExternalKind::Memory => "memory",
        ExternalKind::Global => "global",
        ExternalKind::Tag => "tag",
    }
}

/// One memory or table limit, flags first: `shared min 16 max 32`, or
/// `min 16 max none` when it says nothing extra. The width is the index type
/// both kinds have, so it is named for that and not for memory.
fn limits(min: u64, max: Option<u64>, index64: bool, shared: bool) -> String {
    let mut text = String::new();
    if index64 {
        text.push_str("64-bit ");
    }
    if shared {
        text.push_str("shared ");
    }
    let _ = write!(text, "min {min}");
    match max {
        Some(max) => {
            let _ = write!(text, " max {max}");
        }
        None => text.push_str(" max none"),
    }
    text
}

/// One producer as it reads. A section may name a tool without a version, and
/// the blank left behind there is not information.
pub fn producer_text(name: &str, version: &str) -> String {
    if version.is_empty() {
        name.to_string()
    } else {
        format!("{name} {version}")
    }
}

/// Read an unsigned LEB128 at `at`, returning value and bytes consumed.
fn leb32(bytes: &[u8], at: usize) -> Option<(usize, usize)> {
    let mut value = 0u64;
    let mut shift = 0u32;
    let mut read = 0usize;
    loop {
        let byte = *bytes.get(at + read)?;
        read += 1;
        value |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 35 {
            return None;
        }
    }
    Some((value as usize, read))
}

/// The byte offsets and sizes of every top-level section, header included.
pub fn section_table(bytes: &[u8]) -> Result<Vec<SectionRow>, String> {
    let mut rows = Vec::new();
    let mut pos = 8usize;
    while pos < bytes.len() {
        let id = bytes[pos];
        let kind = section_kind(id);
        let Some((contents, len)) = leb32(bytes, pos + 1) else {
            return Err(format!(
                "section {kind} at {pos:#x}: its length field runs off the end of the file"
            ));
        };
        let start = pos + 1 + len;
        let Some(end) = start.checked_add(contents) else {
            return Err(format!(
                "section {kind} at {pos:#x}: length {contents} overflows"
            ));
        };
        if end > bytes.len() {
            return Err(format!(
                "section {kind} at {pos:#x} claims {contents} contents bytes, only {} remain",
                bytes.len() - start
            ));
        }
        let name = if id == 0 {
            let decoded = leb32(bytes, start).and_then(|(len, head)| {
                let tail = bytes.get(start + head..)?;
                let name = tail.get(..len)?;
                std::str::from_utf8(name).ok()
            });
            Some(match decoded {
                Some(name) => name.to_string(),
                None => "<unreadable name>".to_string(),
            })
        } else {
            None
        };
        rows.push(SectionRow {
            id,
            kind,
            name,
            offset: pos,
            bytes: end - pos,
            contents,
        });
        pos = end;
    }
    Ok(rows)
}

/// The `target_features` payload LLVM writes: a count, then per feature one
/// policy byte (`'+'` used, `'-'` disallowed) and a name.
///
/// Returns the entries and whether they filled the payload exactly. Anything
/// else is not this convention, and gets reported as the bytes it is rather
/// than as a guess.
fn parse_target_features(data: &[u8]) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let Some((count, mut at)) = leb32(data, 0) else {
        return (out, false);
    };
    for _ in 0..count {
        let Some(prefix @ (b'+' | b'-')) = data.get(at).copied() else {
            return (out, false);
        };
        at += 1;
        let Some((len, head)) = leb32(data, at) else {
            return (out, false);
        };
        at += head;
        let Some(bytes) = data.get(at..at + len) else {
            return (out, false);
        };
        let Ok(name) = std::str::from_utf8(bytes) else {
            return (out, false);
        };
        out.push(format!("{}{}", char::from(prefix), name));
        at += len;
    }
    let exact = at == data.len();
    (out, exact)
}

fn describe(error: wasmparser::BinaryReaderError) -> String {
    let offset = error.offset();
    format!("{offset:#x}: {}", error.message())
}

/// One line of the verdict, from operator counts plus non-operator evidence.
#[allow(clippy::too_many_arguments)]
fn feature(
    name: &'static str,
    scan: &Scan,
    proposals: &[Proposal],
    extra_count: u64,
    extra_text: &str,
    extra_example: Option<(u64, String)>,
    setter: Option<&'static str>,
    supported: bool,
) -> Feature {
    let ops = scan.operators_of(proposals);
    let op_example = scan.first_of(proposals);
    let mut evidence = String::new();
    if ops > 0 {
        let _ = write!(evidence, "{} operators", verdict::group(ops));
        if let Some((offset, variant)) = &op_example {
            let _ = write!(evidence, ", first at {offset:#x} ({variant})");
        }
    }
    if extra_count > 0 {
        if !evidence.is_empty() {
            evidence.push_str("; ");
        }
        evidence.push_str(extra_text);
    }
    if evidence.is_empty() {
        evidence.push_str("none found");
    }
    Feature {
        name,
        used: ops + extra_count > 0,
        count: ops + extra_count,
        example: if ops > 0 { op_example } else { extra_example },
        evidence,
        setter,
        supported,
    }
}

/// The feature verdict: what the module's code actually asks of an engine.
///
/// Every line is computed from the operator scan; the three features with no
/// operators of their own (`multi_value`, `extended_const`, `memory64`) come
/// from the types and constant expressions the scan read on the way.
pub fn verdict(report: &Report) -> Vec<Feature> {
    let scan = &report.scan;
    let shared_objects = (report.memories.iter().filter(|m| m.shared).count() as u64)
        + (report.tables.iter().filter(|t| t.shared).count() as u64)
        + (report.globals.iter().filter(|g| g.shared).count() as u64);
    let externref_tables = report
        .tables
        .iter()
        .filter(|t| t.elem_type != "funcref")
        .count() as u64;
    let structural_types = report.types.iter().filter(|t| t.kind != "func").count() as u64;
    let indexed_types = report.types.iter().filter(|t| t.gc).count() as u64;
    let memory64 = (report.memories.iter().filter(|m| m.memory64).count() as u64)
        + (report.tables.iter().filter(|t| t.table64).count() as u64);
    let custom_page_sizes = report
        .memories
        .iter()
        .filter(|m| m.page_size_log2.is_some())
        .count() as u64;
    let passive_segments = scan.passive_data + scan.passive_elements;

    vec![
        feature(
            "simd",
            scan,
            &[Proposal::Simd],
            0,
            "",
            None,
            Some("wasm_simd"),
            true,
        ),
        feature(
            "threads",
            scan,
            &[Proposal::Threads, Proposal::SharedEverythingThreads],
            shared_objects,
            &format!("{shared_objects} shared memory/table/global object(s)"),
            report
                .memories
                .iter()
                .find(|m| m.shared)
                .map(|m| (m.index as u64, format!("memory #{} is shared", m.index))),
            None,
            false,
        ),
        feature(
            "bulk_memory",
            scan,
            &[Proposal::BulkMemory],
            passive_segments + scan.data_count.map(|_| 1).unwrap_or(0),
            &format!(
                "{} passive segment(s), data count section {}",
                passive_segments,
                if scan.data_count.is_some() {
                    "present"
                } else {
                    "absent"
                }
            ),
            scan.data_count
                .map(|c| (c, format!("{c} data segments declared"))),
            Some("wasm_bulk_memory"),
            true,
        ),
        feature(
            "reference_types",
            scan,
            &[Proposal::ReferenceTypes, Proposal::FunctionReferences],
            externref_tables,
            &format!("{externref_tables} table(s) not of funcref"),
            None,
            Some("wasm_reference_types"),
            true,
        ),
        feature(
            "multi_value",
            scan,
            &[],
            scan.multi_value_types + scan.multi_value_blocks,
            &format!(
                "{} function type(s) with 2+ results, {} block(s) with a multi-value type",
                verdict::group(scan.multi_value_types),
                verdict::group(scan.multi_value_blocks)
            ),
            scan.first_multi_value_type
                .clone()
                .or_else(|| scan.first_multi_value_block.clone()),
            Some("wasm_multi_value"),
            true,
        ),
        feature(
            "sign_extension",
            scan,
            &[Proposal::SignExtension],
            0,
            "",
            None,
            Some("wasm_sign_extension"),
            true,
        ),
        feature(
            "saturating_float_to_int",
            scan,
            &[Proposal::SaturatingFloatToInt],
            0,
            "",
            None,
            Some("wasm_saturating_float_to_int"),
            true,
        ),
        feature(
            "exceptions",
            scan,
            &[Proposal::Exceptions, Proposal::LegacyExceptions],
            scan.tags,
            &format!("{} tag(s)", scan.tags),
            (scan.tags > 0).then(|| (scan.tags, format!("{} tags", scan.tags))),
            None,
            false,
        ),
        feature(
            "tail_call",
            scan,
            &[Proposal::TailCall],
            0,
            "",
            None,
            Some("wasm_tail_call"),
            true,
        ),
        feature(
            "gc",
            scan,
            &[Proposal::Gc],
            structural_types + indexed_types,
            &format!(
                "{structural_types} structural type(s), {indexed_types} type(s) needing the GC index space"
            ),
            report.types.iter().find(|t| t.gc).map(|t| {
                (
                    t.index as u64,
                    format!("type {} is a {} type", t.index, t.kind),
                )
            }),
            None,
            false,
        ),
        feature(
            "extended_const",
            scan,
            &[],
            scan.extended_const,
            &format!(
                "{} add(s) inside a constant expression, which MVP forbids",
                scan.extended_const
            ),
            scan.first_extended_const.clone(),
            Some("wasm_extended_const"),
            true,
        ),
        feature(
            "memory64",
            scan,
            &[],
            memory64,
            &format!("{memory64} memory/table with a 64-bit index"),
            report
                .memories
                .iter()
                .find(|m| m.memory64)
                .map(|m| (m.index as u64, format!("memory #{} is memory64", m.index))),
            Some("wasm_memory64"),
            true,
        ),
        feature(
            "relaxed_simd",
            scan,
            &[Proposal::RelaxedSimd],
            0,
            "",
            None,
            Some("wasm_relaxed_simd"),
            true,
        ),
        // Not part of the verdict the slice asked for, but reported when found.
        feature(
            "custom_page_sizes",
            scan,
            &[],
            custom_page_sizes,
            &format!("{custom_page_sizes} memory(ies) with a non-default page size"),
            None,
            Some("wasm_custom_page_sizes"),
            true,
        ),
    ]
}

/// What a proposal was seen in: its name, how many operators of it the bodies
/// hold, and one example (byte offset and operator) when the evidence has a
/// position in the file.
pub type ProposalEvidence = (&'static str, u64, Option<(u64, String)>);

/// Proposals the scan saw that the verdict lines above do not name.
pub fn other_proposals(report: &Report) -> Vec<ProposalEvidence> {
    [
        Proposal::MemoryControl,
        Proposal::WideArithmetic,
        Proposal::StackSwitching,
        Proposal::Other,
    ]
    .into_iter()
    .filter_map(|proposal| {
        report
            .scan
            .usages
            .get(&proposal)
            .map(|usage| (proposal.name(), usage.operators, usage.first.clone()))
    })
    .collect()
}
