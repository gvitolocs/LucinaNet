/*---
description: Intl.PluralRules, ECMA-402 clause 17. 17.1.1 reads the options, 17.3 is what an instance
  does, and 17.5 is the selection itself, over the cardinal, ordinal and ranges tables of the baked
  data. The three non-standard notations are refused rather than faked, as plural_rules_object.rs
  explains.
---*/

// 17.2: the constructor itself. `length` is 0 because both parameters of 17.1.1's signature are
// optional, and `name` is the constructor's own.
assert.sameValue(typeof Intl.PluralRules, "function");
assert.sameValue(Intl.PluralRules.name, "PluralRules");
assert.sameValue(Intl.PluralRules.length, 0);
assert.sameValue(Object.getPrototypeOf(Intl.PluralRules), Function.prototype);
assert.sameValue(Intl.PluralRules.prototype.constructor, Intl.PluralRules);

const proto = Intl.PluralRules.prototype;
assert.sameValue(Object.getPrototypeOf(proto), Object.prototype);

// 17.3: the three methods of 17.3.2 to 17.3.4, with the length each signature has, and 17.3.5's tag.
assert.sameValue(typeof proto.resolvedOptions, "function");
assert.sameValue(proto.resolvedOptions.length, 0);
assert.sameValue(proto.resolvedOptions.name, "resolvedOptions");
assert.sameValue(typeof proto.select, "function");
assert.sameValue(proto.select.length, 1);
assert.sameValue(proto.select.name, "select");
assert.sameValue(typeof proto.selectRange, "function");
assert.sameValue(proto.selectRange.length, 2);
assert.sameValue(proto.selectRange.name, "selectRange");
assert.sameValue(proto[Symbol.toStringTag], "Intl.PluralRules");
assert.sameValue(Object.prototype.toString.call(new Intl.PluralRules("en")), "[object Intl.PluralRules]");

// 17.2.1, which is 9.2.9 over 17.2.3's empty [[RelevantExtensionKeys]]. A locale is supported on the
// strength of its language subtag, so an extension the plural rules do not depend on still names a
// supported locale and is reported as asked for.
assert.compareArray(
  Intl.PluralRules.supportedLocalesOf(["en-US", "da-DK", "xx-YY", "it-IT-u-nu-latn"]),
  ["en-US", "da-DK", "it-IT-u-nu-latn"]
);
assert.compareArray(Intl.PluralRules.supportedLocalesOf("it"), ["it"]);
assert.compareArray(Intl.PluralRules.supportedLocalesOf([]), []);
assert.sameValue(Intl.PluralRules.supportedLocalesOf.length, 1);
assert.sameValue(Intl.PluralRules.supportedLocalesOf.name, "supportedLocalesOf");
assert.throws(TypeError, () => Intl.PluralRules.supportedLocalesOf([1]));
assert.throws(RangeError, () => Intl.PluralRules.supportedLocalesOf(["not a locale!"]));

// 17.1.1 step 1: no NewTarget, no instance. Unlike NumberFormat and Collator, this constructor has
// no legacy call mode and so no %Intl%.[[FallbackSymbol]] to call through.
assert.throws(TypeError, () => Intl.PluralRules("en"));
assert.throws(TypeError, () => Intl.PluralRules());

// 17.1.1 step 2's OrdinaryCreateFromConstructor, which reads `newTarget.prototype`.
class Subclass extends Intl.PluralRules {}
const sub = new Subclass("en");
assert(sub instanceof Subclass);
assert.sameValue(Object.getPrototypeOf(sub), Subclass.prototype);
assert.sameValue(sub.select(1), "one");

// 17.3.2 step 2, 17.3.3 step 2 and 17.3.4 step 2's RequireInternalSlot, on a receiver that never got
// past 17.1.1 step 1 or is not an instance at all.
assert.throws(TypeError, () => proto.select.call({}, 1));
assert.throws(TypeError, () => proto.select.call(Object.create(proto), 1));
assert.throws(TypeError, () => proto.selectRange.call(Object.create(proto), 1, 2));
assert.throws(TypeError, () => proto.resolvedOptions.call(Object.create(proto)));
assert.throws(TypeError, () => proto.select.call(null, 1));

// 17.1.1 steps 5 and 6, over 9.2.7: the reported tag is the request with the script and region it
// named, while the plural rules come from the available locale that matched ("da" for "da-DK");
// NumberFormat reports the same way, and an unsupported language falls back to the default one
// rather than throwing.
assert.sameValue(new Intl.PluralRules("en").resolvedOptions().locale, "en");
assert.sameValue(new Intl.PluralRules("da-DK").resolvedOptions().locale, "da-DK");
assert.sameValue(new Intl.PluralRules("it-IT").resolvedOptions().locale, "it-IT");
assert.sameValue(new Intl.PluralRules("en-AU").resolvedOptions().locale, "en-AU");
assert.sameValue(new Intl.PluralRules("xx-YY").resolvedOptions().locale, "en");
assert.sameValue(new Intl.PluralRules().resolvedOptions().locale, "en");
assert.sameValue(new Intl.PluralRules([]).resolvedOptions().locale, "en");
assert.sameValue(new Intl.PluralRules(["xx-YY", "it"]).resolvedOptions().locale, "it");
assert.throws(RangeError, () => new Intl.PluralRules("en$"));
// 17.2.3's empty [[RelevantExtensionKeys]] is why the `-u-` a request carries does not come back:
// 9.2.7 step 12.7 appends to the reported tag only the keys a service declares, and PluralRules
// declares none. NumberFormat, whose keys hold `nu`, does write its `-u-` back.
assert.sameValue(new Intl.PluralRules("en-u-nu-latn").resolvedOptions().locale, "en");
assert.sameValue(new Intl.NumberFormat("en-u-nu-latn").resolvedOptions().locale, "en-u-nu-latn");
// 9.2.8 step 4's matcher, of which "lookup" and "best fit" are the only two names.
assert.sameValue(new Intl.PluralRules("it", { localeMatcher: "lookup" }).resolvedOptions().locale, "it");
assert.sameValue(new Intl.PluralRules("en", { localeMatcher: { toString: () => "lookup" } }).resolvedOptions().locale, "en");
assert.throws(RangeError, () => new Intl.PluralRules("en", { localeMatcher: "bogus" }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { localeMatcher: 5 }));

// 17.1.1 steps 7 and 8: [[Type]], the one option of 17.1.1 that changes which rules are read.
assert.sameValue(new Intl.PluralRules("en", { type: "cardinal" }).resolvedOptions().type, "cardinal");
assert.sameValue(new Intl.PluralRules("en", { type: "ordinal" }).resolvedOptions().type, "ordinal");
assert.sameValue(new Intl.PluralRules("en", { type: undefined }).resolvedOptions().type, "cardinal");
assert.sameValue(new Intl.PluralRules("en", { type: { toString: () => "ordinal" } }).resolvedOptions().type, "ordinal");
assert.sameValue(new Intl.PluralRules("en", { type: "ordinal" }).select(3), "few");
assert.throws(RangeError, () => new Intl.PluralRules("en", { type: "plural" }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { type: 5 }));
assert.throws(TypeError, () => new Intl.PluralRules("en", { type: Symbol("ordinal") }));

// 17.1.1 steps 9 to 12: "standard" is the only notation this build pluralizes with, because the
// other three select from a compact or an exponent pattern the baked data has no entry for, and
// [[CompactDisplay]] stays undefined for it. Step 11 still reads `compactDisplay`, so a bad value of
// it still throws.
assert.throws(RangeError, () => new Intl.PluralRules("en", { notation: "compact" }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { notation: "scientific" }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { notation: "engineering" }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { notation: "boggling" }));
assert.sameValue(new Intl.PluralRules("en", { notation: "standard" }).select(1), "one");
assert.throws(RangeError, () => new Intl.PluralRules("en", { compactDisplay: "bogus" }));
assert.sameValue(new Intl.PluralRules("en", { compactDisplay: "long" }).resolvedOptions().compactDisplay, undefined);

// 17.1.1's read order, which 9.2.8 step 4 starts and 17.1.1 step 13's SetNumberFormatDigitOptions
// finishes: every option is read with `Get`, in the order the two clauses list them, and only the
// properties an object actually has are observed.
const read = [];
const probe = new Intl.PluralRules("en", {
  get localeMatcher() { read.push("localeMatcher"); return "best fit"; },
  get type() { read.push("type"); return "ordinal"; },
  get notation() { read.push("notation"); return "standard"; },
  get compactDisplay() { read.push("compactDisplay"); return "short"; },
  get minimumIntegerDigits() { read.push("minimumIntegerDigits"); return 2; },
  get minimumFractionDigits() { read.push("minimumFractionDigits"); return 1; },
  get maximumFractionDigits() { read.push("maximumFractionDigits"); return 2; },
});
assert.compareArray(read, [
  "localeMatcher", "type", "notation", "compactDisplay",
  "minimumIntegerDigits", "minimumFractionDigits", "maximumFractionDigits",
]);
assert.sameValue(probe.resolvedOptions().minimumIntegerDigits, 2);
assert.sameValue(probe.resolvedOptions().type, "ordinal");

// 17.5.1 and 17.5.2, English cardinal: the rule is `i = 1 and v = 0`, so a bare 1 is the only
// singular, and the operands are the digits the formatting shows rather than the argument's own.
const en = new Intl.PluralRules("en");
assert.sameValue(en.select(1), "one");
assert.sameValue(en.select(-1), "one");
assert.sameValue(en.select(1.0), "one");
assert.sameValue(en.select(2), "other");
assert.sameValue(en.select(0), "other");
assert.sameValue(en.select(11), "other");
assert.sameValue(en.select(21), "other");
assert.sameValue(en.select(1.5), "other");
assert.sameValue(en.select(0.5), "other");
assert.sameValue(en.select(1000000), "other");
// The three things a Number can be are "other" whatever the locale (17.5.2 steps 1 to 3), and so is
// the negative zero, which formats as "0".
assert.sameValue(en.select(NaN), "other");
assert.sameValue(en.select(Infinity), "other");
assert.sameValue(en.select(-Infinity), "other");
assert.sameValue(en.select(-0), "other");
assert.sameValue(en.select(), "other");
assert.sameValue(en.select(undefined), "other");
assert.sameValue(en.select(null), "other");
assert.sameValue(en.select(true), "one");
assert.sameValue(en.select({}), "other");
// 16.5.15 step 1 reads a String through the same grammar a literal goes through, so it is the
// mathematical value that pluralizes: "1.0" shows no fraction digit once rounded, and "007" is 7.
assert.sameValue(en.select("1"), "one");
assert.sameValue(en.select("1.0"), "one");
assert.sameValue(en.select("007"), "other");
assert.sameValue(en.select("1e3"), "other");
assert.sameValue(en.select("1,234"), "other");
// 16.5.15 step 2's BigInt, which keeps every digit it has.
assert.sameValue(en.select(1n), "one");
assert.sameValue(en.select(10n), "other");
assert.sameValue(en.select(1000000000000000000000n), "other");
// The same number through the function a NumberFormat's `format` getter would have handed back:
// PluralRules has no such getter, so `format` is absent.
assert.sameValue(en.format, undefined);

// 17.1.1 step 8's `ordinal`, which is a different table of the same locale.
const enOrdinal = new Intl.PluralRules("en", { type: "ordinal" });
assert.sameValue(enOrdinal.select(1), "one");
assert.sameValue(enOrdinal.select(2), "two");
assert.sameValue(enOrdinal.select(3), "few");
assert.sameValue(enOrdinal.select(4), "other");
assert.sameValue(enOrdinal.select(11), "other");
assert.sameValue(enOrdinal.select(12), "other");
assert.sameValue(enOrdinal.select(13), "other");
assert.sameValue(enOrdinal.select(21), "one");
assert.sameValue(enOrdinal.select(22), "two");
assert.sameValue(enOrdinal.select(23), "few");
assert.sameValue(enOrdinal.select(101), "one");
assert.sameValue(enOrdinal.select(103), "few");
assert.sameValue(enOrdinal.select(111), "other");
assert.sameValue(enOrdinal.select(113), "other");
assert.sameValue(enOrdinal.select(0), "other");
assert.sameValue(enOrdinal.select(NaN), "other");

// Danish cardinal answers `one` for a value whose shown digits are not all zero, which is the rule
// reading `t` rather than `i`, and Danish ordinal has no rule at all beyond the fallback.
const da = new Intl.PluralRules("da");
assert.sameValue(da.select(1), "one");
assert.sameValue(da.select(-1), "one");
assert.sameValue(da.select(1.5), "one");
assert.sameValue(da.select(1.01), "one");
assert.sameValue(da.select(0.5), "one");
assert.sameValue(da.select(1.234), "one");
assert.sameValue(da.select(0), "other");
assert.sameValue(da.select(2), "other");
assert.sameValue(da.select(11), "other");
assert.sameValue(da.select(100), "other");
assert.sameValue(da.select("1.0"), "one");

// A region the request adds changes the reported tag, not the rules that are read: "da-DK" selects
// on the "da" table, where English's own `one` needs a whole 1.
assert.sameValue(new Intl.PluralRules("da-DK").select(1.5), "one");
assert.sameValue(new Intl.PluralRules("da-DK").select(2), "other");

const daOrdinal = new Intl.PluralRules("da", { type: "ordinal" });
assert.sameValue(daOrdinal.select(1), "other");
assert.sameValue(daOrdinal.select(2), "other");
assert.sameValue(daOrdinal.select(3), "other");
assert.sameValue(daOrdinal.select(11), "other");
assert.sameValue(daOrdinal.select(100), "other");

// Italian cardinal has the `many` of a power of a million, which English has no name for at all.
const it = new Intl.PluralRules("it");
assert.sameValue(it.select(1), "one");
assert.sameValue(it.select(2), "other");
assert.sameValue(it.select(100), "other");
assert.sameValue(it.select(999999), "other");
assert.sameValue(it.select(1000000), "many");
assert.sameValue(it.select(2000000), "many");
assert.sameValue(it.select(1.5), "other");
assert.sameValue(it.select(0), "other");

const itOrdinal = new Intl.PluralRules("it", { type: "ordinal" });
assert.sameValue(itOrdinal.select(1), "other");
assert.sameValue(itOrdinal.select(2), "other");
assert.sameValue(itOrdinal.select(8), "many");
assert.sameValue(itOrdinal.select(10), "other");
assert.sameValue(itOrdinal.select(11), "many");
assert.sameValue(itOrdinal.select(15), "other");
assert.sameValue(itOrdinal.select(100), "other");

// 17.1.1 step 13 shares NumberFormat's digit options, so what an instance shows decides what it
// answers: 17.5.2 reads the operands off the formatted number, not off the argument.
assert.sameValue(new Intl.PluralRules("en", { minimumFractionDigits: 2 }).select(1), "other");
assert.sameValue(new Intl.PluralRules("en", { minimumFractionDigits: 2 }).select(1.5), "other");
assert.sameValue(
  new Intl.PluralRules("en", { minimumFractionDigits: 2, trailingZeroDisplay: "stripIfInteger" }).select(1),
  "one"
);
// Maximum fraction digits 0 rounds first, half-expand, which moves 1.4 onto `one` and 1.5 off it,
// and which makes the half of a one a one rather than the zero it is written as.
assert.sameValue(new Intl.PluralRules("en", { maximumFractionDigits: 0 }).select(1.4), "one");
assert.sameValue(new Intl.PluralRules("en", { maximumFractionDigits: 0 }).select(1.5), "other");
assert.sameValue(new Intl.PluralRules("en", { maximumFractionDigits: 0 }).select(0.5), "one");
assert.sameValue(
  new Intl.PluralRules("en", { maximumFractionDigits: 0, roundingMode: "ceil" }).select(0.5),
  "one"
);
// Leading zeros are not fraction digits, so `i` is still the value's integer part.
assert.sameValue(new Intl.PluralRules("en", { minimumIntegerDigits: 3 }).select(1), "one");
assert.sameValue(new Intl.PluralRules("en", { minimumIntegerDigits: 3 }).select(0), "other");
// Significant digits imply two shown fraction digits of a one, which is `v ≠ 0`.
assert.sameValue(new Intl.PluralRules("en", { minimumSignificantDigits: 3 }).select(1), "other");
assert.sameValue(new Intl.PluralRules("en", { minimumSignificantDigits: 3 }).select(1200), "other");
assert.sameValue(new Intl.PluralRules("da", { maximumFractionDigits: 1 }).select(1.25), "one");

// 16.1.2's own refusals, which the shared reading of step 13 inherits.
assert.throws(RangeError, () => new Intl.PluralRules("en", { minimumFractionDigits: -1 }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { maximumFractionDigits: 101 }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { minimumFractionDigits: 3, maximumFractionDigits: 2 }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { minimumSignificantDigits: 0 }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { maximumSignificantDigits: 22 }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { roundingIncrement: 3 }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { roundingMode: "halfUp" }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { roundingPriority: "bogus" }));
assert.throws(RangeError, () => new Intl.PluralRules("en", { trailingZeroDisplay: "strip" }));

// 17.3.4 and 17.5.4. A range whose two ends format alike is the category of its start (step 4),
// which is what makes `1 to 1` an answer Danish's rules give in the singular.
assert.sameValue(da.selectRange(1, 1), "one");
assert.sameValue(da.selectRange(-1, 1), "one");
assert.sameValue(da.selectRange(0, 1), "one");
assert.sameValue(da.selectRange(0, 0.5), "one");
assert.sameValue(da.selectRange(1, 2), "other");
assert.sameValue(da.selectRange(2, 3), "other");
assert.sameValue(da.selectRange(102, 201), "other");
assert.sameValue(da.selectRange(200, 200), "other");
assert.sameValue(da.selectRange(0, 0), "other");
// PluralRuleSelectRange (17.5.3) over the ranges table, which Italian answers for `0 to 1` too.
assert.sameValue(it.selectRange(0, 1), "one");
assert.sameValue(it.selectRange(1, 1), "one");
assert.sameValue(it.selectRange(102, 201), "other");
assert.sameValue(it.selectRange(0, 0), "other");
// English has an entry for `other to one` and it is `other`: a range of two different magnitudes is
// not the singular its end names. Only the range that is one number is, which is step 4's `1 to 1`
// and the `1` and `1.0` that round alike.
assert.sameValue(en.selectRange(1, 1), "one");
assert.sameValue(en.selectRange(0, 1), "other");
assert.sameValue(en.selectRange(200, 201), "other");
assert.sameValue(en.selectRange(102, 201), "other");
assert.sameValue(en.selectRange("1", "1.0"), "one");
assert.sameValue(en.selectRange(Infinity, Infinity), "other");
assert.sameValue(en.selectRange(-Infinity, 0), "other");
// 17.3.4 steps 3 and 4 refuse an absent end before anything is converted, and 17.5.4 step 1 refuses
// a not-a-number end once it has been converted.
assert.throws(TypeError, () => en.selectRange(undefined, 1));
assert.throws(TypeError, () => en.selectRange(1, undefined));
assert.throws(TypeError, () => en.selectRange());
assert.throws(RangeError, () => en.selectRange(NaN, 1));
assert.throws(RangeError, () => en.selectRange(1, NaN));
assert.throws(RangeError, () => en.selectRange("1,234", 2));
assert.throws(RangeError, () => en.selectRange(NaN, NaN));

// 17.3.2, Table 32 in the order the table lists the slots. A slot this build leaves undefined is
// absent rather than reported as `undefined`, and 17.2.3 gives PluralRules no [[Locale]]-driven
// digits, no style and no grouping, so none of those rows are filled.
const resolved = new Intl.PluralRules("en").resolvedOptions();
assert.sameValue(Object.getPrototypeOf(resolved), Object.prototype);
assert.sameValue(resolved.locale, "en");
assert.sameValue(resolved.type, "cardinal");
assert.sameValue(resolved.notation, "standard");
assert.sameValue(resolved.minimumIntegerDigits, 1);
assert.sameValue(resolved.minimumFractionDigits, 0);
assert.sameValue(resolved.maximumFractionDigits, 3);
assert.sameValue(resolved.minimumSignificantDigits, undefined);
assert.sameValue(resolved.maximumSignificantDigits, undefined);
assert.sameValue(resolved.roundingIncrement, 1);
assert.sameValue(resolved.roundingMode, "halfExpand");
assert.sameValue(resolved.roundingPriority, "auto");
assert.sameValue(resolved.trailingZeroDisplay, "auto");
assert.sameValue(resolved.style, undefined);
assert.sameValue(resolved.useGrouping, undefined);
assert.sameValue(resolved.numberingSystem, undefined);
assert.sameValue(resolved.currency, undefined);
assert.sameValue(resolved.signDisplay, undefined);
assert.compareArray(Object.keys(resolved), [
  "locale", "type", "notation", "minimumIntegerDigits", "minimumFractionDigits",
  "maximumFractionDigits", "pluralCategories", "roundingIncrement", "roundingMode",
  "roundingPriority", "trailingZeroDisplay",
]);
// A fresh object every call, and the digits an instance was built with are what it reports.
assert(resolved !== new Intl.PluralRules("en").resolvedOptions());
assert.compareArray(new Intl.PluralRules("en", { minimumSignificantDigits: 3 }).resolvedOptions().pluralCategories, ["one", "other"]);
assert.sameValue(new Intl.PluralRules("en", { minimumSignificantDigits: 3 }).resolvedOptions().minimumSignificantDigits, 3);
assert.sameValue(new Intl.PluralRules("en", { minimumSignificantDigits: 3 }).resolvedOptions().maximumSignificantDigits, 21);
assert.compareArray(Object.keys(new Intl.PluralRules("en", { minimumSignificantDigits: 3 }).resolvedOptions()), [
  "locale", "type", "notation", "minimumIntegerDigits",
  "minimumSignificantDigits", "maximumSignificantDigits", "pluralCategories",
  "roundingIncrement", "roundingMode", "roundingPriority", "trailingZeroDisplay",
]);

// 17.3.2 step 4, which is 17.5.1's list of the categories of UTS 35 order. The list is never empty
// and always ends with the fallback, and it is the categories of the locale's own rules, so Danish
// ordinal answers with nothing but "other".
assert(Array.isArray(resolved.pluralCategories));
assert.compareArray(resolved.pluralCategories, ["one", "other"]);
assert.compareArray(new Intl.PluralRules("en", { type: "ordinal" }).resolvedOptions().pluralCategories, ["one", "two", "few", "other"]);
assert.compareArray(new Intl.PluralRules("da").resolvedOptions().pluralCategories, ["one", "other"]);
assert.compareArray(new Intl.PluralRules("da", { type: "ordinal" }).resolvedOptions().pluralCategories, ["other"]);
assert.compareArray(new Intl.PluralRules("it").resolvedOptions().pluralCategories, ["one", "many", "other"]);
assert.compareArray(new Intl.PluralRules("it", { type: "ordinal" }).resolvedOptions().pluralCategories, ["many", "other"]);
// An unsupported tag pluralizes with the default locale's rules, and says so.
const fallback = new Intl.PluralRules("xx-YY").resolvedOptions();
assert.sameValue(fallback.locale, "en");
assert.compareArray(fallback.pluralCategories, ["one", "other"]);
