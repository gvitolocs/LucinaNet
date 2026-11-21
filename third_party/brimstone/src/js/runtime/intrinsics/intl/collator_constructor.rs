//! The Intl.Collator constructor (https://tc39.es/ecma402/#sec-intl.collator-constructor), 10.1,
//! and the two properties of 10.2 that are not its prototype.

use crate::{
    intrinsic_methods,
    runtime::{
        Context, Handle, Realm,
        alloc_error::AllocResult,
        array_object::create_array_from_list,
        intrinsic_builder::IntrinsicBuilder,
        intrinsics::{
            intl::{
                collator_object::CollatorObject,
                collator_options::{locale_matcher, resolve_collator},
                intl_object::canonicalize_locale_list_argument,
                locale_negotiation::supported_locales_of,
                options::coerce_options_to_object,
            },
            intrinsics::Intrinsic,
            rust_runtime::RuntimeFunction,
        },
        object_value::ObjectValue,
    },
    runtime_fn,
};

/// %Intl.Collator% (https://tc39.es/ecma402/#sec-intl.collator-constructor), 10.1.
pub struct CollatorConstructor;

impl CollatorConstructor {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        // The `length` of 0 is what test262's `Collator/length.js` checks: both parameters of
        // 10.1.1 are bracketed, and 20.1 of ECMA-262 leaves optional parameters out of it. The
        // `name` is the last portion of the canonical name "Intl.Collator".
        let mut builder = IntrinsicBuilder::constructor(
            cx,
            realm,
            RuntimeFunction::CollatorConstructor_construct,
            0,
            cx.names.collator(),
            Intrinsic::FunctionPrototype,
        )?;

        builder.prototype(Intrinsic::CollatorPrototype)?;

        intrinsic_methods!(cx, builder, {
            supported_locales_of CollatorConstructor_supported_locales_of (1),
        });

        builder.build()
    }

    runtime_fn! {
    /// The Intl.Collator constructor (https://tc39.es/ecma402/#sec-intl.collator), 10.1.1.
    fn construct(cx, _, arguments) {
        // Step 1: the legacy rule that makes `Intl.Collator()` construct as well as
        // `new Intl.Collator()` does, with the active function standing in for NewTarget.
        let new_target = match cx.current_new_target() {
            Some(new_target) => new_target,
            None => cx.current_function(),
        };

        // Steps 2-3: OrdinaryCreateFromConstructor, which reads `newTarget.prototype` and so
        // happens before anything of the arguments does. The pointer goes into a handle straight
        // away: steps 5-22 allocate, and a collection moves the block out from under a bare pointer,
        // which the caller would then be handed back.
        let mut collator = CollatorObject::new(cx, new_target)?.to_handle();

        // Steps 5-22: the internal slots, in the order the steps compute them.
        let resolved = resolve_collator(cx, arguments.get(cx, 0), arguments.get(cx, 1))?;

        // The two slots that are String values; 10.3.2 reports them unchanged, so they are worth
        // turning into strings here rather than on every `resolvedOptions` call.
        let collation = cx.alloc_string(&resolved.collation)?.as_value();
        let locale = cx.alloc_string(&resolved.locale)?.as_value();
        collator.initialize(resolved.settings, resolved.usage, collation, locale);

        Ok(collator.as_value())
    }}

    runtime_fn! {
    /// Intl.Collator.supportedLocalesOf
    /// (https://tc39.es/ecma402/#sec-intl.collator.supportedlocalesof), 10.2.2.
    fn supported_locales_of(cx, _, arguments) {
        // Step 2: CanonicalizeLocaleList(locales). [[AvailableLocales]] is step 1, and it is the
        // built in list that `locale_negotiation` already works from.
        let requested = canonicalize_locale_list_argument(cx, arguments.get(cx, 0))?;

        // Step 3: FilterLocales (9.2.9), which reads its own `options` argument: steps 1-2 of it
        // are the coercion and the matcher, and 4.3 keeps the requested tag rather than the one it
        // matched, which is why "en-GB" is listed as "en-GB".
        let options = coerce_options_to_object(cx, arguments.get(cx, 1))?;
        let matcher = locale_matcher(cx, options)?;
        let subset = supported_locales_of(&requested, matcher);

        let mut elements = Vec::with_capacity(subset.len());
        for locale in &subset {
            elements.push(cx.alloc_string(locale)?.as_value());
        }

        Ok(create_array_from_list(cx, &elements)?.as_value())
    }}
}
