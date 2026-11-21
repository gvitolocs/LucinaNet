use std::{collections::HashMap, rc::Rc};

use crate::{
    common::wtf_8::{Wtf8Cow, Wtf8String},
    parser::{
        ParseContext, ParsedProgram,
        analyze::{PrivateNameUsage, analyze_for_eval},
        ast::{self},
        parse_script_for_eval,
        scope_tree::BindingKind,
        source::Source,
    },
    runtime::{
        Context, EvalResult, Handle, HeapPtr, PropertyKey, Realm, Value,
        abstract_operations::call_object,
        bytecode::{
            function::ClosureObject, generator::BytecodeProgramGenerator, instruction::EvalFlags,
        },
        embed,
        error::{syntax_error, syntax_parse_error, type_error},
        global_names::{
            can_declare_global_function, can_declare_global_var, create_global_function_binding,
            create_global_var_binding,
        },
        interned_strings::InternedStrings,
        property::Property,
        scope::{Scope, ScopeKind},
        string_value::FlatString,
    },
};

pub fn perform_eval(
    mut cx: Context,
    code: Handle<Value>,
    is_strict_caller: bool,
    direct_scope: Option<Handle<Scope>>,
    flags: EvalFlags,
) -> EvalResult<Handle<Value>> {
    let original_code = code;
    let code = if code.is_string() {
        code.as_string()
    } else if code.is_object() && embed::has_host(cx) {
        match embed::host(cx).code_for_eval(cx, code.as_object())? {
            Some(source) => source,
            None => return Ok(code),
        }
    } else {
        return Ok(code);
    };

    // Let an embedder host's Content Security Policy block the eval
    let kind = if direct_scope.is_some() {
        embed::CompileStringsKind::Eval
    } else {
        embed::CompileStringsKind::IndirectEval
    };
    embed::ensure_can_compile_strings(cx, kind, code, &[], Some(code), &[], original_code)?;

    let is_direct = direct_scope.is_some();

    let private_names = get_private_names_from_scopes(direct_scope.map(|s| *s));

    // Use the file path of the active source file
    let file_path = cx
        .vm()
        .current_source_file()
        .map_or_else(|| "<anonymous>".to_string(), |source| source.path().to_string());

    // Parse source code
    let source = match Source::new_for_eval(file_path, code.to_wtf8_string()?) {
        Ok(source) => Rc::new(source),
        Err(error) => return syntax_parse_error(cx, &error),
    };
    let pcx = ParseContext::new(source);

    let in_derived_constructor = flags.contains(EvalFlags::IN_DERIVED_CONSTRUCTOR);
    let parse_result = parse_script_for_eval(
        &pcx,
        cx.options.clone(),
        is_direct,
        is_strict_caller,
        in_derived_constructor,
    );
    let parse_result = match parse_result {
        Ok(parse_result) => parse_result,
        Err(error) => return syntax_parse_error(cx, &error),
    };

    // Analyze source code
    let analyzed_result = analyze_for_eval(
        &pcx,
        parse_result,
        private_names,
        flags.contains(EvalFlags::IN_FUNCTION),
        flags.contains(EvalFlags::IN_METHOD),
        flags.contains(EvalFlags::IN_STATIC),
        flags.contains(EvalFlags::IN_DERIVED_CONSTRUCTOR),
        flags.contains(EvalFlags::IN_STATIC_INITIALIZER),
        flags.contains(EvalFlags::IN_CLASS_FIELD_INITIALIZER),
    );

    // Return the first syntax error
    let analyzed_result = match analyzed_result {
        Ok(analyzed_result) => analyzed_result,
        Err(errors) => return syntax_parse_error(cx, &errors.errors[0]),
    };

    // Sloppy direct evals must perform EvalDeclarationInstantiation as var scoped bindings will
    // be placed in the parent var scope. Strict direct evals do not need to call EDI as var scoped
    // bindings are handled normally when setting up a call frame in the VM.
    if !analyzed_result.program.is_strict_mode {
        eval_declaration_instantiation(cx, &analyzed_result.program)?;
    }

    // Generate bytecode for the program
    let realm = cx.current_realm();
    let generate_result =
        BytecodeProgramGenerator::generate_from_eval_parse_result(cx, &analyzed_result, realm);
    let bytecode_function = match generate_result {
        Ok(func) => func,
        Err(error) => return syntax_error(cx, &error.to_string()),
    };

    // Eval function's parent scope is the global scope in an indirect eval
    let eval_scope = direct_scope.unwrap_or_else(|| cx.current_realm().default_global_scope());
    let realm = cx.current_realm();
    let closure = ClosureObject::new(cx, bytecode_function, eval_scope, realm)?;

    // Determine the receiver for the eval function call
    let receiver: Handle<Value> = if is_direct {
        if flags.contains(EvalFlags::IN_ARROW_FUNCTION) {
            // Eval is directly inside an arrow function. Arrow functions do not have the current
            // receiver register set so we must look up captured `this` from scope chain.
            Scope::lookup_this_for_eval(cx, *direct_scope.unwrap()).to_handle(cx)
        } else {
            // Direct evals inherit their receiver from the caller
            cx.vm().receiver().to_handle(cx)
        }
    } else {
        // For indirect evals receiver is the global object
        closure.global_object().into()
    };

    // Execute the eval function's bytecode in the VM
    call_object(cx, closure.into(), receiver, &[])
}

/// Gather private names from parent class scopes.
fn get_private_names_from_scopes(
    scope: Option<HeapPtr<Scope>>,
) -> Option<HashMap<Wtf8Cow<'static>, PrivateNameUsage>> {
    let mut private_names = None;
    let mut scope_opt = scope;

    while let Some(scope) = scope_opt {
        let scope_names = scope.scope_names_ptr();
        if scope_names.is_class_scope() {
            if private_names.is_none() {
                private_names = Some(HashMap::new());
            }

            for (i, name) in scope_names.name_ptrs().iter().enumerate() {
                if scope_names.is_private_name(i) {
                    let private_name = Wtf8String::from_string(name.to_string());
                    private_names
                        .as_mut()
                        .unwrap()
                        .insert(Wtf8Cow::Owned(private_name), PrivateNameUsage::used());
                }
            }
        }

        scope_opt = scope.parent();
    }

    private_names
}

// Check if any names conflict with lexical bindings in parent scopes.
fn check_eval_var_name_conflicts(
    mut cx: Context,
    eval_var_names: &[Handle<FlatString>],
    eval_func_names: &[Handle<FlatString>],
) -> EvalResult<()> {
    let mut scope = cx.vm().scope().to_handle();

    // Special case for an eval in the function params scope
    if scope.scope_names_ptr().is_function_parameters_scope() {
        for name in eval_var_names.iter().chain(eval_func_names.iter()) {
            if let Some(index) = scope.scope_names_ptr().lookup_name(**name) {
                if scope.scope_names_ptr().is_function_parameter(index) {
                    return error_name_already_declared(cx, *name);
                }
            }
        }

        return Ok(());
    }

    // Otherwise walk scopes up to the parent var scope
    loop {
        // Name will conflict with a lexical binding in any parent scope
        for name in eval_var_names.iter().chain(eval_func_names.iter()) {
            if let Some(index) = scope.scope_names_ptr().lookup_name(**name) {
                if scope.scope_names_ptr().is_lexical(index) {
                    return error_name_already_declared(cx, *name);
                }
            }
        }

        // If in a global scope, check for lexical bindings in other global scopes
        if scope.kind() == ScopeKind::Global {
            let realm = scope.global_scope_realm();
            for name in eval_var_names.iter().chain(eval_func_names.iter()) {
                if realm.has_lexical_name(**name) {
                    return error_name_already_declared(cx, *name);
                }
            }
        }

        // Do not ascend past enclosing var scope
        if scope.scope_names_ptr().is_var_scope() {
            break;
        }

        // Move to parent scope
        if let Some(parent) = scope.parent().as_ref() {
            scope.replace(*parent);
        } else {
            break;
        }
    }

    Ok(())
}

/// EvalDeclarationInstantiation (https://tc39.es/ecma262/#sec-evaldeclarationinstantiation)
fn eval_declaration_instantiation(mut cx: Context, program: &ast::Program) -> EvalResult<()> {
    // Find the function and var names in the eval scope
    let mut eval_var_names = vec![];
    let mut eval_func_names = vec![];
    for (name, binding) in program.scope.as_ref().iter_var_decls() {
        let name = InternedStrings::alloc_wtf8_str(cx, name)?;
        match binding.kind() {
            BindingKind::Var => eval_var_names.push(name),
            BindingKind::Function { .. } => eval_func_names.push(name),
            _ => unreachable!(),
        }
    }

    // Check if any var scoped names in eval body conflict with lexical bindings in parent scopes.
    let mut eval_var_names = vec![];
    let eval_scope = program.scope.as_ref();
    for (name, _) in eval_scope.iter_var_decls() {
        let name = InternedStrings::alloc_wtf8_str(cx, name)?;
        eval_var_names.push(name);
    }

    check_eval_var_name_conflicts(cx, &eval_var_names, &eval_func_names)?;

    // Find the enclosing var scope
    let mut var_scope = cx.vm().scope().to_handle();
    while !var_scope.scope_names_ptr().is_var_scope() {
        var_scope.replace(var_scope.parent().unwrap());
    }

    let is_global_scope = var_scope.kind() == ScopeKind::Global;
    let mut scope_object = var_scope.ensure_scope_object(cx)?;

    // If in global scope, check if we can declare the var or function binding
    if is_global_scope {
        for func_name in &eval_func_names {
            if !can_declare_global_function(cx, scope_object, func_name.cast())? {
                return type_error(cx, &format!("cannot declare global function `{}`", *func_name));
            }
        }

        for var_name in &eval_var_names {
            if !can_declare_global_var(cx, scope_object, var_name.cast())? {
                return type_error(cx, &format!("cannot declare global variable `{}`", *var_name));
            }
        }
    }

    // Step 13 (B.3.2): block-level FunctionDeclarations of the eval code whose replacement by a
    // var is not an early error (decided in the scope tree, which also knows whether the name is
    // in declaredFuncOrVariableNames)
    for func in eval_scope.annex_b_functions() {
        let name = InternedStrings::alloc_wtf8_str(cx, func.name())?;
        let name_key = name.cast::<PropertyKey>();

        // 13.b.ii.1-4: bindingExists
        let binding_exists = annex_b_binding_exists_below_var_scope(cx, name);

        // 13.b.ii.5: If bindingExists is false and variableEnv is a Global Environment Record
        let func_definable = if !binding_exists && is_global_scope {
            // 13.b.ii.5.a: If HasLexicalDeclaration(variableEnv, funcName) is false, let
            // funcDefinable be ? CanDeclareGlobalVar(variableEnv, funcName), else false
            let realm = var_scope.global_scope_realm();
            !realm.has_lexical_name(*name) && can_declare_global_var(cx, scope_object, name_key)?
        } else {
            // 13.b.ii.6: Else, let funcDefinable be true.
            true
        };

        // 13.b.ii.7: If bindingExists is false and funcDefinable is true, then
        if binding_exists || !func_definable {
            // Step 13.b.ii.7.b does not apply, so evaluating the declaration does not set the var
            func.disable();
            continue;
        }

        // 13.b.ii.7.a: If declaredFuncOrVariableNames does not contain funcName, then
        if func.creates_var() {
            if is_global_scope {
                // 13.b.ii.7.a.i.1: Perform ? CreateGlobalVarBinding(variableEnv, funcName, true).
                create_global_var_binding(cx, scope_object, name_key, /* can_delete */ true)?;
            } else {
                // 13.b.ii.7.a.ii.1: Set bindingExists to ! variableEnv.HasBinding(funcName).
                let binding_exists = var_scope.scope_names_ptr().lookup_name(*name).is_some()
                    || scope_object.has_property(cx, name_key)?;

                // 13.b.ii.7.a.ii.2: If bindingExists is false, create the binding and initialize
                // it to undefined.
                if !binding_exists {
                    let desc = Property::default_data(cx.undefined());
                    scope_object.set_property(cx, name_key, desc)?;
                }
            }
        }
    }

    // Create and initialize bindings for all functions
    for name in &eval_func_names {
        if is_global_scope {
            create_global_function_binding(
                cx,
                scope_object,
                name.cast(),
                /* can_delete */ true,
            )?;
        } else {
            // All functions are initialized to undefined, overwriting existing declarations
            let desc = Property::default_data(cx.undefined());
            scope_object.set_property(cx, name.cast(), desc)?;
        }
    }

    // Create and initialize bindings for all vars
    for name in &eval_var_names {
        if is_global_scope {
            create_global_var_binding(cx, scope_object, name.cast(), /* can_delete */ true)?;
        } else {
            // Vars only initialized to undefined if the do not have been declared yet
            if !scope_object.has_property(cx, name.cast())? {
                let desc = Property::default_data(cx.undefined());
                scope_object.set_property(cx, name.cast(), desc)?;
            }
        }
    }

    Ok(())
}

/// EvalDeclarationInstantiation steps 13.b.ii.1-4 (B.3.2): whether an environment between the
/// eval's lexical environment and its variable environment binds the name, not counting object
/// environments (`with`) and catch parameters. Catch parameters are not lexical names of their
/// scope in Annex B mode (see `gen_scope_name_flags`). The top level lexical declarations of a
/// sloppy function are in its var scope, so its lexical names are checked as well.
fn annex_b_binding_exists_below_var_scope(mut cx: Context, name: Handle<FlatString>) -> bool {
    let mut scope = cx.vm().scope();

    loop {
        // 13.b.ii.4.a: If thisEnv is not an Object Environment Record, then
        if scope.kind() != ScopeKind::With {
            let scope_names = scope.scope_names_ptr();

            // 13.b.ii.4.a.i: If ! thisEnv.HasBinding(funcName) is true, set bindingExists to true
            // (13.b.ii.4.a.i.1.a: unless thisEnv is the Environment Record for a Catch clause)
            if let Some(index) = scope_names.lookup_name(*name) {
                if scope_names.is_lexical(index) {
                    return true;
                }
            }
        }

        // 13.b.ii.4: Repeat, while thisEnv is not variableEnv
        if scope.scope_names_ptr().is_var_scope() {
            return false;
        }

        // 13.b.ii.4.b: Set thisEnv to thisEnv.[[OuterEnv]].
        match scope.parent() {
            Some(parent) => scope = parent,
            None => return false,
        }
    }
}

fn error_name_already_declared(cx: Context, name: Handle<FlatString>) -> EvalResult<()> {
    syntax_error(cx, &format!("identifier `{name}` has already been declared"))
}

/// Evaluate a script in the given realm.
pub fn evaluate_script(
    mut cx: Context,
    realm: Handle<Realm>,
    source: Rc<Source>,
) -> EvalResult<Handle<Value>> {
    let program = match ParsedProgram::parse(source, cx.options.clone(), /* is_module */ false) {
        Ok(program) => Rc::new(program),
        Err(error) => return syntax_parse_error(cx, &error),
    };

    let bytecode_script =
        match BytecodeProgramGenerator::generate_from_parsed_script(cx, &program, realm) {
            Ok(bytecode_script) => bytecode_script,
            Err(error) => return syntax_error(cx, &error.to_string()),
        };

    cx.vm().execute_script(bytecode_script)
}
