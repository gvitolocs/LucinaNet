/*---
description: Intl.DisplayNames, ECMA-402 clause 12. 12.1.1 reads the six `type`s and the three
  options, 12.3 is what an instance does, 12.5.1 is the shape a code has to have before the data is
  asked about it, and the data of this build answers language, region, script and currency out of the
  three `multi::*` display name formatters and nothing else - calendar and dateTimeField names are
  not baked, so those two types return 12.3.3 step 7's code.
---*/

// 12.1 and 12.2: the constructor itself. `length` is 2 because 12.1.1's signature names locales and
// options, and `name` is the constructor's own.
assert.sameValue(typeof Intl.DisplayNames, "function");
assert.sameValue(Intl.DisplayNames.name, "DisplayNames");
assert.sameValue(Intl.DisplayNames.length, 2);
assert.sameValue(Object.getPrototypeOf(Intl.DisplayNames), Function.prototype);
assert.sameValue(Intl.DisplayNames.prototype.constructor, Intl.DisplayNames);

// 12.2.1: `prototype` is a non-writable, non-enumerable, non-configurable data property.
const prototypeDescriptor = Object.getOwnPropertyDescriptor(Intl.DisplayNames, "prototype");
assert.sameValue(prototypeDescriptor.writable, false);
assert.sameValue(prototypeDescriptor.enumerable, false);
assert.sameValue(prototypeDescriptor.configurable, false);

const proto = Intl.DisplayNames.prototype;
assert.sameValue(Object.getPrototypeOf(proto), Object.prototype);

// 12.3.3 and 12.3.2, the two methods of the prototype, with the length each signature has, and
// 12.3.4's tag. Nothing else is on it.
assert.sameValue(Object.getOwnPropertyNames(proto).sort().join(","), "constructor,of,resolvedOptions");
assert.sameValue(typeof proto.of, "function");
assert.sameValue(proto.of.length, 1);
assert.sameValue(proto.of.name, "of");
assert.sameValue(typeof proto.resolvedOptions, "function");
assert.sameValue(proto.resolvedOptions.length, 0);
assert.sameValue(proto.resolvedOptions.name, "resolvedOptions");
assert.sameValue(proto[Symbol.toStringTag], "Intl.DisplayNames");
const tagDescriptor = Object.getOwnPropertyDescriptor(proto, Symbol.toStringTag);
assert.sameValue(tagDescriptor.writable, false);
assert.sameValue(tagDescriptor.enumerable, false);
assert.sameValue(tagDescriptor.configurable, true);
assert.sameValue(
  Object.prototype.toString.call(new Intl.DisplayNames("en", { type: "region" })),
  "[object Intl.DisplayNames]"
);

// 12.2.2's `supportedLocalesOf` on the constructor, which 9.1's shared behaviour gives the same
// attributes as every other service constructor's.
const supportedLocalesOfDescriptor = Object.getOwnPropertyDescriptor(Intl.DisplayNames, "supportedLocalesOf");
assert.sameValue(typeof supportedLocalesOfDescriptor.value, "function");
assert.sameValue(supportedLocalesOfDescriptor.value.name, "supportedLocalesOf");
assert.sameValue(supportedLocalesOfDescriptor.value.length, 1);
assert.sameValue(supportedLocalesOfDescriptor.writable, true);
assert.sameValue(supportedLocalesOfDescriptor.enumerable, false);
assert.sameValue(supportedLocalesOfDescriptor.configurable, true);

// 12.1.1 step 1: no NewTarget, no instance, and the error comes before locales or options are read.
assert.throws(TypeError, () => Intl.DisplayNames("en", { type: "region" }));
assert.throws(TypeError, () => Intl.DisplayNames());
let readType = false;
assert.throws(TypeError, () =>
  Intl.DisplayNames("en", { get type() { readType = true; return "region"; } })
);
assert.sameValue(readType, false);

// 12.1.1 step 2's OrdinaryCreateFromConstructor, which reads `newTarget.prototype`.
class Subclass extends Intl.DisplayNames {}
const subclassed = new Subclass("en", { type: "region" });
assert.sameValue(Object.getPrototypeOf(subclassed), Subclass.prototype);
assert.sameValue(subclassed instanceof Subclass, true);
assert.sameValue(subclassed.of("DK"), "Denmark");
assert.sameValue(Object.getPrototypeOf(new Intl.DisplayNames("en", { type: "region" })), proto);

// 12.3.2 step 2 and 12.3.3 step 2's RequireInternalSlot, on a receiver that is not an instance or
// never got past the constructor.
assert.throws(TypeError, () => proto.of.call({}, "DK"));
assert.throws(TypeError, () => proto.of.call(Object.create(proto), "DK"));
assert.throws(TypeError, () => proto.resolvedOptions.call(Object.create(proto)));
assert.throws(TypeError, () => proto.of.call(Intl.NumberFormat(), "DK"));
assert.throws(TypeError, () => proto.resolvedOptions.call(Intl.NumberFormat()));

// 12.1.1 step 3's `require-options`: DisplayNames is the only service built so far that has no
// defaults for all of its options, so an absent or non-object options argument is a TypeError, and
// `type` has no default at all (12.1.1 step 8's `required`).
assert.throws(TypeError, () => new Intl.DisplayNames("en"));
assert.throws(TypeError, () => new Intl.DisplayNames("en", null));
assert.throws(TypeError, () => new Intl.DisplayNames("en", undefined));
assert.throws(TypeError, () => new Intl.DisplayNames("en", {}));
assert.throws(TypeError, () => new Intl.DisplayNames("en", 7));

// Step 8's six allowed values, each of which is read as the name of a table rather than as a locale.
for (const type of ["language", "region", "script", "currency", "calendar", "dateTimeField"]) {
  assert.sameValue(new Intl.DisplayNames("en", { type }).resolvedOptions().type, type);
}

// Step 8 again, and the same for the other three: a value outside the allowed list is not clamped to
// the default but refused. `type` is case-sensitive, so "Language" is not one of the six names.
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "Language" }));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "languages" }));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "datetimefield" }));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region", style: "medium" }));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region", fallback: "default" }));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "language", languageDisplay: "dialect " }));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region", localeMatcher: "fastest" }));
assert.sameValue(new Intl.DisplayNames("en", { type: "region", style: undefined }).resolvedOptions().style, "long");

// Steps 6, 10 and 16's defaults, and step 19: `languageDisplay` is only kept for type "language",
// which is why it is the one resolved property that can be absent.
const defaults = new Intl.DisplayNames("en", { type: "region" }).resolvedOptions();
assert.sameValue(defaults.style, "long");
assert.sameValue(defaults.fallback, "code");
assert.sameValue(defaults.locale, "en");
assert.sameValue("languageDisplay" in defaults, false);
assert.sameValue(
  new Intl.DisplayNames("en", { type: "region", languageDisplay: "standard" }).resolvedOptions().languageDisplay,
  undefined
);
assert.sameValue(
  new Intl.DisplayNames("en", { type: "language" }).resolvedOptions().languageDisplay,
  "dialect"
);
assert.sameValue(
  new Intl.DisplayNames("da", { type: "language", languageDisplay: "standard", style: "short" })
    .resolvedOptions()
    .languageDisplay,
  "standard"
);

// 12.3.2's Table 18 order: the five properties are written in the order the rows are, and no other
// key is on the object.
assert.sameValue(
  Object.keys(new Intl.DisplayNames("en", { type: "language" }).resolvedOptions()).join(","),
  "locale,style,type,fallback,languageDisplay"
);
assert.sameValue(
  Object.keys(new Intl.DisplayNames("it", { type: "currency", style: "narrow", fallback: "none" }).resolvedOptions()).join(","),
  "locale,style,type,fallback"
);

// 12.2.2 and 9.2.6's FilterLocales against the three locales this build has data for: what comes
// back is what is available, so "sv" - which is not - is dropped rather than answered in Swedish.
assert.sameValue(Intl.DisplayNames.supportedLocalesOf(["da", "it", "en-GB"]).join(","), "da,it,en-GB");
assert.sameValue(Intl.DisplayNames.supportedLocalesOf("da").join(","), "da");
assert.sameValue(Intl.DisplayNames.supportedLocalesOf({ 0: "it", length: 1 }).join(","), "it");
assert.sameValue(Intl.DisplayNames.supportedLocalesOf(["sv", "de"]).length, 0);
assert.sameValue(Intl.DisplayNames.supportedLocalesOf([]).length, 0);
assert.sameValue(Intl.DisplayNames.supportedLocalesOf(["en", "en"]).length, 1);
assert.throws(RangeError, () => Intl.DisplayNames.supportedLocalesOf(["en"], { localeMatcher: "fastest" }));
assert.throws(TypeError, () => Intl.DisplayNames.supportedLocalesOf([1, "en"]));

// 9.2.6 with an unavailable tag: the resolution falls back to a locale that has data, and
// resolvedOptions reports the one that answered rather than the one that was asked for.
assert.sameValue(new Intl.DisplayNames("sv", { type: "region" }).resolvedOptions().locale, "en");
assert.sameValue(new Intl.DisplayNames("sv", { type: "region" }).of("SE"), "Sweden");
assert.sameValue(new Intl.DisplayNames(["nl", "it"], { type: "language" }).resolvedOptions().locale, "it");
assert.sameValue(new Intl.DisplayNames(["da", "en"], { type: "language" }).of("en"), "engelsk");

// 9.2.7's canonicalization of the tag, which 12.2.3's empty [[RelevantExtensionKeys]] leaves
// untouched by any extension: `-u-ca-buddhist` changes no table.
assert.sameValue(new Intl.DisplayNames("en-gb", { type: "region" }).resolvedOptions().locale, "en-GB");
assert.sameValue(new Intl.DisplayNames("en-u-ca-buddhist", { type: "region" }).of("DK"), "Denmark");

// 12.5.1 step 2: a region is two letters or three digits, and step 2.2 ASCII-uppercases it before the
// data is read. The page case, in all three locales.
assert.sameValue(new Intl.DisplayNames("en", { type: "region" }).of("DK"), "Denmark");
assert.sameValue(new Intl.DisplayNames("da", { type: "region" }).of("DK"), "Danmark");
assert.sameValue(new Intl.DisplayNames("it", { type: "region" }).of("DK"), "Danimarca");
assert.sameValue(new Intl.DisplayNames("en", { type: "region" }).of("dk"), "Denmark");
assert.sameValue(new Intl.DisplayNames("en", { type: "region" }).of("us"), "United States");
assert.sameValue(new Intl.DisplayNames("da", { type: "region" }).of("US"), "USA");
assert.sameValue(new Intl.DisplayNames("it", { type: "region" }).of("US"), "Stati Uniti");
assert.sameValue(new Intl.DisplayNames("en", { type: "region" }).of("IT"), "Italy");
assert.sameValue(new Intl.DisplayNames("da", { type: "region" }).of("IT"), "Italien");
assert.sameValue(new Intl.DisplayNames("it", { type: "region" }).of("IT"), "Italia");
assert.sameValue(new Intl.DisplayNames("it", { type: "region" }).of("DE"), "Germania");
assert.sameValue(new Intl.DisplayNames("da", { type: "region" }).of("DE"), "Tyskland");
assert.sameValue(new Intl.DisplayNames("en", { type: "region" }).of("GB"), "United Kingdom");
assert.sameValue(new Intl.DisplayNames("da", { type: "region" }).of("GB"), "Storbritannien");
assert.sameValue(new Intl.DisplayNames("it", { type: "region" }).of("GB"), "Regno Unito");

// The M49 numeric form of step 1's `unicode_region_subtag`, and the code CLDR keeps for no region at
// all. Both are data, so both are answered rather than invented.
assert.sameValue(new Intl.DisplayNames("en", { type: "region" }).of("419"), "Latin America");
assert.sameValue(new Intl.DisplayNames("da", { type: "region" }).of("419"), "Latinamerika");
assert.sameValue(new Intl.DisplayNames("it", { type: "region" }).of("419"), "America Latina");
assert.sameValue(new Intl.DisplayNames("en", { type: "region" }).of("ZZ"), "Unknown Region");
assert.sameValue(new Intl.DisplayNames("da", { type: "region" }).of("ZZ"), "Ukendt område");
assert.sameValue(new Intl.DisplayNames("it", { type: "region" }).of("ZZ"), "Regione sconosciuta");
assert.sameValue(new Intl.DisplayNames("en", { type: "region" }).of("AQ"), "Antarctica");

// Steps 2.1 and 5.2: what the shape does not match is refused before anything is read. "EURU" is the
// four-letter form of a currency code, "1" is neither two letters nor three digits.
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region" }).of("D"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region" }).of("DKK"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region" }).of("41"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region" }).of("4190"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region" }).of("D-"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region" }).of(""));

// 12.5.1 steps 1.1 to 1.3: a language is a `unicode_language_id`, IsWellFormedLanguageTag, and then
// CanonicalizeUnicodeLocaleId, which lowercases the subtags of a tag whose pieces are all known. The
// page case, and the three-letter ISO 639-2 code that CanonicalizeUnicodeLocaleId expands.
assert.sameValue(new Intl.DisplayNames("en", { type: "language" }).of("da"), "Danish");
assert.sameValue(new Intl.DisplayNames("da", { type: "language" }).of("en"), "engelsk");
assert.sameValue(new Intl.DisplayNames("it", { type: "language" }).of("da"), "danese");
assert.sameValue(new Intl.DisplayNames("en", { type: "language" }).of("it"), "Italian");
assert.sameValue(new Intl.DisplayNames("it", { type: "language" }).of("it"), "italiano");
assert.sameValue(new Intl.DisplayNames("en", { type: "language" }).of("ENg"), "English");
assert.sameValue(new Intl.DisplayNames("it", { type: "language" }).of("eng"), "inglese");
assert.sameValue(new Intl.DisplayNames("en", { type: "language" }).of("nb"), "Norwegian Bokmål");
assert.sameValue(new Intl.DisplayNames("da", { type: "language" }).of("nb"), "bokmål");
assert.sameValue(new Intl.DisplayNames("en", { type: "language" }).of("und"), "Unknown language");
assert.sameValue(new Intl.DisplayNames("da", { type: "language" }).of("und"), "ukendt sprog");

// Step 1.1 is a statement about the shape of the code, not about whether any language bears the
// name: a two-letter code is a `unicode_language_id` even when it is a region code, so "GB" is
// well-formed, is lowercased by step 1.3, and is then not in the data. 12.3.3 steps 7 and 8 answer
// it rather than guessing at a name for it.
assert.sameValue(new Intl.DisplayNames("en", { type: "language" }).of("GB"), "gb");
assert.sameValue(new Intl.DisplayNames("en", { type: "language" }).of("xyz"), "xyz");
assert.sameValue(new Intl.DisplayNames("it", { type: "language" }).of("xyz"), "xyz");
assert.sameValue(
  new Intl.DisplayNames("en", { type: "language", fallback: "none" }).of("xyz"),
  undefined
);

// Steps 1.1 and 1.2 refused: a one-letter code, a variant-length first subtag, an `-x-` private
// extension, which no `unicode_language_id` matches, and a non-well-formed tag.
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "language" }).of("e"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "language" }).of("toolonglang"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "language" }).of("en-x-foo"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "language" }).of("en- -US"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "language" }).of("root"));

// A compound tag is a well-formed `unicode_language_id`, but the keys of the display name tables of
// this build are single language subtags, so it is the code that comes back. Dialect names are
// `LocaleDisplayNamesV1`, which `icu/intl_markers.txt` does not bake: the answer is the fallback,
// and the absence of the data is what 12.1.1 step 19's `languageDisplay` cannot change.
assert.sameValue(new Intl.DisplayNames("en", { type: "language" }).of("en-GB"), "en-GB");
assert.sameValue(new Intl.DisplayNames("en", { type: "language", style: "short" }).of("en-GB"), "en-GB");
assert.sameValue(
  new Intl.DisplayNames("en", { type: "language", languageDisplay: "standard" }).of("sr-Cyrl-RS"),
  "sr-Cyrl-RS"
);
assert.sameValue(
  new Intl.DisplayNames("da", { type: "language" }).of("de-AT"),
  "de-AT"
);

// 12.5.1 steps 3.1 to 3.5: a script is four letters, uppercased in the first position and lowercased
// in the other three. So `of("latn")` is not the same question as `of("Zzzz")` only in case, and it
// is the canonical form that the data is keyed at.
assert.sameValue(new Intl.DisplayNames("en", { type: "script" }).of("Latn"), "Latin");
assert.sameValue(new Intl.DisplayNames("en", { type: "script" }).of("latn"), "Latin");
assert.sameValue(new Intl.DisplayNames("en", { type: "script" }).of("LATN"), "Latin");
assert.sameValue(new Intl.DisplayNames("da", { type: "script" }).of("Cyrl"), "kyrillisk");
assert.sameValue(new Intl.DisplayNames("it", { type: "script" }).of("Cyrl"), "cirillico");
assert.sameValue(new Intl.DisplayNames("en", { type: "script" }).of("Arab"), "Arabic");
assert.sameValue(new Intl.DisplayNames("it", { type: "script" }).of("Arab"), "arabo");
assert.sameValue(new Intl.DisplayNames("da", { type: "script" }).of("Hans"), "forenklet");
assert.sameValue(new Intl.DisplayNames("en", { type: "script" }).of("Hant"), "Traditional");
assert.sameValue(new Intl.DisplayNames("it", { type: "script" }).of("Deva"), "devanagari");
assert.sameValue(new Intl.DisplayNames("en", { type: "script" }).of("Zzzz"), "Unknown Script");
assert.sameValue(new Intl.DisplayNames("da", { type: "script" }).of("Zzzz"), "ukendt skriftsprog");

// Step 3.1: three letters is a region, five is a variant, and neither is a script.
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "script" }).of("Lat"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "script" }).of("Latin"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "script" }).of("Latn-Latn"));
assert.sameValue(new Intl.DisplayNames("en", { type: "script" }).of("Qaak"), "Qaak");

// 12.5.1 steps 6 to 8: a currency is exactly three letters, uppercased. The page case. The names are
// the ones NumberFormat's symbols read through, in the sentence case of the CLDR this build carries.
assert.sameValue(new Intl.DisplayNames("en", { type: "currency" }).of("USD"), "US dollar");
assert.sameValue(new Intl.DisplayNames("en", { type: "currency" }).of("usd"), "US dollar");
assert.sameValue(new Intl.DisplayNames("en", { type: "currency" }).of("DKK"), "Danish krone");
assert.sameValue(new Intl.DisplayNames("da", { type: "currency" }).of("USD"), "amerikansk dollar");
assert.sameValue(new Intl.DisplayNames("da", { type: "currency" }).of("DKK"), "dansk krone");
assert.sameValue(new Intl.DisplayNames("it", { type: "currency" }).of("EUR"), "euro");
assert.sameValue(new Intl.DisplayNames("it", { type: "currency" }).of("USD"), "dollaro statunitense");
assert.sameValue(new Intl.DisplayNames("it", { type: "currency" }).of("DKK"), "corona danese");
assert.sameValue(new Intl.DisplayNames("en", { type: "currency" }).of("JPY"), "Japanese yen");
assert.sameValue(new Intl.DisplayNames("da", { type: "currency" }).of("CHF"), "schweizerfranc");
assert.sameValue(new Intl.DisplayNames("en", { type: "currency" }).of("XXX"), "(unknown unit of currency)");

// Step 7 (8.3.1's IsWellFormedCurrencyCode): three characters that are not letters is not a currency
// code, and neither is a code of two or four.
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "currency" }).of("123"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "currency" }).of("US$"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "currency" }).of("EU"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "currency" }).of("EURU"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "currency" }).of("euro"));

// 12.5.1 step 4: a calendar is a `type` production, with no backwards-compatibility syntax,
// lowercased by step 4.3. V8 answers "GREGORY" for the code it was given because it does not
// lowercase; the spec's step 4.3 does, and the data of this build has no calendar display names at
// all, so what comes back is the lowercased code of step 4.3 rather than a name.
assert.sameValue(new Intl.DisplayNames("en", { type: "calendar" }).of("gregory"), "gregory");
assert.sameValue(new Intl.DisplayNames("en", { type: "calendar" }).of("GREGORY"), "gregory");
assert.sameValue(new Intl.DisplayNames("da", { type: "calendar" }).of("Gregory"), "gregory");
assert.sameValue(new Intl.DisplayNames("it", { type: "calendar" }).of("buddhist"), "buddhist");
assert.sameValue(new Intl.DisplayNames("en", { type: "calendar" }).of("islamic-umalqura"), "islamic-umalqura");
assert.sameValue(new Intl.DisplayNames("en", { type: "calendar" }).of("gregori"), "gregori");
assert.sameValue(
  new Intl.DisplayNames("it", { type: "calendar", fallback: "none" }).of("persian"),
  undefined
);

// Step 4.1: nine alphanumeric characters is past the `type` production, and a value that is not
// alphanumeric at all never was one.
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "calendar" }).of("gregoryxyz"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "calendar" }).of("gregory!"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "calendar" }).of("ca-gregory"));

// 12.5.2 against Table 19's twelve codes, and step 5.2: a dateTimeField is returned exactly as it
// was given, so the camel-cased names are the only spelling that is a code at all. These names are
// the field names of UTS #35's Table; this build bakes no display names for them, which is why the
// answer is the code and not "day of the week".
for (const field of ["era", "year", "quarter", "month", "weekOfYear", "weekday", "day",
                     "dayPeriod", "hour", "minute", "second", "timeZoneName"]) {
  assert.sameValue(new Intl.DisplayNames("en", { type: "dateTimeField" }).of(field), field);
}
assert.sameValue(new Intl.DisplayNames("da", { type: "dateTimeField" }).of("weekday"), "weekday");
assert.sameValue(new Intl.DisplayNames("it", { type: "dateTimeField" }).of("dayPeriod"), "dayPeriod");
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "dateTimeField" }).of("Weekday"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "dateTimeField" }).of("week"));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "dateTimeField" }).of("era-short"));
assert.sameValue(
  new Intl.DisplayNames("en", { type: "dateTimeField", fallback: "none" }).of("weekday"),
  undefined
);

// 12.3.3 steps 6 to 8, the two halves of the fallback option: a code the data has an entry for is
// answered even under "none", and one it has not is the code or undefined.
assert.sameValue(new Intl.DisplayNames("en", { type: "region", fallback: "none" }).of("DK"), "Denmark");
assert.sameValue(new Intl.DisplayNames("en", { type: "region", fallback: "none" }).of("ZZ"), "Unknown Region");
assert.sameValue(new Intl.DisplayNames("da", { type: "language", fallback: "none" }).of("xyz"), undefined);
assert.sameValue(new Intl.DisplayNames("it", { type: "script", fallback: "none" }).of("Qaak"), undefined);

// 12.1.1 step 20: the style picks which of the three tables of 12.2.3 the name is read out of. CLDR
// gives region, language and script names no short or narrow spellings of their own for these three
// locales, so all three styles answer with the wide name - which is what the data has, and is not
// the same claim as a name the engine made up.
for (const style of ["long", "short", "narrow"]) {
  assert.sameValue(new Intl.DisplayNames("en", { type: "region", style }).of("DK"), "Denmark");
  assert.sameValue(new Intl.DisplayNames("da", { type: "language", style }).of("da"), "dansk");
  assert.sameValue(new Intl.DisplayNames("it", { type: "script", style }).of("Latn"), "latino");
  assert.sameValue(new Intl.DisplayNames("en", { type: "currency", style }).of("USD"), "US dollar");
}

// 12.3.3 steps 3 and 4: the code is a String before it is examined, so a toString of an object is a
// code, and anything that will not become one is the TypeError of ToString rather than the RangeError
// of the shape.
assert.sameValue(
  new Intl.DisplayNames("en", { type: "region" }).of({ toString: () => "DK" }),
  "Denmark"
);
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region" }).of());
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region" }).of(undefined));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region" }).of(42));
assert.throws(RangeError, () => new Intl.DisplayNames("en", { type: "region" }).of(true));
assert.throws(TypeError, () => new Intl.DisplayNames("en", { type: "region" }).of(Symbol("DK")));
assert.throws(TypeError, () => new Intl.DisplayNames("en", { type: "region" }).of({
  toString: () => { throw new TypeError("refused"); },
}));

// The order of 12.3.3: step 2 is checked before step 3 coerces, so an uninitialized receiver is a
// TypeError even when the argument could not be a string.
assert.throws(TypeError, () => proto.of.call({}, Symbol("DK")));

// 12.3.3 steps 4 and 5, the two readings that must not be confused: the same code under two types is
// two different questions, and the one that has no name is not the one that has no shape.
assert.sameValue(new Intl.DisplayNames("en", { type: "region" }).of("DK"), "Denmark");
assert.sameValue(new Intl.DisplayNames("en", { type: "language" }).of("DK"), "dk");
assert.sameValue(new Intl.DisplayNames("en", { type: "language" }).of("dk"), "dk");
assert.sameValue(new Intl.DisplayNames("it", { type: "currency" }).of("DKK"), "corona danese");
assert.sameValue(new Intl.DisplayNames("it", { type: "region" }).of("DK"), "Danimarca");

// One instance answers the same way every time, and the tables behind it are shared rather than
// rebuilt per call: the same code repeated a thousand times is the same string each time.
const repeated = new Intl.DisplayNames("en", { type: "region" });
let allTheSame = true;
for (let i = 0; i < 1000; i++) {
  if (repeated.of("DK") !== "Denmark") {
    allTheSame = false;
  }
}
assert.sameValue(allTheSame, true);

// And the three locales keep their own tables: a `type` and a code read out of the pairs the data
// has, with no name shared between two locales where the data differs.
const daAndIt = [
  ["da", "region", "IT", "Italien"],
  ["it", "region", "IT", "Italia"],
  ["da", "language", "it", "italiensk"],
  ["it", "language", "da", "danese"],
  ["da", "script", "Arab", "arabisk"],
  ["it", "script", "Arab", "arabo"],
  ["da", "currency", "EUR", "euro"],
  ["it", "currency", "ITL", "lire italiane"],
];
for (const [locale, type, code, name] of daAndIt) {
  assert.sameValue(new Intl.DisplayNames(locale, { type }).of(code), name);
}

// Every one of the three locales answers the six types without an internal error, which is what a
// missing table must not be: an absent name is a fallback, not a crash.
const oneCodePerType = [
  ["language", "da"],
  ["region", "DK"],
  ["script", "Latn"],
  ["currency", "USD"],
  ["calendar", "gregory"],
  ["dateTimeField", "weekday"],
];
for (const locale of ["en", "da", "it"]) {
  for (const [type, code] of oneCodePerType) {
    assert.sameValue(typeof new Intl.DisplayNames(locale, { type }).of(code), "string");
  }
}
