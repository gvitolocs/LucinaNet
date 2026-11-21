//! The Intl.PluralRules constructor: 17.1.1's construct and 17.2.2's `supportedLocalesOf`.
//!
//! It is a service constructor of 9.1 in every way but one: 17.1.1 step 1 throws when NewTarget is
//! undefined, so unlike Collator's and NumberFormat's it has no legacy call mode, no
//! %Intl%.[[FallbackSymbol]] instance and no ChainPluralRules.

use super::intl_object::canonicalize_locale_list_argument;
use super::locale_negotiation::{Matcher, supported_locales_of};
use super::options::{coerce_options_to_object, get_name_option};
use super::plural_rules_object::PluralRulesObject;
use crate::runtime::{
    Context, Handle, Realm,
    alloc_error::AllocResult,
    array_object::create_array_from_list,
    error::type_error,
    intrinsic_builder::IntrinsicBuilder,
    intrinsics::{intrinsics::Intrinsic, rust_runtime::RuntimeFunction},
    object_value::ObjectValue,
};
use crate::{intrinsic_methods, runtime_fn};

/// %Intl.PluralRules% (https://tc39.es/ecma402/#sec-intl-pluralrules-constructor), 17.1.
pub struct PluralRulesConstructor;

impl PluralRulesConstructor {
    /// Properties of the Intl.PluralRules Constructor
    /// (https://tc39.es/ecma402/#sec-properties-of-the-intl-pluralrules-constructor), 17.2.
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        // `length` is 0 because 17.1.1's signature is `Intl.PluralRules ( [ locales
        // [ , options ] ] )`, where both parameters are optional, and `name` is "PluralRules",
        // which is what the constructor's own reports and `Function.prototype.toString` show.
        let mut builder = IntrinsicBuilder::constructor(
            cx,
            realm,
            RuntimeFunction::PluralRulesConstructor_construct,
            0,
            cx.names.plural_rules(),
            Intrinsic::FunctionPrototype,
        )?;

        // 17.2.1: `prototype`, non-writable, non-enumerable and non-configurable.
        builder.prototype(Intrinsic::PluralRulesPrototype)?;

        // 17.2.2.
        intrinsic_methods!(cx, builder, {
            supported_locales_of PluralRulesConstructor_supported_locales_of (1),
        });

        builder.build()
    }

    runtime_fn! {
    /// Intl.PluralRules ( https://tc39.es/ecma402/#sec-intl-pluralrules-constructor ), 17.1.1.
    fn construct(cx, _, arguments) {
        // Step 1: there is no call mode to fall back on, so a plain call of the constructor is the
        // error, and it is the error before any option or locale has been read.
        let Some(new_target) = cx.current_new_target() else {
            return type_error(cx, "Intl.PluralRules cannot be called without new");
        };

        // Step 2, with every slot of 17.4 empty. The pointer goes into a handle straight away: the
        // steps below allocate, and a collection moves the block out from under a bare pointer, which
        // the caller would then be handed back.
        let plural_rules = PluralRulesObject::new(cx, new_target)?.to_handle();

        // Steps 3 to 13: ResolveOptions (9.2.8) and the reading of every option.
        PluralRulesObject::initialize(
            plural_rules,
            cx,
            arguments.get(cx, 0),
            arguments.get(cx, 1),
        )?;

        // Step 14.
        Ok(plural_rules.into())
    }}

    runtime_fn! {
    /// Intl.PluralRules.supportedLocalesOf ( https://tc39.es/ecma402/#sec-intl.pluralrules.supportedlocalesof ), 17.2.2.
    fn supported_locales_of(cx, _, arguments) {
        // Step 1, [[AvailableLocales]]: the locales this build was generated with.
        // Step 2: ? CanonicalizeLocaleList(locales).
        let requested_locales = canonicalize_locale_list_argument(cx, arguments.get(cx, 0))?;

        // Step 3: ? FilterLocales (9.2.9), whose steps 1 and 2 read the second argument's
        // `localeMatcher` before its steps 3 to 5 filter.
        let options = coerce_options_to_object(cx, arguments.get(cx, 1))?;
        let matcher = get_name_option(
            cx,
            options,
            cx.names.locale_matcher(),
            &["best fit", "lookup"],
            "best fit",
        )?;

        let supported = supported_locales_of(
            &requested_locales,
            if matcher == "lookup" {
                Matcher::Lookup
            } else {
                Matcher::BestFit
            },
        );

        // 9.2.9 step 5: CreateArrayFromList, in the order the caller asked and with the tags as the
        // caller spelled them, canonicalized.
        let mut elements = Vec::with_capacity(supported.len());
        for locale in &supported {
            elements.push(cx.alloc_string(locale)?.as_value());
        }

        Ok(create_array_from_list(cx, &elements)?.into())
    }}
}
