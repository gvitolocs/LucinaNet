//! The `Intl.Locale` instance (https://tc39.es/ecma402/#sec-locale-objects), 15.1 and 15.4: the
//! constructor of 15.1.1, the UpdateLanguageId of 15.1.2 and the slots the two compute.
//!
//! One internal slot is stored, [[Locale]], and the other seven are read back out of it. That is not
//! a shortcut: 15.1.3 steps 5 to 7 write every value the locale record ends up with into the tag
//! before 15.1.1 steps 36 to 43 copy the record into the slots, so a slot is always the `-u-` keyword
//! of [[Locale]] itself and the two cannot drift apart. 15.5.1 to 15.5.5 read the same tag for the
//! base name and its subtags, which is why nothing here keeps a second copy of them.
//!
//! It also means one write instead of eight, which is what the copying collector cares about: a
//! `Value` read out of a heap object is a pointer into the block it lives in, so the only slot this
//! file fills is filled once, after the last allocation of the constructor, from a handle taken
//! before it. The same rule holds in [`super::list_format_object`] and the three other services that
//! were rooted this way.
//!
//! 15.4 lists [[CaseFirst]] and [[Numeric]] among the slots that exist only when "kf" and "kn" are in
//! %Intl.Locale%.[[LocaleExtensionKeys]] (15.2.2). They are: %Intl.Collator%.[[RelevantExtensionKeys]]
//! of this build contains both, so `LOCALE_EXTENSION_KEYS` is the full list of seven and neither
//! getter is conditional.

use super::collator_options::option_text;
use super::intl_object::throw_locale_error;
use super::locale_negotiation::{
    canonicalize_locale_tag, language_subtag, locale_base_name, locale_language, locale_region,
    locale_script, locale_variants, make_locale_record, region_subtag, script_subtag, type_value,
    unicode_extension_value, variant_subtag, weekday_to_u_value,
};
use super::options::{OptionType, coerce_options_to_object, get_option};
use crate::extend_object;
use crate::runtime::{
    Context, Handle, PropertyKey, Value,
    error::{range_error, type_error},
    eval_result::EvalResult,
    gc::{HeapItem, HeapPtr, HeapVisitor},
    intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue,
    ordinary_object::ObjectBuilder,
    to_string,
};

/// %Intl.Locale%.[[LocaleExtensionKeys]]
/// (https://tc39.es/ecma402/#sec-properties-of-intl-locale-constructor), 15.2.2: the five keys the
/// constructor always reads plus "kf" and "kn", which 15.2.2 adds when the Collator has them and
/// this build's Collator does.
pub const LOCALE_EXTENSION_KEYS: [&str; 7] = ["ca", "co", "fw", "hc", "kf", "kn", "nu"];

extend_object! {
    /// Properties of Intl.Locale Instances (https://tc39.es/ecma402/#sec-properties-of-intl-locale-instances),
    /// 15.4.
    pub struct LocaleObject {
        /// [[Locale]], which 15.1.1 step 36 sets to the [[locale]] field of the locale record. Its
        /// absence is this engine's spelling of `[[InitializedLocale]]`, which 15.3.2 step 2 and the
        /// rest of the accessors require and a constructor that threw leaves unset.
        locale: Option<Value>,
    }
}

impl LocaleObject {
    /// 15.1.1 steps 1 to 6: `OrdinaryCreateFromConstructor(newTarget, "%Intl.Locale.prototype%",
    /// internalSlotsList)` with every slot empty. Unlike the four services of 9.1 there is no legacy
    /// call mode: step 1 throws when NewTarget is undefined, so no %Intl%.[[FallbackSymbol]] instance
    /// is ever made here.
    pub fn new(cx: Context, new_target: Handle<ObjectValue>) -> EvalResult<HeapPtr<Self>> {
        // Step 6, through `ObjectBuilder`, which reads `newTarget.prototype` and falls back to the
        // intrinsic step 6 names: OrdinaryCreateFromConstructor steps 1 and 2.
        let mut locale = ObjectBuilder::<Self>::new(cx)
            .constructor_proto(new_target, Intrinsic::LocalePrototype)?
            .build()?;

        // The slots of steps 2 to 5 are empty as the creation leaves them, and stay so until
        // `initialize` writes [[Locale]] in one go.
        locale.locale = None;

        Ok(locale)
    }

    /// `? RequireInternalSlot(loc, [[InitializedLocale]])`, which every step 2 of 15.3's accessors and
    /// methods performs.
    pub fn require_initialized(cx: Context, this_value: Handle<Value>) -> EvalResult<Handle<Self>> {
        if !this_value.is_object() {
            return type_error(cx, "Intl.Locale method called on incompatible receiver");
        }

        match this_value.as_object().as_opt::<Self>() {
            Some(instance) if instance.locale.is_some() => Ok(instance),
            _ => type_error(cx, "Intl.Locale method called on an uninitialized instance"),
        }
    }

    /// [[Locale]] of a value that is an initialized Locale instance, and `None` for anything else:
    /// 15.1.1 step 8 and CanonicalizeLocaleList (9.2.1) step 7.3.3, which are the two places a
    /// `Locale` object is accepted where a tag is asked for.
    pub fn initialized_locale(cx: Context, value: Handle<Value>) -> Option<Handle<Value>> {
        if !value.is_object() {
            return None;
        }

        value
            .as_object()
            .as_opt::<Self>()
            .and_then(|instance| instance.locale)
            .map(|locale| locale.to_handle(cx))
    }

    /// 15.1.1 steps 7 to 43. The instance is a handle rather than `&mut self` on purpose, as in every
    /// other service constructor: each step below allocates, and [[Locale]] is written once at the
    /// end, through a handle taken before the first of them.
    pub fn initialize(
        mut instance: Handle<Self>,
        mut cx: Context,
        tag: Handle<Value>,
        options: Handle<Value>,
    ) -> EvalResult<()> {
        // Step 7: a tag is a String or an Object, and nothing else, before it is read in any way.
        if !tag.is_string() && !tag.is_object() {
            return type_error(cx, "Intl.Locale's tag must be a string or an object");
        }

        // Step 8: the tag of a Locale instance, which needs no coercion. Step 9: anything else,
        // including a plain object, is stringified - and a Locale whose own `toString` is shadowed
        // lands here rather than there, which is what the test262 case of the same name checks.
        let tag = match Self::initialized_locale(cx, tag) {
            Some(locale) => to_string(cx, locale)?,
            None => to_string(cx, tag)?,
        };

        // A well-formed language tag is ASCII, so the lossy conversion only renames the tags that are
        // not well-formed and that step 11 refuses anyway.
        let tag = String::from_utf8_lossy(tag.to_wtf8_string()?.as_bytes()).into_owned();

        // Step 10, before the tag is checked: an `options` that throws on the way to an object stops
        // the constructor before the tag's own spelling has been looked at.
        let options = coerce_options_to_object(cx, options)?;

        // Steps 11 and 13, together: 9.2.1's own well-formedness check and the canonicalization of
        // 6.2.1, which is what turns "en-u-kf-true" into the bare "kf" that 15.1.3 step 4 reads back.
        let tag = match canonicalize_locale_tag(&tag) {
            Ok(tag) => tag,
            // Step 11's RangeError, which is 9.2.1 step 7.3.5's for a tag that is not a well-formed
            // language tag, and step 13's canonicalization is the same pass.
            Err(error) => return throw_locale_error(cx, error),
        };

        // Step 12's note is why 11 and 13 come first: an option overrides a subtag by name, so both
        // have to be in the spelling canonicalization settled on.
        // Step 14.
        let tag = update_language_id(cx, &tag, options)?;

        // Steps 15 to 34, the record `opt`, read in the order the clause names them so that a getter
        // on the bag sees that order.
        let calendar = text_option(cx, options, cx.names.calendar(), None)?;
        if let Some(calendar) = &calendar {
            // Step 17.1.
            check_unicode_type(cx, "calendar", calendar)?;
        }

        let collation = text_option(cx, options, cx.names.collation(), None)?;
        if let Some(collation) = &collation {
            // Step 20.1.
            check_unicode_type(cx, "collation", collation)?;
        }

        let first_day = text_option(cx, options, cx.names.first_day_of_week(), None)?;
        let first_day = match &first_day {
            // Steps 23.1 and 23.2: the day becomes its `fw` u-value first, and that is what has to be
            // a `type`, which a day always is.
            Some(day) => {
                let day = weekday_to_u_value(day);
                check_unicode_type(cx, "firstDayOfWeek", &day)?;
                Some(day)
            }
            None => None,
        };

        // Step 25: GetOption's own list of allowed values is the RangeError here, so no check of
        // 15.1.1's follows it.
        let hour_cycle = text_option(cx, options, cx.names.hour_cycle(), None)?;
        if let Some(hour_cycle) = &hour_cycle {
            check_one_of(cx, "hourCycle", hour_cycle, &["h11", "h12", "h23", "h24"])?;
        }

        let case_first = text_option(cx, options, cx.names.case_first(), None)?;
        if let Some(case_first) = &case_first {
            check_one_of(cx, "caseFirst", case_first, &["upper", "lower", "false"])?;
        }

        // Steps 29 and 30: `numeric` is a Boolean option whose record value is the text of the
        // boolean, which is why the `Some` below is a String and not a bool.
        let numeric = get_option(
            cx,
            options,
            cx.names.numeric(),
            OptionType::Boolean,
            &[],
            Some(cx.undefined()),
        )?;
        let numeric = if numeric.is_undefined() {
            None
        } else {
            Some(option_text(cx, numeric)?)
        };

        let numbering_system = text_option(cx, options, cx.names.numbering_system(), None)?;
        if let Some(numbering_system) = &numbering_system {
            // Step 33.1.
            check_unicode_type(cx, "numberingSystem", numbering_system)?;
        }

        // Steps 18, 21, 24, 26, 28, 31 and 34 into one record, in the key order 15.2.2 gives.
        let overrides: [(&str, Option<&str>); LOCALE_EXTENSION_KEYS.len()] = [
            ("ca", calendar.as_deref()),
            ("co", collation.as_deref()),
            ("fw", first_day.as_deref()),
            ("hc", hour_cycle.as_deref()),
            ("kf", case_first.as_deref()),
            ("kn", numeric.as_deref()),
            ("nu", numbering_system.as_deref()),
        ];

        // Step 35: MakeLocaleRecord (https://tc39.es/ecma402/#sec-makelocalerecord), 15.1.3, which is
        // where each override is canonicalized for its key and written into the tag. Its error is
        // step 6's canonicalization refusing the tag the options made, which is the same refusal
        // step 13's canonicalization answers for a tag that arrived as an argument.
        let (locale, _) = match make_locale_record(&tag, &LOCALE_EXTENSION_KEYS, &overrides) {
            Ok(record) => record,
            Err(error) => return throw_locale_error(cx, error),
        };

        // Steps 36 to 43: the seven slots, every one of them the tag's own keyword, which is why the
        // tag is the only thing stored. `locale` is [[Locale]] and the record's [[locale]] at once.
        let locale = cx.alloc_string(&locale)?.as_value();
        instance.locale = Some(*locale);

        // Step 44.
        Ok(())
    }

    /// [[Locale]], as a value: 15.3.15's `toString`, 15.1.1 step 8 and CanonicalizeLocaleList step
    /// 7.3.3 return it as it is.
    pub fn locale(&self) -> Option<Value> {
        self.locale
    }

    /// [[Locale]] as the text the operations of 15.5 work on.
    pub(crate) fn tag(&self, cx: Context) -> EvalResult<String> {
        let Some(locale) = self.locale else {
            // The question `require_initialized` has already answered.
            return type_error(cx, "Intl.Locale method called on an uninitialized instance");
        };

        // The value is a pointer into the block this object was collected into, so it goes into a
        // handle here, before anything else allocates.
        option_text(cx, locale.to_handle(cx))
    }

    /// UnicodeExtensionValue (https://tc39.es/ecma402/#sec-unicodeextensionvalue), 15.5.6, for one of
    /// the keys of 15.2.2: the value 15.1.3 step 4.6 left in the locale record and step 6.1 wrote
    /// into [[Locale]]. `None` is the spec's "empty", and the empty String is a keyword the tag wrote
    /// without a value, which 15.5.6 step 1.4 returns as it stands.
    fn extension_value(&self, cx: Context, key: &str) -> EvalResult<Option<String>> {
        Ok(unicode_extension_value(&self.tag(cx)?, key))
    }

    /// [[Calendar]] (15.1.1 step 37), [[Collation]] (step 38), [[FirstDayOfWeek]] (step 39),
    /// [[HourCycle]] (step 40) and [[NumberingSystem]] (step 43): each a keyword above, in the
    /// spelling InsertUnicodeExtensionAndCanonicalize left it in.
    pub fn slot(&self, cx: Context, key: &str) -> EvalResult<Option<String>> {
        self.extension_value(cx, key)
    }

    /// [[CaseFirst]] (step 41), which is the record's [[kf]] and so the empty String when the tag
    /// wrote the keyword with no value.
    pub fn case_first(&self, cx: Context) -> EvalResult<Option<String>> {
        self.extension_value(cx, "kf")
    }

    /// [[Numeric]] (step 42): `true` for the record's "true" or for the empty String, `false` for any
    /// other value and also for no `kn` at all, which is step 42.2's "else".
    pub fn numeric(&self, cx: Context) -> EvalResult<bool> {
        Ok(matches!(self.extension_value(cx, "kn")?.as_deref(), Some("true") | Some("")))
    }

    /// GetLocaleBaseName (https://tc39.es/ecma402/#sec-getlocalebasename), 15.5.1: [[Locale]] up to
    /// its `-u-` extension, which is the longest prefix the `unicode_language_id` production matches.
    pub fn base_name(&self, cx: Context) -> EvalResult<String> {
        Ok(locale_base_name(&self.tag(cx)?).to_owned())
    }

    /// GetLocaleLanguage (15.5.2), GetLocaleScript (15.5.3), GetLocaleRegion (15.5.4) and
    /// GetLocaleVariants (15.5.5): the four subtag runs of the base name that the accessors of
    /// 15.3.8, 15.3.14, 15.3.13 and 15.3.23 report. The assert of 15.5.2 step 3, that the first
    /// subtag is a language, is 15.1.1 step 11's check: [[Locale]] was well-formed when it was stored
    /// and nothing rewrites its base name after that.
    pub fn language(&self, cx: Context) -> EvalResult<String> {
        Ok(locale_language(&self.base_name(cx)?).to_owned())
    }

    pub fn script(&self, cx: Context) -> EvalResult<Option<String>> {
        Ok(locale_script(&self.base_name(cx)?).map(str::to_owned))
    }

    pub fn region(&self, cx: Context) -> EvalResult<Option<String>> {
        Ok(locale_region(&self.base_name(cx)?).map(str::to_owned))
    }

    pub fn variants(&self, cx: Context) -> EvalResult<Option<String>> {
        Ok(locale_variants(&self.base_name(cx)?).map(str::to_owned))
    }
}

impl HeapItem for LocaleObject {
    fn byte_size(locale: HeapPtr<Self>) -> usize {
        locale.object_byte_size()
    }

    fn visit_pointers(mut locale: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        locale.visit_object_pointers(visitor);

        if let Some(tag) = &mut locale.locale {
            visitor.visit_value(tag);
        }
    }
}

/// GetOption (https://tc39.es/ecma402/#sec-getoption), 9.2.11, for the `string` options of 15.1.1 and
/// 15.1.2: one read of the property, step 4's stringification of what it returned, and `None` for the
/// undefined that step 2 answers with when the property is absent and `default` is the spec's
/// `undefined`. Step 5's list of allowed values is left to the caller, since three of clause 15's
/// options check what they read against a grammar rather than a list: `check_one_of` below is the
/// same RangeError for the two that do have a list.
fn text_option(
    mut cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
    default: Option<&str>,
) -> EvalResult<Option<String>> {
    // Every option of 15.1.1 and 15.1.2 has the spec's `undefined` as its default, which GetOption
    // step 2 answers for an absent property; `None` here is that case, not the `required` marker.
    let default = match default {
        Some(text) => Some(cx.alloc_string(text)?.as_value()),
        None => Some(cx.undefined()),
    };

    let value = get_option(cx, options, property, OptionType::String, &[], default)?;

    if value.is_undefined() {
        return Ok(None);
    }

    option_text(cx, value).map(Some)
}

/// The RangeError of 15.1.1 step 17.1, 20.1, 23.2 and 33.1, and of the two options that name a fixed
/// set of values (steps 25 and 27), which is the same failure with a different list in the message.
fn check_unicode_type(cx: Context, option: &str, value: &str) -> EvalResult<()> {
    if type_value(value) {
        Ok(())
    } else {
        range_error(
            cx,
            &format!(
                "Intl.Locale's '{option}' option must be a Unicode locale type: \
                 three to eight alphanumeric characters"
            ),
        )
    }
}

fn check_one_of(cx: Context, option: &str, value: &str, allowed: &[&str]) -> EvalResult<()> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        range_error(
            cx,
            &format!("Intl.Locale's '{option}' option must be one of: {}", allowed.join(", ")),
        )
    }
}

/// UpdateLanguageId (https://tc39.es/ecma402/#sec-updatelanguageid), 15.1.2: the four subtags of the
/// base name, each one read from `options` with the tag's own as its default, and the extension of
/// the tag kept out of the way of all of them.
///
/// `tag` is already canonicalized (15.1.1 step 13), so a subtag this function takes from it needs no
/// checking of its own; the checks of steps 3, 5.1, 7.1 and 9 are there for what a program wrote.
fn update_language_id(cx: Context, tag: &str, options: Handle<ObjectValue>) -> EvalResult<String> {
    // Step 1.
    let base_name = locale_base_name(tag);

    // Steps 2 and 3.
    let language = text_option(cx, options, cx.names.language(), Some(locale_language(base_name)))?
        .unwrap_or_else(|| locale_language(base_name).to_owned());
    if !language_subtag(&language) {
        return range_error(
            cx,
            "Intl.Locale's 'language' option must be a Unicode language subtag: \
             two or three letters, or 'und', or five to eight letters",
        );
    }

    // Steps 4 and 5.
    let script = text_option(cx, options, cx.names.script(), locale_script(base_name))?;
    if let Some(script) = &script {
        if !script_subtag(script) {
            return range_error(
                cx,
                "Intl.Locale's 'script' option must be a Unicode script subtag: four letters",
            );
        }
    }

    // Steps 6 and 7.
    let region = text_option(cx, options, cx.names.region(), locale_region(base_name))?;
    if let Some(region) = &region {
        if !region_subtag(region) {
            return range_error(
                cx,
                "Intl.Locale's 'region' option must be a Unicode region subtag: \
                 two letters or three digits",
            );
        }
    }

    // Steps 8 and 9.
    let variants = text_option(cx, options, cx.names.variants(), locale_variants(base_name))?;
    if let Some(variants) = &variants {
        // Step 9.1: a variant list that names nothing. `variants: ""` is this, not "no variants".
        if variants.is_empty() {
            return range_error(cx, "Intl.Locale's 'variants' option cannot be the empty string");
        }

        // Steps 9.2 to 9.4.
        let lower = variants.to_ascii_lowercase();
        let mut subtags: Vec<&str> = Vec::new();
        for subtag in lower.split('-') {
            if !variant_subtag(subtag) {
                return range_error(
                    cx,
                    "Intl.Locale's 'variants' option must be '-' separated Unicode variant subtags: \
                     five to eight alphanumeric characters, or four starting with a digit",
                );
            }

            // Step 9.5.
            if subtags.contains(&subtag) {
                return range_error(
                    cx,
                    "Intl.Locale's 'variants' option cannot repeat a variant subtag",
                );
            }

            subtags.push(subtag);
        }
    }

    // Step 10: everything the base name is not - the `-u-` extension, a `-t-` or `-b-` extension, a
    // private use sequence - moves to the new tag untouched, which is what lets an option override a
    // keyword of the extension rather than replace it.
    let all_extensions = &tag[base_name.len()..];

    // Steps 11 to 15. The subtags keep the spelling the caller gave them; 15.1.3 step 6's
    // InsertUnicodeExtensionAndCanonicalize is what re-cases them, and Annex C lowercases an
    // extension's keywords on the same pass.
    let mut new_tag = language;
    for subtag in [&script, &region, &variants].into_iter().flatten() {
        new_tag.push('-');
        new_tag.push_str(subtag);
    }
    new_tag.push_str(all_extensions);

    Ok(new_tag)
}
