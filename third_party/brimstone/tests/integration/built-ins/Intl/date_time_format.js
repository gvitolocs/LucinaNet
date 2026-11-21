// Copyright (C) 2026 LucinaJS contributors. All rights reserved.
// This code is governed by the BSD license that can be found in the LICENSE file.
/*---
description: Intl.DateTimeFormat and the three Date.prototype.toLocale*String methods, which
  ECMA-402 clause 11 and clauses 20.4.1 to 20.4.3 of define. This build carries the date and time
  patterns of English, Danish and Italian, and falls back to English for every other locale; the time
  zone names of a zone are the offset forms unless the build was made with `tz-names`.
includes: [compareArray.js]
---*/

// 2026-02-18, the day every format below renders. It is a Wednesday, in the first half of a month
// whose number is a single digit, and its time is 09:36:32.180 - one digit of hour and of day and a
// fraction, which is what makes a rendering of two digits say that the pattern pads.
const day = new Date(Date.UTC(2026, 1, 18, 9, 36, 32, 180));
// Midnight and noon, the two hours the four hour cycles disagree about, and the reason a format of
// Table 4's `h24` is not simply the `H23` rendering with a different name.
const midnight = new Date(Date.UTC(2026, 1, 18, 0, 0));
const noon = new Date(Date.UTC(2026, 1, 18, 12, 0));
// The same instant in the zone every `timeZone`-less format below is asked about, so that a host in
// another zone still answers the same strings: the formats that name no zone are the host's own, and
// the assertions that care about a civil time name one.
const utc = { timeZone: "UTC" };

const en = new Intl.DateTimeFormat("en", utc);
const da = new Intl.DateTimeFormat("da", utc);
const it = new Intl.DateTimeFormat("it", utc);

// CLDR writes the English day period behind a narrow no-break space, and `icu_datetime` keeps it in
// both of its answers: `format` and `formatToParts` agree here, where V8's concatenation of the same
// pattern puts an ordinary space. The width of the gap is therefore asserted, not guessed.
const NNBSP = "\u202f";

// 11.1.1: the constructor is a function named "DateTimeFormat" of length 0, whose prototype is
// %Function.prototype% and whose `prototype` has its constructor back on it.
assert.sameValue(typeof Intl.DateTimeFormat, "function");
assert.sameValue(Intl.DateTimeFormat.length, 0);
assert.sameValue(Intl.DateTimeFormat.name, "DateTimeFormat");
assert.sameValue(Object.getPrototypeOf(Intl.DateTimeFormat), Function.prototype);
assert.sameValue(Intl.DateTimeFormat.prototype.constructor, Intl.DateTimeFormat);

// 11.1.1 steps 1 and 2: it works with and without `new`, the legacy `[[FallbackSymbol]]` rule of
// `Intl` service constructors included, and a subclass gets its own prototype.
const withoutNew = Intl.DateTimeFormat("en");
assert.sameValue(withoutNew instanceof Intl.DateTimeFormat, true);
assert.sameValue(withoutNew.resolvedOptions().locale, "en");
assert.sameValue(Intl.DateTimeFormat.call(undefined, "it").resolvedOptions().locale, "it");

class Shouter extends Intl.DateTimeFormat {}
const shouter = new Shouter("da", utc);
assert.sameValue(Object.getPrototypeOf(shouter), Shouter.prototype);
assert.sameValue(shouter instanceof Shouter, true);
assert.sameValue(shouter instanceof Intl.DateTimeFormat, true);
assert.sameValue(shouter.resolvedOptions().locale, "da");

// 11.4's `Intl.DateTimeFormat.prototype` and its @@toStringTag.
assert.sameValue(Object.prototype.toString.call(Intl.DateTimeFormat.prototype), "[object Intl.DateTimeFormat]");
assert.sameValue(Object.prototype.toString.call(new Intl.DateTimeFormat()), "[object Intl.DateTimeFormat]");
assert.sameValue(Intl.DateTimeFormat.prototype.formatToParts.length, 1);
assert.sameValue(Intl.DateTimeFormat.prototype.resolvedOptions.length, 0);
assert.sameValue(Intl.DateTimeFormat.prototype.formatRange.length, 2);
assert.sameValue(Intl.DateTimeFormat.prototype.formatRangeToParts.length, 2);

// 11.1.1 step 5 and 11.3's steps 1: the methods need an initialized instance of their own class, and
// answer the TypeError of `DateTimeFormat`'s checks rather than a format of somebody else's.
assert.throws(TypeError, () => Intl.DateTimeFormat.prototype.resolvedOptions.call({}), "resolvedOptions on a plain object");
assert.throws(TypeError, () => Intl.DateTimeFormat.prototype.format.call(new Intl.DateTimeFormat("en")), "format on the prototype");
assert.throws(TypeError, () => Intl.DateTimeFormat.prototype.formatToParts.call(Intl.NumberFormat()), "formatToParts on a NumberFormat");

// 11.2.2 `supportedLocalesOf`: the three locales of this build are supported, an unsupported tag
// falls back rather than being refused, and a malformed tag is the RangeError of 9.2.2.
assert.compareArray(Intl.DateTimeFormat.supportedLocalesOf("da"), ["da"]);
assert.compareArray(Intl.DateTimeFormat.supportedLocalesOf(["en", "it"]), ["en", "it"]);
assert.compareArray(Intl.DateTimeFormat.supportedLocalesOf("da-DK"), ["da-DK"]);
assert.compareArray(Intl.DateTimeFormat.supportedLocalesOf(["xx", "de"]), [], "no supported locale in the list");
assert.throws(RangeError, () => Intl.DateTimeFormat.supportedLocalesOf("en-US-x"), "a malformed tag");

// 11.3.2 `resolvedOptions`, the five rows of Table 15 that come from ResolveLocale rather than from
// the pattern. `gregory` and `latn` are the only calendars and numbering systems with data here, so
// a `-u-ca-persian-` is dropped rather than resolved into a calendar that has no patterns.
const defaults = en.resolvedOptions();
assert.sameValue(defaults.locale, "en");
assert.sameValue(defaults.calendar, "gregory");
assert.sameValue(defaults.numberingSystem, "latn");
assert.sameValue(defaults.timeZone, "UTC");
assert.sameValue(new Intl.DateTimeFormat("it-IT-u-ca-persian-nu-arab").resolvedOptions().calendar, "gregory", "an unsupported calendar is dropped");
assert.sameValue(new Intl.DateTimeFormat("it-u-hc-h23", { hour: "numeric" }).resolvedOptions().hourCycle, "h23", "the -u-hc- subtag is the hour cycle");

// An unsupported locale falls back to English and never throws, which is the decision in force for
// every service of this build.
assert.sameValue(new Intl.DateTimeFormat("fo").resolvedOptions().locale, "en");
assert.sameValue(new Intl.DateTimeFormat(["kl", "zz"]).resolvedOptions().locale, "en");
assert.sameValue(new Date(Date.UTC(2026, 1, 18)).toLocaleString("fo", { dateStyle: "short", timeZone: "UTC" }), "2/18/26", "the fallback renders as English");

// The zone row of Table 15 is 11.5.3's canonical name: an IANA id as the database spells it, `UTC`
// for the zone of no offset however it was written, and an offset in the `+HH:mm` of 11.1.3.
assert.sameValue(new Intl.DateTimeFormat("en", { timeZone: "utc" }).resolvedOptions().timeZone, "UTC");
assert.sameValue(new Intl.DateTimeFormat("en", { timeZone: "GMT" }).resolvedOptions().timeZone, "UTC");
assert.sameValue(new Intl.DateTimeFormat("en", { timeZone: "Etc/UTC" }).resolvedOptions().timeZone, "UTC");
assert.sameValue(new Intl.DateTimeFormat("en", { timeZone: "europe/copenhagen" }).resolvedOptions().timeZone, "Europe/Copenhagen", "case-insensitive and canonicalized");
assert.sameValue(new Intl.DateTimeFormat("en", { timeZone: "+05:30" }).resolvedOptions().timeZone, "+05:30");
assert.sameValue(new Intl.DateTimeFormat("en", { timeZone: "-08" }).resolvedOptions().timeZone, "-08:00", "11.5.3's offset grammar");
assert.sameValue(new Intl.DateTimeFormat("en", { timeZone: "-0800" }).resolvedOptions().timeZone, "-08:00", "the minute may run on without the colon");
assert.throws(RangeError, () => new Intl.DateTimeFormat("en", { timeZone: "-8" }), "a one-digit hour is not the offset grammar of a timeZone");
assert.throws(RangeError, () => new Intl.DateTimeFormat("en", { timeZone: "Mars/Olympus_Mons" }), "an id the database does not have");
assert.throws(RangeError, () => new Intl.DateTimeFormat("en", { timeZone: "+05:30:15" }), "an offset with seconds");
assert.throws(RangeError, () => new Intl.DateTimeFormat("en", { timeZone: "Z" }), "Z is neither an id nor an offset");

// 11.1.2 steps 12 to 16 and Table 4: the hour cycle the locale counts by, which is 12-hour in
// English and 24-hour in Danish and Italian, and which `hour12` and `hourCycle` both override.
assert.sameValue(new Intl.DateTimeFormat("en", { hour: "numeric" }).resolvedOptions().hourCycle, "h12");
assert.sameValue(new Intl.DateTimeFormat("da", { hour: "numeric" }).resolvedOptions().hourCycle, "h23");
assert.sameValue(new Intl.DateTimeFormat("it", { hour: "numeric" }).resolvedOptions().hourCycle, "h23");
assert.sameValue(new Intl.DateTimeFormat("en", { hour: "numeric", hour12: false }).resolvedOptions().hourCycle, "h23");
assert.sameValue(new Intl.DateTimeFormat("da", { hour: "numeric", hour12: true }).resolvedOptions().hourCycle, "h12");
assert.sameValue(new Intl.DateTimeFormat("en", { hour: "numeric", hourCycle: "h24" }).resolvedOptions().hourCycle, "h24");
assert.sameValue(new Intl.DateTimeFormat("en", { hour: "numeric", hour12: false, hourCycle: "h11" }).resolvedOptions().hourCycle, "h23", "hour12 wins over hourCycle");

// 9.2.7 steps 12.6.3 and 12.11 with the hour cycle, which is the one key whose locale data has a null
// among its values (11.2.3): the `-u-hc-` of the request is the answer when nothing else asks for the
// cycle, and an option that does asks takes the keyword off the reported locale rather than rewriting
// it. A `hour12` goes further and blanks the keyword whatever the `hourCycle` option said beside it -
// 11.1.2 step 3.3 - because the cycle is then asked of the locale data, not of the request (steps 14
// to 16).
const hcTag = "en-u-hc-h11";
const hourOf = (locale, options) => new Intl.DateTimeFormat(locale, { hour: "numeric", ...options }).resolvedOptions();
assert.sameValue(hourOf(hcTag, {}).locale, hcTag, "a keyword nobody overrides is reported");
assert.sameValue(hourOf(hcTag, {}).hourCycle, "h11");
assert.sameValue(hourOf(hcTag, { hourCycle: "h23" }).locale, "en", "a different hourCycle takes the keyword off");
assert.sameValue(hourOf(hcTag, { hourCycle: "h23" }).hourCycle, "h23");
assert.sameValue(hourOf(hcTag, { hourCycle: "h11" }).locale, hcTag, "the same hourCycle leaves the keyword alone");
assert.sameValue(hourOf(hcTag, { hour12: false }).locale, "en", "a hour12 blanks the keyword");
assert.sameValue(hourOf(hcTag, { hour12: false }).hourCycle, "h23", "and asks the cycle of the locale data");
assert.sameValue(hourOf(hcTag, { hour12: true, hourCycle: "h23" }).hourCycle, "h12", "a hour12 wins over the hourCycle named beside it");
assert.sameValue(hourOf("it-u-hc-h24", { hour12: true }).hourCycle, "h12", "and over the cycle the tag named");
assert.sameValue(new Intl.DateTimeFormat("en", { year: "numeric" }).resolvedOptions().hourCycle, undefined, "a format with no hour reports no cycle");

// 11.3.3's `format` getter answers a bound function, and the same one every time: 11.3.3 caches it on
// the instance the first time it is asked.
assert.sameValue(en.format instanceof Intl.DateTimeFormat, false);
assert.sameValue(typeof en.format, "function");
assert.sameValue(en.format, en.format, "the bound format is cached");
assert.sameValue(en.format.name, "", "11.3.3 step 4.2 names the function it hands out nothing");
assert.sameValue(en.format.length, 1);
assert.sameValue(en.format(day), en.format(day), "the same instant twice is the same string");

// 11.3.2's Table 15 read off the matched pattern, and 11.5.4's table of what a dateStyle means: the
// shorthand replaces the components whole, and resolvedOptions reports the components of the pattern
// rather than the two rows of Table 15 that a shorthand leaves out.
const full = new Intl.DateTimeFormat("en", { dateStyle: "full", ...utc }).resolvedOptions();
assert.sameValue(full.dateStyle, "full");
assert.sameValue(full.year, undefined, "a shorthand does not report components");
assert.sameValue(full.month, undefined);
assert.sameValue(new Intl.DateTimeFormat("da", { dateStyle: "short", timeStyle: "short", ...utc }).resolvedOptions().timeStyle, "short");
assert.sameValue(new Intl.DateTimeFormat("en", { dateStyle: "medium", ...utc }).resolvedOptions().timeStyle, undefined);

// The component rows of Table 15 are the widths of the rendering: `{ hour: "numeric" }` in a locale
// whose pattern counts with a zero reports the `2-digit` it renders rather than the request.
const components = new Intl.DateTimeFormat("en", { weekday: "long", year: "numeric", month: "long", day: "numeric", hour: "numeric", minute: "2-digit", second: "2-digit", ...utc }).resolvedOptions();
assert.sameValue(components.weekday, "long");
assert.sameValue(components.year, "numeric");
assert.sameValue(components.month, "long");
assert.sameValue(components.day, "numeric");
assert.sameValue(components.hour, "numeric");
assert.sameValue(components.minute, "2-digit");
assert.sameValue(components.second, "2-digit");
assert.sameValue(new Intl.DateTimeFormat("it", { month: "short", day: "numeric", ...utc }).resolvedOptions().month, "short");
// The `narrow` of Table 16 is a width CLDR keys its date skeletons at and this generation of
// `icu_datetime` has no `Length` for, so the field set is patterned at the longest length of its
// group - which is what V8 patterns a narrow name onto too - and the name symbols of that pattern
// are read at their narrow. These two assertions refused the width outright, as an earlier ICU4X
// generation had to; see date_time_format_narrow.js for the rest of the width.
assert.sameValue(new Intl.DateTimeFormat("it", { month: "narrow", ...utc }).format(day), "F", "the narrow month is the single letter, not the short one");
assert.sameValue(new Intl.DateTimeFormat("en", { era: "narrow", year: "numeric", ...utc }).format(day), "2026 A", "and so is a narrow era");
assert.sameValue(new Intl.DateTimeFormat("en", { era: "short", year: "numeric", ...utc }).resolvedOptions().era, "short");
assert.sameValue(new Intl.DateTimeFormat("en", { fractionalSecondDigits: 3, hour: "numeric", minute: "numeric", second: "numeric", ...utc }).resolvedOptions().fractionalSecondDigits, 3);

// The option values are 9.2.11's `GetOption` over the table of Table 16: anything outside the list of
// a row is the RangeError of its step 5, and `fractionalSecondDigits` is the number of 1 to 3.
assert.throws(RangeError, () => new Intl.DateTimeFormat("en", { year: "wide" }), "a width no row of Table 16 has");
assert.throws(RangeError, () => new Intl.DateTimeFormat("en", { month: 1 }), "a width that is not a string");
assert.throws(RangeError, () => new Intl.DateTimeFormat("en", { dateStyle: "huge" }), "a shorthand that is not one of four");
assert.throws(RangeError, () => new Intl.DateTimeFormat("en", { timeZoneName: "medium" }), "a zone name width Table 16 does not list");
assert.throws(RangeError, () => new Intl.DateTimeFormat("en", { fractionalSecondDigits: 4, hour: "numeric" }), "four digits is out of 11.1.2 step 24.2");
assert.throws(RangeError, () => new Intl.DateTimeFormat("en", { fractionalSecondDigits: 0, hour: "numeric" }), "so is zero");
assert.throws(TypeError, () => new Intl.DateTimeFormat("en", { get year() { throw new TypeError("seen"); } }), "a getter is seen where the spec runs it");

// 11.3.1: what the bag asked is what the format shows. English first, in the zone the assertions name
// rather than the host's, and with the widths of V8's own answer for the same bag.
assert.sameValue(new Intl.DateTimeFormat("en", { dateStyle: "full", ...utc }).format(day), "Wednesday, February 18, 2026");
assert.sameValue(new Intl.DateTimeFormat("en", { dateStyle: "long", ...utc }).format(day), "February 18, 2026");
assert.sameValue(new Intl.DateTimeFormat("en", { dateStyle: "medium", ...utc }).format(day), "Feb 18, 2026");
assert.sameValue(new Intl.DateTimeFormat("en", { dateStyle: "short", ...utc }).format(day), "2/18/26");
assert.sameValue(new Intl.DateTimeFormat("en", { timeStyle: "short", ...utc }).format(day), "9:36" + NNBSP + "AM");
assert.sameValue(new Intl.DateTimeFormat("en", { timeStyle: "medium", ...utc }).format(day), "9:36:32" + NNBSP + "AM");
assert.sameValue(new Intl.DateTimeFormat("en", { dateStyle: "medium", timeStyle: "short", ...utc }).format(day), "Feb 18, 2026, 9:36" + NNBSP + "AM");
assert.sameValue(new Intl.DateTimeFormat("en", { month: "long", day: "numeric", ...utc }).format(day), "February 18");
assert.sameValue(new Intl.DateTimeFormat("en", { month: "short", day: "numeric", ...utc }).format(day), "Feb 18", "the short month with a numeric day");
assert.sameValue(new Intl.DateTimeFormat("en", { month: "short", day: "numeric", ...utc }).format(day), "Feb 18", "the short month with a numeric day");
assert.sameValue(new Intl.DateTimeFormat("en", { month: "numeric", day: "2-digit", ...utc }).format(day), "2/18");
assert.sameValue(new Intl.DateTimeFormat("en", { year: "2-digit", ...utc }).format(day), "26");
assert.sameValue(new Intl.DateTimeFormat("en", { weekday: "short", ...utc }).format(day), "Wed");
assert.sameValue(new Intl.DateTimeFormat("en", { era: "short", year: "numeric", ...utc }).format(day), "2026 AD");
assert.sameValue(new Intl.DateTimeFormat("en", { hour: "2-digit", minute: "2-digit", ...utc }).format(day), "09:36" + NNBSP + "AM");
assert.sameValue(new Intl.DateTimeFormat("en", { hour: "numeric", hour12: false, ...utc }).format(day), "09");
assert.sameValue(new Intl.DateTimeFormat("en", { hourCycle: "h24", hour: "2-digit", minute: "2-digit", ...utc }).format(midnight), "24:00", "the k cycle numbers midnight 24");
assert.sameValue(new Intl.DateTimeFormat("en", { hourCycle: "h11", hour: "numeric", ...utc }).format(midnight), "0" + NNBSP + "AM", "h11 is CLDRs K: it numbers midnight 0");
assert.sameValue(new Intl.DateTimeFormat("en", { hourCycle: "h11", hour: "numeric", ...utc }).format(noon), "0" + NNBSP + "PM", "and noon 0 of the afternoon");
assert.sameValue(new Intl.DateTimeFormat("en", { hourCycle: "h12", hour: "numeric", ...utc }).format(midnight), "12" + NNBSP + "AM", "h12 is CLDRs h");
assert.sameValue(new Intl.DateTimeFormat("da", { hourCycle: "h23", hour: "numeric" }).format(midnight), "00", "h23 is the 0-23 count, and Danish pads it");

// Danish: the `.` separators of its patterns, the `kl.` of a time, and a 24-hour count. The `den`
// of CLDR's full Danish date is a fugue tag, and ICU4X's pattern marker is keyed at the skeleton
// without it, so this build answers the full date of the same fields and no literal.
assert.sameValue(new Intl.DateTimeFormat("da", { dateStyle: "full", ...utc }).format(day), "onsdag 18. februar 2026");
assert.sameValue(new Intl.DateTimeFormat("da", { dateStyle: "long", ...utc }).format(day), "18. februar 2026");
assert.sameValue(new Intl.DateTimeFormat("da", { dateStyle: "medium", ...utc }).format(day), "18. feb. 2026");
assert.sameValue(new Intl.DateTimeFormat("da", { dateStyle: "short", ...utc }).format(day), "18.02.2026");
assert.sameValue(new Intl.DateTimeFormat("da", { timeStyle: "short", ...utc }).format(day), "09.36");
assert.sameValue(new Intl.DateTimeFormat("da", { dateStyle: "short", timeStyle: "short", ...utc }).format(day), "18.02.2026, 09.36");
assert.sameValue(new Intl.DateTimeFormat("da", { month: "short", day: "2-digit", ...utc }).format(day), "18. feb.");
assert.sameValue(new Intl.DateTimeFormat("da", { era: "short", year: "numeric", ...utc }).format(day), "2026 e.Kr.");

// Italian: no separators of the day and the month, and `alle ore` for a time.
assert.sameValue(new Intl.DateTimeFormat("it", { dateStyle: "full", ...utc }).format(day), "mercoledì 18 febbraio 2026");
assert.sameValue(new Intl.DateTimeFormat("it", { dateStyle: "medium", ...utc }).format(day), "18 feb 2026");
assert.sameValue(new Intl.DateTimeFormat("it", { dateStyle: "short", ...utc }).format(day), "18/02/26");
assert.sameValue(new Intl.DateTimeFormat("it", { timeStyle: "short", ...utc }).format(day), "09:36");
// An hour alone is the one field whose width the rendering cannot be talked out of: this build
// formats the group the CLDR skeleton of its locale is, and Table 15 reports what came out. Italian
// counts `09` and Danish `09` where Danish is what V8 answers too, and the row says `2-digit`.
assert.sameValue(new Intl.DateTimeFormat("it", { hour: "numeric", ...utc }).format(day), "09");
assert.sameValue(new Intl.DateTimeFormat("it", { hour: "numeric", ...utc }).resolvedOptions().hour, "2-digit", "the width of the pattern, not the width of the request");
assert.sameValue(new Intl.DateTimeFormat("da", { hour: "numeric", ...utc }).format(day), "09");
assert.sameValue(new Intl.DateTimeFormat("en", { hour: "numeric", ...utc }).format(day), "9" + NNBSP + "AM", "English counts by twelve and does not pad");
assert.sameValue(new Intl.DateTimeFormat("it", { weekday: "short", month: "short", day: "numeric", ...utc }).format(day), "mer 18 feb");
assert.sameValue(new Intl.DateTimeFormat("it", { era: "short", year: "numeric", ...utc }).format(day), "2026 d.C.");

// 11.5.6's zone: the same instant read in two zones is two answers, and the offset of a zone is the
// one it had at that moment rather than the one of another day of the year.
const copenhagen = new Intl.DateTimeFormat("en", { dateStyle: "short", timeStyle: "short", timeZone: "Europe/Copenhagen" });
assert.sameValue(copenhagen.format(day), "2/18/26, 10:36" + NNBSP + "AM");
// 11.1.2 steps 17 to 19: the shorthand and the components of the same group are a TypeError, not a
// bag whose components the shorthand quietly overrides.
assert.throws(TypeError, () => new Intl.DateTimeFormat("en", { dateStyle: "short", hour: "2-digit" }), "a shorthand with an hour");
assert.throws(TypeError, () => new Intl.DateTimeFormat("en", { timeStyle: "short", year: "numeric" }), "a shorthand with a year");
assert.sameValue(new Intl.DateTimeFormat("en", { hour: "2-digit", minute: "2-digit", timeZone: "+05:30" }).format(day), "03:06" + NNBSP + "PM", "an offset zone renders the offset");
assert.sameValue(new Intl.DateTimeFormat("en", { hour: "2-digit", minute: "2-digit", timeZone: "America/New_York" }).format(day), "04:36" + NNBSP + "AM", "and a named one its own offset");
assert.sameValue(new Intl.DateTimeFormat("en", { month: "short", day: "numeric", timeZone: "Pacific/Kiritimati" }).format(new Date(Date.UTC(2026, 1, 18, 12, 0))), "Feb 19", "the zone of UTC+14 moves noon to the next day");
assert.sameValue(new Intl.DateTimeFormat("en", { month: "short", day: "numeric", timeZone: "America/New_York" }).format(new Date(Date.UTC(2026, 1, 18, 0, 0))), "Feb 17", "and a zone behind it moves the same instant back");
assert.sameValue(
  new Intl.DateTimeFormat("en", { hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZoneName: "shortOffset", timeZone: "Pacific/Kiritimati" }).format(new Date(Date.UTC(2026, 1, 18, 10, 0))),
  "00:00 GMT+14",
  "the fourteen hours an IANA zone can be ahead by",
);
assert.sameValue(
  new Intl.DateTimeFormat("en", { month: "short", day: "numeric", hour: "2-digit", hourCycle: "h23", timeZone: "Pacific/Auckland" }).format(new Date(Date.UTC(2026, 6, 18, 23, 30))),
  "Jul 19, 11",
  "a July instant in New Zealand is next-day morning there",
);

// Table 16's six `timeZoneName` forms. The build bakes CLDR's zone name tables, so the generic and
// the specific names print as well as the offsets, and both are read at the moment being formatted:
// the same zone in July is the summer time it is then, not the winter one.
const july = new Date(Date.UTC(2026, 6, 18, 9, 36, 32, 180));
const zone = (locale, form, timeZone, date) =>
  new Intl.DateTimeFormat(locale, { hour: "2-digit", timeZoneName: form, timeZone }).format(date);
assert.sameValue(zone("en", "short", "America/New_York", day), "04" + NNBSP + "AM EST");
assert.sameValue(zone("en", "short", "America/New_York", july), "05" + NNBSP + "AM EDT", "the short form follows the daylight saving");
assert.sameValue(zone("en", "long", "America/New_York", day), "04" + NNBSP + "AM Eastern Standard Time");
assert.sameValue(zone("en", "long", "America/New_York", july), "05" + NNBSP + "AM Eastern Daylight Time");
assert.sameValue(zone("en", "short", "Europe/Copenhagen", day), "10" + NNBSP + "AM GMT+1", "a metazone with no short name of its own is the offset");
assert.sameValue(zone("en", "shortOffset", "America/New_York", day), "04" + NNBSP + "AM GMT-5");
assert.sameValue(zone("en", "longOffset", "America/New_York", day), "04" + NNBSP + "AM GMT-05:00");
assert.sameValue(zone("en", "longOffset", "UTC", day), "09" + NNBSP + "AM GMT+00:00");
assert.sameValue(zone("en", "shortGeneric", "Europe/Copenhagen", day), "10" + NNBSP + "AM Denmark Time");
assert.sameValue(zone("en", "shortGeneric", "Europe/Copenhagen", july), "11" + NNBSP + "AM Denmark Time", "the generic name does not follow the saving");
assert.sameValue(zone("en", "longGeneric", "Europe/Copenhagen", day), "10" + NNBSP + "AM Central European Time");
assert.sameValue(zone("en", "shortGeneric", "+05:30", day), "03" + NNBSP + "PM GMT+5:30", "an offset zone is named by its offset in every form");
assert.sameValue(zone("da", "short", "Europe/Copenhagen", day), "10 CET");
assert.sameValue(zone("da", "long", "Europe/Copenhagen", day), "10 Centraleuropæisk normaltid");
assert.sameValue(zone("da", "long", "Europe/Copenhagen", july), "11 Centraleuropæisk sommertid", "and Danish says sommertid in July");
assert.sameValue(zone("da", "longOffset", "Europe/Copenhagen", july), "11 GMT+02.00", "the Danish offset writes its minutes with a dot");
assert.sameValue(zone("da", "longGeneric", "Europe/Copenhagen", day), "10 Centraleuropæisk tid");
assert.sameValue(zone("it", "short", "Europe/Rome", july), "11 CEST");
assert.sameValue(zone("it", "long", "Europe/Rome", day), "10 Ora standard dell’Europa centrale");
assert.sameValue(zone("it", "longGeneric", "Europe/Rome", day), "10 Ora dell’Europa centrale");
assert.sameValue(
  new Intl.DateTimeFormat("en", { hour: "2-digit", timeZoneName: "shortGeneric" }).resolvedOptions().timeZoneName,
  "shortGeneric",
  "Table 15 reports the form the bag asked for",
);
assert.sameValue(
  new Intl.DateTimeFormat("en", { hour: "2-digit", ...utc }).resolvedOptions().timeZoneName,
  undefined,
  "and nothing when the bag named no zone field",
);

// 11.3.6 `formatToParts`: the `{ type, value }` records of 11.5.7, whose types are the fields of
// Table 3 and whose literals are the text between them.
const parts = new Intl.DateTimeFormat("en", { dateStyle: "medium", timeStyle: "short", ...utc }).formatToParts(day);
assert.sameValue(compareArray(parts.map((part) => part.type), ["month", "literal", "day", "literal", "year", "literal", "hour", "literal", "minute", "literal", "dayPeriod"]), true, "the parts of an English date and time");
assert.sameValue(compareArray(parts.map((part) => part.value), ["Feb", " ", "18", ", ", "2026", ", ", "9", ":", "36", NNBSP, "AM"]), true, "and their values");
assert.sameValue(parts.every((part) => typeof part.type === "string" && typeof part.value === "string"), true, "every record is of two strings");

// A bag of every group at one width, which is the shape 11.5.11 matches to a single pattern: the
// records below are the whole partition of 11.5.7, so the two arrays say what the parts of one
// rendering are down to the space in front of the day period and the dot of the fraction.
const rich = new Intl.DateTimeFormat("en", {
  weekday: "short",
  year: "2-digit",
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
  second: "numeric",
  fractionalSecondDigits: 3,
  timeZoneName: "short",
  hourCycle: "h12",
  timeZone: "UTC",
}).formatToParts(day);
assert.sameValue(
  compareArray(
    rich.map((part) => part.type),
    ["weekday", "literal", "month", "literal", "day", "literal", "year", "literal", "hour", "literal", "minute", "literal", "second", "literal", "fractionalSecond", "literal", "dayPeriod", "literal", "timeZoneName"],
  ),
  true,
  "the fields of Table 3, in the order the pattern puts them",
);
assert.sameValue(
  compareArray(
    rich.map((part) => part.value),
    ["Wed", ", ", "Feb", " ", "18", ", ", "26", ", ", "9", ":", "36", ":", "32", ".", "180", NNBSP, "AM", " ", "UTC"],
  ),
  true,
  "and the text of each of them",
);
assert.sameValue(rich.every((part) => part.source === undefined), true, "a single date has no source of 11.5.10");
assert.sameValue(rich.some((part) => part.type === "era"), false, "a 2-digit year of the present era brings no era");

// The dot of the fraction is a literal of the pattern rather than a digit of the `second`, so it is
// its own record and the `second` keeps only its own two.
const fraction = new Intl.DateTimeFormat("en", { hour: "2-digit", minute: "2-digit", second: "2-digit", fractionalSecondDigits: 3, ...utc }).formatToParts(day);
assert.sameValue(compareArray(fraction.map((part) => part.type), ["hour", "literal", "minute", "literal", "second", "literal", "fractionalSecond", "literal", "dayPeriod"]), true, "the parts of a fractional second");
assert.sameValue(fraction.find((part) => part.type === "second").value, "32", "the second of the bag, with nothing after it");
assert.sameValue(fraction.find((part) => part.type === "fractionalSecond").value, "180", "three digits of fraction");

assert.sameValue(new Intl.DateTimeFormat("da", { dateStyle: "short", ...utc }).formatToParts(day).every((part) => part.type !== "literal" || part.value.length > 0), true, "a literal is never empty");

// 11.3.4 `formatRange`: the two ends in the one pattern CLDR chose for the difference between them.
// The English range separator is an en dash with a thin space (U+2009) on each side of it, which is
// what CLDR's `dateTimeRange` pattern carries and what both engines therefore answer.
const RANGE = "\u2009–\u2009";
const range = new Intl.DateTimeFormat("en", { month: "short", day: "numeric", ...utc });
assert.sameValue(range.formatRange(new Date(Date.UTC(2026, 1, 18)), new Date(Date.UTC(2026, 1, 22))), "Feb 18" + RANGE + "22");
assert.sameValue(range.formatRange(new Date(Date.UTC(2026, 1, 18)), new Date(Date.UTC(2026, 1, 18))), "Feb 18", "two equal ends are one date");
assert.sameValue(range.formatRange(day, day) === range.format(day), true, "and that date is what format answers");

// 11.5.8 step 16 answers a range before any range pattern is looked at: two ends that are one
// instant are that instant, spelled once with every part shared, even for a field set this build has
// no range pattern for at all - which would otherwise say the same date twice.
const oneInstant = new Date(Date.UTC(2026, 1, 18, 9, 36, 32));
for (const bag of [{ minute: "numeric", second: "numeric" }, { hour: "numeric", minute: "2-digit", second: "2-digit" }]) {
  const once = new Intl.DateTimeFormat("en", { ...bag, ...utc });
  assert.sameValue(once.formatRange(oneInstant, oneInstant), once.format(oneInstant), "one instant is one side: " + JSON.stringify(bag));
  const sx = once.formatRangeToParts(oneInstant, oneInstant).map((part) => part.source);
  const sy = once.formatToParts(oneInstant).map(() => "shared");
  assert.compareArray(sx, sy, "lengths " + sx.length + "/" + sy.length + " " + JSON.stringify(bag));
}

// 11.5.8 step 15 walks Table 6 no further than the format renders: an afternoon that changes none of
// the year, the month and the day is one date for a format of those three, while the day the format
// does render splits the range in two. The rows above the rendered fields count as well, which the
// month and the day of two different years is what shows - the century is not rendered and is still
// asked about, because it sits above the month in the table.
const morning = new Date(Date.UTC(2026, 1, 18, 7, 33, 20));
const afternoon = new Date(Date.UTC(2026, 1, 18, 13, 3, 20));
const nextMorning = new Date(Date.UTC(2026, 1, 19, 7, 33, 20));
const yearLater = new Date(Date.UTC(2027, 1, 18, 7, 33, 20));
const fullDate = new Intl.DateTimeFormat("en", { year: "numeric", month: "short", day: "numeric", ...utc });
assert.sameValue(fullDate.formatRange(morning, afternoon), fullDate.format(morning), "the hour is not rendered, so an afternoon is one date");
const oneDateParts = fullDate.formatRangeToParts(morning, afternoon).map((part) => part.source);
const oneDateShared = ["shared", "shared", "shared", "shared", "shared"];
assert.compareArray(oneDateParts, oneDateShared, "and every part of it is shared: " + JSON.stringify(oneDateParts));
const twoDays = fullDate.formatRangeToParts(morning, nextMorning).map((part) => part.source);
assert.compareArray(twoDays, ["shared", "shared", "startRange", "shared", "endRange", "shared", "shared"], "the rendered day still splits it: " + JSON.stringify(twoDays));
assert.sameValue(fullDate.formatRange(morning, nextMorning), "Feb 18" + RANGE + "19, 2026", "and the day is what it splits");
const monthDay = new Intl.DateTimeFormat("en", { month: "short", day: "numeric", ...utc });
const twoYears = monthDay.formatRangeToParts(morning, yearLater).map((part) => part.source);
assert.sameValue(
  twoYears.includes("startRange") && twoYears.includes("endRange"),
  true,
  "the century above the month counts unrendered: " + JSON.stringify(twoYears)
);
assert.sameValue(new Intl.DateTimeFormat("da", { month: "short", day: "numeric", ...utc }).formatRange(new Date(Date.UTC(2026, 1, 18)), new Date(Date.UTC(2026, 1, 22))), "18.–22. feb.", "Danish ranges its own way, without the thin spaces");
const rangeParts = range.formatRangeToParts(new Date(Date.UTC(2026, 1, 18)), new Date(Date.UTC(2026, 1, 22)));
assert.sameValue(compareArray(rangeParts.map((part) => part.source), ["shared", "shared", "startRange", "shared", "endRange"]), true, "11.5.10's source of every record of a range");
assert.sameValue(compareArray(rangeParts.map((part) => part.value), ["Feb", " ", "18", RANGE, "22"]), true, "and the text each of them carries");
assert.sameValue(rangeParts.every((part) => typeof part.type === "string"), true, "with the type of 11.5.7 still there");
assert.throws(TypeError, () => Intl.DateTimeFormat.prototype.formatRange(new Date(), undefined), "11.3.4 step 3: both ends have to be there");
assert.sameValue(typeof range.formatRange(new Date(Date.UTC(2026, 1, 22)), new Date(Date.UTC(2026, 1, 18))), "string", "11.3.4 asks for no ordering of the two ends, so a reversed range is a range");
assert.sameValue(
  compareArray(
    range.formatRangeToParts(new Date(Date.UTC(2026, 1, 22)), new Date(Date.UTC(2026, 1, 18))).map((part) => part.value),
    ["Feb", " ", "22", RANGE, "18"],
  ),
  true,
  "and the reversed ends keep their places as startRange and endRange",
);
// 11.5.8 renders each end in the offset that end had, which is what a range across a saving boundary
// answers: the two hours are eleven and twelve in Copenhagen even though both ends are 10:00 UTC.
assert.sameValue(
  new Intl.DateTimeFormat("en", { hour: "numeric", minute: "2-digit", timeZone: "Europe/Copenhagen" }).formatRange(
    new Date(Date.UTC(2026, 2, 8, 10)),
    new Date(Date.UTC(2026, 6, 8, 10)),
  ),
  "11:00" + NNBSP + "AM" + RANGE + "12:00" + NNBSP + "PM",
  "each end with its own offset",
);
assert.throws(RangeError, () => range.formatRange(new Date(NaN), day), "an Invalid Date is a RangeError here");
assert.throws(TypeError, () => Intl.DateTimeFormat.prototype.formatRange.call({}, day, day), "and the this-value is checked first");

// 20.4.1 to 20.4.3: the three methods of `Date.prototype` are the constructor of 11.1 with the
// defaults of `ToDateTimeOptions`, and their no-argument form is the date and time of the host.
assert.sameValue(Date.prototype.toLocaleString.name, "toLocaleString");
assert.sameValue(Date.prototype.toLocaleString.length, 0);
assert.sameValue(Date.prototype.toLocaleDateString.name, "toLocaleDateString");
assert.sameValue(Date.prototype.toLocaleTimeString.name, "toLocaleTimeString");
assert.sameValue(day.toLocaleDateString("en-US", { year: "numeric", month: "short", day: "numeric", timeZone: "UTC" }), "Feb 18, 2026", "the shape pages ask for");
assert.sameValue(day.toLocaleDateString("en", { dateStyle: "short", timeZone: "UTC" }), "2/18/26", "an explicit dateStyle of the bag is honoured, unlike the default");
assert.sameValue(day.toLocaleDateString("da", { dateStyle: "long", timeZone: "UTC" }), "18. februar 2026");
assert.sameValue(day.toLocaleTimeString("it", { hour: "2-digit", minute: "2-digit", timeZone: "UTC" }), "09:36");
assert.sameValue(day.toLocaleDateString("en", { timeZone: "UTC" }), new Intl.DateTimeFormat("en", { ...utc }).format(day), "no options is the constructor's own default bag");
assert.sameValue(day.toLocaleTimeString("en", { timeZone: "UTC" }), "9:36:32" + NNBSP + "AM", "the time string of no options counts its seconds, which is the constructor's own time rather than a style");
assert.sameValue(day.toLocaleString("en", { timeZone: "UTC" }), "2/18/2026, 9:36:32" + NNBSP + "AM", "the plain string of no options is the date and the time of the host's default patterns");
assert.sameValue(day.toLocaleString("en", { timeZone: "UTC" }) === day.toLocaleDateString("en", { timeZone: "UTC" }) + ", " + day.toLocaleTimeString("en", { timeZone: "UTC" }), true, "and the three methods are the two groups of one bag");
assert.sameValue(day.toLocaleString("en", { ...utc, dateStyle: "medium", timeStyle: "short" }), "Feb 18, 2026, 9:36" + NNBSP + "AM");
assert.sameValue(new Date(NaN).toLocaleDateString("en"), "Invalid Date", "20.4.3 step 2: the Invalid Date is a string here, not an error");
assert.throws(RangeError, () => day.toLocaleDateString("en", { timeZone: "Nowhere/Nowhere" }), "and the zone is still 11.5.3's");
assert.throws(RangeError, () => day.toLocaleTimeString("en", { dateStyle: "nope" }), "as is the option list");

// 20.4.1 to 20.4.3 keep today's English output for a call with no arguments at all, which is what
// pages that never name a locale see.
const noArgument = new Date(Date.UTC(2026, 1, 18, 9, 36, 32, 180));
assert.sameValue(
  noArgument.toLocaleDateString({ dateStyle: undefined, timeZone: "UTC" }),
  new Intl.DateTimeFormat(undefined, { timeZone: "UTC" }).format(noArgument),
  "an empty bag of options is the constructor's default",
);
assert.sameValue(
  noArgument.toLocaleString("en", { timeZone: "UTC", calendar: "gregory", numberingSystem: "latn" }).length > 0,
  true,
  "the extension keys of the bag are read",
);
assert.sameValue(
  new Intl.DateTimeFormat("en", { timeZone: "UTC" }).format(day) === new Intl.DateTimeFormat("en-GB", { timeZone: "UTC" }).format(day),
  true,
  "a region of a supported language renders with that language's data",
);

// 11.3.3 step 3 and 11.5.6 step 1: `format` of no argument is the current instant, and the argument
// is a time value however it is spelled.
assert.sameValue(typeof en.format(), "string", "a bare format answers the current date");
assert.sameValue(new Intl.DateTimeFormat("en", { dateStyle: "short", ...utc }).format(day.getTime()), "2/18/26", "a number is a time value");
assert.sameValue(new Intl.DateTimeFormat("en", { dateStyle: "short", ...utc }).format(String(day.getTime())), "2/18/26", "and so is the string of one, through ToNumber");
assert.sameValue(new Intl.DateTimeFormat("en", { dateStyle: "short", ...utc }).format(null), "1/1/70", "null is the number 0 to ToNumber, which is the epoch");
assert.throws(RangeError, () => new Intl.DateTimeFormat("en", { dateStyle: "short", ...utc }).format("2026-02-18T00:00:00Z"), "a date string is NaN to ToNumber, and 11.5.6 does not parse it");
assert.throws(RangeError, () => new Intl.DateTimeFormat("en", { dateStyle: "short", ...utc }).format(Infinity), "and neither does an infinity, which TimeClip refuses");
assert.throws(RangeError, () => en.format(new Date(NaN)), "and an Invalid Date is 11.5.6's RangeError");
