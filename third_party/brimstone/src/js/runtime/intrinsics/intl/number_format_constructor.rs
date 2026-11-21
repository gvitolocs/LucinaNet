//! The Intl.NumberFormat constructor: 16.1.1's call and construct, and 16.2.2's
//! `supportedLocalesOf`. What the two have in common with the rest of the service is that the
//! instance and its options are the object file's, and this file only decides which of them a
//! program reaches.

use super::intl_object::canonicalize_locale_list_argument;
use super::locale_negotiation::{Matcher, supported_locales_of};
use super::number_format_object::NumberFormatObject;
use super::options::{coerce_options_to_object, get_name_option};
use crate::runtime::{
    Context, Handle, Realm,
    abstract_operations::{define_property_or_throw, ordinary_has_instance},
    alloc_error::AllocResult,
    array_object::create_array_from_list,
    intrinsic_builder::IntrinsicBuilder,
    intrinsics::{intrinsics::Intrinsic, rust_runtime::RuntimeFunction},
    object_value::ObjectValue,
    property_descriptor::PropertyDescriptor,
};
use crate::{intrinsic_methods, runtime_fn};

/// %Intl.NumberFormat% (https://tc39.es/ecma402/#sec-intl-numberformat-constructor), 16.1.
pub struct NumberFormatConstructor;

impl NumberFormatConstructor {
    /// Properties of the Intl.NumberFormat Constructor
    /// (https://tc39.es/ecma402/#sec-properties-of-the-intl-numberformat-constructor), 16.2.
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        // `length` is 0 because 16.1.1's signature is `Intl.NumberFormat ( [ locales
        // [ , options ] ] )`, where both parameters are optional, and `name` is "NumberFormat",
        // which is what the constructor's own reports and `Function.prototype.toString` show.
        let mut builder = IntrinsicBuilder::constructor(
            cx,
            realm,
            RuntimeFunction::NumberFormatConstructor_construct,
            0,
            cx.names.number_format(),
            Intrinsic::FunctionPrototype,
        )?;

        // 16.2.1: `prototype`, non-writable, non-enumerable and non-configurable.
        builder.prototype(Intrinsic::NumberFormatPrototype)?;

        // 16.2.2.
        intrinsic_methods!(cx, builder, {
            supported_locales_of NumberFormatConstructor_supported_locales_of (1),
        });

        builder.build()
    }

    runtime_fn! {
    /// Intl.NumberFormat ( https://tc39.es/ecma402/#sec-initializenumberformat ), 16.1.1.
    fn construct(cx, this_value, arguments) {
        // Step 1: NewTarget, or the active function object when there is none.
        let new_target = match cx.current_new_target() {
            Some(new_target) => new_target,
            // The legacy call mode of 4.3 Note 1, which 16.1.1.1 and 16.5.9 are built around:
            // `Intl.NumberFormat("da")` without `new` makes its object from the function being
            // called, exactly as `new` would have.
            None => cx.current_function(),
        };

        // Step 2: OrdinaryCreateFromConstructor, with every slot of 16.4 empty. The pointer goes into
        // a handle straight away: steps 3 to 26 allocate, and a collection moves the block out from
        // under a bare pointer, which the caller would then be handed back.
        let number_format = NumberFormatObject::new(cx, new_target)?.to_handle();

        // Steps 3 to 26: ResolveOptions (9.2.8) and the reading of every option.
        NumberFormatObject::initialize(
            number_format,
            cx,
            arguments.get(cx, 0),
            arguments.get(cx, 1),
        )?;

        // Steps 27 and 28: ChainNumberFormat
        // (https://tc39.es/ecma402/#sec-chainnumberformat), which is the normative optional half of
        // that call mode.
        if cx.current_new_target().is_none()
            && ordinary_has_instance(
                cx,
                cx.get_intrinsic(Intrinsic::NumberFormatConstructor).into(),
                this_value,
            )?
        {
            // 16.1.1.1 step 1: the `this` value was already a NumberFormat when the call started,
            // which is what `Object.create(Intl.NumberFormat.prototype)` then `Intl.NumberFormat`
            // called on it means. The instance goes on it under %Intl%. [[FallbackSymbol]], non-
            // writable, non-enumerable and non-configurable, and the object rather than the
            // instance is what the caller gets back.
            define_property_or_throw(
                cx,
                this_value.as_object(),
                cx.symbols.intl_fallback_symbol(),
                PropertyDescriptor::frozen(number_format.into()),
            )?;

            return Ok(this_value);
        }

        // Step 28, and 16.1.1.1 step 2 for a call that `this` was not a NumberFormat for, which
        // includes every call made with `new`.
        Ok(number_format.into())
    }}

    runtime_fn! {
    /// Intl.NumberFormat.supportedLocalesOf ( https://tc39.es/ecma402/#sec-intl.numberformat.supportedlocalesof ), 16.2.2.
    fn supported_locales_of(cx, _, arguments) {
        // Step 1, [[AvailableLocales]]: the locales this build was generated with.
        // Step 2: ? CanonicalizeLocaleList(locales).
        let requested_locales = canonicalize_locale_list_argument(cx, arguments.get(cx, 0))?;

        // Step 3: ? FilterLocales (https://tc39.es/ecma402/#sec-filterlocales), 9.2.9, whose steps 1
        // and 2 read the second argument's `localeMatcher` before its steps 3 to 5 filter.
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
