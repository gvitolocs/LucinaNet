//! The Intl.Collator prototype object
//! (https://tc39.es/ecma402/#sec-properties-of-the-intl-collator-prototype-object), 10.3.

use crate::{
    intrinsic_methods,
    runtime::{
        Context, Handle, Realm,
        abstract_operations::create_data_property_or_throw,
        alloc_error::AllocResult,
        builtin_function::BuiltinFunction,
        error::type_error,
        intrinsic_builder::IntrinsicBuilder,
        intrinsics::{
            intl::collator_object::{CollatorObject, compare_strings, this_collator},
            intrinsics::Intrinsic,
            rust_runtime::RuntimeFunction,
        },
        object_value::ObjectValue,
        ordinary_object::ordinary_object_create,
        to_string,
    },
    runtime_fn,
};

/// %Intl.Collator.prototype%
/// (https://tc39.es/ecma402/#sec-properties-of-the-intl-collator-prototype-object), 10.3.
pub struct CollatorPrototype;

impl CollatorPrototype {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        // 10.3: an ordinary object whose [[Prototype]] is %Object.prototype%, and which is not
        // itself a collator - it has no [[InitializedCollator]], which is why every method of it
        // throws when called on the prototype.
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        // The `constructor` property of 10.3.1 is added by `intrinsics.rs` once the constructor
        // itself exists.
        intrinsic_methods!(cx, builder, {
            resolved_options CollatorPrototype_resolved_options (0),
        });

        // get Intl.Collator.prototype.compare (10.3.3), an accessor whose setter is `undefined` and
        // which is non-enumerable and configurable like every accessor of the spec.
        builder.getter(cx.names.compare(), RuntimeFunction::CollatorPrototype_compare)?;

        // Intl.Collator.prototype [ @@toStringTag ]
        // (https://tc39.es/ecma402/#sec-intl.collator.prototype-@@tostringtag), 10.3.4:
        // non-writable, non-enumerable and configurable, and the value that makes
        // `Object.prototype.toString.call(new Intl.Collator())` report "[object Intl.Collator]".
        builder.to_string_tag_str("Intl.Collator")?;

        builder.build()
    }

    runtime_fn! {
    /// Intl.Collator.prototype.resolvedOptions
    /// (https://tc39.es/ecma402/#sec-intl.collator.prototype.resolvedoptions), 10.3.2.
    fn resolved_options(cx, this_value, _) {
        // Steps 1-2: RequireInternalSlot(collator, [[InitializedCollator]]).
        let collator = this_collator(cx, this_value)?;
        let settings = collator
            .settings()
            .expect("`this_collator` only returns a collator that 10.1.1 step 22 initialized");

        // The seven values of Table 3, each in its own spelling: [[Locale]] and [[Collation]] are
        // already Strings, [[Usage]], [[Sensitivity]] and [[CaseFirst]] are the names of the enums
        // they hold, and [[IgnorePunctuation]] and [[Numeric]] are Booleans.
        let locale = collator.locale().to_handle(cx);
        let usage = cx
            .alloc_static_string(collator.usage().as_str())?
            .as_value();
        let sensitivity = cx
            .alloc_static_string(settings.sensitivity.as_str())?
            .as_value();
        let ignore_punctuation = cx.bool(settings.ignore_punctuation);
        let collation = collator.collation().to_handle(cx);
        let numeric = cx.bool(settings.numeric);
        let case_first = cx
            .alloc_static_string(settings.case_first.as_str())?
            .as_value();

        // Step 3: OrdinaryObjectCreate(%Object.prototype%).
        let options = ordinary_object_create(cx)?;

        // Step 4, in the order of Table 3. A row is left out when its extension key is not relevant
        // (step 4.4.2), and 10.2.3 makes both "kn" and "kf" relevant to this build - so `numeric`
        // and `caseFirst` are always reported here, the way the other engines report them.
        create_data_property_or_throw(cx, options, cx.names.locale(), locale)?;
        create_data_property_or_throw(cx, options, cx.names.usage(), usage)?;
        create_data_property_or_throw(cx, options, cx.names.sensitivity(), sensitivity)?;
        create_data_property_or_throw(
            cx,
            options,
            cx.names.ignore_punctuation(),
            ignore_punctuation,
        )?;
        create_data_property_or_throw(cx, options, cx.names.collation(), collation)?;
        create_data_property_or_throw(cx, options, cx.names.numeric(), numeric)?;
        create_data_property_or_throw(cx, options, cx.names.case_first(), case_first)?;

        // Step 5.
        Ok(options.as_value())
    }}

    runtime_fn! {
    /// get Intl.Collator.prototype.compare
    /// (https://tc39.es/ecma402/#sec-intl-collator-prototype-get-intl-collator-compare), 10.3.3.
    fn compare(cx, this_value, _) {
        // Steps 1-2: RequireInternalSlot(collator, [[InitializedCollator]]).
        let mut collator = this_collator(cx, this_value)?;

        // Step 3: the function is created once and handed out again from [[BoundCompare]] after
        // that, which is what makes `collator.compare === collator.compare` hold.
        let cached = collator.bound_compare();
        if !cached.is_undefined() {
            return Ok(cached.to_handle(cx));
        }

        // Step 3.2: CreateBuiltinFunction of the closure of step 3.1, with length 2, the empty name
        // and no properties. The collator is attached to it as a private element - the same way
        // `Proxy.revocable` attaches a proxy to its revoker - because that is how this engine lets
        // a builtin function capture a value. 10.3.3's closure is not `Function.prototype.bind`, so
        // it ignores its `this` value entirely.
        let realm = cx.current_realm();
        let mut closure = BuiltinFunction::create(
            cx,
            RuntimeFunction::collator_prototype_compare_closure,
            2,
            cx.names.empty_string(),
            realm,
            None,
        )?;
        closure.private_element_set(cx, cx.symbols.collator().cast(), collator.as_value())?;

        collator.set_bound_compare(closure.as_value());

        Ok(closure.into())
    }}
}

runtime_fn! {
/// The closure of 10.3.3 step 3.1: the function that `compare` hands out, holding the collator it
/// was made for.
fn compare_closure(cx, _, arguments) {
    // Steps 3.1.1-3.1.2: ? ToString(x), then ? ToString(y).
    let left = to_string(cx, arguments.get(cx, 0))?;
    let right = to_string(cx, arguments.get(cx, 1))?;

    // The collator the accessor attached above. Step 2 of 10.3.3 checked its
    // [[InitializedCollator]] before this function was ever made, and nothing clears the slot, so a
    // function that cannot find it is a bug rather than a program.
    let closure = cx.current_function();
    let Some(captured) = closure.private_element_find(cx, cx.symbols.collator().cast()) else {
        return type_error(cx, "Intl.Collator compare function is not bound to a collator");
    };
    let Some(settings) = captured.value().cast::<CollatorObject>().settings() else {
        return type_error(cx, "Intl.Collator compare function is not bound to a collator");
    };

    // Step 3.1.3: CompareStrings(collator, xString, yString).
    compare_strings(cx, settings, left, right)
}}
