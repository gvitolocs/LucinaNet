//! The options of `Intl.Collator`: the resolution of 10.1.1 steps 5-22, which is ResolveOptions
//! (9.2.8) run over the [[ResolutionOptionDescriptors]] and the locale data of 10.2.3, followed by
//! the three options that those steps leave outside the locale keys.
//!
//! Nothing here builds an ICU4X object. The result is the `CollatorSettings` that
//! `collator_object.rs` caches a collator under, plus the values that 10.3.2 reports, so each step
//! can be checked against the numbering of the spec in one pass.

use crate::runtime::{
    Context, EvalResult, Handle, PropertyKey, Value,
    error::range_error,
    intrinsics::intl::{
        collator_object::{CaseFirst, CollatorSettings, Sensitivity, Usage},
        intl_object::canonicalize_locale_list_argument,
        locale_negotiation::{ExtensionKey, Matcher, resolve_locale_with_extension_keys},
        options::{OptionType, coerce_options_to_object, get_option},
    },
    object_value::ObjectValue,
    to_string,
};

/// `[[SortLocaleData]].[[<locale>]].[[co]]`, and the same list in the search data (10.2.3). Its
/// first element must be the spec's `null` and "standard" and "search" must not appear in it; this
/// build bakes no named collation type at all, so `null` is its only element and 10.1.1 step 14
/// reports "default" for every collator it resolves.
const COLLATION_VALUES: &[Option<&str>] = &[None];

/// `[[<locale>]].[[kn]]`: the two spellings of a Boolean, "false" first as the default. A bare
/// `-u-kn` therefore means "true" (ResolveLocale step 12.6.4).
const NUMERIC_VALUES: &[Option<&str>] = &[Some("false"), Some("true")];

/// `[[<locale>]].[[kf]]`: the three values 10.2.3 names for `caseFirst`, starting from the default
/// "false". It is the default in every built in locale, Danish included, because the "false" of
/// 10.1.1 step 17 means "leave it to the locale", which is what
/// `CollatorSettings::build_collator` then does.
const CASE_FIRST_VALUES: &[Option<&str>] = &[Some("false"), Some("upper"), Some("lower")];

/// `[[SearchLocaleData]].[[<locale>]].[[sensitivity]]` (10.2.3), the default of 10.1.1 step 19 for
/// `usage: "search"`. ICU4C answers "variant" for the search collation of every locale this build
/// carries, which is also what makes search compare the way sort does here.
const SEARCH_SENSITIVITY: Sensitivity = Sensitivity::Variant;

/// `[[ignorePunctuation]]` of both data tables (10.2.3), which 10.1.1 step 21 uses as the default.
const IGNORE_PUNCTUATION_DEFAULT: bool = false;

/// What one call of the Intl.Collator constructor (https://tc39.es/ecma402/#sec-intl.collator)
/// computes: the internal slots of 10.4 that are not each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCollator {
    /// [[Locale]], the tag of ResolveLocale step 14, carrying the keywords that survived
    /// step 12.12.
    pub locale: String,

    /// [[Usage]], step 8. It sits outside `settings` because it changes nothing about the
    /// comparison this build performs, and must not split the collator cache in two.
    pub usage: Usage,

    /// [[Collation]], step 14.
    pub collation: String,

    /// [[Numeric]], [[CaseFirst]], [[Sensitivity]] and [[IgnorePunctuation]], which are also the
    /// key the collator for this resolution is cached under.
    pub settings: CollatorSettings,
}

/// Steps 5 to 22, in that order. The order is observable: steps 5 to 8 run before ResolveOptions,
/// which itself reads four options, and steps 20 and 22 run after it.
pub fn resolve_collator(
    cx: Context,
    locales: Handle<Value>,
    options: Handle<Value>,
) -> EvalResult<ResolvedCollator> {
    // Step 5: CanonicalizeLocaleList(locales).
    let requested = canonicalize_locale_list_argument(cx, locales)?;

    // Step 6: CoerceOptionsToObject(options).
    let given = coerce_options_to_object(cx, options)?;

    // Steps 7-8: [[Usage]].
    let usage =
        Usage::parse(&text_option(cx, given, cx.names.usage(), &["sort", "search"], "sort")?);

    // Steps 9-10 pick the locale data table by the usage. The sort and the search table of this
    // build differ only in the sensitivity default, which is why those two are the constants above
    // rather than a record that travels through ResolveLocale to be read back out of it.

    // 9.2.8 steps 4-5: the one option ResolveLocale step 1 needs before it matches anything.
    let matcher = locale_matcher(cx, given)?;

    // 9.2.8 step 6 over the three [[ResolutionOptionDescriptors]] of 10.2.3, in their order.
    let collation = locale_option(cx, given, cx.names.collation(), OptionType::String, &[])?;
    let numeric = locale_option(cx, given, cx.names.numeric(), OptionType::Boolean, &[])?;
    let case_first = locale_option(
        cx,
        given,
        cx.names.case_first(),
        OptionType::String,
        &["upper", "lower", "false"],
    )?;

    // 9.2.8 step 7, which is ResolveLocale (9.2.7) over [[RelevantExtensionKeys]] and the locale
    // data. Its `values` come back in the order of the keys, which is the order 10.1.1 reads them.
    let keys = [
        ExtensionKey {
            key: "co",
            values: COLLATION_VALUES,
            requested: collation.as_deref(),
        },
        ExtensionKey {
            key: "kn",
            values: NUMERIC_VALUES,
            requested: numeric.as_deref(),
        },
        ExtensionKey {
            key: "kf",
            values: CASE_FIRST_VALUES,
            requested: case_first.as_deref(),
        },
    ];
    let resolved = resolve_locale_with_extension_keys(
        cx.locale_cache().canonicalizer(),
        &requested,
        matcher,
        &keys,
    );

    // Step 13: [[Locale]].
    let locale = resolved.locale;

    // Step 14: [[Collation]], whose `null` is spelled "default".
    let collation = resolved.values[0]
        .clone()
        .unwrap_or_else(|| "default".to_owned());

    // Steps 15-16: [[Numeric]] is SameValue(resolvedLocale.[[kn]], "true").
    let numeric = resolved.values[1].as_deref() == Some("true");

    // Step 17: [[CaseFirst]]. ResolveLocale step 12.3 always leaves one of the three strings of
    // `CASE_FIRST_VALUES` here, "false" unless the tag or the options asked for otherwise.
    let case_first = CaseFirst::parse(resolved.values[2].as_deref().unwrap_or("false"));

    // Steps 18-19: the sensitivity default, which step 19 fixes for "sort" and takes from the
    // search data for "search".
    let default_sensitivity = match usage {
        Usage::Sort => Sensitivity::Variant,
        Usage::Search => SEARCH_SENSITIVITY,
    };

    // Step 20: [[Sensitivity]].
    let sensitivity = Sensitivity::parse(&text_option(
        cx,
        given,
        cx.names.sensitivity(),
        &["base", "accent", "case", "variant"],
        default_sensitivity.as_str(),
    )?);

    // Steps 21-22: [[IgnorePunctuation]], read as the Boolean that 9.2.11 step 3 converts to.
    let ignore_punctuation = get_option(
        cx,
        given,
        cx.names.ignore_punctuation(),
        OptionType::Boolean,
        &[],
        Some(cx.bool(IGNORE_PUNCTUATION_DEFAULT)),
    )?
    .as_bool();

    Ok(ResolvedCollator {
        locale,
        usage,
        collation,
        settings: CollatorSettings {
            locale: resolved.data_locale,
            numeric,
            case_first,
            sensitivity,
            ignore_punctuation,
        },
    })
}

/// The `localeMatcher` option, which ResolveOptions step 4 and FilterLocales step 2 read out of the
/// options object the same way. `best fit` is the default of both, and this build's best fit is its
/// lookup (see `locale_negotiation::lookup_locale`), so the two answers resolve alike.
pub fn locale_matcher(cx: Context, options: Handle<ObjectValue>) -> EvalResult<Matcher> {
    if text_option(cx, options, cx.names.locale_matcher(), &["lookup", "best fit"], "best fit")?
        == "lookup"
    {
        Ok(Matcher::Lookup)
    } else {
        Ok(Matcher::BestFit)
    }
}

/// One turn of the loop of ResolveOptions step 6 for a descriptor whose value becomes
/// `opt.[[<key>]]`: step 6.3, then the conversion of step 6.4.1 and the check of step 6.4.2. `None`
/// is the undefined of step 6.3, which leaves the key out of the request entirely.
fn locale_option(
    cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
    option_type: OptionType,
    values: &[&str],
) -> EvalResult<Option<String>> {
    // Step 6.3, whose default is undefined so that an absent option is the undefined of 9.2.11
    // step 2.2 and not the TypeError of its step 2.1.
    let value = get_option(cx, options, property, option_type, values, Some(cx.undefined()))?;
    if value.is_undefined() {
        return Ok(None);
    }

    // Steps 6.4.1-6.4.2. The check runs on what the conversion produced, so a `numeric` of
    // `undefined` never reaches it, and one of `1` arrives as "true".
    let text = option_text(cx, value)?;
    if !is_unicode_locale_type(&text) {
        return range_error(cx, "Intl option value is not a Unicode locale type");
    }

    Ok(Some(text))
}

/// GetOption for the options that 10.1.1 reads outside ResolveOptions, in the text form the tables
/// of this file are written in. `default` is the String that 9.2.11 step 2.2 substitutes, which
/// needs neither the conversion of its step 4 nor the check of its step 5 because this file wrote
/// it.
fn text_option(
    mut cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
    values: &[&str],
    default: &'static str,
) -> EvalResult<String> {
    let value = get_option(
        cx,
        options,
        property,
        OptionType::String,
        values,
        Some(cx.alloc_static_string(default)?.as_value()),
    )?;

    option_text(cx, value)
}

/// The text of a String value, for the comparisons of this file. A keyword value that a language
/// tag can carry is ASCII, so the lossy conversion only renames the values that are going to be
/// refused for not being one.
pub(crate) fn option_text(cx: Context, value: Handle<Value>) -> EvalResult<String> {
    let text = to_string(cx, value)?.to_wtf8_string()?;
    Ok(String::from_utf8_lossy(text.as_bytes()).into_owned())
}

/// Whether `value` is matched by the `type` Unicode locale nonterminal, which ResolveOptions step
/// 6.4.2 checks. `locale_negotiation`'s `type_value` is the one reading of that production in this
/// engine, since 15.1.1's four option checks match against the same nonterminal.
pub fn is_unicode_locale_type(value: &str) -> bool {
    super::locale_negotiation::type_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        ContextBuilder,
        embed::{evaluate_script, with_realm},
        intrinsics::intl::locale_negotiation::default_locale,
    };

    /// Run `body` inside a fresh context, with its realm entered the way an embedder would.
    fn with_context(body: impl FnOnce(Context)) {
        let cx = ContextBuilder::new().build().unwrap();
        with_realm(cx, cx.initial_realm(), |cx| {
            body(cx);
        });
        cx.drop();
    }

    /// Resolve as 10.1.1 would, from two scripts that produce its two arguments.
    fn resolved(locales: &str, options: &str) -> ResolvedCollator {
        let cx = ContextBuilder::new().build().unwrap();
        let outcome = with_realm(cx, cx.initial_realm(), |cx| {
            let locales = evaluate_script(cx, cx.initial_realm(), locales, "locales.js", 1)?;
            let options = evaluate_script(cx, cx.initial_realm(), options, "options.js", 1)?;
            resolve_collator(cx, locales, options)
        });
        cx.drop();

        outcome.unwrap()
    }

    /// The same, for a call that is expected to throw.
    fn refuses(source: &str) {
        with_context(|cx| {
            let options = evaluate_script(cx, cx.initial_realm(), source, "options.js", 1).unwrap();
            assert!(resolve_collator(cx, cx.undefined(), options).is_err(), "{source}");
        });
    }

    #[test]
    fn a_request_with_nothing_in_it_takes_every_default() {
        let collator = resolved("undefined", "undefined");

        assert_eq!(collator.usage, Usage::Sort);
        assert_eq!(collator.collation, "default");
        assert_eq!(collator.locale, default_locale());
        assert_eq!(collator.settings, CollatorSettings::default());
    }

    #[test]
    fn search_usage_is_reported_and_changes_nothing_else() {
        let collator = resolved("undefined", "({ usage: 'search' })");

        assert_eq!(collator.usage, Usage::Search);
        assert_eq!(collator.settings, CollatorSettings::default());

        // Anything else in that list is the RangeError of 9.2.11 step 5.
        refuses("({ usage: 'binary' })");
    }

    #[test]
    fn the_options_are_read_in_the_order_the_steps_put_them() {
        with_context(|cx| {
            let options = evaluate_script(
                cx,
                cx.initial_realm(),
                "(() => { const seen = []; globalThis.seen = seen; return new Proxy({}, { \
                     get(target, name) { if (typeof name === 'string') seen.push(name); \
                     return target[name]; } }); })()",
                "order.js",
                1,
            )
            .unwrap();

            resolve_collator(cx, cx.undefined(), options).unwrap();

            let order =
                evaluate_script(cx, cx.initial_realm(), "seen.join(',')", "order.js", 1).unwrap();
            let expected = "usage,localeMatcher,collation,numeric,caseFirst,sensitivity,\
                            ignorePunctuation";
            assert!(order.as_string().flatten().unwrap().eq_str(expected));
        });
    }

    #[test]
    fn an_option_value_never_reaches_the_reported_tag() {
        // ResolveLocale step 12.11 clears the keyword of a tag as soon as an options value
        // supersedes it, and step 12.12 never adds one for a value that only came from options.
        let collator = resolved("undefined", "({ numeric: true })");

        assert!(collator.settings.numeric);
        assert_eq!(collator.locale, default_locale());
    }

    #[test]
    fn a_keyword_that_survives_the_request_is_written_on_the_tag() {
        let collator = resolved("'en-u-kn-true'", "undefined");

        assert!(collator.settings.numeric);

        // Annex C's canonical syntax drops a type value of "true", so the resolved tag carries the
        // key alone - the second case of test262's
        // `Collator/prototype/resolvedOptions/resolved-numeric-unicode-extensions-and-options.js`.
        assert_eq!(collator.locale, "en-u-kn");

        // The option then overrides the tag, and the tag goes back to being just a locale.
        let collator = resolved("'en-u-kn-false'", "({ numeric: true })");
        assert!(collator.settings.numeric);
        assert_eq!(collator.locale, default_locale());
    }

    #[test]
    fn case_first_comes_out_of_the_tag_and_the_option_alike() {
        assert_eq!(resolved("'en-u-kf-upper'", "undefined").settings.case_first, CaseFirst::Upper);
        assert_eq!(
            resolved("undefined", "({ caseFirst: 'lower' })")
                .settings
                .case_first,
            CaseFirst::Lower
        );

        // The bare spelling of a key whose data holds no "true" is ignored (step 12.6.4).
        assert_eq!(resolved("'en-u-kf'", "undefined").settings.case_first, CaseFirst::False);

        // "false" is the request to leave it to the locale, which is what the comparison of
        // `a_false_case_first_leaves_the_locale_the_boss` checks.
        let danish = resolved("'da-u-kf-false'", "undefined");
        assert_eq!(danish.settings.case_first, CaseFirst::False);
        assert_eq!(danish.settings.locale, "da");
        assert_eq!(danish.locale, "da-u-kf-false");
    }

    #[test]
    fn sensitivity_follows_the_usage_that_step_19_names() {
        // 10.1.1 fixes "variant" for sort and takes search from the data, and this build's data
        // says "variant" for both, so nothing asks the question differently.
        assert_eq!(
            resolved("undefined", "({ usage: 'search' })")
                .settings
                .sensitivity,
            Sensitivity::Variant
        );

        for (source, expected) in [
            ("base", Sensitivity::Base),
            ("accent", Sensitivity::Accent),
            ("case", Sensitivity::Case),
            ("variant", Sensitivity::Variant),
        ] {
            let options = format!("({{ sensitivity: '{source}' }})");
            assert_eq!(resolved("undefined", &options).settings.sensitivity, expected, "{source}");
        }

        refuses("({ sensitivity: 'x' })");
    }

    #[test]
    fn ignore_punctuation_is_coerced_the_way_get_option_coerces_a_boolean() {
        assert!(
            resolved("undefined", "({ ignorePunctuation: 'no' })")
                .settings
                .ignore_punctuation
        );
        assert!(
            !resolved("undefined", "({ ignorePunctuation: '' })")
                .settings
                .ignore_punctuation
        );
    }

    #[test]
    fn a_collation_this_build_has_no_data_for_leaves_the_default() {
        // "standard" is a real `type` and a real CLDR collation, but 10.2.3 forbids it as a value
        // of the locale data, so ResolveLocale step 12.11 refuses it and step 14 says "default".
        assert_eq!(resolved("undefined", "({ collation: 'standard' })").collation, "default");
        assert_eq!(resolved("'en-u-co-standard'", "undefined").collation, "default");
    }

    #[test]
    fn a_value_that_is_not_a_unicode_locale_type_is_refused() {
        // The check is 9.2.8 step 6.4.2, which runs on the converted text and on nothing else.
        for source in [
            "({ collation: 'ea' })",
            "({ collation: 'abcdefghi' })",
            "({ collation: '12' })",
            "({ collation: 'a-bc' })",
            "({ collation: 'two words' })",
        ] {
            refuses(source);
        }

        assert_eq!(resolved("undefined", "({ collation: 'abcdefgh' })").collation, "default");
        assert_eq!(resolved("undefined", "({ collation: 'a23-b45' })").collation, "default");
    }

    #[test]
    fn a_boolean_option_is_checked_after_its_conversion() {
        // Step 6.4.1 stringifies what 9.2.11 step 3 has already coerced, so `1` arrives as "true"
        // and is a `type`, while an uncoercible value could never reach here at all.
        let collator = resolved("undefined", "({ numeric: 1 })");
        assert!(collator.settings.numeric);

        let collator = resolved("undefined", "({ numeric: 'yes' })");
        assert!(collator.settings.numeric);

        let collator = resolved("undefined", "({ numeric: 0 })");
        assert!(!collator.settings.numeric);
    }

    #[test]
    fn the_type_production_needs_three_to_eight_alphanumerics_in_every_group() {
        assert!(is_unicode_locale_type("false"));
        assert!(is_unicode_locale_type("abc"));
        assert!(is_unicode_locale_type("abcdefgh"));
        assert!(is_unicode_locale_type("a23-b45"));
        assert!(is_unicode_locale_type("a23_b45"));

        assert!(!is_unicode_locale_type(""));
        assert!(!is_unicode_locale_type("ab"));
        assert!(!is_unicode_locale_type("abcdefghi"));
        assert!(!is_unicode_locale_type("ab-cde"));
        assert!(!is_unicode_locale_type("abc-de"));
        assert!(!is_unicode_locale_type("abc-de-"));
        assert!(!is_unicode_locale_type("abc.de"));
        assert!(!is_unicode_locale_type("abc de"));
        assert!(!is_unicode_locale_type("ábc"));
    }

    #[test]
    fn an_unsupported_locale_falls_back_to_english_without_its_extension() {
        // ResolveLocale step 4: with nothing matched, the record it works from is the default locale
        // with an empty extension, so ordering that was asked for in a locale this build does not
        // carry never reaches the English collator.
        let collator = resolved("'zh-u-kn-true'", "undefined");

        assert_eq!(collator.locale, default_locale());
        assert_eq!(collator.settings.locale, "en");
        assert!(!collator.settings.numeric);

        // An option of the same request applies to the fallback just as well.
        let collator = resolved("'zh'", "({ numeric: true })");
        assert_eq!(collator.locale, default_locale());
        assert!(collator.settings.numeric);
    }

    #[test]
    fn a_malformed_locale_is_refused_before_any_option_is_read() {
        let cx = ContextBuilder::new().build().unwrap();
        let outcome = with_realm(cx, cx.initial_realm(), |cx| {
            let locales =
                evaluate_script(cx, cx.initial_realm(), "'not a tag'", "locales.js", 1).unwrap();
            resolve_collator(cx, locales, cx.undefined())
        });
        cx.drop();

        assert!(outcome.is_err());
    }

    #[test]
    fn the_word_root_is_refused_as_a_locale_too() {
        // RFC 5646 would read "root" as a four-letter language, but it codes nothing: it is the
        // name of the base data, which is why the data crate lists it and CanonicalizeLocaleList
        // (9.2.1 step 4.c) still refuses it. V8 and SpiderMonkey refuse it alike, and so the word
        // never reaches `CollatorSettings::build_collator`, where ICU4X has no tag to parse.
        let cx = ContextBuilder::new().build().unwrap();
        let outcome = with_realm(cx, cx.initial_realm(), |cx| {
            let locales =
                evaluate_script(cx, cx.initial_realm(), "'root'", "locales.js", 1).unwrap();
            resolve_collator(cx, locales, cx.undefined())
        });
        cx.drop();

        assert!(outcome.is_err());
    }
}
