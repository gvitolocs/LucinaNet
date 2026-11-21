use std::rc::Rc;

use crate::{
    handle_scope, must_a,
    parser::source::Source,
    runtime::{
        Context, EvalResult, Handle, PropertyKey, Realm,
        abstract_operations::set,
        alloc_error::AllocResult,
        builtin_function::BuiltinFunction,
        error::{syntax_parse_error, type_error},
        eval::eval::evaluate_script,
        get,
        intrinsic_builder::IntrinsicBuilder,
        intrinsics::{
            array_buffer_object::{ArrayBufferObject, DetachKey},
            intrinsics::Intrinsic,
            rust_runtime::RuntimeFunction,
        },
        object_value::ObjectValue,
        string_value::StringValue,
    },
    runtime_fn,
};

/// Utility functions used in test262 tests. Must be included in the main library for now so that
/// builtin functions can be registered.
pub struct Test262Object;

impl Test262Object {
    fn new(mut cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        let create_realm_string = cx.alloc_static_string("createRealm")?;
        let create_realm_key = PropertyKey::string_handle(cx, create_realm_string)?;
        builder.method(create_realm_key, RuntimeFunction::Test262Object_create_realm, 0)?;

        let eval_script_string = cx.alloc_static_string("evalScript")?;
        let eval_script_key = PropertyKey::string_handle(cx, eval_script_string)?;
        builder.method(eval_script_key, RuntimeFunction::Test262Object_eval_script, 1)?;

        let global_string = cx.alloc_static_string("global")?;
        let global_key = PropertyKey::string_handle(cx, global_string)?;
        builder.data(global_key, realm.global_object().into())?;

        let detach_array_buffer_string = cx.alloc_static_string("detachArrayBuffer")?;
        let detach_array_buffer_key = PropertyKey::string_handle(cx, detach_array_buffer_string)?;
        builder.method(
            detach_array_buffer_key,
            RuntimeFunction::Test262Object_detach_array_buffer,
            1,
        )?;

        builder.method(cx.names.gc(), RuntimeFunction::GcObject_run, 0)?;

        // The `IsHTMLDDA` hook (test262 INTERPRETING.md): an object with the [[IsHTMLDDA]] internal
        // slot, which must also be callable and return null when called with no argument or with
        // the empty string. Tests that use it are tagged `features: [IsHTMLDDA]`.
        let htmldda_string = cx.alloc_static_string("IsHTMLDDA")?;
        let htmldda_key = PropertyKey::string_handle(cx, htmldda_string)?;
        let mut htmldda = BuiltinFunction::create(
            cx,
            RuntimeFunction::Test262Object_is_htmldda,
            0,
            htmldda_key,
            realm,
            None,
        )?;
        htmldda.init_htmldda(cx)?;
        builder.data(htmldda_key, htmldda.into())?;

        builder.build()
    }

    pub fn install(mut cx: Context, realm: Handle<Realm>) -> AllocResult<()> {
        handle_scope!(cx, {
            // Create the test262 object
            let test_262_object = Test262Object::new(cx, realm)?;

            let mut builder = IntrinsicBuilder::global(cx, realm);

            // Install the "$262" property on the global object
            builder.data(test_262_key(cx)?, test_262_object.into())?;

            // Also install a global print function needed in tests
            let print_string = cx.alloc_static_string("print")?;
            let print_key = PropertyKey::string_handle(cx, print_string)?;
            builder.method(print_key, RuntimeFunction::Test262Object_print, 1)?;

            builder.build()?;

            // Install the global print log property
            let global_object = realm.global_object().as_object();
            must_a!(Self::set_print_log(cx, global_object, cx.names.empty_string().as_string()));

            Ok(())
        })
    }

    fn print_log_key(mut cx: Context) -> AllocResult<Handle<PropertyKey>> {
        let print_log_string = cx.alloc_static_string("$$printLog")?;
        PropertyKey::string_handle(cx, print_log_string)
    }

    pub fn get_print_log(
        cx: Context,
        global_object: Handle<ObjectValue>,
    ) -> EvalResult<Handle<StringValue>> {
        let print_log = get(cx, global_object, Self::print_log_key(cx)?)?;
        if !print_log.is_string() {
            return type_error(cx, "printLog must be a string");
        }

        Ok(print_log.as_string())
    }

    fn set_print_log(
        cx: Context,
        global_object: Handle<ObjectValue>,
        print_log: Handle<StringValue>,
    ) -> EvalResult<()> {
        set(cx, global_object, Self::print_log_key(cx)?, print_log.into(), true)
    }

    runtime_fn! {
    /// Adds strings to a running print log stored on the global object.
    fn print(cx, _, arguments) {
        let argument = arguments.get(cx, 0);
        if !argument.is_string() {
            return type_error(cx, "print expects a string");
        }

        let global_object = cx.current_realm_ptr().global_object().as_object();

        let old_print_log = Self::get_print_log(cx, global_object)?;
        let new_print_log = StringValue::concat(cx, old_print_log, argument.as_string())?;
        Self::set_print_log(cx, global_object, new_print_log)?;

        Ok(cx.undefined())
    }}

    runtime_fn! {
    fn create_realm(cx, _, _) {
        // Create a new realm that also has the test262 object installed
        let mut realm = Realm::new(cx)?;
        realm.install_optional_globals(cx)?;

        get(cx, realm.global_object().as_object(), test_262_key(cx)?)
    }}

    runtime_fn! {
    fn eval_script(cx, _, arguments) {
        let script_text = arguments.get(cx, 0);
        if !script_text.is_string() {
            return type_error(cx, "expected a string");
        }

        // Use the file path of the active source file
        let file_path = cx
            .vm()
            .current_source_file()
            .map_or_else(|| "<anonymous>".to_string(), |source| source.path().to_string());

        let source =
            match Source::new_for_eval(file_path, script_text.as_string().to_wtf8_string()?) {
                Ok(source) => Rc::new(source),
                Err(error) => return syntax_parse_error(cx, &error),
            };

        evaluate_script(cx, cx.current_realm(), source)
    }}

    runtime_fn! {
    fn detach_array_buffer(cx, _, arguments) {
        let value = arguments.get(cx, 0);
        let Some(mut array_buffer) = value.as_opt::<ArrayBufferObject>() else {
            return Ok(cx.undefined());
        };
        // DetachArrayBuffer with an undefined key: a key mismatch is a TypeError (a wasm memory's
        // buffer, whose key is the memory)
        if array_buffer.detach_key() == DetachKey::WasmMemory {
            return type_error(cx, "cannot detach the buffer of a WebAssembly.Memory");
        }
        array_buffer.detach();

        Ok(cx.undefined())
    }}

    runtime_fn! {
    /// The call of the `IsHTMLDDA` hook object, which test262 requires to return null when called
    /// with no argument or with the empty string, so that algorithms which also call `document.all`
    /// can be tested. The object itself carries the [[IsHTMLDDA]] internal slot, set on it when the
    /// hook is created.
    fn is_htmldda(cx, _, arguments) {
        let first = arguments.get(cx, 0);
        if arguments.is_empty() || (first.is_string() && first.as_string().is_empty()) {
            return Ok(cx.null());
        }

        Ok(cx.undefined())
    }}
}

fn test_262_key(mut cx: Context) -> AllocResult<Handle<PropertyKey>> {
    let test_262_string = cx.alloc_static_string("$262")?;
    PropertyKey::string_handle(cx, test_262_string)
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use crate::{
        common::options::OptionsBuilder,
        handle_scope_guard,
        runtime::{
            Context, ContextBuilder, Handle, Realm, Value, embed,
            eval::expression::eval_typeof,
            type_utilities::{is_loosely_equal, is_strictly_equal, to_boolean, to_string},
        },
    };

    /// A realm with `$262` installed, which is how the test262 harness runs a test.
    fn context_with_262() -> (Context, Handle<Realm>) {
        let options = OptionsBuilder::new().expose_test_262(true).build().unwrap();
        let cx = ContextBuilder::new()
            .set_options(Rc::new(options))
            .build()
            .unwrap();

        let mut realm = cx.initial_realm();
        realm.install_optional_globals(cx).unwrap();

        (cx, realm)
    }

    fn run(cx: Context, realm: Handle<Realm>, source: &str) -> Handle<Value> {
        match embed::evaluate_script(cx, realm, source, "htmldda_test.js", 1) {
            Ok(value) => value,
            Err(error) => {
                let error = embed::exception_value(error);
                let message =
                    embed::with_realm(cx, realm, |cx| to_string(cx, error).unwrap().format());
                panic!("script threw: {}", message.unwrap())
            }
        }
    }

    /// The hook object, fetched the way a test fetches it.
    fn hook(cx: Context, realm: Handle<Realm>) -> Handle<Value> {
        run(cx, realm, "$262.IsHTMLDDA")
    }

    /// Every case must evaluate to the boolean `true`.
    fn assert_all_true(cx: Context, realm: Handle<Realm>, cases: &[&str]) {
        for case in cases {
            assert!(run(cx, realm, case).is_true(), "{case}");
        }
    }

    /// The three algorithms of ECMA-262 B.3.6, and the algorithms it deliberately leaves alone.
    #[test]
    fn htmldda_hook_has_the_annex_b_semantics() {
        let (cx, realm) = context_with_262();
        handle_scope_guard!(cx);

        assert_all_true(
            cx,
            realm,
            &[
                // B.3.6.1 typeof
                r#"typeof $262.IsHTMLDDA === "undefined""#,
                r#"typeof $262 === "object""#,
                r#"typeof $262.evalScript === "function""#,
                // B.3.6.2 ToBoolean, on every path that reads a value's truth
                "Boolean($262.IsHTMLDDA) === false",
                "!$262.IsHTMLDDA === true",
                r#"($262.IsHTMLDDA ? "t" : "f") === "f""#,
                "if ($262.IsHTMLDDA) { false } else { true }",
                "var t = 0; while ($262.IsHTMLDDA) { t++ } t === 0",
                "var n = 0; do { n += 1 } while ($262.IsHTMLDDA); n === 1",
                "[$262.IsHTMLDDA, 1, 2].filter(Boolean).length === 2",
                "[$262.IsHTMLDDA].some(function (v) { return v }) === false",
                // B.3.6.3 IsLooselyEqual, in either order
                "$262.IsHTMLDDA == null",
                "$262.IsHTMLDDA == undefined",
                "null == $262.IsHTMLDDA",
                "undefined == $262.IsHTMLDDA",
                "($262.IsHTMLDDA != null) === false",
                "($262.IsHTMLDDA != undefined) === false",
                // The identity of the object itself is unaffected
                "$262.IsHTMLDDA == $262.IsHTMLDDA",
                "$262.IsHTMLDDA === $262.IsHTMLDDA",
                // Equality that the annex does not touch
                "($262.IsHTMLDDA === undefined) === false",
                "($262.IsHTMLDDA === null) === false",
                "Object.is($262.IsHTMLDDA, undefined) === false",
                "[$262.IsHTMLDDA].includes($262.IsHTMLDDA)",
                // Nullish checks do not see the slot
                r#"($262.IsHTMLDDA ?? "d") === $262.IsHTMLDDA"#,
                r#"($262.IsHTMLDDA?.name) === "IsHTMLDDA""#,
                r#"(function (v = "d") { return v })($262.IsHTMLDDA) === $262.IsHTMLDDA"#,
                // Logical operators use ToBoolean, so they do see it
                r#"($262.IsHTMLDDA || "d") === "d""#,
                r#"($262.IsHTMLDDA && "d") === $262.IsHTMLDDA"#,
                // The object is still an ordinary callable object in every other way
                r#"Object.prototype.toString.call($262.IsHTMLDDA) === "[object Function]""#,
                "$262.IsHTMLDDA instanceof Function",
                "typeof $262.IsHTMLDDA.call === \"function\"",
                // Untouched types keep their old answers
                "({} == null) === false",
                "(0 == false)",
                "('' == 0)",
                "(null == undefined)",
                "(typeof {}) === \"object\"",
            ],
        );
    }

    /// test262 INTERPRETING.md: the object must also be callable, and its call returns null when
    /// there is no argument or the argument is the empty string.
    #[test]
    fn htmldda_hook_is_callable() {
        let (cx, realm) = context_with_262();
        handle_scope_guard!(cx);

        assert_all_true(
            cx,
            realm,
            &[
                "$262.IsHTMLDDA() === null",
                "$262.IsHTMLDDA('') === null",
                "$262.IsHTMLDDA(undefined) === undefined",
                "$262.IsHTMLDDA(0) === undefined",
                "$262.IsHTMLDDA('x') === undefined",
                "$262.IsHTMLDDA.apply(undefined, []) === null",
                "$262.IsHTMLDDA.apply(undefined, ['']) === null",
                "$262.IsHTMLDDA.apply(undefined, ['x']) === undefined",
                "$262.IsHTMLDDA.length === 0",
                "$262.IsHTMLDDA.name === 'IsHTMLDDA'",
                r#"(function () { return $262.IsHTMLDDA() === null })()"#,
                r#"[].concat($262.IsHTMLDDA('')).length === 1"#,
                r#"typeof $262.IsHTMLDDA.call === "function""#,
            ],
        );
    }

    /// The slot lives on the object: neither a primitive nor an ordinary object reports it, and
    /// giving it to one object must not give it to the objects that share its shape.
    #[test]
    fn htmldda_flag_is_private_and_survives_shape_changes() {
        let (cx, realm) = context_with_262();
        handle_scope_guard!(cx);

        for source in [
            "undefined",
            "null",
            "1",
            "''",
            "true",
            "({})",
            "[]",
            "$262",
            "$262.evalScript",
            "Symbol.iterator",
        ] {
            assert!(!run(cx, realm, source).is_htmldda(), "{source}");
        }
        assert!(hook(cx, realm).is_htmldda());

        run(cx, realm, "globalThis.pairs = [{ x: 1 }, { x: 1 }]");
        let first = run(cx, realm, "pairs[0]");
        let second = run(cx, realm, "pairs[1]");
        assert!(!first.is_htmldda() && !second.is_htmldda());

        let mut object = first.as_object();
        object.init_htmldda(cx).unwrap();

        assert!(first.is_htmldda());
        assert!(
            !second.is_htmldda(),
            "the flag went on a private copy of the shape, not on the shared one"
        );
        assert_eq!(
            eval_typeof(cx, *second).format().unwrap(),
            "object",
            "the sibling is unchanged"
        );
        assert_eq!(eval_typeof(cx, *first).format().unwrap(), "undefined");

        // Every shape mutation shallow-clones the flags, so the slot survives all of them.
        run(cx, realm, "pairs[0].y = 2; delete pairs[0].y;");
        assert!(run(cx, realm, "pairs[0]").is_htmldda());

        run(
            cx,
            realm,
            "Object.defineProperty(pairs[0], 'z', { value: 1, enumerable: true });",
        );
        assert!(run(cx, realm, "pairs[0]").is_htmldda());

        // More properties than the shape will hold transitions for, so it becomes a map mode shape.
        run(cx, realm, "for (var i = 0; i < 300; i++) { pairs[0]['k' + i] = i; }");
        let in_map_mode = run(cx, realm, "pairs[0]");
        assert!(in_map_mode.as_object().shape().is_map_mode());
        assert!(in_map_mode.is_htmldda());

        run(cx, realm, "Object.setPrototypeOf(pairs[0], null);");
        let reparented = run(cx, realm, "pairs[0]");
        assert!(reparented.is_htmldda());
        assert_eq!(eval_typeof(cx, *reparented).format().unwrap(), "undefined");

        // Last, because a non-extensible object cannot change its prototype.
        run(cx, realm, "Object.preventExtensions(pairs[0]);");
        assert!(run(cx, realm, "pairs[0]").is_htmldda());
        assert!(!run(cx, realm, "pairs[1]").is_htmldda());
    }

    /// The predicates the VM and the abstract operations call, rather than a script.
    #[test]
    fn htmldda_predicates_agree_with_the_scripts() {
        let (cx, realm) = context_with_262();
        handle_scope_guard!(cx);

        let htmldda = hook(cx, realm);
        let null = cx.null();
        let undefined = cx.undefined();

        assert!(htmldda.is_htmldda());
        assert!(!to_boolean(*htmldda));
        assert!(!to_boolean(*undefined));
        assert!(to_boolean(*run(cx, realm, "'s'")));

        for other in [null, undefined] {
            assert!(is_loosely_equal(cx, htmldda, other).unwrap());
            assert!(is_loosely_equal(cx, other, htmldda).unwrap());
            assert!(!is_strictly_equal(cx, htmldda, other).unwrap());
            assert!(!is_strictly_equal(cx, other, htmldda).unwrap());
        }

        assert!(is_loosely_equal(cx, htmldda, htmldda).unwrap());
        assert!(is_strictly_equal(cx, htmldda, htmldda).unwrap());

        // An object without the slot answers the way it did before, and so do the primitives.
        let plain = run(cx, realm, "({})");
        assert!(!is_loosely_equal(cx, plain, null).unwrap());
        assert!(!is_loosely_equal(cx, plain, undefined).unwrap());
        assert!(is_loosely_equal(cx, plain, plain).unwrap());

        let zero = run(cx, realm, "0");
        let empty_string = run(cx, realm, "''");
        assert!(is_loosely_equal(cx, zero, run(cx, realm, "false")).unwrap());
        assert!(is_loosely_equal(cx, zero, empty_string).unwrap());
        assert!(!is_loosely_equal(cx, htmldda, zero).unwrap());
        assert!(!is_loosely_equal(cx, htmldda, empty_string).unwrap());
        assert!(!is_loosely_equal(cx, zero, null).unwrap());
    }
}
