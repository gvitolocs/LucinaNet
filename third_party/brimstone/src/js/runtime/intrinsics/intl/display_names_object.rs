//! The `Intl.DisplayNames` instance (https://tc39.es/ecma402/#sec-properties-of-intl-displaynames-instances),
//! 12.4: what 12.1.1 computes for its slots, the [[Fields]] of 12.4 that 12.3.3 step 6 looks a code
//! up in, and CanonicalCodeForDisplayNames
//! (https://tc39.es/ecma402/#sec-canonicalcodefordisplaynames), 12.5.1.
//!
//! The constructor and the methods a program calls are
//! [`super::display_names_constructor`] and [`super::display_names_prototype`].
//!
//! An instance holds what 12.4 lists and nothing else: the resolved locale and the four options.
//! [[Fields]] is the sixth slot of that list and the only one that is not a value a program chose:
//! 12.2.3 spells it out as a view of `[[LocaleData]][[<locale>]][[types]][[<type>]][[<style>]]`, with
//! `[[<languageDisplay>]]` in the chain for type "language" alone, and it is therefore a record of
//! what the data has rather than anything negotiated. Like every other service's data it lives in
//! [`super::cache::LocaleCache`] under the [`DisplayNamesSettings`] the instance keeps, and the
//! lookup of 12.3.3 step 6 is [`DisplayNamesSettings::name_of`].
//!
//! The language, region and script names are read through `icu_experimental`'s own
//! `multi::{Language,Region,Script}DisplayNames`, whose `of` runs UTS 35's fall-back from a style's
//! column to the plain column below it. That is the same fall-back 12.2.3's "should contain" records
//! leave to the implementation, so nothing here chooses between the columns: what the data has no
//! column for is a code the [[Fields]] has no field for, and the answer is then 12.3.3 steps 7 and 8,
//! the code or `undefined` - never a name this build made up.

use brimstone_intl_data::IntlDataProvider;
use icu_experimental::dimension::provider::currency::extended::CurrencyExtendedDataV1;
use icu_experimental::displaynames::multi::{
    LanguageDisplayNames, RegionDisplayNames, ScriptDisplayNames,
};
use icu_experimental::displaynames::{DisplayNamesOptions, DisplayNamesPreferences, Style};
use icu_locale::Locale as IcuLocale;
use icu_locale::subtags::{Language, Region, Script};
use icu_plurals::PluralOperands;
use icu_provider::prelude::DataError;

use super::cache::DataLocale;
use super::intl_object::canonicalize_locale_list_argument;
use super::locale_negotiation::{
    LocaleError, Matcher, canonicalize_locale_tag, language_subtag, region_subtag, resolve_locale,
    script_subtag, type_value, variant_subtag,
};
use super::number_format_currency::{load_optional, marker_attributes};
use super::options::{
    get_enum_option, get_name_option, get_options_object, get_required_name_option,
};
use super::plural_rules_object::{PluralRuleType, PluralRulesSettings};
use crate::extend_object;
use crate::runtime::{
    Context, Handle, Value,
    error::{range_error, type_error},
    eval_result::EvalResult,
    gc::{HeapPtr, HeapVisitor},
    intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue,
    ordinary_object::ObjectBuilder,
};

/// [[Type]] (https://tc39.es/ecma402/#sec-Intl.DisplayNames), 12.1.1 step 9: which of the six sets of
/// names 12.3.3 reads, and which production 12.5.1 checks a code against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayNamesType {
    Language,
    Region,
    Script,
    Currency,
    Calendar,
    DateTimeField,
}

impl DisplayNamesType {
    /// The six names 12.1.1 step 8 lists, in its order.
    pub const NAMES: [&'static str; 6] = [
        "language",
        "region",
        "script",
        "currency",
        "calendar",
        "dateTimeField",
    ];

    /// The String value 12.3.2 reports for it, which is also the option value 12.1.1 step 8 accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Language => "language",
            Self::Region => "region",
            Self::Script => "script",
            Self::Currency => "currency",
            Self::Calendar => "calendar",
            Self::DateTimeField => "dateTimeField",
        }
    }

    /// 12.1.1 step 8 has already refused everything but the six names of this enum.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "language" => Some(Self::Language),
            "region" => Some(Self::Region),
            "script" => Some(Self::Script),
            "currency" => Some(Self::Currency),
            "calendar" => Some(Self::Calendar),
            "dateTimeField" => Some(Self::DateTimeField),
            _ => None,
        }
    }
}

/// [[Style]] (https://tc39.es/ecma402/#sec-Intl.DisplayNames), 12.1.1 step 7: which column of the
/// data a name is read out of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayNamesStyle {
    Narrow,
    Short,
    Long,
}

impl DisplayNamesStyle {
    /// The three names 12.1.1 step 6 lists, in its order.
    pub const NAMES: [&'static str; 3] = ["narrow", "short", "long"];

    /// The String value 12.3.2 reports for it, which is also the option value 12.1.1 step 6 accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Narrow => "narrow",
            Self::Short => "short",
            Self::Long => "long",
        }
    }

    /// 12.1.1 step 6 has already refused everything but the three names of this enum.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "narrow" => Some(Self::Narrow),
            "short" => Some(Self::Short),
            "long" => Some(Self::Long),
            _ => None,
        }
    }

    /// The `icu_experimental` style the tables are read with. The three names line up one for one,
    /// and the fourth style that library has - `Style::Menu`, CLDR's `menuNames`, which 12.1.1 step 6
    /// does not list and no ECMA-402 option can ask for - is never passed.
    fn icu(self) -> Style {
        match self {
            Self::Narrow => Style::Narrow,
            Self::Short => Style::Short,
            Self::Long => Style::Long,
        }
    }
}

/// [[Fallback]] (https://tc39.es/ecma402/#sec-Intl.DisplayNames), 12.1.1 step 11: what 12.3.3 steps 7
/// and 8 answer with when the [[Fields]] has no field for the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayNamesFallback {
    Code,
    None,
}

impl DisplayNamesFallback {
    /// The two names 12.1.1 step 10 lists, in its order.
    pub const NAMES: [&'static str; 2] = ["code", "none"];

    /// The String value 12.3.2 reports for it, which is also the option value 12.1.1 step 10 accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::None => "none",
        }
    }

    /// 12.1.1 step 10 has already refused everything but the two names of this enum.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "code" => Some(Self::Code),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// [[LanguageDisplay]] (https://tc39.es/ecma402/#sec-Intl.DisplayNames), 12.1.1 steps 16 and 19, and
/// only ever set when [[Type]] is "language" - which is why 12.3.2 step 4.3 leaves the property out
/// of `resolvedOptions` for the other five types rather than reporting an `undefined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayNamesLanguageDisplay {
    Dialect,
    Standard,
}

impl DisplayNamesLanguageDisplay {
    /// The two names 12.1.1 step 16 lists, in its order.
    pub const NAMES: [&'static str; 2] = ["dialect", "standard"];

    /// The String value 12.3.2 reports for it, which is also the option value 12.1.1 step 16 accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dialect => "dialect",
            Self::Standard => "standard",
        }
    }

    /// 12.1.1 step 16 has already refused everything but the two names of this enum.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "dialect" => Some(Self::Dialect),
            "standard" => Some(Self::Standard),
            _ => None,
        }
    }
}

/// Table 19: Codes For Date Time Field of DisplayNames, which is what IsValidDateTimeFieldCode
/// (https://tc39.es/ecma402/#sec-isvaliddatetimefieldcode), 12.5.2, looks a field up in. In the
/// table's order and spelled as it spells them, camelCase included: 12.5.1 step 5.2 returns the code
/// unchanged, so the two cases of a name that differ only by a letter are two different answers here
/// and only one of them is in the table.
pub const DATE_TIME_FIELD_CODES: [&str; 12] = [
    "era",
    "year",
    "quarter",
    "month",
    "weekOfYear",
    "weekday",
    "day",
    "dayPeriod",
    "hour",
    "minute",
    "second",
    "timeZoneName",
];

/// [[Locale]] (12.1.1 step 12) and the four options of steps 7, 9, 11 and 19, as the `Copy` key the
/// tables of 12.2.3's [[LocaleData]] are cached under and the settings a name is read from.
///
/// The data locale rather than the reported tag because the `-u-ca-gregory` a request can leave on
/// that tag names no locale the data has an entry for, and because 12.2.3's
/// [[RelevantExtensionKeys]] is the empty list: no extension changes which tables are read, which is
/// why every entry of [`super::cache::LocaleCache`] is keyed this way.
///
/// [[LanguageDisplay]] is in the key although this build's data cannot answer it - the dialect names
/// are `LocaleDisplayNamesV1`, which `icu/intl_markers.txt` does not bake - so that the settings say
/// what 12.3.2 reports and nothing pretends to have been used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DisplayNamesSettings {
    pub locale: DataLocale,
    pub ty: DisplayNamesType,
    pub style: DisplayNamesStyle,
    pub fallback: DisplayNamesFallback,
    pub language_display: DisplayNamesLanguageDisplay,
}

/// The three sets of names of 12.2.3 that this build's data has tables for, for one built-in locale
/// and one style: the language, region and script records of
/// `[[LocaleData]][[<locale>]][[types]]`.
///
/// The other three types are not here because they have no marker of their own to load: currency
/// names are one field of the `CurrencyExtendedDataV1` entry that 16.1.3's style "currency" already
/// reads, which [`DisplayNamesSettings::name_of`] reaches through
/// [`super::number_format_currency`], and calendar and dateTimeField have no display-name table in
/// this generation of `icu_experimental` at all, so those two types answer 12.3.3 steps 7 and 8 for
/// every code.
pub struct DisplayNamesTables {
    language: LanguageDisplayNames,
    region: RegionDisplayNames,
    script: ScriptDisplayNames,
}

impl DisplayNamesTables {
    /// The tables of `locale` for one `style`, read from the baked data: 12.1.1 steps 13 to 22's
    /// `resolvedLocaleData`, `types`, `typeFields` and `styleFields` in one object.
    ///
    /// The `DataError` a marker that will not load is not something a program can bring about - the
    /// locale was resolved out of `BUILT_IN_LOCALES`, whose every entry has all three tables, and the
    /// style is one of the three 12.1.1 step 6 allows - so callers turn it into the RangeError that
    /// names the build rather than the program as the cause.
    pub(crate) fn build(locale: DataLocale, style: DisplayNamesStyle) -> Result<Self, DataError> {
        let icu_locale = locale
            .tag()
            .parse::<IcuLocale>()
            .map_err(|_| DataError::custom("a built-in locale tag that does not parse"))?;

        let preferences = DisplayNamesPreferences::from(&icu_locale);

        // `DisplayNamesOptions` is non-exhaustive, so it is built from its default and then set. The
        // two fields left at their defaults are `fallback` and `language_display`: the first of them
        // is what 12.3.3 steps 7 and 8 do, which the caller runs because this library answers `None`
        // for a name it does not have and does not itself fall back; the second is the dialect chain
        // of 12.1.1 step 19.2, whose table is not baked.
        let mut options = DisplayNamesOptions::default();
        options.style = Some(style.icu());

        Ok(Self {
            language: LanguageDisplayNames::try_new_unstable(
                &IntlDataProvider,
                preferences,
                options,
            )?,
            region: RegionDisplayNames::try_new_unstable(&IntlDataProvider, preferences, options)?,
            script: ScriptDisplayNames::try_new_unstable(&IntlDataProvider, preferences, options)?,
        })
    }

    /// The name `code` has in the table of `ty`, or `None` where the [[Fields]] of 12.3.3 step 6 has
    /// no field `[[<code>]]`.
    ///
    /// `code` is what 12.5.1 returned, so its case is already the one the data keys by. A code that
    /// is well-formed for its type and yet names no subtag of the size that table is keyed at - a
    /// dialect tag such as "en-GB", which is a `unicode_language_id` but is not a three-letter
    /// language - is `None` for the same reason an empty table is: the record of 12.2.3 only "should
    /// contain" the keys this build has rows for, and 12.3.3's fallback is what the spec makes of a
    /// code it has no field for.
    fn name_of(&self, ty: DisplayNamesType, code: &str) -> Option<&str> {
        match ty {
            DisplayNamesType::Language => self.language.of(code.parse::<Language>().ok()?),
            DisplayNamesType::Region => self.region.of(code.parse::<Region>().ok()?),
            DisplayNamesType::Script => self.script.of(code.parse::<Script>().ok()?),
            DisplayNamesType::Currency
            | DisplayNamesType::Calendar
            | DisplayNamesType::DateTimeField => None,
        }
    }
}

impl DisplayNamesSettings {
    /// The [[Fields]] of 12.4 for one already canonicalized code: the tables of this build's locale
    /// and style, read by type. Currency goes its own way because its name is not in a display-names
    /// marker but in the currency data of 16.1.3; the two types with no table at all answer `None`,
    /// which is 12.3.3 step 6 finding no field.
    pub(crate) fn display_name(self, cx: Context, code: &str) -> Result<Option<String>, DataError> {
        if self.ty == DisplayNamesType::Currency {
            return currency_name(cx, self.locale, code);
        }

        let tables = cx.locale_cache().display_names(self.locale, self.style)?;

        Ok(tables.name_of(self.ty, code).map(str::to_owned))
    }
}

/// The currency name 12.2.3's type "currency" reads out of the `CurrencyExtendedDataV1` entry that
/// 16.1.3's style "currency" reads the same name out of: the name of the currency, asked for with the
/// plural operands of the number `1`.
///
/// A display name names a currency and not an amount of it, and CLDR stores one name per plural
/// category, so `1` is the category the spec's table of display names means and the answer to `of`
/// does not depend on any amount. The data has no short or narrow column of currency names either, so
/// all three styles of 12.1.1 step 6 read this one entry.
///
/// `Ok(None)` is the hole in the data that 12.3.3 steps 7 and 8 fall back on, which is what a code
/// outside ISO 4219 or one this build's data carries nothing for means; `Err` is a load that failed
/// for a reason no code can bring about.
fn currency_name(cx: Context, locale: DataLocale, code: &str) -> Result<Option<String>, DataError> {
    let attributes = marker_attributes(code)?;
    let data_locale = icu_data_locale(locale)?;

    let Some(extended) = load_optional::<CurrencyExtendedDataV1>(&data_locale, attributes)? else {
        return Ok(None);
    };

    let rules = cx
        .locale_cache()
        .plural_rules(PluralRulesSettings { locale, kind: PluralRuleType::Cardinal });

    // The operands of `1`, which is the category the name of the currency alone is stored under: the
    // `PluralOperands` of a bare integer has no visible fraction digits, which is what "one US
    // dollar" is pluralized by.
    let operands = PluralOperands::from(1_u128);

    Ok(Some(
        extended
            .get()
            .elements
            .get(operands, rules.rules())
            .1
            .to_owned(),
    ))
}

/// The `icu_provider` locale the tables of one built-in locale are asked with, which is the tag of
/// `BUILT_IN_LOCALES` parsed the way every other Intl service in this engine parses it.
fn icu_data_locale(locale: DataLocale) -> Result<icu_provider::prelude::DataLocale, DataError> {
    let icu_locale = locale
        .tag()
        .parse::<IcuLocale>()
        .map_err(|_| DataError::custom("a built-in locale tag that does not parse"))?;

    Ok(icu_provider::prelude::DataLocale::from(&icu_locale))
}

/// CanonicalCodeForDisplayNames (https://tc39.es/ecma402/#sec-canonicalcodefordisplaynames), 12.5.1:
/// checks that `code` is well-formed for `ty` and answers its case-regularized form, which is the key
/// 12.3.3 step 6 looks [[Fields]] up by and the string 12.3.3 step 7 returns when it finds nothing.
///
/// It takes and returns an owned `String` because the regularized form is not the input in any of the
/// six branches: the language branch is the whole of 6.2.2, which reorders and recases the subtags of
/// a tag, and the other five case-fold at least part of it.
///
/// [`LocaleError::InvalidTag`] is the RangeError the eight "throw" steps of 12.5.1 answer with;
/// [`LocaleError::NotAString`] cannot happen, because 12.3.3 step 3 has already run `ToString`.
pub(crate) fn canonical_code_for_display_names(
    ty: DisplayNamesType,
    code: &str,
) -> Result<String, LocaleError> {
    match ty {
        // Steps 1.1 and 1.2: the code has to be matched by `unicode_language_id` and be well-formed,
        // which together are the whole of 6.2.1 plus the production's own shape, and step 1.3 is the
        // 6.2.2 canonicalization `canonicalize_locale_tag` runs. The shape is checked on the way out
        // rather than on the way in because canonicalization does not add or remove a subtag: a tag
        // with a Unicode extension sequence, a private-use subtag or a `-t-` transform is matched by
        // `unicode_locale_id` and not by `unicode_language_id`, and step 1.1 refuses it whichever
        // order the two checks run in.
        DisplayNamesType::Language => {
            let canonical = canonicalize_locale_tag(code)?;

            if is_unicode_language_id(&canonical) {
                Ok(canonical)
            } else {
                Err(LocaleError::InvalidTag)
            }
        }

        // Steps 2.1 and 2.2.
        DisplayNamesType::Region => {
            if !region_subtag(code) {
                return Err(LocaleError::InvalidTag);
            }

            Ok(code.to_ascii_uppercase())
        }

        // Steps 3.1 to 3.5: four letters, the first upper and the rest lower. Step 3.3's assert that
        // the length is 4 and that every unit is an ASCII letter already follows from `script_subtag`,
        // so nothing here repeats it.
        DisplayNamesType::Script => {
            if !script_subtag(code) {
                return Err(LocaleError::InvalidTag);
            }

            let (first, rest) = code.split_at(1);

            Ok(format!("{}{}", first.to_ascii_uppercase(), rest.to_ascii_lowercase()))
        }

        // Steps 4.1 to 4.3. `type_value` is the `type` production of 6.4.3, and it allows the
        // underscore between pieces the way an input to be canonicalized may carry one; step 4.2's
        // backwards compatibility syntax is exactly those underscore forms of UTS 35 3.3.2, so the
        // underscore is refused here rather than lowercased into a key the data cannot have.
        DisplayNamesType::Calendar => {
            if !type_value(code) || code.contains('_') {
                return Err(LocaleError::InvalidTag);
            }

            Ok(code.to_ascii_lowercase())
        }

        // Steps 5.1 and 5.2: 12.5.2 is a lookup in Table 19, and the code is returned as it came.
        DisplayNamesType::DateTimeField => {
            if DATE_TIME_FIELD_CODES.contains(&code) {
                Ok(code.to_owned())
            } else {
                Err(LocaleError::InvalidTag)
            }
        }

        // Steps 6 to 8: 12.1.1 step 8 resolved `type` to one of the six names, so what is left of the
        // assert of step 6 is currency. IsWellFormedCurrencyCode (8.3.1) is three ASCII letters and
        // nothing else, and step 8 uppercases them.
        DisplayNamesType::Currency => {
            let bytes = code.as_bytes();

            if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_alphabetic) {
                return Err(LocaleError::InvalidTag);
            }

            Ok(code.to_ascii_uppercase())
        }
    }
}

/// Whether `tag` is matched by the `unicode_language_id` production of 6.2.1: a
/// `unicode_language_subtag`, then a script, then a region, then variants, and nothing else - no
/// Unicode extension sequence, no private-use subtag and no BCP 47 extension.
///
/// 12.5.1 step 1.1 asks for this and the tag canonicalization of 6.2.2 does not check it, because a
/// locale tag may of course carry the things this refuses: `Intl.getCanonicalLocales("en-u-ca-japan")`
/// answers with the tag, and `DisplayNames` of type "language" has to refuse the same string.
fn is_unicode_language_id(tag: &str) -> bool {
    let mut pieces = tag.split('-').peekable();

    // `unicode_language_subtag`, which the production requires and which is never "root" - 6.2.1's
    // own refusal, the one `Intl.Locale`'s `language` option also relies on.
    if !pieces.next().is_some_and(language_subtag) {
        return false;
    }

    // `(sep unicode_script_subtag)?`, then `(sep unicode_region_subtag)?`, then
    // `(sep unicode_variant_subtag)*`. The four predicates answer different shapes - two or three or
    // 5-8 letters, four letters, two letters or three digits, five to eight alphanumeric or a
    // leading-digit four - so at each position the next piece either is what the grammar allows
    // there or is not taken, and nothing has to be guessed between them.
    if pieces.peek().is_some_and(|piece| script_subtag(piece)) {
        pieces.next();
    }

    if pieces.peek().is_some_and(|piece| region_subtag(piece)) {
        pieces.next();
    }

    // A piece that is none of the four productions is the refusal: the "u", "ca" and "japan" of a
    // Unicode extension sequence, the "x" of a private-use subtag, or a second language subtag.
    pieces.all(variant_subtag)
}

extend_object! {
    /// Properties of Intl.DisplayNames Instances
    /// (https://tc39.es/ecma402/#sec-properties-of-intl-displaynames-instances), 12.4.
    ///
    /// Nothing here is an ICU4X object, which a GC object may not hold: the tables [[Fields]] is a
    /// view of are the cache's, and what an instance keeps is the [`DisplayNamesSettings`] that names
    /// them.
    pub struct DisplayNamesObject {
        /// What 12.1.1 steps 5 to 22 computed. Its absence is this engine's spelling of
        /// `[[InitializedDisplayNames]]`, which 12.3.2 step 2 and 12.3.3 step 2 each require, and
        /// which a constructor that threw leaves unset.
        resolved: Option<ResolvedDisplayNames>,
    }
}

/// The slots of 12.4 this build keeps, carried with the options so that one `Option` on the instance
/// says whether it is initialized at all.
#[derive(Clone, Copy)]
pub struct ResolvedDisplayNames {
    /// [[Locale]] (12.1.1 step 12), which 12.3.2 reports as `locale`.
    pub locale: Value,

    /// The data locale of 9.2.7 step 5 with the four options of 12.1.1 steps 7, 9, 11 and 19: the
    /// settings the names are read from and the four options 12.3.2 reports.
    pub settings: DisplayNamesSettings,
}

impl DisplayNamesObject {
    /// 12.1.1 steps 1 and 2: `OrdinaryCreateFromConstructor(newTarget,
    /// "%Intl.DisplayNames.prototype%", « [[InitializedDisplayNames]], [[Locale]], [[Style]],
    /// [[Type]], [[Fallback]], [[LanguageDisplay]], [[Fields]] »)`, every one of them empty. Like
    /// RelativeTimeFormat's, ListFormat's and PluralRules' and unlike NumberFormat's and Collator's,
    /// this constructor has no legacy call mode: 12.1.1 step 1 throws when NewTarget is undefined, so
    /// there is no %Intl%.[[FallbackSymbol]] instance to make and no ChainDisplayNames.
    pub fn new(cx: Context, new_target: Handle<ObjectValue>) -> EvalResult<HeapPtr<Self>> {
        // Step 2, through `ObjectBuilder`, which reads `newTarget.prototype` and falls back to the
        // intrinsic the spec names: OrdinaryCreateFromConstructor steps 1 and 2.
        let mut display_names = ObjectBuilder::<Self>::new(cx)
            .constructor_proto(new_target, Intrinsic::DisplayNamesPrototype)?
            .build()?;

        // The slots are empty as step 2 leaves them, and stay so until `initialize` writes the whole
        // record in one go.
        display_names.resolved = None;

        Ok(display_names)
    }

    /// `? RequireInternalSlot(displayNames, [[InitializedDisplayNames]])` of 12.3.2 step 2 and 12.3.3
    /// step 2, on the `this` value itself. There is no UnwrapDisplayNames in front of it: 12.1.1
    /// step 1 refuses the only call mode that would have put an instance behind a fallback symbol.
    pub fn require_initialized(cx: Context, this_value: Handle<Value>) -> EvalResult<Handle<Self>> {
        if !this_value.is_object() {
            return type_error(cx, "Intl.DisplayNames method called on incompatible receiver");
        }

        match this_value.as_object().as_opt::<Self>() {
            Some(instance) if instance.resolved.is_some() => Ok(instance),
            _ => type_error(cx, "Intl.DisplayNames method called on an uninitialized instance"),
        }
    }

    /// The resolved options of an initialized instance: 12.3.2's `resolvedOptions` reads them through
    /// Table 18, and 12.3.3's `of` works from them.
    pub fn resolved(&self) -> Option<ResolvedDisplayNames> {
        self.resolved
    }

    /// 12.1.1 (https://tc39.es/ecma402/#sec-Intl.DisplayNames) steps 3 to 23. The instance is a
    /// handle rather than `&mut self` on purpose, as in every service: every step below allocates, and
    /// the record is only written at the end, through a deref taken after the last of those
    /// allocations. A `&mut` held across them would point at the block the collection moved away from,
    /// and the caller would hand back the address it had before.
    pub fn initialize(
        mut instance: Handle<Self>,
        mut cx: Context,
        locales: Handle<Value>,
        options: Handle<Value>,
    ) -> EvalResult<()> {
        // Steps 3 to 5: ResolveOptions (https://tc39.es/ecma402/#sec-resolveoptions), 9.2.8 with the
        // `require-options` behaviour 12.1.1 step 3 names, which RelativeTimeFormat's call of it does
        // not: a number or a Boolean there is a TypeError rather than an empty bag. Its step 1 still
        // reads the locale list first, which is what makes a getter on `locales` run before one on the
        // options.
        let requested_locales = canonicalize_locale_list_argument(cx, locales)?;
        let options = get_options_object(cx, options)?;

        // 9.2.8 steps 2 to 6: the `localeMatcher` of 12.1.1's [[ResolutionOptionDescriptors]] is
        // nothing - 12.2.3 says that list and [[RelevantExtensionKeys]] are both empty - so the
        // descriptors of step 4 and the keys of step 6 are empty lists and 9.2.7 runs with no key to
        // look for at all. That is why a `calendar` or `collation` option on the bag is not read by
        // this constructor: it is not one of the six options of steps 6 to 19, and the extension keys
        // that would have made it mean something are the empty list.
        let matcher = get_name_option(
            cx,
            options,
            cx.names.locale_matcher(),
            &["best fit", "lookup"],
            "best fit",
        )?;

        // Steps 3 to 5 of 9.2.8 are ResolveLocale (9.2.7), whose step 14 leaves [[Locale]] the
        // canonicalized form of the built-in tag that survived and whose step 5 names the locale the
        // tables are read for.
        let resolved = resolve_locale(
            &requested_locales,
            if matcher == "lookup" {
                Matcher::Lookup
            } else {
                Matcher::BestFit
            },
        );

        // Step 12.
        let locale = cx.alloc_string(&resolved.locale)?.as_value();

        // Steps 13 to 15 reach [[LocaleData]], which is keyed by the built-in locale of 9.2.7 step 5
        // rather than by the tag step 14 built.
        let Ok(settings_locale) = DataLocale::try_from(resolved.data_locale.as_str()) else {
            return range_error(
                cx,
                &format!(
                    "Intl.DisplayNames resolved the locale '{}' but this build has no data for it",
                    resolved.data_locale
                ),
            );
        };

        // Steps 6 and 7: [[Style]].
        let style = get_enum_option(
            cx,
            options,
            cx.names.style(),
            &DisplayNamesStyle::NAMES,
            "long",
            DisplayNamesStyle::from_name,
        )?;

        // Steps 8 and 9: [[Type]], which 9.2.11 step 2.1 refuses to leave unset.
        let ty = DisplayNamesType::from_name(get_required_name_option(
            cx,
            options,
            cx.names.type_(),
            &DisplayNamesType::NAMES,
        )?)
        // 9.2.11 step 5 has already refused everything but the six names of the list above.
        .expect("the resolved type is one of the six names");

        // Steps 10 and 11: [[Fallback]].
        let fallback = get_enum_option(
            cx,
            options,
            cx.names.fallback(),
            &DisplayNamesFallback::NAMES,
            "code",
            DisplayNamesFallback::from_name,
        )?;

        // Steps 16 to 19: [[LanguageDisplay]], which step 19 writes only for type "language". The
        // option is read for every type because step 16 comes before step 19 and a getter on `type`
        // of the bag has therefore already run by then; for the other five the slot stays
        // `undefined`, which is what 12.3.2 step 4.3 leaves out of `resolvedOptions`.
        let language_display = get_enum_option(
            cx,
            options,
            cx.names.language_display(),
            &DisplayNamesLanguageDisplay::NAMES,
            "dialect",
            DisplayNamesLanguageDisplay::from_name,
        )?;

        let language_display = if ty == DisplayNamesType::Language {
            language_display
        } else {
            // The value of a slot 12.1.1 never sets, kept as the default the option would have had so
            // that the settings stay one `Copy` record; 12.3.2 below is what reports it as absent.
            DisplayNamesLanguageDisplay::Dialect
        };

        // Steps 17 to 22 select `typeFields` and `styleFields`, which are the tables of the locale,
        // the type and the style. They are reached when a code is asked for rather than built now:
        // 12.2.3's [[LocaleData]] is the cache's, and no part of selecting it can fail before a code
        // arrives - a type this build has no table for still constructs, because 12.1.1 refuses
        // nothing on that account and 12.3.3 steps 7 and 8 are the answer to every code of it.
        instance.resolved = Some(ResolvedDisplayNames {
            locale: *locale,
            settings: DisplayNamesSettings {
                locale: settings_locale,
                ty,
                style,
                fallback,
                language_display,
            },
        });

        Ok(())
    }
}

impl crate::runtime::gc::HeapItem for DisplayNamesObject {
    fn byte_size(display_names: HeapPtr<Self>) -> usize {
        display_names.object_byte_size()
    }

    fn visit_pointers(mut display_names: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        display_names.visit_object_pointers(visitor);

        if let Some(resolved) = &mut display_names.resolved {
            visitor.visit_value(&mut resolved.locale);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 12.5.1 over the six types, in Rust because the answer is a string and the error kind rather
    /// than a thrown exception: what the engine turns into a RangeError is one line of
    /// `display_names_prototype`, and what this checks is the eight steps that decide it.
    fn canonical(ty: DisplayNamesType, code: &str) -> Option<String> {
        canonical_code_for_display_names(ty, code).ok()
    }

    #[test]
    fn a_language_code_is_a_language_id_canonicalized() {
        // Step 1.3, which is the whole of 6.2.2: the script and region of a dialect tag are recased
        // and reordered, and a code that is only a language is lowercased.
        assert_eq!(canonical(DisplayNamesType::Language, "en-gb").as_deref(), Some("en-GB"));

        // The pieces of a `unicode_language_id` are in the production's order, and a tag that spells
        // them in another order is refused rather than rewritten: the canonicalization of 6.2.2
        // recases and deduplicates, but what it makes of `sr-rs-cyrl` is still a region in front of
        // the script it belongs behind, which is what step 1.1 asks about. `V8` throws here too.
        assert_eq!(canonical(DisplayNamesType::Language, "sr-rs-cyrl"), None);
        assert_eq!(
            canonical(DisplayNamesType::Language, "sr-Cyrl-RS").as_deref(),
            Some("sr-Cyrl-RS")
        );
        assert_eq!(canonical(DisplayNamesType::Language, "DK").as_deref(), Some("dk"));

        // Steps 1.1 and 1.2: what 6.2.1 refuses is refused, and what only `unicode_locale_id` matches
        // is refused by step 1.1 - an extension sequence, a transform and a private-use subtag are
        // not parts of a `unicode_language_id`.
        assert_eq!(canonical(DisplayNamesType::Language, "root"), None);
        assert_eq!(canonical(DisplayNamesType::Language, "dk_"), None);
        assert_eq!(canonical(DisplayNamesType::Language, "en-u-ca-gregory"), None);
        assert_eq!(canonical(DisplayNamesType::Language, "en-t-en"), None);
        assert_eq!(canonical(DisplayNamesType::Language, "qaa-x-a"), None);
    }

    #[test]
    fn a_region_code_is_upper_and_a_script_code_is_title_case() {
        // Steps 2.1 and 2.2.
        assert_eq!(canonical(DisplayNamesType::Region, "dk").as_deref(), Some("DK"));
        assert_eq!(canonical(DisplayNamesType::Region, "419").as_deref(), Some("419"));

        // Step 2.1 refuses what `unicode_region_subtag` refuses: three letters is a currency, two
        // digits is nothing, and the empty string is refused rather than uppercased into an answer.
        assert_eq!(canonical(DisplayNamesType::Region, "dka"), None);
        assert_eq!(canonical(DisplayNamesType::Region, "99"), None);
        assert_eq!(canonical(DisplayNamesType::Region, ""), None);

        // Steps 3.3 to 3.5, the one case rule of clause 12 that touches a single letter.
        assert_eq!(canonical(DisplayNamesType::Script, "latn").as_deref(), Some("Latn"));
        assert_eq!(canonical(DisplayNamesType::Script, "CYRL").as_deref(), Some("Cyrl"));

        // Step 3.1: neither five letters nor three is a `unicode_script_subtag`.
        assert_eq!(canonical(DisplayNamesType::Script, "cyrl1"), None);
        assert_eq!(canonical(DisplayNamesType::Script, "xyz"), None);
    }

    #[test]
    fn a_calendar_code_is_lower_and_its_backwards_forms_are_refused() {
        // Steps 4.2 and 4.3: the ASCII-lowercase of a `type` value.
        assert_eq!(canonical(DisplayNamesType::Calendar, "GREGORY").as_deref(), Some("gregory"));
        assert_eq!(canonical(DisplayNamesType::Calendar, "chinese").as_deref(), Some("chinese"));

        // Step 4.1: 3 to 8 alphanumeric characters, so two letters, nine letters and the empty
        // string all throw.
        assert_eq!(canonical(DisplayNamesType::Calendar, "ca"), None);
        assert_eq!(canonical(DisplayNamesType::Calendar, "gregoryxyz"), None);
        assert_eq!(canonical(DisplayNamesType::Calendar, ""), None);

        // A `type` value the calendar registry does not know is still a `type` value, which is all
        // step 4 asks. The name is the lookup's to refuse, not this operation's.
        assert_eq!(canonical(DisplayNamesType::Calendar, "gregoryx").as_deref(), Some("gregoryx"));

        // Step 4.2's backwards compatibility syntax, which UTS 35 3.3.2 spells with an underscore.
        assert_eq!(canonical(DisplayNamesType::Calendar, "islamic_civility"), None);
    }

    #[test]
    fn a_date_time_field_code_is_table_19_and_nothing_else() {
        // Steps 5.1 and 5.2 over 12.5.2's lookup: the twelve codes come back as Table 19 writes them,
        // and a code returned is the code asked for.
        for code in DATE_TIME_FIELD_CODES {
            assert_eq!(
                canonical(DisplayNamesType::DateTimeField, code).as_deref(),
                Some(code),
                "{code} is a code of Table 19"
            );
        }

        // The same words in the wrong case are not in the table, and neither is a word only close to
        // one of them.
        assert_eq!(canonical(DisplayNamesType::DateTimeField, "WEEKDAY"), None);
        assert_eq!(canonical(DisplayNamesType::DateTimeField, "WeekOfYear"), None);
        assert_eq!(canonical(DisplayNamesType::DateTimeField, "milliseconds"), None);
    }

    #[test]
    fn a_currency_code_is_three_letters_upper() {
        // Steps 7 and 8: 8.3.1's three ASCII letters and the ASCII-uppercase of them.
        assert_eq!(canonical(DisplayNamesType::Currency, "usd").as_deref(), Some("USD"));
        assert_eq!(canonical(DisplayNamesType::Currency, "DKK").as_deref(), Some("DKK"));

        // Step 7: a digit, two letters, four letters and the empty string are all refused.
        assert_eq!(canonical(DisplayNamesType::Currency, "us1"), None);
        assert_eq!(canonical(DisplayNamesType::Currency, "us"), None);
        assert_eq!(canonical(DisplayNamesType::Currency, "usdd"), None);
        assert_eq!(canonical(DisplayNamesType::Currency, ""), None);
    }

    #[test]
    fn the_language_id_production_leaves_the_other_codes_alone() {
        // The shape 12.5.1 step 1.1 asks for, checked on its own because the tag canonicalization
        // that comes with it accepts a great deal more than a language id does.
        assert!(is_unicode_language_id("da"));
        assert!(is_unicode_language_id("en-GB"));
        assert!(is_unicode_language_id("sr-Cyrl-RS"));
        assert!(is_unicode_language_id("de-DE-1996"));
        assert!(is_unicode_language_id("zh-guoyu"));

        // Two letters are a `unicode_language_subtag` as much as they are a `unicode_region_subtag`;
        // the production is about shapes, and which of the two a program meant is what `type` says.
        // `V8` answers the code of `of("GB")` of type "language" rather than throwing, which is the
        // same thing in the other direction.
        assert!(is_unicode_language_id("GB"));

        // A variant before a region is not the production's order, and the pieces of an extension or
        // of a private-use subtag are not parts of it.
        assert!(!is_unicode_language_id("en-1996-DE"));
        assert!(!is_unicode_language_id("en-u-ca-japan"));
        assert!(!is_unicode_language_id("en-x-priv"));
        assert!(!is_unicode_language_id(""));
    }
}
