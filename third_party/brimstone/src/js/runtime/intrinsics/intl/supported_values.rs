//! `Intl.supportedValuesOf` (https://tc39.es/ecma402/#sec-intl.supportedvaluesof), 8.3.2: the six
//! lists of option values this build's data can answer with.
//!
//! Each branch of steps 2 to 7 is the list the spec names, read out of the data
//! `icu/generate_intl_data.sh` baked rather than out of a table of what CLDR or another engine
//! happens to know. That is what makes the empty answers honest: a value that is absent here is a
//! value the engine cannot format anything with, and 8.3.2 has no branch that asks for more than the
//! implementation provides. Nothing in this module invents a value to fill a list out.

use super::locale_data::available_calendars;
use super::locale_negotiation::canonical_u_value;
use super::number_format_currency::{load_optional, marker_attributes};
use crate::runtime::{
    Context, EvalResult, Handle, Value, array_object::create_array_from_list, error::range_error,
    to_string,
};

/// The keys 8.3.2 steps 2 to 7 test, in that order, and step 8's RangeError names them all.
pub const SUPPORTED_VALUE_KEYS: [&str; 6] = [
    "calendar",
    "collation",
    "currency",
    "numberingSystem",
    "timeZone",
    "unit",
];

/// Intl.supportedValuesOf (https://tc39.es/ecma402/#sec-intl.supportedvaluesof), 8.3.2.
pub fn supported_values_of(mut cx: Context, key: Handle<Value>) -> EvalResult<Handle<Value>> {
    // Step 1: ? ToString(key).
    let text = to_string(cx, key)?.to_wtf8_string()?;
    let key = String::from_utf8_lossy(text.as_bytes()).into_owned();

    // Steps 2 to 7.
    let list = match key.as_str() {
        "calendar" => calendar_values(),
        "collation" => collation_values(),
        "currency" => currency_values(),
        "numberingSystem" => numbering_system_values(),
        "timeZone" => time_zone_values(cx),
        "unit" => unit_values(),
        // Step 8: any other key, including one that names a real option of one of the constructors
        // but is not one of the six (`"style"` and `"type"` among them), is the RangeError.
        _ => {
            return range_error(
                cx,
                &format!(
                    "Intl.supportedValuesOf key must be one of: {}",
                    SUPPORTED_VALUE_KEYS.join(", ")
                ),
            );
        }
    };

    // Step 9: CreateArrayFromList(list).
    let mut elements = Vec::with_capacity(list.len());
    for value in &list {
        elements.push(cx.alloc_string(value)?.as_value());
    }

    Ok(create_array_from_list(cx, &elements)?.into())
}

/// AvailableCanonicalCalendars, the branch of step 2: `AvailableCalendars` filtered by the value
/// spelling registry the same operation writes back out. The one calendar this build formats with is
/// spelled the registry's way already, so nothing is dropped here; a CLDR alias such as "islamic-civil"
/// would be, because 15.5.9 would answer it back canonicalized.
fn calendar_values() -> Vec<String> {
    available_calendars()
        .iter()
        // Step 2.2: keep the identifier only when canonicalization leaves it where it is.
        .filter(|identifier| canonical_u_value("ca", identifier) == **identifier)
        .map(|identifier| (*identifier).to_owned())
        .collect()
}

/// AvailableCanonicalCollations, the branch of step 3.
///
/// Empty, because this build bakes no collation type: `[[SortLocaleData]].[[co]]` is the single
/// `null` of 10.2.3 that `collator_options.rs`'s `COLLATION_VALUES` says, so a `-u-co-` value names
/// a tailoring no `Intl.Collator` of this engine can be built with. Reporting CLDR's twenty-odd
/// collation types here would promise comparisons this build does not make.
fn collation_values() -> Vec<String> {
    Vec::new()
}

/// AvailableCanonicalCurrencies, the branch of step 4: the codes this build's currency data has a row
/// for, which is the same set of identifiers `Intl.NumberFormat` formats and `Intl.DisplayNames`
/// names, because both read this one table.
///
/// The list is found rather than written down. Every three-letter combination is asked of the baked
/// data - the 17,576 of 8.3.1's `IsWellFormedCurrencyCode` space, which a trie answers for a miss in
/// a handful of probes - and only the codes the data has a row for are kept. A code the data lacks is
/// therefore absent whatever ISO 4217 may say about it, and nothing here is a list somebody typed.
/// The scan runs once and its answer is kept for the rest of the process, which is why the cost of
/// the misses is paid once rather than once per call.
fn currency_values() -> Vec<String> {
    use std::sync::OnceLock;

    static CURRENCY_CODES: OnceLock<Vec<String>> = OnceLock::new();

    CURRENCY_CODES
        .get_or_init(|| {
            let mut list = Vec::new();

            for first in b'A'..=b'Z' {
                for second in b'A'..=b'Z' {
                    for third in b'A'..=b'Z' {
                        let code = [first, second, third]
                            .iter()
                            .map(|byte| *byte as char)
                            .collect::<String>();

                        if currency_is_baked(&code) {
                            list.push(code);
                        }
                    }
                }
            }

            list
        })
        .clone()
}

/// Whether the baked currency data has a row for `code`, asked with the locales the data is keyed at:
/// `und`, CLDR's root, and `en`, the locale this build falls back to and so the one whose rows are
/// always there. An error that is not the miss of an absent code is a build without the marker at
/// all, and answers the same way an absent code does.
fn currency_is_baked(code: &str) -> bool {
    use icu_experimental::dimension::provider::currency::extended::CurrencyExtendedDataV1;

    let Ok(attributes) = marker_attributes(code) else {
        return false;
    };

    ["und", "en"]
        .iter()
        .filter_map(|tag| tag.parse::<icu_locale::Locale>().ok())
        .any(|locale| {
            matches!(
                load_optional::<CurrencyExtendedDataV1>(&(&locale).into(), attributes),
                Ok(Some(_))
            )
        })
}

/// AvailableCanonicalNumberingSystems, the branch of step 5.
///
/// The digits this build writes: `latn`, which is the one entry of the `nu` list of 11.2.3 and of the
/// symbol tables `number_format_object.rs` resolves through. test262's Table 4 asks for the decimal
/// digit systems of the specification as well; those would be the digits of a numbering system whose
/// symbols the data does not carry, so they are absent rather than rendered with Latin ones.
fn numbering_system_values() -> Vec<String> {
    ["latn"]
        .iter()
        .filter(|identifier| canonical_u_value("nu", identifier) == **identifier)
        .map(|identifier| (*identifier).to_owned())
        .collect()
}

/// AvailablePrimaryTimeZoneIdentifiers, the branch of step 6: the zone names of the IANA database the
/// time-zone provider resolves, without the links that alias them and without the names that resolve
/// to nothing.
///
/// The provider is the one `Intl.DateTimeFormat` formats with - `Context::temporal_provider` resolves
/// an id through the same normalizer this reads - so the list is the ids the engine accepts, and an
/// id it accepts only as an alias ("US/Eastern", "Africa/Asmera") is reported as the name it
/// canonicalizes to rather than as both.
fn time_zone_values(cx: Context) -> Vec<String> {
    let mut list: Vec<String> = primary_zone_names()
        .into_iter()
        // The alias table and the zone data are two separate parts of the IANA distribution, and a
        // name can be a key of the first with no entry in the second - "Asia/Hanoi" is one, which
        // 11.5.3 refuses and so does every option that takes a zone. A value 8.3.2 reports has to be
        // a value the engine can format with, so the parser of that clause filters the name, which
        // leaves the order [`primary_zone_names`] put them in.
        .filter(|name| {
            cx.locale_cache().iana_parser().as_borrowed().parse(name) != icu_time::TimeZone::UNKNOWN
        })
        .collect();

    // 11.5.3 canonicalizes `UTC` and its IANA spelling `Etc/UTC` to the same zone, and it is the one
    // id the engine formats with without looking the name up, so it is a primary identifier of this
    // build whether or not the alias table names it as one. Sorted again because the entry this adds
    // is added at the end.
    if !list.iter().any(|name| name == "UTC") {
        list.push("UTC".to_owned());
        list.sort();
    }

    list
}

/// The primary names of the baked IANA alias table, before the zone data is consulted.
fn primary_zone_names() -> Vec<String> {
    use timezone_provider::provider::{NormalizedId, TimeZoneNormalizer};
    use timezone_provider::{CompiledNormalizer, SINGLETON_IANA_NORMALIZER};

    let normalizer = CompiledNormalizer;
    let mut list = Vec::new();

    for index in 0..SINGLETON_IANA_NORMALIZER.normalized_identifiers.len() {
        let id = NormalizedId(index);

        // A name is primary when canonicalizing it leaves it where it is; a link is rewritten to the
        // zone it points at, which is the name reported instead.
        if normalizer.canonicalized(id).ok() != Some(id) {
            continue;
        }

        if let Ok(name) = normalizer.identifier(id) {
            list.push(name.to_owned());
        }
    }

    // A `%Array.prototype.sort%` with no comparator orders by UTF-16 code unit. These ids are the
    // ASCII the IANA database writes, where Rust's byte order and that order are the same order.
    list.sort();
    list.dedup();

    list
}

/// AvailableCanonicalUnits, the branch of step 7.
///
/// Empty, because no unit data is baked: `number_format_core.rs`'s `Style` names "unit" and has no
/// variant for it, so `Intl.NumberFormat` of that style is the RangeError it reports. Table 1's
/// simple units are the identifiers of a format this build cannot produce.
fn unit_values() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_calendar_list_is_the_one_calendar_this_build_formats_with() {
        assert_eq!(calendar_values(), ["gregory"]);

        // Step 2.2 keeps a value only in the registry's own spelling, so an alias is not reported
        // even in principle: `canonical_u_value` rewrites "islamic-civil" to the registry's form.
        assert_eq!(canonical_u_value("ca", "islamic-civil"), "islamic-civil");
        assert_eq!(canonical_u_value("ca", "gregory"), "gregory");
    }

    #[test]
    fn the_collation_list_is_empty_because_no_collation_type_is_baked() {
        assert!(collation_values().is_empty());
    }

    #[test]
    fn the_currency_list_is_the_codes_the_baked_currency_data_has() {
        let currencies = currency_values();

        // The data has a row per currency rather than one per locale and currency, so the list is
        // far longer than the handful of currencies the page formats and far shorter than ISO 4217.
        assert!(currencies.len() > 100, "{} codes baked", currencies.len());
        assert!(currencies.contains(&"USD".to_owned()));
        assert!(currencies.contains(&"DKK".to_owned()));
        assert!(currencies.contains(&"EUR".to_owned()));

        // Sorted, unique, three ASCII letters, and upper case - the three properties 8.3.2's
        // currency branch and step 9's array together report.
        let mut sorted = currencies.clone();
        sorted.sort();
        assert_eq!(sorted, currencies);

        let mut unique = currencies.clone();
        unique.dedup();
        assert_eq!(unique.len(), currencies.len());

        assert!(
            currencies
                .iter()
                .all(|code| code.len() == 3 && code.to_ascii_uppercase() == *code)
        );
    }

    #[test]
    fn the_numbering_system_list_is_the_digits_the_data_carries() {
        assert_eq!(numbering_system_values(), ["latn"]);
    }

    #[test]
    fn the_time_zone_list_is_primary_names_only() {
        // The names before the zone data filters them, which is the half of `time_zone_values` that
        // needs no context; that the remainder are every one of them is the integration test's
        // `Intl.supportedValuesOf` section, which formats through each.
        let zones = primary_zone_names();

        assert!(zones.len() > 300, "{} zones", zones.len());
        assert!(zones.contains(&"America/New_York".to_owned()));
        assert!(zones.contains(&"Europe/Copenhagen".to_owned()));

        // A link is the zone it points at, so the alias is not its own entry.
        assert!(!zones.contains(&"US/Eastern".to_owned()));
        assert!(!zones.contains(&"Africa/Asmera".to_owned()));
        assert!(!zones.contains(&"Etc/UTC".to_owned()));

        let mut sorted = zones.clone();
        sorted.sort();
        assert_eq!(sorted, zones);

        let mut unique = zones.clone();
        unique.dedup();
        assert_eq!(unique.len(), zones.len());
    }

    #[test]
    fn the_unit_list_is_empty_because_no_unit_data_is_baked() {
        assert!(unit_values().is_empty());
    }

    #[test]
    fn the_six_keys_are_the_ones_the_operation_tests() {
        assert_eq!(
            SUPPORTED_VALUE_KEYS,
            [
                "calendar",
                "collation",
                "currency",
                "numberingSystem",
                "timeZone",
                "unit"
            ]
        );
    }
}
