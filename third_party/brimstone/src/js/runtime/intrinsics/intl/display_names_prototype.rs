//! The `Intl.DisplayNames.prototype` object
//! (https://tc39.es/ecma402/#sec-properties-of-the-intl-displaynames-prototype-object), 12.3:
//! `resolvedOptions`, `of` and the tag that makes an instance report "[object Intl.DisplayNames]".
//!
//! The instance, the reading of its options and the tables the names come out of are
//! [`super::display_names_object`]; this file is only the two methods a program calls.

use super::display_names_object::{
    DisplayNamesFallback, DisplayNamesObject, DisplayNamesType, canonical_code_for_display_names,
};
use super::number_format_prototype::{report_str, report_value};
use crate::runtime::{
    Context, Handle, Realm,
    alloc_error::AllocResult,
    error::{range_error, type_error},
    intrinsic_builder::IntrinsicBuilder,
    intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue,
    ordinary_object::ordinary_object_create,
    to_string,
};
use crate::{intrinsic_methods, runtime_fn};

/// %Intl.DisplayNames.prototype%
/// (https://tc39.es/ecma402/#sec-properties-of-the-intl-displaynames-prototype-object), 12.3.
pub struct DisplayNamesPrototype;

impl DisplayNamesPrototype {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        // 12.3: an ordinary object that is not an instance and has none of the slots of one, so
        // `resolvedOptions` and `of` below both refuse it.
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        intrinsic_methods!(cx, builder, {
            resolved_options DisplayNamesPrototype_resolved_options (0),
            of DisplayNamesPrototype_of (1),
        });

        // 12.3.4: @@toStringTag, the String "Intl.DisplayNames", non-writable and configurable.
        builder.to_string_tag(cx.names.intl_display_names())?;

        builder.build()
    }

    runtime_fn! {
    /// Intl.DisplayNames.prototype.resolvedOptions ( https://tc39.es/ecma402/#sec-intl.displaynames.prototype.resolvedoptions ), 12.3.2.
    fn resolved_options(cx, this_value, _) {
        // Steps 1 and 2.
        let display_names = DisplayNamesObject::require_initialized(cx, this_value)?;

        let Some(resolved) = display_names.resolved() else {
            // The question `require_initialized` has already answered.
            return type_error(cx, "Intl.DisplayNames method called on an uninitialized instance");
        };

        // [[Locale]] is a raw pointer inside the instance's own data, so it goes into a handle here,
        // before anything else allocates: a collection during step 3 would move the string and leave
        // the copy above pointing at the block it was copied out of.
        let locale = resolved.locale.to_handle(cx);

        // Step 3: `OrdinaryObjectCreate(%Object.prototype%)`.
        let options = ordinary_object_create(cx)?;

        // Steps 4 and 5, in the order Table 18 lists the slots. The four slots that always have a
        // value are the options 12.1.1 steps 7, 9, 11 and 12 wrote; [[Fields]] is not in the table
        // because it is not an option, and [[LocaleData]] and [[RelevantExtensionKeys]] leave nothing
        // else to report.
        report_value(cx, options, cx.names.locale(), locale)?;
        report_str(cx, options, cx.names.style(), resolved.settings.style.as_str())?;
        report_str(cx, options, cx.names.type_(), resolved.settings.ty.as_str())?;
        report_str(cx, options, cx.names.fallback(), resolved.settings.fallback.as_str())?;

        // Step 4.3, for the one row of Table 18 whose value can be `undefined`: 12.1.1 step 19 sets
        // [[LanguageDisplay]] only when the type is "language", so for the other five the property is
        // not created at all.
        if resolved.settings.ty == DisplayNamesType::Language {
            report_str(
                cx,
                options,
                cx.names.language_display(),
                resolved.settings.language_display.as_str(),
            )?;
        }

        // Step 5.
        Ok(options.as_value())
    }}

    runtime_fn! {
    /// Intl.DisplayNames.prototype.of ( https://tc39.es/ecma402/#sec-intl.displaynames.prototype.of ), 12.3.3.
    fn of(cx, this_value, arguments) {
        // Steps 1 and 2: `RequireInternalSlot(displayNames, [[InitializedDisplayNames]])`.
        let display_names = DisplayNamesObject::require_initialized(cx, this_value)?;

        let Some(resolved) = display_names.resolved() else {
            return type_error(cx, "Intl.DisplayNames method called on an uninitialized instance");
        };

        // Step 3: `Set code to ? ToString(code)`.
        let code = to_string(cx, arguments.get(cx, 0))?.to_wtf8_string()?;
        let code = String::from_utf8_lossy(code.as_bytes()).into_owned();

        // Step 4: ? CanonicalCodeForDisplayNames([[Type]], code), whose eight throws are the RangeError
        // below. The canonicalized code is what step 6 looks up and what step 7 returns, so a request
        // for "dk" and one for "DA" are the same key here, and the fallback of a code that has no name
        // is the regularized spelling of it rather than the spelling the program typed.
        let Ok(code) = canonical_code_for_display_names(resolved.settings.ty, &code) else {
            return range_error(
                cx,
                &format!(
                    "Intl.DisplayNames of type '{}' has no code {code:?} to name",
                    resolved.settings.ty.as_str(),
                ),
            );
        };

        // Steps 5 and 6: [[Fields]][[<code>]], the table of this build's locale, type and style read at
        // the key step 4 returned. A `DataError` out of it means the build baked no display-names
        // marker for a locale this engine resolves to, which no code can bring about and which is
        // therefore reported as the build's own fault rather than as the fallback.
        let name = match resolved.settings.display_name(cx, &code) {
            Ok(name) => name,
            Err(error) => {
                return range_error(
                    cx,
                    &format!(
                        "Intl.DisplayNames has no display name data for '{}': {error}",
                        resolved.settings.locale.tag()
                    ),
                );
            }
        };

        let Some(name) = name else {
            // Steps 7 and 8: the fallback, which is the code itself for "code" and `undefined` for
            // "none". The code is the canonicalized one, as step 7 says.
            return match resolved.settings.fallback {
                DisplayNamesFallback::Code => Ok(cx.alloc_string(&code)?.as_value()),
                DisplayNamesFallback::None => Ok(cx.undefined()),
            };
        };

        Ok(cx.alloc_string(&name)?.as_value())
    }}
}
