//! The `Intl.Locale` constructor (https://tc39.es/ecma402/#sec-properties-of-the-intl-locale-constructor),
//! 15.2: 15.1.1's construct, and the properties the clause gives the function itself.
//!
//! There is no `supportedLocalesOf` on this constructor, which is the one way it differs from the
//! four services of 9.1: 15.2 lists `prototype`, `length` and `name` and nothing else, because a
//! `Locale` is a description of a tag rather than a formatter that has to say which tags it can
//! format. `Intl.Locale.supportedLocalesOf` is absent, and 15.3.16 to 15.3.22 are where a program
//! asks what the locale knows.
//!
//! 15.2.2's [[LocaleExtensionKeys]] internal slot is not a property and so appears nowhere here: it
//! is [`LOCALE_EXTENSION_KEYS`](super::locale_object::LOCALE_EXTENSION_KEYS), the list the
//! constructor reads.

use super::locale_object::LocaleObject;
use crate::runtime::{
    Context, Handle, Realm,
    alloc_error::AllocResult,
    error::type_error,
    intrinsic_builder::IntrinsicBuilder,
    intrinsics::{intrinsics::Intrinsic, rust_runtime::RuntimeFunction},
    object_value::ObjectValue,
};
use crate::runtime_fn;

/// %Intl.Locale% (https://tc39.es/ecma402/#sec-intl-locale-constructor), 15.1.
pub struct LocaleConstructor;

impl LocaleConstructor {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        // `length` is 1 because 15.1.1's signature is `Intl.Locale ( tag [ , options ] )`, where only
        // `options` is optional, and `name` is "Locale" (15.2.4).
        let mut builder = IntrinsicBuilder::constructor(
            cx,
            realm,
            RuntimeFunction::LocaleConstructor_construct,
            1,
            cx.names.locale_(),
            Intrinsic::FunctionPrototype,
        )?;

        // 15.2.1: `prototype`, non-writable, non-enumerable and non-configurable, which is what
        // `builder.prototype` installs.
        builder.prototype(Intrinsic::LocalePrototype)?;

        builder.build()
    }

    runtime_fn! {
    /// Intl.Locale ( https://tc39.es/ecma402/#sec-intl-locale-constructor ), 15.1.1.
    fn construct(cx, _, arguments) {
        // Step 1: no call mode of any kind, so `Intl.Locale("en")` is the error and it is the error
        // before the tag has been looked at.
        let Some(new_target) = cx.current_new_target() else {
            return type_error(cx, "Intl.Locale cannot be called without new");
        };

        // Steps 2 to 6, with every slot of 15.4 empty.
        let locale = LocaleObject::new(cx, new_target)?.to_handle();

        // Steps 7 to 43. The handle above is what keeps the allocations of 15.1.1 and 15.1.2 from
        // moving the instance out from under the pointer that goes back to the caller.
        LocaleObject::initialize(locale, cx, arguments.get(cx, 0), arguments.get(cx, 1))?;

        // Step 44.
        Ok(locale.into())
    }}
}
