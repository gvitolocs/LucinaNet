/*---
description: Intl.getCanonicalLocales, the one function of ECMA-402 section 8.3, following
  CanonicalizeLocaleList (9.2.1) and CanonicalizeUnicodeLocaleId (6.2.2).
---*/

// Step 1: no argument, or an undefined one, is the empty list.
assert.compareArray(Intl.getCanonicalLocales(), []);
assert.compareArray(Intl.getCanonicalLocales(undefined), []);

// A single string is the one-element list it stands for, and a tag is canonicalized rather than
// echoed back: the subtags get their canonical casing.
assert.compareArray(Intl.getCanonicalLocales("en"), ["en"]);
assert.compareArray(Intl.getCanonicalLocales("EN"), ["en"]);
assert.compareArray(Intl.getCanonicalLocales("en-gb"), ["en-GB"]);
assert.compareArray(Intl.getCanonicalLocales("EN-us-u-hc-H23"), ["en-US-u-hc-h23"]);
assert.compareArray(Intl.getCanonicalLocales("da-DK"), ["da-DK"]);
assert.compareArray(Intl.getCanonicalLocales("it-it"), ["it-IT"]);

// The unknown locale is a tag of its own, and a private use sequence is kept.
assert.compareArray(Intl.getCanonicalLocales("und"), ["und"]);
assert.compareArray(Intl.getCanonicalLocales("en-x-icu"), ["en-x-icu"]);

// CLDR aliases are replaced, and the region alias of a language alone is not filled back in.
assert.compareArray(Intl.getCanonicalLocales("TL"), ["fil"]);

// A Unicode locale extension has its keys sorted and its repeated keys reduced to the first value,
// so that two requests that mean the same thing canonicalize to the same tag.
assert.compareArray(Intl.getCanonicalLocales("it-IT-u-nu-latn"), ["it-IT-u-nu-latn"]);
assert.compareArray(Intl.getCanonicalLocales("en-u-hc-h23"), ["en-u-hc-h23"]);
assert.compareArray(Intl.getCanonicalLocales("en-u-ca-gregory-nu-latn"), [
  "en-u-ca-gregory-nu-latn",
]);
assert.compareArray(Intl.getCanonicalLocales("en-u-nu-latn-ca-gregory"), [
  "en-u-ca-gregory-nu-latn",
]);
assert.compareArray(Intl.getCanonicalLocales("en-u-nu-latn-hc-h23-nu-digit"), [
  "en-u-hc-h23-nu-latn",
]);
assert.compareArray(
  Intl.getCanonicalLocales(["en-u-nu-latn-ca-gregory", "en-u-ca-gregory-nu-latn"]),
  ["en-u-ca-gregory-nu-latn"]
);

// An array is read element by element, in order, with duplicates of the same canonical tag dropped.
assert.compareArray(Intl.getCanonicalLocales(["en", "it", "da"]), ["en", "it", "da"]);
assert.compareArray(Intl.getCanonicalLocales(["en", "EN", "en-US"]), ["en", "en-US"]);
assert.compareArray(Intl.getCanonicalLocales(["en-GB", "en"]), ["en-GB", "en"]);

// Steps 7.1-7.2: the list is read by index, so an array-like object works and a hole in an array is
// not a request at all.
assert.compareArray(Intl.getCanonicalLocales({ length: 2, 0: "en", 1: "it" }), ["en", "it"]);
assert.compareArray(Intl.getCanonicalLocales([, "en"]), ["en"]);
assert.compareArray(Intl.getCanonicalLocales({ length: 0 }), []);

// Step 4 takes anything that is not a string through ToObject, which is why a number reads as an
// object with no length rather than as the tag "7".
assert.compareArray(Intl.getCanonicalLocales(7), []);
assert.compareArray(Intl.getCanonicalLocales(true), []);

// Step 7.3.2: an element has to be a string or an object. The note to 9.2.1 gives the reason for
// the rule with the number NaN, which would otherwise stringify to "nan" and name Min Nan Chinese.
assert.throws(TypeError, () => Intl.getCanonicalLocales([7]));
assert.throws(TypeError, () => Intl.getCanonicalLocales([NaN]));
assert.throws(TypeError, () => Intl.getCanonicalLocales([null]));
assert.throws(TypeError, () => Intl.getCanonicalLocales([undefined]));
assert.throws(TypeError, () => Intl.getCanonicalLocales(["en", 7]));
assert.throws(TypeError, () => Intl.getCanonicalLocales([Symbol("en")]));
assert.throws(TypeError, () => Intl.getCanonicalLocales(null));

// Step 7.3.4: an object that is not an Intl.Locale is stringified, so its own conversion is what is
// read as the tag.
assert.compareArray(Intl.getCanonicalLocales([{ toString: () => "en-gb" }]), ["en-GB"]);
assert.throws(RangeError, () => Intl.getCanonicalLocales([{ toString: () => "en_US" }]));

// Steps 7.3.5-7.3.6: a tag that is not well-formed is a RangeError, and it is reported before any
// later element is read.
assert.throws(RangeError, () => Intl.getCanonicalLocales(""));
assert.throws(RangeError, () => Intl.getCanonicalLocales("en_US"));
assert.throws(RangeError, () => Intl.getCanonicalLocales("en-US-"));
assert.throws(RangeError, () => Intl.getCanonicalLocales("i-navajo"));
assert.throws(RangeError, () => Intl.getCanonicalLocales(["en", ""]));
assert.throws(RangeError, () => Intl.getCanonicalLocales(["en", "de", "en_US", "it"]));

// A locale this build has no data for is still a well-formed tag, so it is canonicalized and
// returned rather than rejected; which locales are supported is a question for
// Intl.supportedValuesOf and for the constructors, neither of which is built yet.
assert.compareArray(Intl.getCanonicalLocales("de"), ["de"]);
assert.compareArray(Intl.getCanonicalLocales("zh-Hant-TW"), ["zh-Hant-TW"]);
