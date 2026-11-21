// Copyright (C) 2026 LucinaJS contributors. All rights reserved.
// This code is governed by the BSD license that can be found in the LICENSE file.
/*---
description: Intl.Collator and String.prototype.localeCompare, which ECMA-402 clause 10 and clause
  20.1.1 of define. This build carries the collation data of English, Danish and Italian, and falls
  back to English for every other locale.
includes: [compareArray.js]
---*/

const en = new Intl.Collator("en");
const da = new Intl.Collator("da");

// 10.1 and 10.2: the constructor is a function of length 0, named "Collator", whose prototype is
// %Function.prototype%, and whose `prototype` property is frozen in place.
assert.sameValue(typeof Intl.Collator, "function");
assert.sameValue(Intl.Collator.length, 0);
assert.sameValue(Intl.Collator.name, "Collator");
assert.sameValue(Object.getPrototypeOf(Intl.Collator), Function.prototype);
assert.sameValue(Intl.Collator.prototype.constructor, Intl.Collator);

// 10.1.1: it works with and without `new`, because step 1 substitutes the active function for an
// absent NewTarget.
const withoutNew = Intl.Collator("en");
assert.sameValue(withoutNew instanceof Intl.Collator, true);
assert.sameValue(withoutNew.resolvedOptions().locale, "en");

// 10.1.1 step 1 uses the NewTarget, which a plain call leaves absent, and never the this value:
// test262's `Collator/this-value-ignored.js` hands it an instance of every other service
// constructor, which since NumberFormat is built means an instance of NumberFormat is one of them.
const foreign = new Intl.NumberFormat("en");
assert.notSameValue(Intl.Collator.call(foreign), foreign, "the this-value is ignored");

// 10.1.1 step 3 is OrdinaryCreateFromConstructor, so a subclass gets its own prototype.
class Shouter extends Intl.Collator {}
const shouter = new Shouter("da");
assert.sameValue(Object.getPrototypeOf(shouter), Shouter.prototype);
assert.sameValue(shouter instanceof Shouter, true);
assert.sameValue(shouter instanceof Intl.Collator, true);
assert.sameValue(shouter.resolvedOptions().locale, "da");

// 10.3.3: `compare` is an accessor on the prototype, not a method, and the function it returns is
// cached in [[BoundCompare]]: the same object every time, with length 2 and no name.
const compareDescriptor = Object.getOwnPropertyDescriptor(Intl.Collator.prototype, "compare");
assert.sameValue(typeof compareDescriptor.get, "function");
assert.sameValue(compareDescriptor.set, undefined);
assert.sameValue(compareDescriptor.enumerable, false);
assert.sameValue(compareDescriptor.configurable, true);

const compare = en.compare;
assert.sameValue(typeof compare, "function");
assert.sameValue(compare, en.compare, "the accessor caches the function it made");
assert.sameValue(compare.length, 2);
assert.sameValue(compare.name, "");
assert.sameValue(Object.prototype.toString.call(en), "[object Intl.Collator]");
assert.sameValue(Intl.Collator.prototype[Symbol.toStringTag], "Intl.Collator");

// The function is a closure over the collator rather than a bound method, so it keeps working
// detached and ignores whatever `this` it is called with (10.3.3 step 3.1).
assert.sameValue(["zebra", "apple"].sort(compare)[0], "apple");
assert.sameValue(compare.call(undefined, "b", "a"), 1);
assert.sameValue(compare.call(null, "a", "b"), -1);

// 10.3.2 and 10.3.3 refuse anything that is not an initialized collator.
assert.throws(TypeError, () => Intl.Collator.prototype.compare);
assert.throws(TypeError, () => Intl.Collator.prototype.resolvedOptions.call({}));
assert.throws(TypeError, () => en.resolvedOptions.call(Intl.Collator.prototype));

// 10.3.2: the resolved options, in the order Table 3 lists them, with the values 10.1.1 gives when
// the caller passes nothing at all.
const defaults = en.resolvedOptions();
assert.compareArray(Object.keys(defaults), [
  "locale",
  "usage",
  "sensitivity",
  "ignorePunctuation",
  "collation",
  "numeric",
  "caseFirst",
]);
assert.sameValue(defaults.locale, "en");
assert.sameValue(defaults.usage, "sort");
assert.sameValue(defaults.sensitivity, "variant");
assert.sameValue(defaults.ignorePunctuation, false);
assert.sameValue(defaults.collation, "default");
assert.sameValue(defaults.numeric, false);
assert.sameValue(defaults.caseFirst, "false");
assert.notSameValue(defaults, en.resolvedOptions(), "a fresh object every call");

// The language subtag alone decides the data, so a request narrows to the locale that has it (9.2.3)
// and a locale this build does not carry falls back to English without throwing (9.2.7 step 4).
assert.sameValue(new Intl.Collator("en-GB").resolvedOptions().locale, "en");
assert.sameValue(new Intl.Collator("da-DK").resolvedOptions().locale, "da");
assert.sameValue(new Intl.Collator("it-IT").resolvedOptions().locale, "it");
assert.sameValue(new Intl.Collator("de").resolvedOptions().locale, "en");
assert.sameValue(new Intl.Collator("zh").resolvedOptions().locale, "en");
assert.sameValue(new Intl.Collator(["da", "en"]).resolvedOptions().locale, "da");
assert.sameValue(new Intl.Collator(["zh", "it"]).resolvedOptions().locale, "it");

// 10.2.2: supportedLocalesOf keeps the requested spelling of the locales that matched, and drops
// the ones this build has no data for.
assert.compareArray(
  Intl.Collator.supportedLocalesOf(["da-DK", "de", "en-GB", "it", "zh"]),
  ["da-DK", "en-GB", "it"]
);
assert.compareArray(Intl.Collator.supportedLocalesOf("it"), ["it"]);
assert.compareArray(Intl.Collator.supportedLocalesOf([]), []);
assert.sameValue(Intl.Collator.supportedLocalesOf.length, 1);
assert.throws(RangeError, () => Intl.Collator.supportedLocalesOf(["not a tag"]));
assert.compareArray(Intl.Collator.supportedLocalesOf(7), [], "a number has no length");

// "root" names the base data the tailoring falls back to, not a locale, and so is refused as a tag
// even though four letters of it would pass RFC 5646 - the way V8 and SpiderMonkey refuse it.
assert.throws(RangeError, () => new Intl.Collator("root"));
assert.throws(RangeError, () => Intl.Collator.supportedLocalesOf("root"));

// 10.1.1 step 7: usage is either "sort" or "search". The comparison this build performs is the same
// for both, and step 8 reports what was asked.
const search = new Intl.Collator("en", { usage: "search" });
assert.sameValue(search.resolvedOptions().usage, "search");
assert.sameValue(search.compare("a", "A"), en.compare("a", "A"));
assert.throws(RangeError, () => new Intl.Collator("en", { usage: "compare" }));

// The options are read in the order of the steps, and an unknown value for one of the closed sets
// is a RangeError before anything else happens (9.2.11 steps 4-5).
assert.throws(RangeError, () => new Intl.Collator("en", { sensitivity: "x" }));
assert.throws(RangeError, () => new Intl.Collator("en", { localeMatcher: "nearest" }));
assert.throws(RangeError, () => new Intl.Collator("en", { caseFirst: "any" }));
assert.throws(TypeError, () => new Intl.Collator(["en", 5]));
assert.throws(RangeError, () => new Intl.Collator(["x"]));

// 10.3.3.1 Table 4, level by level: "base" ignores case and accents, "accent" only case, "case"
// only accents, and "variant" - the default - tells the two apart.
const base = new Intl.Collator("en", { sensitivity: "base" });
assert.sameValue(base.compare("a", "A"), 0);
assert.sameValue(base.compare("a", "á"), 0);
assert.sameValue(base.compare("a", "b"), -1);

const accent = new Intl.Collator("en", { sensitivity: "accent" });
assert.sameValue(accent.compare("a", "A"), 0);
assert.notSameValue(accent.compare("a", "á"), 0);

const caseOnly = new Intl.Collator("en", { sensitivity: "case" });
assert.sameValue(caseOnly.compare("a", "á"), 0);
assert.notSameValue(caseOnly.compare("a", "A"), 0);

assert.notSameValue(en.compare("a", "á"), 0);
assert.notSameValue(en.compare("a", "A"), 0);

// The sign, not just the tie: English is lower-case first, and a run of letters is a run of letters.
assert.compareArray(
  ["zebra", "Ápple", "Apple", "aardvark", "apple", "banana"].sort(en.compare),
  ["aardvark", "apple", "Apple", "Ápple", "banana", "zebra"]
);
assert.sameValue(en.compare("a", "a"), 0);
assert.sameValue(en.compare("", "a"), -1);

// 10.1.1 step 16: numeric ordering reads a run of digits as a number.
const numeric = new Intl.Collator("en", { numeric: true });
assert.sameValue(numeric.resolvedOptions().numeric, true);
assert.sameValue(numeric.compare("a2", "a10"), -1);
assert.sameValue(en.compare("a2", "a10"), 1);
assert.sameValue(numeric.compare("item-9", "item-10"), -1);

// 10.1.1 step 17: caseFirst says which case comes first when case decides anything at all.
const upper = new Intl.Collator("en", { caseFirst: "upper" });
const lower = new Intl.Collator("en", { caseFirst: "lower" });
assert.sameValue(upper.resolvedOptions().caseFirst, "upper");
assert.sameValue(upper.compare("A", "a"), -1);
assert.sameValue(lower.compare("A", "a"), 1);
assert.sameValue(upper.compare("a", "b"), -1);

// 10.1.1 step 22: ignoring punctuation takes it out of the comparison rather than ranking it.
const unpunctuated = new Intl.Collator("en", { ignorePunctuation: true });
assert.sameValue(unpunctuated.resolvedOptions().ignorePunctuation, true);
assert.sameValue(unpunctuated.compare("a.b", "ab"), 0);
assert.sameValue(unpunctuated.compare("a b", "ab"), 0);
assert.sameValue(unpunctuated.compare("a.b", "ac"), -1);
assert.sameValue(en.compare("a.b", "ab"), -1);

// The same three settings read out of the locale instead of the options object (9.2.7 step 12.6),
// and the keyword then stays on the reported tag - in the canonical spelling 9.2.1 step 7.3.6 gave it,
// which is Annex C's removal of a value of "true": test262's
// `Collator/prototype/resolvedOptions/resolved-numeric-unicode-extensions-and-options.js` asks for
// "en-u-kn" from a request that spelled "en-u-kn-true".
assert.sameValue(new Intl.Collator("en-u-kn-true").resolvedOptions().numeric, true);
assert.sameValue(new Intl.Collator("en-u-kn-true").resolvedOptions().locale, "en-u-kn");
assert.sameValue(new Intl.Collator("en-u-kn").resolvedOptions().numeric, true);
assert.sameValue(new Intl.Collator("en-u-kn").resolvedOptions().locale, "en-u-kn");
assert.sameValue(new Intl.Collator("en-u-kf-upper").resolvedOptions().caseFirst, "upper");
assert.sameValue(new Intl.Collator("en-u-kf-upper").compare("A", "a"), -1);

// A collation this build has no data for is refused by 9.2.7 step 12.6.3.1, which leaves the
// default collation and takes the keyword off the tag.
assert.sameValue(new Intl.Collator("en-u-co-phonebk").resolvedOptions().collation, "default");
assert.sameValue(new Intl.Collator("en-u-co-phonebk").resolvedOptions().locale, "en");

// The options beat the tag (step 12.11), and then the tag keyword is dropped as well.
const overruled = new Intl.Collator("en-u-kn-true", { numeric: false });
assert.sameValue(overruled.resolvedOptions().numeric, false);
assert.sameValue(overruled.resolvedOptions().locale, "en");

// 10.2.3: a keyword that is not one of the three a collator cares about changes nothing at all.
const distracted = new Intl.Collator("en-u-ka-shifted-nu-latn");
assert.sameValue(distracted.resolvedOptions().locale, "en");
assert.sameValue(distracted.compare("a", "b"), -1);

// The Danish tailoring moves æ, ø and å past z, in that order. It also expands the digraph "aa",
// which used to spell å and still spells it in names, to that å: it joins the å behind the z, so it
// comes out above the "ab" that root order would have put first. Italian has no such rule.
assert.compareArray(["å", "z", "ø", "æ"].sort(da.compare), ["z", "æ", "ø", "å"]);
assert.compareArray(["å", "aa", "ab"].sort(da.compare), ["ab", "å", "aa"]);
assert.sameValue(da.compare("aa", "ab"), 1);
assert.sameValue(da.compare("aa", "å"), 1);
assert.sameValue(da.compare("A", "a"), -1, "Danish is upper-case first by default");

// English leaves them where root puts them: å is an "a" with a mark on it, æ expands to "ae", and
// ø is an "o" with a mark, so all three stay in front of z.
assert.compareArray(["å", "z", "ø", "æ"].sort(en.compare), ["å", "æ", "ø", "z"]);
assert.sameValue(en.compare("A", "a"), 1);

// Italian has no tailoring of its own, so it orders what English orders.
const it = new Intl.Collator("it");
assert.sameValue(it.resolvedOptions().locale, "it");
assert.compareArray(["å", "z", "ø", "æ"].sort(it.compare), ["å", "æ", "ø", "z"]);
assert.sameValue(it.compare("a", "B"), -1);
assert.sameValue(it.compare("e", "é"), -1);

// 20.1.1: String.prototype.localeCompare is a three-parameter method that compares with a
// collator built from its last two arguments.
assert.sameValue(String.prototype.localeCompare.length, 1);
assert.sameValue(String.prototype.localeCompare.name, "localeCompare");
assert.sameValue("a".localeCompare("a"), 0);
assert.sameValue("a".localeCompare("b"), -1);
assert.sameValue("b".localeCompare("a"), 1);
assert.sameValue("a10".localeCompare("a2"), -1);
assert.sameValue("a2".localeCompare("a10", "en", { numeric: true }), -1);
assert.sameValue("a".localeCompare("A", "en", { sensitivity: "base" }), 0);
assert.sameValue("A".localeCompare("a", "da"), -1);
assert.sameValue("A".localeCompare("a", "en"), 1);

// 10.3.3.1: CompareStrings is required to honour canonical equivalence, and an answer of equal is
// +0 rather than a tie broken at some level - so the precomposed and the decomposed spelling of the
// same letter cannot be told apart here, at any sensitivity.
assert.sameValue("ä".localeCompare("a\u0308"), 0);
assert.sameValue("Å".localeCompare("A\u030A"), 0);
assert.sameValue("a\u0308".localeCompare("ä", "en", { sensitivity: "accent" }), 0);
assert.throws(TypeError, () => String.prototype.localeCompare.call(undefined, "a"));

// The step constructs %Intl.Collator%, the intrinsic, so replacing the property of `Intl` does not
// change how strings compare.
const saved = Intl.Collator;
Intl.Collator = undefined;
assert.sameValue("a".localeCompare("b"), -1);
Intl.Collator = saved;

// The same collator over and over, as an Array.prototype.sort comparator would use it.
const sorter = new Intl.Collator("en", { numeric: true });
assert.compareArray(["a10", "a2", "a1"].sort(sorter.compare), ["a1", "a2", "a10"]);

// 20.1.1 with no arguments resolves the default settings, and the context keeps the collator built
// from them, so a big sort through localeCompare costs comparisons and not collators - which is the
// shape the marketplace page runs at load. The values are a permutation of 0 to 9999, so every one
// of them is in the list exactly once.
const many = Array.from({ length: 10000 }, (_, i) => "item-" + ((i * 7919) % 10000));
many.sort((a, b) => a.localeCompare(b));
assert.sameValue(many.length, 10000);
assert.compareArray(many.slice(0, 3), ["item-0", "item-1", "item-10"]);
assert.sameValue(many[9999], "item-9999");
