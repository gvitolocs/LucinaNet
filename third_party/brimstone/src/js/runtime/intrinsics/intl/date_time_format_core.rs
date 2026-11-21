//! The parts of `Intl.DateTimeFormat` that are about dates rather than about JavaScript: the table
//! that says which ICU4X field set an ECMA-402 option bag asks for, the rendering of a moment
//! through that field set and its parts, the arithmetic that makes a moment civil in a zone, and the
//! zone spine that turns a `timeZone` name into the canonical identifier and the offset that name
//! had at the moment being formatted.
//!
//! The instance and the reading of its options are [`super::date_time_format_object`]; what a
//! program calls is [`super::date_time_format_prototype`] and
//! [`super::date_time_format_constructor`].
//!
//! The shape of the option-to-field-set table follows Boa's
//! `core/engine/src/builtins/intl/date_time_format/options.rs` (boa-dev/boa, MIT OR Unlicense),
//! which the design review names as the worked example to adapt. Its parts collector and its
//! hardcoded UTC default time zone are not taken, as the review says: the collector below is what
//! makes ECMA-402's named records come out of `writeable`, and the zone is the host's own, read
//! through the `timezone_provider` database the fork already ships for Temporal.

use brimstone_intl_data::{IntlDataProvider, TIME_ZONE_NAMES_ARE_BAKED};
use icu_calendar::{Date, Gregorian};
use icu_datetime::DateTimeFormatterPreferences;
use icu_datetime::fieldsets::builder::{DateFields, FieldSetBuilder, ZoneStyle};
use icu_datetime::fieldsets::enums::CompositeFieldSet;
use icu_datetime::input::{DateTime, TimeZoneInfo, ZonedDateTime};
use icu_datetime::options::{Length, SubsecondDigits, TimePrecision, YearStyle};
use icu_datetime::parts as datetime_parts;
use icu_datetime::pattern::{DateTimePattern, FixedCalendarDateTimeNames};
use icu_datetime::provider::fields::{Field, FieldLength, FieldSymbol};
use icu_datetime::provider::pattern::PatternItem;
use icu_datetime::provider::pattern::runtime::Pattern as RuntimePattern;
use icu_decimal::parts as decimal_parts;
use icu_locale::Locale;
use icu_locale::preferences::extensions::unicode::keywords::HourCycle as IcuHourCycle;
use icu_time::Time;
use icu_time::TimeZone;
use icu_time::zone::UtcOffset;
use icu_time::zone::models::AtTime;
use timezone_provider::provider::{TimeZoneId, TimeZoneProvider};
use writeable::{Part, PartsWrite, TryWriteable, Writeable};
use zerovec::ZeroVec;

use super::cache::DataLocale;
use crate::runtime::Context;

/// Which zone a [`FormatZone`] is, in the form the zone database and 11.5.3 name it in.
///
/// The three cases are the three spellings `resolvedOptions().timeZone` answers with, and the id of
/// the database rather than a String is what makes this `Copy`: a DateTimeFormat instance lives in
/// the GC heap, which holds nothing that can drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneId {
    /// An IANA zone, by the database's own id of it, which is what [`FormatZone::name`] writes the
    /// canonical spelling back out of and what the offset of a moment is looked up by.
    Iana(TimeZoneId),
    /// `UTC` (and `GMT`, which 11.5.1 step 19.3 canonicalizes to it), which the database has no id
    /// for and whose offset is zero at every moment.
    Utc,
    /// An ECMA-402 offset zone of 11.5.3, whose name is the `+HH:mm` form of its offset and whose
    /// CLDR subtag is the unknown one.
    Offset,
}

/// The zone of the moment a format renders: what `timeZone` named, the offset that name had at that
/// moment, and the BCP-47 subtag the `timeZoneName` field is looked up with.
///
/// The id is the canonical IANA spelling when the option named a zone and the ECMA-402 offset
/// spelling when it was an offset, which is what `resolvedOptions().timeZone` reports; the offset is
/// the one that applies at the moment, which is what makes the civil arithmetic of
/// [`civil_date_time`] and the `timeZoneName` field agree with each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatZone {
    id: ZoneId,
    pub offset_seconds: i32,
    /// CLDR keys its zone names by an eight-letter BCP-47 time-zone subtag rather than by the IANA
    /// id, and the subtag of an offset zone is the unknown one, which is what makes ICU4X render the
    /// offset of such a zone rather than a name for it.
    subtag: TimeZone,
}

impl FormatZone {
    /// 11.5.3's `CanonicalizeTimeZone`, which is the RangeError of its step 2 for a name that names
    /// nothing: `Mars/Olympus`, `GMT Standard Time`, and every other id the IANA database does not
    /// have.
    ///
    /// The case-insensitive lookup and the alias table are those of the zone database the fork
    /// already ships for Temporal, so `america/new_york` and `US/Eastern` come out as
    /// `America/New_York` the way they do in V8; an ECMA-402 offset form is canonicalized by
    /// [`FormatZone::from_offset_seconds`] instead, which is 11.5.3's other step.
    pub fn canonicalize(cx: Context, name: &str) -> Result<Self, String> {
        // `GMT` is the id that canonicalizes to `UTC` rather than to itself, which is what V8 and the
        // `[[PrimaryIdentifier]]` of 11.5.1 step 19.3 do. `Z` is deliberately not here: it is neither
        // an offset form of 11.5.1 step 18 nor an id the IANA database has, so it is the RangeError
        // of step 19.2.
        let utc = name.eq_ignore_ascii_case("UTC") || name.eq_ignore_ascii_case("GMT");

        if !utc {
            if let Some(zone) = parse_offset(name) {
                // Step 18.3: an offset whose spelling reaches the seconds is refused rather than
                // rounded down, because `FormatOffsetTimeZoneIdentifier` has no form to write it back
                // in. `+15:59:00` is the test262 case: a zero of its own is still a seconds field.
                if zone.seconds_written {
                    return Err(format!("Invalid time zone specified: {name}"));
                }

                return Ok(zone.zone);
            }
        }

        let name = if utc { "UTC" } else { name };

        // The provider's own record of the name is the canonical one: `get` normalizes the spelling
        // and follows the aliases, and the id it answers with is what [`FormatZone::name`] writes the
        // canonical spelling back out of and what the offset of a moment is looked up by.
        let provider = cx.temporal_provider();
        let invalid = || format!("Invalid time zone specified: {name}");
        let id = provider.get(name.as_bytes()).map_err(|_| invalid())?;
        let canonical = provider.identifier(id).map_err(|_| invalid())?.into_owned();

        // The database spells a few of its ids as zones of the alias rather than of UTC itself, and
        // 11.5.3's `CanonicalizeTimeZoneName` answers `UTC` for all of them; the offset of each is
        // zero at every moment, so the zone is the `ZoneId::Utc` the name says it is.
        let utc = utc || is_utc_alias(&canonical);

        Ok(Self {
            id: if utc { ZoneId::Utc } else { ZoneId::Iana(id) },
            // The offset is the one of the moment being formatted, which the caller does not have
            // yet: this is the id alone, and [`FormatZone::with_offset_at`] is what fills it in
            // before anything renders. Zero stands in until it does.
            offset_seconds: 0,
            subtag: cx
                .locale_cache()
                .iana_parser()
                .as_borrowed()
                .parse(&canonical),
        })
    }

    /// The canonical name 11.5.3 answers with, which is what 11.3.2's Table 15 reports as
    /// `timeZone`: the database's own spelling of an IANA id, `UTC`, and the `+HH:mm` of an offset.
    pub fn name(self, cx: Context) -> Result<String, String> {
        match self.id {
            ZoneId::Iana(id) => cx
                .temporal_provider()
                .identifier(id)
                .map(|name| name.into_owned())
                .map_err(|_| format!("Invalid time zone specified: {id:?}")),
            ZoneId::Utc => Ok("UTC".to_owned()),
            ZoneId::Offset => Ok(offset_name(self.offset_seconds)),
        }
    }

    /// The same id with the offset of one moment filled in, which is what makes the civil arithmetic
    /// of [`civil_date_time`] and the `timeZoneName` field agree about which instant is being
    /// rendered. An id the zone database does not have is refused the way 11.5.3 refuses it.
    pub fn with_offset_at(self, cx: Context, epoch_milliseconds: f64) -> Result<Self, String> {
        let offset_seconds = match self.id {
            ZoneId::Iana(id) => FormatZone::offset_of(cx, id, epoch_milliseconds)?,
            ZoneId::Utc => 0,
            // An offset zone has one offset at every moment, which `from_offset_seconds` filled in.
            ZoneId::Offset => self.offset_seconds,
        };

        Ok(Self { offset_seconds, ..self })
    }

    /// [`FormatZone::with_offset_at`] of the epoch nanoseconds of 15.6.14's Value Format Record, which
    /// is the moment an instant of 15.6.20 renders at. The offset a zone had is a property of the
    /// nanosecond rather than of the millisecond, but the two agree for every moment a `Date` can
    /// hold and for every instant Temporal reaches: the database answers in nanoseconds either way.
    pub fn with_offset_at_nanoseconds(
        self,
        cx: Context,
        epoch_nanoseconds: i128,
    ) -> Result<Self, String> {
        let offset_seconds = match self.id {
            ZoneId::Iana(id) => FormatZone::offset_of_nanoseconds(cx, id, epoch_nanoseconds)?,
            ZoneId::Utc => 0,
            ZoneId::Offset => self.offset_seconds,
        };

        Ok(Self { offset_seconds, ..self })
    }

    /// The offset the zone named by `id` had at a moment, which is 11.5.10's `GetNamedZoneOffset`
    /// over the zone database.
    pub fn offset_of(cx: Context, id: TimeZoneId, epoch_milliseconds: f64) -> Result<i32, String> {
        let error = || format!("Invalid time zone specified: {id:?}");

        FormatZone::offset_of_nanoseconds(
            cx,
            id,
            epoch_nanoseconds_of(epoch_milliseconds).ok_or_else(error)?,
        )
    }

    /// The same lookup in the unit 15.6.1's `GetNamedZoneOffsetNanoseconds` is specified in, which is
    /// what an instant of 15.6.20 has rather than a time value.
    pub fn offset_of_nanoseconds(
        cx: Context,
        id: TimeZoneId,
        epoch_nanoseconds: i128,
    ) -> Result<i32, String> {
        let error = || format!("Invalid time zone specified: {id:?}");

        let provider = cx.temporal_provider();

        // The database answers in nanoseconds since the epoch, which is the unit both this and
        // 11.5.10's `GetNamedZoneOffset` are specified in; a JavaScript time value reaches it through
        // [`epoch_nanoseconds_of`], whose milliseconds are the same count in the unit `Date` keeps.
        let offset = provider
            .transition_nanoseconds_for_utc_epoch_nanoseconds(id, epoch_nanoseconds)
            .map_err(|_| error())?;

        i32::try_from(offset.0).map_err(|_| error())
    }

    /// 11.5.3's canonicalization of an ECMA-402 offset: `-0800` and `-08:00` and `-8` are the same
    /// zone, written the way 11.1.3's `FormatOffsetTimeZoneIdentifier` writes it - the sign, two
    /// digits of hour, a colon, two of minute - which is also what `resolvedOptions().timeZone`
    /// reports for one. The seconds an offset string may spell are not part of that form, and
    /// [`parse_offset`] refuses the strings that have them before this is reached.
    ///
    /// The offset is kept as seconds rather than as an ICU4X `UtcOffset`, which refuses more than
    /// ±18 h while 11.5.3's grammar allows ±23:59. See [`FormatZone::offset_is_renderable`] for the
    /// one field that cannot cope with the wider range.
    pub fn from_offset_seconds(offset_seconds: i32) -> Self {
        Self {
            id: ZoneId::Offset,
            offset_seconds,
            // An offset zone has no CLDR name to look a `timeZoneName` field up with, which is what
            // makes such a field come out as the offset it is.
            subtag: TimeZone::UNKNOWN,
        }
    }

    /// Whether ICU4X can be handed this zone, which its `UtcOffset` refuses for an offset of more
    /// than ±18 h. The civil parts do not care - they are ours - but a `timeZoneName` field does.
    pub fn offset_is_renderable(&self) -> bool {
        UtcOffset::try_from_seconds(self.offset_seconds).is_ok()
    }

    /// The zone of the ICU4X input of a moment: the id, if there is one, and always the offset.
    ///
    /// The offset is given rather than looked up again, so that the civil parts of
    /// [`civil_date_time`] and the zone field of the rendering cannot disagree about which moment is
    /// being rendered.
    fn to_icu(self, date_time: DateTime<Gregorian>) -> TimeZoneInfo<AtTime> {
        self.to_icu_named(date_time)
    }

    /// The same zone, but with its `timeZoneName` field read at another moment than the one whose
    /// civil parts are being rendered: the civil parts of a [`ZonedDateTime`] come from its own date
    /// and time, and this moment only picks the instant the name is looked up at.
    ///
    /// ICU4X compares the two inputs of a range before it will select a range pattern at all, and one
    /// of the three things it compares is this lookup moment - so two ends of one range in one zone
    /// that has not changed its offset between them, which have one name to look up, would count as
    /// two different zones and fall back to both ends spelled out. See [`FormatZone::same_zone_as`].
    fn to_icu_named(self, name_time: DateTime<Gregorian>) -> TimeZoneInfo<AtTime> {
        let offset = UtcOffset::try_from_seconds(self.offset_seconds).ok();

        self.subtag.with_offset(offset).at_date_time(name_time)
    }

    /// Whether one zone has the name of another at every moment between them, which is what makes the
    /// two ends of a range one zone for the range pattern of 11.5.8.
    fn same_zone_as(&self, other: &Self) -> bool {
        self.id == other.id && self.offset_seconds == other.offset_seconds
    }
}

/// The `FormatOffsetTimeZoneIdentifier` of 11.1.3 that names an offset zone: the sign, two digits of
/// hour, a colon, two of minute. Seconds never appear in it, which is why [`parse_offset`] refuses an
/// offset that spells them.
fn offset_name(offset_seconds: i32) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let hours = (offset_seconds / 3_600).abs();
    let minutes = (offset_seconds / 60).abs() % 60;

    format!("{sign}{hours:02}:{minutes:02}")
}

/// The ids of the IANA database that name a zone of no offset by anything but `UTC`, and that
/// 11.5.3's `CanonicalizeTimeZoneName` therefore answers as `UTC`.
fn is_utc_alias(name: &str) -> bool {
    name.eq_ignore_ascii_case("Etc/UTC")
        || name.eq_ignore_ascii_case("Etc/GMT")
        || name.eq_ignore_ascii_case("Etc/Greenwich")
        || name.eq_ignore_ascii_case("GMT0")
        || name.eq_ignore_ascii_case("UCT")
        || name.eq_ignore_ascii_case("Zulu")
}

/// The moment of a JavaScript time value in the nanoseconds since the epoch that the zone database
/// counts in. A time value that is not finite has no moment: that is the `Invalid Date` a format
/// answers before it ever reaches a zone, so this says `None` for it.
fn epoch_nanoseconds_of(epoch_milliseconds: f64) -> Option<i128> {
    epoch_milliseconds
        .is_finite()
        .then(|| (epoch_milliseconds / 1_000.0).floor() as i128 * 1_000_000_000)
}

/// The hour cycle a built-in locale counts its hours by when the bag said nothing, which is 11.1.2
/// step 14's `resolvedLocaleData.[[HourCycle]]` of 9.2.7.
///
/// CLDR keeps that answer in the `hours` preference data, which this build's payload does not carry,
/// so reading it off a rendering would answer with the root's 12-hour cycle for every locale. The
/// three locales with data are therefore named here, and an unknown language is left to
/// [`DateTimeRenderer`]'s measurement of its own rendering rather than to a made-up default.
pub fn hour_cycle_of_locale(locale: DataLocale) -> Option<HourCycle> {
    Some(match locale.tag() {
        "en" => HourCycle::H12,
        "da" | "it" => HourCycle::H23,
        _ => return None,
    })
}

/// The zone of the host, which is the `DefaultTimeZone` of 11.5.1 that `resolvedOptions().timeZone`
/// answers with when no `timeZone` was given and that every format without that option renders in.
///
/// The fork has nowhere else to read it from: Temporal's `get_host_time_zone` and `Date`'s local
/// time are both still UTC placeholders, and the crate that would ask the operating system is not a
/// dependency of this build. So the host is asked the way a host is asked - the `TZ` environment
/// variable, then the `/etc/timezone` file Debian writes, then the `/etc/localtime` symlink every
/// distribution points at a zone file - and each answer is canonicalized through the same database
/// any other name goes through, so an offset in `TZ` stays an offset. A host that names a zone this
/// build's database does not have is a host in UTC, which is 11.5.1's answer for an embedder with
/// no zone of its own and the same clock `Date` keeps here.
pub fn host_zone(cx: Context) -> FormatZone {
    let name = std::env::var("TZ")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| read_first_line("/etc/timezone"))
        .or_else(|| {
            std::fs::read_link("/etc/localtime")
                .ok()
                .and_then(|target| {
                    let target = target.to_string_lossy().into_owned();

                    // The symlink names the zone file the distribution keeps under `zoneinfo/`, and the
                    // part after that directory is the id.
                    target
                        .split_once("/zoneinfo/")
                        .map(|(_, id)| id.to_string())
                        .filter(|value| !value.is_empty())
                })
        })
        .unwrap_or_else(|| "UTC".to_string());

    FormatZone::canonicalize(cx, &name)
        .or_else(|_| FormatZone::canonicalize(cx, "UTC"))
        .unwrap_or_else(|invalid| {
            // The database that answers a host's own zone answers this one too, or the build has
            // no zone data at all and nothing in Intl would work.
            panic!("the zone database does not know UTC: {invalid}")
        })
}

/// The first non-empty line of a file, or `None` for a file that is not there or says nothing.
fn read_first_line(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// An offset zone of 11.5.3 and the spelling it was written in: `seconds_written` says the string
/// carried a seconds field, which 11.5.3 refuses because `FormatOffsetTimeZoneIdentifier` has no form
/// to write it back in, even when the field is a zero.
struct ParsedOffset {
    zone: FormatZone,
    seconds_written: bool,
}

/// The offset of an ECMA-402 time zone string, 11.5.3's grammar, or `None` for anything that is not
/// one.
///
/// The forms the spec allows are `[+-]HH`, `[+-]HHmm`, `[+-]HH:mm`, and the two that carry seconds -
/// `[+-]HHmmss`, `[+-]HH:mm:ss` - which is what the `UTC` offset of a moment with a sub-minute
/// difference is written as. `Z` and the `GMT` and `UTC` ids are not offset forms and are
/// canonicalized as the ids they are. The whole of the parsing is here rather than in the option
/// reading because 11.5.3 says a string that is neither an offset nor a known id is the RangeError.
fn parse_offset(name: &str) -> Option<ParsedOffset> {
    // The sign is peeled rather than split off at a byte index: 11.5.3's grammar is ASCII, but a
    // program may hand any string to it, and the first byte of a non-ASCII one is not a boundary.
    let (sign, rest) = match name.as_bytes().first() {
        Some(b'+') => (1, &name[1..]),
        Some(b'-') => (-1, &name[1..]),
        _ => return None,
    };

    // 11.5.3's grammar is all ASCII and a program may hand any string to it, so the fields are read
    // from bytes rather than from string slices: a byte index in a non-ASCII string is not a
    // character boundary.
    let bytes = rest.as_bytes();

    let (shape, seconds_written) = match bytes {
        // The shapes of 11.5.3, with and without the colon, and with and without seconds.
        hh if hh.len() == 2 => ([offset_digits(hh), Some(0), Some(0)], false),
        hmm if hmm.len() == 4 && hmm[2] != b':' => {
            ([offset_digits(&hmm[..2]), offset_digits(&hmm[2..]), Some(0)], false)
        }
        hmm if hmm.len() == 5 && hmm[2] == b':' => {
            ([offset_digits(&hmm[..2]), offset_digits(&hmm[3..]), Some(0)], false)
        }
        hmm if hmm.len() == 6 && hmm[2] != b':' => (
            [
                offset_digits(&hmm[..2]),
                offset_digits(&hmm[2..4]),
                offset_digits(&hmm[4..]),
            ],
            true,
        ),
        hmm if hmm.len() == 8 && hmm[2] == b':' && hmm[5] == b':' => (
            [
                offset_digits(&hmm[..2]),
                offset_digits(&hmm[3..5]),
                offset_digits(&hmm[6..]),
            ],
            true,
        ),
        _ => return None,
    };

    let [Some(hour), Some(minute), Some(second)] = shape else {
        return None;
    };

    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let seconds = sign * (hour * 3600 + minute * 60 + second) as i32;

    Some(ParsedOffset {
        zone: FormatZone::from_offset_seconds(seconds),
        seconds_written,
    })
}

/// The value of a pair of ASCII digits, which is every field of 11.5.3's grammar.
fn offset_digits(pair: &[u8]) -> Option<u32> {
    let tens = char::from(*pair.first()?).to_digit(10)?;
    let ones = char::from(*pair.get(1)?).to_digit(10)?;

    Some(tens * 10 + ones)
}

/// The four values 11.1.2 step 33 and Table 15 of 11.3.2 name for `[[HourCycle]]`, from the row of
/// Table 4 of the same spec.
///
/// This is an engine type rather than ICU4X's own keyword because the two lists are not the same
/// list: `h24` - the 24-hour cycle numbered 1 to 24, CLDR's `k` - is a value ECMA-402 requires and
/// this ICU4X generation has no keyword for, and the two auto cycles it does have (`c12`, `c24`, the
/// `j` of a skeleton) are not answers an ECMA-402 format can report. [`HourCycle::to_icu`] gives
/// ICU4X the cycle it can render and the hour of a `k` cycle is corrected on the way out of
/// [`DateTimeRenderer::format`], which is the one place the two differ: a `0` hour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HourCycle {
    H11,
    H12,
    H23,
    H24,
}

impl HourCycle {
    /// The option value and the `resolvedOptions` string of Table 4.
    pub fn as_str(self) -> &'static str {
        match self {
            HourCycle::H11 => "h11",
            HourCycle::H12 => "h12",
            HourCycle::H23 => "h23",
            HourCycle::H24 => "h24",
        }
    }

    /// 9.2.11 over the Values column of the `hourCycle` row of Table 16, or of the `-u-hc-` subtag
    /// of 11.1.2 step 13.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "h11" => HourCycle::H11,
            "h12" => HourCycle::H12,
            "h23" => HourCycle::H23,
            "h24" => HourCycle::H24,
            _ => return None,
        })
    }

    /// The preference ICU4X renders this cycle with. Only `h24` has no equivalent: its one
    /// difference from `h23` is the hour it gives midnight, which is rewritten after the rendering.
    fn to_icu(self) -> IcuHourCycle {
        match self {
            HourCycle::H11 => IcuHourCycle::H11,
            HourCycle::H12 => IcuHourCycle::H12,
            HourCycle::H23 | HourCycle::H24 => IcuHourCycle::H23,
        }
    }
}

/// Which of the fields of 11.3's Table 3 a format renders, and how wide each of them is.
///
/// This is the residue of 11.5.4 after the option bag has been read: the components with their
/// widths, the two shorthands that replace whole groups of them, and the hour cycle the hour fields
/// render with. Everything else of the bag is either a preference of the locale - the calendar and
/// the numbering system - or a slot the field set does not carry, like `timeZone`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Components {
    pub weekday: Option<Width>,
    /// The era of Table 16, which 2.3 renders through [`YearStyle::WithEra`] - the locale's own era
    /// beside the year - rather than at the width the option asked for. A bag that asks for an era
    /// and no year gets the year anyway, the way V8's `{ era: "long" }` renders
    /// `2/3/2021 Anno Domini` and reports `year: "numeric"`.
    pub era: Option<Width>,
    pub year: Option<Width>,
    pub month: Option<Width>,
    pub day: Option<Width>,
    pub hour: Option<Width>,
    pub minute: Option<Width>,
    pub second: Option<Width>,
    pub fractional_second_digits: Option<u8>,
    pub day_period: Option<Width>,
    pub time_zone_name: Option<TimeZoneNameStyle>,
    pub date_style: Option<Style>,
    pub time_style: Option<Style>,
    /// The hour cycle of 11.5.6's step 12, resolved and kept because it is both a field of the
    /// rendering and the answer `resolvedOptions()` gives.
    pub hour_cycle: Option<HourCycle>,
    /// The Boolean of 11.5.6's step 19, which is what the option bag's `hour12` resolved to once the
    /// hour cycle answered it.
    pub hour12: Option<bool>,
}

impl Components {
    /// Whether any field of the date group was asked for.
    fn has_date(&self) -> bool {
        self.year.is_some() || self.month.is_some() || self.day.is_some() || self.weekday.is_some()
    }

    /// Whether any field of the time group was asked for, which is what decides whether the hour
    /// cycle of 11.5.6's step 12 is reported at all. Node gives no `hourCycle` for a date-only
    /// format, and this is the bit that says so.
    fn has_time(&self) -> bool {
        self.hour.is_some()
            || self.minute.is_some()
            || self.second.is_some()
            || self.fractional_second_digits.is_some()
            || self.day_period.is_some()
    }

    /// Whether the bag named any of the nine components, which is what 15.6.1's `anyPresent` answers
    /// and what decides whether a Temporal type's own format may fall back on its defaults. The two
    /// shorthands and the zone are deliberately not among the nine: a bag of `{ timeZoneName }` only
    /// is as unnamed to 15.6.1 as a bag of nothing is.
    pub fn has_any_component(&self) -> bool {
        ComponentProp::ANY_PRESENT
            .iter()
            .any(|property| property.is_asked(self))
    }
}

/// How wide a field is rendered, in the spelling of 11.3's Table 3.
///
/// The two families - the numeric `numeric` and `2-digit`, the named `long`, `short` and `narrow` -
/// are one enum because a CLDR date skeleton has one length for the whole of the date group, so the
/// table below has to be able to say whether two components of one group can be rendered together
/// at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Width {
    Numeric,
    TwoDigit,
    Long,
    Short,
    Narrow,
}

impl Width {
    /// The names of 11.3's Table 3, which are the strings the option bag carries.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "numeric" => Some(Width::Numeric),
            "2-digit" => Some(Width::TwoDigit),
            "long" => Some(Width::Long),
            "short" => Some(Width::Short),
            "narrow" => Some(Width::Narrow),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Width::Numeric => "numeric",
            Width::TwoDigit => "2-digit",
            Width::Long => "long",
            Width::Short => "short",
            Width::Narrow => "narrow",
        }
    }

    /// The ICU4X length of a field of a name.
    fn to_named_length(self) -> Option<Length> {
        match self {
            Width::Long => Some(Length::Long),
            // The `short` of Table 3 is the abbreviated name, `MMM` of 11.5.4's skeletons, which is
            // what this generation calls the `medium` length of a group: its `short` is the numeric
            // `M/d` that a `numeric` month asks for.
            Width::Short => Some(Length::Medium),
            // CLDR's skeleton key has no narrow length for a date group and 2.3's [`Length`] has no
            // variant for it either, so the narrow of Table 16 is not what picks the skeleton. The
            // longest one is what V8 patterns a narrow name onto - `MMMMM d, y` for an English
            // `{ month: "narrow", day: "numeric" }` - and the width itself is answered afterwards,
            // by rewriting the symbol of the field. See [`NarrowWidths`].
            Width::Narrow => Some(Length::Long),
            Width::Numeric | Width::TwoDigit => None,
        }
    }
}

/// The four shorthands of `dateStyle` and `timeStyle`, which are not widths: each one says what the
/// whole of a group is, and 11.5.4 reads them before it reads the components it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Full,
    Long,
    Medium,
    Short,
}

impl Style {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "full" => Some(Style::Full),
            "long" => Some(Style::Long),
            "medium" => Some(Style::Medium),
            "short" => Some(Style::Short),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Style::Full => "full",
            Style::Long => "long",
            Style::Medium => "medium",
            Style::Short => "short",
        }
    }

    /// The length the shorthand asks for. `full` is the one that is not a length of its own: it is
    /// the long group with the weekday added, which is what 11.3's Table 3 says it is.
    fn to_length(self) -> Length {
        match self {
            Style::Full | Style::Long => Length::Long,
            Style::Medium => Length::Medium,
            Style::Short => Length::Short,
        }
    }

    /// Whether the shorthand brings the weekday with it, which is all that distinguishes `full` from
    /// `long` in the field sets of ICU4X.
    fn has_weekday(self) -> bool {
        self == Style::Full
    }
}

/// The six spellings of the `timeZoneName` option of Table 16.
///
/// These are not the widths of the other fields: the zone group asks whether it wants the name the
/// zone has or the offset it is at, and only then how long that should be. `narrow` and the bare
/// `generic` of an earlier draft are not among them, so a bag that says either is the RangeError of
/// 11.1.2 step 24.2's `GetOption`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeZoneNameStyle {
    Long,
    Short,
    LongOffset,
    ShortOffset,
    LongGeneric,
    ShortGeneric,
}

impl TimeZoneNameStyle {
    pub const NAMES: [&'static str; 6] = [
        "short",
        "long",
        "shortOffset",
        "longOffset",
        "shortGeneric",
        "longGeneric",
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "long" => Self::Long,
            "short" => Self::Short,
            "longOffset" => Self::LongOffset,
            "shortOffset" => Self::ShortOffset,
            "longGeneric" => Self::LongGeneric,
            "shortGeneric" => Self::ShortGeneric,
            _ => return None,
        })
    }

    /// The option name 11.3.2 reports back, which is the one that was asked for rather than the
    /// field that renders it: `shortGeneric` and `short` are two answers even where a build with no
    /// name tables gives them the same text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Short => "short",
            Self::LongOffset => "longOffset",
            Self::ShortOffset => "shortOffset",
            Self::LongGeneric => "longGeneric",
            Self::ShortGeneric => "shortGeneric",
        }
    }

    /// The field set that renders this, which is the same question once the widths of the name
    /// families are lined up.
    ///
    /// A build without the zone name tables asks for the offset instead of the name, which is the
    /// fallback ICU4X itself takes field by field but is put here so that the format is built at all
    /// and so that `resolvedOptions` keeps reporting the name style that was asked for.
    fn to_zone_style(self) -> ZoneStyle {
        if !TIME_ZONE_NAMES_ARE_BAKED {
            return match self {
                Self::Long | Self::LongOffset | Self::LongGeneric => ZoneStyle::LocalizedOffsetLong,
                Self::Short | Self::ShortOffset | Self::ShortGeneric => {
                    ZoneStyle::LocalizedOffsetShort
                }
            };
        }

        match self {
            Self::Long => ZoneStyle::SpecificLong,
            Self::Short => ZoneStyle::SpecificShort,
            Self::LongOffset => ZoneStyle::LocalizedOffsetLong,
            Self::ShortOffset => ZoneStyle::LocalizedOffsetShort,
            Self::LongGeneric => ZoneStyle::GenericLong,
            Self::ShortGeneric => ZoneStyle::GenericShort,
        }
    }
}

/// The field set of a format plus the two things the field set does not carry: the hour cycle, which
/// is a preference of the locale rather than of the pattern, and the hour cycle's answer to whether
/// the format has an hour at all.
///
/// This is the key of the cache of [`LocaleCache::datetime_renderers`]: a format is its field set and
/// its preferences, and every instance that resolves to the same pair shares one renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FieldSetKey {
    /// The date group's fields, or `None` for a format with no date.
    pub date_fields: Option<DateFieldsKey>,
    /// The length of the date group. A format with a `dateStyle` has one whatever its components
    /// asked for; a format of components has the one width they agree on, and no key at all when
    /// they do not agree.
    pub length: Option<LengthKey>,
    /// The time group.
    pub time_precision: Option<TimePrecisionKey>,
    /// The zone field: the question the bag's `timeZoneName` asked and the answer the build can give.
    pub zone_style: Option<TimeZoneNameStyle>,
    /// The year's style, which is the one field of the date group a CLDR skeleton carries on its own.
    pub year_style: Option<YearStyleKey>,
    /// Whether the year is shortened to its last two digits on the way out of the rendering, which
    /// is what `year: "2-digit"` of Table 16 asks for and 2.3's [`YearStyle`] has no case for.
    pub two_digit_year: bool,
    /// The digit widths the bag asked for the numeric fields, which [`DateTimeRenderer::format`]
    /// pads to two digits when the pattern of the group renders one. See [`AskedWidths`].
    pub asked: AskedWidths,
    /// The named fields the bag asked at `narrow`, which the field set renders at their longest and
    /// the pattern of the rendering rewrites. See [`NarrowWidths`].
    pub narrow: NarrowWidths,
    /// Whether the format has an hour field, which is what 11.1.2 step 33 reads before it reports
    /// `hourCycle`.
    pub has_hour: bool,
    /// The hour cycle the renderer is built with. `None` leaves ICU4X to take the locale's own,
    /// which is what 11.1.2 steps 14 to 16 do when the bag said nothing.
    pub hour_cycle: Option<HourCycle>,
}

/// The digit widths of a bag: the `numeric` and `2-digit` of Table 16 for the fields that count in
/// digits rather than name by length.
///
/// A CLDR skeleton of this generation carries one length for a whole group, so the width of one
/// field of it is not something the pattern can be asked for; `2-digit` is therefore answered after
/// the rendering, by putting the zero in front of the one digit the pattern wrote. `numeric` answers
/// nothing: a pattern that pads (`{ hour: "numeric" }` in Danish, which is `09`) keeps its zero, and
/// 11.3.2's Table 15 reports the `2-digit` the rendering shows, which is what V8 reports for the
/// same bag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct AskedWidths {
    pub month: Option<Width>,
    pub day: Option<Width>,
    pub hour: Option<Width>,
    pub minute: Option<Width>,
    pub second: Option<Width>,
}

impl AskedWidths {
    /// The widths of a bag, which a style shorthand replaces whole: 11.5.4 reads the two groups the
    /// `dateStyle` and the `timeStyle` name and never looks at the components of those groups.
    fn of(components: &Components) -> Self {
        if components.date_style.is_some() || components.time_style.is_some() {
            return Self::default();
        }

        Self {
            month: components.month,
            day: components.day,
            hour: components.hour,
            minute: components.minute,
            second: components.second,
        }
    }

    /// Whether one part of a rendering is a digit field the bag asked for at two digits.
    fn asks_two_digits(&self, kind: PartKind) -> bool {
        let width = match kind {
            PartKind::Month => self.month,
            PartKind::Day => self.day,
            PartKind::Hour => self.hour,
            PartKind::Minute => self.minute,
            PartKind::Second => self.second,
            _ => return false,
        };

        width == Some(Width::TwoDigit)
    }

    /// Whether one part of a rendering is a field of the date group the bag asked for at `numeric`,
    /// which is the width that has to be taken back rather than added.
    ///
    /// Only the day and the month, and deliberately not the hour: a CLDR date skeleton counts a day
    /// of the month with `d` where the length-keyed pattern this build reads has `dd`, so a
    /// `{ day: "numeric" }` would otherwise answer `08` where Table 16 promises one or two digits as
    /// the number needs them. The time group has the same gap for the hour, but its 2.3 skeleton is
    /// what the engines render with too - `{ hour: "numeric" }` is `09` in Danish in both - so the
    /// hour keeps the width of its pattern and `resolvedOptions` reports it as `2-digit`.
    fn asks_one_digit(&self, kind: PartKind) -> bool {
        let width = match kind {
            PartKind::Month => self.month,
            PartKind::Day => self.day,
            _ => return false,
        };

        width == Some(Width::Numeric)
    }
}

/// The named fields a bag asked at the `narrow` of 11.3's Table 3, which is the one width a field
/// set of this ICU4X generation cannot carry: [`Length`] has no narrow and CLDR's skeleton table has
/// no narrow length for a group.
///
/// It is answered one step after the field set instead. The pattern the group selects is read back off
/// a rendering, the symbol of each field named here is widened to the five of UTS #35 - `EEEEE`,
/// `MMMMM`, `GGGGG` - which is the key the name tables of the data are baked at, and that pattern is
/// what the format renders with. See [`DateTimeRenderer::narrow`].
///
/// The day period is deliberately not one of the three: CLDR's narrow day period is the phrase of
/// `bbbbb` - the English "in the afternoon" - and this build's data has only the am/pm names, which
/// are a different width of a different symbol. [`FieldSetKey::for_components`] refuses that one
/// rather than spelling the half of the day at the wrong width.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct NarrowWidths {
    pub month: bool,
    pub weekday: bool,
    pub era: bool,
}

impl NarrowWidths {
    /// Whether the format has a narrow field at all, which is what leaves a rendering on the plain
    /// pattern of its field set.
    fn is_empty(self) -> bool {
        self == Self::default()
    }

    /// The narrow length of one symbol of a pattern, or `None` for a field this format does not
    /// rewrite and for a symbol that is a number rather than a name: the `d` of a day and the `M` of
    /// a numeric month are digits, and the `e` of one or two digits is the numeric weekday, which the
    /// five of would not spell.
    fn narrow_of(self, field: Field) -> Option<FieldLength> {
        if !matches!(
            field.length,
            FieldLength::Three | FieldLength::Four | FieldLength::Five | FieldLength::Six
        ) {
            return None;
        }

        match field.symbol {
            FieldSymbol::Month(_) if self.month => Some(FieldLength::Five),
            FieldSymbol::Weekday(_) if self.weekday => Some(FieldLength::Five),
            FieldSymbol::Era if self.era => Some(FieldLength::Five),
            _ => None,
        }
    }
}

/// The date groups of 11.5.4's table, which are the shapes of [`DateFields`] one for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DateFieldsKey {
    Day,
    MonthDay,
    YearMonthDay,
    DayWeekday,
    MonthDayWeekday,
    YearMonthDayWeekday,
    Weekday,
    Month,
    YearMonth,
    Year,
}

/// The three lengths of a date group, which are the three of 2.3's [`Length`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LengthKey {
    Long,
    Medium,
    Short,
}

/// The precisions of a time group, of [`TimePrecision`] without the subsecond digit count, which is
/// carried beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimePrecisionKey {
    Hour,
    Minute,
    Second,
    Subsecond(u8),
}

/// The year widths of [`YearStyle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum YearStyleKey {
    Auto,
    Full,
    /// The century with the era beside it, which is what [`Components::era`] asks of a generation
    /// whose field sets carry no era field of their own.
    WithEra,
}

impl DateFieldsKey {
    fn to_icu(self) -> DateFields {
        match self {
            DateFieldsKey::Day => DateFields::D,
            DateFieldsKey::MonthDay => DateFields::MD,
            DateFieldsKey::YearMonthDay => DateFields::YMD,
            DateFieldsKey::DayWeekday => DateFields::DE,
            DateFieldsKey::MonthDayWeekday => DateFields::MDE,
            DateFieldsKey::YearMonthDayWeekday => DateFields::YMDE,
            DateFieldsKey::Weekday => DateFields::E,
            DateFieldsKey::Month => DateFields::M,
            DateFieldsKey::YearMonth => DateFields::YM,
            DateFieldsKey::Year => DateFields::Y,
        }
    }

    /// Whether the group has a year in it, which is what decides whether a year width means
    /// anything to the builder.
    fn has_year(self) -> bool {
        matches!(
            self,
            DateFieldsKey::YearMonthDay
                | DateFieldsKey::YearMonthDayWeekday
                | DateFieldsKey::YearMonth
                | DateFieldsKey::Year
        )
    }

    /// The row of 11.5.8's Table 6 the group reaches, counted from the top of the table: the year is
    /// the first field it names, the month the second and the day of the month the third.
    ///
    /// The weekday has no row of its own, and it is the day that answers for it: the weekday is a
    /// function of the day, so a group of the weekday alone reaches the day's row, which is what
    /// keeps two Saturdays two weeks apart from spelling one date.
    fn table_row(self) -> u8 {
        match self {
            DateFieldsKey::Year => 1,
            DateFieldsKey::Month | DateFieldsKey::YearMonth => 2,
            DateFieldsKey::Day
            | DateFieldsKey::MonthDay
            | DateFieldsKey::YearMonthDay
            | DateFieldsKey::DayWeekday
            | DateFieldsKey::MonthDayWeekday
            | DateFieldsKey::YearMonthDayWeekday
            | DateFieldsKey::Weekday => 3,
        }
    }
}

impl LengthKey {
    fn to_icu(self) -> Length {
        match self {
            LengthKey::Long => Length::Long,
            LengthKey::Medium => Length::Medium,
            LengthKey::Short => Length::Short,
        }
    }
}

impl TimePrecisionKey {
    fn to_icu(self) -> TimePrecision {
        match self {
            TimePrecisionKey::Hour => TimePrecision::Hour,
            TimePrecisionKey::Minute => TimePrecision::Minute,
            TimePrecisionKey::Second => TimePrecision::Second,
            TimePrecisionKey::Subsecond(digits) => TimePrecision::Subsecond(
                SubsecondDigits::try_from_int(digits).unwrap_or(SubsecondDigits::S3),
            ),
        }
    }

    /// The row of 11.5.8's Table 6 the group reaches, after the three rows of the date group: the
    /// hour is the fourth field of the table, the minute the fifth, the second the sixth and the
    /// fraction of it the seventh.
    fn table_row(self) -> u8 {
        match self {
            TimePrecisionKey::Hour => 4,
            TimePrecisionKey::Minute => 5,
            TimePrecisionKey::Second => 6,
            TimePrecisionKey::Subsecond(_) => 7,
        }
    }
}

impl YearStyleKey {
    /// The builder's width of the year.
    ///
    /// 2.3's [`YearStyle`] has no two-digit case: the option was dropped when the field sets took
    /// the century out of the year symbol. What asks for two digits here is therefore rendered by
    /// [`DateTimeRenderer`], which shortens the year the pattern wrote out; the CLDR `yy` skeleton
    /// that shorthand formats use is the same two digits of the same number.
    fn to_icu(self) -> YearStyle {
        match self {
            YearStyleKey::Auto => YearStyle::Auto,
            YearStyleKey::Full => YearStyle::Full,
            YearStyleKey::WithEra => YearStyle::WithEra,
        }
    }
}

impl FieldSetKey {
    /// The builder-shaped field set of this key, which is what 2.3's `FieldSetBuilder` turns into
    /// the composite one every reachable combination of fields lives in.
    ///
    /// `None` is the combination the builder rejects: a group of fields no CLDR pattern of this
    /// generation has, which [`field_set_for`] has already ruled out and which is reported here as
    /// the loud error rather than rendered as a different format.
    pub fn to_icu(self) -> Option<CompositeFieldSet> {
        let mut builder = FieldSetBuilder::default();

        builder.length = self.length.map(LengthKey::to_icu);
        builder.date_fields = self.date_fields.map(DateFieldsKey::to_icu);
        builder.time_precision = self.time_precision.map(TimePrecisionKey::to_icu);
        builder.zone_style = self.zone_style.map(TimeZoneNameStyle::to_zone_style);
        // The builder refuses a year style for a field set with no year in it, which is the shape
        // of a format that asked for an era and no year at all.
        builder.year_style = match self.date_fields {
            Some(fields) if fields.has_year() => self.year_style.map(YearStyleKey::to_icu),
            _ => None,
        };

        builder.build_composite().ok()
    }

    /// The field set of an ECMA-402 option bag, which is 11.5.4's `FormatMatcher` read down to what
    /// ICU4X can be told.
    ///
    /// The rules are the ones of 11.5.4's table: the two shorthands replace whole groups, the
    /// components of a group that are asked for name the shape of that group, and the widths of the
    /// components of one group have to agree because a skeleton of this generation carries one
    /// length for all of them. A bag that asks for something no field set renders is
    /// [`FieldSetError`], which the instance reports loudly rather than as a format that shows
    /// other fields.
    pub fn for_components(components: &Components) -> Result<Self, FieldSetError> {
        // Table 16's `narrow` is a width of the named families that no skeleton of this generation is
        // keyed at, so the three of the date group are patterned at their longest and rewritten
        // afterwards by [`NarrowWidths`]. The day period is the one family the rewrite has no answer
        // for: its narrow is the phrase of `bbbbb`, which this build's data does not carry, and the
        // am/pm name that stands in for it is a different width of a different symbol.
        if components.day_period == Some(Width::Narrow) {
            return Err(FieldSetError::NarrowDayPeriod);
        }

        let narrow = NarrowWidths {
            month: components.month == Some(Width::Narrow),
            weekday: components.weekday == Some(Width::Narrow),
            era: components.era == Some(Width::Narrow),
        };

        let date_fields = date_fields_for(components)?;
        let length = length_for(components, date_fields)?;
        let time_precision = time_precision_for(components);
        let zone_style = zone_style_for(components);
        let year_style = year_style_for(components);

        if date_fields.is_none() && time_precision.is_none() && zone_style.is_none() {
            // 11.5.4's table with nothing in it. `ToDateTimeOptions` puts the default date fields
            // into any bag that had none, so this is only reachable through a bag that named no
            // field at all and asked for no style; it is reported rather than rendered as the empty
            // string.
            return Err(FieldSetError::NoFields);
        }

        Ok(Self {
            date_fields,
            length,
            time_precision,
            zone_style,
            year_style,
            two_digit_year: date_fields.is_some_and(|fields| {
                fields.has_year() && components.year == Some(Width::TwoDigit)
            }),
            asked: AskedWidths::of(components),
            narrow,
            has_hour: time_precision.is_some(),
            hour_cycle: components.hour_cycle,
        })
    }

    /// 15.6.2's `AdjustDateTimeStyleFormat` of this format - the `baseFormat` of 11.1.2 step 31.1's
    /// style branch, which is the shorthand's own - down to the fields of one Temporal type's
    /// `allowedOptions`.
    ///
    /// A format of a shorthand has no field outside the two groups and the zone, so the re-match of
    /// 15.6.2's last steps is this build's same skeleton with the groups the type has no field for
    /// taken out of it: the date group keeps its shape and its length, the time group keeps the
    /// length of its own shorthand because the CLDR time skeleton is keyed by it (see
    /// [`length_for`]), and `timeZoneName` belongs to no Temporal type but `Temporal.Instant`.
    ///
    /// `None` is a shape with nothing left in it, which 11.1.2 has already made the null of a slot
    /// whose shorthand was never asked for; the two are the same TypeError of 15.6.15 to 15.6.18.
    /// A style format names neither the widths of [`AskedWidths`] nor the `narrow` of
    /// [`NarrowWidths`] - both are the residue of components, which a shorthand replaces whole - so
    /// there is nothing of either to mask here.
    pub fn kept_for_temporal(self, kind: TemporalKind, time_style: Option<Style>) -> Option<Self> {
        // The date group alone: every field of `allowedOptions` of a type that has one of the date,
        // the year and month, or the month and day is a field of this format's date group.
        let date_only = Self {
            time_precision: None,
            zone_style: None,
            has_hour: false,
            ..self
        };

        let kept = match kind {
            TemporalKind::Date if self.date_fields.is_some() => date_only,
            TemporalKind::YearMonth if self.date_fields.is_some() => {
                Self { date_fields: Some(DateFieldsKey::YearMonth), ..date_only }
            }
            TemporalKind::MonthDay if self.date_fields.is_some() => {
                Self { date_fields: Some(DateFieldsKey::MonthDay), ..date_only }
            }
            TemporalKind::Time => Self {
                date_fields: None,
                // 2.3 takes no year width for a field set with no year in it, which
                // [`FieldSetKey::to_icu`] already rules out, and the length of a time-only format is
                // the length of its own shorthand.
                year_style: None,
                length: time_style.map(|style| length_key_of(style.to_length())),
                zone_style: None,
                ..self
            },
            // `allowedOptions` of `Temporal.PlainDateTime` is every component, so the zone is the one
            // field of the base it has to lose, and `Temporal.Instant` is the one type that keeps a
            // zone at all: both are the base itself beside that.
            TemporalKind::DateTime => Self { zone_style: None, ..self },
            TemporalKind::Instant => self,
            TemporalKind::Date | TemporalKind::YearMonth | TemporalKind::MonthDay => {
                return None;
            }
        };

        (kept.date_fields.is_some() || kept.time_precision.is_some()).then_some(kept)
    }
}

/// The Temporal types of 15.6.12's `IsTemporalObject` that 15.6 formats, in the order 15.6.22's
/// dispatch of `HandleDateTimeValue` tests for them.
///
/// `Temporal.ZonedDateTime` is deliberately not one of them: the dispatch ends by asserting the value
/// is one and throwing a TypeError, so nothing here ever names it, and the code that reads an
/// argument says so before it has a source to describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalKind {
    Date,
    YearMonth,
    MonthDay,
    Time,
    DateTime,
    Instant,
}

impl TemporalKind {
    /// `requiredOptions` of 15.6.1 for this type's slot, which are the components of the bag copied
    /// into the format of 11.1.2 step 31.2's `GetDateTimeFormat`.
    fn required_options(self) -> &'static [ComponentProp] {
        const DATE: [ComponentProp; 4] = [
            ComponentProp::Weekday,
            ComponentProp::Year,
            ComponentProp::Month,
            ComponentProp::Day,
        ];
        const TIME: [ComponentProp; 5] = [
            ComponentProp::DayPeriod,
            ComponentProp::Hour,
            ComponentProp::Minute,
            ComponentProp::Second,
            ComponentProp::FractionalSecondDigits,
        ];
        const YEAR_MONTH: [ComponentProp; 2] = [ComponentProp::Year, ComponentProp::Month];
        const MONTH_DAY: [ComponentProp; 2] = [ComponentProp::Month, ComponentProp::Day];
        const ANY: [ComponentProp; 9] = [
            ComponentProp::Weekday,
            ComponentProp::Year,
            ComponentProp::Month,
            ComponentProp::Day,
            ComponentProp::DayPeriod,
            ComponentProp::Hour,
            ComponentProp::Minute,
            ComponentProp::Second,
            ComponentProp::FractionalSecondDigits,
        ];

        match self {
            Self::Date => &DATE,
            Self::Time => &TIME,
            Self::YearMonth => &YEAR_MONTH,
            Self::MonthDay => &MONTH_DAY,
            Self::DateTime | Self::Instant => &ANY,
        }
    }

    /// `defaultOptions` of 15.6.1 for this type's slot, which 15.6.1 fills with `"numeric"` when the
    /// bag named none of [`TemporalKind::required_options`].
    fn default_options(self) -> &'static [ComponentProp] {
        const DATE: [ComponentProp; 3] = [
            ComponentProp::Year,
            ComponentProp::Month,
            ComponentProp::Day,
        ];
        const TIME: [ComponentProp; 3] = [
            ComponentProp::Hour,
            ComponentProp::Minute,
            ComponentProp::Second,
        ];
        const YEAR_MONTH: [ComponentProp; 2] = [ComponentProp::Year, ComponentProp::Month];
        const MONTH_DAY: [ComponentProp; 2] = [ComponentProp::Month, ComponentProp::Day];
        const ALL: [ComponentProp; 6] = [
            ComponentProp::Year,
            ComponentProp::Month,
            ComponentProp::Day,
            ComponentProp::Hour,
            ComponentProp::Minute,
            ComponentProp::Second,
        ];

        match self {
            Self::Date => &DATE,
            Self::Time => &TIME,
            Self::YearMonth => &YEAR_MONTH,
            Self::MonthDay => &MONTH_DAY,
            // `defaults` is `all` of both, which is the same six fields; `zoned-date-time` would add
            // the zone, and only 15.11's `toLocaleString` asks for that one.
            Self::DateTime | Self::Instant => &ALL,
        }
    }

    /// Whether 15.6.1 copies the bag's `[[era]]` beside the required group: its `required` is one of
    /// `date`, `year-month` or `any`.
    fn inherits_era(self) -> bool {
        matches!(self, Self::Date | Self::YearMonth | Self::DateTime | Self::Instant)
    }

    /// Whether 15.6.1 copies the bag's `[[hourCycle]]`: its `required` is either `time` or `any`.
    fn inherits_hour_cycle(self) -> bool {
        matches!(self, Self::Time | Self::DateTime | Self::Instant)
    }

    /// The type as a message names it, which is the spelling 15.6.12's `IsTemporalObject` and the
    /// error messages of the engines agree on.
    pub fn name(self) -> &'static str {
        match self {
            Self::Date => "Temporal.PlainDate",
            Self::YearMonth => "Temporal.PlainYearMonth",
            Self::MonthDay => "Temporal.PlainMonthDay",
            Self::Time => "Temporal.PlainTime",
            Self::DateTime => "Temporal.PlainDateTime",
            Self::Instant => "Temporal.Instant",
        }
    }

    /// Whether `inherit` is `all` for this type's slot, which is `[[TemporalInstantFormat]]` alone:
    /// an instant is a whole moment, so it takes the constructor's own bag rather than the half of it
    /// that a civil type can answer.
    fn inherits_everything(self) -> bool {
        self == Self::Instant
    }

    /// `[[IsPlain]]` of 15.6.14's Value Format Record, which 15.6.4 reads as the `+00:00` it formats
    /// a wall clock with: a civil type names no zone, so its fields are made civil in UTC rather than
    /// in the zone of 11.1.2 step 17.
    pub fn is_plain(self) -> bool {
        self != Self::Instant
    }
}

/// One Temporal argument, read the way 15.6.15 to 15.6.20 read it: the epoch nanoseconds of its
/// Value Format Record, and the calendar its handler compares with `[[Calendar]]`.
///
/// Both are plain copies out of the object, which is what lets the reader of an argument hold
/// nothing from the GC heap while the rendering allocates: `calendar` is the borrowed identifier of
/// `temporal_rs`, which lives in the binary rather than in the block the object was collected from.
/// `Temporal.Instant` has no `[[Calendar]]`, and is given the one 15.6.15 would accept of it.
#[derive(Debug, Clone, Copy)]
pub struct TemporalSource {
    pub kind: TemporalKind,
    pub epoch_nanoseconds: i128,
    pub calendar: &'static str,
}

/// 15.6.11's `ToDateTimeFormattable` answer, which is what the four entry points of 11.3 hand down
/// and what 15.6.5 and 15.6.8 are written in terms of: either a value that is still a number, whose
/// `TimeClip` and ±8.64e15 milliseconds belong to 21.4.1.1 and are checked here, or a civil source
/// whose fields are already in hand and which no coercion is asked of.
#[derive(Debug, Clone, Copy)]
pub enum Formattable {
    /// `ToNumber` of the argument, unclipped: 15.6.21's handler of a number is what clips it, which
    /// is also the RangeError of an `Invalid Date`.
    TimeValue(f64),
    Temporal(TemporalSource),
    /// A `Temporal.ZonedDateTime`, which 15.6.11 hands back untouched and only 15.6.22's dispatch
    /// refuses, as its last step and as a TypeError. It is a variant rather than an error out of the
    /// coercion because the dispatch comes after `ToDateTimeFormattable` of *both* arguments of
    /// 15.6.8: a range of one of these and of an object with a `valueOf` has to run that `valueOf`
    /// before it fails.
    ZonedDateTime,
}

/// One of the nine components of 15.6.1's `requiredOptions`, `defaultOptions` and `anyPresent` lists,
/// which are the rows of Table 16 that say which fields a format has.
///
/// `era` and `timeZoneName` are deliberately not among them: 15.6.1 names the first beside a
/// `required` of `date`, `year-month` or `any` and never counts it as present, and the second only
/// through `inherit` of `all`, which is [`TemporalKind::inherits_everything`] rather than a property
/// of any list here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentProp {
    Weekday,
    Year,
    Month,
    Day,
    DayPeriod,
    Hour,
    Minute,
    Second,
    FractionalSecondDigits,
}

impl ComponentProp {
    /// The nine of 15.6.1's `anyPresent` walk, in the order of its list.
    const ANY_PRESENT: [ComponentProp; 9] = [
        ComponentProp::Weekday,
        ComponentProp::Year,
        ComponentProp::Month,
        ComponentProp::Day,
        ComponentProp::DayPeriod,
        ComponentProp::Hour,
        ComponentProp::Minute,
        ComponentProp::Second,
        ComponentProp::FractionalSecondDigits,
    ];

    /// Whether a bag named this component.
    fn is_asked(self, components: &Components) -> bool {
        self.width_of(components).is_some() || self.digits_of(components).is_some()
    }

    /// The width a bag named this component with, which is a number for the fraction of the second
    /// and so is not a width at all (see [`ComponentProp::digits_of`]).
    fn width_of(self, components: &Components) -> Option<Width> {
        match self {
            ComponentProp::Weekday => components.weekday,
            ComponentProp::Year => components.year,
            ComponentProp::Month => components.month,
            ComponentProp::Day => components.day,
            ComponentProp::DayPeriod => components.day_period,
            ComponentProp::Hour => components.hour,
            ComponentProp::Minute => components.minute,
            ComponentProp::Second => components.second,
            ComponentProp::FractionalSecondDigits => None,
        }
    }

    /// `fractionalSecondDigits`, which Table 16 reads with `GetNumberOption` rather than with
    /// `GetOption` and so is the one component that is a count.
    fn digits_of(self, components: &Components) -> Option<u8> {
        match self {
            ComponentProp::FractionalSecondDigits => components.fractional_second_digits,
            _ => None,
        }
    }

    /// 15.6.1's `Set formatOptions.[[prop]] to options.[[prop]]`.
    fn copy(self, from: &Components, into: &mut Components) {
        match self {
            ComponentProp::FractionalSecondDigits => {
                into.fractional_second_digits = from.fractional_second_digits;
            }
            _ => {
                let width = self.width_of(from);
                self.set_width(into, width);
            }
        }
    }

    /// 15.6.1's `Set formatOptions.[[prop]] to "numeric"`, which of the fraction is the three digits
    /// its default of 1 is spelled as.
    fn set_numeric(self, into: &mut Components) {
        match self {
            ComponentProp::FractionalSecondDigits => into.fractional_second_digits = Some(3),
            _ => self.set_width(into, Some(Width::Numeric)),
        }
    }

    fn set_width(self, into: &mut Components, width: Option<Width>) {
        match self {
            ComponentProp::Weekday => into.weekday = width,
            ComponentProp::Year => into.year = width,
            ComponentProp::Month => into.month = width,
            ComponentProp::Day => into.day = width,
            ComponentProp::DayPeriod => into.day_period = width,
            ComponentProp::Hour => into.hour = width,
            ComponentProp::Minute => into.minute = width,
            ComponentProp::Second => into.second = width,
            ComponentProp::FractionalSecondDigits => {}
        }
    }
}

/// 15.6.1's `GetDateTimeFormat` of one Temporal type, for the branch 11.1.2 step 31 takes when
/// neither shorthand was asked for: the components of that type's `requiredOptions` that the bag
/// named, or the `defaultOptions` of its `defaults` when it named none of them, matched by the
/// 15.6.1's own last steps - which this build answers with the one skeleton of
/// [`FieldSetKey::for_components`], as 11.1.2 step 31.8 explains.
///
/// `None` is the null 15.6.1 returns for a bag that named a component of another group and none of
/// this type's, which 15.6.15 to 15.6.18 report as a TypeError rather than as a format.
pub fn temporal_components(asked: &Components, kind: TemporalKind) -> Option<Components> {
    // `inherit` of `all` starts from the whole bag, of which the hour cycle and the zone are as much
    // a part as the components; `relevant` starts from nothing and copies the two 15.6.1 names.
    let mut filled = if kind.inherits_everything() {
        *asked
    } else {
        Components::default()
    };

    if kind.inherits_era() {
        filled.era = asked.era;
    }

    if kind.inherits_hour_cycle() || kind.inherits_everything() {
        filled.hour_cycle = asked.hour_cycle;
    }

    let any_present = asked.has_any_component();
    let mut need_defaults = true;

    for property in kind.required_options() {
        if property.is_asked(asked) {
            property.copy(asked, &mut filled);
            need_defaults = false;
        }
    }

    if need_defaults {
        if any_present && !kind.inherits_everything() {
            return None;
        }

        for property in kind.default_options() {
            property.set_numeric(&mut filled);
        }
    }

    // 11.1.2's rule that an era comes with a year, which this build renders through
    // [`YearStyleKey::WithEra`] rather than through a field of its own and which therefore belongs to
    // the derived bag as much as to the constructor's.
    if filled.era.is_some() && filled.year.is_none() {
        filled.year = Some(Width::Numeric);
    }

    Some(filled)
}

/// What an option bag can ask for that no field set of this ICU4X generation renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldSetError {
    /// The two components of one group at two widths, which one skeleton cannot carry: the `long`
    /// month with the `2-digit` day of `{ month: "long", day: "2-digit" }`.
    MixedWidths,
    /// A `narrow` day period, whose CLDR name is the phrase of `bbbbb` and which this build's day
    /// period tables have no entry for: they carry the am/pm names of `a`, which is another width of
    /// another symbol. See [`NarrowWidths`].
    NarrowDayPeriod,
    /// A `narrow` field whose name the data of this build does not have, so that the rewritten
    /// pattern of [`NarrowWidths`] has nothing to render with.
    NarrowNames,
    /// A group no pattern of this generation has, like the year with the day and no month.
    UnsupportedShape,
    /// A bag with no field in it at all.
    NoFields,
    /// The builder's own answer for a combination its tables do not have.
    Builder,
}

impl std::fmt::Display for FieldSetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MixedWidths => write!(
                formatter,
                "the components of one group were asked for at two different widths"
            ),
            Self::NarrowDayPeriod => {
                write!(formatter, "the narrow day period has no phrase names in this build")
            }
            Self::NarrowNames => {
                write!(formatter, "the narrow names are not in this build's data")
            }
            Self::UnsupportedShape => {
                write!(formatter, "no pattern of this build has that group of fields")
            }
            Self::NoFields => write!(formatter, "no field was asked for"),
            Self::Builder => write!(formatter, "the field set has no pattern"),
        }
    }
}

/// The date group of a bag: the shape its components make, or `None` for a bag with no date field.
fn date_fields_for(components: &Components) -> Result<Option<DateFieldsKey>, FieldSetError> {
    if let Some(style) = components.date_style {
        // The shorthand replaces the components of the group entirely, which is 11.5.4's table: a
        // bag with `dateStyle` renders the style whatever else it named. `full` is the one that
        // brings the weekday with it.
        return Ok(Some(if style.has_weekday() {
            DateFieldsKey::YearMonthDayWeekday
        } else {
            DateFieldsKey::YearMonthDay
        }));
    }

    if !components.has_date() {
        return Ok(None);
    }

    let shape = (
        components.weekday.is_some(),
        components.year.is_some(),
        components.month.is_some(),
        components.day.is_some(),
    );

    Ok(Some(match shape {
        (false, true, true, true) => DateFieldsKey::YearMonthDay,
        (true, true, true, true) => DateFieldsKey::YearMonthDayWeekday,
        (false, false, true, true) => DateFieldsKey::MonthDay,
        (true, false, true, true) => DateFieldsKey::MonthDayWeekday,
        (false, false, false, true) => DateFieldsKey::Day,
        (true, false, false, true) => DateFieldsKey::DayWeekday,
        (false, true, true, false) => DateFieldsKey::YearMonth,
        (false, false, true, false) => DateFieldsKey::Month,
        (false, true, false, false) => DateFieldsKey::Year,
        (true, false, false, false) => DateFieldsKey::Weekday,
        // The shapes no CLDR date pattern of this generation is keyed at: a weekday with a year and
        // no month, and a year with a day and no month.
        _ => return Err(FieldSetError::UnsupportedShape),
    }))
}

/// The length of the date group, which the shorthand sets and the components have to agree on.
///
/// `2-digit` and `numeric` are not lengths: they are the digit widths of the numeric fields, which
/// 11.5.4's table reaches through [`YearStyle`] for the year and which this generation's patterns
/// carry for the day and the month as part of the length. A group of only numeric fields therefore
/// has one length, `short`, whatever the digit widths asked for, and the widths that disagree are
/// the named ones - the `long` month with the `short` weekday of a full date.
fn length_for(
    components: &Components,
    date_fields: Option<DateFieldsKey>,
) -> Result<Option<LengthKey>, FieldSetError> {
    if components.date_style.is_none() && date_fields.is_none() {
        // The time-only formats still want a length, because the CLDR *time* skeleton is keyed by
        // the same option; the shorthand of `timeStyle` says it and the components of the time group
        // do not.
        return Ok(components
            .time_style
            .map(|style| length_key_of(style.to_length())));
    }

    if let Some(style) = components.date_style {
        return Ok(Some(length_key_of(style.to_length())));
    }

    let mut chosen: Option<LengthKey> = None;

    for width in [components.month, components.weekday].into_iter().flatten() {
        // The `numeric` and `2-digit` of 11.5.4's table are the digit widths, which say nothing about
        // the length of the group they are in; the `narrow` says the longest, which is what V8 patterns
        // a narrow name onto, and is rewritten by [`NarrowWidths`] afterwards.
        let Some(named) = width.to_named_length() else {
            continue;
        };

        let wanted = length_key_of(named);
        match chosen {
            None => chosen = Some(wanted),
            Some(previous) if previous == wanted => {}
            Some(_) => return Err(FieldSetError::MixedWidths),
        }
    }

    Ok(Some(chosen.unwrap_or(LengthKey::Short)))
}

/// The [`LengthKey`] of an ICU4X length.
fn length_key_of(length: Length) -> LengthKey {
    match length {
        Length::Long => LengthKey::Long,
        Length::Medium => LengthKey::Medium,
        // 2.3's [`Length`] is `non_exhaustive` and has nothing else today: an answer from a later
        // generation is read as the shortest of the three rather than as a compile error here.
        _ => LengthKey::Short,
    }
}

/// The precision of the time group, which is the finest of the components asked for, or the one the
/// shorthand says.
///
/// 11.5.4's table takes the time group as one field set, so a format with seconds is rendered to the
/// second whatever the minute and the hour widths were; and the subsecond count of
/// `fractionalSecondDigits` is a precision of its own, which is the one option of Table 3 that is a
/// number rather than a name.
fn time_precision_for(components: &Components) -> Option<TimePrecisionKey> {
    if let Some(style) = components.time_style {
        // `short` is to the minute and the other three are to the second; the difference between
        // `full`, `long` and `medium` is in the zone and in the length of the date, not here.
        return Some(match style {
            Style::Short => TimePrecisionKey::Minute,
            _ => TimePrecisionKey::Second,
        });
    }

    if !components.has_time() {
        return None;
    }

    Some(if let Some(digits) = components.fractional_second_digits {
        TimePrecisionKey::Subsecond(digits)
    } else if components.second.is_some() || components.day_period.is_some() {
        TimePrecisionKey::Second
    } else if components.minute.is_some() {
        TimePrecisionKey::Minute
    } else {
        TimePrecisionKey::Hour
    })
}

/// The zone group of 11.5.4's table, which is nothing at all unless the bag named one.
///
/// The two shorthands of the time group are the exception CLDR writes into the patterns themselves:
/// its `full` and `long` time formats name the zone beside the seconds, where `medium` and `short` do
/// not. 2.3's [`Length`] has no `full`, so the two shorthands are one length apart only through this
/// zone, which is also how V8 tells them apart. The name is not put onto `[[TimeZoneName]]`, which
/// Table 15 only reports for a bag that asked for it.
fn zone_style_for(components: &Components) -> Option<TimeZoneNameStyle> {
    components.time_zone_name.or(match components.time_style {
        Some(Style::Full) => Some(TimeZoneNameStyle::Long),
        Some(Style::Long) => Some(TimeZoneNameStyle::Short),
        _ => None,
    })
}

/// The style of the year, which is the one field of the date group whose width a CLDR skeleton
/// carries on its own.
///
/// An `era` of Table 16 is rendered by 2.3's [`YearStyle::WithEra`], which puts the locale's era
/// beside the year: it is the one era width CLDR's skeleton key has, so the `narrow`, `short` and
/// `long` of the option become whatever the locale's era of that pattern is. A `numeric` year with
/// no era asked for is the four digits of [`YearStyleKey::Full`], which is what V8 and SpiderMonkey
/// give for `{ year: "numeric" }` and for `new Date().toLocaleDateString()`; a year that comes from
/// a `dateStyle` keeps [`YearStyleKey::Auto`], which is the skeleton CLDR chose for that length of
/// that locale - the two digits `1/1/21` for `short` in English, the four `01.01.2021` for `short`
/// in Danish.
fn year_style_for(components: &Components) -> Option<YearStyleKey> {
    if components.era.is_some() {
        return Some(YearStyleKey::WithEra);
    }

    components.year?;

    Some(if components.date_style.is_some() {
        YearStyleKey::Auto
    } else {
        YearStyleKey::Full
    })
}

/// The renderer of one format: the ICU4X formatter of one field set and one locale, and the hour
/// cycle it renders with.
///
/// The type is what a resolved format is: a field set, a locale's data, and nothing else. It is held
/// behind an [`Rc`] by the cache of [`LocaleCache`] and by the instance, so that two formats of the
/// same options share one loaded pattern set the way the two of a number format share one renderer.
pub struct DateTimeRenderer {
    formatter: icu_datetime::FixedCalendarDateTimeFormatter<Gregorian, CompositeFieldSet>,
    /// The same field set as a range formatter, which is what `formatRange` of 11.3.4 renders
    /// through. `None` when the data has no range pattern for this group of fields, in which case
    /// 11.5.8's fallback spells both ends out.
    range:
        Option<icu_datetime::range::FixedCalendarDateRangeFormatter<Gregorian, CompositeFieldSet>>,
    /// The hour cycle the format renders with, which is what 11.1.2 step 33 reports and what the
    /// instance needs in order to answer 11.3.2's `hourCycle` row. `None` when the format has no
    /// hour: the locale's own answer is then not used, because there is nothing to render with it.
    pub hour_cycle: Option<HourCycle>,
    /// Whether the year is shortened to its last two digits on the way out of the rendering, which
    /// is what `year: "2-digit"` of Table 16 asks of a generation whose [`YearStyle`] has no
    /// two-digit case.
    two_digit_year: bool,
    /// The digit widths the bag asked for, which are padded onto the rendering. See [`AskedWidths`].
    asked: AskedWidths,
    /// Whether the field set has a `timeZoneName` field, which is the only part of a format that
    /// asks ICU4X for the zone itself rather than for the offset seconds [`civil_date_time`] works
    /// with. See [`FormatZone::offset_is_renderable`].
    names_zone: bool,
    /// The date group of the field set, which with [`DateTimeRenderer::time_precision`] is how far
    /// the walk of 11.5.8 step 15 has to compare the ends of a range.
    date_fields: Option<DateFieldsKey>,
    /// The time group of the field set, see [`DateTimeRenderer::date_fields`].
    time_precision: Option<TimePrecisionKey>,
    /// What the rendering of this field set looks like, read off two fixed instants once the format
    /// is built. See [`RenderedWidths`].
    pub widths: RenderedWidths,
    /// The pattern of this format with its `narrow` name symbols and the names loaded for it, which
    /// is what [`DateTimeRenderer::format`] renders with instead of the field set. `None` for a
    /// format that asked no narrow width. See [`NarrowWidths`].
    narrow: Option<NarrowRender>,
}

/// A format whose pattern says `EEEEE`, `MMMMM` or `GGGGG`, which is the narrow of Table 16 as UTS #35
/// spells it and as the name tables of the data are keyed.
///
/// The two fields belong together: [`FixedCalendarDateTimeNames::with_pattern_unchecked`] renders a
/// pattern off names that were loaded for that very pattern, and the names are loaded once here by
/// [`FixedCalendarDateTimeNames::load_for_pattern`] so that a group the data has no narrow name for
/// is refused at the constructor rather than written out at another width.
struct NarrowRender {
    pattern: DateTimePattern,
    names: FixedCalendarDateTimeNames<Gregorian, CompositeFieldSet>,
}

/// The instant the widths of [`RenderedWidths`] are read off: 2021-02-03 at 03:05:06.070, the third
/// day of a month whose number is a single digit. Every one of the numeric fields of that moment is
/// one digit, which is what makes a rendering of two of them say that the pattern pads.
const PROBE_MOMENT: f64 = 1_612_321_506_070.0;

/// The connector of 11.5.8's fallback pattern, the `{0}–{1}` that joins both ends when the locale has
/// no range pattern for the difference between them. The three built-in locales spell it the same
/// way; ICU4X hands the same character as the glue of the patterns it does have.
const RANGE_CONNECTIVE: &str = "\u{2013}";

/// The same day at midnight, which is the only moment whose hour tells the four hour cycles apart:
/// `h12` writes it as 12, `h11` as 11, `h23` as 0 and `h24` as 24.
const PROBE_MIDNIGHT: f64 = 1_612_310_400_000.0;

/// What a field set renders, as opposed to what the option bag asked for.
///
/// ECMA-402's `resolvedOptions` reports the components of the pattern the format matched, not the
/// ones of the bag (11.3.2's Table 15 reads `dateTimeFormat.[[DateTimeFormat]]`), and this ICU4X
/// generation does not hand the matched skeleton back: the Danish `{ hour: "numeric" }` that
/// renders `03` is reported as `2-digit` for that reason. The digit widths are therefore read off
/// the rendering itself, which is what the two probes below are for, and are the answer whenever
/// the rendering has the field in it.
///
/// The named widths - the `long`, `short` and `narrow` of a month or a weekday - are not in here,
/// because telling `Feb` from `February` needs the name tables rather than the rendering, and 11.3.2
/// therefore reports what was asked for those.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderedWidths {
    pub year: Option<Width>,
    pub month: Option<Width>,
    pub day: Option<Width>,
    pub hour: Option<Width>,
    pub minute: Option<Width>,
    pub second: Option<Width>,
    /// The hour cycle of the rendering, which is how the locale's own answer to the question is
    /// reached without a table of them: 11.1.2's steps 14 to 16 turn it into the 12- or 24-hour one
    /// the bag asked for.
    pub hour_cycle: Option<HourCycle>,
}

impl RenderedWidths {
    /// The widths and the hour cycle of a rendering, read off the two probe instants in a zone that
    /// moves neither: the offset zone of zero, whose civil time is the UTC time of the instant.
    fn probe(parts: &[DateTimePart], midnight: &[DateTimePart]) -> Self {
        let digit_width = |kind: PartKind| -> Option<Width> {
            let text = parts.iter().find(|part| part.kind == kind)?.value.as_str();
            let digits = text.chars().filter(char::is_ascii_digit).count();

            match digits {
                // The named fields - a `February` or an `AM` - carry no digits and so answer nothing
                // here, which is what leaves them to the width the bag asked for.
                0 => None,
                1 => Some(Width::Numeric),
                // Three digits is a `2021`, which is the four of `numeric` and not the two of
                // `2-digit`; the same is true of whatever a locale renders wider than that.
                _ if digits > 2 => Some(Width::Numeric),
                _ => Some(Width::TwoDigit),
            }
        };

        Self {
            year: digit_width(PartKind::Year),
            month: digit_width(PartKind::Month),
            day: digit_width(PartKind::Day),
            hour: digit_width(PartKind::Hour),
            minute: digit_width(PartKind::Minute),
            second: digit_width(PartKind::Second),
            // The cycle is read off what midnight renders as, which is the one hour of the day the
            // four cycles of Table 4 disagree about: `h12` gives it 12, `h24` gives it 24, and
            // `h11` and `h23` both give it 0 and are told apart by whether the rendering says which
            // half of the day it is - `b`/`K` of 11.5.4 comes with a day period, `H` does not.
            hour_cycle: midnight
                .iter()
                .find(|part| part.kind == PartKind::Hour)
                .and_then(|part| part.value.trim().parse::<i32>().ok())
                .and_then(|hour| {
                    Some(match hour {
                        12 => HourCycle::H12,
                        24 => HourCycle::H24,
                        0 => {
                            if midnight.iter().any(|part| part.kind == PartKind::DayPeriod) {
                                HourCycle::H11
                            } else {
                                HourCycle::H23
                            }
                        }
                        _ => return None,
                    })
                }),
        }
    }
}

impl NarrowRender {
    /// The pattern the field set of `formatter` selects, with the symbols of the narrow fields widened
    /// to the five of UTS #35, and the names this build's data has for that pattern.
    ///
    /// The pattern is read off one rendering rather than written out here: it is the locale's own
    /// glue and the locale's own digits that the narrow of Table 16 is asked to keep, and only the
    /// length of a name is its business. Which pattern a field set selects does not depend on the
    /// moment - the date and time variants a selection looks at are fixed by the calendar and by the
    /// hour cycle, both of which the renderer holds - so the probe instant is as good a moment as
    /// any.
    ///
    /// [`FixedCalendarDateTimeNames::load_for_pattern`] is the check that the data answers the
    /// rewritten pattern: it loads the name of every symbol in it, and a symbol the tables have no
    /// entry for is a [`FieldSetError::NarrowNames`] here rather than a rendering that quietly falls
    /// back to the digits or to another width.
    fn try_new(
        formatter: &icu_datetime::FixedCalendarDateTimeFormatter<Gregorian, CompositeFieldSet>,
        narrow: NarrowWidths,
        preferences: DateTimeFormatterPreferences,
        moment: f64,
        zone: &FormatZone,
    ) -> Result<Self, FieldSetError> {
        let Some(date_time) = civil_date_time(moment, zone.offset_seconds) else {
            return Err(FieldSetError::NarrowNames);
        };

        let input = ZonedDateTime {
            date: date_time.date,
            time: date_time.time,
            zone: zone.to_icu(date_time),
        };

        let RuntimePattern { items, metadata } = formatter.format(&input).pattern().into();
        let mut items = items.to_vec();
        for item in &mut items {
            if let PatternItem::Field(field) = item {
                if let Some(length) = narrow.narrow_of(*field) {
                    field.length = length;
                }
            }
        }

        let pattern = DateTimePattern::from(RuntimePattern {
            items: ZeroVec::alloc_from_slice(&items),
            metadata,
        });

        let mut names =
            FixedCalendarDateTimeNames::<Gregorian, CompositeFieldSet>::try_new_unstable(
                &IntlDataProvider,
                preferences,
            )
            .map_err(|_| FieldSetError::NarrowNames)?;
        names
            .load_for_pattern(&IntlDataProvider, &pattern)
            .map_err(|_| FieldSetError::NarrowNames)?;

        Ok(Self { pattern, names })
    }
}

impl DateTimeRenderer {
    /// Whether a moment rendered here gets a `timeZoneName` field, which is what tells an
    /// unrenderable zone offset apart from one that only needs the civil arithmetic.
    pub fn names_zone(&self) -> bool {
        self.names_zone
    }
    /// The renderer of a locale's tag and a field set, built by [`LocaleCache::datetime_renderer`]
    /// so that every instance with the same resolved options shares it.
    ///
    /// The hour cycle goes on the preferences rather than on the field set: 11.5.6's step 13 resolves
    /// it from the option bag and from the locale's data, and ICU4X reads it from the preferences of
    /// the constructor, which is where a preference of the locale belongs.
    pub fn try_new(tag: &str, key: FieldSetKey) -> Result<Self, FieldSetError> {
        let locale: Locale = tag.parse().map_err(|_| FieldSetError::Builder)?;

        let mut preferences = DateTimeFormatterPreferences::from(&locale);
        preferences.hour_cycle = key.hour_cycle.map(|cycle| cycle.to_icu());

        let field_set = key.to_icu().ok_or(FieldSetError::Builder)?;

        let formatter = icu_datetime::FixedCalendarDateTimeFormatter::try_new_unstable(
            &IntlDataProvider,
            preferences,
            field_set,
        )
        .map_err(|_| FieldSetError::Builder)?;

        // The range patterns of the group of fields, which the format of 11.3.4 renders through. A
        // build without them still formats the group, and its range falls back to the two ends
        // spelled out, which is 11.5.8's own fallback. See [`DateTimeRenderer::format_range_parts`].
        // A narrow format takes that fallback on purpose: a range pattern of the data spells its
        // names at the lengths of the field set, which are the widths the bag did not ask for.
        let range = if key.narrow.is_empty() {
            icu_datetime::range::FixedCalendarDateRangeFormatter::try_new_unstable(
                &IntlDataProvider,
                preferences,
                field_set,
            )
            .ok()
        } else {
            None
        };

        let utc = FormatZone::from_offset_seconds(0);

        let narrow = if key.narrow.is_empty() {
            None
        } else {
            Some(NarrowRender::try_new(&formatter, key.narrow, preferences, PROBE_MOMENT, &utc)?)
        };

        let mut renderer = Self {
            formatter,
            range,
            hour_cycle: key.hour_cycle,
            two_digit_year: key.two_digit_year,
            asked: key.asked,
            names_zone: key.zone_style.is_some(),
            date_fields: key.date_fields,
            time_precision: key.time_precision,
            widths: RenderedWidths::default(),
            narrow,
        };

        renderer.widths = RenderedWidths::probe(
            &renderer.format(PROBE_MOMENT, &utc),
            &renderer.format(PROBE_MIDNIGHT, &utc),
        );

        Ok(renderer)
    }

    /// 11.5.8's `PartitionDateTimeRangePattern`, which is what `formatRangeToParts` of 11.3.5 answers
    /// with and, joined, what `formatRange` of 11.3.4 answers with.
    ///
    /// The two ends are rendered by the range pattern CLDR chose for the difference between them,
    /// which is the one piece of data 11.5.8's steps 10 to 19 is written in terms of and which this
    /// ICU4X generation owns. Where the build has no range pattern for the field set at all, steps
    /// 21 to 23 say what to do instead: each end is spelled out with the format's own pattern and the
    /// two halves are joined by the range connector, which is `RANGE_CONNECTIVE`.
    ///
    /// The glue of that pattern - the ` - ` of an English day range - is what 11.5.11's partition
    /// calls a literal, and `icu_datetime::range` writes it as plain characters rather than as an
    /// annotated part, so the collector below already lands it in the `literal` record: an
    /// annotation this crate does not name is a piece of text between two fields.
    pub fn format_range_parts(
        &self,
        start: f64,
        start_zone: &FormatZone,
        end: f64,
        end_zone: &FormatZone,
    ) -> Option<Vec<DateTimePart>> {
        let (Some(start_time), Some(end_time)) = (
            civil_date_time(start, start_zone.offset_seconds),
            civil_date_time(end, end_zone.offset_seconds),
        ) else {
            return None;
        };

        Some(self.range_parts_at(start_time, start_zone, end_time, end_zone, self.range.as_ref()))
    }

    /// [`DateTimeRenderer::format_range_parts`] of two ends that are already nanoseconds since the
    /// epoch, which is what a range of two Temporal sources of one type is made of.
    ///
    /// The interval patterns of the data are not consulted for these, which is what the `None` of
    /// [`DateTimeRenderer::range_parts_at`] says.
    pub fn format_range_nanoseconds_to_parts(
        &self,
        start: i128,
        start_zone: &FormatZone,
        end: i128,
        end_zone: &FormatZone,
    ) -> Option<Vec<DateTimePart>> {
        let (Some(start_time), Some(end_time)) = (
            civil_from_epoch_nanoseconds(start, start_zone.offset_seconds),
            civil_from_epoch_nanoseconds(end, end_zone.offset_seconds),
        ) else {
            return None;
        };

        Some(self.range_parts_at(start_time, start_zone, end_time, end_zone, None))
    }

    /// 11.5.8's `PartitionDateTimeRangePattern` of two civil ends, which 15.6.8 asks for in the same
    /// words of two Temporal moments.
    ///
    /// `interval` is the range formatter of this field set, or `None` where its patterns may not be
    /// consulted and 11.5.8's steps 21 to 23 are the whole of the answer, both ends spelled out. A
    /// range of nanoseconds is given `None` for two reasons: the CLDR interval patterns re-derive the
    /// fields of the pair from the skeleton they match instead of rendering the format's own pattern
    /// twice, so a field the format has and that pair does not - the second of a `short` time - is
    /// dropped; and their name tables are loaded for the years a time value reaches, while
    /// `Temporal.PlainDate` reaches ±274690 of them. `icu_datetime` asserts on a field whose names are
    /// missing, so the ends of such a range are spelled out here rather than handed to it.
    fn range_parts_at(
        &self,
        start_time: DateTime<Gregorian>,
        start_zone: &FormatZone,
        end_time: DateTime<Gregorian>,
        end_zone: &FormatZone,
        interval: Option<
            &icu_datetime::range::FixedCalendarDateRangeFormatter<Gregorian, CompositeFieldSet>,
        >,
    ) -> Vec<DateTimePart> {
        // The sides are what the two ends render on their own, which is the same information 11.5.8
        // reads out of the range pattern's placeholders. Table 17's `source` is not annotated by the
        // range formatter of this generation, so it is recovered from them.
        let start_side = self.format_at(start_time, start_zone);
        let end_side = self.format_at(end_time, end_zone);

        // 11.5.8 steps 13 to 16, ahead of any range pattern: the two ends are compared field by
        // field down Table 6, and where the fields the format reaches are the same the range is the
        // one date, spelled once with `[[Pattern]]` and with every part shared. A field set the data
        // has no range pattern for would otherwise fall into the two halves joined below, which says
        // twice what the format says once.
        if self.relevant_fields_equal(&start_time, &end_time) {
            let mut parts = start_side;
            for part in &mut parts {
                part.source = Some(RangeSource::Shared);
            }

            return parts;
        }

        let Some(range) = interval else {
            let mut parts = start_side.clone();

            parts.push(DateTimePart::new(PartKind::Literal, RANGE_CONNECTIVE));
            parts.extend(end_side.clone());

            assign_range_sources(&mut parts, &start_side, &end_side);

            return parts;
        };

        // Two ends of one zone that has not changed its offset between them have one name, and are
        // given one lookup moment so that ICU4X sees one zone and selects a range pattern rather than
        // falling back; where the offset did change, the two ends genuinely have two names.
        let name_time = if start_zone.same_zone_as(end_zone) {
            start_time
        } else {
            end_time
        };

        let start_input = ZonedDateTime {
            date: start_time.date,
            time: start_time.time,
            zone: start_zone.to_icu_named(name_time),
        };
        let end_input = ZonedDateTime {
            date: end_time.date,
            time: end_time.time,
            zone: end_zone.to_icu_named(name_time),
        };

        let mut collector = PartsCollector::new();
        let _ = range
            .format(&start_input, &end_input)
            .write_to_parts(&mut collector);

        let mut parts = collector.into_parts();
        self.apply_table_widths(&mut parts);

        assign_range_sources(&mut parts, &start_side, &end_side);

        parts
    }

    /// 11.5.9's `FormatDateTimeRange`: the partition of 11.5.8 joined, with no record made for
    /// either.
    pub fn format_range(
        &self,
        start: f64,
        start_zone: &FormatZone,
        end: f64,
        end_zone: &FormatZone,
    ) -> Option<String> {
        Some(join_parts(&self.format_range_parts(start, start_zone, end, end_zone)?))
    }

    /// 11.5.9's `FormatDateTimeRange` of two Temporal ends, which 15.6.8 has already asked to be the
    /// same type and has therefore given one format to both.
    pub fn format_range_nanoseconds(
        &self,
        start: i128,
        start_zone: &FormatZone,
        end: i128,
        end_zone: &FormatZone,
    ) -> Option<String> {
        Some(join_parts(
            &self.format_range_nanoseconds_to_parts(start, start_zone, end, end_zone)?,
        ))
    }

    /// 11.5.8's `relevantFieldsEqual`, which is the walk of steps 14 to 15 down Table 6.
    ///
    /// The walk runs from the year down to the lowest row the field set has a range pattern for,
    /// which is the lowest field the format renders, and it reads the rows above that one as well:
    /// the hour of a `{ year, month, day }` format is therefore nothing the range asks about, while
    /// the year of a `{ month, day }` format is, because it comes above the month. Below the lowest
    /// rendered field nothing is compared at all, so two ends a millisecond apart are one date for a
    /// format that stops at the second.
    ///
    /// The rows of the data this generation can name are the fields of the two groups, so the walk is
    /// read off [`DateTimeRenderer::date_fields`] and [`DateTimeRenderer::time_precision`] rather than
    /// off the pattern map, which 2.3 does not expose. A format that renders no field of the table at
    /// all - the zone's name, or a weekday with nothing beside it - has no row to stop at, and the
    /// whole civil time is compared. The fraction of the second is compared in full, which is what
    /// three digits of it are; one or two digits of it let a smaller difference pass.
    fn relevant_fields_equal(&self, x: &DateTime<Gregorian>, y: &DateTime<Gregorian>) -> bool {
        let lowest = self
            .date_fields
            .map(DateFieldsKey::table_row)
            .into_iter()
            .chain(self.time_precision.map(TimePrecisionKey::table_row))
            .max();

        let Some(lowest) = lowest else {
            return x == y;
        };

        (lowest < 1 || x.date.year() == y.date.year())
            && (lowest < 2 || x.date.month().ordinal == y.date.month().ordinal)
            && (lowest < 3 || x.date.day_of_month().0 == y.date.day_of_month().0)
            && (lowest < 4 || x.time.hour == y.time.hour)
            && (lowest < 5 || x.time.minute == y.time.minute)
            && (lowest < 6 || x.time.second == y.time.second)
            && (lowest < 7 || x.time.subsecond == y.time.subsecond)
    }

    /// 11.5.5's `FormatDateTimeToParts`, which is what `formatToParts` of 11.3.6 and `format` of
    /// 11.3.3 are both built on: the string is the parts joined, which is what 11.5.11's
    /// `FormatDateTimePattern` does with the same pattern.
    pub fn format(&self, epoch_milliseconds: f64, zone: &FormatZone) -> Vec<DateTimePart> {
        let Some(date_time) = civil_date_time(epoch_milliseconds, zone.offset_seconds) else {
            return Vec::new();
        };

        self.format_at(date_time, zone)
    }

    /// 15.6.4's `FormatDateTimePattern` of the epoch nanoseconds of one of 15.6.14's Value Format
    /// Records, which is the other moment this build renders: a Temporal source names its own civil
    /// fields, so its nanoseconds are made civil in the offset of 15.6.4's `timeZone` - the `+00:00`
    /// of `[[IsPlain]]`, which [`FormatZone::from_offset_seconds`] spells - rather than in the
    /// milliseconds of a time value.
    pub fn format_nanoseconds(
        &self,
        epoch_nanoseconds: i128,
        zone: &FormatZone,
    ) -> Vec<DateTimePart> {
        let Some(date_time) = civil_from_epoch_nanoseconds(epoch_nanoseconds, zone.offset_seconds)
        else {
            return Vec::new();
        };

        self.format_at(date_time, zone)
    }

    /// 11.5.5's `FormatDateTimeToParts` of a moment that is already civil in the zone it renders in,
    /// which both entries above reduce to.
    ///
    /// The civil date and time of the zone is given to ICU4X as a moment in that zone rather than
    /// as a UTC instant it converts itself: the conversion has already been done, and doing it
    /// twice, once here and once through a zone id ICU4X would look up in its own database, is
    /// how a rendering ends up with the hour of one zone and the name of another.
    fn format_at(&self, date_time: DateTime<Gregorian>, zone: &FormatZone) -> Vec<DateTimePart> {
        let input = ZonedDateTime {
            date: date_time.date,
            time: date_time.time,
            zone: zone.to_icu(date_time),
        };

        let mut collector = PartsCollector::new();
        match &self.narrow {
            Some(narrow) => {
                let _ = narrow
                    .names
                    .with_pattern_unchecked(&narrow.pattern)
                    .format(&input)
                    .try_write_to_parts(&mut collector);
            }
            None => {
                let _ = self.formatter.format(&input).write_to_parts(&mut collector);
            }
        }

        let mut parts = collector.into_parts();
        self.apply_table_widths(&mut parts);

        parts
    }

    /// The widths of Table 16 that the pattern of this generation does not carry, put onto a
    /// rendering of either the single or the range formatter.
    ///
    /// A CLDR date skeleton has one length for the whole of its group, so the width of one field
    /// cannot always be had from the pattern that renders it: `{ day: "2-digit" }` in a locale whose
    /// skeleton writes `d`, and `{ day: "numeric" }` in one whose length-keyed pattern writes `dd`,
    /// are both answered after the rendering rather than by a pattern this build does not have. The
    /// `h24` of Table 4 is the same kind of gap: 2.3 renders it as `H`, and the two cycles agree on
    /// every hour but the one this pass rewrites.
    fn apply_table_widths(&self, parts: &mut [DateTimePart]) {
        if self.two_digit_year {
            for part in &mut *parts {
                if part.kind == PartKind::Year {
                    part.value = short_year(&part.value);
                }
            }
        }

        for part in &mut *parts {
            if self.asked.asks_two_digits(part.kind) {
                widen_to_two_digits(&mut part.value);
            } else if self.asked.asks_one_digit(part.kind) {
                narrow_to_one_digit(&mut part.value);
            }
        }

        // The `k` cycle of Table 4's `h24`, which this ICU4X generation renders as `H`: the two agree
        // on every hour but midnight, which `k` numbers 24 rather than 0. Both spellings of the hour
        // are two digits wide or one, and `24` is the answer of either.
        if self.hour_cycle == Some(HourCycle::H24) {
            for part in &mut *parts {
                if part.kind == PartKind::Hour && part.value.trim().parse::<u32>().ok() == Some(0) {
                    part.value = "24".to_owned();
                }
            }
        }
    }

    /// 11.5.5's `FormatDateTimeToParts` joined, which is what 11.3.3's `format` and 20.4.1 to 20.4.3
    /// of `Date.prototype` return.
    pub fn format_to_string(&self, epoch_milliseconds: f64, zone: &FormatZone) -> String {
        join_parts(&self.format(epoch_milliseconds, zone))
    }

    /// [`DateTimeRenderer::format_to_string`] of the epoch nanoseconds of one of 15.6.14's Value
    /// Format Records.
    pub fn format_nanoseconds_to_string(
        &self,
        epoch_nanoseconds: i128,
        zone: &FormatZone,
    ) -> String {
        join_parts(&self.format_nanoseconds(epoch_nanoseconds, zone))
    }
}

/// The concatenation of a partition, which is the String 11.5.6's `FormatDateTime` and 11.5.9's
/// `FormatDateTimeRange` are defined as.
fn join_parts(parts: &[DateTimePart]) -> String {
    let mut text = String::new();
    for part in parts {
        text.push_str(&part.value);
    }

    text
}

/// The `source` of Table 17 put onto the parts of 11.5.8's range partition, which are read off the
/// two ends' own partitions of 11.5.5.
///
/// A range pattern either spells both ends out or merges them into one another, and the two shapes
/// answer `source` differently:
///
/// * Both ends spelled out - 11.5.8's fallback, and what ICU4X gives when the two ends are not the
///   same zone - is one half after the other with the connector between them: each part belongs to
///   the end it was rendered from, and only the connector is `shared`.
/// * A merged range pattern spells the text the two halves have in common once. `[[Day]]` of English
///   is the plain case: `Jan 3, 2019 – Jan 5, 2019` becomes `Jan 3 – 5, 2019`, where `Jan ` and
///   `, 2019` are the common prefix and suffix of the two standalone renderings and are shared, and
///   only the two days are each one end's own.
///
/// Which of the two the partition is, and to which end each of its parts belongs, is then read as:
///
/// * a range of two equal instants renders one date, and every part of it is `shared`;
/// * the merged pattern spells the head of the format once for both of its placeholders, which is
///   recognized by the partition being shorter than the two ends and their glue: the run the two ends
///   agree on at the front is then the part that was spelled once, and it is `shared`;
/// * what the pattern then spells like the start date is `startRange`, and what it ends with, spelled
///   like the end date, is `endRange`;
/// * what is left in between is the glue - the ` - ` of an English day range - which belongs to the
///   pattern rather than to either of its two sides.
fn assign_range_sources(parts: &mut [DateTimePart], start: &[DateTimePart], end: &[DateTimePart]) {
    if parts.len() >= start.len() + end.len() {
        let head_len = start.len().min(parts.len());
        let (head, rest) = parts.split_at_mut(head_len);

        for part in head {
            part.source = Some(RangeSource::StartRange);
        }

        let end_len = end.len().min(rest.len());
        let (glue, tail) = rest.split_at_mut(rest.len() - end_len);

        for part in glue {
            part.source = Some(RangeSource::Shared);
        }

        for part in tail {
            part.source = Some(RangeSource::EndRange);
        }

        return;
    }

    let prefix = start
        .iter()
        .zip(end)
        .take_while(|(side, other)| side.is_same_as(other))
        .count();

    let suffix = start[prefix..]
        .iter()
        .rev()
        .zip(end[prefix..].iter().rev())
        .take_while(|(side, other)| side.is_same_as(other))
        .count();

    // The runs the pattern keeps to each end, which are what is left of the two halves once the text
    // it spells once is taken off either side.
    let start_run = start.len().saturating_sub(prefix + suffix);
    let end_run = end.len().saturating_sub(prefix + suffix);

    let shared_head = prefix.min(parts.len());
    for part in &mut parts[..shared_head] {
        part.source = Some(RangeSource::Shared);
    }

    let start_end = (shared_head + start_run).min(parts.len());
    for part in &mut parts[shared_head..start_end] {
        part.source = Some(RangeSource::StartRange);
    }

    let shared_tail = suffix.min(parts.len() - start_end);
    let split = parts.len() - shared_tail;

    // The connector, and anything else the pattern spells once between the two runs.
    let end_begin = split.saturating_sub(end_run).max(start_end);
    for part in &mut parts[start_end..end_begin] {
        part.source = Some(RangeSource::Shared);
    }

    for part in &mut parts[end_begin..split] {
        part.source = Some(RangeSource::EndRange);
    }

    for part in &mut parts[split..] {
        part.source = Some(RangeSource::Shared);
    }
}

/// The last two digits of a rendered year, which is what a CLDR `yy` skeleton shows of it and what
/// `year: "2-digit"` asks for.
fn short_year(year: &str) -> String {
    let digits: Vec<char> = year.chars().collect();

    digits[digits.len().saturating_sub(2)..].iter().collect()
}

/// The same number with its zero in front, which is all that separates the `2-digit` of Table 16 from
/// the `numeric` of the same field.
///
/// The digits are counted at the front of the text rather than over all of it, because the affixes of
/// a locale's field stay where they are: the Danish day that renders `3.` is a two-digit `03.`, and
/// prefixing the whole text would put the zero outside the number.
fn widen_to_two_digits(text: &mut String) {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();

    if digits == 1 {
        text.insert(0, '0');
    }
}

/// The same number without the zero that a `dd` or `MM` skeleton puts in front of it, which is what
/// Table 16's `numeric` asks for and what [`widen_to_two_digits`] is the inverse of.
///
/// Exactly two digits are shortened, so nothing else a field renders as - the four of a `numeric`
/// year, the pair of a `2-digit` one, a day of the month that was already two - keeps every character
/// it was given.
fn narrow_to_one_digit(text: &mut String) {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();

    if digits == 2 && text.starts_with('0') {
        text.remove(0);
    }
}

/// The civil date and time of a JavaScript time value in a zone of a fixed offset.
///
/// The arithmetic is 20.4.2's `ToLocalTime` of the `Date` object - the same split of the millisecond
/// count into a day and a time of day - and the day number then becomes a year, a month and a day of
/// the month by Hinnant's `civil_from_days`, which is the algorithm 20.4.2.3's `MakeDate` is written
/// as and which is exact for every value a time value can hold.
fn civil_date_time(epoch_milliseconds: f64, offset_seconds: i32) -> Option<DateTime<Gregorian>> {
    if !epoch_milliseconds.is_finite() {
        return None;
    }

    // 15.6.5's `ℤ(ℝ(x) × 10⁶)` of a clipped time value, which is exact because a time value is a
    // whole number of milliseconds and 21.4.1.1 keeps it inside the integers an `f64` counts.
    civil_from_epoch_nanoseconds(epoch_milliseconds.trunc() as i128 * 1_000_000, offset_seconds)
}

/// The civil date and time of a count of nanoseconds since the epoch in a zone of a fixed offset,
/// which is 15.6.1's `ToLocalTime` of the same two arguments: the day the count falls in, and the
/// time of day inside it, both moved by the offset.
///
/// The nanoseconds of a Temporal source arrive here rather than the milliseconds of [`civil_date_time`]
/// because 15.6.15 to 15.6.20 name a count of nanoseconds as the thing they format, and a PlainDate of
/// the year 273970 is a day count an `f64` of milliseconds would round: the split below is exact for
/// every value Temporal reaches, and `civil_from_days` is exact for the days of both ranges.
fn civil_from_epoch_nanoseconds(
    epoch_nanoseconds: i128,
    offset_seconds: i32,
) -> Option<DateTime<Gregorian>> {
    const NANOOSECONDS_PER_DAY: i128 = 86_400_000_000_000;

    let mut day = epoch_nanoseconds.div_euclid(NANOOSECONDS_PER_DAY);
    let mut nanoseconds_of_day = epoch_nanoseconds.rem_euclid(NANOOSECONDS_PER_DAY);

    // The offset belongs to the day count and to the time of day, not to the sum: a zone of `-05:30`
    // moves a moment across midnight, and the half hour has to end up in the hours and minutes of
    // the civil time rather than in a fraction of a day.
    let offset = i128::from(offset_seconds) * 1_000_000_000;
    day += offset.div_euclid(NANOOSECONDS_PER_DAY);
    nanoseconds_of_day += offset.rem_euclid(NANOOSECONDS_PER_DAY);
    if nanoseconds_of_day >= NANOOSECONDS_PER_DAY {
        nanoseconds_of_day -= NANOOSECONDS_PER_DAY;
        day += 1;
    }

    let (year, month, day_of_month) = civil_from_days(i64::try_from(day).ok()?);

    let hour = nanoseconds_of_day / 3_600_000_000_000;
    let minute = (nanoseconds_of_day / 60_000_000_000) % 60;
    let second = (nanoseconds_of_day / 1_000_000_000) % 60;
    let nanosecond = nanoseconds_of_day % 1_000_000_000;

    // `Date::try_new_gregorian` validates the year against `-9999..=9999`, which is far short of the
    // ±275760 years 20.4.2.1's time value reaches. `try_from_fields` is the constructor ICU4X
    // documents as taking the wider range (its own is ±1_040_000), so the date is built from a field
    // bag rather than from the calendar-specific helper.
    let mut fields = icu_calendar::types::DateFields::default();
    fields.extended_year = Some(year);
    fields.ordinal_month = Some(month);
    fields.day = Some(day_of_month);
    let date = Date::try_from_fields(
        fields,
        icu_calendar::options::DateFromFieldsOptions::default(),
        Gregorian,
    )
    .ok()?;
    let time = Time::try_new(hour as u8, minute as u8, second as u8, nanosecond as u32).ok()?;

    Some(DateTime { date, time })
}

/// The proleptic Gregorian date of a count of days since 1970-01-01.
///
/// The cycle of 146_097 days is the 400 years of the Gregorian rule, which is what makes the year
/// come out of two divisions rather than out of a loop; the month is the `Mar..Feb` ordering the
/// same rule puts the leap day at the end of, which is why March is month 0 here.
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = i32::try_from(year_of_era + era * 400).unwrap_or(i32::MAX);
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_shift = (5 * day_of_year + 2) / 153;
    let day_of_month = u8::try_from(day_of_year - (153 * month_shift + 2) / 5 + 1).unwrap_or(1);
    // The month of Hinnant's algorithm is already the 1-based ordinal `Date::try_new_gregorian`
    // takes: March is `mp` 0 and answers 3, January is `mp` 10 and answers 1.
    let month = u8::try_from(month_shift + if month_shift < 10 { 3 } else { -9 }).unwrap_or(1);

    // The year of the era arithmetic counts from March, which is where the Gregorian cycle of 146_097
    // days it divides by starts: a January or a February belongs to the year after the one it is
    // holding, and that is the whole of the correction.
    let year = year + i32::from(month <= 2);

    (year, month, day_of_month)
}

/// One part of a rendered date and time, which is the `{ type, value }` record of 11.3.6's step 4
/// and, with a `source`, the record of 11.3.5's step 5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateTimePart {
    pub kind: PartKind,
    pub value: String,
    /// Which side of a range this part belongs to, of Table 17. `None` is a part of a single date,
    /// which has no sides and so no `source` row in its record.
    pub source: Option<RangeSource>,
}

impl DateTimePart {
    pub fn new(kind: PartKind, value: impl Into<String>) -> Self {
        Self { kind, value: value.into(), source: None }
    }

    /// Whether two parts of a partition are the same record, which for this question is the type and
    /// the text: the `source` of a range is derived from the parts of the two ends, so comparing it
    /// while it is being worked out would compare a half-filled answer with itself.
    fn is_same_as(&self, other: &Self) -> bool {
        self.kind == other.kind && self.value == other.value
    }
}

/// The `source` of Table 17, which is the third field of the records `formatRangeToParts` of 11.3.5
/// answers with and says which of the two dates a part of the range was rendered from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSource {
    Shared,
    StartRange,
    EndRange,
}

impl RangeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            RangeSource::Shared => "shared",
            RangeSource::StartRange => "startRange",
            RangeSource::EndRange => "endRange",
        }
    }
}

/// Which field of 11.3's Table 3 a part is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Literal,
    Era,
    Year,
    Month,
    Day,
    Weekday,
    Hour,
    Minute,
    Second,
    FractionalSecond,
    DayPeriod,
    TimeZoneName,
}

impl PartKind {
    /// The `type` of the record, which is the option name of 11.3's Table 3 rather than the CLDR
    /// symbol the part carries.
    pub fn name(self) -> &'static str {
        match self {
            PartKind::Literal => "literal",
            PartKind::Era => "era",
            PartKind::Year => "year",
            PartKind::Month => "month",
            PartKind::Day => "day",
            PartKind::Weekday => "weekday",
            PartKind::Hour => "hour",
            PartKind::Minute => "minute",
            PartKind::Second => "second",
            PartKind::FractionalSecond => "fractionalSecond",
            PartKind::DayPeriod => "dayPeriod",
            PartKind::TimeZoneName => "timeZoneName",
        }
    }

    /// The field an ICU4X datetime part names. The two spellings are the same words for every field
    /// of Table 3, which is why this is a lookup rather than a table; the one that is not is
    /// `relatedYear`, which is not a field of ECMA-402 and has no record.
    fn of_datetime(value: &str) -> Option<Self> {
        DATETIME_PART_NAMES
            .iter()
            .find(|(name, _)| *name == value)
            .map(|(_, kind)| *kind)
    }

    /// The field the digits of a numeric part belong to, which the collector is inside of when the
    /// digits arrive: the fraction is the `fractionalSecond` of 11.3.6's example, and everything
    /// else of a number is the field it is rendering.
    ///
    /// The separator of that same number is the exception, because 11.5.11's pattern of a fractional
    /// second is `ss.SSS` - the dot is a literal of the pattern rather than a digit of the `second`,
    /// and so is its own record instead of a piece of the field it sits in.
    fn of_decimal(value: &str, container: PartKind) -> PartKind {
        if container != PartKind::Literal {
            if value == decimal_parts::FRACTION.value {
                return PartKind::FractionalSecond;
            }
            if value == decimal_parts::DECIMAL.value {
                return PartKind::Literal;
            }
        }

        container
    }
}

/// The `icu_datetime` name of each field of 11.3's Table 3 and the [`PartKind`] it means. The names
/// are taken from the crate's own part constants rather than spelled out, so that a renderer that
/// annotates a field with the constant of the same crate and a lookup here cannot drift apart.
const DATETIME_PART_NAMES: [(&str, PartKind); 10] = [
    (datetime_parts::YEAR.value, PartKind::Year),
    (datetime_parts::MONTH.value, PartKind::Month),
    (datetime_parts::DAY.value, PartKind::Day),
    (datetime_parts::WEEKDAY.value, PartKind::Weekday),
    (datetime_parts::ERA.value, PartKind::Era),
    (datetime_parts::DAY_PERIOD.value, PartKind::DayPeriod),
    (datetime_parts::HOUR.value, PartKind::Hour),
    (datetime_parts::MINUTE.value, PartKind::Minute),
    (datetime_parts::SECOND.value, PartKind::Second),
    (datetime_parts::TIME_ZONE_NAME.value, PartKind::TimeZoneName),
];

/// The parts collector, which is what `formatToParts` is: the annotations of a rendered date become
/// the records of 11.3.6.
///
/// A date renders its numbers through the decimal tables of the locale, so the annotations arrive
/// nested - a `year` part whose text is decimal parts - and the ECMA-402 record of that field is one
/// piece of text with the field's name. That is what the `container` below carries while the digits
/// are written, and it is the same shape as the collector of
/// [`super::number_format_core`], which `writeable` defines for both.
struct PartsCollector {
    parts: Vec<DateTimePart>,
    /// The field whose text is being written now. A numeric field's digits and the separator between
    /// them are its own record; a named field's text is its record whatever it is written with.
    container: PartKind,
}

impl PartsCollector {
    fn new() -> Self {
        Self { parts: Vec::new(), container: PartKind::Literal }
    }

    fn into_parts(self) -> Vec<DateTimePart> {
        self.parts
    }

    /// One piece of text under one field name, with the merging of 11.5.11's
    /// `MergeAdjacentSameType`, which a rendered date needs because CLDR's literals come in runs:
    /// the `", "` of a long date and the narrow no-break space before a day period are two adjacent
    /// literals, and ECMA-402's parts show them as one.
    fn push(&mut self, kind: PartKind, text: &str) {
        if text.is_empty() {
            return;
        }

        match self.parts.last_mut() {
            Some(last) if last.kind == kind => last.value.push_str(text),
            _ => self.parts.push(DateTimePart::new(kind, text)),
        }
    }
}

impl PartsWrite for PartsCollector {
    type SubPartsWrite = Self;

    fn with_part(
        &mut self,
        part: Part,
        mut write: impl FnMut(&mut Self) -> std::fmt::Result,
    ) -> std::fmt::Result {
        // The datetime parts name the field they are; the decimal ones name a piece of a number,
        // which belongs to the field the collector is inside of.
        let outer = self.container;

        self.container = if part.category == "decimal" {
            PartKind::of_decimal(part.value, outer)
        } else {
            PartKind::of_datetime(part.value).unwrap_or(PartKind::Literal)
        };

        let result = write(self);
        self.container = outer;

        result
    }
}

impl std::fmt::Write for PartsCollector {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.push(self.container, text);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The civil parts of a moment in a zone of one offset, as `(year, month, day, hour, minute,
    /// second)`.
    fn civil(epoch_milliseconds: f64, offset_seconds: i32) -> (i32, u8, u8, u8, u8, u8) {
        let date_time = civil_date_time(epoch_milliseconds, offset_seconds).unwrap_or_else(|| {
            panic!("{epoch_milliseconds} in {offset_seconds} is not renderable")
        });

        (
            date_time.date.year().extended_year(),
            date_time.date.month().ordinal,
            date_time.date.day_of_month().0,
            date_time.time.hour.into(),
            date_time.time.minute.into(),
            date_time.time.second.into(),
        )
    }

    /// An option bag with nothing in it, which the tests fill in field by field.
    fn bag() -> Components {
        Components {
            weekday: None,
            era: None,
            year: None,
            month: None,
            day: None,
            hour: None,
            minute: None,
            second: None,
            fractional_second_digits: None,
            day_period: None,
            time_zone_name: None,
            date_style: None,
            time_style: None,
            hour_cycle: None,
            hour12: None,
        }
    }

    fn part(kind: PartKind, value: &str) -> DateTimePart {
        DateTimePart::new(kind, value)
    }

    #[test]
    fn the_day_number_of_the_epoch_is_the_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // 2000-03-01, the first day of the era Hinnant's algorithm counts from, and a leap day.
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(civil_from_days(8_400), (1992, 12, 31));
        assert_eq!(civil_from_days(18_261), (2019, 12, 31));
    }

    #[test]
    fn the_millisecond_grid_is_truncated_toward_zero() {
        // TimeClip's `ToIntegerOrInfinity`: -0.9 milliseconds is the millisecond 0, not the -1 a
        // floor would answer, and -1.5 is the -1 that is a whole millisecond inside it.
        assert_eq!(civil(-0.9, 0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(-1.5, 0), (1969, 12, 31, 23, 59, 59));
        assert_eq!(civil(0.9, 0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn a_half_hour_offset_moves_the_moment_across_midnight() {
        // The offset has to land in the hours and minutes of the civil time rather than in a
        // fraction of a day, which is what a `-05:30` does to a moment just after midnight UTC.
        assert_eq!(civil(0.0, -19_800), (1969, 12, 31, 18, 30, 0));
        assert_eq!(civil(0.0, 19_800), (1970, 1, 1, 5, 30, 0));
        assert_eq!(civil(1_612_321_506_070.0, 0), (2021, 2, 3, 3, 5, 6));
    }

    #[test]
    fn the_extreme_gregorian_dates_need_the_field_constructor() {
        // The two years 20.4.2.1's range reaches, which is where a format of a Date is refused or
        // not, before any of the rest of the rendering is involved. `civil_date_time` builds its
        // date from fields because the calendar-specific constructor stops at ±9999.
        for (year, month, day) in [(275_760, 9, 13), (-271_821, 4, 20), (2021, 2, 3)] {
            let mut fields = icu_calendar::types::DateFields::default();
            fields.extended_year = Some(year);
            fields.ordinal_month = Some(month);
            fields.day = Some(day);

            let date = Date::try_from_fields(
                fields,
                icu_calendar::options::DateFromFieldsOptions::default(),
                Gregorian,
            );
            assert!(date.is_ok(), "{year}-{month:02}-{day:02}: {:?}", date.err());

            let narrow = Date::try_new_gregorian(year, month, day);
            assert_eq!(
                narrow.is_ok(),
                (-9_999..=9_999).contains(&year),
                "{year}: the narrow constructor answers {narrow:?}"
            );
        }
    }

    #[test]
    fn every_time_value_a_date_can_hold_is_renderable() {
        // The two ends of 20.4.2.1's range, which a format of `Date` has to answer like any other.
        assert_eq!(civil(8.64e15, 0), (275760, 9, 13, 0, 0, 0));
        assert_eq!(civil(-8.64e15, 0), (-271_821, 4, 20, 0, 0, 0));
    }

    #[test]
    fn the_offset_grammar_of_the_spec() {
        // 11.5.3's `a` to `d`: the sign, one or two digits of hour, then an optional minute with the
        // colon optional and matching. The hour may reach 23, which is past ICU4X's own limit.
        for (text, seconds) in [
            ("+05", 18_000),
            ("-0530", -19_800),
            ("+05:30", 19_800),
            ("+00", 0),
            ("+23", 82_800),
            ("-21", -75_600),
            ("+22:27", 80_820),
            ("-22:23", -80_580),
        ] {
            let parsed = parse_offset(text).unwrap_or_else(|| panic!("{text} is an offset"));
            assert_eq!(parsed.zone.offset_seconds, seconds, "{text}");
            assert!(!parsed.seconds_written, "{text}");
        }

        for text in [
            // A one-digit hour is not in the grammar, which is two digits of hour; `Z` is neither
            // an offset nor an id the database has.
            "Z",
            "UTC",
            "gmt",
            "+",
            "-8",
            "+8",
            "+0",
            "+24",
            "-24:00",
            "+5:3",
            "+5:30",
            "+053",
            "+0:30",
            "05:30",
            "+05:30:1",
            "Europe/Rome",
        ] {
            assert!(parse_offset(text).is_none(), "{text} is not an offset");
        }

        // Seconds are in the grammar's reach but not in `FormatOffsetTimeZoneIdentifier`, so the
        // caller refuses them; `parse_offset` says that they were written rather than refusing.
        let written = parse_offset("+05:30:15").expect("+05:30:15 spells an offset");
        assert!(written.seconds_written);
        assert_eq!(written.zone.offset_seconds, 19_815);
    }

    #[test]
    fn an_offset_zone_is_named_by_its_offset() {
        assert_eq!(offset_name(0), "+00:00");
        assert_eq!(offset_name(-19_800), "-05:30");
        assert_eq!(offset_name(82_800), "+23:00");
        assert_eq!(offset_name(-80_580), "-22:23");

        // What ICU4X's `UtcOffset` refuses, and what therefore cannot carry a `timeZoneName` field.
        assert!(FormatZone::from_offset_seconds(64_800).offset_is_renderable());
        assert!(!FormatZone::from_offset_seconds(64_801).offset_is_renderable());
        assert!(!FormatZone::from_offset_seconds(-64_801).offset_is_renderable());
    }

    #[test]
    fn the_hour_cycle_a_locale_counts_by() {
        assert_eq!(hour_cycle_of_locale(DataLocale::try_from("en").unwrap()), Some(HourCycle::H12));
        assert_eq!(hour_cycle_of_locale(DataLocale::try_from("da").unwrap()), Some(HourCycle::H23));
        assert_eq!(hour_cycle_of_locale(DataLocale::try_from("it").unwrap()), Some(HourCycle::H23));
    }

    #[test]
    fn a_field_set_is_the_shape_of_the_bag() {
        // A bag of nothing has no format to match, and the refusals are the ones the instance reports
        // in words rather than rendering a different format.
        assert_eq!(FieldSetKey::for_components(&bag()), Err(FieldSetError::NoFields));

        // The narrow of Table 16 is not a length of a skeleton, so the bag is patterned at the longest
        // group and the width is carried beside it for the pattern rewrite. See [`NarrowWidths`].
        let mut narrow = bag();
        narrow.month = Some(Width::Narrow);
        let key = FieldSetKey::for_components(&narrow).expect("a narrow month");
        assert_eq!(key.length, Some(LengthKey::Long));
        assert_eq!(key.narrow, NarrowWidths { month: true, weekday: false, era: false });

        // The day period is the named family the rewrite has no answer for: its narrow is the phrase
        // of `bbbbb`, which no table of this build carries.
        let mut narrow_day_period = bag();
        narrow_day_period.hour = Some(Width::Numeric);
        narrow_day_period.day_period = Some(Width::Narrow);
        assert_eq!(
            FieldSetKey::for_components(&narrow_day_period),
            Err(FieldSetError::NarrowDayPeriod)
        );

        // One group at two *named* lengths is the shape a single skeleton cannot carry: the long
        // month beside the short weekday of the same date.
        let mut mixed = bag();
        mixed.month = Some(Width::Long);
        mixed.weekday = Some(Width::Short);
        mixed.day = Some(Width::Numeric);
        assert_eq!(FieldSetKey::for_components(&mixed), Err(FieldSetError::MixedWidths));

        // The digit widths are not that: `{ month: "long", day: "2-digit" }` is the long month with
        // the day padded on the way out, which the renderer does carry.
        let mut padded = bag();
        padded.month = Some(Width::Long);
        padded.day = Some(Width::TwoDigit);
        let key = FieldSetKey::for_components(&padded).expect("a long month with a padded day");
        assert_eq!(key.length, Some(LengthKey::Long));
        assert_eq!(key.asked.day, Some(Width::TwoDigit));

        let mut no_month = bag();
        no_month.year = Some(Width::Numeric);
        no_month.day = Some(Width::Numeric);
        assert_eq!(FieldSetKey::for_components(&no_month), Err(FieldSetError::UnsupportedShape));

        let mut date = bag();
        date.year = Some(Width::Numeric);
        date.month = Some(Width::Numeric);
        date.day = Some(Width::Numeric);
        let key = FieldSetKey::for_components(&date).expect("a plain date");
        assert!(key.to_icu().is_some());
        assert_eq!(key.hour_cycle, None);

        // The hour of a time group is what carries the hour cycle; a date has none to report.
        let mut time = bag();
        time.hour = Some(Width::Numeric);
        time.minute = Some(Width::Numeric);
        time.hour_cycle = Some(HourCycle::H23);
        let key = FieldSetKey::for_components(&time).expect("a plain time");
        assert!(key.to_icu().is_some());
        assert_eq!(key.hour_cycle, Some(HourCycle::H23));
    }

    #[test]
    fn a_merged_range_shares_what_both_ends_spelled_once() {
        // `Jan 3, 2019` and `Jan 5, 2019` merged into `Jan 3 – 5, 2019`: the month and its space are
        // common at the head, the comma and the year at the tail, and only the days are each end's.
        let start = vec![
            part(PartKind::Month, "Jan"),
            part(PartKind::Literal, " "),
            part(PartKind::Day, "3"),
            part(PartKind::Literal, ", "),
            part(PartKind::Year, "2019"),
        ];
        let end = vec![
            part(PartKind::Month, "Jan"),
            part(PartKind::Literal, " "),
            part(PartKind::Day, "5"),
            part(PartKind::Literal, ", "),
            part(PartKind::Year, "2019"),
        ];

        let mut merged = vec![
            part(PartKind::Month, "Jan"),
            part(PartKind::Literal, " "),
            part(PartKind::Day, "3"),
            part(PartKind::Literal, " – "),
            part(PartKind::Day, "5"),
            part(PartKind::Literal, ", "),
            part(PartKind::Year, "2019"),
        ];
        assign_range_sources(&mut merged, &start, &end);

        let sources: Vec<&str> = merged
            .iter()
            .map(|part| part.source.unwrap().as_str())
            .collect();
        assert_eq!(
            sources,
            [
                "shared",
                "shared",
                "startRange",
                "shared",
                "endRange",
                "shared",
                "shared"
            ]
        );
    }

    #[test]
    fn a_range_of_one_date_is_one_date_whatever_the_format_leaves_out() {
        // 11.5.8 step 15 walks Table 6 down to the lowest field the format renders and no further:
        // the two ends of a `{ year, month, day }` range an afternoon apart are one date, because
        // the format says nothing about the hour. The rows above the rendered fields are read all the
        // same, so the same day of two centuries is two dates even for a format that never shows the
        // year, and a range of one day that crosses into the next is two dates for the same format.
        let zone = FormatZone::from_offset_seconds(0);
        let morning = 1_771_400_000_000.0_f64; // 2026-02-18 at 07:33:20

        let mut date = bag();
        date.year = Some(Width::Numeric);
        date.month = Some(Width::Short);
        date.day = Some(Width::Numeric);
        let key = FieldSetKey::for_components(&date).expect("a full date");
        let renderer = DateTimeRenderer::try_new("en", key).expect("the rendering of a full date");

        let afternoon = morning + 5.5 * 3_600_000.0;
        assert_eq!(
            renderer.format_range(morning, &zone, afternoon, &zone),
            renderer.format_range(morning, &zone, morning, &zone),
            "the hour is not rendered, so it is not two dates either"
        );

        let next_day = renderer
            .format_range_parts(morning, &zone, morning + 86_400_000.0, &zone)
            .expect("a range over two days");
        assert!(
            !next_day
                .iter()
                .all(|part| part.source == Some(RangeSource::Shared)),
            "a day the format renders is worth two ends: {next_day:?}"
        );

        let mut month_day = bag();
        month_day.month = Some(Width::Short);
        month_day.day = Some(Width::Numeric);
        let key = FieldSetKey::for_components(&month_day).expect("a month and a day");
        let renderer =
            DateTimeRenderer::try_new("en", key).expect("the rendering of a month and a day");

        let year_apart = renderer
            .format_range_parts(morning, &zone, morning + 365.0 * 86_400_000.0, &zone)
            .expect("the same day of the next year");
        assert!(
            !year_apart
                .iter()
                .all(|part| part.source == Some(RangeSource::Shared)),
            "the century above the month counts even when it is not rendered: {year_apart:?}"
        );
    }

    #[test]
    fn a_range_whose_two_ends_are_one_instant_is_spelled_once() {
        // A bag of minute and second has no range pattern in this data, so its range would fall into
        // the two halves joined by the connector - twice the same date - if 11.5.8 step 16 did not
        // answer it before the pattern is looked at.
        let mut time = bag();
        time.minute = Some(Width::Numeric);
        time.second = Some(Width::Numeric);
        let key = FieldSetKey::for_components(&time).expect("a minute and a second");
        let renderer =
            DateTimeRenderer::try_new("en", key).expect("the rendering of a minute and a second");

        let zone = FormatZone::from_offset_seconds(0);
        let start = 1_771_400_000_000.0_f64;

        let same = renderer
            .format_range_parts(start, &zone, start, &zone)
            .expect("two ends of one instant");
        assert!(
            same.iter()
                .all(|part| part.source == Some(RangeSource::Shared)),
            "every part of one instant is shared: {same:?}"
        );
        assert!(
            !same
                .iter()
                .any(|part| part.value.contains(RANGE_CONNECTIVE.trim_end())),
            "one instant is spelled once: {same:?}"
        );

        let apart = renderer
            .format_range_parts(start, &zone, start + 60_000.0, &zone)
            .expect("a minute apart");
        assert!(
            apart
                .iter()
                .any(|part| part.source == Some(RangeSource::StartRange))
        );
        assert!(
            apart
                .iter()
                .any(|part| part.source == Some(RangeSource::EndRange))
        );
    }

    #[test]
    fn a_range_of_one_instant_is_one_date_and_a_fallback_is_two() {
        let start = vec![
            part(PartKind::Month, "Aug"),
            part(PartKind::Literal, " "),
            part(PartKind::Day, "10"),
            part(PartKind::Literal, ", "),
            part(PartKind::Year, "2019"),
        ];

        // The two ends are the same date: the range pattern says it once, and once is all shared.
        let mut single = start.clone();
        assign_range_sources(&mut single, &start, &start);
        assert!(
            single
                .iter()
                .all(|part| part.source == Some(RangeSource::Shared))
        );

        // Both ends spelled out, which is 11.5.8's fallback: each half is its own side and only the
        // connector between them is shared.
        let end = vec![
            part(PartKind::Month, "Jan"),
            part(PartKind::Literal, " "),
            part(PartKind::Day, "5"),
            part(PartKind::Literal, ", "),
            part(PartKind::Year, "2019"),
        ];
        let mut both = start.clone();
        both.push(part(PartKind::Literal, " – "));
        both.extend(end.clone());
        assign_range_sources(&mut both, &start, &end);

        let sources: Vec<&str> = both
            .iter()
            .map(|part| part.source.unwrap().as_str())
            .collect();
        assert_eq!(
            sources,
            [
                "startRange",
                "startRange",
                "startRange",
                "startRange",
                "startRange",
                "shared",
                "endRange",
                "endRange",
                "endRange",
                "endRange",
                "endRange",
            ]
        );
    }

    #[test]
    fn a_bag_of_the_time_group_is_no_format_for_a_civil_type_of_the_date_group() {
        // 15.6.1's `required` of `Temporal.PlainDate` is `date`, and `inherit` is `relevant`: a bag
        // whose only component is the hour has nothing to copy into that format and nothing of the
        // bag to keep, which is the null those handlers report as a TypeError.
        let mut asked = bag();
        asked.hour = Some(Width::Numeric);
        assert!(
            temporal_components(&asked, TemporalKind::Date).is_none(),
            "an hour is no part of a PlainDate"
        );

        // The same bag with a month beside it is a format of the month alone: 15.6.1 copies the
        // relevant components over, and its `defaults` are only needed when the bag asked for nothing
        // at all or `inherit` was `all`. This is what `temporal-objects-not-overlapping-options.js`
        // means by "a PlainDate formats like `{ month }` alone".
        asked.month = Some(Width::TwoDigit);
        let date =
            temporal_components(&asked, TemporalKind::Date).expect("a month is a date field");
        assert_eq!(date.month, Some(Width::TwoDigit), "the month the bag asked for");
        assert_eq!(date.hour, None, "the hour a PlainDate has no field for is left behind");
        assert_eq!(
            date.day, None,
            "a bag that named one relevant field gets that field, not the whole date group"
        );

        // An empty bag is the other half of 15.6.1: `required` and `defaults` are both `date`, so the
        // type's own three fields are filled in at `numeric`.
        let filled =
            temporal_components(&bag(), TemporalKind::Date).expect("a bag of nothing has a date");
        assert_eq!(
            filled.month,
            Some(Width::Numeric),
            "15.6.1's `defaults` fill what was asked for"
        );
        assert_eq!(filled.day, Some(Width::Numeric), "of the whole date group");

        // `inherit` of `all` is 15.6.20's answer for an instant, which takes the whole bag because an
        // instant has every field, zone name included.
        let mut asked = bag();
        asked.time_zone_name = Some(TimeZoneNameStyle::Short);
        let instant =
            temporal_components(&asked, TemporalKind::Instant).expect("an instant has a zone");
        assert_eq!(
            instant.time_zone_name,
            Some(TimeZoneNameStyle::Short),
            "the zone name of the bag is the format's own"
        );

        // A `Temporal.PlainTime` has no date group at all, so a bag of a month and a day is refused
        // even though both are components of Table 16.
        let mut asked = bag();
        asked.month = Some(Width::Long);
        asked.day = Some(Width::Numeric);
        assert!(
            temporal_components(&asked, TemporalKind::Time).is_none(),
            "a PlainDate's fields are no part of a PlainTime"
        );
    }

    #[test]
    fn a_range_of_nanoseconds_renders_the_fields_it_has_at_both_ends() {
        let zone = FormatZone::from_offset_seconds(0);

        // A second is a field of this format, and the interval pattern of a pair whose greatest
        // difference is the hour does not carry one: 11.5.8's fallback renders the format's own
        // pattern twice, which is what 15.6.8's `FormatDateTimePattern` of `[[pattern]]` says.
        let mut time = bag();
        time.hour = Some(Width::Numeric);
        time.minute = Some(Width::TwoDigit);
        time.second = Some(Width::TwoDigit);
        let key = FieldSetKey::for_components(&time).expect("an hour, a minute and a second");
        let renderer =
            DateTimeRenderer::try_new("en", key).expect("the rendering of a time of day");

        let start = 1_628_037_045_000_000_000_i128;
        let text = renderer
            .format_range_nanoseconds(start, &zone, start + 82_800_000_000_000, &zone)
            .expect("a range over most of a day");
        assert!(
            text.contains("12:30:45") && text.contains("11:30:45"),
            "both ends keep the second of the format: {text}"
        );

        // The two years a `Date` stops at, as the nanoseconds of 15.6.14's record, and a weekday beside
        // them: the interval patterns of this data have no name for that field at those years, and
        // `icu_datetime` asserts on a field whose names are missing. Spelled out, both ends answer.
        let mut date = bag();
        date.weekday = Some(Width::Long);
        date.year = Some(Width::Numeric);
        date.month = Some(Width::Long);
        date.day = Some(Width::Numeric);
        let key = FieldSetKey::for_components(&date).expect("a weekday date");
        let renderer =
            DateTimeRenderer::try_new("en", key).expect("the rendering of a weekday date");

        let extremes = renderer
            .format_range_nanoseconds(
                -8_640_000_000_000_000_000_000,
                &zone,
                8_640_000_000_000_000_000_000,
                &zone,
            )
            .expect("the range of the two ends of a time value");
        assert!(
            extremes.contains("Tuesday") && extremes.contains("Saturday"),
            "both ends of the range name their weekday: {extremes}"
        );
        assert!(
            extremes.contains("271822") && extremes.contains("275760"),
            "the years a `Date` writes as 1970 plus or minus 273 000 are rendered as themselves: \
             {extremes}"
        );
    }
}
