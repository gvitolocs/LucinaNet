//! The CLDR rows that `Intl.Locale`'s Locale Info getters read, for the regions this build's
//! locales resolve to.
//!
//! Unlike the rest of this crate, this module is not generated: `icu/generate_intl_data.sh` bakes
//! what the ICU4X 2.3 data markers can express, and there is no marker for time data, for calendar
//! preferences, for week elements or for the zones in common use in a region
//! (https://unicode.org/reports/tr35/tr35-dates.html#Time_Data,
//! https://unicode.org/reports/tr35/tr35-dates.html#Calendar_Preference_Data,
//! https://unicode.org/reports/tr35/tr35-dates.html#Date_Patterns_Week_Elements). These are the
//! rows of those tables that ECMA-402 15.5.9, 15.5.11, 15.5.13 and 15.5.17 ask for, copied out of
//! the same CLDR release the generator uses - 48.1.0 - for the four regions that matters here:
//! `001` is the world row the spec falls back to, and `US`, `DK` and `IT` are the regions `en`,
//! `da` and `it` resolve to. A region with no row is not claimed to be supported: `getHourCycles`
//! and `getCalendars` answer from the `001` row, which is what CLDR intends for every region it
//! does not list, and `getTimeZones` answers with nothing at all.
//!
//! The zone list comes from the IANA time zone database's own `zone.tab`, which is the source CLDR
//! reads, and is the set of zones whose principal country is the region.

/// The hour cycles CLDR's time data names for a region, in its own descending preference order.
///
/// The letters are the ones `timeData.xml` writes (`_preferred` and `_allowed`), and they are kept
/// as letters rather than turned into cycles here so that the row can be read against CLDR.
/// `h` is the 12-hour cycle `h12`, `H` the 24-hour cycle `h23`, `K` `h11` and `k` `h24`; `hb` and
/// `hB` name a *day period* rather than a cycle of their own - they pair `h` or `K`, and `H` or `k`,
/// with AM/PM or with a day-period marker - so they add no cycle to the list.
pub struct TimeData {
    /// CLDR's `_preferred` letter.
    pub preferred: &'static str,

    /// CLDR's `_allowed` letters, in the order it writes them.
    pub allowed: &'static str,
}

/// The rows of CLDR 48.1.0 `timeData.json`, for the regions named in
/// [`crate::BUILT_IN_LOCALES`] and for `001`.
pub const TIME_DATA: &[(&str, TimeData)] = &[
    ("001", TimeData { preferred: "H", allowed: "H h" }),
    ("DK", TimeData { preferred: "H", allowed: "H" }),
    ("IT", TimeData { preferred: "H", allowed: "H hB" }),
    ("US", TimeData { preferred: "h", allowed: "h hb H hB" }),
];

/// The rows of the same table that CLDR keys by whole locale rather than by region, which
/// ECMA-402 15.5.11 step 6.2 reads before the region row of step 6.3. `en-001` is the English of
/// the world (`en` with no region resolves to `US`, but `en-001` is a tag a program can ask for),
/// and `it-IT` and `it-CH` are the two Italian rows that differ from their regions': Italy's own row
/// allows `H hB` while the language's adds `h`, so `getHourCycles` for `it` reports `h12` as well.
pub const TIME_DATA_LOCALES: &[(&str, TimeData)] = &[
    ("en-001", TimeData { preferred: "h", allowed: "h hb H hB" }),
    ("it-CH", TimeData { preferred: "H", allowed: "H h hB" }),
    ("it-IT", TimeData { preferred: "H", allowed: "H h hB" }),
];

/// The rows of CLDR 48.1.0 `calendarPreferenceData.xml`: the calendars in use in a region, most
/// preferred first. CLDR keys this table by region and, for a handful of languages, by whole locale;
/// the four rows here are the region-keyed ones, and all four name the Gregorian calendar alone -
/// which is what `getCalendars` answers for every locale of this build, and the reason
/// [`crate::locale_info::CALENDAR_PREFERENCE`] carries a list rather than a single name: a region
/// such as `AE` or `IR` does have a second entry, and this is the shape that would hold it.
pub const CALENDAR_PREFERENCE: &[(&str, &[&str])] = &[
    ("001", &["gregorian"]),
    ("DK", &["gregorian"]),
    ("IT", &["gregorian"]),
    ("US", &["gregorian"]),
];

/// The rows of CLDR 48.1.0 `weekData.json`: which day begins the week, and which days are the
/// weekend. `001` carries the weekend, which is why the other three rows only change the first day:
/// CLDR 48 names `sat`-`sun` as the weekend for every region but a handful in the Gulf and North
/// Africa, none of which is one of this build's locales.
pub struct WeekData {
    /// CLDR's `firstDay`, spelled as the `fw` key spells it.
    pub first_day: &'static str,

    /// CLDR's `weekendStart`.
    pub weekend_start: &'static str,

    /// CLDR's `weekendEnd`.
    pub weekend_end: &'static str,
}

pub const WEEK_DATA: &[(&str, WeekData)] = &[
    ("001", WeekData { first_day: "mon", weekend_start: "sat", weekend_end: "sun" }),
    ("DK", WeekData { first_day: "mon", weekend_start: "sat", weekend_end: "sun" }),
    ("IT", WeekData { first_day: "mon", weekend_start: "sat", weekend_end: "sun" }),
    ("US", WeekData { first_day: "sun", weekend_start: "sat", weekend_end: "sun" }),
];

/// The zones in common use in a region, sorted: what 15.5.13 step 3 asks for.
pub const TIME_ZONES: &[(&str, &[&str])] = &[
    ("DK", &["Europe/Copenhagen"]),
    ("IT", &["Europe/Rome"]),
    (
        "US",
        &[
            "America/Adak",
            "America/Anchorage",
            "America/Boise",
            "America/Chicago",
            "America/Denver",
            "America/Detroit",
            "America/Indiana/Indianapolis",
            "America/Indiana/Knox",
            "America/Indiana/Marengo",
            "America/Indiana/Petersburg",
            "America/Indiana/Tell_City",
            "America/Indiana/Vevay",
            "America/Indiana/Vincennes",
            "America/Indiana/Winamac",
            "America/Juneau",
            "America/Kentucky/Louisville",
            "America/Kentucky/Monticello",
            "America/Los_Angeles",
            "America/Menominee",
            "America/Metlakatla",
            "America/New_York",
            "America/Nome",
            "America/North_Dakota/Beulah",
            "America/North_Dakota/Center",
            "America/North_Dakota/New_Salem",
            "America/Phoenix",
            "America/Sitka",
            "America/Yakutat",
            "Pacific/Honolulu",
        ],
    ),
];

/// The row of one of the tables above, `None` when this build has none for the key.
fn row<'a, T>(table: &'a [(&'a str, T)], region: &str) -> Option<&'a T> {
    let mut index = 0;

    while index < table.len() {
        if table[index].0 == region {
            return Some(&table[index].1);
        }

        index += 1;
    }

    None
}

/// CLDR's own root, the row every table falls back to when a region is not listed. The Locale Info
/// operations read it last, after the region of the tag: see the module documentation.
pub const ROOT_REGION: &str = "001";

/// The time data row ECMA-402 15.5.11 steps 6.2 and 6.3 ask for: the row CLDR keys by the whole
/// locale the language and region make, or the one it keys by region, or `None` when it has
/// neither.
pub fn time_data_row(language: &str, region: &str) -> Option<&'static TimeData> {
    // Step 6.1, the concatenation of the language and the region.
    let locale = format!("{language}-{region}");

    row(TIME_DATA_LOCALES, &locale).or_else(|| row(TIME_DATA, region))
}

/// The calendar preference row of 15.5.9 steps 4.2 and 4.3. CLDR keys most of that table by region
/// and a few rows by whole locale, so the same two lookups read it.
pub fn calendar_preference_row(language: &str, region: &str) -> Option<&'static [&'static str]> {
    let locale = format!("{language}-{region}");

    // The table holds a slice, so the row of it is a reference to that reference.
    row(CALENDAR_PREFERENCE, &locale)
        .or_else(|| row(CALENDAR_PREFERENCE, region))
        .copied()
}

/// The week data row of 15.5.17 steps 4 and 5. CLDR 48 keys `firstDay` by region alone, so there is
/// no locale row to read first here as there is for the time data.
pub fn week_data_row(region: &str) -> Option<&'static WeekData> {
    row(WEEK_DATA, region)
}

/// The zones of a region, or `None` when this build carries no row for it. There is no fallback
/// here: unlike the hour cycles and the week, the zones in common use in `DE` are not the ones in
/// common use in the world, and `001`'s row is a list of every zone CLDR knows rather than an
/// answer about Germany.
pub fn time_zones_row(region: &str) -> Option<&'static [&'static str]> {
    row(TIME_ZONES, region).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rows_cover_the_build_and_the_world() {
        for region in [ROOT_REGION, "US", "DK", "IT"] {
            assert!(time_data_row("en", region).is_some(), "no time data for {region}");
            assert!(week_data_row(region).is_some(), "no week data for {region}");
        }

        assert_eq!(calendar_preference_row("en", ROOT_REGION).unwrap(), &["gregorian"][..]);

        // A region the build says nothing about has no row of its own, which is what makes the
        // caller read `001` instead: the world's hour cycles, and no zones.
        assert!(time_data_row("de", "DE").is_none());
        assert!(week_data_row("DE").is_none());
        assert!(calendar_preference_row("de", "DE").is_none());
        assert!(time_zones_row("DE").is_none());
        assert!(time_zones_row(ROOT_REGION).is_none());
    }

    #[test]
    fn a_locale_row_is_read_before_the_region_row() {
        // 15.5.11 step 6.2 before step 6.3: Italy's own region row allows `H hB`, the Italian
        // language's adds `h`.
        assert_eq!(time_data_row("it", "IT").unwrap().allowed, "H h hB");
        assert_eq!(time_data_row("it", "CH").unwrap().allowed, "H h hB");
        assert_eq!(row(TIME_DATA, "IT").unwrap().allowed, "H hB");

        // A region with no locale row of its own falls through to the region's.
        assert_eq!(time_data_row("da", "DK").unwrap().allowed, "H");

        // `en-001` is the English of the world, and the row CLDR keys by locale rather than region.
        assert_eq!(time_data_row("en", ROOT_REGION).unwrap().preferred, "h");
        assert_eq!(row(TIME_DATA_LOCALES, "en-001").unwrap().preferred, "h");
    }

    #[test]
    fn the_zone_lists_are_sorted_as_the_spec_wants_them() {
        for (region, zones) in TIME_ZONES {
            let mut sorted = zones.to_vec();
            sorted.sort_unstable();

            assert_eq!(sorted, zones.to_vec(), "the zones of {region} are not sorted");
            assert!(!zones.is_empty(), "the zones of {region} are empty");

            let mut unique = sorted.clone();
            unique.dedup();
            assert_eq!(unique, sorted, "{region} lists a zone twice");
        }
    }
}
