//! The Locale Info operations, ECMA-402 15.5.9 to 15.5.17: what `Intl.Locale`'s `get…` methods ask
//! CLDR about a locale. They take the [[Locale]] of an instance rather than the instance, and work
//! on plain Rust values, so that what a tag answers can be checked without a JS context.
//!
//! Two things about the data decide most of what these functions return. The first is that CLDR
//! keys its tables by region and, for a few of its rows, by whole locale: 15.5.9 step 4 and 15.5.11
//! step 6 walk those keys in that order, and `brimstone_intl_data::locale_info` holds the rows of
//! this build's locales and of `001`, CLDR's own root. Where the spec asks whether data is
//! "available" for a region the table does not name, the root row answers: that is how CLDR is read,
//! and the reason an hour cycle is reported for a region this build carries no row of rather than
//! nothing. The one operation that will not inherit is [`time_zones_of_locale`], because which
//! zones a region uses is a fact about that region and not about the world.
//!
//! The second is that these are the operations the spec leaves implementation-defined, and what an
//! implementation has decided here is which data the generator baked: `en`, `da` and `it`, and
//! nothing else. An answer for another locale is either the general one CLDR gives for everyone
//! (`001`) or a refusal to answer, which is what 15.5.13 leaves to the implementation as much as
//! the list itself.

use std::sync::LazyLock;

use brimstone_intl_data::IntlDataProvider;
use brimstone_intl_data::locale_info::{
    ROOT_REGION, TimeData, calendar_preference_row, time_data_row, time_zones_row, week_data_row,
};
use icu_locale::{Direction, LanguageIdentifier, LocaleDirectionality};

use super::locale_negotiation::{
    RegionPreference, add_likely_subtags, canonical_u_value, locale_language, locale_region,
    locale_script, region_preference, unicode_extension_value, weekday_u_value_to_number,
};

/// The direction of the text of a script, from CLDR's script metadata
/// (https://unicode.org/reports/tr35/tr35.html#Script_Metadata), which is what the
/// `LocaleScriptDirectionV1` marker bakes and 15.5.14 reads.
static DIRECTIONALITY: LazyLock<LocaleDirectionality> = LazyLock::new(|| {
    LocaleDirectionality::try_new_extended_unstable(&IntlDataProvider)
        // Baked data, so a failure here is a generator that left the marker out rather than
        // something a program can cause.
        .unwrap()
});

/// The regions the data of 15.5.9, 15.5.11 and 15.5.17 is read for, in the order the operations
/// name them: the override the tag asked for first, then the region the tag resolves to.
fn preferred_regions(preference: &RegionPreference) -> Vec<&str> {
    let mut regions = Vec::with_capacity(2);

    if let Some(region_override) = &preference.region_override {
        regions.push(region_override.as_str());
    }

    regions.push(preference.region.as_str());

    regions
}

/// AvailableCalendars (https://tc39.es/ecma402/#sec-availablecalendars), 15.5.9 step 8's callee and
/// the branch of `Intl.supportedValuesOf("calendar")`: the calendar types this build can format with.
/// There is one, because there is one the baked data carries a row for and one the locales of this
/// build prefer.
pub(crate) fn available_calendars() -> &'static [&'static str] {
    &["gregory"]
}

/// CalendarsOfLocale (https://tc39.es/ecma402/#sec-calendarsoflocale), 15.5.9.
pub(crate) fn calendars_of_locale(tag: &str) -> Vec<String> {
    // Step 1: the calendar the tag names, which 15.1.1 step 18 and 15.1.3 step 4 wrote into it.
    if let Some(calendar) = unicode_extension_value(tag, "ca") {
        return vec![calendar];
    }

    // Steps 2-3.
    let preference = region_preference(tag);
    let language = locale_language(tag);

    // Steps 4-6: the first preferred region with a row of its own answers.
    let mut calendars_in_use: &[&str] = &[];

    for region in preferred_regions(&preference) {
        if let Some(row) = calendar_preference_row(language, region) {
            calendars_in_use = row;
            break;
        }
    }

    // A region the table does not name prefers what the world does, which is `001`'s row.
    if calendars_in_use.is_empty() {
        calendars_in_use = calendar_preference_row(language, ROOT_REGION).unwrap_or_default();
    }

    // Steps 7-9.
    let mut list: Vec<String> = Vec::new();

    for identifier in calendars_in_use {
        // Step 9.1: CLDR's spelling of a calendar is not always the registry's, and the slots and
        // the getters report the registry's.
        let canonical = canonical_u_value("ca", identifier);

        // Step 9.2.
        if available_calendars().contains(&canonical.as_str()) && !list.contains(&canonical) {
            list.push(canonical);
        }
    }

    // Step 10: the calendar an implementation that formats dates always has.
    if list.is_empty() {
        list.push("gregory".to_owned());
    }

    list
}

/// CollationsOfLocale (https://tc39.es/ecma402/#sec-collationsoflocale), 15.5.10.
pub(crate) fn collations_of_locale(tag: &str) -> Vec<String> {
    // Step 1.
    if let Some(collation) = unicode_extension_value(tag, "co") {
        return vec![collation];
    }

    // Steps 2-4: the list step 3 reads is the Collator's [[SortLocaleData]][foundLocale]][[co]]
    // without its leading null, and this build bakes the collator's *rules* and never the
    // `collation.xml` index that would say which collation *types* a locale has. With no such table
    // to read there is no locale that answers with its own types, and every tag lands on step 4.1 -
    // the two types the spec itself names for a locale whose data is missing.
    let mut list = vec!["emoji".to_owned(), "eor".to_owned()];

    // Step 5: lexicographic code unit order, which for these ASCII names is the byte order.
    list.sort();

    list
}

/// The hour cycle a letter of CLDR's time data names, as 15.5.11 step 6.1 wants it spelled: `h` is
/// the 12-hour cycle that numbers midnight 12 and noon 12, `H` the 24-hour cycle that numbers both
/// 0, `K` the 12-hour cycle that numbers midnight 0, and `k` the 24-hour cycle that numbers midnight
/// 24 (https://unicode.org/reports/tr35/tr35-dates.html#Hour_Symbols). A row also writes `b` and `B`
/// after a letter, which are day period markers rather than hours: `hb` and `hB` are `h` again, with
/// "in the AM/PM" and "in the morning/afternoon" respectively, so they name no cycle of their own.
fn hour_cycle(letter: char) -> Option<&'static str> {
    match letter {
        'h' => Some("h12"),
        'H' => Some("h23"),
        'K' => Some("h11"),
        'k' => Some("h24"),
        _ => None,
    }
}

/// The hour cycles of a time data row, in the order the row lists them, without repeats: what
/// 15.5.11 steps 6.2 and 6.3 mean by "sorted in descending preference".
fn hour_cycles_of(data: &TimeData) -> Vec<String> {
    let mut cycles: Vec<String> = Vec::new();

    for token in data.allowed.split(' ') {
        if let Some(cycle) = token.chars().next().and_then(hour_cycle) {
            if !cycles.iter().any(|listed| listed == cycle) {
                cycles.push(cycle.to_owned());
            }
        }
    }

    cycles
}

/// HourCyclesOfLocale (https://tc39.es/ecma402/#sec-hourcyclesoflocale), 15.5.11.
pub(crate) fn hour_cycles_of_locale(tag: &str) -> Vec<String> {
    // Step 1.
    if let Some(hour_cycle) = unicode_extension_value(tag, "hc") {
        return vec![hour_cycle];
    }

    // Steps 2-3.
    let preference = region_preference(tag);
    let language = locale_language(tag);

    // Steps 4-6.
    let mut cycles: Vec<String> = Vec::new();

    for region in preferred_regions(&preference) {
        // Step 6.2 before step 6.3: the row for the whole locale first, then the region's.
        if let Some(data) = time_data_row(language, region) {
            cycles = hour_cycles_of(data);
            break;
        }
    }

    // A region the table names no row for takes the world's, which is how CLDR is read. It comes
    // after the whole walk rather than inside it because a second preferred region may have a row
    // of its own, which is what 15.5.11 step 6 reads before it settles for the root's.
    if cycles.is_empty()
        && let Some(data) = time_data_row(language, ROOT_REGION)
    {
        cycles = hour_cycles_of(data);
    }

    // Step 7.
    if cycles.is_empty() {
        cycles.push("h23".to_owned());
    }

    cycles
}

/// NumberingSystemsOfLocale (https://tc39.es/ecma402/#sec-numberingsystemsoflocale), 15.5.12.
pub(crate) fn numbering_systems_of_locale(tag: &str) -> Vec<String> {
    // Step 1.
    if let Some(numbering_system) = unicode_extension_value(tag, "nu") {
        return vec![numbering_system];
    }

    // Steps 2-4: the NumberFormat of this build numbers `en`, `da` and `it` with Latin digits, and
    // `latn` is also what step 4.1 answers for a locale it has no data for, so the two branches
    // agree here. The list is the first element of the locale's `[[nu]]`, which is the only element
    // there is.
    vec!["latn".to_owned()]
}

/// What 15.5.13 answers for a tag.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TimeZones {
    /// Steps 1-2: the tag names no region, so the operation returns undefined rather than a list.
    NoRegion,

    /// Step 3 for a region this build carries a row for: the zones of that region, sorted.
    Known(&'static [&'static str]),

    /// Step 3 for a region it carries no row for. An empty list would be the spec's answer for a
    /// region where no zone is in common use, which is not a claim this build can make about, say,
    /// Germany; 15.5.13 leaves the list implementation-defined, and this build refuses to answer
    /// for a region whose data it never generated.
    UnknownRegion(String),
}

/// TimeZonesOfLocale (https://tc39.es/ecma402/#sec-timezonesoflocale), 15.5.13.
///
/// Note that this is the one operation that reads the region of the tag and nothing else: it is
/// step 1's `GetLocaleRegion`, not the RegionPreference the other operations read, so no likely
/// subtags step in for a tag that names no region and `en` has no zones rather than America's.
pub(crate) fn time_zones_of_locale(tag: &str) -> TimeZones {
    // Steps 1-2.
    let Some(region) = locale_region(tag) else {
        return TimeZones::NoRegion;
    };

    // Step 3.
    match time_zones_row(region) {
        Some(zones) => TimeZones::Known(zones),
        None => TimeZones::UnknownRegion(region.to_owned()),
    }
}

/// TextDirectionOfLocale (https://tc39.es/ecma402/#sec-textdirectionoflocale), 15.5.14.
pub(crate) fn text_direction_of_locale(tag: &str) -> Option<&'static str> {
    // Steps 2-3: the script the tag names, or the one likely subtags give it.
    let script = locale_script(tag).map(str::to_owned).or_else(|| {
        // Step 3.1: an error from the algorithm is the undefined of steps 3.3 and 6.
        let maximal = add_likely_subtags(tag)?;

        // Steps 3.2-3.3.
        locale_script(&maximal).map(str::to_owned)
    })?;

    // Steps 4-6, over CLDR's script metadata. The identifier is the language and the script alone:
    // which script a locale is written in is what has a direction, and neither the region nor the
    // variants nor the extension changes it.
    let identifier = format!("{}-{script}", locale_language(tag))
        .parse::<LanguageIdentifier>()
        .ok()?;

    match DIRECTIONALITY.get(&identifier)? {
        Direction::LeftToRight => Some("ltr"),
        Direction::RightToLeft => Some("rtl"),

        // Note 1: a script whose direction is neither, or is not recorded.
        _ => None,
    }
}

/// The WeekInfo Record of 15.5.17, whose fields Table 27 defines.
pub(crate) struct WeekInfo {
    /// [[FirstDay]], the ISO 8601 ordinal of the day that begins the week.
    pub first_day: u8,

    /// [[Weekend]], the same ordinals of the days that are the weekend, in week order.
    pub weekend: Vec<u8>,
}

/// The days from `start` to `end` of a week, inclusive and wrapping at Sunday: how a CLDR weekend
/// pair becomes the list Table 27 asks for.
fn weekend_days(start: u8, end: u8) -> Vec<u8> {
    let mut days = Vec::with_capacity(7);
    let mut day = start;

    loop {
        days.push(day);

        if day == end {
            break;
        }

        day += 1;

        if day > 7 {
            day = 1;
        }
    }

    days
}

/// WeekInfoOfLocale (https://tc39.es/ecma402/#sec-weekinfooflocale), 15.5.17.
pub(crate) fn week_info_of_locale(tag: &str) -> WeekInfo {
    // Steps 1-6: the override's row if it has one, the region's if it has one, and `001`'s, which
    // this build always carries.
    let preference = region_preference(tag);

    let lookup_region = preferred_regions(&preference)
        .into_iter()
        .find(|region| week_data_row(region).is_some())
        .unwrap_or(ROOT_REGION);

    // The rows of `locale_info` are named by hand after CLDR, and `001` is among them.
    let data = week_data_row(lookup_region).unwrap();

    // Table 27, over the row.
    let mut first_day = day_number(data.first_day);
    let weekend = weekend_days(day_number(data.weekend_start), day_number(data.weekend_end));

    // Steps 8-9: the day the tag itself declares overrides the region's, and nothing else. A first
    // day that is not a day at all overrides nothing, which is what step 9.2 says.
    if let Some(first_day_string) = unicode_extension_value(tag, "fw")
        && let Some(number) = weekday_u_value_to_number(&first_day_string)
    {
        first_day = number;
    }

    WeekInfo { first_day, weekend }
}

/// The ISO 8601 ordinal of a day named in one of the hand-written rows: Table 26 again, the same
/// walk 15.5.16 makes. A name the table does not know is answered with Monday, which no row of
/// `locale_info` writes.
fn day_number(name: &str) -> u8 {
    weekday_u_value_to_number(name).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_calendars_of_a_locale_are_the_ones_its_region_prefers() {
        // 15.5.9 steps 2-6 over `001`'s row, the only one CLDR writes for these regions, filtered
        // through the calendars this build can format and canonicalized out of CLDR's spelling.
        assert_eq!(calendars_of_locale("en"), ["gregory"]);
        assert_eq!(calendars_of_locale("da"), ["gregory"]);
        assert_eq!(calendars_of_locale("it"), ["gregory"]);
        assert_eq!(calendars_of_locale("de"), ["gregory"]);
        assert_eq!(calendars_of_locale("und"), ["gregory"]);

        // Step 1: what the tag names is the whole answer, whatever the data prefers.
        assert_eq!(calendars_of_locale("en-u-ca-buddhist"), ["buddhist"]);
        assert_eq!(calendars_of_locale("it-IT-u-ca-coptic"), ["coptic"]);

        // The override of steps 3 and 6, which is the region the data is read for.
        assert_eq!(calendars_of_locale("en-US-u-rg-itzzzz"), ["gregory"]);
    }

    #[test]
    fn the_collations_of_a_locale_are_the_two_the_spec_names() {
        // Steps 2-4: no locale has collation *type* data in this build, so every one of them answers
        // with step 4.1's list.
        assert_eq!(collations_of_locale("en"), ["emoji", "eor"]);
        assert_eq!(collations_of_locale("da-DK"), ["emoji", "eor"]);
        assert_eq!(collations_of_locale("de"), ["emoji", "eor"]);
        assert_eq!(collations_of_locale("ar"), ["emoji", "eor"]);

        // Step 1, and the sorting of step 5.
        assert_eq!(collations_of_locale("en-u-co-phonebk"), ["phonebk"]);
        assert_eq!(collations_of_locale("de-u-co-standard"), ["standard"]);
    }

    #[test]
    fn the_hour_cycles_come_from_the_time_data_row() {
        // `US` prefers the 12-hour cycle, `DK` names only the 24-hour one, and the Italian
        // language's row adds the 12-hour cycle the region's leaves out.
        assert_eq!(hour_cycles_of_locale("en"), ["h12", "h23"]);
        assert_eq!(hour_cycles_of_locale("en-US"), ["h12", "h23"]);
        assert_eq!(hour_cycles_of_locale("da"), ["h23"]);
        assert_eq!(hour_cycles_of_locale("da-DK"), ["h23"]);
        assert_eq!(hour_cycles_of_locale("it"), ["h23", "h12"]);
        assert_eq!(hour_cycles_of_locale("it-IT"), ["h23", "h12"]);

        // The English of the world, whose row CLDR keys by locale rather than by region.
        assert_eq!(hour_cycles_of_locale("en-001"), ["h12", "h23"]);

        // A region with no row inherits `001`, which allows the 24-hour cycle first and the 12-hour
        // cycle after it.
        assert_eq!(hour_cycles_of_locale("de"), ["h23", "h12"]);

        // `und` is not the root: RegionPreference (15.5.8) maximizes it first, and the tables expand
        // the bare `und` row to `en-Latn-US`, so it is read as America and answers the 12-hour cycle
        // first - the same row `en-US` gives.
        assert_eq!(hour_cycles_of_locale("und"), ["h12", "h23"]);

        // Step 1.
        assert_eq!(hour_cycles_of_locale("en-u-hc-h11"), ["h11"]);

        // Steps 3 and 6: the override is the region the data is read for.
        assert_eq!(hour_cycles_of_locale("en-US-u-rg-dkzzzz"), ["h23"]);
        assert_eq!(hour_cycles_of_locale("da-DK-u-rg-uszzzz"), ["h12", "h23"]);
    }

    #[test]
    fn the_numbering_systems_are_the_ones_the_data_numbers_with() {
        assert_eq!(numbering_systems_of_locale("en"), ["latn"]);
        assert_eq!(numbering_systems_of_locale("it-IT"), ["latn"]);
        assert_eq!(numbering_systems_of_locale("de"), ["latn"]);

        // Step 1.
        assert_eq!(numbering_systems_of_locale("en-u-nu-arab"), ["arab"]);
    }

    #[test]
    fn the_zones_are_only_the_regions_the_data_names() {
        // Steps 1-2: `en` names no region, so there is nothing to return.
        assert_eq!(time_zones_of_locale("en"), TimeZones::NoRegion);
        assert_eq!(time_zones_of_locale("da"), TimeZones::NoRegion);
        assert_eq!(time_zones_of_locale("it"), TimeZones::NoRegion);

        assert_eq!(time_zones_of_locale("da-DK"), TimeZones::Known(&["Europe/Copenhagen"]));
        assert_eq!(time_zones_of_locale("it-IT"), TimeZones::Known(&["Europe/Rome"]));

        // The 29 zones of `US`, in the order step 3 sorts them.
        let TimeZones::Known(zones) = time_zones_of_locale("en-US") else {
            panic!("the United States has zones in the data");
        };
        assert_eq!(zones.len(), 29);
        assert_eq!(zones.first().copied(), Some("America/Adak"));
        assert_eq!(zones.last().copied(), Some("Pacific/Honolulu"));

        let mut sorted = zones.to_vec();
        sorted.sort();
        assert_eq!(sorted, zones);

        // A region the data carries no row for is refused rather than guessed at.
        assert_eq!(time_zones_of_locale("de-DE"), TimeZones::UnknownRegion("DE".to_owned()));

        // The region of the tag alone: no likely subtags, and no `rg` override.
        assert_eq!(time_zones_of_locale("en-GB"), TimeZones::UnknownRegion("GB".to_owned()));
    }

    #[test]
    fn the_direction_is_the_scripts() {
        assert_eq!(text_direction_of_locale("en"), Some("ltr"));
        assert_eq!(text_direction_of_locale("da"), Some("ltr"));
        assert_eq!(text_direction_of_locale("it"), Some("ltr"));
        assert_eq!(text_direction_of_locale("de"), Some("ltr"));

        // Steps 2-3: the script of the tag wins, and a tag that names none is maximized first.
        assert_eq!(text_direction_of_locale("en-Latn"), Some("ltr"));
        assert_eq!(text_direction_of_locale("und-Arab"), Some("rtl"));
        assert_eq!(text_direction_of_locale("ar"), Some("rtl"));
        assert_eq!(text_direction_of_locale("he"), Some("rtl"));

        // Note 1 of 15.5.14 is about a language the tables give no script for. `und` has one: the
        // `und` row of the likely-subtags table is `en-Latn-US`, so step 3's maximization supplies
        // Latn and the direction is left-to-right, which is what 15.3.21 answers for it.
        assert_eq!(text_direction_of_locale("und"), Some("ltr"));
        assert_eq!(text_direction_of_locale("und-Zzzz"), None);

        // The extension takes no part in it.
        assert_eq!(text_direction_of_locale("en-US-u-ca-buddhist"), Some("ltr"));
    }

    #[test]
    fn the_week_of_a_locale_begins_where_its_region_says() {
        // `US` begins on Sunday; `001`, which `DK` and `IT` agree with, on Monday.
        assert_eq!(week_info_of_locale("en-US").first_day, 7);
        assert_eq!(week_info_of_locale("en").first_day, 7);
        assert_eq!(week_info_of_locale("da-DK").first_day, 1);
        assert_eq!(week_info_of_locale("it-IT").first_day, 1);
        assert_eq!(week_info_of_locale("de-DE").first_day, 1);

        // Steps 4-6: the override is the region the row is read for.
        assert_eq!(week_info_of_locale("en-US-u-rg-dkzzzz").first_day, 1);
        assert_eq!(week_info_of_locale("da-DK-u-rg-uszzzz").first_day, 7);

        // Table 27's weekend, from `001`.
        assert_eq!(week_info_of_locale("en-US").weekend, [6, 7]);
        assert_eq!(week_info_of_locale("da").weekend, [6, 7]);

        // Steps 8-9: the day the tag declares overrides the region's, and is the whole weekend
        // only because CLDR's pair still says which days are the weekend.
        assert_eq!(week_info_of_locale("en-US-u-fw-mon").first_day, 1);
        assert_eq!(week_info_of_locale("en-US-u-fw-mon").weekend, [6, 7]);
        assert_eq!(week_info_of_locale("en-US-u-fw-7").first_day, 7);

        // A first day that is not a day leaves the row alone (15.5.16 step 2).
        assert_eq!(week_info_of_locale("en-US-u-fw-fredag").first_day, 7);
    }

    #[test]
    fn a_weekend_wraps_over_the_end_of_the_week() {
        assert_eq!(weekend_days(6, 7), [6, 7]);
        assert_eq!(weekend_days(1, 1), [1]);
        assert_eq!(weekend_days(6, 2), [6, 7, 1, 2]);
        assert_eq!(weekend_days(7, 6), [7, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn a_time_data_row_reads_as_its_hour_cycles() {
        let united_states = time_data_row("en", "US").unwrap();
        assert_eq!(united_states.preferred, "h");

        // `hb` and `hB` are day period markers, so the row names two cycles rather than four.
        assert_eq!(hour_cycles_of(united_states), ["h12", "h23"]);
        assert_eq!(hour_cycle('b'), None);
        assert_eq!(hour_cycle('B'), None);
    }
}
