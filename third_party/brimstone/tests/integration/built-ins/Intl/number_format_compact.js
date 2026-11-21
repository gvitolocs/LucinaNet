/*---
description: Intl.NumberFormat with the notation "compact", which is 16.5.12's ComputeExponent, the
  two-significant-digit rounding 16.1.2 step 23 gives a compact format nobody set a precision on,
  and the compact piece 16.5.4 puts next to the number. The expected strings are the ones V8 with
  ICU 78 gives for these three locales.
---*/

// The Danish and Italian short forms separate the number from the abbreviation with U+00A0, which
// is what the pattern data has and not something this test should read as a plain space.
const NBSP = " ";

const short_en = new Intl.NumberFormat("en-US", { notation: "compact" });
const long_en = new Intl.NumberFormat("en-US", { notation: "compact", compactDisplay: "long" });

// The piece of 16.5.12 the whole notation is about: three digits off the value and a letter for
// them. The example-site-style call reads "1.2K" rather than the eleven characters of the whole number.
assert.sameValue(short_en.format(1234), "1.2K");
assert.sameValue(short_en.format(1234.5), "1.2K");
assert.sameValue(short_en.format(1000), "1K");
assert.sameValue(short_en.format(1500), "1.5K");
assert.sameValue(short_en.format(2207), "2.2K");
assert.sameValue(short_en.format(-1234), "-1.2K");

// 16.5.12 steps 5 to 10: the scale is chosen, the value is rounded against it, and a rounding that
// moves the most significant digit moves the scale as well. `999999` is a million, not a thousand
// thousand, and the exponent comes from the locale's list rather than from the 33 of the spec's
// worked example, which is why `1e23` stays in the trillions the way Italian and Danish leave it.
assert.sameValue(short_en.format(999999), "1M");
assert.sameValue(short_en.format(1000000), "1M");
assert.sameValue(short_en.format(1234567), "1.2M");
assert.sameValue(short_en.format(1e13), "10T");
assert.sameValue(short_en.format(1e23), "100,000,000,000T");
assert.sameValue(short_en.format(1e24), "1,000,000,000,000T");

// 16.1.2 step 19.3 with step 23: a compact format with nothing said about its precision rounds to
// two significant digits, but keeps whichever of the two roundings is the finer, so the fraction
// digits disappear as soon as there are two digits in front of them. These are the numbers
// test262's own compact test asserts, and the reason the rounding is not simply `3` digits.
assert.sameValue(short_en.format(987654321), "988M");
assert.sameValue(short_en.format(98765432), "99M");
assert.sameValue(short_en.format(98765), "99K");
assert.sameValue(short_en.format(9876), "9.9K");
assert.sameValue(short_en.format(15127), "15K");
assert.sameValue(short_en.format(145000), "145K");
assert.sameValue(short_en.format(159), "159");
assert.sameValue(short_en.format(15.9), "16");
assert.sameValue(short_en.format(1.59), "1.6");
assert.sameValue(short_en.format(0.159), "0.16");
assert.sameValue(short_en.format(0.0159), "0.016");
assert.sameValue(short_en.format(0.00159), "0.0016");

// Below a thousand the locale's list has no scale to name, so the number stands on its own.
assert.sameValue(short_en.format(999), "999");
assert.sameValue(short_en.format(0.05), "0.05");
assert.sameValue(short_en.format(0), "0");
assert.sameValue(short_en.format(-0), "-0");

// 16.5.15: the same arithmetic over the three other ways a value arrives.
assert.sameValue(short_en.format("1234"), "1.2K");
assert.sameValue(short_en.format(1234567n), "1.2M");
assert.sameValue(short_en.format(NaN), "NaN");
assert.sameValue(short_en.format(Infinity), "∞");
assert.sameValue(short_en.format(-Infinity), "-∞");

// `compactDisplay: "long"` reads the other list of the locale, whose words are chosen by the
// plural category of the scaled number: 1.2 million is the plural and a million the singular.
assert.sameValue(long_en.format(1000), "1 thousand");
assert.sameValue(long_en.format(1234), "1.2 thousand");
assert.sameValue(long_en.format(98765), "99 thousand");
assert.sameValue(long_en.format(1234567), "1.2 million");
assert.sameValue(long_en.format(999999), "1 million");
assert.sameValue(long_en.format(1e13), "10 trillion");

// 16.1.1 step 13: a currency takes its own precision as the digit defaults only for the standard
// notation, so a compact dollar is rounded by step 23 and not to cents.
assert.sameValue(
  new Intl.NumberFormat("en-US", { notation: "compact", style: "currency", currency: "USD" }).format(1234),
  "$1.2K"
);
assert.sameValue(
  new Intl.NumberFormat("en-US", { notation: "compact", style: "currency", currency: "USD" }).format(145000),
  "$145K"
);
assert.sameValue(
  new Intl.NumberFormat("en-US", { notation: "compact", style: "currency", currency: "USD" }).format(999999),
  "$1M"
);
assert.sameValue(
  new Intl.NumberFormat("en-US", { notation: "compact", style: "currency", currency: "USD" }).format(-1234),
  "-$1.2K"
);
assert.sameValue(
  new Intl.NumberFormat("en-US", { notation: "compact", style: "currency", currency: "USD" }).format(0.05),
  "$0.05"
);
assert.sameValue(
  new Intl.NumberFormat("en-US", { notation: "compact", style: "currency", currency: "USD" }).format(0),
  "$0"
);

// Percent scales by 100 first (16.5.2 step 5.1.2), which is what ComputeExponent of step 5.1.3 then
// reads the magnitude of, and the percent sign is the pattern's own tail after the compact piece.
const percent = new Intl.NumberFormat("en-US", { notation: "compact", style: "percent" });
assert.sameValue(percent.format(0.05), "5%");
assert.sameValue(percent.format(999), "100K%");
assert.sameValue(percent.format(1234), "123K%");
assert.sameValue(percent.format(15127), "1.5M%");
assert.sameValue(percent.format(-1234), "-123K%");

// The two locales whose compact list separates the number with U+00A0 in the short form and with an
// ordinary space in the long one, and whose Italian thousand is a word with no room in it for the
// number at all.
const short_da = new Intl.NumberFormat("da", { notation: "compact" });
assert.sameValue(short_da.format(1234), "1,2" + NBSP + "t");
assert.sameValue(short_da.format(98765), "99" + NBSP + "t");
assert.sameValue(short_da.format(1234567), "1,2" + NBSP + "mio.");
assert.sameValue(short_da.format(1e13), "10" + NBSP + "bio.");
assert.sameValue(new Intl.NumberFormat("da", { notation: "compact", compactDisplay: "long" }).format(1234567), "1,2 million");
assert.sameValue(new Intl.NumberFormat("da", { notation: "compact", compactDisplay: "long" }).format(1000), "1 tusind");

const short_it = new Intl.NumberFormat("it", { notation: "compact" });
assert.sameValue(short_it.format(1234), "1,2K");
assert.sameValue(short_it.format(999999), "1" + NBSP + "Mln");
assert.sameValue(short_it.format(1234567), "1,2" + NBSP + "Mln");
assert.sameValue(short_it.format(1e13), "10" + NBSP + "Bln");
const long_it = new Intl.NumberFormat("it", { notation: "compact", compactDisplay: "long" });
assert.sameValue(long_it.format(1000), "mille");
assert.sameValue(long_it.format(1234), "1,2 mila");
assert.sameValue(long_it.format(98765), "99 mila");
assert.sameValue(long_it.format(1234567), "1,2 milioni");
assert.sameValue(long_it.format(999999), "1 milione");

// 16.1.2 steps 19.3 and 22.1: say a precision and the two-significant-digit rounding is gone; the
// digits asked for are the digits used, at whatever scale.
const three = new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 3 });
assert.sameValue(three.format(1234), "1.234K");
assert.sameValue(three.format(15127), "15.127K");
assert.sameValue(three.format(999999), "999.999K");
const two_min = new Intl.NumberFormat("en-US", { notation: "compact", minimumFractionDigits: 2 });
assert.sameValue(two_min.format(1234), "1.234K");
assert.sameValue(two_min.format(1500), "1.50K");
assert.sameValue(two_min.format(145000), "145.00K");
assert.sameValue(two_min.format(0), "0.00");
const two_significant = new Intl.NumberFormat("en-US", {
  notation: "compact",
  minimumSignificantDigits: 2,
  maximumSignificantDigits: 2,
});
assert.sameValue(two_significant.format(999), "1.0K");
assert.sameValue(two_significant.format(145000), "150K");
assert.sameValue(two_significant.format(999999), "1.0M");

// 16.1.1 step 19.2: a compact format groups by "min2" whether or not the program said so, and the
// long significand of 1e23 is where the difference between that and no grouping at all shows.
assert.sameValue(
  new Intl.NumberFormat("en-US", { notation: "compact", useGrouping: false }).format(1e23),
  "100000000000T"
);
assert.sameValue(short_en.format(1e23), "100,000,000,000T");
assert.sameValue(
  new Intl.NumberFormat("it", { notation: "compact", useGrouping: "always" }).format(1e23),
  "100.000.000.000" + NBSP + "Bln"
);

// 16.5.6 over 16.5.4: the abbreviation is a part of its own, type "compact", and the space the
// pattern puts in front of it is a "literal".
const parts = short_en.formatToParts(1234);
assert.compareArray(parts.map((part) => part.type), ["integer", "decimal", "fraction", "compact"]);
assert.compareArray(parts.map((part) => part.value), ["1", ".", "2", "K"]);
assert.sameValue(parts.map((part) => part.value).join(""), short_en.format(1234));

const long_parts = long_en.formatToParts(1234);
assert.compareArray(
  long_parts.map((part) => part.type),
  ["integer", "decimal", "fraction", "literal", "compact"]
);
assert.compareArray(long_parts.map((part) => part.value), ["1", ".", "2", " ", "thousand"]);

// The sign stays outside the number, where 16.5.10's pattern puts it, and the grouping of a long
// significand keeps its "group" parts.
assert.compareArray(
  short_en.formatToParts(-98765).map((part) => part.type),
  ["minusSign", "integer", "compact"]
);
assert.compareArray(
  short_en.formatToParts(1e23).map((part) => part.type),
  ["integer", "group", "integer", "group", "integer", "group", "integer", "compact"]
);
assert.compareArray(short_en.formatToParts(159).map((part) => part.type), ["integer"]);

// 16.5.4 step 4.3 over a pattern that has no place for the number: Italian "mille" is the whole
// format, so the parts are the one word.
assert.compareArray(long_it.formatToParts(1000).map((part) => part.type), ["compact"]);
assert.compareArray(long_it.formatToParts(1000).map((part) => part.value), ["mille"]);

// 16.3.2's Table 28 rows: the two new ones, with the four digit rows step 23 wrote and the
// "min2" grouping of step 19.2.
const resolved = short_en.resolvedOptions();
assert.sameValue(resolved.notation, "compact");
assert.sameValue(resolved.compactDisplay, "short");
assert.sameValue(resolved.useGrouping, "min2");
assert.sameValue(resolved.minimumFractionDigits, 0);
assert.sameValue(resolved.maximumFractionDigits, 0);
assert.sameValue(resolved.minimumSignificantDigits, 1);
assert.sameValue(resolved.maximumSignificantDigits, 2);
assert.sameValue(resolved.roundingPriority, "morePrecision");
assert.sameValue(long_en.resolvedOptions().compactDisplay, "long");

// A precision that was said leaves the rounding of step 27 in place, and with it the "auto"
// priority and no significant-digit rows at all.
const resolved_two = two_min.resolvedOptions();
assert.sameValue(resolved_two.minimumFractionDigits, 2);
assert.sameValue(resolved_two.maximumFractionDigits, 3);
assert.sameValue(resolved_two.roundingPriority, "auto");
assert.sameValue("minimumSignificantDigits" in resolved_two, false);
assert.sameValue("maximumSignificantDigits" in resolved_two, false);

// [[CompactDisplay]] is written for the compact notation only (16.1.1 step 19.1), so the standard
// one says nothing about it even though step 17 read and validated the option.
const standard = new Intl.NumberFormat("en-US", { compactDisplay: "long" }).resolvedOptions();
assert.sameValue(standard.notation, "standard");
assert.sameValue("compactDisplay" in standard, false);
assert.sameValue(standard.useGrouping, "auto");

// 16.1.1 step 11's list is still the four names, and the two exponent notations are still the two
// this build refuses rather than renders wrong; 9.2.11 is what rejects a name outside the list.
assert.throws(RangeError, () => new Intl.NumberFormat("en", { notation: "scientific" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { notation: "engineering" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { notation: "compactish" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { compactDisplay: "shorter" }));
// 9.2.11's GetOption reads the name with ToString, so a number is an out-of-range name rather than
// a type error; only a symbol cannot be turned into a string at all.
assert.throws(RangeError, () => new Intl.NumberFormat("en", { notation: 7 }));
assert.throws(TypeError, () => new Intl.NumberFormat("en", { notation: Symbol("compact") }));

// A compact format with a rounding increment has neither digit list to put the increment in, so
// 16.1.2 step 28 says so: the TypeError of step 28.1 for the rounding type, and the RangeError of
// step 28.2 for the increment that a lone `maximumFractionDigits` cannot carry.
assert.throws(TypeError, () => new Intl.NumberFormat("en", { notation: "compact", roundingIncrement: 5 }));
assert.throws(
  RangeError,
  () => new Intl.NumberFormat("en", { notation: "compact", roundingIncrement: 5, maximumFractionDigits: 2 })
);
