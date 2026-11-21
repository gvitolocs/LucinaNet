//! The arithmetic of `Intl.NumberFormat` (https://tc39.es/ecma402/#numberformat-objects), section
//! 16, on `fixed_decimal` and `icu_decimal` with no `Context` in sight: the half that is testable
//! as plain Rust. The object that reads the options works out a [`NumberFormatSlots`] and hands
//! that and an [`IntlMathValue`] to the renderer below.
//!
//! Two things ECMA-402 asks locale data for are not in the data this build bakes, and are said so
//! here rather than pretended to be read:
//!
//! - The NaN and infinity strings of 16.5.3 steps 2-4, which are ILD values. The number-symbols
//!   marker `icu_decimal` 2.1 reads carries separators, sign affixes and grouping sizes only, so the
//!   one string below serves NaN and the one after it serves infinity for every locale built here.
//!   They are the spellings V8 and SpiderMonkey use for English, and SpiderMonkey uses them for every
//!   locale.
//! - The percent pattern of 16.2.3's `[[percent]]` leaves, for which `icu_decimal` has no marker
//!   either. Across the locales built here the pattern is `#,##0%` except in Danish, whose CLDR
//!   pattern is `#,##0 %` with U+00A0 NO-BREAK SPACE, so the only per-locale piece of it is the one
//!   string [`DecimalRenderer::percent_spacing`] holds.
//!
//! Everything else — the separators, the grouping positions and sizes, the plus and minus affixes
//! and their position around the number — comes from the baked CLDR data through `icu_decimal`.
//!
//! One thing the spec allows unlimited is capped here, and is said so because it is a difference
//! someone will otherwise read as a wrong digit: `fixed_decimal` keeps digit positions in an
//! `i16`. [`MAX_EXACT_DIGITS`] is that limit. A literal with more digits than it loses its least
//! significant ones, which no option can round at a position low enough to notice; a `BigInt` with
//! more than it is reported rather than formatted.

use std::str::FromStr;

use brimstone_intl_data::IntlDataProvider;
use fixed_decimal::{Decimal, RoundingIncrement, Sign, UnsignedDecimal, UnsignedRoundingMode};
use icu_decimal::{
    CompactDecimalFormatter, DecimalFormatter,
    options::{DecimalFormatterOptions, GroupingStrategy},
    provider::{
        DecimalCompactLongV1, DecimalCompactShortV1, DecimalDigitsV1, DecimalSymbols,
        DecimalSymbolsV1,
    },
};
use icu_locale::Locale;
use icu_plurals::provider::PluralsCardinalV1;
use icu_provider::prelude::{DataError, DataProvider, DataRequest, DataResponse};
use writeable::{Part, PartsWrite, Writeable};

/// The NaN string of 16.5.3 step 2.1, which is ILD and so is left to the implementation.
pub(crate) const NAN_STRING: &str = "NaN";

/// The infinity string of 16.5.3 steps 3.1 and 4.1. The negative pattern adds the locale's minus
/// affix in front of it, which is what 16.5.10's sign selection is for, so one string is enough.
pub(crate) const INFINITY_STRING: &str = "∞";

/// The most digits a value can have here. `fixed_decimal` keeps digit positions in an `i16` and the
/// rounding positions below move a few digits around the value's own magnitude, so the limit sits
/// well inside it. See the module documentation for what happens to a value past it.
pub const MAX_EXACT_DIGITS: usize = 30_000;

/// [[Style]] (16.1.3 step 2) for the styles this build has patterns for. `unit` is absent rather
/// than rendered with the decimal machinery: a unit string that is only the number would report
/// something the locale data never said. The constructor answers RangeError for it instead; see
/// [`Style::NAMES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Decimal,
    Percent,
    Currency,
}

impl Style {
    /// The four values 16.1.3 step 2 allows. One of them is named here and has no variant, because
    /// the constructor reports why: see `Style::from_name`.
    pub const NAMES: [&'static str; 4] = ["decimal", "percent", "currency", "unit"];

    /// The name Table 28's `style` row gives, which 16.3.2 reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Style::Decimal => "decimal",
            Style::Percent => "percent",
            Style::Currency => "currency",
        }
    }

    /// The variant a name of [`Style::NAMES`] means, or `None` for the style this build has no
    /// patterns for. `None` is not "not a style": the constructor has already checked the name
    /// against the list, so it is the RangeError of an unsupported style.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "decimal" => Some(Style::Decimal),
            "percent" => Some(Style::Percent),
            "currency" => Some(Style::Currency),
            _ => None,
        }
    }
}

/// [[Currency]] (16.1.3 steps 5 and 12.1): an ISO 4217 code, kept as its three uppercase ASCII
/// letters, which is what 16.1.3 step 12.1's `ASCII-uppercase` of the option leaves and what
/// [`super::number_format_currency`] asks the baked data with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CurrencyCode([u8; 3]);

impl CurrencyCode {
    /// 16.1.3 step 5.1's `IsWellFormedCurrencyCode`
    /// (https://tc39.es/ecma402/#sec-iswellformedcurrencycode) and step 12.1's uppercasing in one:
    /// three ASCII letters or nothing. The letters are case-insensitive in the option, and the code
    /// is only ever kept uppercase, so `xyz` and `XYZ` are the same currency here.
    ///
    /// A code this build has no data for is still a code: 16.5.10 step 5 falls back to the pattern
    /// of the locale and 16.5.3 step 9.11's "use currency itself" is what prints the letters, so
    /// `XYZ` formats as `XYZ 1,234.50` rather than being refused.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();

        if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_alphabetic) {
            return None;
        }

        let mut code = [b' '; 3];
        for (stored, letter) in code.iter_mut().zip(bytes) {
            *stored = letter.to_ascii_uppercase();
        }

        Some(Self(code))
    }

    /// The code as the String 16.3.2 reports and the data requests are keyed by. The bytes are
    /// letters, so the UTF-8 check that `from_utf8` performs cannot fail here; the empty string it
    /// falls back to is unreachable and only keeps an `unwrap` out of the engine.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0[..]).unwrap_or_default()
    }
}

/// [[CurrencyDisplay]] (16.1.3 steps 6 and 12.2): which of the four representations of a currency
/// the pattern puts around the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CurrencyDisplay {
    /// The locale's symbol for the currency, which for US dollars in English is `$`.
    Symbol,
    /// The symbol without whatever makes it distinguishable from other dollars, `narrowSymbol` of
    /// 16.1.3 step 6. In English both give `$`; in Danish the symbol is `US$` and the narrow form
    /// `$`.
    NarrowSymbol,
    /// The ISO 4217 code itself.
    Code,
    /// The name of the currency in the locale's language, pluralized for the number.
    Name,
}

impl CurrencyDisplay {
    /// The four values 16.1.3 step 6 lists, in the order the spec puts them.
    pub const NAMES: [&'static str; 4] = ["code", "symbol", "narrowSymbol", "name"];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "code" => Some(CurrencyDisplay::Code),
            "symbol" => Some(CurrencyDisplay::Symbol),
            "narrowSymbol" => Some(CurrencyDisplay::NarrowSymbol),
            "name" => Some(CurrencyDisplay::Name),
            _ => None,
        }
    }

    /// The name Table 28's `currencyDisplay` row gives.
    pub fn as_str(self) -> &'static str {
        match self {
            CurrencyDisplay::Code => "code",
            CurrencyDisplay::Symbol => "symbol",
            CurrencyDisplay::NarrowSymbol => "narrowSymbol",
            CurrencyDisplay::Name => "name",
        }
    }
}

/// [[CurrencySign]] (16.1.3 steps 7 and 12.3), which picks the second set of patterns of the
/// locale's currency data: the accounting one puts a negative amount between parentheses instead of
/// marking it with a minus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CurrencySign {
    Standard,
    Accounting,
}

impl CurrencySign {
    /// The two values 16.1.3 step 7 lists.
    pub const NAMES: [&'static str; 2] = ["standard", "accounting"];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "standard" => Some(CurrencySign::Standard),
            "accounting" => Some(CurrencySign::Accounting),
            _ => None,
        }
    }

    /// The name Table 28's `currencySign` row gives.
    pub fn as_str(self) -> &'static str {
        match self {
            CurrencySign::Standard => "standard",
            CurrencySign::Accounting => "accounting",
        }
    }
}

/// [[Notation]] (16.1.1 steps 11 and 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Notation {
    Standard,
    Compact,
}

impl Notation {
    /// The two values of 16.1.1 step 11 this build renders; the two exponent notations are refused
    /// by the constructor rather than listed here.
    pub const NAMES: [&'static str; 2] = ["standard", "compact"];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "standard" => Some(Notation::Standard),
            "compact" => Some(Notation::Compact),
            _ => None,
        }
    }

    /// The name Table 28's `notation` row gives.
    pub fn as_str(self) -> &'static str {
        match self {
            Notation::Standard => "standard",
            Notation::Compact => "compact",
        }
    }
}

/// [[CompactDisplay]] (16.1.1 steps 17 and 19.1), which says whether the piece naming the scale is
/// the abbreviation ("1.2K") or the word ("1.2 thousand"), and with it which of the two compact
/// pattern lists of the locale is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompactDisplay {
    Short,
    Long,
}

impl CompactDisplay {
    /// The two values 16.1.1 step 17 lists.
    pub const NAMES: [&'static str; 2] = ["short", "long"];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "short" => Some(CompactDisplay::Short),
            "long" => Some(CompactDisplay::Long),
            _ => None,
        }
    }

    /// The name Table 28's `compactDisplay` row gives.
    pub fn as_str(self) -> &'static str {
        match self {
            CompactDisplay::Short => "short",
            CompactDisplay::Long => "long",
        }
    }
}

/// [[RoundingType]] (16.1.2 steps 23-27), which says which of the two roundings 16.5.2 applies, or
/// that both are applied and one is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundingType {
    FractionDigits,
    SignificantDigits,
    MorePrecision,
    LessPrecision,
}

/// [[ComputedRoundingPriority]] (16.1.2 steps 23-27), which is what `resolvedOptions` reports and
/// can be "morePrecision" where the option said "auto".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundingPriority {
    Auto,
    MorePrecision,
    LessPrecision,
}

impl RoundingPriority {
    /// The three values 16.1.2 step 24 allows.
    pub const NAMES: [&'static str; 3] = ["auto", "morePrecision", "lessPrecision"];

    /// The variant a name of [`RoundingPriority::NAMES`] means.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(RoundingPriority::Auto),
            "morePrecision" => Some(RoundingPriority::MorePrecision),
            "lessPrecision" => Some(RoundingPriority::LessPrecision),
            _ => None,
        }
    }

    /// The name Table 28's `roundingPriority` row gives.
    pub fn as_str(self) -> &'static str {
        match self {
            RoundingPriority::Auto => "auto",
            RoundingPriority::MorePrecision => "morePrecision",
            RoundingPriority::LessPrecision => "lessPrecision",
        }
    }
}

/// [[UseGrouping]] (16.1.1 steps 18-24), after the `true` and `false` of the older spelling have
/// been replaced and a `true` has become "always".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UseGrouping {
    /// `false`
    Never,
    /// "min2"
    Min2,
    /// "auto"
    Auto,
    /// "always", and the `true` that became it
    Always,
}

impl UseGrouping {
    /// The name Table 28's `useGrouping` row gives, or `None` for the Boolean `false`, which is
    /// reported as `false` and not as a string.
    pub fn resolved_name(self) -> Option<&'static str> {
        match self {
            UseGrouping::Never => None,
            UseGrouping::Min2 => Some("min2"),
            UseGrouping::Auto => Some("auto"),
            UseGrouping::Always => Some("always"),
        }
    }

    fn grouping_strategy(self) -> GroupingStrategy {
        match self {
            UseGrouping::Never => GroupingStrategy::Never,
            UseGrouping::Min2 => GroupingStrategy::Min2,
            UseGrouping::Auto => GroupingStrategy::Auto,
            UseGrouping::Always => GroupingStrategy::Always,
        }
    }

    /// Whether the locale's own `minimumGroupingDigits` has to be lowered to render this choice.
    ///
    /// `icu_decimal` 2.1 documents `GroupingStrategy::Always` as behaving like `Auto`, so it still
    /// respects the locale's minimum, but `"always"` asks for grouping whenever there are enough
    /// digits for one. For Italian, whose minimum is 2, that is the difference between "1234" and
    /// "1.234"; the only way to get the second out of this data is to lower the number the grouper
    /// reads.
    fn needs_grouping_of_four_digit_numbers(self) -> bool {
        self == UseGrouping::Always
    }
}

/// [[SignDisplay]] (16.1.1 steps 25-26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignDisplay {
    Auto,
    Never,
    Always,
    ExceptZero,
    Negative,
}

impl SignDisplay {
    /// The five values 16.1.1 step 25 allows.
    pub const NAMES: [&'static str; 5] = ["auto", "never", "always", "exceptZero", "negative"];

    /// The variant a name of [`SignDisplay::NAMES`] means.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(SignDisplay::Auto),
            "never" => Some(SignDisplay::Never),
            "always" => Some(SignDisplay::Always),
            "exceptZero" => Some(SignDisplay::ExceptZero),
            "negative" => Some(SignDisplay::Negative),
            _ => None,
        }
    }

    /// The name Table 28's `signDisplay` row gives.
    pub fn as_str(self) -> &'static str {
        match self {
            SignDisplay::Auto => "auto",
            SignDisplay::Never => "never",
            SignDisplay::Always => "always",
            SignDisplay::ExceptZero => "exceptZero",
            SignDisplay::Negative => "negative",
        }
    }
}

/// [[TrailingZeroDisplay]] (16.1.2 steps 10 and 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailingZeroDisplay {
    Auto,
    StripIfInteger,
}

impl TrailingZeroDisplay {
    /// The two values 16.1.2 step 10 allows.
    pub const NAMES: [&'static str; 2] = ["auto", "stripIfInteger"];

    /// The variant a name of [`TrailingZeroDisplay::NAMES`] means.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(TrailingZeroDisplay::Auto),
            "stripIfInteger" => Some(TrailingZeroDisplay::StripIfInteger),
            _ => None,
        }
    }

    /// The name Table 28's `trailingZeroDisplay` row gives.
    pub fn as_str(self) -> &'static str {
        match self {
            TrailingZeroDisplay::Auto => "auto",
            TrailingZeroDisplay::StripIfInteger => "stripIfInteger",
        }
    }
}

/// [[RoundingMode]] (16.1.2 steps 8 and 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundingMode {
    Ceil,
    Floor,
    Expand,
    Trunc,
    HalfCeil,
    HalfFloor,
    HalfExpand,
    HalfTrunc,
    HalfEven,
}

impl RoundingMode {
    /// The nine values 16.1.2 step 13 allows.
    pub const NAMES: [&'static str; 9] = [
        "ceil",
        "floor",
        "expand",
        "trunc",
        "halfCeil",
        "halfFloor",
        "halfExpand",
        "halfTrunc",
        "halfEven",
    ];

    /// The variant a name of [`RoundingMode::NAMES`] means.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "ceil" => Some(RoundingMode::Ceil),
            "floor" => Some(RoundingMode::Floor),
            "expand" => Some(RoundingMode::Expand),
            "trunc" => Some(RoundingMode::Trunc),
            "halfCeil" => Some(RoundingMode::HalfCeil),
            "halfFloor" => Some(RoundingMode::HalfFloor),
            "halfExpand" => Some(RoundingMode::HalfExpand),
            "halfTrunc" => Some(RoundingMode::HalfTrunc),
            "halfEven" => Some(RoundingMode::HalfEven),
            _ => None,
        }
    }

    /// The name Table 28's `roundingMode` row gives.
    pub fn as_str(self) -> &'static str {
        match self {
            RoundingMode::Ceil => "ceil",
            RoundingMode::Floor => "floor",
            RoundingMode::Expand => "expand",
            RoundingMode::Trunc => "trunc",
            RoundingMode::HalfCeil => "halfCeil",
            RoundingMode::HalfFloor => "halfFloor",
            RoundingMode::HalfExpand => "halfExpand",
            RoundingMode::HalfTrunc => "halfTrunc",
            RoundingMode::HalfEven => "halfEven",
        }
    }

    /// GetUnsignedRoundingMode (https://tc39.es/ecma402/#sec-getunsignedroundingmode), 16.5.16: the
    /// row of Table 31 for this mode and the sign of the value, named the way `fixed_decimal` names
    /// the five unsigned modes it has.
    ///
    /// What gets rounded is the absolute value (16.5.2 step 3.3), so a mode that looks to one
    /// infinity looks to the other once the sign is back: `ceil` and `floor` swap between expand and
    /// trunc, and so do `halfCeil` and `halfFloor` on a tie. In the unsigned domain the two tie-break
    /// directions of Table 31 are the ones `fixed_decimal` calls half-expand (away from zero) and
    /// half-trunc (towards it), which is all the distinction that survives here.
    fn unsigned(self, negative: bool) -> UnsignedRoundingMode {
        match self {
            RoundingMode::Ceil => {
                if negative {
                    UnsignedRoundingMode::Trunc
                } else {
                    UnsignedRoundingMode::Expand
                }
            }
            RoundingMode::Floor => {
                if negative {
                    UnsignedRoundingMode::Expand
                } else {
                    UnsignedRoundingMode::Trunc
                }
            }
            RoundingMode::Expand => UnsignedRoundingMode::Expand,
            RoundingMode::Trunc => UnsignedRoundingMode::Trunc,
            RoundingMode::HalfCeil => {
                if negative {
                    UnsignedRoundingMode::HalfTrunc
                } else {
                    UnsignedRoundingMode::HalfExpand
                }
            }
            RoundingMode::HalfFloor => {
                if negative {
                    UnsignedRoundingMode::HalfExpand
                } else {
                    UnsignedRoundingMode::HalfTrunc
                }
            }
            RoundingMode::HalfExpand => UnsignedRoundingMode::HalfExpand,
            RoundingMode::HalfTrunc => UnsignedRoundingMode::HalfTrunc,
            RoundingMode::HalfEven => UnsignedRoundingMode::HalfEven,
        }
    }
}

/// [[RoundingIncrement]] (16.1.2 steps 6-7). Only the values that step lists are possible, and each
/// of them is a 1, 2, 5 or 25 times a power of ten, which is what `fixed_decimal` rounds on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Increment {
    One,
    Two,
    Five,
    Ten,
    Twenty,
    TwentyFive,
    Fifty,
    Hundred,
    TwoHundred,
    TwoHundredFifty,
    FiveHundred,
    Thousand,
    TwoThousand,
    TwoThousandFiveHundred,
    FiveThousand,
}

impl Increment {
    /// The list 16.1.2 steps 6-7 allows, in its order.
    pub const VALUES: [u32; 15] = [
        1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000, 2500, 5000,
    ];

    /// The number Table 28's `roundingIncrement` row gives, as a Number.
    pub fn as_u32(self) -> u32 {
        match self {
            Increment::One => 1,
            Increment::Two => 2,
            Increment::Five => 5,
            Increment::Ten => 10,
            Increment::Twenty => 20,
            Increment::TwentyFive => 25,
            Increment::Fifty => 50,
            Increment::Hundred => 100,
            Increment::TwoHundred => 200,
            Increment::TwoHundredFifty => 250,
            Increment::FiveHundred => 500,
            Increment::Thousand => 1000,
            Increment::TwoThousand => 2000,
            Increment::TwoThousandFiveHundred => 2500,
            Increment::FiveThousand => 5000,
        }
    }

    /// The increment whose value this is, or `None` for a value 16.1.2 step 7 rejects with a
    /// RangeError. The constructor validates the list itself and so never reaches the `None`.
    pub fn from_u32(value: u32) -> Option<Increment> {
        match value {
            1 => Some(Increment::One),
            2 => Some(Increment::Two),
            5 => Some(Increment::Five),
            10 => Some(Increment::Ten),
            20 => Some(Increment::Twenty),
            25 => Some(Increment::TwentyFive),
            50 => Some(Increment::Fifty),
            100 => Some(Increment::Hundred),
            200 => Some(Increment::TwoHundred),
            250 => Some(Increment::TwoHundredFifty),
            500 => Some(Increment::FiveHundred),
            1000 => Some(Increment::Thousand),
            2000 => Some(Increment::TwoThousand),
            2500 => Some(Increment::TwoThousandFiveHundred),
            5000 => Some(Increment::FiveThousand),
            _ => None,
        }
    }

    /// The 1, 2, 5 or 25 part of it.
    fn base(self) -> RoundingIncrement {
        match self {
            Increment::One | Increment::Ten | Increment::Hundred | Increment::Thousand => {
                RoundingIncrement::MultiplesOf1
            }
            Increment::Two | Increment::Twenty | Increment::TwoHundred | Increment::TwoThousand => {
                RoundingIncrement::MultiplesOf2
            }
            Increment::Five
            | Increment::Fifty
            | Increment::FiveHundred
            | Increment::FiveThousand => RoundingIncrement::MultiplesOf5,
            Increment::TwentyFive
            | Increment::TwoHundredFifty
            | Increment::TwoThousandFiveHundred => RoundingIncrement::MultiplesOf25,
        }
    }

    /// How far the base sits from the units digit.
    fn magnitude(self) -> i16 {
        match self {
            Increment::One | Increment::Two | Increment::Five | Increment::TwentyFive => 0,
            Increment::Ten | Increment::Twenty | Increment::Fifty | Increment::TwoHundredFifty => 1,
            Increment::Hundred
            | Increment::TwoHundred
            | Increment::FiveHundred
            | Increment::TwoThousandFiveHundred => 2,
            Increment::Thousand | Increment::TwoThousand | Increment::FiveThousand => 3,
        }
    }

    pub fn is_one(self) -> bool {
        self == Increment::One
    }
}

/// The resolved options of one `Intl.NumberFormat`: the internal slots of 16.2.3 that this build
/// implements and nothing else, so that an instance stores a record of small integers and no
/// ICU4X object. [[Locale]] is absent because the locale is what selects the [`DecimalRenderer`]
/// the value is formatted with, [[BoundFormat]] belongs to the object, and [[NumberingSystem]]
/// and [[Notation]] are absent because this build has exactly one value of each: the constructor
/// validates the option and 16.3.2 reports the constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumberFormatSlots {
    /// [[Style]] (16.1.3 step 2).
    pub style: Style,
    /// [[Currency]] (16.1.3 steps 5 and 12.1), only there for the currency style: 16.3.2 leaves the
    /// `currency` row of Table 28 out of its answer whenever the slot is `undefined`, which is what
    /// a `style: "decimal"` with a `currency` option reports.
    pub currency: Option<CurrencyCode>,
    /// [[CurrencyDisplay]] and [[CurrencySign]] (16.1.3 steps 12.2 and 12.3). 16.1.3's step 6 and
    /// step 7 read and validate the two options whatever the style is, but step 12 only writes the
    /// two slots for the currency style, and Table 28 therefore reports them exactly when
    /// [`Self::currency`] is there: 16.1.3 step 4 makes the two cases the same one.
    pub currency_display: CurrencyDisplay,
    pub currency_sign: CurrencySign,
    /// [[Notation]] (16.1.1 steps 11-12).
    pub notation: Notation,
    /// [[CompactDisplay]] (16.1.1 steps 17 and 19.1), which 16.3.2 reports only for the compact
    /// notation, the only one that reads it.
    pub compact_display: CompactDisplay,
    /// [[MinimumIntegerDigits]] (16.1.2 steps 1 and 13): 1 to 21.
    pub minimum_integer_digits: u8,
    /// [[MinimumFractionDigits]] and [[MaximumFractionDigits]] (16.1.2 steps 17-23): 0 to 100.
    ///
    /// Whether the pair is part of what `resolvedOptions` reports is [`Self::reports_fraction_digits`]
    /// rather than `Option` around these two: 16.1.2 leaves the slots `undefined` when it does not
    /// need a list, and the same is recorded one field below for nothing else, because the rounding
    /// of 16.5.2 only ever reads a pair whose type its own [[RoundingType]] names.
    pub minimum_fraction_digits: u8,
    pub maximum_fraction_digits: u8,
    /// [[MinimumSignificantDigits]] and [[MaximumSignificantDigits]] (16.1.2 steps 18-21): 1 to 21.
    pub minimum_significant_digits: u8,
    pub maximum_significant_digits: u8,
    /// 16.1.2's `needFractionDigits` and `needSignificantDigits`, which are what the spec's step 21
    /// and step 22 gate on and therefore what decides whether the two pairs above are slots that
    /// exist or defaults the rounding never asked for. 16.3.2 reports a resolved option only when
    /// its slot is not `undefined`, so these are what makes `minimumSignificantDigits` appear in
    /// `resolvedOptions()` for `{ significant digits }` and not for `{ fraction digits }`.
    pub reports_fraction_digits: bool,
    pub reports_significant_digits: bool,
    /// [[RoundingType]] (16.1.2 steps 23-27).
    pub rounding_type: RoundingType,
    /// [[UseGrouping]] (16.1.1 steps 18-24).
    pub use_grouping: UseGrouping,
    /// [[SignDisplay]] (16.1.1 steps 25-26).
    pub sign_display: SignDisplay,
    /// [[RoundingIncrement]] (16.1.2 steps 6-7 and 14).
    pub rounding_increment: Increment,
    /// [[RoundingMode]] (16.1.2 steps 8 and 15).
    pub rounding_mode: RoundingMode,
    /// [[ComputedRoundingPriority]] (16.1.2 steps 23-27).
    pub rounding_priority: RoundingPriority,
    /// [[TrailingZeroDisplay]] (16.1.2 steps 10 and 16).
    pub trailing_zero_display: TrailingZeroDisplay,
}

impl Default for NumberFormatSlots {
    /// What a `style: "decimal"`, `notation: "standard"` format settles on with no options at all:
    /// 16.1.1 steps 15-16 give 0 and 3 fraction digits, 16.1.1 step 19 gives "auto" grouping, and
    /// 16.1.2 gives the rest.
    fn default() -> Self {
        Self {
            style: Style::Decimal,
            currency: None,
            currency_display: CurrencyDisplay::Symbol,
            currency_sign: CurrencySign::Standard,
            notation: Notation::Standard,
            compact_display: CompactDisplay::Short,
            minimum_integer_digits: 1,
            minimum_fraction_digits: 0,
            maximum_fraction_digits: 3,
            minimum_significant_digits: 1,
            maximum_significant_digits: 21,
            reports_fraction_digits: true,
            reports_significant_digits: false,
            rounding_type: RoundingType::FractionDigits,
            use_grouping: UseGrouping::Auto,
            sign_display: SignDisplay::Auto,
            rounding_increment: Increment::One,
            rounding_mode: RoundingMode::HalfExpand,
            rounding_priority: RoundingPriority::Auto,
            trailing_zero_display: TrailingZeroDisplay::Auto,
        }
    }
}

/// The value domain of ToIntlMathematicalValue
/// (https://tc39.es/ecma402/#sec-tointlmathematicalvalue), 16.5.15: a mathematical value, which
/// here is always exact, plus the three things a Number can also be and the negative zero the spec
/// keeps apart from zero.
#[derive(Debug, Clone, PartialEq)]
pub enum IntlMathValue {
    /// A mathematical value, `negative` when it is below zero. Zero itself is never flagged here:
    /// that is what [`IntlMathValue::NegativeZero`] means, per 16.5.15 step 4.2.
    Number {
        negative: bool,
        unsigned: UnsignedDecimal,
    },
    NegativeZero,
    NaN,
    PositiveInfinity,
    NegativeInfinity,
}

impl IntlMathValue {
    fn zero() -> Self {
        IntlMathValue::Number { negative: false, unsigned: UnsignedDecimal::default() }
    }

    fn infinity(negative: bool) -> Self {
        if negative {
            IntlMathValue::NegativeInfinity
        } else {
            IntlMathValue::PositiveInfinity
        }
    }

    fn signed_zero(negative: bool) -> Self {
        if negative {
            IntlMathValue::NegativeZero
        } else {
            IntlMathValue::zero()
        }
    }
}

/// 16.5.15 step 4 for a Number.
///
/// `decimal` is `Number::toString(x, 10)`, which the caller takes from the engine because the
/// shortest round-trip spelling is the engine's own; `x` is read only for the negative zero that
/// step 4.2 returns before any string is made. What that path yields is exact: the shortest decimal
/// that reads back as `x` names exactly the number `x` is, so no rounding happens here at all.
pub fn math_value_from_number(x: f64, decimal: &str) -> IntlMathValue {
    // Step 4.2.
    if x == 0.0 && x.is_sign_negative() {
        return IntlMathValue::NegativeZero;
    }

    // Steps 4.3-10. An infinity and a NaN spell themselves as "Infinity", "-Infinity" and "NaN",
    // which are all in the grammar step 6 parses.
    math_value_from_literal_text(decimal)
}

/// 16.5.15 steps 3 and 5-10 for a String.
pub fn math_value_from_string(text: &str) -> IntlMathValue {
    math_value_from_literal_text(text)
}

/// The half 16.5.15 steps 5-10 shares for a Number and a String: parse the literal, then apply
/// RoundMVResult, which is ECMA-262's 7.1.4.1.3 and is what turns "1e400" into an infinity and
/// "1e-400" into a zero while leaving everything a double can hold exactly as written.
fn math_value_from_literal_text(text: &str) -> IntlMathValue {
    let (negative, literal) = match parse_decimal_literal(text) {
        Some(literal) => literal,
        // Step 7: what the grammar does not read is not-a-number.
        None => return IntlMathValue::NaN,
    };

    match literal {
        // The `Infinity` production of step 6.
        Literal::Infinity => IntlMathValue::infinity(negative),
        // Step 9.4: a zero that the sign made negative is negative-zero.
        Literal::Zero => IntlMathValue::signed_zero(negative),
        Literal::Digits { digits, point } => {
            // The magnitude of the first significant digit, which says whether the value can still
            // be a Number before its exact digits are turned into a string at all. 10^309 is above
            // the largest double and 10^-325 is below half of the smallest subnormal one, so
            // outside this band RoundMVResult's answer is certain: an infinity at one end, a zero
            // at the other.
            let magnitude = point - 1 - leading_zeros(&digits) as i32;
            if magnitude >= 309 {
                return IntlMathValue::infinity(negative);
            }
            if magnitude <= -325 {
                return IntlMathValue::signed_zero(negative);
            }

            // Steps 9.1-9.5 for the values inside the band, which the band does not decide: 9e308
            // is in it and is still not a double. `parse` converts to a Number exactly the way
            // RoundMVResult's "the nearest double, with the digits of a value that has 20 or fewer
            // of them being the value itself" does, and only the two ends of the range change an
            // answer.
            let mut decimal = decimal_text(&digits, point);
            cap_to_stored_digits(&mut decimal);
            match decimal.parse::<f64>() {
                Ok(x) if x.is_infinite() => return IntlMathValue::infinity(negative),
                Ok(0.0) => return IntlMathValue::signed_zero(negative),
                Ok(_) => {}
                // Not a decimal, which the scanner above makes impossible.
                Err(_) => return IntlMathValue::NaN,
            }

            match UnsignedDecimal::try_from_str(&decimal) {
                // Step 9.5, and the reason the digits above are capped: the exact value.
                Ok(unsigned) => IntlMathValue::Number { negative, unsigned },
                // Outside the band the value was already returned, and inside it the positions fit,
                // so the only thing left is a literal whose magnitude and digit count both stay
                // below the cap while the total still does not, which the cap keeps from happening.
                Err(_) => IntlMathValue::NaN,
            }
        }
    }
}

/// Drop the digits of a decimal string that `fixed_decimal` could not keep positions for. What
/// comes off is the least significant end, and no option rounds at a position that low: the lowest
/// is maximumFractionDigits 100 with roundingIncrement 1, and significant digits stop at 21, so
/// nothing any format can ask for depends on the digits that go.
fn cap_to_stored_digits(decimal: &mut String) {
    if decimal.len() <= MAX_EXACT_DIGITS {
        return;
    }
    decimal.truncate(MAX_EXACT_DIGITS);
    // A literal may not end at the point, and the point is the only punctuation in what is built
    // above, so dropping it keeps the value the same.
    while decimal.ends_with('.') {
        decimal.pop();
    }
}

/// 16.5.15 step 2: ℝ of a BigInt, which is exact and does not go through the Number conversion.
/// `text` is the BigInt's own decimal form, so a leading `-` is the only punctuation in it.
///
/// `None` means the value has more digits than [`MAX_EXACT_DIGITS`], which the caller reports
/// instead of formatting digits it cannot hold.
pub fn math_value_from_bigint(text: &str) -> Option<IntlMathValue> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    if digits.is_empty()
        || digits.len() > MAX_EXACT_DIGITS
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let unsigned = UnsignedDecimal::try_from_str(digits).ok()?;
    Some(IntlMathValue::Number { negative, unsigned })
}

/// The two pieces of a numeric literal that say its exact value: every digit in the order written,
/// and `point`, how many digits of them sit above the decimal separator. `.5` is the digit `5` with
/// `point` 0, and `12e3` is the digits `12` with `point` 5.
enum Literal {
    Infinity,
    Zero,
    Digits { digits: Vec<u8>, point: i32 },
}

/// StringNumericLiteral, as 16.5.15 step 6 parses it, with the optional sign of
/// StrSignedDecimalLiteral taken off first. `None` is what the grammar does not read.
fn parse_decimal_literal(text: &str) -> Option<(bool, Literal)> {
    // StrWhiteSpaceLeft_opt and StrWhiteSpaceRight_opt.
    let trimmed = text.trim_matches(is_space);

    // StringNumericLiteral :: StrWhiteSpace_opt, which is the empty string and reads as zero.
    if trimmed.is_empty() {
        return Some((false, Literal::Zero));
    }

    let (negative, body) = match trimmed.as_bytes() {
        [b'+', rest @ ..] => (false, rest),
        [b'-', rest @ ..] => (true, rest),
        body => (false, body),
    };
    if body.is_empty() {
        return None;
    }

    // StrUnsignedDecimalLiteral :: Infinity
    if body == b"Infinity" {
        return Some((negative, Literal::Infinity));
    }

    // NonDecimalIntegerLiteral, which is a mathematical value like any other.
    if body.first() == Some(&b'0') {
        let radix = match body.get(1) {
            Some(b'b') | Some(b'B') => Some(2),
            Some(b'o') | Some(b'O') => Some(8),
            Some(b'x') | Some(b'X') => Some(16),
            _ => None,
        };
        if let Some(radix) = radix {
            return decimal_of_radix(radix, &body[2..]).map(|digits| (negative, digits));
        }
    }

    // StrUnsignedDecimalLiteral :: DecimalDigits . DecimalDigits_opt ExponentPart_opt, and the
    // production that starts at the point.
    let mut digits = Vec::with_capacity(body.len());
    let mut point: Option<i32> = None;
    let mut index = 0;
    while index < body.len() {
        let byte = body[index];
        match byte {
            b'.' => {
                if point.is_some() {
                    return None;
                }
                point = Some(digits.len() as i32);
            }
            b'e' | b'E' => {
                let exponent = parse_exponent(&body[index + 1..])?;
                let point = point.unwrap_or(digits.len() as i32) + exponent;
                return Some((negative, digits_literal(digits, point)?));
            }
            _ if byte.is_ascii_digit() => digits.push(byte),
            _ => return None,
        }
        index += 1;
    }

    let point = point.unwrap_or(digits.len() as i32);
    Some((negative, digits_literal(digits, point)?))
}

/// DecimalDigits of a radix form, written out exactly. `None` is a form with no digits or with a
/// digit that is not valid in its radix.
fn decimal_of_radix(radix: u32, digits: &[u8]) -> Option<Literal> {
    if digits.is_empty() || digits.len() > MAX_EXACT_DIGITS {
        return None;
    }

    let mut decimal: Vec<u8> = Vec::new();
    for digit in digits {
        let value = char::from(*digit).to_digit(radix)?;
        multiply_decimal_by(&mut decimal, radix);
        add_to_decimal(&mut decimal, value);
    }

    let point = decimal.len() as i32;
    digits_literal(decimal, point)
}

fn digits_literal(digits: Vec<u8>, point: i32) -> Option<Literal> {
    if digits.is_empty() {
        // A literal needs at least one digit: "1e", ".", "+".
        return None;
    }
    if digits.iter().all(|byte| *byte == b'0') {
        return Some(Literal::Zero);
    }
    Some(Literal::Digits { digits, point })
}

/// The ExponentPart of the grammar. Its value only matters up to the point where a value stops
/// being a Number, so anything past that is clamped, and the clamp is far below the position limit.
fn parse_exponent(body: &[u8]) -> Option<i32> {
    let (sign, digits) = match body {
        [b'+', rest @ ..] => (1, rest),
        [b'-', rest @ ..] => (-1, rest),
        rest => (1, rest),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }

    let mut value: i32 = 0;
    for digit in digits {
        value = value
            .saturating_mul(10)
            .saturating_add(i32::from(digit - b'0'));
        if value >= MAX_EXPONENT {
            return Some(sign * MAX_EXPONENT);
        }
    }
    Some(sign * value)
}

/// The largest exponent worth keeping, which is past both ends of the double range.
const MAX_EXPONENT: i32 = 400;

fn leading_zeros(digits: &[u8]) -> usize {
    digits
        .iter()
        .position(|byte| *byte != b'0')
        .unwrap_or(digits.len())
}

/// StrWhiteSpace, which is WhiteSpace and LineTerminator together.
fn is_space(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\r' | '\u{b}' | '\u{c}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// The exact decimal text of a scanned literal with the point where the grammar put it, which is
/// what `fixed_decimal` reads. Every digit the literal had stays in it.
fn decimal_text(digits: &[u8], point: i32) -> String {
    let digits = String::from_utf8_lossy(digits).into_owned();
    if point <= 0 {
        let mut text = String::with_capacity(2 + (-point) as usize + digits.len());
        text.push_str("0.");
        for _ in 0..-point {
            text.push('0');
        }
        text.push_str(&digits);
        text
    } else if point as usize >= digits.len() {
        let mut text = digits;
        for _ in text.len()..point as usize {
            text.push('0');
        }
        text
    } else {
        let split = point as usize;
        format!("{}.{}", &digits[..split], &digits[split..])
    }
}

fn multiply_decimal_by(digits: &mut Vec<u8>, factor: u32) {
    let mut carry = 0;
    for digit in digits.iter_mut().rev() {
        let product = u32::from(*digit - b'0') * factor + carry;
        *digit = b'0' + product.rem_euclid(10) as u8;
        carry = product / 10;
    }
    while carry > 0 {
        digits.insert(0, b'0' + (carry % 10) as u8);
        carry /= 10;
    }
}

fn add_to_decimal(digits: &mut Vec<u8>, value: u32) {
    let mut carry = value;
    let mut index = digits.len();
    while carry > 0 {
        if index == 0 {
            digits.insert(0, b'0');
            index = 1;
        }
        index -= 1;
        let sum = u32::from(digits[index] - b'0') + carry;
        digits[index] = b'0' + sum.rem_euclid(10) as u8;
        carry = sum / 10;
    }
}

/// The four cases 16.5.10 steps 9-12 sorts the rounded value into, which is all the sign selection
/// of steps 14-19 needs to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignCategory {
    NegativeNonZero,
    NegativeZero,
    PositiveNonZero,
    PositiveZero,
}

/// GetNumberFormatPattern (https://tc39.es/ecma402/#sec-getnumberformatpattern), 16.5.10, for the
/// patterns of the two styles this build has. All eight of them are the sign, `{number}` and, for
/// percent, the percent sign, so which one step 5 through step 19 selects is fully said by the sign
/// it carries.
pub(crate) fn pattern_sign(display: SignDisplay, category: SignCategory) -> Sign {
    let negative = matches!(category, SignCategory::NegativeNonZero | SignCategory::NegativeZero);
    let zero = matches!(category, SignCategory::NegativeZero | SignCategory::PositiveZero);
    match display {
        // Step 15: "never" always takes a zero pattern.
        SignDisplay::Never => Sign::None,
        // Step 16: "auto" shows a sign only when the value is negative.
        SignDisplay::Auto if negative => Sign::Negative,
        SignDisplay::Auto => Sign::None,
        // Step 17.
        SignDisplay::Always if negative => Sign::Negative,
        SignDisplay::Always => Sign::Positive,
        // Step 18: "exceptZero" gives both of the zeros a zero pattern.
        SignDisplay::ExceptZero if zero => Sign::None,
        SignDisplay::ExceptZero if negative => Sign::Negative,
        SignDisplay::ExceptZero => Sign::Positive,
        // Step 19: "negative" shows a sign for a negative non-zero value and nothing else.
        SignDisplay::Negative if category == SignCategory::NegativeNonZero => Sign::Negative,
        SignDisplay::Negative => Sign::None,
    }
}

/// The digits FormatNumericToString (16.5.2) arrives at, with the position its rounding-priority
/// comparison (steps 7.3-7.6) measures the two roundings against.
struct Rounded {
    digits: UnsignedDecimal,
    rounding_magnitude: i16,
}

/// 16.5.2 steps 4-7, with the sign that step 3.3 takes off already known.
fn round_to_slots(slots: &NumberFormatSlots, unsigned: UnsignedDecimal, negative: bool) -> Rounded {
    let mode = slots.rounding_mode.unsigned(negative);
    match slots.rounding_type {
        // Step 5.
        RoundingType::SignificantDigits => to_raw_precision(
            unsigned,
            slots.minimum_significant_digits as i16,
            slots.maximum_significant_digits as i16,
            mode,
        ),
        // Step 6.
        RoundingType::FractionDigits => to_raw_fixed(
            unsigned,
            slots.minimum_fraction_digits as i16,
            slots.maximum_fraction_digits as i16,
            slots.rounding_increment,
            mode,
        ),
        // Step 7: round both ways and keep the rounding at the finer position, or at the coarser
        // one when `roundingPriority` asks for that.
        RoundingType::MorePrecision | RoundingType::LessPrecision => {
            let significant = to_raw_precision(
                unsigned.clone(),
                slots.minimum_significant_digits as i16,
                slots.maximum_significant_digits as i16,
                mode,
            );
            let fraction = to_raw_fixed(
                unsigned,
                slots.minimum_fraction_digits as i16,
                slots.maximum_fraction_digits as i16,
                slots.rounding_increment,
                mode,
            );
            let fixed_is_more_precise =
                fraction.rounding_magnitude < significant.rounding_magnitude;
            if (slots.rounding_type == RoundingType::MorePrecision) == fixed_is_more_precise {
                fraction
            } else {
                significant
            }
        }
    }
}

/// What FormatNumericToString (https://tc39.es/ecma402/#sec-formatnumerictostring), 16.5.2,
/// arrives at for one value: the digits it rounds or pads, with the sign category 16.5.10 steps
/// 9-12 sorts them into, or one of the three values of steps 2-3 that never reach the arithmetic.
///
/// This is the half of 16.5.3's PartitionNumberPattern that is not about the pattern, and
/// [`super::plural_rules_object`] needs it: 17.5.2 step 4 rounds a value through this same
/// operation and then asks the CLDR rules of the digits, so PluralRules reads them here rather
/// than re-deriving them.
#[derive(Clone, Debug, PartialEq)]
pub enum RoundedNumber {
    /// 16.5.2 steps 2-3, for NaN and the two infinities: the word to write, with the category the
    /// pattern's sign is selected from, in which NaN counts as a positive zero.
    Special(SignCategory, PartKind),
    /// 16.5.2 steps 4-14: the digits after the rounding, the trailing-zero display of step 10 and
    /// the padding of steps 11-13, with the category of step 14.
    Digits(SignCategory, UnsignedDecimal),
}

/// FormatNumericToString (https://tc39.es/ecma402/#sec-formatnumerictostring), 16.5.2, over these
/// slots, without the formatted string of steps 12-13: the digits are what the callers that are not
/// NumberFormat want, and 16.5.3's rendering writes them for the ones that do.
pub fn round_number(slots: &NumberFormatSlots, value: &IntlMathValue) -> RoundedNumber {
    // Steps 2-3.
    let (negative, unsigned) = match value {
        IntlMathValue::NaN => {
            return RoundedNumber::Special(SignCategory::PositiveZero, PartKind::Nan);
        }
        IntlMathValue::PositiveInfinity => {
            return RoundedNumber::Special(SignCategory::PositiveNonZero, PartKind::Infinity);
        }
        IntlMathValue::NegativeInfinity => {
            return RoundedNumber::Special(SignCategory::NegativeNonZero, PartKind::Infinity);
        }
        // The sign comes off before anything is rounded, so what follows works on the absolute
        // value alone.
        IntlMathValue::NegativeZero => (true, UnsignedDecimal::default()),
        IntlMathValue::Number { negative, unsigned } => (*negative, unsigned.clone()),
    };

    let mut unsigned = unsigned;
    if slots.style == Style::Percent {
        // Steps 5.1.2 and 5.1.3: percent scales by 100, and the only other scaling is
        // ComputeExponent (16.5.12 steps 1-2), which is 0 for the one notation this build has.
        unsigned.multiply_pow10(2);
    }

    // Steps 4-7.
    let rounded = round_to_slots(slots, unsigned, negative);

    // Step 10, which takes the fraction digits off when the value came out integral.
    let mut digits = rounded.digits;
    if slots.trailing_zero_display == TrailingZeroDisplay::StripIfInteger {
        digits.trim_end_if_integer();
    }

    // Steps 11-13, which is where minimumIntegerDigits pads the digits in front of the number.
    // `padded_start` takes the number of integer digits wanted and never removes one.
    digits = digits.padded_start(slots.minimum_integer_digits as i16);

    // Step 14 and 16.5.10 steps 9-12: the category the sign display reads, with a negative value
    // that rounded away still counted as a zero.
    let category = match (negative, digits.is_zero()) {
        (true, true) => SignCategory::NegativeZero,
        (true, false) => SignCategory::NegativeNonZero,
        (false, true) => SignCategory::PositiveZero,
        (false, false) => SignCategory::PositiveNonZero,
    };

    RoundedNumber::Digits(category, digits)
}

/// ToRawFixed (https://tc39.es/ecma402/#sec-torawfixed), 16.5.8.
fn to_raw_fixed(
    x: UnsignedDecimal,
    min_fraction: i16,
    max_fraction: i16,
    increment: Increment,
    mode: UnsignedRoundingMode,
) -> Rounded {
    // Steps 2-5: n is a multiple of the increment, and the rounded value is n × 10^-maxFraction.
    // `fixed_decimal` rounds at a digit position, and its quantum there is the increment moved to
    // the last shown fraction digit, because base × 10^position = increment × 10^-maxFraction.
    let position = increment.magnitude() - max_fraction;
    let value = x.rounded_with_mode_and_increment(position, mode, increment.base());

    // Steps 5.5 and 6-8: every fraction digit up to maxFraction shown, zeros included.
    let shown_end = -max_fraction;
    let value = value.padded_end(shown_end);

    // Steps 9-11: at most maxFraction - minFraction of the trailing zeros come off, and never a
    // digit that is not zero. The last is the highest position the strip may reach.
    let cut = max_fraction - min_fraction;
    let end = (shown_end + cut).min(last_nonzero_magnitude(&value, shown_end));
    let value = value.padded_end(end);

    // Step 12.
    Rounded { digits: value, rounding_magnitude: -max_fraction }
}

/// ToRawPrecision (https://tc39.es/ecma402/#sec-torawprecision), 16.5.7.
fn to_raw_precision(
    x: UnsignedDecimal,
    min_precision: i16,
    max_precision: i16,
    mode: UnsignedRoundingMode,
) -> Rounded {
    // Steps 1-3: n has maxPrecision digits and the value is n × 10^(e-maxPrecision+1), so n's
    // digits are the value rounded at maxPrecision digits from its first significant one. For zero
    // the spec's e is 0 and its m is maxPrecision zeros, and `nonzero_magnitude_start` is 0 there
    // too, so the same position produces the same digits.
    let magnitude = x.nonzero_magnitude_start();
    let position = magnitude + 1 - max_precision;
    let value = x.rounded_with_mode_and_increment(position, mode, RoundingIncrement::MultiplesOf1);

    // Steps 4-6: the point goes after the first e + 1 digits, which for a value below 1 means the
    // "0." and the zeros that follow it. `e` is read off the rounded value, because the rounding can
    // carry the number into a digit of its own and `0.999999` is then shown as `1` with maxPrecision
    // 2, not as the `1.00` that the position of its 9 would say.
    let position = value.nonzero_magnitude_start().max(magnitude) + 1 - max_precision;
    let shown_end = position.min(0);
    let value = value.padded_end(shown_end);

    // Step 7: the strip only happens where a point is being shown, and cuts at most maxPrecision -
    // minPrecision zeros.
    let end = if shown_end < 0 {
        (shown_end + (max_precision - min_precision)).min(last_nonzero_magnitude(&value, shown_end))
    } else {
        shown_end
    };
    let value = value.padded_end(end);

    // Step 8.
    Rounded { digits: value, rounding_magnitude: position }
}

/// The highest digit position at or above `from` that holds a non-zero digit, which is the lowest
/// the trailing-zero strip of 16.5.7 step 7 and 16.5.8 steps 10-11 may reach. A value with nothing
/// but zeros above `from` reports `from` itself, and the zero value reports position 0.
fn last_nonzero_magnitude(value: &UnsignedDecimal, from: i16) -> i16 {
    let top = value.nonzero_magnitude_start();
    let mut magnitude = from;
    while magnitude < top && value.digit_at(magnitude) == 0 {
        magnitude += 1;
    }
    magnitude
}

/// What FormatNumericToParts (16.5.6) talks about. The names are the `type` values its step 4.2
/// puts in the objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Integer,
    Group,
    Decimal,
    Fraction,
    MinusSign,
    PlusSign,
    PercentSign,
    /// 16.5.3 steps 9.9 to 9.11, which is where a currency code, a symbol and a name all land.
    Currency,
    /// 16.5.3 step 9.3 through 16.5.4 step 4, which is where the `K` of "1.2K" and the "thousand" of
    /// "1.2 thousand" land.
    Compact,
    Nan,
    Infinity,
    Literal,
}

impl PartKind {
    /// 16.5.6 step 4.2.
    pub fn name(self) -> &'static str {
        match self {
            PartKind::Integer => "integer",
            PartKind::Group => "group",
            PartKind::Decimal => "decimal",
            PartKind::Fraction => "fraction",
            PartKind::MinusSign => "minusSign",
            PartKind::PlusSign => "plusSign",
            PartKind::PercentSign => "percentSign",
            PartKind::Currency => "currency",
            PartKind::Compact => "compact",
            PartKind::Nan => "nan",
            PartKind::Infinity => "infinity",
            PartKind::Literal => "literal",
        }
    }
}

/// One record of the list PartitionNumberPattern (16.5.3) builds, before 16.5.6 turns it into a `{
/// type, value }` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberPart {
    pub kind: PartKind,
    pub value: String,
}

/// Where the pieces of a formatted number go. 16.5.5 concatenates them and 16.5.6 keeps them, and
/// the two must agree because a test and every caller that compares them relies on it, so both
/// walk the one rendering below. The two sinks below keep the promises 16.5.6 step 4 makes about
/// the pieces it hands back: no piece is empty and no two neighbours share a type. `icu_decimal`
/// annotates one character at a time, and it annotates the empty string too when the locale's
/// second minus affix is empty, so the promises have to be kept on the way in rather than by
/// whoever reads the list.
pub trait NumberSink {
    fn write_part(&mut self, kind: PartKind, value: &str);
}

/// 16.5.5 steps 2-3.
#[derive(Default)]
pub struct StringSink(pub String);

impl NumberSink for StringSink {
    fn write_part(&mut self, _kind: PartKind, value: &str) {
        self.0.push_str(value);
    }
}

/// 16.5.6 step 4, up to the point where the parts become JS objects.
#[derive(Default)]
pub struct PartList(pub Vec<NumberPart>);

impl NumberSink for PartList {
    fn write_part(&mut self, kind: PartKind, value: &str) {
        if value.is_empty() {
            return;
        }
        match self.0.last_mut() {
            Some(last) if last.kind == kind => last.value.push_str(value),
            _ => self.0.push(NumberPart { kind, value: value.to_string() }),
        }
    }
}

/// One locale's decimal rendering: the `icu_decimal` formatter for that locale and one grouping
/// choice, plus the piece of the percent pattern its data has no marker for.
///
/// These are built once per locale and grouping option and kept in the per-context cache, because
/// constructing a `DecimalFormatter` copies the symbol and digit tables out of the baked data.
///
/// The currency style renders its digits through this and wraps them in its own pattern, so what a
/// caller of [`Self::render`] and [`Self::write_unsigned`] needs is the rounding and the digits
/// without the sign: a currency pattern says where the sign goes, and the decimal formatter would
/// put it next to the number instead.
pub struct DecimalRenderer {
    formatter: DecimalFormatter,
    percent_spacing: &'static str,
    /// The two compact pattern lists of the locale, of which [`Self::compact_of`] picks one with
    /// [[CompactDisplay]]. They come with the decimal rendering rather than with a cache key of
    /// their own because the exponent they name is read from the digits, which the rendering owns.
    compact_short: CompactDecimalFormatter,
    compact_long: CompactDecimalFormatter,
    /// The locale's minus and plus affixes and which end of the string each of them is at.
    /// [`Self::write_special`] needs them for NaN and infinity, which never go through the digit
    /// rendering, and [`super::number_format_currency`] needs them to write the sign a currency
    /// pattern has no room for.
    signs: SignAffixes,
}

impl DecimalRenderer {
    /// The bare decimal rendering of the locale, for callers whose pattern is not one this
    /// renderer's own formats wrap: the relative-time patterns interpolate the number into their
    /// own text, and the writeable pass of that interpolation is what carries the part
    /// annotations.
    pub(crate) fn decimal_formatter(&self) -> &DecimalFormatter {
        &self.formatter
    }
}

/// The scale a number was divided by for the compact notation, and which of the two lists of the
/// locale names it: what 16.5.3 step 5.1.3 worked out and step 9.3 hands to 16.5.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Compacting {
    /// The exponent 16.5.12's ComputeExponent returned, which is what selects the pattern of the
    /// locale's compact list and therefore the plural category the word agrees with.
    exponent: u8,
    display: CompactDisplay,
}

/// The two sign affixes of a locale, as the data has them: `icu_decimal`'s symbols hold a prefix and
/// a suffix for each of the two, and a locale that puts its minus after the number leaves the prefix
/// empty.
pub(crate) struct SignAffixes {
    minus_prefix: String,
    minus_suffix: String,
    plus_prefix: String,
    plus_suffix: String,
}

impl SignAffixes {
    /// The affix `sign` means, as the two ends around the number.
    pub(crate) fn of(&self, sign: Sign) -> (&str, &str) {
        match sign {
            Sign::Negative => (&self.minus_prefix, &self.minus_suffix),
            Sign::Positive => (&self.plus_prefix, &self.plus_suffix),
            Sign::None => ("", ""),
        }
    }

    /// The type of a literal of a pattern that is not `icu_decimal`'s own: 16.5.3 step 9.12 gives
    /// every other piece of a pattern the `literal` type, except that a piece which *is* the
    /// locale's minus or plus affix is a sign, which is what the sign parts of a currency pattern
    /// are. `-` is compared against the affix rather than recognized by character because a locale
    /// whose minus is U+2212 has to get that from its own data.
    pub(crate) fn kind_of_literal(&self, text: &str) -> PartKind {
        if text == self.minus_prefix || text == self.minus_suffix {
            PartKind::MinusSign
        } else if text == self.plus_prefix || text == self.plus_suffix {
            PartKind::PlusSign
        } else {
            PartKind::Literal
        }
    }

    /// The affix around the whole of what `sign` says, for a pattern that has no placeholder for it
    /// and has to be wrapped in it: 16.2.3's `positivePattern` and `negativePattern` of a style with
    /// no data of its own, and the negative form of a currency pattern that has no room for a sign
    /// because its own parentheses or minus already carry one.
    pub(crate) fn wrap<S: NumberSink>(&self, sign: Sign, sink: &mut S, inner: impl FnOnce(&mut S)) {
        let kind = match sign {
            Sign::Negative => PartKind::MinusSign,
            Sign::Positive => PartKind::PlusSign,
            // Nothing to write around it: `Sign::None` is the caller saying the pattern, or the
            // sign display, has no sign for this value.
            Sign::None => return inner(sink),
        };

        let (prefix, suffix) = self.of(sign);

        // The affix the locale keeps at this end is empty here, and an empty part would be a part
        // 16.5.6 has no reason to list.
        if !prefix.is_empty() {
            sink.write_part(kind, prefix);
        }

        inner(sink);

        if !suffix.is_empty() {
            sink.write_part(kind, suffix);
        }
    }
}

/// What FormatNumericToString (16.5.2) arrives at for one set of slots, kept with the category
/// 16.5.10 steps 9 to 13 sorts the value into, which is what every style's pattern selection reads.
pub(crate) enum Rendered {
    /// NaN or one of the two infinities: 16.5.3 steps 2 to 4 replace the arithmetic with an
    /// implementation-defined word, which the renderer writes as its own part.
    Special { category: SignCategory, kind: PartKind },
    /// The digits, already rounded, already padded to `minimumIntegerDigits`, and without any sign:
    /// what a pattern puts its affixes around.
    Number {
        category: SignCategory,
        digits: UnsignedDecimal,
        /// The compact scale the digits above were divided by, of 16.5.2 steps 5.1.3 and 5.1.4, for
        /// the one notation that has one. `None` is the exponent 0 of the standard notation, whose
        /// digits are the value itself.
        compact: Option<Compacting>,
    },
}

impl DecimalRenderer {
    /// Read the baked data of `tag` and build the rendering for one `useGrouping` choice.
    pub fn try_new(tag: &str, grouping: UseGrouping) -> Result<Self, DataError> {
        let locale = Locale::from_str(tag)
            // `resolve_locale` only ever hands out tags this build has data for, all of which
            // parse.
            .map_err(|_| DataError::custom("Intl resolved a locale tag that does not parse"))?;

        let mut options = DecimalFormatterOptions::default();
        // The one field of a type that is `#[non_exhaustive]`.
        options.grouping_strategy = Some(grouping.grouping_strategy());

        // The compact lists are read through the same provider as the decimal formatter below, so
        // that the number it interpolates into `0K` groups the way ours does and the two strings
        // still line up in [`Self::write_compact`].
        let (formatter, compact_short, compact_long) = if grouping
            .needs_grouping_of_four_digit_numbers()
        {
            let provider = MinimumGroupingDigitsOne;
            (
                DecimalFormatter::try_new_unstable(&provider, (&locale).into(), options)?,
                CompactDecimalFormatter::try_new_short_unstable(
                    &provider,
                    (&locale).into(),
                    grouping.grouping_strategy().into(),
                )?,
                CompactDecimalFormatter::try_new_long_unstable(
                    &provider,
                    (&locale).into(),
                    grouping.grouping_strategy().into(),
                )?,
            )
        } else {
            (
                DecimalFormatter::try_new_unstable(&IntlDataProvider, (&locale).into(), options)?,
                CompactDecimalFormatter::try_new_short_unstable(
                    &IntlDataProvider,
                    (&locale).into(),
                    grouping.grouping_strategy().into(),
                )?,
                CompactDecimalFormatter::try_new_long_unstable(
                    &IntlDataProvider,
                    (&locale).into(),
                    grouping.grouping_strategy().into(),
                )?,
            )
        };

        Ok(Self {
            signs: sign_affixes_of(&formatter),
            formatter,
            compact_short,
            compact_long,
            percent_spacing: percent_spacing_of(&locale),
        })
    }

    /// FormatNumeric (https://tc39.es/ecma402/#sec-formatnumeric), 16.5.5.
    pub fn format(&self, slots: &NumberFormatSlots, value: &IntlMathValue) -> String {
        let mut sink = StringSink::default();
        self.partition(slots, value, &mut sink);
        sink.0
    }

    /// FormatNumericToParts (https://tc39.es/ecma402/#sec-formatnumerictoparts), 16.5.6.
    pub fn format_to_parts(
        &self,
        slots: &NumberFormatSlots,
        value: &IntlMathValue,
    ) -> Vec<NumberPart> {
        let mut sink = PartList::default();
        self.partition(slots, value, &mut sink);
        sink.0
    }

    /// PartitionNumberPattern (https://tc39.es/ecma402/#sec-partitionnumberpattern), 16.5.3, for the
    /// two styles this build has patterns for and for the standard and the compact notation.
    fn partition<S: NumberSink>(
        &self,
        slots: &NumberFormatSlots,
        value: &IntlMathValue,
        sink: &mut S,
    ) {
        match self.render(slots, value) {
            Rendered::Special { category, kind } => self.write_special(slots, category, kind, sink),
            Rendered::Number { category, digits, compact } => {
                self.write_decimal(slots, category, digits, compact, sink)
            }
        }
    }

    /// FormatNumericToString (https://tc39.es/ecma402/#sec-formatnumerictostring), 16.5.2: the
    /// scaling, the rounding, the padding and the category, with none of the pattern.
    ///
    /// This is what a style whose pattern comes from somewhere other than `icu_decimal` needs: the
    /// currency patterns say where the sign, the affixes and the digits go, so the renderer takes the
    /// digits from here and lays out the rest itself.
    pub(crate) fn render(&self, slots: &NumberFormatSlots, value: &IntlMathValue) -> Rendered {
        // Steps 2-4: the three special values skip the arithmetic. Their category is what steps
        // 9-12 of 16.5.10 would sort them into, in which NaN counts as a positive zero.
        let (negative, unsigned) = match value {
            IntlMathValue::NaN => {
                return Rendered::Special {
                    category: SignCategory::PositiveZero,
                    kind: PartKind::Nan,
                };
            }
            IntlMathValue::PositiveInfinity => {
                return Rendered::Special {
                    category: SignCategory::PositiveNonZero,
                    kind: PartKind::Infinity,
                };
            }
            IntlMathValue::NegativeInfinity => {
                return Rendered::Special {
                    category: SignCategory::NegativeNonZero,
                    kind: PartKind::Infinity,
                };
            }
            // 16.5.2 steps 2-3: the sign comes off before anything is rounded, so what follows
            // works on the absolute value alone.
            IntlMathValue::NegativeZero => (true, UnsignedDecimal::default()),
            IntlMathValue::Number { negative, unsigned } => (*negative, unsigned.clone()),
        };

        let mut unsigned = unsigned;
        if slots.style == Style::Percent {
            // Step 5.1.2 of 16.5.2: percent scales by 100, which step 5.1.3's ComputeExponent then
            // reads the magnitude of.
            unsigned.multiply_pow10(2);
        }

        // 16.5.2 step 5.1.4 with 16.5.12: the compact notation divides the value by the power of ten
        // its data names for its magnitude, and it does so before anything is rounded because the
        // rounding is what decides whether that power was the right one.
        let exponent = match slots.notation {
            Notation::Standard => 0,
            // 16.5.12 step 1: zero has no magnitude to pick a scale by.
            Notation::Compact if unsigned.is_zero() => 0,
            Notation::Compact => self.compact_exponent(slots, &unsigned),
        };
        if exponent != 0 {
            unsigned.multiply_pow10(-i16::from(exponent));
        }

        let compact = match slots.notation {
            Notation::Standard => None,
            Notation::Compact => Some(Compacting { exponent, display: slots.compact_display }),
        };

        // 16.5.2 steps 4-7.
        let rounded = round_to_slots(slots, unsigned, negative);

        // 16.5.2 step 10, which takes the fraction digits off when the value came out integral.
        let mut digits = rounded.digits;
        if slots.trailing_zero_display == TrailingZeroDisplay::StripIfInteger {
            digits.trim_end_if_integer();
        }

        // 16.5.2 steps 11-13, which is where minimumIntegerDigits pads the digits in front of the
        // number. `padded_start` takes the number of integer digits wanted and never removes one.
        digits = digits.padded_start(slots.minimum_integer_digits as i16);

        // 16.5.2 step 14 and 16.5.10 steps 9-12: the category the sign display reads, with a
        // negative value that rounded away still counted as a zero.
        let category = match (negative, digits.is_zero()) {
            (true, true) => SignCategory::NegativeZero,
            (true, false) => SignCategory::NegativeNonZero,
            (false, true) => SignCategory::PositiveZero,
            (false, false) => SignCategory::PositiveNonZero,
        };

        Rendered::Number { category, digits, compact }
    }

    /// The rounded digits of [`Rendered::Number`], written with the locale's separators and grouping
    /// and no sign of their own: the sign of a currency or unit pattern sits wherever that pattern
    /// puts it, which is not necessarily next to the number.
    ///
    /// A [`Compacting`] scale adds the piece of the compact pattern that names it, which is what
    /// makes this the only way the digits of either notation are ever written: a currency pattern
    /// wraps them, and 16.5.4 puts the compact word inside them.
    pub(crate) fn write_unsigned<S: NumberSink>(
        &self,
        digits: &UnsignedDecimal,
        compact: Option<Compacting>,
        sink: &mut S,
    ) {
        // `Signed` is `#[non_exhaustive]`, so the fields are set rather than a literal written.
        let mut value = Decimal::default();
        value.sign = Sign::None;
        value.absolute = digits.clone();

        match compact {
            None => write_formatted(&self.formatter, &value, sink),
            Some(compact) => self.write_compact(&value, compact, sink),
        }
    }

    /// The compact list [[CompactDisplay]] asks the locale's data for.
    fn compact_of(&self, display: CompactDisplay) -> &CompactDecimalFormatter {
        match display {
            CompactDisplay::Short => &self.compact_short,
            CompactDisplay::Long => &self.compact_long,
        }
    }

    /// ComputeExponent (https://tc39.es/ecma402/#sec-computeexponent), 16.5.12, for the compact
    /// notation: the scale of step 4, then the same rounding of step 6 asked over the scaled value
    /// to find out whether it moved the most significant digit after all. `999_999` is the case the
    /// clause is written for: it rounds to `1000` thousand, which the data says is one million.
    fn compact_exponent(&self, slots: &NumberFormatSlots, unsigned: &UnsignedDecimal) -> u8 {
        let compact = self.compact_of(slots.compact_display);
        let magnitude = unsigned.nonzero_magnitude_start();
        let exponent = compact.compact_exponent_for_magnitude(magnitude);

        // Step 6, whose `[[RoundedNumber]]` is what steps 7 to 9 read. The sign of 16.5.2 step 3 is
        // already off this value, so the rounding mode of step 3.3 of 16.5.12's own call has nothing
        // to flip.
        let scaled = unsigned
            .clone()
            .multiplied_pow10(-i16::from(exponent))
            .trimmed_end();
        let rounded = round_to_slots(slots, scaled, false).digits;

        // Steps 7 to 9: nothing moved, so the scale stands.
        if rounded.is_zero() || rounded.nonzero_magnitude_start() == magnitude - i16::from(exponent)
        {
            return exponent;
        }

        // Step 10, one magnitude further, which is the next entry of the locale's list or the same
        // one again when the data has nothing larger.
        compact.compact_exponent_for_magnitude(magnitude + 1)
    }

    /// 16.5.3 step 9.3 through PartitionNotationSubPattern (https://tc39.es/ecma402/#sec-partitionnotationsubpattern),
    /// 16.5.4: the scaled digits and the piece of the locale's compact pattern that names the scale.
    ///
    /// `icu_decimal` writes that pattern around the number it formats itself, as plain text with no
    /// part annotation to read the `K` off, so the string is taken whole and cut at the digits this
    /// renderer writes the same way: what is left in front and behind is the compact piece, with its
    /// spaces split off as literals the way 16.5.4 step 4.3 splits a pattern.
    fn write_compact<S: NumberSink>(&self, value: &Decimal, compact: Compacting, sink: &mut S) {
        let Ok(formatted) = self
            .compact_of(compact.display)
            .format_with_exponent(value, compact.exponent)
        else {
            // The exponent is the one this same data named for these digits and step 10 above is
            // what checks the rounding against it, so an inconsistency here means the locale's list
            // has no entry for the scale at all. Say nothing about it rather than guess.
            return write_formatted(&self.formatter, value, sink);
        };

        let whole = formatted.write_to_string();
        let number = self.unsigned_text(value);

        // A pattern with no number in it is all compact piece: Italian long display says `mille`
        // for a thousand and leaves the one out.
        match whole.split_once(number.as_str()) {
            Some((prefix, suffix)) => {
                write_compact_affix(prefix, sink);
                write_formatted(&self.formatter, value, sink);
                write_compact_affix(suffix, sink);
            }
            None => sink.write_part(PartKind::Compact, whole.as_ref()),
        }
    }

    /// The digits of `value` alone, without a compact piece around them: the string
    /// [`Self::write_compact`] looks for inside the compact formatter's output.
    fn unsigned_text(&self, value: &Decimal) -> String {
        let mut sink = StringSink::default();
        write_formatted(&self.formatter, value, &mut sink);
        sink.0
    }

    /// The locale's minus and plus affixes, for the patterns this renderer has no data for.
    pub(crate) fn signs(&self) -> &SignAffixes {
        &self.signs
    }

    /// 16.5.3 steps 9.3 and 9.4 for a number: the sign the pattern of 16.5.10 selected, then the
    /// digits through `icu_decimal`, which knows the locale's separators and grouping positions,
    /// and then the tail of a percent pattern.
    fn write_decimal<S: NumberSink>(
        &self,
        slots: &NumberFormatSlots,
        category: SignCategory,
        digits: UnsignedDecimal,
        compact: Option<Compacting>,
        sink: &mut S,
    ) {
        let Some(compact) = compact else {
            // `Signed` is `#[non_exhaustive]`, so the fields are set rather than a literal written.
            let mut value = Decimal::default();
            value.sign = pattern_sign(slots.sign_display, category);
            value.absolute = digits;

            write_formatted(&self.formatter, &value, sink);
            self.write_percent_affix(slots, sink);
            return;
        };

        // The digits of a compact format reach the sink through [`Self::write_unsigned`], which
        // writes them without a sign so that the compact text can be cut around them, so the sign
        // the pattern selected is written here instead.
        let sign = pattern_sign(slots.sign_display, category);
        self.signs.wrap(sign, sink, |sink| {
            self.write_unsigned(&digits, Some(compact), sink);
        });

        self.write_percent_affix(slots, sink);
    }

    /// 16.5.3 steps 2-4 with step 9: the sign the pattern carries, then the word.
    fn write_special<S: NumberSink>(
        &self,
        slots: &NumberFormatSlots,
        category: SignCategory,
        kind: PartKind,
        sink: &mut S,
    ) {
        self.signs
            .wrap(pattern_sign(slots.sign_display, category), sink, |sink| {
                sink.write_part(
                    kind,
                    if kind == PartKind::Nan {
                        NAN_STRING
                    } else {
                        INFINITY_STRING
                    },
                );

                self.write_percent_affix(slots, sink);
            });
    }

    /// The tail of a percent pattern: a space where the locale's pattern has one, then the percent
    /// sign. Every pattern this build has puts the number first.
    fn write_percent_affix<S: NumberSink>(&self, slots: &NumberFormatSlots, sink: &mut S) {
        if slots.style != Style::Percent {
            return;
        }
        if !self.percent_spacing.is_empty() {
            sink.write_part(PartKind::Literal, self.percent_spacing);
        }
        sink.write_part(PartKind::PercentSign, "%");
    }
}

/// Write one value through `icu_decimal`'s part annotations into `sink`.
///
/// Both sinks only ever push to a `String` or to a `Vec`, so the only thing `write_to_parts` can
/// report from here is success; a debug build checks that instead of an unwrap carrying a panic
/// into the engine. The call itself must stay outside the check: `debug_assert!` does not evaluate
/// its argument without assertions, and this argument *is* the writing.
fn write_formatted<S: NumberSink>(formatter: &DecimalFormatter, value: &Decimal, sink: &mut S) {
    let mut bridge = PartsBridge { sink, part: None };
    let written = formatter.format(value).write_to_parts(&mut bridge);
    debug_assert!(written.is_ok());
}

/// The locale's minus and plus affixes and where they sit, read off what the formatter writes for a
/// signed zero.
///
/// `icu_decimal` keeps the two affixes inside the pattern it loaded and its fields are private, so
/// the placement comes from the rendering rather than from the pattern text. Every locale built here
/// puts both affixes in front of the number, and this reads that instead of assuming it.
fn sign_affixes_of(formatter: &DecimalFormatter) -> SignAffixes {
    let (minus_prefix, minus_suffix) =
        affix_pair_of(formatter, Sign::Negative, PartKind::MinusSign);
    let (plus_prefix, plus_suffix) = affix_pair_of(formatter, Sign::Positive, PartKind::PlusSign);
    SignAffixes { minus_prefix, minus_suffix, plus_prefix, plus_suffix }
}

/// The two ends of the affix `sign` renders as, which is everything of kind `kind` before and after
/// the digits of a rendered zero.
fn affix_pair_of(formatter: &DecimalFormatter, sign: Sign, kind: PartKind) -> (String, String) {
    let mut value = Decimal::default();
    value.sign = sign;
    let mut parts = PartList::default();
    write_formatted(formatter, &value, &mut parts);

    let mut prefix = String::new();
    let mut suffix = String::new();
    let mut past_digits = false;
    for part in parts.0 {
        if part.kind == kind {
            if past_digits {
                suffix.push_str(&part.value);
            } else {
                prefix.push_str(&part.value);
            }
        } else if matches!(
            part.kind,
            PartKind::Integer | PartKind::Group | PartKind::Decimal | PartKind::Fraction
        ) {
            past_digits = true;
        }
    }
    (prefix, suffix)
}

/// The one piece of the percent pattern that differs between the locales built here: CLDR has
/// `#,##0 %` with U+00A0 for Danish and `#,##0%` for English and Italian. The language subtag is
/// what the pattern is keyed on, so a tag with a region in it, such as `da-DK`, gets the space too.
fn percent_spacing_of(locale: &Locale) -> &'static str {
    match locale.id.language.to_string().as_str() {
        "da" => "\u{a0}",
        _ => "",
    }
}

/// One side of a compact pattern, written as the parts 16.5.4 step 4.3 asks for: the spaces the
/// pattern puts between its number and its word are literals, and the word or abbreviation is the
/// `compact` piece.
fn write_compact_affix<S: NumberSink>(affix: &str, sink: &mut S) {
    let mut rest = affix;

    while !rest.is_empty() {
        let spaces = rest.len() - rest.trim_start_matches(char::is_whitespace).len();
        if spaces > 0 {
            sink.write_part(PartKind::Literal, &rest[..spaces]);
            rest = &rest[spaces..];
            continue;
        }

        let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
        sink.write_part(PartKind::Compact, &rest[..word]);
        rest = &rest[word..];
    }
}

/// The number symbols of a locale, read with the minimum grouping size set to one.
///
/// `icu_decimal` 2.1's `GroupingStrategy::Always` is documented in its own grouper as behaving
/// like `Auto`, so it still respects the locale's `minimumGroupingDigits`, which for Italian is
/// 2. `useGrouping: "always"` asks for a separator wherever there are enough digits for one, and
/// the way to get "1.234" for Italian out of this data is to lower the number the grouper compares
/// against.
struct MinimumGroupingDigitsOne;

impl DataProvider<DecimalSymbolsV1> for MinimumGroupingDigitsOne {
    fn load(&self, request: DataRequest) -> Result<DataResponse<DecimalSymbolsV1>, DataError> {
        let mut response = IntlDataProvider.load(request)?;
        // `with_mut` copies the symbol table into owned storage first, which is why the renderer is
        // built once per locale and not per call.
        response.payload.with_mut(lower_minimum_grouping_digits);
        Ok(response)
    }
}

/// Put the locale's `minimumGroupingDigits` at one, which is what `useGrouping: "always"` asks for.
///
/// The type is the yokeable form of the marker's data, which is what `with_mut` hands its closure;
/// it is written out because that form is not inferable there.
fn lower_minimum_grouping_digits(symbols: &mut DecimalSymbols<'_>) {
    symbols.grouping_sizes.min_grouping = 1;
}

impl DataProvider<DecimalDigitsV1> for MinimumGroupingDigitsOne {
    fn load(&self, request: DataRequest) -> Result<DataResponse<DecimalDigitsV1>, DataError> {
        IntlDataProvider.load(request)
    }
}

// The compact lists and the plural rules they select their word with are not touched by the
// grouping size above, so they go straight through.
impl DataProvider<DecimalCompactShortV1> for MinimumGroupingDigitsOne {
    fn load(&self, request: DataRequest) -> Result<DataResponse<DecimalCompactShortV1>, DataError> {
        IntlDataProvider.load(request)
    }
}

impl DataProvider<DecimalCompactLongV1> for MinimumGroupingDigitsOne {
    fn load(&self, request: DataRequest) -> Result<DataResponse<DecimalCompactLongV1>, DataError> {
        IntlDataProvider.load(request)
    }
}

impl DataProvider<PluralsCardinalV1> for MinimumGroupingDigitsOne {
    fn load(&self, request: DataRequest) -> Result<DataResponse<PluralsCardinalV1>, DataError> {
        IntlDataProvider.load(request)
    }
}

/// The bridge from `icu_decimal`'s part annotations to [`PartKind`]. Annotations nest, and the
/// innermost one is the only one that says what the text being written is, which is how `writeable`
/// defines it.
struct PartsBridge<'a, S> {
    sink: &'a mut S,
    part: Option<Part>,
}

impl<S: NumberSink> PartsWrite for PartsBridge<'_, S> {
    type SubPartsWrite = Self;

    fn with_part(
        &mut self,
        part: Part,
        mut f: impl FnMut(&mut Self::SubPartsWrite) -> std::fmt::Result,
    ) -> std::fmt::Result {
        let outer = self.part.replace(part);
        let result = f(self);
        self.part = outer;
        result
    }
}

impl<S: NumberSink> std::fmt::Write for PartsBridge<'_, S> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.sink
            .write_part(self.part.map_or(PartKind::Literal, kind_of), text);
        Ok(())
    }
}

/// The name `icu_decimal` gives its parts, which is where their meaning is; the category only says
/// that they came from a decimal formatter.
fn kind_of(part: Part) -> PartKind {
    match part.value {
        "integer" => PartKind::Integer,
        "group" => PartKind::Group,
        "decimal" => PartKind::Decimal,
        "fraction" => PartKind::Fraction,
        "minusSign" => PartKind::MinusSign,
        "plusSign" => PartKind::PlusSign,
        _ => PartKind::Literal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer(locale: &str) -> DecimalRenderer {
        DecimalRenderer::try_new(locale, UseGrouping::Auto).unwrap()
    }

    fn renderer_with(locale: &str, grouping: UseGrouping) -> DecimalRenderer {
        DecimalRenderer::try_new(locale, grouping).unwrap()
    }

    /// The string one format produces, with the slots changed the way the test says.
    fn format(locale: &str, change: impl FnOnce(&mut NumberFormatSlots), text: &str) -> String {
        let mut slots = NumberFormatSlots::default();
        change(&mut slots);
        renderer(locale).format(&slots, &math_value_from_string(text))
    }

    #[test]
    fn a_plain_number_gets_three_fraction_digits_and_groups() {
        assert_eq!(format("en", no_change, "1234567.891"), "1,234,567.891");
        // The default maximumFractionDigits of 16.1.1 step 15 rounds, it does not truncate.
        assert_eq!(format("en", no_change, "1.0055"), "1.006");
        assert_eq!(format("en", no_change, "0.5"), "0.5");
        // Percent is 0 and 0 fraction digits (16.1.1 steps 15-16) and scales before it rounds.
        assert_eq!(format("en", percent, "0.5678"), "57%");
        assert_eq!(format("en", percent, "1.005"), "101%");
    }

    fn no_change(_slots: &mut NumberFormatSlots) {}

    /// The slots the constructor of 16.1.1 steps 15-16 leaves a style: "percent" number with no
    /// explicit fraction digits: the defaults there are 0 and 0, not 0 and 3.
    fn percent(slots: &mut NumberFormatSlots) {
        slots.style = Style::Percent;
        slots.minimum_fraction_digits = 0;
        slots.maximum_fraction_digits = 0;
    }

    #[test]
    fn fraction_digit_options_are_read_the_way_the_spec_reads_them() {
        let min_max = |min: u8, max: u8| {
            move |slots: &mut NumberFormatSlots| {
                slots.minimum_fraction_digits = min;
                slots.maximum_fraction_digits = max;
            }
        };
        assert_eq!(format("en", min_max(2, 2), "1234.5"), "1,234.50");
        assert_eq!(format("en", min_max(0, 0), "1234.5"), "1,235");
        // Zeros between min and max stay, and above max they go (16.5.8 steps 9-11).
        assert_eq!(format("en", min_max(1, 3), "1.5000"), "1.5");
        assert_eq!(format("en", min_max(3, 3), "1.5"), "1.500");
        assert_eq!(format("en", min_max(0, 3), "1.5"), "1.5");
        assert_eq!(format("en", min_max(2, 5), "1.5"), "1.50");
    }

    #[test]
    fn minimum_integer_digits_pads_in_front() {
        // 16.5.2 steps 11-13.
        assert_eq!(format("en", |slots| slots.minimum_integer_digits = 3, "0.5"), "000.5");
        assert_eq!(format("en", |slots| slots.minimum_integer_digits = 5, "12.34"), "00,012.34");
        // A value with more integer digits than the option asks for is left as it is.
        assert_eq!(format("en", |slots| slots.minimum_integer_digits = 2, "1234"), "1,234");
        assert_eq!(format("en", |slots| slots.minimum_integer_digits = 2, "-0"), "-00");
    }

    #[test]
    fn significant_digits_round_the_way_16_5_7_says() {
        let three = |slots: &mut NumberFormatSlots| {
            slots.minimum_significant_digits = 3;
            slots.maximum_significant_digits = 3;
            slots.rounding_type = RoundingType::SignificantDigits;
        };
        assert_eq!(format("en", three, "1234"), "1,230");
        assert_eq!(format("en", three, "0.00123456"), "0.00123");
        assert_eq!(format("en", three, "1200"), "1,200");

        let one_to_four = |slots: &mut NumberFormatSlots| {
            slots.minimum_significant_digits = 1;
            slots.maximum_significant_digits = 4;
            slots.rounding_type = RoundingType::SignificantDigits;
        };
        // Step 7 cuts down to minPrecision, but never past a digit that is not zero.
        assert_eq!(format("en", one_to_four, "1234"), "1,234");
        assert_eq!(format("en", one_to_four, "1230"), "1,230");
        assert_eq!(format("en", one_to_four, "1.5000"), "1.5");
        assert_eq!(format("en", one_to_four, "0"), "0");
    }

    #[test]
    fn rounding_priority_chooses_between_the_two_roundings() {
        let both = |rounding_type: RoundingType, priority: RoundingPriority| {
            move |slots: &mut NumberFormatSlots| {
                slots.minimum_fraction_digits = 0;
                slots.maximum_fraction_digits = 1;
                slots.minimum_significant_digits = 2;
                slots.maximum_significant_digits = 2;
                slots.rounding_type = rounding_type;
                slots.rounding_priority = priority;
            }
        };
        use RoundingPriority::{LessPrecision, MorePrecision};
        // The fraction rounding is at position -1 and the significant rounding at position 0, so
        // here they are the same for 1.25 and differ for 12.5: 16.5.2 steps 7.3-7.6.
        assert_eq!(format("en", both(RoundingType::MorePrecision, MorePrecision), "1.25"), "1.3");
        assert_eq!(format("en", both(RoundingType::LessPrecision, LessPrecision), "1.25"), "1.3");
        assert_eq!(format("en", both(RoundingType::MorePrecision, MorePrecision), "12.5"), "12.5");
        assert_eq!(format("en", both(RoundingType::LessPrecision, LessPrecision), "12.5"), "13");
    }

    #[test]
    fn every_rounding_mode_rounds_a_halfway_value_its_own_way() {
        let at = |mode: RoundingMode, text: &str| {
            format(
                "en",
                move |slots: &mut NumberFormatSlots| {
                    slots.minimum_fraction_digits = 1;
                    slots.maximum_fraction_digits = 1;
                    slots.rounding_mode = mode;
                },
                text,
            )
        };
        assert_eq!(at(RoundingMode::Ceil, "1.25"), "1.3");
        assert_eq!(at(RoundingMode::Ceil, "-1.25"), "-1.2");
        assert_eq!(at(RoundingMode::Floor, "1.25"), "1.2");
        assert_eq!(at(RoundingMode::Floor, "-1.25"), "-1.3");
        assert_eq!(at(RoundingMode::Expand, "1.25"), "1.3");
        assert_eq!(at(RoundingMode::Expand, "-1.25"), "-1.3");
        assert_eq!(at(RoundingMode::Expand, "1.24"), "1.3");
        assert_eq!(at(RoundingMode::Trunc, "1.25"), "1.2");
        assert_eq!(at(RoundingMode::Trunc, "-1.25"), "-1.2");
        assert_eq!(at(RoundingMode::HalfCeil, "1.25"), "1.3");
        assert_eq!(at(RoundingMode::HalfCeil, "-1.25"), "-1.2");
        assert_eq!(at(RoundingMode::HalfCeil, "1.24"), "1.2");
        assert_eq!(at(RoundingMode::HalfFloor, "1.25"), "1.2");
        assert_eq!(at(RoundingMode::HalfFloor, "-1.25"), "-1.3");
        assert_eq!(at(RoundingMode::HalfFloor, "1.26"), "1.3");
        // Table 31's tie directions look towards the two infinities rather than away from and
        // towards zero, so a negative value gets the other one of the pair: -1.25 is a tie between
        // -1.2 and -1.3, and only halfCeil reaches for -1.2. Away from a tie both follow the nearer
        // multiple, which does not care about the sign.
        assert_eq!(at(RoundingMode::HalfCeil, "-1.24"), "-1.2");
        assert_eq!(at(RoundingMode::HalfFloor, "-1.26"), "-1.3");
        assert_eq!(at(RoundingMode::HalfExpand, "1.25"), "1.3");
        assert_eq!(at(RoundingMode::HalfExpand, "-1.25"), "-1.3");
        assert_eq!(at(RoundingMode::HalfTrunc, "1.25"), "1.2");
        assert_eq!(at(RoundingMode::HalfTrunc, "-1.25"), "-1.2");
        // Table 31's half-even looks at the digit being kept, so both of these land on an even one.
        assert_eq!(at(RoundingMode::HalfEven, "1.25"), "1.2");
        assert_eq!(at(RoundingMode::HalfEven, "1.35"), "1.4");
        assert_eq!(at(RoundingMode::HalfEven, "-1.25"), "-1.2");
    }

    #[test]
    fn a_rounding_increment_rounds_on_the_last_fraction_digit() {
        // 16.5.8 steps 2-3: the increment is a multiple of the lowest digit shown, so 25 with two
        // fraction digits means quarters of one percent of a unit.
        let at = |value: u32, mode: RoundingMode, text: &str| {
            format(
                "en",
                move |slots: &mut NumberFormatSlots| {
                    slots.minimum_fraction_digits = 2;
                    slots.maximum_fraction_digits = 2;
                    slots.rounding_increment = Increment::from_u32(value).unwrap();
                    slots.rounding_mode = mode;
                },
                text,
            )
        };
        // 0.125 sits exactly between the multiples 0.00 and 0.25, so half-expand takes the larger
        // and half-even, which is what 0.125 was measured against above, takes the even 0.00.
        assert_eq!(at(25, RoundingMode::HalfExpand, "0.125"), "0.25");
        assert_eq!(at(25, RoundingMode::HalfExpand, "0.375"), "0.50");
        assert_eq!(at(25, RoundingMode::HalfEven, "0.375"), "0.50");
        assert_eq!(at(25, RoundingMode::HalfEven, "0.625"), "0.50");
        assert_eq!(at(25, RoundingMode::HalfEven, "0.875"), "1.00");
        assert_eq!(at(25, RoundingMode::HalfCeil, "0.125"), "0.25");
        assert_eq!(at(25, RoundingMode::HalfFloor, "0.125"), "0.00");
        assert_eq!(at(5, RoundingMode::HalfEven, "0.025"), "0.00");
        assert_eq!(at(5, RoundingMode::HalfEven, "0.075"), "0.10");
        assert_eq!(at(2, RoundingMode::HalfEven, "0.0125"), "0.02");
        // The increment of 200 two digits down is 2.00, and 1.25 is nearer 2.00 than 0.00.
        assert_eq!(at(200, RoundingMode::HalfEven, "1.25"), "2.00");
        assert_eq!(at(200, RoundingMode::HalfExpand, "1.5"), "2.00");
        assert_eq!(at(1000, RoundingMode::Trunc, "12.5"), "10.00");
        assert_eq!(at(1000, RoundingMode::Expand, "12.5"), "20.00");
        assert_eq!(at(2500, RoundingMode::HalfEven, "1.25"), "0.00");
        assert_eq!(at(1, RoundingMode::HalfExpand, "1.005"), "1.01");
    }

    #[test]
    fn trailing_zero_display_strips_only_an_integral_result() {
        let strip = |slots: &mut NumberFormatSlots| {
            slots.minimum_fraction_digits = 2;
            slots.maximum_fraction_digits = 2;
            slots.trailing_zero_display = TrailingZeroDisplay::StripIfInteger;
        };
        assert_eq!(format("en", strip, "1"), "1");
        assert_eq!(format("en", strip, "1.5"), "1.50");
        assert_eq!(format("en", strip, "0"), "0");
        assert_eq!(format("en", strip, "1.25"), "1.25");
        // 1.005 at two digits is 1.01, which is not an integer, so there is nothing to strip.
        assert_eq!(format("en", strip, "1.005"), "1.01");
    }

    #[test]
    fn grouping_follows_the_locale_and_the_option() {
        let value = "1234567.891";
        assert_eq!(format("en", no_change, value), "1,234,567.891");
        assert_eq!(format("da", no_change, value), "1.234.567,891");
        assert_eq!(format("it", no_change, value), "1.234.567,891");

        // Italian and Danish data both say minimumGroupingDigits, and Italian's is 2.
        let four = "1234";
        assert_eq!(format("it", no_change, four), "1234");
        assert_eq!(format("en", no_change, four), "1,234");
        assert_eq!(format("da", no_change, four), "1.234");

        for (grouping, expected_it, expected_en) in [
            (UseGrouping::Never, "1234", "1234"),
            (UseGrouping::Min2, "1234", "1234"),
            (UseGrouping::Auto, "1234", "1,234"),
            (UseGrouping::Always, "1.234", "1,234"),
        ] {
            let slots = NumberFormatSlots::default();
            assert_eq!(
                renderer_with("it", grouping).format(&slots, &math_value_from_string(four)),
                expected_it,
                "it with {grouping:?}"
            );
            assert_eq!(
                renderer_with("en", grouping).format(&slots, &math_value_from_string(four)),
                expected_en,
                "en with {grouping:?}"
            );
        }

        let slots = NumberFormatSlots::default();
        assert_eq!(
            renderer_with("it", UseGrouping::Never).format(&slots, &math_value_from_string(value)),
            "1234567,891"
        );
        assert_eq!(
            renderer_with("it", UseGrouping::Always).format(&slots, &math_value_from_string(value)),
            "1.234.567,891"
        );
    }

    #[test]
    fn sign_display_covers_the_four_categories_of_16_5_10() {
        let with = |display: SignDisplay, text: &str| {
            format("en", move |slots| slots.sign_display = display, text)
        };
        assert_eq!(with(SignDisplay::Auto, "-0"), "-0");
        assert_eq!(with(SignDisplay::Always, "-0"), "-0");
        assert_eq!(with(SignDisplay::ExceptZero, "-0"), "0");
        assert_eq!(with(SignDisplay::Negative, "-0"), "0");
        assert_eq!(with(SignDisplay::Never, "-5"), "5");
        assert_eq!(with(SignDisplay::Always, "5"), "+5");
        assert_eq!(with(SignDisplay::ExceptZero, "5"), "+5");
        assert_eq!(with(SignDisplay::ExceptZero, "0"), "0");
        assert_eq!(with(SignDisplay::Negative, "5"), "5");
        assert_eq!(with(SignDisplay::Auto, "-5"), "-5");
        assert_eq!(with(SignDisplay::Never, "0"), "0");

        // A value that is negative but rounds to nothing is still in the negative-zero category,
        // which is 16.5.2 step 14.
        assert_eq!(format("en", |slots| slots.maximum_fraction_digits = 2, "-0.001"), "-0");
        assert_eq!(
            format(
                "en",
                |slots| {
                    slots.maximum_fraction_digits = 2;
                    slots.sign_display = SignDisplay::ExceptZero;
                },
                "-0.001"
            ),
            "0"
        );
    }

    #[test]
    fn the_special_values_take_the_sign_of_their_category() {
        let with = |display: SignDisplay, text: &str| {
            format("en", move |slots| slots.sign_display = display, text)
        };
        assert_eq!(with(SignDisplay::Auto, "NaN"), "NaN");
        assert_eq!(with(SignDisplay::Always, "NaN"), "+NaN");
        assert_eq!(with(SignDisplay::Never, "NaN"), "NaN");
        assert_eq!(with(SignDisplay::ExceptZero, "NaN"), "NaN");
        assert_eq!(with(SignDisplay::Auto, "Infinity"), "∞");
        assert_eq!(with(SignDisplay::Auto, "-Infinity"), "-∞");
        assert_eq!(with(SignDisplay::Never, "-Infinity"), "∞");
        assert_eq!(with(SignDisplay::Always, "-Infinity"), "-∞");
        assert_eq!(with(SignDisplay::Negative, "-Infinity"), "-∞");
        assert_eq!(with(SignDisplay::Negative, "Infinity"), "∞");
        // A percent pattern puts its sign in front of the number, so the space and the percent sign
        // follow the word.
        assert_eq!(format("en", |slots| slots.style = Style::Percent, "-Infinity"), "-∞%");
    }

    #[test]
    fn percent_uses_the_locale_own_spacing_and_sign() {
        assert_eq!(format("en", percent, "0.5"), "50%");
        assert_eq!(format("da", percent, "0.5"), "50\u{a0}%");
        assert_eq!(format("it", percent, "0.5"), "50%");
        assert_eq!(format("da", percent, "-0.5"), "-50\u{a0}%");
        assert_eq!(format("en", percent, "12.345"), "1,235%");

        let slots = slots_with(|slots| slots.style = Style::Percent);
        let parts = renderer("da").format_to_parts(&slots, &math_value_from_string("0.5"));
        let kinds: Vec<(&str, &str)> = parts
            .iter()
            .map(|part| (part.kind.name(), part.value.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("integer", "50"),
                ("literal", "\u{a0}"),
                ("percentSign", "%")
            ]
        );
    }

    fn slots_with(change: impl FnOnce(&mut NumberFormatSlots)) -> NumberFormatSlots {
        let mut slots = NumberFormatSlots::default();
        change(&mut slots);
        slots
    }

    #[test]
    fn the_parts_concatenate_to_the_string() {
        for (locale, text, change) in [
            ("en", "1234567.891", no_change as fn(&mut NumberFormatSlots)),
            ("da", "-1234.5", no_change),
            ("it", "0.00005", no_change),
            ("it", "1234", |slots| slots.use_grouping = UseGrouping::Always),
            ("en", "1.5", |slots| {
                slots.minimum_fraction_digits = 4;
                slots.maximum_fraction_digits = 4;
                slots.sign_display = SignDisplay::Always;
            }),
            ("da", "0.25", percent),
        ] {
            let mut slots = NumberFormatSlots::default();
            change(&mut slots);
            let value = math_value_from_string(text);
            let renderer = renderer(locale);
            let string = renderer.format(&slots, &value);
            let parts = renderer.format_to_parts(&slots, &value);
            let rebuilt: String = parts.iter().map(|part| part.value.as_str()).collect();
            assert_eq!(rebuilt, string, "{locale} {text}");
            assert!(
                parts
                    .iter()
                    .all(|part| !part.value.is_empty() && part.kind.name() != ""),
                "{locale} {text} has an empty part: {parts:?}"
            );
        }
    }

    #[test]
    fn parts_of_one_number_are_named_by_kind() {
        let slots = slots_with(|slots| {
            slots.minimum_fraction_digits = 2;
            slots.maximum_fraction_digits = 2;
            slots.sign_display = SignDisplay::Always;
        });
        let parts = renderer("en").format_to_parts(&slots, &math_value_from_string("-1234.567"));
        let kinds: Vec<(&str, &str)> = parts
            .iter()
            .map(|part| (part.kind.name(), part.value.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("minusSign", "-"),
                ("integer", "1"),
                ("group", ","),
                ("integer", "234"),
                ("decimal", "."),
                ("fraction", "57")
            ]
        );
    }

    #[test]
    fn bigint_and_long_string_values_keep_every_digit() {
        assert_eq!(
            format(
                "en",
                |slots| slots.maximum_fraction_digits = 3,
                "123456789012345678901234567890.123456789"
            ),
            "123,456,789,012,345,678,901,234,567,890.123"
        );

        let big = math_value_from_bigint("1234567890123456789").unwrap();
        assert_eq!(
            renderer("en").format(&NumberFormatSlots::default(), &big),
            "1,234,567,890,123,456,789"
        );
        assert_eq!(
            renderer("da").format(
                &NumberFormatSlots::default(),
                &math_value_from_bigint("-1234567").unwrap()
            ),
            "-1.234.567"
        );
        // 16.5.8's rounding does not invent digits the value does not have.
        assert_eq!(format("en", |slots| slots.maximum_fraction_digits = 0, "-1234"), "-1,234");
    }

    #[test]
    fn number_values_go_through_the_engines_own_spelling() {
        let of = |x: f64, text: &str| {
            renderer("en").format(&NumberFormatSlots::default(), &math_value_from_number(x, text))
        };
        assert_eq!(of(0.1, "0.1"), "0.1");
        assert_eq!(of(-0.0, "0"), "-0");
        assert_eq!(of(f64::NAN, "NaN"), "NaN");
        assert_eq!(of(f64::INFINITY, "Infinity"), "∞");
        assert_eq!(of(f64::NEG_INFINITY, "-Infinity"), "-∞");
        // A double of 10^21 or above is spelled out, not put in exponent form, by 16.5.15 step 4.3.
        assert_eq!(of(1e21, "1e+21"), "1,000,000,000,000,000,000,000");
        assert_eq!(of(1e-7, "1e-7"), "0");
        assert_eq!(of(1.5e-7, "1.5e-7"), "0");
    }

    #[test]
    fn the_string_grammar_is_read_the_way_the_grammar_reads_it() {
        assert_eq!(math_value_from_string(" 12 "), math_value_from_string("12"));
        assert_eq!(math_value_from_string("+.5"), math_value_from_string("0.5"));
        assert_eq!(math_value_from_string("5."), math_value_from_string("5"));
        assert_eq!(math_value_from_string("0x10"), math_value_from_string("16"));
        assert_eq!(math_value_from_string("0b11"), math_value_from_string("3"));
        assert_eq!(math_value_from_string("0o17"), math_value_from_string("15"));
        assert_eq!(math_value_from_string("1e3"), math_value_from_string("1000"));
        assert_eq!(math_value_from_string("1E-3"), math_value_from_string("0.001"));
        assert_eq!(math_value_from_string(""), IntlMathValue::zero());
        assert_eq!(math_value_from_string("-0"), IntlMathValue::NegativeZero);
        for rejected in [
            "1e", "1.2.3", "1_000", "0x", "x1", "12a", "-", "+", "1e5e5", ".e3",
        ] {
            assert_eq!(math_value_from_string(rejected), IntlMathValue::NaN, "{rejected}");
        }

        // RoundMVResult: what a Number cannot hold is an infinity or a zero, whatever the literal
        // said.
        assert_eq!(math_value_from_string("1e309"), IntlMathValue::PositiveInfinity);
        assert_eq!(math_value_from_string("-1e400"), IntlMathValue::NegativeInfinity);
        assert_eq!(math_value_from_string("1e40000"), IntlMathValue::PositiveInfinity);
        assert_eq!(math_value_from_string("1e-400"), IntlMathValue::zero());
        assert_eq!(math_value_from_string("-1e-400"), IntlMathValue::NegativeZero);
        // Inside the range of a double the literal stays exactly as written.
        assert_eq!(format("en", |slots| slots.maximum_fraction_digits = 0, "1e-300"), "0");
        assert_eq!(
            renderer("en").format(
                &slots_with(|slots| slots.maximum_fraction_digits = 0),
                &math_value_from_string("1e25")
            ),
            "10,000,000,000,000,000,000,000,000"
        );
    }

    #[test]
    fn bigint_takes_only_its_own_form() {
        assert_eq!(math_value_from_bigint("-123").unwrap(), math_value_from_string("-123"));
        assert_eq!(math_value_from_bigint("0").unwrap(), IntlMathValue::zero());
        for rejected in ["", "1e5", "1.5", "-0x10", " 12 ", "abc"] {
            assert_eq!(math_value_from_bigint(rejected), None, "{rejected}");
        }

        // Past the digit limit the value is reported, not rounded into something wrong.
        let digits = "1".repeat(MAX_EXACT_DIGITS + 1);
        assert_eq!(math_value_from_bigint(&digits), None);
    }

    #[test]
    fn increments_only_come_from_the_list_of_16_1_2_step_7() {
        assert_eq!(
            Increment::VALUES.to_vec(),
            vec![
                1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000, 2500, 5000
            ]
        );
        for value in Increment::VALUES {
            let increment = Increment::from_u32(value).unwrap();
            assert_eq!(increment.as_u32(), value);
        }
        for value in [0, 3, 4, 24, 26, 5001, 6000] {
            assert_eq!(Increment::from_u32(value), None, "{value}");
        }
        assert!(Increment::One.is_one());
        assert!(!Increment::TwoHundredFifty.is_one());
    }

    #[test]
    fn the_option_names_are_the_ones_resolvedoptions_reports() {
        assert_eq!(Style::Decimal.as_str(), "decimal");
        assert_eq!(Style::Percent.as_str(), "percent");
        assert_eq!(RoundingPriority::MorePrecision.as_str(), "morePrecision");
        assert_eq!(SignDisplay::ExceptZero.as_str(), "exceptZero");
        assert_eq!(TrailingZeroDisplay::StripIfInteger.as_str(), "stripIfInteger");
        assert_eq!(UseGrouping::Never.resolved_name(), None);
        assert_eq!(UseGrouping::Min2.resolved_name(), Some("min2"));
        assert_eq!(UseGrouping::Auto.resolved_name(), Some("auto"));
        assert_eq!(UseGrouping::Always.resolved_name(), Some("always"));
        assert_eq!(
            [
                RoundingMode::Ceil.as_str(),
                RoundingMode::Floor.as_str(),
                RoundingMode::Expand.as_str(),
                RoundingMode::Trunc.as_str(),
                RoundingMode::HalfCeil.as_str(),
                RoundingMode::HalfFloor.as_str(),
                RoundingMode::HalfExpand.as_str(),
                RoundingMode::HalfTrunc.as_str(),
                RoundingMode::HalfEven.as_str(),
            ],
            [
                "ceil",
                "floor",
                "expand",
                "trunc",
                "halfCeil",
                "halfFloor",
                "halfExpand",
                "halfTrunc",
                "halfEven"
            ]
        );
    }

    #[test]
    fn the_locales_do_not_share_separators_and_their_defaults_are_their_own() {
        let slots = slots_with(|slots| {
            slots.minimum_fraction_digits = 1;
            slots.maximum_fraction_digits = 1;
        });
        let value = math_value_from_string("1234.5");
        assert_eq!(renderer("en").format(&slots, &value), "1,234.5");
        assert_eq!(renderer("da").format(&slots, &value), "1.234,5");
        // Italian data asks for at least four digits in front of a group, so 1234 stays as it is
        // even though its separators are the same as Danish ones.
        assert_eq!(renderer("it").format(&slots, &value), "1234,5");
        // The sign affixes come from the same data as the rest, and both of them are prefixes here.
        let renderer = renderer("en");
        let signs = renderer.signs();
        assert_eq!(signs.of(Sign::Negative), ("-", ""));
        assert_eq!(signs.of(Sign::Positive), ("+", ""));
    }
}
