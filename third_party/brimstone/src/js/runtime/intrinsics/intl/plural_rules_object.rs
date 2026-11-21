//! The `Intl.PluralRules` instance (https://tc39.es/ecma402/#sec-pluralrules-objects), 17.4, and
//! the two operations that work from it: ResolvePlural (17.5.2) and ResolvePluralRange (17.5.4).
//!
//! An instance holds what 17.4 lists and nothing else: the resolved locale, the plural `type`, and
//! the digit options 17.1.1 step 13 reads through SetNumberFormatDigitOptions - which is
//! NumberFormat's 16.1.2, so the same [`NumberFormatSlots`] holds them here and the same function
//! fills them, as Boa shares them too. The category itself comes from ICU4X, whose rules object
//! owns buffers and therefore cannot sit on a GC object: it lives in
//! [`super::cache::LocaleCache`] under the [`PluralRulesSettings`] an instance keeps instead.

use std::rc::Rc;

use brimstone_intl_data::IntlDataProvider;
use icu_plurals::{
    PluralCategory, PluralOperands, PluralRules, PluralRulesOptions, PluralRulesPreferences,
    PluralRulesWithRanges,
};

use super::cache::DataLocale;
use super::intl_object::canonicalize_locale_list_argument;
use super::locale_negotiation::{Matcher, resolve_locale};
use super::number_format_core::{
    IntlMathValue, Notation, NumberFormatSlots, RoundedNumber, round_number,
};
use super::number_format_object::set_number_format_digit_options;
use super::options::{coerce_options_to_object, get_enum_option, get_name_option};
use crate::extend_object;
use crate::runtime::{
    Context, EvalResult, Handle, Value,
    error::{range_error, type_error},
    gc::{HeapItem, HeapPtr, HeapVisitor},
    intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue,
    ordinary_object::ObjectBuilder,
};

/// [[Type]] (https://tc39.es/ecma402/#sec-intl-pluralrules-constructor), 17.1.1 step 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluralRuleType {
    Cardinal,
    Ordinal,
}

impl PluralRuleType {
    /// The two names 17.1.1 step 7 lists, in that order.
    pub const NAMES: [&'static str; 2] = ["cardinal", "ordinal"];

    /// The String value 17.3.2 reports for it, and the option value 17.1.1 step 7 accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cardinal => "cardinal",
            Self::Ordinal => "ordinal",
        }
    }

    /// 17.1.1 step 7 has already refused everything but the two names of this enum, so the only
    /// question this answers is which of them was said.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "cardinal" => Some(Self::Cardinal),
            "ordinal" => Some(Self::Ordinal),
            _ => None,
        }
    }
}

/// The String value of one of the six categories 17.5.1 answers with, which is what 17.3.3 returns
/// and 17.3.2 puts into the `pluralCategories` array.
///
/// The order is the order of the `case` arms below, which is the order 17.3.2 step 4 sorts
/// `pluralCategories` by and so the order UTS 35 fixes - and the order ICU4X's own iteration
/// follows, which is what lets [`PluralRulesObject::plural_categories`] report what it gives back
/// without sorting it again.
fn category_name(category: PluralCategory) -> &'static str {
    match category {
        PluralCategory::Zero => "zero",
        PluralCategory::One => "one",
        PluralCategory::Two => "two",
        PluralCategory::Few => "few",
        PluralCategory::Many => "many",
        PluralCategory::Other => "other",
    }
}

/// The instance's resolved locale and `type` as the `Copy` key of the cache entry that holds its
/// ICU4X rules.
///
/// The data locale rather than the resolved tag, because a `-u-nu-latn` a request can leave on that
/// tag names no locale the plural tables have an entry for - the same reason
/// [`super::cache::LocaleCache`] keys its decimal renderers by the data locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PluralRulesSettings {
    pub locale: DataLocale,
    pub kind: PluralRuleType,
}

impl PluralRulesSettings {
    /// The ICU4X rules these options describe.
    ///
    /// The only way this fails is missing data, and the data is compiled into this binary:
    /// `brimstone_intl_data`'s own tests build cardinal and ordinal rules in every built-in locale,
    /// which is what lets the one cache entry per distinct settings unwrap here rather than pass an
    /// error up.
    pub fn build_plural_rules(&self) -> PluralRulesWithRanges<PluralRules> {
        let locale = self
            .locale
            .tag()
            .parse::<icu_locale::Locale>()
            .expect("a built in locale tag parses");

        let kind = match self.kind {
            PluralRuleType::Cardinal => icu_plurals::PluralRuleType::Cardinal,
            PluralRuleType::Ordinal => icu_plurals::PluralRuleType::Ordinal,
        };

        PluralRulesWithRanges::try_new_unstable(
            &IntlDataProvider,
            PluralRulesPreferences::from(&locale),
            PluralRulesOptions::from(kind),
        )
        .unwrap()
    }
}

extend_object! {
    /// Properties of Intl.PluralRules Instances
    /// (https://tc39.es/ecma402/#sec-properties-of-intl-pluralrules-instances), 17.4.
    ///
    /// Nothing here is an ICU4X object, which a GC object may not hold: the rules are the cache's,
    /// and what an instance keeps is the key they are cached under.
    pub struct PluralRulesObject {
        /// What 17.1.1 steps 6 to 13 computed. Its absence is this engine's spelling of
        /// `[[InitializedPluralRules]]`, which 17.3.2 step 2, 17.3.3 step 2 and 17.3.4 step 2 each
        /// require, and which a constructor that threw leaves unset.
        resolved: Option<ResolvedPluralRules>,
    }
}

/// The slots 17.1.1 resolves, carried with the options so that one `Option` on the instance says
/// whether it is initialized at all.
#[derive(Clone, Copy)]
pub struct ResolvedPluralRules {
    /// [[Locale]] (17.1.1 step 6), which 17.3.2 reports as `locale`.
    pub locale: Value,
    /// [[LocaleData]] (9.1, the slot 17.2.3 gives the constructor too) and [[Type]] (17.1.1 step 8),
    /// which together are what the rules are cached under.
    pub settings: PluralRulesSettings,
    /// The digit slots of 17.4, which 17.1.1 step 13 filled through NumberFormat's 16.1.2.
    ///
    /// [[Notation]] and [[CompactDisplay]] are not in here: 17.1.1 step 10 allows one notation in
    /// this build and step 12 leaves the display undefined for it, so 17.3.2 reports the constant
    /// and reports nothing for the display.
    pub slots: NumberFormatSlots,
}

impl PluralRulesObject {
    /// 17.1.1 steps 1 and 2: `OrdinaryCreateFromConstructor(newTarget,
    /// "%Intl.PluralRules.prototype%", ‹ the slots of 17.4 ›)`, every one of them empty. Unlike
    /// NumberFormat's and Collator's, this constructor has no legacy call mode: 17.1.1 step 1 throws
    /// when NewTarget is undefined, so `Reflect.construct` and a subclass's `Symbol.construct` are
    /// the only ways an instance is made without `new`.
    pub fn new(cx: Context, new_target: Handle<ObjectValue>) -> EvalResult<HeapPtr<Self>> {
        // Step 2, through `ObjectBuilder`, which reads `newTarget.prototype` and falls back to the
        // intrinsic the spec names: OrdinaryCreateFromConstructor steps 1 and 2.
        let mut plural_rules = ObjectBuilder::<Self>::new(cx)
            .constructor_proto(new_target, Intrinsic::PluralRulesPrototype)?
            .build()?;

        // The slots are empty as step 2 leaves them, and stay so until `initialize` writes the whole
        // record in one go.
        plural_rules.resolved = None;

        Ok(plural_rules)
    }

    /// `? RequireInternalSlot(pr, [[InitializedPluralRules]])` (17.3.2 step 2, 17.3.3 step 2,
    /// 17.3.4 step 2), on the `this` value itself. PluralRules has no %Intl%.[[FallbackSymbol]]
    /// instance to look through, because 17.1.1 step 1 refuses the call mode that would have left
    /// one.
    pub fn require_initialized(cx: Context, this_value: Handle<Value>) -> EvalResult<Handle<Self>> {
        if !this_value.is_object() {
            return type_error(cx, "Intl.PluralRules method called on incompatible receiver");
        }

        match this_value.as_object().as_opt::<Self>() {
            Some(instance) if instance.resolved.is_some() => Ok(instance),
            _ => type_error(cx, "Intl.PluralRules method called on an uninitialized instance"),
        }
    }

    /// The resolved options of an initialized instance: 17.3.2's `resolvedOptions` reads them row by
    /// row through Table 32, and the two resolve operations below work from them.
    pub fn resolved(&self) -> Option<ResolvedPluralRules> {
        self.resolved
    }

    /// 17.1.1 (https://tc39.es/ecma402/#sec-intl-pluralrules-constructor) steps 3 to 13.
    /// The instance is a handle rather than `&mut self` on purpose: every step below allocates, and
    /// the block is only written at the end, through a deref taken after the last of those
    /// allocations. A `&mut` held across them would point at the block the collection moved away
    /// from, and the caller would hand back the address it had before.
    pub fn initialize(
        mut instance: Handle<Self>,
        mut cx: Context,
        locales: Handle<Value>,
        options: Handle<Value>,
    ) -> EvalResult<()> {
        // Steps 3 to 5: ResolveOptions (https://tc39.es/ecma402/#sec-resolveoptions), 9.2.8, with
        // `coerce-options`. Its step 1 reads the locale list and its step 3 the options bag, in that
        // order, which is what makes a getter on the `locales` argument run before one on the
        // options.
        let requested_locales = canonicalize_locale_list_argument(cx, locales)?;
        let options = coerce_options_to_object(cx, options)?;

        // 9.2.8 step 4.
        let matcher = get_name_option(
            cx,
            options,
            cx.names.locale_matcher(),
            &["best fit", "lookup"],
            "best fit",
        )?;

        // Steps 5 and 6 through ResolveLocale (https://tc39.es/ecma402/#sec-resolvelocale), 9.2.7,
        // over 17.2.3's [[RelevantExtensionKeys]], which is empty: Note 1 of 17.2.3 says TR35 names
        // no keyword the pluralization process depends on, so no `-u-` of a requested tag is kept
        // and no option value is looked up against a list of supported ones.
        let resolved = resolve_locale(
            &requested_locales,
            if matcher == "lookup" {
                Matcher::Lookup
            } else {
                Matcher::BestFit
            },
        );

        // Step 6.
        let locale = cx.alloc_string(&resolved.locale)?.as_value();

        // The rules are keyed by the built-in locale the resolved tag names, and 17.2.3's
        // [[AvailableLocales]] is exactly the set this build was generated with, so a tag outside it
        // means data is missing rather than that the caller can be answered in another locale.
        let Ok(data_locale) = DataLocale::try_from(resolved.data_locale.as_str()) else {
            return range_error(
                cx,
                &format!(
                    "Intl.PluralRules resolved the locale '{}' but this build has no data for it",
                    resolved.data_locale
                ),
            );
        };

        // Steps 7 and 8.
        let kind = get_enum_option(
            cx,
            options,
            cx.names.type_(),
            &PluralRuleType::NAMES,
            "cardinal",
            PluralRuleType::from_name,
        )?;

        // Steps 9 to 12: the two reads are done in the order the spec gives them, so a getter on the
        // options bag sees `notation` before `compactDisplay` here exactly as it would in a build
        // that can pluralize with compact notation. Step 12 stores the display only for "compact",
        // and the refusal below means this build never reaches that store: [[CompactDisplay]] stays
        // empty, which is how 17.3.2 leaves its row of Table 32 off.
        let notation = get_name_option(
            cx,
            options,
            cx.names.notation(),
            &["standard", "scientific", "engineering", "compact"],
            "standard",
        )?;

        get_name_option(cx, options, cx.names.compact_display(), &["short", "long"], "short")?;

        // Of the four notations step 9 lists, "standard" is the only one this build pluralizes with:
        // the other three select from a compact or an exponent pattern, and the baked data has no
        // entry for those. Saying so is what keeps 17.3.2 from reporting a notation the categories
        // were not computed from, and it comes before step 13 because that step reads the fraction
        // defaults off the notation it is handed.
        if notation != "standard" {
            return range_error(
                cx,
                &format!("PluralRules notation '{notation}' is not supported in this build"),
            );
        }

        // Step 13: SetNumberFormatDigitOptions (https://tc39.es/ecma402/#sec-setnfdigitoptions),
        // 16.1.2, with the defaults 17.1.1 hands it, 0 and 3, and with the notation above - which past
        // the refusal is only ever `standard`, so no fraction default is read off a compact scale.
        let mut slots = NumberFormatSlots::default();
        set_number_format_digit_options(cx, options, &mut slots, 0.0, 3.0, Notation::Standard)?;

        instance.resolved = Some(ResolvedPluralRules {
            locale: *locale,
            settings: PluralRulesSettings { locale: data_locale, kind },
            slots,
        });

        Ok(())
    }

    /// ResolvePlural (https://tc39.es/ecma402/#sec-resolveplural), 17.5.2, for one value: the
    /// category half of the record it returns.
    ///
    /// The formatted string of steps 1.1, 2.1, 3.1 and 5 is only ever read by 17.5.4 step 4, and
    /// [`Self::resolve_plural_range`] asks that question of the rounded value itself, so what is
    /// returned here is the category and nothing else.
    pub fn resolve_plural(&self, cx: Context, n: &IntlMathValue) -> EvalResult<&'static str> {
        let Some(resolved) = self.resolved() else {
            return type_error(cx, "Intl.PluralRules method called on an uninitialized instance");
        };

        // Steps 1 to 10, which are [`plural_rule_select`] over the digits 16.5.2 arrived at.
        Ok(plural_rule_select(&rules(cx, &resolved), &round_number(&resolved.slots, n)))
    }

    /// ResolvePluralRange (https://tc39.es/ecma402/#sec-resolvepluralrange), 17.5.4.
    pub fn resolve_plural_range(
        &self,
        cx: Context,
        x: &IntlMathValue,
        y: &IntlMathValue,
    ) -> EvalResult<&'static str> {
        let Some(resolved) = self.resolved() else {
            return type_error(cx, "Intl.PluralRules method called on an uninitialized instance");
        };

        // Step 1.
        if matches!(x, IntlMathValue::NaN) || matches!(y, IntlMathValue::NaN) {
            return range_error(cx, "Intl.PluralRules.selectRange does not take a NaN endpoint");
        }

        // Steps 2 and 3: ResolvePlural for each end, of which this build needs the digits and the
        // category, both of which [`RoundedNumber`] carries.
        let rules = rules(cx, &resolved);

        Ok(range_category(
            &rules,
            &round_number(&resolved.slots, x),
            &round_number(&resolved.slots, y),
        ))
    }

    /// 17.3.2 step 4: every category the locale's rules can answer with, in the order that step
    /// sorts them. ICU4X iterates its own list in that order already, and it always ends with
    /// "other".
    pub fn plural_categories(&self, cx: Context) -> EvalResult<Vec<&'static str>> {
        let Some(resolved) = self.resolved() else {
            return type_error(cx, "Intl.PluralRules method called on an uninitialized instance");
        };

        Ok(rules(cx, &resolved)
            .rules()
            .categories()
            .map(category_name)
            .collect())
    }
}

impl HeapItem for PluralRulesObject {
    fn byte_size(plural_rules: HeapPtr<Self>) -> usize {
        plural_rules.object_byte_size()
    }

    fn visit_pointers(mut plural_rules: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        plural_rules.visit_object_pointers(visitor);

        if let Some(resolved) = &mut plural_rules.resolved {
            visitor.visit_value(&mut resolved.locale);
        }
    }
}

/// The rules `resolved` pluralizes with, from the per-context cache of them.
fn rules(cx: Context, resolved: &ResolvedPluralRules) -> Rc<PluralRulesWithRanges<PluralRules>> {
    cx.locale_cache().plural_rules(resolved.settings)
}

/// PluralRuleSelect (https://tc39.es/ecma402/#sec-pluralruleselect), 17.5.1, for the locale and
/// `type` `rules` were built for and one rounded value: the String category 17.3.3 returns.
///
/// Steps 1 to 3 of 17.5.2, for not-a-number and the two infinities, are "other" whatever the
/// locale, since no plural rule has an entry for them. Steps 4 to 10, for a number that did reach
/// the arithmetic, take the operands off the rounded decimal of 16.5.2 rather than off the
/// formatted string step 10 names, because the sign and the grouping separators that string carries
/// are not part of any operand, while the digit positions UTS 35 counts - `i`, `v`, `w`, `f` and
/// `t` - are exactly what `PluralOperands` reads off this decimal.
fn plural_rule_select(
    rules: &PluralRulesWithRanges<PluralRules>,
    rounded: &RoundedNumber,
) -> &'static str {
    category_name(match rounded {
        RoundedNumber::Digits(_category, digits) => {
            rules.rules().category_for(PluralOperands::from(digits))
        }
        RoundedNumber::Special(..) => PluralCategory::Other,
    })
}

/// 17.5.4 steps 2 to 9, for the two already rounded ends of a range.
fn range_category(
    rules: &PluralRulesWithRanges<PluralRules>,
    x: &RoundedNumber,
    y: &RoundedNumber,
) -> &'static str {
    let x_category = plural_rule_select(rules, x);
    let y_category = plural_rule_select(rules, y);

    // Step 4: the range that formats as one number - `5 to 5` - takes the category of its start.
    // Two equal rounded values are the same formatted string, and the converse holds up to the
    // sign, which no plural rule distinguishes: that is why this asks the rounded value rather than
    // the rendering of it, whose separators are ILD.
    if x == y {
        return x_category;
    }

    // Steps 5 to 9: PluralRuleSelectRange (17.5.3), the ranges table of UTS 35, which answers
    // "other" when it has no entry for the pair - which is how `resolve_range` falls back.
    category_name(rules.resolve_range(category_of(x_category), category_of(y_category)))
}

/// The category of one of the six names, which is what the ranges table of UTS 35 is keyed by and
/// what [`plural_rule_select`] has already turned it into a String for.
fn category_of(name: &str) -> PluralCategory {
    match name {
        "zero" => PluralCategory::Zero,
        "one" => PluralCategory::One,
        "two" => PluralCategory::Two,
        "few" => PluralCategory::Few,
        "many" => PluralCategory::Many,
        _ => PluralCategory::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::intrinsics::intl::number_format_core::{
        RoundingType, Style, TrailingZeroDisplay, math_value_from_string,
    };

    fn rules(tag: &str, kind: PluralRuleType) -> PluralRulesWithRanges<PluralRules> {
        PluralRulesSettings {
            locale: DataLocale::try_from(tag).expect("a built in locale"),
            kind,
        }
        .build_plural_rules()
    }

    /// The category of one decimal string under the default slots, which are the ones 17.1.1
    /// resolves for `new Intl.PluralRules(tag)`: fraction digits 0 to 3 from 17.1.1 step 13.
    fn select(rules: &PluralRulesWithRanges<PluralRules>, text: &str) -> &'static str {
        plural_rule_select(
            rules,
            &round_number(&NumberFormatSlots::default(), &math_value_from_string(text)),
        )
    }

    /// The same under the digit options a test says.
    fn select_with(
        rules: &PluralRulesWithRanges<PluralRules>,
        change: impl FnOnce(&mut NumberFormatSlots),
        text: &str,
    ) -> &'static str {
        let mut slots = NumberFormatSlots::default();
        change(&mut slots);

        plural_rule_select(rules, &round_number(&slots, &math_value_from_string(text)))
    }

    fn range(rules: &PluralRulesWithRanges<PluralRules>, start: &str, end: &str) -> &'static str {
        let slots = NumberFormatSlots::default();

        range_category(
            rules,
            &round_number(&slots, &math_value_from_string(start)),
            &round_number(&slots, &math_value_from_string(end)),
        )
    }

    fn categories(rules: &PluralRulesWithRanges<PluralRules>) -> Vec<&'static str> {
        rules.rules().categories().map(category_name).collect()
    }

    #[test]
    fn each_built_in_locale_reports_its_own_categories_in_uts35_order() {
        assert_eq!(categories(&rules("en", PluralRuleType::Cardinal)), ["one", "other"]);
        // 17.3.2 step 4 sorts the list "zero", "one", "two", "few", "many", "other", which is the
        // order ICU4X's own iterator walks the rules in, so the two agree without a sort here.
        assert_eq!(
            categories(&rules("en", PluralRuleType::Ordinal)),
            ["one", "two", "few", "other"]
        );
        assert_eq!(categories(&rules("da", PluralRuleType::Cardinal)), ["one", "other"]);
        assert_eq!(categories(&rules("da", PluralRuleType::Ordinal)), ["other"]);
        assert_eq!(categories(&rules("it", PluralRuleType::Cardinal)), ["one", "many", "other"]);
        assert_eq!(categories(&rules("it", PluralRuleType::Ordinal)), ["many", "other"]);

        // 17.3.2 step 4: "other" is always one of them, because it is the fallback of 17.5.1.
        for tag in brimstone_intl_data::BUILT_IN_LOCALES {
            for kind in [PluralRuleType::Cardinal, PluralRuleType::Ordinal] {
                assert_eq!(categories(&rules(tag, kind)).last().copied(), Some("other"));
            }
        }
    }

    #[test]
    fn english_cardinal_rules_count_only_a_bare_one_as_one() {
        let en = rules("en", PluralRuleType::Cardinal);

        assert_eq!(select(&en, "1"), "one");
        assert_eq!(select(&en, "-1"), "one");
        assert_eq!(select(&en, "0"), "other");
        assert_eq!(select(&en, "2"), "other");
        assert_eq!(select(&en, "21"), "other");
        assert_eq!(select(&en, "101"), "other");
        assert_eq!(select(&en, "1000000"), "other");

        // Any shown fraction digit takes the value off the `i = 1 and v = 0` of the rule, which is
        // what 17.5.2 step 4's FormatNumericToString is for. The string "1.0" is not such a case:
        // 16.5.15 step 1 turns it into the mathematical value 1, and the fraction digits the default
        // slots allow are then stripped again by 16.5.2 steps 9 to 11.
        assert_eq!(select(&en, "1.5"), "other");
        assert_eq!(select(&en, "1.0"), "one");
        assert_eq!(select(&en, "0.5"), "other");
    }

    #[test]
    fn english_ordinal_rules_are_the_ones_two_fews_and_the_rest() {
        let en = rules("en", PluralRuleType::Ordinal);

        assert_eq!(select(&en, "1"), "one");
        assert_eq!(select(&en, "2"), "two");
        assert_eq!(select(&en, "3"), "few");
        assert_eq!(select(&en, "4"), "other");
        assert_eq!(select(&en, "11"), "other");
        assert_eq!(select(&en, "12"), "other");
        assert_eq!(select(&en, "21"), "one");
        assert_eq!(select(&en, "22"), "two");
        assert_eq!(select(&en, "23"), "few");
        assert_eq!(select(&en, "101"), "one");
        assert_eq!(select(&en, "111"), "other");
    }

    #[test]
    fn danish_cardinal_rules_follow_the_shown_digits_rather_than_the_integer() {
        let da = rules("da", PluralRuleType::Cardinal);

        assert_eq!(select(&da, "1"), "one");
        assert_eq!(select(&da, "-1"), "one");
        // The fraction digits that make these `one` are the ones da's rule asks for, and da's
        // ordinal rules ask for nothing at all.
        assert_eq!(select(&da, "1.5"), "one");
        assert_eq!(select(&da, "1.01"), "one");
        assert_eq!(select(&da, "0.5"), "one");
        assert_eq!(select(&da, "1.234"), "one");
        assert_eq!(select(&da, "0"), "other");
        assert_eq!(select(&da, "2"), "other");
        assert_eq!(select(&da, "11"), "other");

        assert_eq!(select(&rules("da", PluralRuleType::Ordinal), "1"), "other");
        assert_eq!(select(&rules("da", PluralRuleType::Ordinal), "22"), "other");
    }

    #[test]
    fn italian_rules_answer_many_for_the_large_powers_of_ten() {
        let it = rules("it", PluralRuleType::Cardinal);

        assert_eq!(select(&it, "1"), "one");
        assert_eq!(select(&it, "1.5"), "other");
        assert_eq!(select(&it, "2"), "other");
        assert_eq!(select(&it, "1000000"), "many");
        assert_eq!(select(&it, "999999"), "other");

        let ordinal = rules("it", PluralRuleType::Ordinal);
        assert_eq!(select(&ordinal, "1"), "other");
        assert_eq!(select(&ordinal, "8"), "many");
        assert_eq!(select(&ordinal, "10"), "other");
    }

    #[test]
    fn the_digit_options_change_the_category_because_the_operands_are_the_shown_digits() {
        let en = rules("en", PluralRuleType::Cardinal);

        // 17.1.1 step 13 over 16.1.2: `minimumFractionDigits: 2` shows two fraction digits of a
        // whole one, which takes it off `one` ...
        assert_eq!(select_with(&en, |slots| slots.minimum_fraction_digits = 2, "1"), "other");
        // ... unless 16.1.2 steps 25-28 also said `stripIfInteger`, which takes them off again.
        assert_eq!(
            select_with(
                &en,
                |slots| {
                    slots.minimum_fraction_digits = 2;
                    slots.trailing_zero_display = TrailingZeroDisplay::StripIfInteger;
                },
                "1",
            ),
            "one"
        );

        // Rounding at the maximum of 0 fraction digits is what decides between the neighbours.
        assert_eq!(select_with(&en, |slots| slots.maximum_fraction_digits = 0, "1.4"), "one");
        assert_eq!(select_with(&en, |slots| slots.maximum_fraction_digits = 0, "1.5"), "other");

        // `minimumIntegerDigits` pads in front, which UTS 35's `i` reads as the number it is.
        assert_eq!(select_with(&en, |slots| slots.minimum_integer_digits = 3, "1"), "one");
        assert_eq!(select_with(&en, |slots| slots.minimum_integer_digits = 3, "0"), "other");

        // Significant-digit rounding shows zeros of its own, so `1` is not `one` any more.
        assert_eq!(
            select_with(
                &en,
                |slots| {
                    slots.minimum_significant_digits = 3;
                    slots.maximum_significant_digits = 3;
                    slots.rounding_type = RoundingType::SignificantDigits;
                    slots.reports_fraction_digits = false;
                    slots.reports_significant_digits = true;
                },
                "1",
            ),
            "other"
        );
    }

    #[test]
    fn a_range_of_one_number_is_the_category_of_its_start() {
        // 17.5.4 step 4.
        let da = rules("da", PluralRuleType::Cardinal);
        assert_eq!(range(&da, "1", "1"), "one");
        assert_eq!(range(&da, "200", "200"), "other");

        let it = rules("it", PluralRuleType::Cardinal);
        assert_eq!(range(&it, "1", "1"), "one");
    }

    #[test]
    fn the_ranges_table_answers_the_pair_it_has_an_entry_for() {
        // The pairs of 17.5.3 as CLDR has them for these three locales: da's `0 to 1` and `1 to 1`
        // are `one`, its `2 to 3` has no entry and is `other` for the end of it.
        let da = rules("da", PluralRuleType::Cardinal);
        assert_eq!(range(&da, "0", "1"), "one");
        assert_eq!(range(&da, "-1", "1"), "one");
        assert_eq!(range(&da, "0", "0.5"), "one");
        assert_eq!(range(&da, "1", "2"), "other");
        assert_eq!(range(&da, "2", "3"), "other");
        assert_eq!(range(&da, "102", "201"), "other");

        let it = rules("it", PluralRuleType::Cardinal);
        assert_eq!(range(&it, "0", "1"), "one");
        assert_eq!(range(&it, "102", "201"), "other");

        // English's table is what answers these, and `0 to 1` is the pair of them that is not merely
        // the end of the range: `other to one` is `other`, where 17.5.3's fallback would have said
        // `one`.
        let en = rules("en", PluralRuleType::Cardinal);
        assert_eq!(range(&en, "102", "201"), "other");
        assert_eq!(range(&en, "200", "201"), "other");
        assert_eq!(range(&en, "0", "1"), "other");
    }

    #[test]
    fn the_special_values_are_other_and_the_range_of_two_of_them_says_so() {
        // 17.5.2 steps 1 to 3, which the rounded value carries as [`RoundedNumber::Special`].
        for tag in brimstone_intl_data::BUILT_IN_LOCALES {
            let rules = rules(tag, PluralRuleType::Cardinal);

            for text in ["NaN", "Infinity", "-Infinity"] {
                let rounded =
                    round_number(&NumberFormatSlots::default(), &math_value_from_string(text));

                assert!(matches!(rounded, RoundedNumber::Special(..)));
                assert_eq!(plural_rule_select(&rules, &rounded), "other");
            }

            // 17.5.4 step 4 for the pair of infinities, whose strings are the same word.
            let infinity =
                round_number(&NumberFormatSlots::default(), &math_value_from_string("Infinity"));
            assert_eq!(range_category(&rules, &infinity, &infinity), "other");
        }
    }

    #[test]
    fn percent_scaling_happens_before_the_operands_are_read() {
        // 16.5.2 steps 5.1.2-5.1.3 scale a percent by 100 before 17.1.1 step 13's rounding, so
        // `0.01` pluralizes as the `1` it is.
        let en = rules("en", PluralRuleType::Cardinal);

        let slots = NumberFormatSlots {
            style: Style::Percent,
            minimum_fraction_digits: 0,
            maximum_fraction_digits: 0,
            ..NumberFormatSlots::default()
        };

        let rounded = round_number(&slots, &math_value_from_string("0.01"));
        assert_eq!(plural_rule_select(&en, &rounded), "one");

        // 1.5%, which rounds up to `2%` at 0 fraction digits, is not.
        let rounded = round_number(&slots, &math_value_from_string("0.015"));
        assert_eq!(plural_rule_select(&en, &rounded), "other");
    }
}
