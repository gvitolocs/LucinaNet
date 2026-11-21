/*---
description: Intl.ListFormat, ECMA-402 clause 14. 14.1.1 reads the options, 14.3 is what an instance
  does, 14.5 is the formatting, over the and, or and unit lists of the baked data.
---*/

// 14.1 and 14.2: the constructor itself. `length` is 0 because both parameters of 14.1.1's signature
// are optional, and `name` is the constructor's own.
assert.sameValue(typeof Intl.ListFormat, "function");
assert.sameValue(Intl.ListFormat.name, "ListFormat");
assert.sameValue(Intl.ListFormat.length, 0);
assert.sameValue(Object.getPrototypeOf(Intl.ListFormat), Function.prototype);
assert.sameValue(Intl.ListFormat.prototype.constructor, Intl.ListFormat);

// 14.2.1: `prototype` is a non-writable, non-enumerable, non-configurable data property.
const protoDesc = Object.getOwnPropertyDescriptor(Intl.ListFormat, "prototype");
assert.sameValue(protoDesc.writable, false);
assert.sameValue(protoDesc.enumerable, false);
assert.sameValue(protoDesc.configurable, false);
assert.sameValue(protoDesc.value, Intl.ListFormat.prototype);

const proto = Intl.ListFormat.prototype;
assert.sameValue(Object.getPrototypeOf(proto), Object.prototype);

// 14.3: the three methods of 14.3.2 to 14.3.4, with the length each signature has, and 14.3.5's tag.
assert.sameValue(typeof proto.resolvedOptions, "function");
assert.sameValue(proto.resolvedOptions.length, 0);
assert.sameValue(proto.resolvedOptions.name, "resolvedOptions");
assert.sameValue(typeof proto.format, "function");
assert.sameValue(proto.format.length, 1);
assert.sameValue(proto.format.name, "format");
assert.sameValue(typeof proto.formatToParts, "function");
assert.sameValue(proto.formatToParts.length, 1);
assert.sameValue(proto.formatToParts.name, "formatToParts");
assert.sameValue(proto[Symbol.toStringTag], "Intl.ListFormat");
assert.sameValue(Object.prototype.toString.call(new Intl.ListFormat("en")), "[object Intl.ListFormat]");

// 14.1.1 step 1: no NewTarget, no instance. Like PluralRules and unlike NumberFormat and Collator,
// this constructor has no legacy call mode, so there is no %Intl%.[[FallbackSymbol]] to call through
// and the error comes before any locale or option is read.
assert.throws(TypeError, () => Intl.ListFormat(["A", "B"]));
assert.throws(TypeError, () => Intl.ListFormat());
let readTarget = false;
assert.throws(TypeError, () =>
  Intl.ListFormat(["A"], { get type() { readTarget = true; return "unit"; } })
);
assert.sameValue(readTarget, false);

// 14.1.1 step 2's OrdinaryCreateFromConstructor, which reads `newTarget.prototype`.
class Subclass extends Intl.ListFormat {}
const sub = new Subclass("en");
assert(sub instanceof Subclass);
assert.sameValue(Object.getPrototypeOf(sub), Subclass.prototype);
assert.sameValue(sub.format(["A", "B"]), "A and B");
assert.sameValue(new Subclass("en", { type: "unit" }).format(["A", "B"]), "A, B");

// 14.3.2 step 2, 14.3.3 step 2 and 14.3.4 step 2's RequireInternalSlot, on a receiver that is not an
// instance or never got past 14.1.1 step 1.
assert.throws(TypeError, () => proto.format.call({}, ["A"]));
assert.throws(TypeError, () => proto.format.call(Object.create(proto), ["A"]));
assert.throws(TypeError, () => proto.formatToParts.call(Object.create(proto), ["A"]));
assert.throws(TypeError, () => proto.resolvedOptions.call(Object.create(proto)));
assert.throws(TypeError, () => proto.format.call(null, ["A"]));
assert.throws(TypeError, () => proto.format.call(undefined, ["A"]));

// 14.2.2, which is 9.2.9 over 14.2.3's [[AvailableLocales]] - the set this build was generated with -
// and its empty [[RelevantExtensionKeys]].
assert.sameValue(typeof Intl.ListFormat.supportedLocalesOf, "function");
assert.sameValue(Intl.ListFormat.supportedLocalesOf.length, 1);
assert.sameValue(Intl.ListFormat.supportedLocalesOf.name, "supportedLocalesOf");
assert.compareArray(
  Intl.ListFormat.supportedLocalesOf(["en-US", "da-DK", "xx-YY", "it-IT"]),
  ["en-US", "da-DK", "it-IT"]
);
assert.compareArray(Intl.ListFormat.supportedLocalesOf("da"), ["da"]);
assert.compareArray(Intl.ListFormat.supportedLocalesOf([]), []);
assert.compareArray(Intl.ListFormat.supportedLocalesOf(), []);
assert.compareArray(Intl.ListFormat.supportedLocalesOf(["da", "en", "it"]), ["da", "en", "it"]);
// 9.2.1 step 7 keeps the spelling the caller used, and two spellings of the same language are two
// entries of the list, so both come back.
assert.compareArray(
  Intl.ListFormat.supportedLocalesOf(["it-IT", "en", "it", "da-DK"]),
  ["it-IT", "en", "it", "da-DK"]
);
assert.compareArray(Intl.ListFormat.supportedLocalesOf(["en", "en-US", "en-GB"]), ["en", "en-US", "en-GB"]);
assert.throws(TypeError, () => Intl.ListFormat.supportedLocalesOf([1]));
assert.throws(RangeError, () => Intl.ListFormat.supportedLocalesOf(["not a locale!"]));
// FilterLocales step 1 wraps its second argument even when 14.1.1 step 3's GetOptionsObject would
// have refused it, and its steps 3 and 4 read `localeMatcher` before anything is filtered.
assert.compareArray(Intl.ListFormat.supportedLocalesOf(["en"], 5), ["en"]);
assert.compareArray(
  Intl.ListFormat.supportedLocalesOf(["en", "xx"], { localeMatcher: "lookup" }),
  ["en"]
);
assert.throws(RangeError, () => Intl.ListFormat.supportedLocalesOf(["en"], { localeMatcher: "bogus" }));

// 14.1.1 steps 5 and 6, over 9.2.7: the reported tag is the request with the script and region it
// named, while the patterns come from the available locale that matched ("da" for "da-DK", below);
// NumberFormat reports the same way, and a language this build has no data for falls back to the
// default one instead of throwing.
assert.sameValue(new Intl.ListFormat("en").resolvedOptions().locale, "en");
assert.sameValue(new Intl.ListFormat("da-DK").resolvedOptions().locale, "da-DK");
assert.sameValue(new Intl.ListFormat("it-IT").resolvedOptions().locale, "it-IT");
assert.sameValue(new Intl.ListFormat("en-AU").resolvedOptions().locale, "en-AU");
assert.sameValue(new Intl.ListFormat("xx-YY").resolvedOptions().locale, "en");
assert.sameValue(new Intl.ListFormat().resolvedOptions().locale, "en");
assert.sameValue(new Intl.ListFormat([]).resolvedOptions().locale, "en");
assert.sameValue(new Intl.ListFormat(["xx-YY", "it"]).resolvedOptions().locale, "it");
assert.throws(RangeError, () => new Intl.ListFormat("en$"));
// 14.2.3's [[RelevantExtensionKeys]] is empty, so a `-u-` extension is neither read as an option nor
// written back into the reported tag. NumberFormat, whose keys hold `nu`, does write its `-u-` back.
assert.sameValue(new Intl.ListFormat("en-u-ca-gregory").resolvedOptions().locale, "en");
assert.sameValue(new Intl.NumberFormat("en-u-nu-latn").resolvedOptions().locale, "en-u-nu-latn");
assert.sameValue(
  new Intl.ListFormat("it", { localeMatcher: "lookup" }).resolvedOptions().locale,
  "it"
);
assert.sameValue(
  new Intl.ListFormat("da", { localeMatcher: { toString: () => "lookup" } }).resolvedOptions().locale,
  "da"
);
assert.throws(RangeError, () => new Intl.ListFormat("en", { localeMatcher: "bogus" }));
assert.throws(RangeError, () => new Intl.ListFormat("en", { localeMatcher: 5 }));

// 14.1.1 steps 7 and 8: [[Type]].
for (const name of ["conjunction", "disjunction", "unit"]) {
  assert.sameValue(new Intl.ListFormat("en", { type: name }).resolvedOptions().type, name);
}
assert.sameValue(new Intl.ListFormat("en").resolvedOptions().type, "conjunction");
assert.sameValue(new Intl.ListFormat("en", { type: undefined }).resolvedOptions().type, "conjunction");
assert.sameValue(
  new Intl.ListFormat("en", { type: { toString: () => "unit" } }).resolvedOptions().type,
  "unit"
);
assert.throws(RangeError, () => new Intl.ListFormat("en", { type: "list" }));
assert.throws(RangeError, () => new Intl.ListFormat("en", { type: 5 }));
assert.throws(TypeError, () => new Intl.ListFormat("en", { type: Symbol("unit") }));

// 14.1.1 steps 9 and 10: [[Style]].
for (const style of ["long", "short", "narrow"]) {
  assert.sameValue(new Intl.ListFormat("en", { style }).resolvedOptions().style, style);
}
assert.sameValue(new Intl.ListFormat("en").resolvedOptions().style, "long");
assert.sameValue(new Intl.ListFormat("en", { style: undefined }).resolvedOptions().style, "long");
assert.sameValue(
  new Intl.ListFormat("en", { style: { toString: () => "narrow" } }).resolvedOptions().style,
  "narrow"
);
assert.throws(RangeError, () => new Intl.ListFormat("en", { style: "wide" }));
assert.throws(RangeError, () => new Intl.ListFormat("en", { style: false }));
assert.throws(RangeError, () => new Intl.ListFormat("en", { style: null }));
assert.throws(TypeError, () => new Intl.ListFormat("en", { style: Symbol("long") }));

// 9.2.8's two reads of the same bag: `GetOptionsObject` first, which is what makes a primitive options
// argument the TypeError it is rather than a bag with no properties, and then the options themselves.
assert.throws(TypeError, () => new Intl.ListFormat("en", 5));
assert.throws(TypeError, () => new Intl.ListFormat("en", "long"));

// 14.1.1's read order, which 9.2.8 step 1 starts and step 4 finishes: the locale list is filtered
// before any option is read, and the three options come in the order the clause lists them.
const read = [];
new Intl.ListFormat("en", {
  get localeMatcher() { read.push("localeMatcher"); return "best fit"; },
  get type() { read.push("type"); return "unit"; },
  get style() { read.push("style"); return "narrow"; },
});
assert.compareArray(read, ["localeMatcher", "type", "style"]);

const listRead = [];
const localesFirst = [];
Object.defineProperty(localesFirst, 0, {
  get() { listRead.push("locales"); return "en"; },
  enumerable: true,
  configurable: true,
});
new Intl.ListFormat(localesFirst, { get type() { listRead.push("type"); return "unit"; } });
assert.compareArray(listRead, ["locales", "type"]);

// 14.3.2 and Table 25: the three slots 14.1.1 resolves, in the order the table lists them, and
// nothing else - there is no numbering system to report, no digit option and no `pluralCategories`.
const resolved = new Intl.ListFormat("it", { type: "disjunction", style: "short" }).resolvedOptions();
assert.sameValue(Object.getPrototypeOf(resolved), Object.prototype);
assert.compareArray(Object.keys(resolved), ["locale", "type", "style"]);
assert.sameValue(resolved.locale, "it");
assert.sameValue(resolved.type, "disjunction");
assert.sameValue(resolved.style, "short");
assert.sameValue(resolved.minimumFractionDigits, undefined);
assert.sameValue(resolved.numberingSystem, undefined);
// A fresh object every call, and the instance's own options are what it reports.
assert(resolved !== new Intl.ListFormat("it", { type: "disjunction", style: "short" }).resolvedOptions());
assert.sameValue(new Intl.ListFormat("en").resolvedOptions().style, "long");
assert.sameValue(Intl.ListFormat.prototype.resolvedOptions.call(sub).locale, "en");

// 14.5.5 and 14.3.3: the argument is read as an iterable of Strings, and only of Strings. A primitive
// that is not a String is refused where an option would have been coerced.
const en = new Intl.ListFormat("en");
assert.sameValue(en.format(["A"]), "A");
assert.sameValue(en.format([]), "");
assert.sameValue(en.format(), "");
assert.sameValue(en.format(undefined), "");
assert.sameValue(en.format(new Set(["A", "B"])), "A and B");
assert.sameValue(en.format(new Map([["A", "x"], ["B", "y"]]).keys()), "A and B");
assert.sameValue(en.format("AB"), "A and B");
assert.throws(TypeError, () => en.format(5));
assert.throws(TypeError, () => en.format(null));
assert.throws(TypeError, () => en.format(true));
assert.throws(TypeError, () => en.format({}));
assert.throws(TypeError, () => en.format([1]));
assert.throws(TypeError, () => en.format([null]));
assert.throws(TypeError, () => en.format([undefined]));
assert.throws(TypeError, () => en.format([{}]));
assert.throws(TypeError, () => en.format([Symbol("A")]));
assert.throws(TypeError, () => en.format(["A", , "B"]));
assert.throws(TypeError, () => en.formatToParts([2]));
assert.sameValue(en.format(["A", ""]), "A and ");
assert.sameValue(en.format(["", "A"]), " and A");

// 14.5.5 step 4.3's error leaves through IteratorClose (ECMA-262 7.4.5), so a generator's `finally`
// runs before the method reports the TypeError.
const closed = [];
function* twoThenNumber() {
  try {
    yield "A";
    yield "B";
    yield 2;
  } finally {
    closed.push("finally");
  }
}
assert.throws(TypeError, () => en.format(twoThenNumber()));
assert.compareArray(closed, ["finally"]);
assert.throws(TypeError, () => en.formatToParts(twoThenNumber()));

// IteratorClose step 6 hands back the completion it was given, so the TypeError of step 4.3 is what
// leaves even though the iterator's own `return` runs first - the same answer V8 gives.
let returned = 0;
const threeStep = {
  index: 0,
  next() {
    this.index += 1;
    return this.index < 3 ? { value: "A", done: false } : { value: 2, done: false };
  },
  return() { returned += 1; return { value: undefined, done: true }; },
  [Symbol.iterator]() { return this; },
};
assert.throws(TypeError, () => en.format(threeStep));
assert.sameValue(returned, 1);
assert.throws(TypeError, () => en.formatToParts(threeStep));
assert.sameValue(returned, 2);

// 14.5.2 over the pair template: two places read Pair, and a longer list reads Start, Middle and End.
const conjunction = new Intl.ListFormat("en", { type: "conjunction", style: "long" });
assert.sameValue(conjunction.format(["A", "B"]), "A and B");
assert.sameValue(conjunction.format(["A", "B", "C"]), "A, B, and C");
assert.sameValue(conjunction.format(["A", "B", "C", "D"]), "A, B, C, and D");
const enShort = new Intl.ListFormat("en", { type: "conjunction", style: "short" });
assert.sameValue(enShort.format(["A", "B"]), "A & B");
assert.sameValue(enShort.format(["A", "B", "C"]), "A, B, & C");
const enNarrow = new Intl.ListFormat("en", { type: "conjunction", style: "narrow" });
assert.sameValue(enNarrow.format(["A", "B"]), "A, B");
assert.sameValue(enNarrow.format(["A", "B", "C", "D"]), "A, B, C, D");
const orLong = new Intl.ListFormat("en", { type: "disjunction", style: "long" });
assert.sameValue(orLong.format(["A", "B"]), "A or B");
assert.sameValue(orLong.format(["A", "B", "C"]), "A, B, or C");
const unitNarrow = new Intl.ListFormat("en", { type: "unit", style: "narrow" });
assert.sameValue(unitNarrow.format(["A", "B"]), "A B");
assert.sameValue(unitNarrow.format(["A", "B", "C"]), "A B C");

// 14.5.4 and 14.5.2: one record per part, `type` "element" for a place of the list and "literal" for
// the text between two places, in the order the templates give them.
const partsOfThree = conjunction.formatToParts(["A", "B", "C"]);
assert.sameValue(partsOfThree.length, 5);
assert.compareArray(partsOfThree.map(p => p.type), ["element", "literal", "element", "literal", "element"]);
assert.compareArray(partsOfThree.map(p => p.value), ["A", ", ", "B", ", and ", "C"]);
for (const part of partsOfThree) {
  assert.compareArray(Object.keys(part), ["type", "value"]);
}
assert.compareArray(conjunction.formatToParts(["A", "B"]).map(p => p.type), ["element", "literal", "element"]);
assert.compareArray(conjunction.formatToParts(["A", "B"]).map(p => p.value), ["A", " and ", "B"]);
assert.compareArray(conjunction.formatToParts(["A"]).map(p => p.type), ["element"]);
assert.sameValue(conjunction.formatToParts([]).length, 0);
assert.sameValue(conjunction.formatToParts().length, 0);
assert(Array.isArray(conjunction.formatToParts(["A"])));
assert.sameValue(Object.getPrototypeOf(conjunction.formatToParts(["A"])), Array.prototype);

// `format` is 14.5.3's concatenation of the very same parts, so the two cannot disagree.
for (const list of [["A"], ["A", "B"], ["A", "B", "C"], ["A", "B", "C", "D", "E"]]) {
  for (const type of ["conjunction", "disjunction", "unit"]) {
    for (const style of ["long", "short", "narrow"]) {
      const f = new Intl.ListFormat("en", { type, style });
      assert.sameValue(
        f.format(list),
        f.formatToParts(list).map(p => p.value).join(""),
        type + " " + style
      );
    }
  }
}

// The empty place of a list is a part of its own: DeconstructPattern step 3.4.1 appends a substitution
// whatever it holds, where PartitionPattern steps 2.2 and 6 drop a literal that came out empty. V8
// merges the literals around the empty place instead, so its parts are fewer; the text agrees.
const withHole = conjunction.formatToParts(["A", "", "B"]);
assert.compareArray(withHole.map(p => p.type), ["element", "literal", "element", "literal", "element"]);
assert.compareArray(withHole.map(p => p.value), ["A", ", ", "", ", and ", "B"]);
assert.sameValue(conjunction.format(["A", "", "B"]), "A, , and B");
assert.compareArray(
  conjunction.formatToParts(["", ""]).map(p => p.value),
  ["", " and ", ""]
);

// 14.2.3's LocaleData is per locale: Danish and Italian connect with their own words, and neither has
// the serial comma English has.
const da = new Intl.ListFormat("da", { type: "conjunction", style: "long" });
assert.sameValue(da.format(["A", "B"]), "A og B");
assert.sameValue(da.format(["A", "B", "C"]), "A, B og C");
assert.sameValue(da.format(["A", "B", "C", "D"]), "A, B, C og D");
// The region the request named stays in the reported tag while the patterns come from the available
// locale that matched, so "da-DK" connects like "da".
assert.sameValue(
  new Intl.ListFormat("da-DK", { type: "conjunction", style: "long" }).format(["A", "B", "C"]),
  "A, B og C"
);
assert.sameValue(new Intl.ListFormat("da", { type: "disjunction", style: "short" }).format(["A", "B"]), "A el. B");
assert.sameValue(
  new Intl.ListFormat("da", { type: "disjunction", style: "long" }).format(["A", "B", "C"]),
  "A, B eller C"
);
const it = new Intl.ListFormat("it", { type: "conjunction", style: "long" });
assert.sameValue(it.format(["A", "B"]), "A e B");
assert.sameValue(it.format(["A", "B", "C"]), "A, B e C");
assert.sameValue(new Intl.ListFormat("it", { type: "disjunction", style: "long" }).format(["A", "B"]), "A o B");
assert.sameValue(new Intl.ListFormat("it", { type: "unit", style: "narrow" }).format(["A", "B"]), "A B");
// Italian has no comma-only list: its `unit` set is the standard one, which is what V8 reads too.
assert.sameValue(new Intl.ListFormat("it", { type: "unit", style: "long" }).format(["A", "B"]), "A e B");
assert.sameValue(new Intl.ListFormat("it", { type: "conjunction", style: "narrow" }).format(["A", "B"]), "A e B");

// Italian's `e` becomes `ed` before a word that starts with a vowel: the template choice of 14.5.2
// step 3.1 and step 7.2 is the implementation-defined index the clause names, and the CLDR data this
// build baked has the rule.
assert.sameValue(it.format(["Andrea", "Anna", "Ivan"]), "Andrea, Anna e Ivan");
assert.sameValue(it.format(["Andrea", "ed", "Ivan"]), "Andrea, ed e Ivan");
assert.sameValue(
  new Intl.ListFormat("it", { type: "disjunction" }).format(["Andrea", "Anna", "Ivan"]),
  "Andrea, Anna o Ivan"
);

// An unsupported tag formats with the default locale's templates and says so.
const fallback = new Intl.ListFormat("xx-YY", { type: "unit", style: "short" });
assert.sameValue(fallback.resolvedOptions().locale, "en");
assert.sameValue(fallback.format(["A", "B"]), "A, B");

// Text is carried through untouched, including a lone surrogate, which is what makes a part's value
// the place's own String rather than anything read back through UTF-8.
assert.sameValue(en.format(["🎉", "a"]), "🎉 and a");
assert.sameValue(conjunction.formatToParts(["🎉", "a"])[0].value, "🎉");
const half = "\uD800";
assert.sameValue(conjunction.format([half, "B"]), half + " and B");
assert.sameValue(conjunction.formatToParts([half, "B"])[0].value, half);
assert.sameValue(conjunction.formatToParts([half, "B"])[0].value.length, 1);
assert.sameValue(conjunction.format(["\u2028", "B"]), "\u2028 and B");
