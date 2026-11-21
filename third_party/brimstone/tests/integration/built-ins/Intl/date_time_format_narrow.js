// Copyright (C) 2026 LucinaJS contributors. All rights reserved.
// This code is governed by the BSD license that can be found in the LICENSE file.
/*---
description: The `narrow` width of Table 16 of Intl.DateTimeFormat - the weekday, the month and the
  era at their shortest. This build has no `Length` for it, so the field set is patterned at the
  longest length of its group, which is what V8 patterns a narrow name onto as well, and the name
  symbols of that pattern are then read at `EEEEE`, `MMMMM` and `GGGGG` out of the same data. A
  narrow day period is the phrase of `bbbbb`, which the day period tables of this build have no entry
  for, and is therefore refused rather than answered with the `AM` of another width.
includes: [compareArray.js]
---*/

// 2026-02-18, the day every format below renders: a Wednesday, in the first half of a month whose
// number is a single digit, at 09:36:32.180.
const day = new Date(Date.UTC(2026, 1, 18, 9, 36, 32, 180));
const utc = { timeZone: "UTC" };

// CLDR writes the English day period behind a narrow no-break space, and `icu_datetime` keeps it;
// the width of the gap is asserted rather than guessed, as in the main DateTimeFormat test.
const NNBSP = "\u202f";
// The range connector this build joins two ends with where 11.5.8 has no range pattern to merge
// them into. V8's merged patterns carry a thin space on each side of the same dash.
const DASH = "\u2013";

const narrow = (locale, options) => new Intl.DateTimeFormat(locale, { ...options, ...utc });

// 11.3's Table 3 and Table 16: `narrow` is a listed width of the weekday, the month and the era, so
// it is accepted and answered. One field on its own is the shortest case of all three names, in all
// three locales this build carries data for.
assert.sameValue(narrow("en", { weekday: "narrow" }).format(day), "W", "the English narrow weekday");
assert.sameValue(narrow("da", { weekday: "narrow" }).format(day), "O", "Dansk onsdag");
assert.sameValue(narrow("it", { weekday: "narrow" }).format(day), "M", "Italiano mercoledi");
assert.sameValue(narrow("en", { month: "narrow" }).format(day), "F", "the English narrow month");
assert.sameValue(narrow("da", { month: "narrow" }).format(day), "F", "and Danish, whose short is feb.");
assert.sameValue(narrow("it", { month: "narrow" }).format(day), "F", "and Italian, whose short is feb");
assert.sameValue(narrow("en", { era: "narrow", year: "numeric" }).format(day), "2026 A", "the English narrow era");
assert.sameValue(narrow("da", { era: "narrow", year: "numeric" }).format(day), "2026 eKr", "Dansk efter Kristus");
assert.sameValue(narrow("it", { era: "narrow", year: "numeric" }).format(day), "2026 dC", "Italiano dopo Cristo");

// The name is the one of that width and not the shortest one of another: each of these differs from
// what the same bag asks at `short`, which is what a faked narrow would have answered with.
assert.sameValue(narrow("en", { weekday: "narrow" }).format(day) !== narrow("en", { weekday: "short" }).format(day), true, "narrow is not short: W against Wed");
assert.sameValue(narrow("da", { month: "narrow" }).format(day) !== narrow("da", { month: "short" }).format(day), true, "and not the Danish feb.");
assert.sameValue(narrow("en", { era: "narrow", year: "numeric" }).format(day) !== narrow("en", { era: "short", year: "numeric" }).format(day), true, "A against AD");

// 11.5.5: the parts of the same rendering, with the narrow name as the value of its own record.
assert.compareArray(narrow("en", { weekday: "narrow" }).formatToParts(day).map((part) => part.type), ["weekday"], "one part for one field");
assert.compareArray(narrow("da", { weekday: "narrow" }).formatToParts(day).map((part) => part.type), ["weekday"], "the same in Danish");
assert.compareArray(narrow("it", { month: "narrow" }).formatToParts(day).map((part) => part.type), ["month"], "and for the month");
assert.sameValue(narrow("en", { weekday: "narrow" }).formatToParts(day)[0].value, "W", "the value of the weekday record");
assert.sameValue(narrow("it", { month: "narrow" }).formatToParts(day)[0].value, "F", "the value of the month record");
assert.sameValue(narrow("it", { era: "narrow", year: "numeric" }).formatToParts(day)[2].type, "era", "the era is the last record");
assert.sameValue(narrow("it", { era: "narrow", year: "numeric" }).formatToParts(day)[2].value, "dC", "and reads dC");

// 11.3.2: what the bag asked is what `resolvedOptions` reports - the rendering borrows the longest
// pattern of the group, and says nothing about it.
assert.sameValue(narrow("en", { weekday: "narrow" }).resolvedOptions().weekday, "narrow", "the width is reported as asked");
assert.sameValue(narrow("da", { month: "narrow" }).resolvedOptions().month, "narrow");
assert.sameValue(narrow("it", { era: "narrow", year: "numeric" }).resolvedOptions().era, "narrow");
assert.sameValue(narrow("it", { era: "narrow", year: "numeric" }).resolvedOptions().year, "numeric", "and the fields beside it as themselves");

// The shape of the rendering is the longest group's, which is what V8 selects for the same bag: the
// records of a narrow month and day come in the order of the long one, and the narrow is only the
// name inside them.
assert.compareArray(
  narrow("en", { month: "narrow", day: "numeric" }).formatToParts(day).map((part) => part.type),
  narrow("en", { month: "long", day: "numeric" }).formatToParts(day).map((part) => part.type),
  "a narrow month holds the place of the long one in English");
assert.compareArray(
  narrow("da", { month: "narrow", day: "numeric" }).formatToParts(day).map((part) => part.type),
  narrow("da", { month: "long", day: "numeric" }).formatToParts(day).map((part) => part.type),
  "and the day comes first in Danish");
assert.sameValue(
  Object.keys(narrow("en", { month: "narrow", day: "numeric" }).resolvedOptions()).sort().join(),
  Object.keys(narrow("en", { month: "long", day: "numeric" }).resolvedOptions()).sort().join(),
  "the same keys of resolvedOptions, whatever the width asked");

// A narrow name beside fields at their own widths: the narrow reaches one field of the group, and
// every other field keeps what it was asked for.
assert.sameValue(narrow("en", { weekday: "narrow", month: "long", day: "numeric" }).format(day), "W, February 18", "the narrow weekday of a full date");
assert.sameValue(narrow("da", { weekday: "narrow", month: "long", day: "numeric" }).format(day), "O 18. februar", "the same in Danish");
assert.sameValue(narrow("it", { weekday: "narrow", month: "long", day: "numeric" }).format(day), "M 18 febbraio", "and in Italian");
assert.compareArray(
  narrow("en", { weekday: "narrow", month: "long", day: "numeric" }).formatToParts(day).map((part) => part.type),
  ["weekday", "literal", "month", "literal", "day"], "the five records of the English date");
assert.compareArray(
  narrow("it", { weekday: "narrow", month: "long", day: "numeric" }).formatToParts(day).map((part) => part.type),
  ["weekday", "literal", "day", "literal", "month"], "and their order in Italian");
assert.sameValue(
  narrow("en", { weekday: "narrow", month: "long", day: "numeric" }).formatToParts(day)[2].value,
  "February", "the long month of the same date is not narrowed");
assert.sameValue(narrow("en", { weekday: "narrow", month: "long", day: "numeric" }).resolvedOptions().month, "long", "and is reported as asked");
assert.sameValue(narrow("it", { weekday: "narrow", month: "long", day: "numeric" }).resolvedOptions().weekday, "narrow");

// The month alone with the day, with the year, and with both: the four skeletons the narrow month
// reaches in each of the three locales.
assert.sameValue(narrow("en", { month: "narrow", day: "numeric" }).format(day), "F 18", "the narrow month and a numeric day");
assert.sameValue(narrow("da", { month: "narrow", day: "numeric" }).format(day), "18. F", "the day first in Danish");
assert.sameValue(narrow("it", { month: "narrow", day: "numeric" }).format(day), "18 F", "and in Italian");
assert.sameValue(narrow("en", { month: "narrow", day: "numeric", year: "numeric" }).format(day), "F 18, 2026", "the year hangs behind the comma");
assert.sameValue(narrow("da", { month: "narrow", day: "numeric", year: "numeric" }).format(day), "18. F 2026");
assert.sameValue(narrow("it", { month: "narrow", day: "numeric", year: "numeric" }).format(day), "18 F 2026");
assert.sameValue(narrow("en", { month: "narrow", year: "numeric" }).format(day), "F 2026", "the month and the year alone");
assert.sameValue(narrow("da", { month: "narrow", year: "numeric" }).format(day), "F 2026");
assert.sameValue(narrow("it", { month: "narrow", year: "numeric" }).format(day), "F 2026");
assert.sameValue(narrow("en", { month: "narrow", day: "2-digit", year: "2-digit" }).format(day), "F 18, 26", "padded day and two digits of year");
assert.sameValue(narrow("da", { month: "narrow", day: "2-digit", year: "2-digit" }).format(day), "18. F 26");
assert.sameValue(narrow("it", { month: "narrow", day: "2-digit", year: "2-digit" }).format(day), "18 F 26");
assert.sameValue(narrow("it", { month: "narrow", day: "2-digit" }).format(day), "18 F", "a 2-digit day is still the padded one");

// Two narrow fields of one group, and the era narrow beside a long date: the rewrite reaches every
// narrow it was told about, and only those.
assert.sameValue(narrow("en", { weekday: "narrow", month: "narrow", day: "numeric" }).format(day), "W, F 18", "weekday and month both narrow");
assert.sameValue(narrow("da", { weekday: "narrow", month: "narrow", day: "numeric" }).format(day), "O 18. F");
assert.sameValue(narrow("it", { weekday: "narrow", month: "narrow", day: "numeric" }).format(day), "M 18 F");
assert.compareArray(
  narrow("en", { weekday: "narrow", month: "narrow", day: "numeric" }).formatToParts(day).map((part) => part.type),
  ["weekday", "literal", "month", "literal", "day"], "and the records say so");
assert.sameValue(narrow("en", { era: "narrow", month: "long", day: "numeric", year: "numeric" }).format(day), "February 18, 2026 A", "the era hangs at the end of a long date");
assert.sameValue(narrow("da", { era: "narrow", month: "long", day: "numeric", year: "numeric" }).format(day), "18. februar 2026 eKr");
assert.sameValue(narrow("it", { era: "narrow", month: "long", day: "numeric", year: "numeric" }).format(day), "18 febbraio 2026 dC");
assert.sameValue(narrow("en", { era: "narrow", month: "narrow", day: "numeric", year: "numeric" }).format(day), "F 18, 2026 A", "narrow month and narrow era together");
assert.sameValue(narrow("da", { era: "narrow", month: "narrow", day: "numeric", year: "numeric" }).format(day), "18. F 2026 eKr");
assert.sameValue(narrow("it", { era: "narrow", month: "narrow", day: "numeric", year: "numeric" }).format(day), "18 F 2026 dC");

// An era with nothing beside it takes the numeric date of its group, the same skeleton the short and
// the long era take; the digits of that date are this build's, which for Italian are unpadded.
assert.sameValue(narrow("en", { era: "narrow" }).format(day), "2/18/2026 A", "the English numeric date with the narrow era");
assert.sameValue(narrow("da", { era: "narrow" }).format(day), "18.2.2026 eKr", "the Danish one, dotted");
assert.sameValue(narrow("it", { era: "narrow" }).format(day), "18/2/2026 dC", "and the Italian one, which this build leaves unpadded");
assert.sameValue(narrow("en", { era: "narrow", year: "2-digit" }).format(day), "26 A", "two digits of year, one of era");
assert.sameValue(narrow("da", { era: "narrow", year: "2-digit" }).format(day), "26 eKr");
assert.sameValue(narrow("it", { era: "narrow", year: "2-digit" }).format(day), "26 dC");

// The narrow of a date that has a time beside it. The date and the time are two patterns and the
// locale's own glue joins them; this build glues the English date to the time with ` at `, which is
// what it has always done with a long date too, while V8 puts a comma there. The records of the
// rendering, and every value but the glue, are the same in both.
const narrowWithTime = narrow("en", { month: "narrow", day: "numeric", hour: "numeric", minute: "2-digit" });
assert.compareArray(narrowWithTime.formatToParts(day).map((part) => part.type), ["month", "literal", "day", "literal", "hour", "literal", "minute", "literal", "dayPeriod"], "the nine records of a narrow date and a time");
assert.sameValue(narrowWithTime.formatToParts(day)[0].value, "F", "the narrow month of it");
assert.sameValue(narrowWithTime.formatToParts(day)[6].value, "36", "and the minute, which is not narrowed");
assert.sameValue(narrowWithTime.format(day), "F 18 at 9:36" + NNBSP + "AM", "the glue of this build");
assert.sameValue(narrow("en", { weekday: "narrow", hour: "numeric", minute: "2-digit" }).format(day), "W at 9:36" + NNBSP + "AM", "a narrow weekday with a time");
assert.sameValue(narrow("da", { month: "narrow", day: "numeric", hour: "numeric", minute: "numeric", second: "numeric" }).format(day), "18. F kl. 09.36.32", "Danish glues with kl.");
assert.sameValue(narrow("it", { weekday: "narrow", hour: "numeric", minute: "2-digit" }).format(day), "M alle ore 09:36", "and Italian alle ore");
assert.sameValue(narrow("en", { month: "narrow", day: "numeric", hour: "numeric", minute: "2-digit" }).resolvedOptions().month, "narrow", "the width survives the glue");
assert.sameValue(narrow("it", { weekday: "narrow", hour: "numeric", minute: "2-digit" }).resolvedOptions().weekday, "narrow");

// 11.3.4 and 11.5.8: a range of two ends. A narrow field set takes the fallback of steps 21 to 23 -
// each end spelled with the format's own pattern and the two joined by the connector - because the
// range patterns of the data are keyed at the group's skeleton and carry the name of that skeleton,
// not the narrow of the ask. V8 merges them where its range pattern allows; the ends below are the
// same text either way.
const from = new Date(Date.UTC(2026, 1, 18, 7, 33, 20));
const to = new Date(Date.UTC(2026, 1, 20, 16, 45, 0));
assert.sameValue(narrow("en", { month: "narrow", day: "numeric" }).formatRange(from, to), "F 18" + DASH + "F 20", "two narrow ends");
assert.sameValue(narrow("da", { month: "narrow", day: "numeric" }).formatRange(from, to), "18. F" + DASH + "20. F");
assert.sameValue(narrow("it", { month: "narrow", day: "numeric" }).formatRange(from, to), "18 F" + DASH + "20 F");
assert.sameValue(narrow("en", { month: "narrow", day: "numeric", year: "numeric" }).formatRange(from, to), "F 18, 2026" + DASH + "F 20, 2026", "and with the year");
assert.sameValue(narrow("da", { month: "narrow", day: "numeric", year: "numeric" }).formatRange(from, to), "18. F 2026" + DASH + "20. F 2026");
assert.sameValue(narrow("it", { month: "narrow", day: "numeric", year: "numeric" }).formatRange(from, to), "18 F 2026" + DASH + "20 F 2026");
assert.sameValue(narrow("en", { weekday: "narrow", month: "long", day: "numeric" }).formatRange(from, to), "W, February 18" + DASH + "F, February 20", "the weekday of each end names that end");
assert.sameValue(narrow("da", { weekday: "narrow", month: "long", day: "numeric" }).formatRange(from, to), "O 18. februar" + DASH + "F 20. februar");
assert.sameValue(narrow("it", { weekday: "narrow", month: "long", day: "numeric" }).formatRange(from, to), "M 18 febbraio" + DASH + "V 20 febbraio");

const narrowRangeParts = narrow("en", { month: "narrow", day: "numeric", year: "numeric" }).formatRangeToParts(from, to);
assert.sameValue(narrowRangeParts.length, 11, "five records of each end and the connector");
assert.compareArray(narrowRangeParts.map((part) => part.source), ["startRange", "startRange", "startRange", "startRange", "startRange", "shared", "endRange", "endRange", "endRange", "endRange", "endRange"], "11.5.10's source of every one");
assert.compareArray(narrowRangeParts.map((part) => part.value), ["F", " ", "18", ", ", "2026", DASH, "F", " ", "20", ", ", "2026"], "and the text they carry");
assert.compareArray(
  narrow("en", { weekday: "narrow", month: "narrow", day: "numeric" }).formatRangeToParts(from, to).map((part) => part.source).filter((source) => source === "shared"),
  ["shared"], "one connector is the only shared record of an unmerged range");

// 11.5.8 steps 13 to 16 come before any range pattern: two ends that are one date are one date, and
// a narrow format answers it the same way.
const afternoon = new Date(Date.UTC(2026, 1, 18, 13, 3, 20));
const oneDate = narrow("en", { month: "narrow", day: "numeric", year: "numeric" });
assert.sameValue(oneDate.formatRange(from, afternoon), oneDate.format(from), "an afternoon changes no rendered field");
assert.compareArray(oneDate.formatRangeToParts(from, afternoon).map((part) => part.source), ["shared", "shared", "shared", "shared", "shared"], "and every record of it is shared");
assert.sameValue(oneDate.formatRange(from, from), oneDate.format(from), "two equal ends are one date");

// 20.4.2 to 20.4.3: the same width through the three `Date.prototype` methods, which build a
// DateTimeFormat and read it once.
assert.sameValue(day.toLocaleDateString("en", { weekday: "narrow", ...utc }), "W", "toLocaleDateString of a narrow weekday");
assert.sameValue(day.toLocaleDateString("it", { month: "narrow", day: "numeric", ...utc }), "18 F", "and of a narrow month");
assert.sameValue(day.toLocaleDateString("da", { weekday: "narrow", month: "long", day: "numeric", ...utc }), "O 18. februar", "the whole group at once");
assert.sameValue(day.toLocaleString("da", { era: "narrow", year: "numeric", ...utc }), "2026 eKr", "toLocaleString reaches the era as well");

// What the data cannot answer is refused at the constructor, loudly. The narrow day period is the
// phrase of `bbbbb` - the `in the morning` V8 reads out of its own tables - and the day period data
// of this build carries the `AM` names of `a` and nothing else. To answer the ask with `AM` would be
// the faked output the other widths of Table 16 are not.
assert.throws(RangeError, () => narrow("en", { hour: "numeric", dayPeriod: "narrow" }), "a narrow day period is refused");
assert.throws(RangeError, () => narrow("da", { dayPeriod: "narrow" }), "with or without the hour");
assert.throws(RangeError, () => narrow("it", { hour: "2-digit", dayPeriod: "narrow" }), "and in every locale");
let narrowDayPeriodMessage = "";
try {
  narrow("en", { hour: "numeric", dayPeriod: "narrow" });
} catch (error) {
  narrowDayPeriodMessage = error.message;
}
assert.sameValue(narrowDayPeriodMessage.includes("the narrow day period has no phrase names in this build"), true, "and the refusal says which names are missing: " + narrowDayPeriodMessage);

// Table 16 lists no narrow for the year, and none for the name of a zone: those are the option value
// errors of 11.1.2 step 5, the same as for any width off the list.
assert.throws(RangeError, () => narrow("en", { year: "narrow" }), "the year has no narrow");
assert.throws(RangeError, () => narrow("en", { timeZoneName: "narrow" }), "and neither does the name of a zone");

// One group, two widths: the rule this build has always applied to a long month with a short
// weekday applies to a narrow one, because the narrow is patterned as the long. V8 answers both.
assert.throws(RangeError, () => narrow("en", { weekday: "narrow", month: "short", day: "numeric" }), "a narrow weekday with a short month is two widths of one group");
assert.throws(RangeError, () => narrow("en", { weekday: "long", month: "short", day: "numeric" }), "which is the same refusal a long weekday meets");
let mixedWidthMessage = "";
try {
  narrow("en", { weekday: "narrow", month: "short", day: "numeric" });
} catch (error) {
  mixedWidthMessage = error.message;
}
assert.sameValue(mixedWidthMessage.includes("two different widths"), true, "and the message names the mix: " + mixedWidthMessage);

// A group no pattern of this build spells - the weekday and the month with no day beside them - is
// refused the same way whether the names are narrow or long, because the refusal is the skeleton's.
assert.throws(RangeError, () => narrow("en", { weekday: "narrow", month: "narrow" }), "no pattern for weekday and month alone");
assert.throws(RangeError, () => narrow("en", { weekday: "long", month: "long" }), "and none for the long of the same two");

// 11.1.2 step 10: a shorthand and a component of the same group are not mixed, narrow included.
assert.throws(TypeError, () => narrow("en", { era: "narrow", dateStyle: "long" }), "a shorthand with a component is a TypeError before any data is read");
