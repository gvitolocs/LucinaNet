//! The `Intl` namespace object (https://tc39.es/ecma402/#the-intl-object), section 8. It holds the
//! functions that every locale-sensitive object shares, and the constructors that later pieces add
//! will be installed here as well.

use crate::{
    intrinsic_methods,
    runtime::{
        Context, EvalResult, Handle, PropertyKey, Realm, Value,
        abstract_operations::{has_property, length_of_array_like},
        alloc_error::AllocResult,
        array_object::create_array_from_list,
        error::{range_error, type_error},
        get,
        intrinsic_builder::IntrinsicBuilder,
        intrinsics::{
            intl::locale_negotiation::{LocaleError, canonicalize_locale_list_with},
            intl::locale_object::LocaleObject,
            intrinsics::Intrinsic,
        },
        object_value::ObjectValue,
        to_string,
        type_utilities::to_object,
    },
    runtime_fn,
};

/// The Intl Namespace Object (https://tc39.es/ecma402/#sec-the-intl-namespace-object), 8.1.
pub struct IntlObject;

impl IntlObject {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        intrinsic_methods!(cx, builder, {
            get_canonical_locales IntlObject_get_canonical_locales (1),
            supported_values_of IntlObject_supported_values (1),
        });

        // The service constructors, as the service properties 8.2 lists them: writable and
        // configurable, and not enumerable, which is what `builder.data` installs and what
        // test262's `intl402/Collator/prop-desc.js` checks. Each piece of `Intl` adds its own line
        // here, in the table order of 8.2 and next to the intrinsic pair that `intrinsics.rs`
        // registers before this one: Intl.Collator is 8.2.1, Intl.DateTimeFormat 8.2.2,
        // Intl.DisplayNames 8.2.3, Intl.ListFormat 8.2.5, Intl.Locale 8.2.6, Intl.NumberFormat
        // 8.2.7, Intl.PluralRules 8.2.8, Intl.RelativeTimeFormat 8.2.9 and Intl.Segmenter 8.2.10.
        builder.data(
            cx.names.collator(),
            realm
                .get_intrinsic(Intrinsic::CollatorConstructor)
                .as_value(),
        )?;
        builder.data(
            cx.names.date_time_format(),
            realm
                .get_intrinsic(Intrinsic::DateTimeFormatConstructor)
                .as_value(),
        )?;
        builder.data(
            cx.names.display_names(),
            realm
                .get_intrinsic(Intrinsic::DisplayNamesConstructor)
                .as_value(),
        )?;
        builder.data(
            cx.names.list_format(),
            realm
                .get_intrinsic(Intrinsic::ListFormatConstructor)
                .as_value(),
        )?;
        builder.data(
            cx.names.locale_(),
            realm.get_intrinsic(Intrinsic::LocaleConstructor).as_value(),
        )?;
        builder.data(
            cx.names.number_format(),
            realm
                .get_intrinsic(Intrinsic::NumberFormatConstructor)
                .as_value(),
        )?;
        builder.data(
            cx.names.plural_rules(),
            realm
                .get_intrinsic(Intrinsic::PluralRulesConstructor)
                .as_value(),
        )?;
        builder.data(
            cx.names.relative_time_format(),
            realm
                .get_intrinsic(Intrinsic::RelativeTimeFormatConstructor)
                .as_value(),
        )?;
        builder.data(
            cx.names.segmenter(),
            realm
                .get_intrinsic(Intrinsic::SegmenterConstructor)
                .as_value(),
        )?;

        // Intl [ @@toStringTag ] (https://tc39.es/ecma402/#sec-intl-%symbol.tostringtag%), 8.1.1:
        // non-writable, non-enumerable, configurable, and the value that makes
        // `Object.prototype.toString.call(Intl)` report "[object Intl]".
        builder.to_string_tag(cx.names.intl())?;

        builder.build()
    }

    runtime_fn! {
    /// Intl.getCanonicalLocales (https://tc39.es/ecma402/#sec-intl.getcanonicallocales), 8.3.1.
    fn get_canonical_locales(cx, _, arguments) {
        // Step 1: ? CanonicalizeLocaleList(locales).
        let locales = canonicalize_locale_list_argument(cx, arguments.get(cx, 0))?;

        // Step 2: CreateArrayFromList(localeList). The list is already canonical and deduplicated,
        // in the order the caller asked.
        let mut elements = Vec::with_capacity(locales.len());
        for locale in &locales {
            elements.push(cx.alloc_string(locale)?.as_value());
        }

        Ok(create_array_from_list(cx, &elements)?.into())
    }}

    runtime_fn! {
    /// Intl.supportedValuesOf (https://tc39.es/ecma402/#sec-intl.supportedvaluesof), 8.3.2.
    fn supported_values(cx, _, arguments) {
        super::supported_values::supported_values_of(cx, arguments.get(cx, 0))
    }}
}

/// CanonicalizeLocaleList (https://tc39.es/ecma402/#sec-canonicalizelocalelist), 9.2.1, as far as
/// it has to touch a JS value: reading the argument and its elements. The checks and the rewriting
/// of each tag are `locale_negotiation::canonicalize_locale_list`, which is where they can be
/// tested without a heap. Every entry point that takes a `locales` argument goes through it,
/// NumberFormat's constructor included (9.2.1 through ResolveOptions, 9.2.8 step 1).
pub(crate) fn canonicalize_locale_list_argument(
    cx: Context,
    locales: Handle<Value>,
) -> EvalResult<Vec<String>> {
    // Step 1: no argument at all is the empty list, which is what lets every constructor read an
    // omitted `locales` as "use DefaultLocale".
    if locales.is_undefined() {
        return Ok(Vec::new());
    }

    // Step 3: a single string, or a single `Intl.Locale`, is the one-element list it stands for. The
    // second of the two is why `new Intl.NumberFormat(new Intl.Locale("de-AT"))` asks for Austrian
    // German, and why the Locale's own tag is what is read rather than the result of a `toString` a
    // subclass may have replaced.
    let locales_object =
        if locales.is_string() || LocaleObject::initialized_locale(cx, locales).is_some() {
            create_array_from_list(cx, &[locales])?.as_object()
        } else {
            // Step 4: ? ToObject(locales), so that an array, an array-like or any other iterable reads
            // by index.
            to_object(cx, locales)?
        };

    // Step 5: ? LengthOfArrayLike(localesObj).
    let length = length_of_array_like(cx, locales_object)?;

    // Steps 6-7, in order, stopping at the first element that is not a request for a locale.
    let mut tags = Vec::new();
    for index in 0..length {
        // Steps 7.1-7.2. A hole in an array is not a locale request at all, so it is skipped rather
        // than read and stringified.
        let key = PropertyKey::from_u64_handle(cx, index)?;
        if !has_property(cx, locales_object, key)? {
            continue;
        }

        // Step 7.3.1.
        let element = get(cx, locales_object, key)?;

        // Step 7.3.2: only a String or an Object can name a locale. Note 2 of 9.2.1 explains the
        // reason for the rule: the number NaN stringifies to "nan", which is a language tag.
        if !element.is_string() && !element.is_object() {
            return throw_locale_error(cx, LocaleError::NotAString);
        }

        // Steps 7.3.3-7.3.4: an [[InitializedLocale]] object contributes its [[Locale]] slot, which
        // is a canonicalized tag already and needs no coercion; anything else takes the
        // ? ToString(element) path.
        let tag = match LocaleObject::initialized_locale(cx, element) {
            // Step 7.3.3: [[Locale]] is a String by 15.4, and every one of them this engine wrote came
            // out of `alloc_string`.
            Some(locale) => locale.as_string(),
            None => to_string(cx, element)?,
        };

        // A well-formed language tag is ASCII, so the lossy conversion only turns the requests that
        // are already invalid into a tag that no longer parses, which 6.2.1 rejects anyway.
        tags.push(String::from_utf8_lossy(tag.to_wtf8_string()?.as_bytes()).into_owned());
    }

    // Steps 7.3.5-7.3.7: IsWellFormedLanguageTag, CanonicalizeUnicodeLocaleId and the removal of
    // duplicates that 9.2.1 shares with every other entry point that takes a locale list.
    match canonicalize_locale_list_with(cx.locale_cache().canonicalizer(), &tags) {
        Ok(seen) => Ok(seen),
        Err(error) => throw_locale_error(cx, error),
    }
}

/// The exception ECMA-402 prescribes for each way a locale request can fail.
pub(crate) fn throw_locale_error<T>(cx: Context, error: LocaleError) -> EvalResult<T> {
    match error {
        // 9.2.1 step 7.3.5: a tag that is not well-formed (6.2.1) is a RangeError.
        LocaleError::InvalidTag => range_error(cx, "Intl locale is not a well-formed language tag"),
        // 9.2.1 step 7.3.2: the element was not a String or an Object.
        LocaleError::NotAString => {
            type_error(cx, "Intl locale list elements must be strings or objects")
        }
    }
}
