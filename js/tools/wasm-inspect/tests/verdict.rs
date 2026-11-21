/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The verdict, checked against modules built here in memory.
//!
//! Every fixture is a real WebAssembly binary, assembled with `wasm-encoder`
//! one section at a time, so the assertions are about the bytes and not about
//! any text in this file. Nothing here is ever instantiated: the tool only
//! parses.

use wasm_encoder::{
    AbstractHeapType, BlockType, CodeSection, ConstExpr, CustomSection, DataCountSection,
    EntityType, ExportKind, ExportSection, Function, FunctionSection, GlobalSection, GlobalType,
    HeapType, ImportSection, Instruction, MemArg, MemorySection, MemoryType, Module,
    ProducersField, ProducersSection, RefType, StartSection, TableSection, TableType, TypeSection,
    ValType,
};

use wasm_inspect::inspect::{self, Report};
use wasm_inspect::render;
use wasm_inspect::verdict::{self, Feature, Proposal};

/// Build, parse and judge a module in one step.
fn judge(module: Module) -> (Vec<u8>, Report, Vec<Feature>) {
    let bytes = module.finish();
    let report = Report::new(&bytes, "<fixture>").expect("the fixture should parse");
    let features = inspect::verdict(&report);
    (bytes, report, features)
}

fn feature<'f>(features: &'f [Feature], wanted: &str) -> &'f Feature {
    features
        .iter()
        .find(|f| f.name == wanted)
        .unwrap_or_else(|| panic!("no `{wanted}` line in the verdict"))
}

fn used(features: &[Feature], wanted: &str) -> bool {
    feature(features, wanted).used
}

/// `func (params) -> (results)`, added to a type section.
fn func_type(types: &mut TypeSection, params: Vec<ValType>, results: Vec<ValType>) {
    types.ty().function(params, results);
}

fn memory(minimum: u64, maximum: Option<u64>) -> MemoryType {
    MemoryType {
        minimum,
        maximum,
        memory64: false,
        shared: false,
        page_size_log2: None,
    }
}

fn table(element_type: RefType, minimum: u64, maximum: Option<u64>) -> TableType {
    TableType {
        element_type,
        table64: false,
        minimum,
        maximum,
        shared: false,
    }
}

/// A module with one memory, one `() -> i32` function and one body of `ops`.
fn one_body_module(ops: &[Instruction]) -> Module {
    let mut module = Module::new();

    let mut types = TypeSection::new();
    func_type(&mut types, vec![], vec![ValType::I32]);
    module.section(&types);

    let mut functions = FunctionSection::new();
    functions.function(0);
    module.section(&functions);

    let mut memories = MemorySection::new();
    memories.memory(memory(1, None));
    module.section(&memories);

    let mut body = Function::new(vec![]);
    for op in ops {
        body.instruction(op);
    }
    body.instructions().end();
    let mut code = CodeSection::new();
    code.function(&body);
    module.section(&code);

    module
}

/// A module with nothing in it that MVP does not already allow, plus one of
/// every section worth naming.
fn baseline() -> Module {
    let mut module = Module::new();

    let mut types = TypeSection::new();
    func_type(&mut types, vec![ValType::I32], vec![]);
    func_type(&mut types, vec![], vec![ValType::I32]);
    module.section(&types);

    let mut imports = ImportSection::new();
    imports.import("env", "log", EntityType::Function(0));
    module.section(&imports);

    let mut functions = FunctionSection::new();
    functions.function(1);
    module.section(&functions);

    let mut tables = TableSection::new();
    tables.table(table(RefType::FUNCREF, 2, Some(4)));
    module.section(&tables);

    let mut memories = MemorySection::new();
    memories.memory(memory(1, Some(2)));
    module.section(&memories);

    let mut globals = GlobalSection::new();
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(7),
    );
    module.section(&globals);

    let mut exports = ExportSection::new();
    exports.export("run", ExportKind::Func, 1);
    exports.export("mem", ExportKind::Memory, 0);
    exports.export("counter", ExportKind::Global, 0);
    module.section(&exports);

    module.section(&StartSection { function_index: 1 });

    let mut body = Function::new(vec![]);
    body.instructions().i32_const(7).end();
    let mut code = CodeSection::new();
    code.function(&body);
    module.section(&code);

    module
}

#[test]
fn every_section_is_listed_with_its_exact_size() {
    let (bytes, report, _) = judge(baseline());
    let kinds = report
        .sections
        .iter()
        .map(|s| s.kind.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            "type", "import", "function", "table", "memory", "global", "export", "start", "code"
        ]
    );
    assert_eq!(report.sections.first().unwrap().offset, 8);
    // Every byte of the file is accounted for: the 8-byte header, plus each
    // section's id, its LEB size field and its contents. That is what makes the
    // table trustworthy without trusting the parser's own spans.
    let sum: usize = report.sections.iter().map(|s| s.bytes).sum();
    assert_eq!(sum + 8, bytes.len());
    assert_eq!(report.size, bytes.len());
    assert_eq!(report.format, "core wasm module");
    assert_eq!(report.version, 1);
}

#[test]
fn the_shape_of_a_module_is_read_back() {
    let (_, report, _) = judge(baseline());

    assert_eq!(report.imports.len(), 1);
    let log = &report.imports[0];
    assert_eq!((log.module.as_str(), log.name.as_str()), ("env", "log"));
    assert_eq!(log.kind, "func");
    assert_eq!(log.detail, "(i32) -> ()");
    assert_eq!(log.type_index, Some(0));

    let exported = report
        .exports
        .iter()
        .map(|e| (e.name.as_str(), e.kind.as_str(), e.index))
        .collect::<Vec<_>>();
    assert_eq!(
        exported,
        [
            ("run", "func", 1),
            ("mem", "memory", 0),
            ("counter", "global", 0)
        ]
    );

    let mem = &report.memories[0];
    assert_eq!(
        (mem.min, mem.max, mem.shared, mem.memory64),
        (1, Some(2), false, false)
    );
    let tab = &report.tables[0];
    assert_eq!(
        (tab.min, tab.max, tab.elem_type.as_str(), tab.shared),
        (2, Some(4), "funcref", false)
    );
    let global = &report.globals[0];
    assert_eq!(
        (
            global.ty.as_str(),
            global.mutable,
            global.shared,
            global.init.as_str()
        ),
        ("i32", true, false, "i32.const 7")
    );

    assert_eq!(report.start, Some(1));
    assert_eq!((report.funcs_imported, report.funcs_declared), (1, 1));
    assert_eq!(report.scan.bodies, 1);
    // `i32.const 7` and the `end` that closes the body.
    assert_eq!(report.scan.operators, 2);
    assert_eq!(report.component_payloads, 0);
}

#[test]
fn mvp_alone_needs_nothing_from_the_engine() {
    let (_, _, features) = judge(baseline());
    for f in &features {
        assert!(!f.used, "{} should not be needed", f.name);
    }
    assert_eq!(verdict::needs_line(&features), "needs: nothing beyond MVP");
    let config = verdict::wasmi_config_line(&features);
    assert!(config.contains("Config::new() as is"), "{config}");
}

#[test]
fn a_function_is_named_by_where_it_lives_in_the_index_space() {
    let (_, report, _) = judge(baseline());
    assert_eq!(report.describe_func(0), "#0 = import env::log (i32) -> ()");
    assert_eq!(
        report.describe_func(1),
        "#1 = defined function #0, type 1 () -> i32"
    );
    assert_eq!(report.signature(Some(99)), "?");
}

#[test]
fn producers_and_target_features_come_from_the_custom_sections() {
    let mut module = baseline();

    let mut processed = ProducersField::new();
    processed.value("wasm-encoder", "0.245.1");
    processed.value("clang", "14.0.4");
    let mut language = ProducersField::new();
    language.value("Rust", "1.90.0");
    let mut producers = ProducersSection::new();
    producers.field("processed-by", &processed);
    producers.field("language", &language);
    module.section(&producers);

    // LLVM's `target_features` payload: a count, then per feature one policy byte
    // (`+` for used, `-` for disallowed) and a name. Nothing encodes this, so the
    // bytes are written out here.
    let mut target_features: Vec<u8> = vec![2, b'+', 4];
    target_features.extend_from_slice(b"simd");
    target_features.extend_from_slice(&[b'-', 7]);
    target_features.extend_from_slice(b"threads");
    module.section(&CustomSection {
        name: "target_features".into(),
        data: target_features.into(),
    });

    let (_, report, _) = judge(module);
    assert_eq!(report.custom.len(), 2);
    assert_eq!(
        report.target_features,
        vec!["+simd".to_string(), "-threads".to_string()]
    );
    assert_eq!(
        report.custom[1].note, "2 feature(s): +simd, -threads",
        "{:?}",
        report.custom
    );
    let processed_by = report
        .producers
        .iter()
        .find(|(kind, _)| kind == "processed-by")
        .unwrap_or_else(|| panic!("{:?}", report.producers))
        .1
        .clone();
    assert!(
        processed_by.contains(&("clang".to_string(), "14.0.4".to_string())),
        "{processed_by:?}"
    );
    let language = report
        .producers
        .iter()
        .find(|(kind, _)| kind == "language")
        .unwrap_or_else(|| panic!("{:?}", report.producers))
        .1
        .clone();
    assert_eq!(language, vec![("Rust".to_string(), "1.90.0".to_string())]);

    let features = inspect::verdict(&report);
    let text = render::to_text(&report, &features);
    assert!(text.contains("PRODUCERS"), "{text}");
    assert!(text.contains("clang 14.0.4"), "{text}");
    assert!(text.contains("simd"), "{text}");
    let json = render::to_json(&report, &features);
    assert_eq!(json["targetFeatures"][0], "+simd");
}

#[test]
fn a_target_features_payload_that_is_not_the_llvm_list_is_called_bytes() {
    let mut module = baseline();
    // The form older toolchains wrote: names with no policy byte in front. Read as
    // the current one it would invent a feature named after its own length.
    let mut data = vec![1u8, 4];
    data.extend_from_slice(b"simd");
    module.section(&CustomSection {
        name: "target_features".into(),
        data: data.into(),
    });

    let (_, report, _) = judge(module);
    assert!(
        report.target_features.is_empty(),
        "{:?}",
        report.target_features
    );
    assert_eq!(
        report.custom[0].note,
        "6 bytes, not the LLVM list of `+feature` entries"
    );
}

#[test]
fn a_v128_operator_is_simd_and_nothing_else() {
    let mut module = Module::new();
    let mut types = TypeSection::new();
    func_type(&mut types, vec![], vec![ValType::V128]);
    module.section(&types);
    let mut functions = FunctionSection::new();
    functions.function(0);
    module.section(&functions);
    let mut body = Function::new(vec![]);
    body.instructions().v128_const(0).end();
    let mut code = CodeSection::new();
    code.function(&body);
    module.section(&code);

    let (_, _, features) = judge(module);
    assert!(used(&features, "simd"));
    assert_eq!(feature(&features, "simd").count, 1);
    let (offset, variant) = feature(&features, "simd").example.clone().unwrap();
    assert_eq!(variant, "V128Const");
    assert!(
        offset > 8,
        "the example needs a real file offset, got {offset}"
    );
    for name in [
        "threads",
        "bulk_memory",
        "reference_types",
        "multi_value",
        "sign_extension",
        "saturating_float_to_int",
        "tail_call",
        "relaxed_simd",
        "gc",
        "extended_const",
        "memory64",
    ] {
        assert!(!used(&features, name), "{name} should not be needed");
    }
    assert_eq!(verdict::needs_line(&features), "needs: simd");
    let config = verdict::wasmi_config_line(&features);
    assert!(config.contains("cfg.wasm_simd(true)"), "{config}");
}

#[test]
fn an_atomic_operator_is_threads_and_we_cannot_run_threads() {
    let module = one_body_module(&[
        Instruction::I32Const(0),
        Instruction::I32AtomicLoad(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }),
        Instruction::Drop,
    ]);
    let (_, report, features) = judge(module);
    assert!(used(&features, "threads"));
    assert_eq!(feature(&features, "threads").count, 1);
    assert_eq!(
        feature(&features, "threads").example.clone().unwrap().1,
        "I32AtomicLoad"
    );
    assert!(
        !feature(&features, "threads").supported,
        "no engine in this build has threads"
    );
    assert_eq!(report.scan.operators_of(&[Proposal::Threads]), 1);
    let needs = verdict::needs_line(&features);
    assert_eq!(needs, "needs: unsupported by us: threads");
}

#[test]
fn a_shared_memory_is_threads_even_without_an_atomic() {
    let mut module = Module::new();
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 1,
        maximum: None,
        memory64: false,
        shared: true,
        page_size_log2: None,
    });
    module.section(&memories);

    let (_, report, features) = judge(module);
    assert!(used(&features, "threads"));
    assert!(report.memories[0].shared);
    assert_eq!(report.scan.operators_of(&[Proposal::Threads]), 0);
    let evidence = &feature(&features, "threads").evidence;
    assert!(evidence.contains("shared memory"), "{evidence}");
}

#[test]
fn bulk_memory_is_not_reference_types() {
    let module = one_body_module(&[
        Instruction::I32Const(0),
        Instruction::I32Const(0),
        Instruction::I32Const(0),
        Instruction::MemoryFill(0),
    ]);
    let (_, _, features) = judge(module);
    assert!(used(&features, "bulk_memory"));
    assert_eq!(feature(&features, "bulk_memory").count, 1);
    assert_eq!(
        feature(&features, "bulk_memory").example.clone().unwrap().1,
        "MemoryFill"
    );
    assert!(!used(&features, "reference_types"));
    assert!(!used(&features, "simd"));
}

#[test]
fn reference_types_shows_in_a_table_and_a_null_ref() {
    let mut module = Module::new();
    let mut types = TypeSection::new();
    func_type(&mut types, vec![], vec![ValType::EXTERNREF]);
    module.section(&types);

    let mut tables = TableSection::new();
    tables.table(table(RefType::EXTERNREF, 1, None));
    module.section(&tables);

    let mut globals = GlobalSection::new();
    globals.global(
        GlobalType {
            val_type: ValType::Ref(RefType::EXTERNREF),
            mutable: false,
            shared: false,
        },
        &ConstExpr::ref_null(HeapType::Abstract {
            shared: false,
            ty: AbstractHeapType::Extern,
        }),
    );
    module.section(&globals);

    let (_, report, features) = judge(module);
    assert!(used(&features, "reference_types"));
    assert!(!used(&features, "simd"));
    assert!(!used(&features, "gc"));
    assert_eq!(report.tables[0].elem_type, "externref");
    assert_eq!(report.globals[0].init, "ref.null extern");
    let evidence = &feature(&features, "reference_types").evidence;
    assert!(evidence.contains("not of funcref"), "{evidence}");
}

#[test]
fn multi_value_shows_in_a_type_and_in_a_block() {
    let mut module = Module::new();
    let mut types = TypeSection::new();
    // type 0 has two results, which no MVP type may have.
    func_type(&mut types, vec![], vec![ValType::I32, ValType::I32]);
    func_type(&mut types, vec![], vec![]);
    module.section(&types);

    let mut functions = FunctionSection::new();
    functions.function(0);
    functions.function(1);
    module.section(&functions);

    let mut body_a = Function::new(vec![]);
    body_a.instructions().i32_const(1).i32_const(2).end();
    let mut body_b = Function::new(vec![]);
    body_b
        .instructions()
        .block(BlockType::FunctionType(0))
        .i32_const(3)
        .i32_const(4)
        .end()
        .drop()
        .drop()
        .end();
    let mut code = CodeSection::new();
    code.function(&body_a);
    code.function(&body_b);
    module.section(&code);

    let (_, report, features) = judge(module);
    assert!(used(&features, "multi_value"));
    assert_eq!(report.types[0].results, 2);
    assert_eq!(report.types[0].signature, "() -> (i32, i32)");
    assert_eq!(report.scan.multi_value_types, 1);
    assert_eq!(report.scan.multi_value_blocks, 1);
    assert_eq!(report.funcs_declared, 2);
    assert_eq!(verdict::needs_line(&features), "needs: multi_value");
}

#[test]
fn sign_extension_and_saturating_truncation_are_two_separate_flags() {
    let (bytes, report, features) = judge(one_body_module(&[
        Instruction::I32Extend8S,
        Instruction::I32TruncSatF32S,
    ]));
    assert!(used(&features, "sign_extension"));
    assert!(used(&features, "saturating_float_to_int"));
    assert!(!used(&features, "simd"));
    assert_eq!(feature(&features, "sign_extension").count, 1);
    assert_eq!(feature(&features, "saturating_float_to_int").count, 1);
    // two named operators plus the body's `end`.
    assert_eq!(report.scan.operators, 3);
    assert!(bytes.len() < 200, "the fixture should stay tiny");
    assert_eq!(
        verdict::needs_line(&features),
        "needs: sign_extension, saturating_float_to_int"
    );
}

#[test]
fn a_tail_call_is_a_tail_call() {
    let module = one_body_module(&[Instruction::I32Const(0), Instruction::ReturnCall(0)]);
    let (_, _, features) = judge(module);
    assert!(used(&features, "tail_call"));
    assert_eq!(
        feature(&features, "tail_call").example.clone().unwrap().1,
        "ReturnCall"
    );
    let config = verdict::wasmi_config_line(&features);
    assert!(config.contains("cfg.wasm_tail_call(true)"), "{config}");
}

#[test]
fn a_relaxed_simd_operator_is_also_still_simd() {
    let mut module = Module::new();
    let mut types = TypeSection::new();
    func_type(&mut types, vec![], vec![ValType::V128]);
    module.section(&types);
    let mut functions = FunctionSection::new();
    functions.function(0);
    module.section(&functions);

    let mut body = Function::new(vec![]);
    body.instructions()
        .v128_const(0)
        .v128_const(0)
        .i8x16_relaxed_swizzle()
        .end();
    let mut code = CodeSection::new();
    code.function(&body);
    module.section(&code);

    let (_, _, features) = judge(module);
    assert!(used(&features, "relaxed_simd"));
    assert!(used(&features, "simd"), "the v128.const is simd too");
    assert_eq!(feature(&features, "relaxed_simd").count, 1);
    assert_eq!(
        feature(&features, "relaxed_simd")
            .example
            .clone()
            .unwrap()
            .1,
        "I8x16RelaxedSwizzle"
    );
}

#[test]
fn memory64_and_a_custom_page_size_come_from_the_limits() {
    let mut module = Module::new();
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 1,
        maximum: Some(2),
        memory64: true,
        shared: false,
        page_size_log2: Some(16),
    });
    module.section(&memories);

    let (_, report, features) = judge(module);
    assert!(used(&features, "memory64"));
    assert!(used(&features, "custom_page_sizes"));
    assert!(report.memories[0].memory64);
    assert_eq!(report.memories[0].page_size_log2, Some(16));
    assert_eq!(feature(&features, "memory64").count, 1);
}

#[test]
fn an_extended_const_in_a_global_init_is_extended_const() {
    let mut module = Module::new();
    let mut globals = GlobalSection::new();
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: false,
            shared: false,
        },
        &ConstExpr::extended([
            Instruction::I32Const(2),
            Instruction::I32Const(3),
            Instruction::I32Add,
        ]),
    );
    module.section(&globals);

    let (_, report, features) = judge(module);
    assert!(used(&features, "extended_const"));
    assert_eq!(report.globals[0].init, "i32.const 2 … (3 operators)");
    assert_eq!(
        feature(&features, "extended_const")
            .example
            .clone()
            .unwrap()
            .1,
        "I32Add"
    );
    assert_eq!(verdict::needs_line(&features), "needs: extended_const");
}

#[test]
fn a_passive_data_segment_needs_bulk_memory() {
    let mut module = Module::new();
    let mut memories = MemorySection::new();
    memories.memory(memory(1, None));
    module.section(&memories);

    module.section(&DataCountSection { count: 1 });

    let mut data = wasm_encoder::DataSection::new();
    data.passive([1u8, 2, 3, 4]);
    module.section(&data);

    let (_, report, features) = judge(module);
    assert!(used(&features, "bulk_memory"));
    assert_eq!(report.data[0].kind, "passive");
    assert_eq!(report.data[0].bytes, 4);
}

#[test]
fn text_and_json_agree_on_the_verdict() {
    let (bytes, report, features) = judge(baseline());
    let text = render::to_text(&report, &features);
    let json = render::to_json(&report, &features);
    assert!(text.contains("FEATURE VERDICT"));
    assert!(text.contains("needs: nothing beyond MVP"), "{text}");
    assert_eq!(json["size"].as_u64(), Some(bytes.len() as u64));
    assert_eq!(json["file"], "<fixture>");
    assert_eq!(json["needs"], "needs: nothing beyond MVP");
    assert_eq!(json["features"].as_array().unwrap().len(), 14);
    assert_eq!(json["counts"]["functionsDefined"].as_u64(), Some(1));
    assert_eq!(json["counts"]["functionsImported"].as_u64(), Some(1));
    assert_eq!(json["imports"][0]["signature"], "(i32) -> ()");
    assert_eq!(json["imports"][0]["kind"], "func");
    assert_eq!(json["start"]["index"].as_u64(), Some(1));
    assert_eq!(
        json["start"]["description"],
        "#1 = defined function #0, type 1 () -> i32"
    );
}

#[test]
fn a_truncated_or_foreign_file_is_an_error_not_a_panic() {
    let bytes = baseline().finish();
    for cut in [0usize, 3, 5, 7] {
        let error = Report::new(&bytes[..cut], "<truncated>").unwrap_err();
        assert!(!error.is_empty(), "cut at {cut} should complain");
    }
    // A header with nothing after it is a legal empty module.
    assert!(Report::new(&bytes[..8], "<empty>").is_ok());

    let error = Report::new(b"not wasm at all!", "<junk>").unwrap_err();
    assert!(error.contains("magic"), "{error}");
}
