//! Collator objects (https://tc39.es/ecma402/#sec-collator-objects) and the ICU4X collators that
//! do the comparing they stand for.
//!
//! 10.4 is what an instance has to remember, and none of it is an ICU4X object: a GC object has to
//! be trivially destructible, which `register_heap_items!` asserts, while `icu_collator::Collator`
//! owns buffers. So an instance holds its resolved options as `CollatorSettings`, and the collator
//! built from them lives in `cache::LocaleCache` under those same settings. Sorting 10,000 strings
//! with `compare` as the comparator therefore builds one collator, not thirty thousand.

use brimstone_intl_data::{DEFAULT_LOCALE, IntlDataProvider};
use icu_collator::{
    Collator, CollatorPreferences,
    options::{AlternateHandling, CaseLevel, CollatorOptions, MaxVariable, Strength},
    preferences::{CollationCaseFirst, CollationNumericOrdering},
};
use icu_locale::Locale;

use crate::extend_object;
use crate::runtime::{
    Context, EvalResult, Handle, HeapPtr, Value,
    error::type_error,
    gc::{HeapItem, HeapVisitor},
    intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue,
    ordinary_object::ObjectBuilder,
    string_value::StringValue,
};

/// [[Usage]] (https://tc39.es/ecma402/#sec-intl.collator-internals), 10.1.1 step 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    Sort,
    Search,
}

impl Usage {
    /// The String value 10.3.2 reports for it, and the option value 9.2.11 accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sort => "sort",
            Self::Search => "search",
        }
    }

    /// 10.1.1 step 7. GetOption has already refused everything but the two names of this enum
    /// (9.2.11 step 5), so anything that is not "search" is the default that step also names.
    pub fn parse(text: &str) -> Self {
        if text == Self::Search.as_str() {
            Self::Search
        } else {
            Self::Sort
        }
    }
}

/// [[Sensitivity]], which level of the collation element sequence decides a comparison (Table 4 of
/// 10.3.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sensitivity {
    Base,
    Accent,
    Case,
    Variant,
}

impl Sensitivity {
    /// The String value 10.3.2 reports for it, and the option value 9.2.11 accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Accent => "accent",
            Self::Case => "case",
            Self::Variant => "variant",
        }
    }

    /// 10.1.1 step 20, whose list of allowed values is exactly the four names of this enum and
    /// whose default is "variant" for "sort".
    pub fn parse(text: &str) -> Self {
        match text {
            "base" => Self::Base,
            "accent" => Self::Accent,
            "case" => Self::Case,
            _ => Self::Variant,
        }
    }
}

/// [[CaseFirst]], the "kf" keyword: whether case decides anything, and which case comes first when
/// it does (10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaseFirst {
    False,
    Upper,
    Lower,
}

impl CaseFirst {
    /// The String value 10.3.2 reports for it, the CLDR keyword value it came from, and the option
    /// value 9.2.11 accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::False => "false",
            Self::Upper => "upper",
            Self::Lower => "lower",
        }
    }

    /// 10.1.1 step 17 and the `caseFirst` option of 10.2.3, which allow the same three values.
    /// "false" is what ResolveLocale step 12.3 leaves when nothing asked otherwise.
    pub fn parse(text: &str) -> Self {
        match text {
            "upper" => Self::Upper,
            "lower" => Self::Lower,
            _ => Self::False,
        }
    }
}

/// The resolved options that decide how two strings compare, in the shape a cache key needs: every
/// field is `Copy`, so a collator can be found again without holding an ICU4X object or borrowing
/// anything from a JS value.
///
/// [[Collation]] is not one of them. 10.2.3 makes the spec's `null` the only value a locale's "co"
/// data may start with, and `icu/generate_intl_data.sh` bakes no collation type at all, so every
/// collator of this build compares with the root tailoring of its locale and 10.1.1 step 14 always
/// reports "default".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CollatorSettings {
    /// The locale ResolveLocale step 5 found: the one whose data compares, and so [[Locale]] minus
    /// its `-u-` keywords. Those keywords are the other fields of this struct in their other
    /// spelling, so leaving them out cannot lose a distinction.
    pub locale: &'static str,

    /// [[Numeric]], the "kn" keyword.
    pub numeric: bool,

    /// [[CaseFirst]], the "kf" keyword.
    pub case_first: CaseFirst,

    /// [[Sensitivity]].
    pub sensitivity: Sensitivity,

    /// [[IgnorePunctuation]].
    pub ignore_punctuation: bool,
}

impl Default for CollatorSettings {
    /// The options 10.1.1 resolves when the caller passes nothing at all: the default locale with
    /// variant sensitivity and every keyword left at what that locale itself says.
    fn default() -> Self {
        Self {
            locale: DEFAULT_LOCALE,
            numeric: false,
            case_first: CaseFirst::False,
            sensitivity: Sensitivity::Variant,
            ignore_punctuation: false,
        }
    }
}

impl CollatorSettings {
    /// The ICU4X collator these options describe.
    ///
    /// The only way this fails is missing data, and the data is compiled into this binary:
    /// `brimstone_intl_data`'s own tests build a collator in every built in locale, which is what
    /// lets the one cache entry per distinct settings unwrap here instead of passing an error up.
    pub fn build_collator(&self) -> Collator {
        // Only the locales of `brimstone_intl_data::available_locales` that a request can name ever
        // reach this, and the word "root" is not one of them: it names the base data rather than a
        // locale, so CanonicalizeLocaleList (9.2.1 step 4.c) throws on it, which is how V8 and
        // SpiderMonkey treat it too and what keeps ICU4X, which has no such language subtag, from
        // being asked to parse it here.
        let locale = self
            .locale
            .parse::<Locale>()
            .expect("a built in locale tag parses");

        let mut preferences = CollatorPreferences::from(&locale);

        // The tailoring to load is the locale's own, since there is no collation type to ask for:
        // "co" resolves to the spec's `null` in this build (10.2.3).
        preferences.collation_type = None;

        // `caseFirst: "false"` is not a setting of its own but the locale's default, which TR35
        // spells with no keyword at all and ICU4X with `None`: Danish and Maltese take upper case
        // first and every other locale lower case. `Some(False)` would override that and disagree
        // with ICU4C, and so with V8 and SpiderMonkey, about `["A", "a"].sort()` in "da".
        preferences.case_first = match self.case_first {
            CaseFirst::Upper => Some(CollationCaseFirst::Upper),
            CaseFirst::Lower => Some(CollationCaseFirst::Lower),
            CaseFirst::False => None,
        };

        preferences.numeric_ordering = Some(if self.numeric {
            CollationNumericOrdering::True
        } else {
            CollationNumericOrdering::False
        });

        let mut options = CollatorOptions::default();

        // Table 4 of 10.3.3.1, level by level: "base" stops after the base letters, "accent" adds
        // diacritics and stops there, "case" adds case and nothing else, and "variant" compares
        // every level there is.
        options.strength = Some(match self.sensitivity {
            Sensitivity::Base | Sensitivity::Case => Strength::Primary,
            Sensitivity::Accent => Strength::Secondary,
            Sensitivity::Variant => Strength::Tertiary,
        });

        // The one level that "case" wants and primary strength would leave out.
        options.case_level = Some(if matches!(self.sensitivity, Sensitivity::Case) {
            CaseLevel::On
        } else {
            CaseLevel::Off
        });

        if self.ignore_punctuation {
            // 10.3.3.1: "If collator.[[IgnorePunctuation]] is true, then punctuation is ignored
            // (e.g., strings that differ only in punctuation compare as equal)". TR35 spells that
            // as `ka=shifted` with the variable top at `vt=punctuation`, which is the
            // `[:punctuation:]` set and so leaves the `[:symbol:]` characters weighted.
            options.alternate_handling = Some(AlternateHandling::Shifted);
            options.max_variable = Some(MaxVariable::Punctuation);
        }

        Collator::try_new_unstable(&IntlDataProvider, preferences, options).unwrap()
    }
}

extend_object! {
    /// Collator Objects (https://tc39.es/ecma402/#sec-collator-objects), 10.4.
    pub struct CollatorObject {
        /// [[InitializedCollator]] together with the slots 10.1.1 computes with it: `None` until
        /// step 22 has resolved the arguments, which is also the check that the
        /// `RequireInternalSlot` of every method of 10.3 makes.
        settings: Option<CollatorSettings>,

        /// [[Usage]]. It sits outside `settings` because it does not change the comparison this
        /// build performs, and so must not split the collator cache in two.
        usage: Usage,

        /// [[Collation]], the String that 10.1.1 step 14 made out of the "co" keyword.
        collation: Value,

        /// [[Locale]], the language tag of ResolveLocale step 14 as the String of 10.1.1 step 13.
        locale: Value,

        /// [[BoundCompare]], the function that the `compare` accessor caches (10.3.3 step 3).
        bound_compare: Value,
    }
}

impl CollatorObject {
    /// The object that 10.1.1 step 3 creates, before any of its arguments has been read.
    pub fn new(
        cx: Context,
        constructor: Handle<ObjectValue>,
    ) -> EvalResult<Handle<CollatorObject>> {
        let mut object = ObjectBuilder::<CollatorObject>::new(cx)
            .constructor_proto(constructor, Intrinsic::CollatorPrototype)?
            .build()?;

        // Every field has to be written: `build` allocates the object uninitialized.
        object.settings = None;
        object.usage = Usage::Sort;
        object.collation = Value::undefined();
        object.locale = Value::undefined();
        object.bound_compare = Value::undefined();

        Ok(object.to_handle())
    }

    /// The internal slots that 10.1.1 steps 8 and 13-22 compute. [[Numeric]], [[CaseFirst]],
    /// [[Sensitivity]] and [[IgnorePunctuation]] are the fields of `settings`, because that is the
    /// shape the collator cache wants them in.
    pub fn initialize(
        &mut self,
        settings: CollatorSettings,
        usage: Usage,
        collation: Handle<Value>,
        locale: Handle<Value>,
    ) {
        self.settings = Some(settings);
        self.usage = usage;
        self.collation = *collation;
        self.locale = *locale;
    }

    pub fn settings(&self) -> Option<CollatorSettings> {
        self.settings
    }

    pub fn usage(&self) -> Usage {
        self.usage
    }

    pub fn collation(&self) -> Value {
        self.collation
    }

    pub fn locale(&self) -> Value {
        self.locale
    }

    pub fn bound_compare(&self) -> Value {
        self.bound_compare
    }

    pub fn set_bound_compare(&mut self, bound_compare: Handle<Value>) {
        self.bound_compare = *bound_compare;
    }
}

impl HeapItem for CollatorObject {
    fn byte_size(collator_object: HeapPtr<Self>) -> usize {
        collator_object.object_byte_size()
    }

    fn visit_pointers(mut collator_object: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        collator_object.visit_object_pointers(visitor);
        visitor.visit_value(&mut collator_object.collation);
        visitor.visit_value(&mut collator_object.locale);
        visitor.visit_value(&mut collator_object.bound_compare);
    }
}

/// RequireInternalSlot(collator, [[InitializedCollator]]), the check that both methods of 10.3
/// begin with (10.3.2 step 2 and 10.3.3 step 2). This object spells that slot as its `settings`
/// being present, so a value that is not a collator instance, or is one that 10.1.1 never finished
/// initializing, is refused here rather than reaching an option that was never resolved.
///
/// The message names no method because both say the same thing in the engines that have checked it:
/// test262's `Collator/prototype/this-value-not-collator.js` only asks for a TypeError.
pub fn this_collator(cx: Context, value: Handle<Value>) -> EvalResult<Handle<CollatorObject>> {
    let collator = if value.is_object() {
        value.as_opt::<CollatorObject>()
    } else {
        None
    };

    match collator {
        Some(collator) if collator.settings().is_some() => Ok(collator),
        _ => type_error(cx, "Intl.Collator method requires an initialized collator"),
    }
}

/// CompareStrings (https://tc39.es/ecma402/#sec-comparestrings), 10.3.3.1, of two strings that the
/// caller has already turned into Strings.
///
/// The strings are compared as the bytes of their WTF-8 encoding, which is what the engine's other
/// ICU4X use does. A lone surrogate is not a code point and ICU4X gives it the weights of the
/// replacement character, which is the only sensible answer for a sequence ECMA-402 says nothing
/// about.
pub fn compare_strings(
    cx: Context,
    settings: CollatorSettings,
    left: Handle<StringValue>,
    right: Handle<StringValue>,
) -> EvalResult<Handle<Value>> {
    let collator = cx.locale_cache().collator(settings);

    let left_bytes = left.to_wtf8_string()?;
    let right_bytes = right.to_wtf8_string()?;

    let ordering = collator
        .as_borrowed()
        .compare_utf8(left_bytes.as_bytes(), right_bytes.as_bytes());

    // 10.3.3.1 fixes only the sign. -1, 0 and 1 are what every engine returns, and `Ordering` is
    // exactly those three values in memory.
    Ok(cx.smi(ordering as i8))
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::{CaseFirst, CollatorSettings, Sensitivity};

    /// What CompareStrings says about two strings under `settings`.
    fn ordering(settings: CollatorSettings, left: &str, right: &str) -> Ordering {
        settings
            .build_collator()
            .as_borrowed()
            .compare_utf8(left.as_bytes(), right.as_bytes())
    }

    /// The settings of 10.1.1 with no arguments, with only the options named by `change` on top.
    fn resolved(change: impl FnOnce(&mut CollatorSettings)) -> CollatorSettings {
        let mut settings = CollatorSettings::default();
        change(&mut settings);
        settings
    }

    #[test]
    fn the_default_settings_sort_the_way_english_reads() {
        let settings = CollatorSettings::default();

        assert_eq!(ordering(settings, "a", "b"), Ordering::Less);
        assert_eq!(ordering(settings, "apple", "banana"), Ordering::Less);
        assert_eq!(ordering(settings, "", "a"), Ordering::Less);
        assert_eq!(ordering(settings, "a1", "a10"), Ordering::Less);
        assert_eq!(ordering(settings, "b", "b"), Ordering::Equal);

        // The letters come first, so a capital does not lift "Zebra" above "apple".
        assert_eq!(ordering(settings, "Zebra", "apple"), Ordering::Greater);
    }

    #[test]
    fn sensitivity_base_ignores_both_case_and_accents() {
        // Table 4 of 10.3.3.1: "a" vs "á" and "a" vs "A" are both equal.
        let settings = resolved(|s| s.sensitivity = Sensitivity::Base);

        assert_eq!(ordering(settings, "a", "á"), Ordering::Equal);
        assert_eq!(ordering(settings, "a", "A"), Ordering::Equal);
        assert_eq!(ordering(settings, "á", "A"), Ordering::Equal);

        // Only the base letters are left to disagree about.
        assert_eq!(ordering(settings, "a", "b"), Ordering::Less);
    }

    #[test]
    fn sensitivity_accent_ignores_case_but_not_accents() {
        let settings = resolved(|s| s.sensitivity = Sensitivity::Accent);

        assert_eq!(ordering(settings, "a", "A"), Ordering::Equal);
        assert_ne!(ordering(settings, "a", "á"), Ordering::Equal);
        assert_eq!(ordering(settings, "a", "b"), Ordering::Less);
    }

    #[test]
    fn sensitivity_case_ignores_accents_but_not_case() {
        let settings = resolved(|s| s.sensitivity = Sensitivity::Case);

        assert_eq!(ordering(settings, "a", "á"), Ordering::Equal);
        assert_ne!(ordering(settings, "a", "A"), Ordering::Equal);
        assert_eq!(ordering(settings, "a", "b"), Ordering::Less);
    }

    #[test]
    fn sensitivity_variant_tells_case_and_accents_apart() {
        let settings = resolved(|s| s.sensitivity = Sensitivity::Variant);

        assert_ne!(ordering(settings, "a", "A"), Ordering::Equal);
        assert_ne!(ordering(settings, "a", "á"), Ordering::Equal);

        // It is also what 10.1.1 step 19 defaults to for "sort", so the settings above and the
        // settings a bare `new Intl.Collator()` resolves to compare the same.
        assert_eq!(settings, CollatorSettings::default());
    }

    #[test]
    fn numeric_sorts_a_run_of_digits_by_its_value() {
        let numeric = resolved(|s| s.numeric = true);

        assert_eq!(ordering(numeric, "a2", "a10"), Ordering::Less);
        assert_eq!(ordering(numeric, "item-9", "item-10"), Ordering::Less);

        // Without "kn" the digits are characters, which gives the other order.
        assert_eq!(ordering(CollatorSettings::default(), "a2", "a10"), Ordering::Greater);
    }

    #[test]
    fn case_first_picks_which_case_comes_first() {
        let upper = resolved(|s| s.case_first = CaseFirst::Upper);
        let lower = resolved(|s| s.case_first = CaseFirst::Lower);

        assert_eq!(ordering(upper, "A", "a"), Ordering::Less);
        assert_eq!(ordering(lower, "A", "a"), Ordering::Greater);

        // The letters themselves are unaffected, in both spellings.
        assert_eq!(ordering(upper, "a", "b"), Ordering::Less);
        assert_eq!(ordering(lower, "a", "b"), Ordering::Less);
    }

    #[test]
    fn a_false_case_first_leaves_the_locale_the_boss() {
        // TR35 makes Danish and Maltese the two locales whose own answer is "upper", so
        // `caseFirst: "false"` has to mean the locale default rather than a forced "lower" -
        // which is what ICU4C and V8 report for `["A", "a"].sort()` in "da".
        let danish = resolved(|s| {
            s.locale = "da";
            s.case_first = CaseFirst::False;
        });
        let english = resolved(|s| s.case_first = CaseFirst::False);

        assert_eq!(ordering(danish, "A", "a"), Ordering::Less, "Danish is upper first");
        assert_eq!(ordering(english, "A", "a"), Ordering::Greater, "English is lower first");
    }

    #[test]
    fn ignoring_punctuation_drops_it_from_the_comparison() {
        let settings = resolved(|s| s.ignore_punctuation = true);

        assert_eq!(ordering(settings, "a.b", "ab"), Ordering::Equal);
        assert_eq!(ordering(settings, "a b", "ab"), Ordering::Equal);
        assert_eq!(ordering(settings, "a-b", "a.b"), Ordering::Equal);

        // What is left when the punctuation goes still sorts.
        assert_eq!(ordering(settings, "a.b", "ac"), Ordering::Less);

        // The default weighs it in, and punctuation comes before letters.
        assert_eq!(ordering(CollatorSettings::default(), "a.b", "ab"), Ordering::Less);
    }

    #[test]
    fn canonically_equivalent_strings_are_equal() {
        // 10.3.3.1: CompareStrings "is required to recognize and honour canonical equivalence",
        // which is what the NFD tables of `icu/intl_markers.txt` are for. The two spellings of Å
        // are distinguishable strings, so the answer is +0 rather than a tie broken at any level.
        let settings = CollatorSettings::default();

        assert_eq!(ordering(settings, "Å", "Å"), Ordering::Equal);
        assert_eq!(ordering(settings, "ä", "ä"), Ordering::Equal);
    }

    #[test]
    fn danish_moves_three_letters_past_z_and_italian_moves_none() {
        let danish = resolved(|s| s.locale = "da");
        let english = CollatorSettings::default();

        // The Danish alphabet ends a ... x, y, z, æ, ø, å.
        for pair in [("z", "æ"), ("æ", "ø"), ("ø", "å")] {
            assert_eq!(
                ordering(danish, pair.0, pair.1),
                Ordering::Less,
                "Danish sorts {} before {}",
                pair.0,
                pair.1
            );
        }

        // Root order expands æ to "ae", which is why English leaves it at the front instead.
        assert_eq!(ordering(english, "z", "æ"), Ordering::Greater);

        // CLDR's Danish tailoring also expands the digraph "aa" to the å it used to spell - names
        // still written Aa today - so it leaves the a it starts with and joins the å behind the z,
        // above the "ab" that root order would have put first. Italian has no tailoring at all.
        assert_eq!(ordering(danish, "ab", "å"), Ordering::Less);
        assert_eq!(ordering(danish, "aa", "ab"), Ordering::Greater);
        assert_eq!(ordering(danish, "aa", "å"), Ordering::Greater);
        assert_eq!(ordering(english, "aa", "ab"), Ordering::Less);
        assert_eq!(ordering(resolved(|s| s.locale = "it"), "aa", "ab"), Ordering::Less);
    }
}
