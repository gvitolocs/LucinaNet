use std::{collections::HashSet, path::Path, rc::Rc};

use crate::{
    completion_value, must_a,
    parser::{
        ParseContext, ParsedProgram, analyze::analyze, parse_module, print_program, source::Source,
    },
    runtime::{
        Context, Handle, Realm, Value,
        abstract_operations::call_object,
        alloc_error::AllocResult,
        bytecode::generator::BytecodeProgramGenerator,
        context::ModuleCacheKey,
        embed,
        error::{syntax_error, syntax_parse_error},
        eval_result::EvalResult,
        intrinsics::{intrinsics::Intrinsic, json_object::json_parse},
        module::{
            module::{DynModule, ModuleId},
            source_text_module::{ModuleRequest, ModuleState, SourceTextModule},
            synthetic_module::SyntheticModule,
        },
        promise_object::{PromiseCapability, PromiseObject},
        string_value::FlatString,
    },
};

/// GraphLoadingStateRecord (https://tc39.es/ecma262/#graphloadingstate-record)
struct GraphLoader {
    is_loading: bool,
    pending_modules_count: usize,
    visited: HashSet<ModuleId>,
    promise_capability: Handle<PromiseCapability>,
    realm: Handle<Realm>,
}

impl GraphLoader {
    /// InnerModuleLoading (https://tc39.es/ecma262/#sec-InnerModuleLoading)
    fn inner_module_loading(&mut self, cx: Context, module: DynModule) -> AllocResult<()> {
        if let Some(mut module) = module.as_source_text_module() {
            // A module whose graph failed to load earlier stays Unlinked with some of its
            // [[LoadedModules]] entries still empty, so a later load must walk it again and
            // retry exactly the requests that are missing. Modules past Unlinked (Linking,
            // Linked, evaluated) have all requests loaded and are skipped.
            if matches!(module.state(), ModuleState::New | ModuleState::Unlinked)
                && self.visited.insert(module.id())
            {
                if module.state() == ModuleState::New {
                    module.set_state(ModuleState::Unlinked);
                }

                let module_requests = module.requested_modules();
                let loaded_modules = module.loaded_modules();

                self.pending_modules_count += module_requests.len();

                for i in 0..module_requests.len() {
                    match loaded_modules.as_slice()[i] {
                        Some(loaded_module) => {
                            self.inner_module_loading(cx, DynModule::from_heap(&loaded_module))?
                        }
                        None => {
                            let module_request =
                                ModuleRequest::from_heap(&module_requests.as_slice()[i]);

                            // Create the SourceTextModule for the module with the given specifier,
                            // or evaluate to an error.
                            let load_result = host_load_imported_module(
                                cx,
                                Some(module),
                                module.source_file_path(),
                                module_request,
                                self.realm,
                            );

                            // Continue module loading with the SourceTextModule or error result
                            self.finish_loading_imported_module(
                                cx,
                                module,
                                module_request,
                                load_result,
                            )?;
                        }
                    }

                    if !self.is_loading {
                        return Ok(());
                    }
                }
            }
        }

        self.pending_modules_count -= 1;

        if self.pending_modules_count == 0 {
            self.is_loading = false;

            must_a!(call_object(
                cx,
                self.promise_capability.resolve(),
                cx.undefined(),
                &[cx.undefined()]
            ));
        }

        Ok(())
    }

    /// FinishLoadingImportedModule (https://tc39.es/ecma262/#sec-FinishLoadingImportedModule)
    fn finish_loading_imported_module(
        &mut self,
        cx: Context,
        mut referrer: Handle<SourceTextModule>,
        module_request: ModuleRequest,
        module_result: EvalResult<DynModule>,
    ) -> AllocResult<()> {
        if let Ok(module) = module_result {
            let module_index = referrer
                .lookup_module_request_index(&module_request.to_heap())
                .unwrap();
            if !referrer.has_loaded_module_at(module_index) {
                referrer.set_loaded_module_at(module_index, module);
            }
        }

        self.continue_module_loading(cx, module_result)
    }

    /// ContinueModuleLoading (https://tc39.es/ecma262/#sec-ContinueModuleLoading)
    fn continue_module_loading(
        &mut self,
        cx: Context,
        module_result: EvalResult<DynModule>,
    ) -> AllocResult<()> {
        if !self.is_loading {
            return Ok(());
        }

        match completion_value!(module_result, cx) {
            Ok(module) => {
                self.inner_module_loading(cx, module)?;
            }
            Err(error) => {
                self.is_loading = false;
                must_a!(call_object(
                    cx,
                    self.promise_capability.reject(),
                    cx.undefined(),
                    &[error]
                ));
            }
        }

        Ok(())
    }
}

/// LoadRequestedModules (https://tc39.es/ecma262/#sec-LoadRequestedModules)
pub fn load_requested_modules(
    cx: Context,
    module: Handle<SourceTextModule>,
) -> AllocResult<Handle<PromiseObject>> {
    let promise_constructor = cx.get_intrinsic(Intrinsic::PromiseConstructor);
    let capability = must_a!(PromiseCapability::new(cx, promise_constructor.into()));
    let realm = module.program_function_ptr().realm();

    let mut graph_loader = GraphLoader {
        is_loading: true,
        pending_modules_count: 1,
        visited: HashSet::new(),
        promise_capability: capability,
        realm,
    };

    graph_loader.inner_module_loading(cx, module.as_dyn_module())?;

    // Known to be a PromiseObject since it was created by the intrinsic Promise constructor
    Ok(capability.promise().cast::<PromiseObject>())
}

/// HostLoadImportedModule (https://tc39.es/ecma262/#sec-HostLoadImportedModule)
pub fn host_load_imported_module(
    mut cx: Context,
    referrer: Option<Handle<SourceTextModule>>,
    source_file_path: Handle<FlatString>,
    module_request: ModuleRequest,
    realm: Handle<Realm>,
) -> EvalResult<DynModule> {
    // An embedder host supplies module records itself
    if embed::has_host(cx) {
        let resolved = embed::host(cx).resolve_module(
            cx,
            referrer,
            source_file_path,
            module_request.specifier,
            module_request.attributes,
        )?;

        if let Some(module) = resolved {
            return Ok(module.as_dyn_module());
        }
    }

    // The referrer's path comes from the embedder and may not be a file system path at all (e.g. a
    // URL), so fail the load with an error instead of aborting.
    let source_file_path = match Path::new(&source_file_path.to_string()).canonicalize() {
        Ok(path) => path,
        Err(error) => return syntax_error(cx, &error.to_string()),
    };
    let Some(source_file_dir) = source_file_path.parent() else {
        return syntax_error(cx, "module source path has no parent directory");
    };

    // Join source file path with specifier path so that relative specifiers will be applied to
    // source path and absolute specifiers will overwrite source path.
    let new_module_path = source_file_dir
        .join(Path::new(&module_request.specifier.to_string()))
        .canonicalize();

    let new_module_path = match new_module_path {
        Ok(path) => path,
        Err(error) => return syntax_error(cx, &error.to_string()),
    };

    let Some(new_module_path_str) = new_module_path.to_str() else {
        return syntax_error(cx, "module path is not valid UTF-8");
    };
    let new_module_path_string = cx.alloc_string_ptr(new_module_path_str)?.to_handle();
    let module_cache_key = ModuleCacheKey::new(new_module_path_string, module_request.attributes);

    // Use the cached module if it has already been loaded
    {
        if let Some(module) = cx.modules.get(&module_cache_key.into_heap()) {
            return Ok(DynModule::from_heap(module));
        }
    }

    // Check if the module is a JSON module
    if let Some(attributes) = &module_request.attributes {
        if attributes.has_attribute_with_value("type", "json") {
            let json_value = parse_json_file_at_path(cx, new_module_path.as_path())?;
            let json_module = SyntheticModule::new_default_export(cx, realm, json_value)?;

            // Cache the JSON module
            cx.insert_module(module_cache_key, json_module.as_dyn_module())?;

            return Ok(json_module.as_dyn_module());
        }
    }

    // Find the source file at the given path
    let source = match Source::new_from_file(new_module_path_str) {
        Ok(source) => Rc::new(source),
        Err(error) => return syntax_parse_error(cx, &error),
    };

    // Parse and analyze the source
    let options = cx.options.clone();
    let program = ParsedProgram::new(source, |pcx: &ParseContext| {
        let parse_result = parse_module(pcx, options.clone())?;

        if options.print_ast {
            println!("{}", print_program(&parse_result));
        }

        analyze(parse_result).map_err(|mut errors| errors.errors.swap_remove(0))
    });
    let program = match program {
        Ok(program) => Rc::new(program),
        Err(error) => return syntax_parse_error(cx, &error),
    };

    // Finally generate the SourceTextModule for the parsed module
    let bytecode_result =
        BytecodeProgramGenerator::generate_from_parsed_module(cx, &program, realm);

    let module = match bytecode_result {
        Ok(module) => module,
        Err(error) => return syntax_error(cx, &error.to_string()),
    };

    // Cache the module
    cx.insert_module(module_cache_key, module.as_dyn_module())?;

    Ok(module.as_dyn_module())
}

fn parse_json_file_at_path(mut cx: Context, path: &Path) -> EvalResult<Handle<Value>> {
    // Read the contents of the file into the heap
    let Some(path_str) = path.to_str() else {
        return syntax_error(cx, "module path is not valid UTF-8");
    };
    let file_contents = match Source::new_from_file(path_str) {
        Ok(source) => cx.alloc_wtf8_string(&source.contents)?,
        Err(error) => return syntax_parse_error(cx, &error),
    };

    json_parse(cx, file_contents.as_value(), cx.undefined())
}
