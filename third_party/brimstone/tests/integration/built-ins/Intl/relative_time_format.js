/*---
description: Intl.RelativeTimeFormat, ECMA-402 clause 18. 18.1.1 reads the options, 18.3 is what an
  instance does, 18.5 is the partition of a value and a unit over the past and future patterns of the
  baked data, with the numeric "auto" phrases read off the same records.
---*/

// 18.1 and 18.2: the constructor itself. `length` is 0 because both parameters of 18.1.1's signature
// are optional, and `name` is the constructor's own.
assert.sameValue(typeof Intl.RelativeTimeFormat, "function");
assert.sameValue(Intl.RelativeTimeFormat.name, "RelativeTimeFormat");
assert.sameValue(Intl.RelativeTimeFormat.length, 0);
assert.sameValue(Object.getPrototypeOf(Intl.RelativeTimeFormat), Function.prototype);
assert.sameValue(Intl.RelativeTimeFormat.prototype.constructor, Intl.RelativeTimeFormat);

// 18.2.1: `prototype` is a non-writable, non-enumerable, non-configurable data property.
const protoDesc = Object.getOwnPropertyDescriptor(Intl.RelativeTimeFormat, "prototype");
assert.sameValue(protoDesc.writable, false);
assert.sameValue(protoDesc.enumerable, false);
assert.sameValue(protoDesc.configurable, false);
assert.sameValue(protoDesc.value, Intl.RelativeTimeFormat.prototype);

const proto = Intl.RelativeTimeFormat.prototype;
assert.sameValue(Object.getPrototypeOf(proto), Object.prototype);

// 18.3: the three methods of 18.3.2 to 18.3.4, with the length each signature has - `format` and
// `formatToParts` both take the value and the unit - and 18.3.5's tag.
assert.sameValue(typeof proto.resolvedOptions, "function");
assert.sameValue(proto.resolvedOptions.length, 0);
assert.sameValue(proto.resolvedOptions.name, "resolvedOptions");
assert.sameValue(typeof proto.format, "function");
assert.sameValue(proto.format.length, 2);
assert.sameValue(proto.format.name, "format");
assert.sameValue(typeof proto.formatToParts, "function");
assert.sameValue(proto.formatToParts.length, 2);
assert.sameValue(proto.formatToParts.name, "formatToParts");
assert.sameValue(proto[Symbol.toStringTag], "Intl.RelativeTimeFormat");
const toStringTagDesc = Object.getOwnPropertyDescriptor(proto, Symbol.toStringTag);
assert.sameValue(toStringTagDesc.writable, false);
assert.sameValue(toStringTagDesc.enumerable, false);
assert.sameValue(toStringTagDesc.configurable, true);
assert.sameValue(
  Object.prototype.toString.call(new Intl.RelativeTimeFormat("en")),
  "[object Intl.RelativeTimeFormat]"
);

// 18.1.1 step 1: no NewTarget, no instance. Like ListFormat and unlike NumberFormat and Collator,
// this constructor has no legacy call mode, so there is no %Intl%.[[FallbackSymbol]] to call through
// and the error comes before any locale or option is read.
assert.throws(TypeError, () => Intl.RelativeTimeFormat("en"));
assert.throws(TypeError, () => Intl.RelativeTimeFormat());
let readTarget = false;
assert.throws(TypeError, () =>
  Intl.RelativeTimeFormat("en", { get style() { readTarget = true; return "narrow"; } })
);
assert.sameValue(readTarget, false);

// 18.1.1 step 2's OrdinaryCreateFromConstructor, which reads `newTarget.prototype`.
class Subclass extends Intl.RelativeTimeFormat {}
const sub = new Subclass("en");
assert(sub instanceof Subclass);
assert.sameValue(Object.getPrototypeOf(sub), Subclass.prototype);
assert.sameValue(sub.format(-3, "day"), "3 days ago");
assert.sameValue(new Subclass("en", { style: "short" }).format(-3, "day"), "3 days ago");
assert.sameValue(Object.getPrototypeOf(new Intl.RelativeTimeFormat("en")), proto);

// 18.3.2 step 2, 18.3.3 step 2 and 18.3.4 step 2's RequireInternalSlot, on a receiver that is not an
// instance or never got past 18.1.1 step 1.
assert.throws(TypeError, () => proto.format.call({}, -1));
assert.throws(TypeError, () => proto.format.call(Object.create(proto), -1));
assert.throws(TypeError, () => proto.formatToParts.call(Object.create(proto), -1));
assert.throws(TypeError, () => proto.resolvedOptions.call(Object.create(proto)));
assert.throws(TypeError, () => proto.format.call(null, -1));
assert.throws(TypeError, () => proto.format.call(undefined, -1));

// 18.2.2, which is 9.2.9 over 18.2.3's [[AvailableLocales]] and its single relevant extension key.
assert.sameValue(typeof Intl.RelativeTimeFormat.supportedLocalesOf, "function");
assert.sameValue(Intl.RelativeTimeFormat.supportedLocalesOf.length, 1);
assert.sameValue(Intl.RelativeTimeFormat.supportedLocalesOf.name, "supportedLocalesOf");
assert.compareArray(
  Intl.RelativeTimeFormat.supportedLocalesOf(["en-US", "da-DK", "xx-YY", "it-IT"]),
  ["en-US", "da-DK", "it-IT"]
);
assert.compareArray(Intl.RelativeTimeFormat.supportedLocalesOf("da"), ["da"]);
assert.compareArray(Intl.RelativeTimeFormat.supportedLocalesOf([]), []);
assert.compareArray(Intl.RelativeTimeFormat.supportedLocalesOf(), []);
assert.compareArray(
  Intl.RelativeTimeFormat.supportedLocalesOf(["it-IT", "en", "it", "da-DK"]),
  ["it-IT", "en", "it", "da-DK"]
);
assert.compareArray(
  Intl.RelativeTimeFormat.supportedLocalesOf(["en", "en-US", "en-GB"]),
  ["en", "en-US", "en-GB"]
);
assert.throws(TypeError, () => Intl.RelativeTimeFormat.supportedLocalesOf([1]));
assert.throws(RangeError, () => Intl.RelativeTimeFormat.supportedLocalesOf(["not a locale!"]));
// FilterLocales step 1 wraps its second argument even when 18.1.1 step 3's GetOptionsObject would
// have refused it, and its steps 3 and 4 read `localeMatcher` before anything is filtered.
assert.compareArray(Intl.RelativeTimeFormat.supportedLocalesOf(["en"], 5), ["en"]);
assert.compareArray(
  Intl.RelativeTimeFormat.supportedLocalesOf(["en", "xx"], { localeMatcher: "lookup" }),
  ["en"]
);
assert.throws(
  RangeError,
  () => Intl.RelativeTimeFormat.supportedLocalesOf(["en"], { localeMatcher: "bogus" })
);

// 18.1.1 steps 5 and 6, over 9.2.7: the reported tag is the request with the script and region it
// named, while the patterns come from the available locale that matched ("da" for "da-DK", below);
// a language this build has no data for falls back to the default one instead of throwing.
assert.sameValue(new Intl.RelativeTimeFormat("en").resolvedOptions().locale, "en");
assert.sameValue(new Intl.RelativeTimeFormat("da-DK").resolvedOptions().locale, "da-DK");
assert.sameValue(new Intl.RelativeTimeFormat("it-IT").resolvedOptions().locale, "it-IT");
assert.sameValue(new Intl.RelativeTimeFormat("en-AU").resolvedOptions().locale, "en-AU");
assert.sameValue(new Intl.RelativeTimeFormat("xx-YY").resolvedOptions().locale, "en");
assert.sameValue(new Intl.RelativeTimeFormat().resolvedOptions().locale, "en");
assert.sameValue(new Intl.RelativeTimeFormat([]).resolvedOptions().locale, "en");
assert.sameValue(new Intl.RelativeTimeFormat(["xx-YY", "it"]).resolvedOptions().locale, "it");
assert.sameValue(new Intl.RelativeTimeFormat("EN").resolvedOptions().locale, "en");
assert.sameValue(new Intl.RelativeTimeFormat(["en", "EN"]).resolvedOptions().locale, "en");
assert.sameValue(new Intl.RelativeTimeFormat(["EN", "en"]).resolvedOptions().locale, "en");
assert.throws(RangeError, () => new Intl.RelativeTimeFormat("en$"));
assert.throws(RangeError, () => new Intl.RelativeTimeFormat(["en-GB-oed"]));
assert.throws(RangeError, () => new Intl.RelativeTimeFormat(["x-private"]));
// 18.2.3's [[RelevantExtensionKeys]] holds "nu" with "latn" among its values, so a `-u-nu-latn`
// request is written back into the reported tag, the way NumberFormat reports it; an extension
// naming a key this service does not read is dropped.
assert.sameValue(new Intl.RelativeTimeFormat("en-u-nu-latn").resolvedOptions().locale, "en-u-nu-latn");
assert.sameValue(new Intl.RelativeTimeFormat("en-u-ca-gregory").resolvedOptions().locale, "en");
assert.sameValue(
  new Intl.RelativeTimeFormat("it", { localeMatcher: "lookup" }).resolvedOptions().locale,
  "it"
);
assert.sameValue(
  new Intl.RelativeTimeFormat("da", { localeMatcher: { toString: () => "lookup" } })
    .resolvedOptions()
    .locale,
  "da"
);

// 18.1.1 steps 14 and 15: [[Style]].
for (const style of ["long", "short", "narrow"]) {
  assert.sameValue(new Intl.RelativeTimeFormat("en", { style }).resolvedOptions().style, style);
}
assert.sameValue(new Intl.RelativeTimeFormat("en").resolvedOptions().style, "long");
assert.sameValue(new Intl.RelativeTimeFormat("en", { style: undefined }).resolvedOptions().style, "long");
assert.sameValue(
  new Intl.RelativeTimeFormat("en", { style: { toString: () => "narrow" } }).resolvedOptions().style,
  "narrow"
);
assert.throws(RangeError, () => new Intl.RelativeTimeFormat("en", { style: "wide" }));
assert.throws(RangeError, () => new Intl.RelativeTimeFormat("en", { style: false }));
assert.throws(RangeError, () => new Intl.RelativeTimeFormat("en", { style: null }));
assert.throws(TypeError, () => new Intl.RelativeTimeFormat("en", { style: Symbol("long") }));

// 18.1.1 steps 16 and 17: [[Numeric]].
for (const numeric of ["always", "auto"]) {
  assert.sameValue(new Intl.RelativeTimeFormat("en", { numeric }).resolvedOptions().numeric, numeric);
}
assert.sameValue(new Intl.RelativeTimeFormat("en").resolvedOptions().numeric, "always");
assert.sameValue(
  new Intl.RelativeTimeFormat("en", { numeric: { toString: () => "auto" } }).resolvedOptions().numeric,
  "auto"
);
assert.throws(RangeError, () => new Intl.RelativeTimeFormat("en", { numeric: "maybe" }));
assert.throws(RangeError, () => new Intl.RelativeTimeFormat("en", { numeric: 5 }));
assert.throws(TypeError, () => new Intl.RelativeTimeFormat("en", { numeric: Symbol("auto") }));

// 18.1.1 steps 6 to 8: `localeMatcher` and the one descriptor of 18.2.3, the key "nu" read from the
// `numberingSystem` property. Values this build does not support resolve to "latn", the one set of
// digits its data has, and a value that is not a type sequence is a RangeError.
assert.sameValue(new Intl.RelativeTimeFormat("en").resolvedOptions().numberingSystem, "latn");
assert.sameValue(
  new Intl.RelativeTimeFormat("en", { numberingSystem: "latn" }).resolvedOptions().numberingSystem,
  "latn"
);
assert.sameValue(
  new Intl.RelativeTimeFormat("en", { numberingSystem: "abc" }).resolvedOptions().numberingSystem,
  "latn"
);
assert.throws(RangeError, () => new Intl.RelativeTimeFormat("en", { numberingSystem: "" }));
assert.throws(RangeError, () => new Intl.RelativeTimeFormat("en", { numberingSystem: "a" }));
assert.throws(RangeError, () => new Intl.RelativeTimeFormat("en", { numberingSystem: "latn-ca" }));
assert.throws(RangeError, () => new Intl.RelativeTimeFormat("en", { numberingSystem: "!invalid!" }));

// 18.1.1 step 3's coerce-options reading: unlike ListFormat, a primitive options argument is wrapped
// into an object with no own properties rather than refused, and `null` is still a TypeError.
assert.sameValue(new Intl.RelativeTimeFormat("en", 5).resolvedOptions().numeric, "always");
assert.sameValue(new Intl.RelativeTimeFormat("en", true).resolvedOptions().style, "long");
assert.sameValue(new Intl.RelativeTimeFormat("en", "test").resolvedOptions().numeric, "always");
assert.throws(TypeError, () => new Intl.RelativeTimeFormat("en", null));

// 18.1.1's read order: the locale list is filtered before any option is read, and the four options
// come in the order the clause lists them.
const read = [];
new Intl.RelativeTimeFormat("en", {
  get localeMatcher() { read.push("localeMatcher"); return "best fit"; },
  get numberingSystem() { read.push("numberingSystem"); return "latn"; },
  get style() { read.push("style"); return "short"; },
  get numeric() { read.push("numeric"); return "auto"; },
});
assert.compareArray(read, ["localeMatcher", "numberingSystem", "style", "numeric"]);

const argRead = [];
const localesFirst = [];
Object.defineProperty(localesFirst, 0, {
  get() { argRead.push("locales"); return "en"; },
  enumerable: true,
  configurable: true,
});
new Intl.RelativeTimeFormat(localesFirst, { get style() { argRead.push("style"); return "short"; } });
assert.compareArray(argRead, ["locales", "style"]);

// 18.3.4 and Table 33: the four slots 18.1.1 resolves, in the order the table lists them, and
// nothing else - there is no pattern field to report and no plural categories.
const resolved = new Intl.RelativeTimeFormat("it", { style: "short", numeric: "auto" })
  .resolvedOptions();
assert.sameValue(Object.getPrototypeOf(resolved), Object.prototype);
assert.compareArray(Object.keys(resolved), ["locale", "style", "numeric", "numberingSystem"]);
assert.sameValue(resolved.locale, "it");
assert.sameValue(resolved.style, "short");
assert.sameValue(resolved.numeric, "auto");
assert.sameValue(resolved.numberingSystem, "latn");
// A fresh object every call, and the instance's own options are what it reports.
assert(
  resolved !==
    new Intl.RelativeTimeFormat("it", { style: "short", numeric: "auto" }).resolvedOptions()
);
assert.sameValue(new Intl.RelativeTimeFormat("en").resolvedOptions().style, "long");
assert.sameValue(Intl.RelativeTimeFormat.prototype.resolvedOptions.call(sub).locale, "en");

// 18.5.2 over 18.4's [[Patterns]]: every unit, always numeric, with the singular for one and the
// plural for everything else, the group marks of the decimal format inside, and -0 on the past side
// of step 8 while still printing as 0.
const units = ["second", "minute", "hour", "day", "week", "month", "quarter", "year"];
const en = new Intl.RelativeTimeFormat("en-US");
for (const unit of units) {
  assert.sameValue(en.format(1000, unit), `in 1,000 ${unit}s`, `always 1000 ${unit}`);
  assert.sameValue(en.format(10, unit), `in 10 ${unit}s`, `always 10 ${unit}`);
  assert.sameValue(en.format(2, unit), `in 2 ${unit}s`, `always 2 ${unit}`);
  assert.sameValue(en.format(1, unit), `in 1 ${unit}`, `always 1 ${unit}`);
  assert.sameValue(en.format(0, unit), `in 0 ${unit}s`, `always 0 ${unit}`);
  assert.sameValue(en.format(-0, unit), `0 ${unit}s ago`, `always -0 ${unit}`);
  assert.sameValue(en.format(-1, unit), `1 ${unit} ago`, `always -1 ${unit}`);
  assert.sameValue(en.format(-2, unit), `2 ${unit}s ago`, `always -2 ${unit}`);
  assert.sameValue(en.format(-10, unit), `10 ${unit}s ago`, `always -10 ${unit}`);
  assert.sameValue(en.format(-1000, unit), `1,000 ${unit}s ago`, `always -1000 ${unit}`);
}

// The page case: short day has the same words as long day in English.
assert.sameValue(new Intl.RelativeTimeFormat("en", { style: "short" }).format(-3, "day"), "3 days ago");

// 18.5.2 steps 6 and 10: the short tables name their own abbreviated units, with the plural forms
// qtr. has and day does not.
const unitsShort = {
  second: ["sec."],
  minute: ["min."],
  hour: ["hr."],
  day: ["day", "days"],
  week: ["wk."],
  month: ["mo."],
  quarter: ["qtr.", "qtrs."],
  year: ["yr."],
};
const enShort = new Intl.RelativeTimeFormat("en-US", { style: "short" });
for (const [unitArgument, unitStrings] of Object.entries(unitsShort)) {
  const [singular, plural = singular] = unitStrings;
  assert.sameValue(enShort.format(1000, unitArgument), `in 1,000 ${plural}`, `short 1000 ${unitArgument}`);
  assert.sameValue(enShort.format(1, unitArgument), `in 1 ${singular}`, `short 1 ${unitArgument}`);
  assert.sameValue(enShort.format(0, unitArgument), `in 0 ${plural}`, `short 0 ${unitArgument}`);
  assert.sameValue(enShort.format(-0, unitArgument), `0 ${plural} ago`, `short -0 ${unitArgument}`);
  assert.sameValue(enShort.format(-1, unitArgument), `1 ${singular} ago`, `short -1 ${unitArgument}`);
  assert.sameValue(enShort.format(-1000, unitArgument), `1,000 ${plural} ago`, `short -1000 ${unitArgument}`);
}

// The narrow tables drop the space of "1 hr. ago" to "1h ago".
const enNarrow = new Intl.RelativeTimeFormat("en", { style: "narrow" });
assert.sameValue(enNarrow.format(-3, "day"), "3d ago");
assert.sameValue(enNarrow.format(3, "day"), "in 3d");
assert.sameValue(enNarrow.format(-1, "hour"), "1h ago");
assert.sameValue(enNarrow.format(-3, "hour"), "3h ago");
assert.sameValue(enNarrow.format(1, "second"), "in 1s");

// 18.5.2 step 7: numeric "auto" reads the phrase table first, and -0 reads as 0, whose phrase each
// unit has. Only units whose table has no entry for 0 - hour and minute say "this", second says
// "now" - and the singular patterns above decide what 2 and 1000 say.
const autoExceptions = {
  year: { "-1": "last year", "0": "this year", "1": "next year" },
  quarter: { "-1": "last quarter", "0": "this quarter", "1": "next quarter" },
  month: { "-1": "last month", "0": "this month", "1": "next month" },
  week: { "-1": "last week", "0": "this week", "1": "next week" },
  day: { "-1": "yesterday", "0": "today", "1": "tomorrow" },
  hour: { "-1": "1 hour ago", "0": "this hour", "1": "in 1 hour" },
  minute: { "-1": "1 minute ago", "0": "this minute", "1": "in 1 minute" },
  second: { "-1": "1 second ago", "0": "now", "1": "in 1 second" },
};
const enAuto = new Intl.RelativeTimeFormat("en-US", { numeric: "auto" });
for (const unit of units) {
  assert.sameValue(enAuto.format(1, unit), autoExceptions[unit]["1"], `auto 1 ${unit}`);
  assert.sameValue(enAuto.format(0, unit), autoExceptions[unit]["0"], `auto 0 ${unit}`);
  assert.sameValue(enAuto.format(-0, unit), autoExceptions[unit]["0"], `auto -0 ${unit}`);
  assert.sameValue(enAuto.format(-1, unit), autoExceptions[unit]["-1"], `auto -1 ${unit}`);
  assert.sameValue(enAuto.format(1000, unit), `in 1,000 ${unit}s`, `auto 1000 ${unit}`);
  assert.sameValue(enAuto.format(-1000, unit), `1,000 ${unit}s ago`, `auto -1000 ${unit}`);
}

// 18.5.1: a plural unit name is accepted and folded to its singular, so "days" is "day".
assert.sameValue(en.format(-3, "days"), "3 days ago");
assert.sameValue(en.format(3, "seconds"), "in 3 seconds");
assert.sameValue(enShort.format(3, "quarters"), "in 3 qtrs.");

// 18.5.2 step 1: a value that is not finite is a RangeError, including the NaN that ToNumber
// produces for undefined, and step 3 runs before step 5, so a Symbol value is a TypeError even
// though the unit would have been read first by a naive reading.
assert.throws(RangeError, () => en.format(NaN, "day"));
assert.throws(RangeError, () => en.format(Infinity, "day"));
assert.throws(RangeError, () => en.format(-Infinity, "day"));
assert.throws(RangeError, () => en.format(undefined, "second"));
assert.throws(RangeError, () => en.format("string", "second"));
assert.throws(RangeError, () => en.format({}, "second"));
assert.throws(TypeError, () => en.format(Symbol("3"), "day"));

// 18.5.1: a unit that is not one of the eight singular names is a RangeError, whatever spelling or
// type it arrives as, and a Symbol is the TypeError of ToString before any list is consulted.
assert.throws(RangeError, () => en.format(0, undefined));
assert.throws(RangeError, () => en.format(0, null));
assert.throws(RangeError, () => en.format(0, true));
assert.throws(RangeError, () => en.format(0, 1));
assert.throws(RangeError, () => en.format(0, ""));
assert.throws(RangeError, () => en.format(0, "SECOND"));
assert.throws(RangeError, () => en.format(0, "fortnight"));
assert.throws(RangeError, () => en.format(0, "decades"));
assert.throws(TypeError, () => en.format(0, Symbol("day")));

// 18.5.2 step 3 through 8: the value is read with ToNumber, so the coercions of 18.3.3 apply, and
// the sign of the coerced value picks the tense - "-5" formats like -5.
assert.sameValue(en.format("3", "day"), "in 3 days");
assert.sameValue(en.format("  6  ", "second"), "in 6 seconds");
assert.sameValue(en.format("-5", "second"), "5 seconds ago");
assert.sameValue(en.format(null, "day"), "in 0 days");
assert.sameValue(en.format(true, "day"), "in 1 day");
assert.sameValue(en.format(false, "day"), "in 0 days");
assert.sameValue(en.format({ toString() { return 7; } }, "second"), "in 7 seconds");
assert.sameValue(en.format({ valueOf() { return 7; } }, "second"), "in 7 seconds");

// 18.5.3 over 18.5.2's list: one record per piece, `unit` on the pieces the number format cut out
// and none on the literals, in the order the pattern has them.
assert.compareArray(en.formatToParts(-3, "day").map(p => p.type), ["integer", "literal"]);
assert.compareArray(en.formatToParts(-3, "day").map(p => p.value), ["3", " days ago"]);
const pastParts = en.formatToParts(-3, "day");
assert.compareArray(Object.keys(pastParts[0]), ["type", "value", "unit"]);
assert.compareArray(Object.keys(pastParts[1]), ["type", "value"]);
assert.sameValue(pastParts[0].unit, "day");
assert.sameValue(pastParts[1].unit, undefined);
assert.compareArray(en.formatToParts(3, "day").map(p => p.value), ["in ", "3", " days"]);
assert.sameValue(en.formatToParts(3, "day")[1].unit, "day");
assert(Array.isArray(en.formatToParts(-3, "day")));
assert.sameValue(Object.getPrototypeOf(en.formatToParts(-3, "day")), Array.prototype);

// The group and decimal marks are parts of their own, and the unit rides on each of them.
assert.compareArray(en.formatToParts(-1234, "day").map(p => `${p.type}:${p.value}`), [
  "integer:1",
  "group:,",
  "integer:234",
  "literal: days ago",
]);
assert.compareArray(en.formatToParts(-1.5, "day").map(p => `${p.type}:${p.value}`), [
  "integer:1",
  "decimal:.",
  "fraction:5",
  "literal: days ago",
]);
assert.compareArray(en.formatToParts(123456.78, "day").map(p => `${p.type}:${p.value}`), [
  "literal:in ",
  "integer:123",
  "group:,",
  "integer:456",
  "decimal:.",
  "fraction:78",
  "literal: days",
]);

// Step 7's phrase answer is a single literal with no unit.
const yesterdayParts = enAuto.formatToParts(-1, "day");
assert.compareArray(yesterdayParts.map(p => p.type), ["literal"]);
assert.compareArray(yesterdayParts.map(p => p.value), ["yesterday"]);
assert.compareArray(Object.keys(yesterdayParts[0]), ["type", "value"]);

// `format` is 18.5.4's concatenation of the very same parts, so the two cannot disagree.
for (const unit of units) {
  for (const style of ["long", "short", "narrow"]) {
    for (const numeric of ["always", "auto"]) {
      const f = new Intl.RelativeTimeFormat("en", { style, numeric });
      for (const value of [0, 1, 2, -1, -2, 10, -10, 1000, -1000]) {
        assert.sameValue(
          f.format(value, unit),
          f.formatToParts(value, unit).map(p => p.value).join(""),
          `${numeric} ${style} ${value} ${unit}`
        );
      }
    }
  }
}

// 18.2.3's LocaleData is per locale: Danish puts the pattern's words around the number, Italian
// after it, and both have their own narrow and short tables.
const da = new Intl.RelativeTimeFormat("da");
assert.sameValue(da.format(-3, "day"), "for 3 dage siden");
assert.sameValue(da.format(3, "day"), "om 3 dage");
assert.sameValue(da.format(-3, "hour"), "for 3 timer siden");
assert.sameValue(da.format(3, "month"), "om 3 måneder");
assert.sameValue(new Intl.RelativeTimeFormat("da", { style: "short" }).format(-3, "day"), "3 dage siden");
assert.sameValue(new Intl.RelativeTimeFormat("da", { style: "narrow" }).format(-3, "day"), "3 dage siden");
// The region the request named stays in the reported tag while the patterns come from the available
// locale that matched, so "da-DK" answers like "da".
assert.sameValue(new Intl.RelativeTimeFormat("da-DK").format(-3, "day"), "for 3 dage siden");
assert.sameValue(new Intl.RelativeTimeFormat("da-DK").resolvedOptions().locale, "da-DK");

const daAuto = new Intl.RelativeTimeFormat("da", { numeric: "auto" });
assert.sameValue(daAuto.format(-1, "day"), "i går");
assert.sameValue(daAuto.format(0, "day"), "i dag");
assert.sameValue(daAuto.format(1, "day"), "i morgen");
assert.sameValue(daAuto.format(-0, "day"), "i dag");
assert.sameValue(daAuto.format(-1, "week"), "sidste uge");
assert.sameValue(daAuto.format(1, "week"), "næste uge");
assert.sameValue(daAuto.format(-1, "month"), "sidste måned");
assert.sameValue(daAuto.format(1, "month"), "næste måned");
assert.sameValue(daAuto.format(-1, "year"), "sidste år");
assert.sameValue(daAuto.format(1, "year"), "næste år");
assert.sameValue(daAuto.format(-1, "quarter"), "sidste kvartal");
assert.sameValue(daAuto.format(1, "quarter"), "næste kvartal");
assert.sameValue(daAuto.format(0, "second"), "nu");
assert.sameValue(daAuto.format(0, "minute"), "dette minut");
assert.sameValue(daAuto.format(0, "hour"), "denne time");
assert.sameValue(daAuto.format(1000, "day"), "om 1.000 dage");

const it = new Intl.RelativeTimeFormat("it");
assert.sameValue(it.format(-3, "day"), "3 giorni fa");
assert.sameValue(it.format(3, "day"), "tra 3 giorni");
assert.sameValue(it.format(-3, "hour"), "3 ore fa");
assert.sameValue(it.format(3, "year"), "tra 3 anni");
assert.sameValue(new Intl.RelativeTimeFormat("it", { style: "short" }).format(-3, "day"), "3 gg fa");
assert.sameValue(new Intl.RelativeTimeFormat("it", { style: "narrow" }).format(-3, "day"), "3 gg fa");
assert.sameValue(new Intl.RelativeTimeFormat("it-IT").format(-3, "day"), "3 giorni fa");
assert.sameValue(new Intl.RelativeTimeFormat("it-IT").resolvedOptions().locale, "it-IT");

const itAuto = new Intl.RelativeTimeFormat("it", { numeric: "auto" });
assert.sameValue(itAuto.format(-1, "day"), "ieri");
assert.sameValue(itAuto.format(0, "day"), "oggi");
assert.sameValue(itAuto.format(1, "day"), "domani");
assert.sameValue(itAuto.format(-0, "day"), "oggi");
assert.sameValue(itAuto.format(-1, "week"), "settimana scorsa");
assert.sameValue(itAuto.format(1, "week"), "settimana prossima");
assert.sameValue(itAuto.format(-1, "month"), "mese scorso");
assert.sameValue(itAuto.format(1, "month"), "mese prossimo");
assert.sameValue(itAuto.format(-1, "year"), "anno scorso");
assert.sameValue(itAuto.format(1, "year"), "anno prossimo");
assert.sameValue(itAuto.format(-1, "quarter"), "trimestre scorso");
assert.sameValue(itAuto.format(1, "quarter"), "trimestre prossimo");
assert.sameValue(itAuto.format(0, "second"), "ora");
assert.sameValue(itAuto.format(0, "minute"), "questo minuto");
assert.sameValue(itAuto.format(0, "hour"), "quest\u2019ora");

// The number inside is the decimal format of the locale: Danish marks its thousands with a dot,
// while Italian's minimum grouping of two digits leaves 1000 and 1234 unmarked.
assert.sameValue(da.format(1000, "day"), "om 1.000 dage");
assert.sameValue(it.format(1000, "day"), "tra 1000 giorni");
assert.sameValue(it.format(-1234, "day"), "1234 giorni fa");

// An unsupported tag formats with the default locale's patterns and says so.
const fallback = new Intl.RelativeTimeFormat("xx-YY", { style: "short" });
assert.sameValue(fallback.resolvedOptions().locale, "en");
assert.sameValue(fallback.format(-3, "day"), "3 days ago");

// The fractional value of steps 11 and 12 is rounded to the digits the two internal services share,
// so 0.5 prints and pluralizes as its rounded self, and the sign of the original picks the tense.
assert.sameValue(en.format(0.5, "second"), "in 0.5 seconds");
assert.sameValue(en.format(-0.5, "second"), "0.5 seconds ago");
assert.sameValue(en.format(0.9999, "day"), "in 1 day");
assert.sameValue(enAuto.format(1.5, "day"), "in 1.5 days");

// One instance answers many times over, and every answer is fresh.
assert.sameValue(en.format(-3, "day"), "3 days ago");
assert.sameValue(en.format(-3, "day"), "3 days ago");
assert(en.formatToParts(-3, "day") !== en.formatToParts(-3, "day"));
