//! ES modules driven by the host, as SpiderMonkey's `JS::CompileModule`, `JS::ModuleLink`,
//! `JS::ModuleEvaluate` and the module load/dynamic import hooks.
//!
//! The host compiles each module from source, reads its requested specifiers, and supplies the
//! module records for them: either up front with `set_requested_module`, or on demand from
//! `Host::resolve_module` while linking. The engine never loads files while the host answers.
//! `import()` calls `Host::dynamic_import`, which may finish the import later with
//! `finish_dynamic_import`.

use std::rc::Rc;

pub use crate::runtime::module::{
    import_attributes::ImportAttributes,
    source_text_module::{ModuleState, SourceTextModule},
};
use crate::{
    common::wtf_8::Wtf8String,
    eval_err,
    parser::{ParsedProgram, source::Source},
    runtime::{
        Context, EvalResult, Handle, HeapPtr, Realm, Value,
        abstract_operations::call_object,
        alloc_error::AllocResult,
        bytecode::generator::BytecodeProgramGenerator,
        embed::{with_any_realm, with_realm},
        error::{syntax_error, syntax_parse_error},
        module::{execute::continue_dynamic_import, module::Module},
        object_value::ObjectValue,
        promise_object::{PromiseCapability, PromiseObject},
        string_value::FlatString,
    },
};

/// A requested module of a module: its specifier and import attributes, in source order.
pub struct ModuleRequestInfo {
    pub specifier: Handle<FlatString>,
    /// Import attribute (key, value) pairs, e.g. `("type", "json")`, keys sorted.
    pub attributes: Vec<(Handle<FlatString>, Handle<FlatString>)>,
}

/// Parse and compile a module for a realm. Syntax errors are thrown here. `line` is the line
/// number of the first line of `source`.
pub fn compile_module(
    cx: Context,
    realm: Handle<Realm>,
    source: &str,
    file_name: &str,
    line: u32,
) -> EvalResult<Handle<SourceTextModule>> {
    with_realm(cx, realm, |cx| {
        let mut text = "\n".repeat(line.saturating_sub(1) as usize);
        text.push_str(source);

        let source = match Source::new_for_string(file_name, Wtf8String::from_string(text)) {
            Ok(source) => Rc::new(source),
            Err(error) => return syntax_parse_error(cx, &error),
        };

        let program =
            match ParsedProgram::parse(source, cx.options.clone(), /* is_module */ true) {
                Ok(program) => Rc::new(program),
                Err(error) => return syntax_parse_error(cx, &error),
            };

        match BytecodeProgramGenerator::generate_from_parsed_module(cx, &program, realm) {
            Ok(module) => Ok(module),
            Err(error) => syntax_error(cx, &error.to_string()),
        }
    })
}

/// The modules requested by a module's imports and re-exports (`JS::GetRequestedModules`).
pub fn module_requests(module: Handle<SourceTextModule>) -> Vec<ModuleRequestInfo> {
    module
        .requested_modules()
        .as_slice()
        .iter()
        .map(|request| ModuleRequestInfo {
            specifier: request.specifier.to_handle(),
            attributes: request
                .attributes
                .map(|attributes| attributes.to_handle().pairs())
                .unwrap_or_default(),
        })
        .collect()
}

/// Supply the module record for request `index` of `module` (`JS::FinishLoadingImportedModule`
/// for static imports). Requests left empty are resolved with `Host::resolve_module` when linking.
/// The first module set for a request wins. Panics if the index is out of range.
pub fn set_requested_module(
    mut module: Handle<SourceTextModule>,
    index: usize,
    dependency: Handle<SourceTextModule>,
) {
    if !module.has_loaded_module_at(index) {
        module.set_loaded_module_at(index, dependency.as_dyn_module());
    }
}

/// The module record supplied for request `index`, if any.
pub fn requested_module(
    module: Handle<SourceTextModule>,
    index: usize,
) -> Option<Handle<SourceTextModule>> {
    if !module.has_loaded_module_at(index) {
        return None;
    }

    module.get_loaded_module_at(index).as_source_text_module()
}

/// Link a module and its dependency graph (`JS::ModuleLink`). Missing module records are
/// requested from `Host::resolve_module` first. Throws on resolution or link errors.
pub fn module_link(cx: Context, module: Handle<SourceTextModule>) -> EvalResult<()> {
    with_realm(cx, module_realm(module), |cx| {
        // Loading is synchronous since the host answers resolution synchronously
        let load_promise = module.load_requested_modules(cx)?;
        if let Some(error) = load_promise.rejected_value() {
            return eval_err!(error.to_handle(cx));
        }

        // Linking walks every loaded module and assumes all requests have been resolved, so a
        // load that did not complete synchronously must not be linked.
        if load_promise.is_pending() {
            return syntax_error(cx, "module graph could not be loaded synchronously");
        }

        module.link(cx)
    })
}

/// Evaluate a linked module (`JS::ModuleEvaluate`). Returns the evaluation promise, which settles
/// once the module and its dependencies (including top-level await) finished; run the host's jobs
/// to drive it. Evaluation errors reject the promise.
pub fn module_evaluate(
    cx: Context,
    module: Handle<SourceTextModule>,
) -> EvalResult<Handle<PromiseObject>> {
    with_realm(cx, module_realm(module), |cx| Ok(module.evaluate(cx)?))
}

/// The module namespace object (`JS::GetModuleNamespace`). The module must be linked.
pub fn module_namespace(
    cx: Context,
    module: Handle<SourceTextModule>,
) -> AllocResult<Handle<ObjectValue>> {
    let mut module = module;
    Ok(module.get_namespace_object(cx)?.as_object().to_handle())
}

/// The state of a module record.
pub fn module_state(module: Handle<SourceTextModule>) -> ModuleState {
    module.state()
}

/// The realm a module was compiled for.
pub fn module_realm(module: Handle<SourceTextModule>) -> Handle<Realm> {
    module.program_function().realm()
}

/// The file name a module was compiled with (`JS::GetModuleScript` gives SpiderMonkey's script).
pub fn module_file_name(module: Handle<SourceTextModule>) -> Handle<FlatString> {
    module.source_file_path()
}

/// The evaluation error of a module that failed to evaluate, if any.
pub fn module_evaluation_error(
    cx: Context,
    module: Handle<SourceTextModule>,
) -> Option<Handle<Value>> {
    module.evaluation_error(cx)
}

/// Attach a value to a module (`JS::SetModulePrivate`). Traced by the GC.
pub fn set_module_private(mut module: Handle<SourceTextModule>, value: Value) {
    module.set_host_defined(value);
}

/// The value attached with `set_module_private`, undefined by default (`JS::GetModulePrivate`).
pub fn module_private(module: Handle<SourceTextModule>) -> Value {
    module.host_defined()
}

/// A pending dynamic `import()` handed to `Host::dynamic_import`. Holds the import's promise
/// capability, a GC heap item: the host must report `cell_ptr` from `Host::trace_roots` (it may
/// move) until it calls `finish_dynamic_import`.
#[derive(Clone, Copy)]
pub struct DynamicImport {
    capability: HeapPtr<PromiseCapability>,
}

impl DynamicImport {
    pub(crate) fn new(capability: HeapPtr<PromiseCapability>) -> Self {
        DynamicImport { capability }
    }

    /// The pointer to report from `Host::trace_roots` with `RootVisitor::cell`.
    pub fn cell_ptr(&self) -> *mut std::ffi::c_void {
        self.capability.as_ptr().cast()
    }

    /// Rebuild from a (possibly updated) pointer previously returned by `cell_ptr`.
    pub fn from_cell_ptr(ptr: *mut std::ffi::c_void) -> Self {
        DynamicImport { capability: HeapPtr::from_ptr(ptr.cast()) }
    }

    /// The promise returned to the `import()` call.
    pub fn promise(&self) -> Handle<ObjectValue> {
        self.capability.to_handle().promise()
    }
}

/// Finish a dynamic import the host took over. With a module record the engine loads its missing
/// dependencies (through `Host::resolve_module`), links and evaluates it, and resolves the import's
/// promise with its namespace through the job queue. With an error value the promise is rejected.
pub fn finish_dynamic_import(
    cx: Context,
    import: DynamicImport,
    result: Result<Handle<SourceTextModule>, Handle<Value>>,
) -> EvalResult<()> {
    let capability = import.capability.to_handle();

    with_any_realm(cx, |cx| match result {
        Ok(module) => Ok(continue_dynamic_import(cx, capability, Ok(module.as_dyn_module()))?),
        Err(error) => {
            call_object(cx, capability.reject(), cx.undefined(), &[error])?;
            Ok(())
        }
    })
}
