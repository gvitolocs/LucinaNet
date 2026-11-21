/*---
description: Intl.Locale, ECMA-402 clause 15. 15.1.1 is the constructor and the slots it fills, 15.2
  the properties of the function itself, 15.3 the twelve accessors of 15.4's slots, the three methods
  that restate the tag and the eight Locale Info operations - the last six of which answer from the
  en, da and it rows of the baked locale info data.
---*/

// 15.1 and 15.2: %Intl.Locale% is a function of `length` 1 - only `options` of 15.1.1's signature is
// optional - named "Locale", with `prototype` and nothing else. In particular there is no
// `supportedLocalesOf`: a Locale describes a tag rather than formatting it, and 15.3.16 to 15.3.22
// are where a program asks what the locale knows.
assert.sameValue(typeof Intl.Locale, "function");
assert.sameValue(Intl.Locale.name, "Locale");
assert.sameValue(Intl.Locale.length, 1);
assert.sameValue(Object.getPrototypeOf(Intl.Locale), Function.prototype);
assert.sameValue(Intl.Locale.supportedLocalesOf, undefined);
assert.sameValue(Object.getOwnPropertyNames(Intl.Locale).sort().join(","), "length,name,prototype");

// 15.2.1: `prototype` is a non-writable, non-enumerable, non-configurable data property.
const localeProtoDesc = Object.getOwnPropertyDescriptor(Intl.Locale, "prototype");
assert.sameValue(localeProtoDesc.writable, false);
assert.sameValue(localeProtoDesc.enumerable, false);
assert.sameValue(localeProtoDesc.configurable, false);
assert.sameValue(localeProtoDesc.value, Intl.Locale.prototype);

const proto = Intl.Locale.prototype;
assert.sameValue(Object.getPrototypeOf(proto), Object.prototype);
assert.sameValue(proto.constructor, Intl.Locale);

// 15.3.1: `constructor` is the one data property, and it is the ordinary kind.
const constructorDesc = Object.getOwnPropertyDescriptor(proto, "constructor");
assert.sameValue(constructorDesc.writable, true);
assert.sameValue(constructorDesc.enumerable, false);
assert.sameValue(constructorDesc.configurable, true);

// 15.3.2 to 15.3.14 and 15.3.23, the twelve accessors; 15.3.9, 15.3.10, 15.3.15 and 15.3.16 to
// 15.3.22, the ten methods; and 15.3.24's tag. This build has all of them, including `caseFirst` and
// `numeric`, which 15.3.4 and 15.3.12 make conditional on 15.2.2's [[LocaleExtensionKeys]] containing
// "kf" and "kn".
assert.sameValue(
  Object.getOwnPropertyNames(proto).sort().join(","),
  "baseName,calendar,caseFirst,collation,constructor,firstDayOfWeek,getCalendars,getCollations," +
    "getHourCycles,getNumberingSystems,getTextInfo,getTimeZones,getWeekInfo,hourCycle,language," +
    "maximize,minimize,numberingSystem,numeric,region,script,toString,variants",
);
assert.sameValue(proto[Symbol.toStringTag], "Intl.Locale");
assert.sameValue(Object.prototype.toString.call(new Intl.Locale("en")), "[object Intl.Locale]");

for (const name of [
  "baseName",
  "calendar",
  "caseFirst",
  "collation",
  "firstDayOfWeek",
  "hourCycle",
  "language",
  "numberingSystem",
  "numeric",
  "region",
  "script",
  "variants",
]) {
  const desc = Object.getOwnPropertyDescriptor(proto, name);

  // Each is a getter and nothing else: 15.3 says so by writing only "get Intl.Locale.prototype.x".
  assert.sameValue(typeof desc.get, "function");
  assert.sameValue(desc.set, undefined);
  assert.sameValue(desc.enumerable, false);
  assert.sameValue(desc.configurable, true);
  assert.sameValue(desc.get.name, "get " + name);
  assert.sameValue(desc.get.length, 0);
}

for (const name of [
  "maximize",
  "minimize",
  "toString",
  "getCalendars",
  "getCollations",
  "getHourCycles",
  "getNumberingSystems",
  "getTextInfo",
  "getTimeZones",
  "getWeekInfo",
]) {
  const method = proto[name];
  const desc = Object.getOwnPropertyDescriptor(proto, name);

  // None of the ten takes an argument, so every `length` is 0.
  assert.sameValue(typeof method, "function");
  assert.sameValue(method.name, name);
  assert.sameValue(method.length, 0);
  assert.sameValue(desc.writable, true);
  assert.sameValue(desc.enumerable, false);
  assert.sameValue(desc.configurable, true);
}

// 15.1.1 step 1: the constructor is a constructor and nothing else. No call mode, so the TypeError
// comes before the tag has been looked at.
assert.throws(TypeError, () => Intl.Locale("en"));
assert.throws(TypeError, () => Intl.Locale());

// 15.1.1 steps 7 to 9: a tag is a String or an Object - and an Object is either another Locale, whose
// [[Locale]] step 8 takes directly, or something to stringify.
assert.throws(TypeError, () => new Intl.Locale(7));
assert.throws(TypeError, () => new Intl.Locale(null));
assert.throws(TypeError, () => new Intl.Locale());
assert.sameValue(new Intl.Locale({ toString: () => "it-IT" }).toString(), "it-IT");
assert.throws(TypeError, () => new Intl.Locale({ toString() { throw new TypeError("seen"); } }));
assert.sameValue(new Intl.Locale(new Intl.Locale("it-IT")).toString(), "it-IT");
assert.sameValue(new Intl.Locale(new Intl.Locale("it-IT")).language, "it");

// 15.1.1 step 10 is CoerceOptionsToObject (9.2.10), which is ToObject rather than a strict check: a
// primitive bag becomes an object with no options at all, `undefined` becomes an empty one, and only
// `null` - which ToObject refuses - is an error. Step 10 comes before step 11's check of the tag, so a
// bag that throws is the error even for a tag that is also wrong.
assert.throws(TypeError, () => new Intl.Locale("en", null));
assert.throws(TypeError, () => new Intl.Locale("en_US", null));
assert.sameValue(new Intl.Locale("en", 7).toString(), "en");
assert.sameValue(new Intl.Locale("en", undefined).numeric, false);
assert.sameValue(new Intl.Locale("en", {}).toString(), "en");
assert.throws(TypeError, () => new Intl.Locale("en", { get calendar() { throw new TypeError("seen"); } }));

// Step 11: a tag that is not a well-formed language tag, whatever the reason - the empty String, a
// subtag of the wrong length, an underscore instead of a hyphen.
assert.throws(RangeError, () => new Intl.Locale("not a tag"));
assert.throws(RangeError, () => new Intl.Locale(""));
assert.throws(RangeError, () => new Intl.Locale("en_US"));
assert.throws(RangeError, () => new Intl.Locale({}));

// Step 13 is CanonicalizeUnicodeLocaleId, so the tag an instance reports is the canonical one and the
// slots are read out of it. The `-u-kn-true` case is Annex C's "any type value 'true' is removed",
// which 6.2.2 step 1 applies to the whole extension.
assert.sameValue(new Intl.Locale("EN-u-kn-TRUE").toString(), "en-u-kn");
assert.sameValue(new Intl.Locale("en-Latn-us").toString(), "en-Latn-US");
assert.sameValue(new Intl.Locale("de-DE-1996").toString(), "de-DE-1996");

// 15.3.2, 15.3.8 and 15.3.13 to 15.3.15 and 15.3.23: the four subtags of the base name, which are
// GetLocaleLanguage, GetLocaleScript, GetLocaleRegion and GetLocaleVariants of 15.5.1 to 15.5.5 on
// [[Locale]], and the base name itself. `language`, `baseName` and `toString` never answer undefined;
// the other three do when the tag does not name them.
const en = new Intl.Locale("en");
assert.sameValue(en.language, "en");
assert.sameValue(en.baseName, "en");
assert.sameValue(en.toString(), "en");
assert.sameValue(en.script, undefined);
assert.sameValue(en.region, undefined);
assert.sameValue(en.variants, undefined);

const daDK = new Intl.Locale("da-DK");
assert.sameValue(daDK.language, "da");
assert.sameValue(daDK.region, "DK");
assert.sameValue(daDK.baseName, "da-DK");
assert.sameValue(daDK.script, undefined);

const zhTW = new Intl.Locale("zh-Hant-TW");
assert.sameValue(zhTW.language, "zh");
assert.sameValue(zhTW.script, "Hant");
assert.sameValue(zhTW.region, "TW");
assert.sameValue(zhTW.baseName, "zh-Hant-TW");

// 15.5.5: the whole run of variants, hyphen separated. The tag the instance reports is the canonical
// one, and Annex C orders the variant subtags by the grammar that defines them, which puts the
// digit-leading "1996" in front of the letter-leading "fonipa" however the program spelled the pair.
const sr = new Intl.Locale("sr-Cyrl-RS-fonipa-1996");
assert.sameValue(sr.variants, "1996-fonipa");
assert.sameValue(sr.script, "Cyrl");
assert.sameValue(sr.baseName, "sr-Cyrl-RS-1996-fonipa");
assert.sameValue(sr.toString(), "sr-Cyrl-RS-1996-fonipa");
assert.sameValue(sr.minimize().toString(), "sr-1996-fonipa");
assert.sameValue(new Intl.Locale("de-DE-POSIX-1901").toString(), "de-DE-1901-posix");
assert.sameValue(zhTW.variants, undefined);

// "und" is a language subtag, and an und tag is a Locale like any other. 15.5.2 has no more to say
// about it than about "en".
const und = new Intl.Locale("und");
assert.sameValue(und.language, "und");
assert.sameValue(und.toString(), "und");
assert.sameValue(und.region, undefined);

// A tag this engine knows nothing about is still a well-formed one, and the accessors only report the
// subtags: it is the data of 15.5 that has nothing to say, not the tag that is wrong.
const qq = new Intl.Locale("qq-QQ");
assert.sameValue(qq.language, "qq");
assert.sameValue(qq.region, "QQ");
assert.sameValue(qq.toString(), "qq-QQ");
assert.sameValue(qq.calendar, undefined);

// 15.3.3, 15.3.5, 15.3.7 and 15.3.11, the four slots that are the `ca`, `co`, `hc` and `nu` keywords
// of the tag as 15.1.1 steps 37, 38, 40 and 43 took them, and 15.3.6's `fw` (step 39) which is a
// String rather than the number 15.3.22 reports.
const keywords = new Intl.Locale("it-IT-u-ca-gregory-co-phonebk-fw-monday-hc-h23-nu-arab");
assert.sameValue(keywords.calendar, "gregory");
assert.sameValue(keywords.collation, "phonebk");
assert.sameValue(keywords.firstDayOfWeek, "monday");
assert.sameValue(keywords.hourCycle, "h23");
assert.sameValue(keywords.numberingSystem, "arab");
assert.sameValue(keywords.language, "it");
assert.sameValue(keywords.region, "IT");
assert.sameValue(keywords.toString(), "it-IT-u-ca-gregory-co-phonebk-fw-monday-hc-h23-nu-arab");

// The same five of a tag that writes none, and the undefined of 15.4's "either undefined or a String".
assert.sameValue(en.calendar, undefined);
assert.sameValue(en.collation, undefined);
assert.sameValue(en.firstDayOfWeek, undefined);
assert.sameValue(en.hourCycle, undefined);
assert.sameValue(en.numberingSystem, undefined);

// 15.3.4 and 15.3.12, the two slots 15.2.2's keys condition on. `caseFirst` is the `kf` keyword as
// the tag spells it, which for a bare `-kf` is the empty String; `numeric` is a Boolean, and 15.1.1
// step 42.2 gives it false rather than undefined for a tag that says nothing.
assert.sameValue(new Intl.Locale("en").caseFirst, undefined);
assert.sameValue(new Intl.Locale("en").numeric, false);
assert.sameValue(new Intl.Locale("en-u-kf").caseFirst, "");
assert.sameValue(new Intl.Locale("en-u-kf-upper").caseFirst, "upper");
assert.sameValue(new Intl.Locale("en-u-kf-false").caseFirst, "false");
assert.sameValue(new Intl.Locale("en-u-kn").numeric, true);
assert.sameValue(new Intl.Locale("en-u-kn-true").numeric, true);
assert.sameValue(new Intl.Locale("en-u-kn-false").numeric, false);
assert.sameValue(new Intl.Locale("en-u-kn-false").toString(), "en-u-kn-false");

// 15.1.2: the four subtag options, each one defaulting to the tag's own, and 15.1.1 steps 16 to 34,
// the seven keyword overrides that 15.1.3 writes into the extension. An option overrides rather than
// replaces: the keywords the tag already wrote and the override does not name survive in the order
// 15.2.2 lists them.
assert.sameValue(new Intl.Locale("en", { language: "it" }).toString(), "it");
assert.sameValue(new Intl.Locale("en", { language: "it", region: "IT" }).toString(), "it-IT");
assert.sameValue(new Intl.Locale("en", { script: "Latn" }).toString(), "en-Latn");
assert.sameValue(new Intl.Locale("en", { region: "GB" }).toString(), "en-GB");
assert.sameValue(new Intl.Locale("en", { variants: "fonipa" }).toString(), "en-fonipa");
assert.sameValue(new Intl.Locale("en", { variants: "1996-fonipa" }).toString(), "en-1996-fonipa");
assert.sameValue(new Intl.Locale("en-fonipa", { region: "RS" }).toString(), "en-RS-fonipa");
assert.sameValue(new Intl.Locale("en", { calendar: "gregory" }).toString(), "en-u-ca-gregory");
assert.sameValue(new Intl.Locale("en", { collation: "phonebk" }).toString(), "en-u-co-phonebk");
assert.sameValue(new Intl.Locale("en", { caseFirst: "upper" }).toString(), "en-u-kf-upper");
assert.sameValue(new Intl.Locale("en", { numberingSystem: "arab" }).toString(), "en-u-nu-arab");
assert.sameValue(new Intl.Locale("en", { hourCycle: "h24" }).toString(), "en-u-hc-h24");

// 15.1.1 steps 22 to 24: `firstDayOfWeek` goes through WeekdayToUValue, whose Table 26 has only the
// digits "0" to "7" and returns anything else unmodified - and the result then has to be a `type`, so
// a day name is written as the name itself while "8" is refused.
assert.sameValue(new Intl.Locale("en", { firstDayOfWeek: "monday" }).toString(), "en-u-fw-monday");
assert.sameValue(new Intl.Locale("en", { firstDayOfWeek: "monday" }).firstDayOfWeek, "monday");
assert.sameValue(new Intl.Locale("en", { firstDayOfWeek: "sunday" }).firstDayOfWeek, "sunday");
assert.sameValue(new Intl.Locale("en", { firstDayOfWeek: "0" }).firstDayOfWeek, "sun");
assert.sameValue(new Intl.Locale("en", { firstDayOfWeek: "3" }).firstDayOfWeek, "wed");
assert.sameValue(new Intl.Locale("en", { firstDayOfWeek: "6" }).firstDayOfWeek, "sat");
assert.sameValue(new Intl.Locale("en", { firstDayOfWeek: "7" }).firstDayOfWeek, "sun");
assert.throws(RangeError, () => new Intl.Locale("en", { firstDayOfWeek: "8" }));
assert.throws(RangeError, () => new Intl.Locale("en", { firstDayOfWeek: 8 }));

// Steps 29 and 30: `numeric` is the one boolean option, whose value GetOption step 3 takes with
// ToBoolean - a truth test rather than a parse, which is why "false" is true here - and 15.1.1 step 30
// then writes as text. Annex C spells that text for true as the bare key.
assert.sameValue(new Intl.Locale("en", { numeric: true }).toString(), "en-u-kn");
assert.sameValue(new Intl.Locale("en", { numeric: true }).numeric, true);
assert.sameValue(new Intl.Locale("en", { numeric: false }).toString(), "en-u-kn-false");
assert.sameValue(new Intl.Locale("en", { numeric: false }).numeric, false);
assert.sameValue(new Intl.Locale("en", { numeric: 1 }).numeric, true);
assert.sameValue(new Intl.Locale("en", { numeric: 0 }).numeric, false);
assert.sameValue(new Intl.Locale("en", { numeric: {} }).numeric, true);
assert.sameValue(new Intl.Locale("en", { numeric: "" }).numeric, false);
assert.sameValue(new Intl.Locale("en", { numeric: "false" }).numeric, true);
assert.sameValue(new Intl.Locale("en", { numeric: "false" }).toString(), "en-u-kn");

// An override replaces the keyword the tag wrote rather than adding a second one, and a keyword the
// options do not name is left where the tag put it.
assert.sameValue(new Intl.Locale("it-IT-u-hc-h12", { hourCycle: "h23" }).hourCycle, "h23");
assert.sameValue(new Intl.Locale("it-IT-u-hc-h12", { hourCycle: "h23" }).toString(), "it-IT-u-hc-h23");
assert.sameValue(new Intl.Locale("it-IT-u-ca-gregory", { calendar: "buddhist" }).calendar, "buddhist");
assert.sameValue(
  new Intl.Locale("en-u-hc-h12-nu-arab", { numberingSystem: "thai" }).toString(),
  "en-u-hc-h12-nu-thai",
);
assert.sameValue(new Intl.Locale("en-u-ca-gregory").calendar, "gregory");
assert.sameValue(new Intl.Locale("en", {}).calendar, undefined);
assert.sameValue(new Intl.Locale("en", undefined).numeric, false);

// The checks of 15.1.1 steps 17.1, 20.1, 23.2, 25, 27, 32.1 and 15.1.2 steps 3 to 9.5, all of them
// RangeErrors: a value that is not a `type`, an `hc` or `kf` off its list, a subtag of the wrong
// shape, an empty or repeated variant. A number is stringified by GetOption first, so 777 is the type
// "777" and 7 is the one-character thing that is not.
assert.throws(RangeError, () => new Intl.Locale("en", { calendar: "gregorian" }));
assert.sameValue(new Intl.Locale("en", { calendar: "ab7" }).calendar, "ab7");
assert.sameValue(new Intl.Locale("en", { calendar: 777 }).calendar, "777");
assert.throws(RangeError, () => new Intl.Locale("en", { calendar: 7 }));
assert.throws(RangeError, () => new Intl.Locale("en", { calendar: "12" }));
assert.throws(RangeError, () => new Intl.Locale("en", { collation: "phonebook" }));
assert.throws(RangeError, () => new Intl.Locale("en", { numberingSystem: "latn-ca" }));
assert.throws(RangeError, () => new Intl.Locale("en", { hourCycle: "h25" }));
assert.throws(RangeError, () => new Intl.Locale("en", { hourCycle: "H23" }));
assert.throws(RangeError, () => new Intl.Locale("en", { caseFirst: "uppercasing" }));
assert.throws(RangeError, () => new Intl.Locale("en", { language: "a" }));
assert.throws(RangeError, () => new Intl.Locale("en", { language: "abcd" }));
// A `unicode_language_subtag` of 5 to 8 letters is well-formed for 6.2.1 - the length the registry
// reserves for a registered language, which is what "posix" and a CLDR `varieties` description are -
// so it is a tag `Intl` reads and canonicalizes like any other. It is only the *option* grammar that
// decides, and 15.1.2 step 3 asks the same production.
assert.sameValue(new Intl.Locale("en", { language: "english" }).toString(), "english");
assert.sameValue(new Intl.Locale("en", { language: "english" }).language, "english");
assert.sameValue(new Intl.Locale("en-GB", { language: "english" }).toString(), "english-GB");
assert.sameValue(Intl.getCanonicalLocales("english")[0], "english");
assert.sameValue(new Intl.Locale("ENGLISH").toString(), "english");
assert.sameValue(new Intl.Locale("english-Latn-GB").toString(), "english-Latn-GB");
assert.sameValue(new Intl.Locale("english-u-ca-buddhist").toString(), "english-u-ca-buddhist");
// The tables have no row for an unregistered language, so Add and Remove Likely Subtags signal an
// error and 15.3.9 and 15.3.10 leave the tag to itself.
assert.sameValue(new Intl.Locale("english").maximize().toString(), "english");
assert.sameValue(new Intl.Locale("english").minimize().toString(), "english");
// Four letters is the length BCP 47 keeps for future use, and nine is too long for either branch of
// the production.
assert.throws(RangeError, () => new Intl.Locale("en", { language: "notalanguage" }));
assert.throws(RangeError, () => new Intl.Locale("abcd"));
assert.throws(RangeError, () => new Intl.Locale("en", { script: "Latin" }));
assert.throws(RangeError, () => new Intl.Locale("en", { region: "USA" }));
assert.throws(RangeError, () => new Intl.Locale("en", { variants: "" }));
assert.throws(RangeError, () => new Intl.Locale("en", { variants: "fonipa-fonipa" }));
assert.throws(RangeError, () => new Intl.Locale("en", { variants: "fon" }));
assert.throws(RangeError, () => new Intl.Locale("en", { variants: "123456789" }));

// 15.1.2 step 9.2 lowercases a variant before checking it and the canonicalization of 15.1.3 step 6
// writes it back case-regularized, so an option that spells a variant in capitals is not an error and
// the getter reports the canonical spelling.
assert.sameValue(new Intl.Locale("en", { variants: "FONIPA" }).variants, "fonipa");
assert.sameValue(new Intl.Locale("en", { variants: "FONIPA" }).toString(), "en-fonipa");
assert.sameValue(new Intl.Locale("en", { variants: "1996-FONIPA" }).toString(), "en-1996-fonipa");

// An option overrides one subtag of the base name and leaves the others where the tag put them.
assert.sameValue(new Intl.Locale("en-fonipa", { region: "RS" }).toString(), "en-RS-fonipa");
assert.sameValue(new Intl.Locale("en-fonipa", { region: "RS" }).variants, "fonipa");

// 15.3.15 is the tag, and 15.3.9 and 15.3.10 are the same tag through Add Likely Subtags and Remove
// Likely Subtags. Both return a new `Intl.Locale` rather than a String, and neither keeps the
// receiver's prototype: a subclass's `maximize` is a plain Locale because steps 4 name %Intl.Locale%.
assert.sameValue(en.maximize() instanceof Intl.Locale, true);
assert.sameValue(en.maximize().toString(), "en-Latn-US");
assert.sameValue(en.minimize().toString(), "en");
assert.sameValue(daDK.maximize().toString(), "da-Latn-DK");
assert.sameValue(daDK.minimize().toString(), "da");
assert.sameValue(new Intl.Locale("it").maximize().toString(), "it-Latn-IT");
assert.sameValue(new Intl.Locale("it").minimize().toString(), "it");
assert.sameValue(new Intl.Locale("en-GB").maximize().toString(), "en-Latn-GB");

// The removal is not the maximization read backwards: it searches for the shortest tag that expands
// to this one, which is why "en-GB" minimizes to itself but "en-US" does not.
assert.sameValue(new Intl.Locale("en-GB").minimize().toString(), "en-GB");
assert.sameValue(new Intl.Locale("en-US").minimize().toString(), "en");
assert.sameValue(new Intl.Locale("en-Latn").minimize().toString(), "en");
assert.sameValue(new Intl.Locale("zh-Hant-TW").minimize().toString(), "zh-TW");
assert.sameValue(new Intl.Locale("sr-Cyrl-RS-fonipa").minimize().toString(), "sr-fonipa");

// The extension and the private use sequence ride along with the subtags they are attached to.
assert.sameValue(
  new Intl.Locale("it-IT-u-ca-gregory-hc-h23").maximize().toString(),
  "it-Latn-IT-u-ca-gregory-hc-h23",
);
assert.sameValue(
  new Intl.Locale("it-IT-u-ca-gregory-hc-h23").minimize().toString(),
  "it-u-ca-gregory-hc-h23",
);
assert.sameValue(new Intl.Locale("en-u-nu-arab").maximize().toString(), "en-Latn-US-u-nu-arab");
assert.sameValue(new Intl.Locale("en-x-icu").maximize().toString(), "en-Latn-US-x-icu");

// The tables know only the subtags of the base name, so every extension sequence - an attribute, a
// transform, a private use - is as untouched by the two as the `-u-` one is.
assert.sameValue(new Intl.Locale("en-a-bar").maximize().toString(), "en-Latn-US-a-bar");
assert.sameValue(new Intl.Locale("en-Latn-US-a-bar").minimize().toString(), "en-a-bar");
assert.sameValue(
  new Intl.Locale("en-a-bar-u-nu-arab-x-icu").maximize().toString(),
  "en-Latn-US-a-bar-u-nu-arab-x-icu",
);
assert.sameValue(new Intl.Locale("en-x-icu").minimize().toString(), "en-x-icu");

// CLDR 44 and later hold a likely-subtags row for "und" itself - the root language maximizes to the
// language of the whole world's default - so 15.3.9 answers it and 15.3.10 minimizes back through it.
// A language the tables really have no row for is the case the second sentence of 15.3.9 step 3 is
// about, and `qq` below is that.
assert.sameValue(und.maximize().toString(), "en-Latn-US");
assert.sameValue(und.minimize().toString(), "en");
assert.sameValue(qq.maximize().toString(), "qq-QQ");
assert.sameValue(qq.minimize().toString(), "qq-QQ");

// 15.3.16 to 15.3.19 and 15.3.21 and 15.3.22, the six information methods of 15.5 that the baked data
// answers. Every one of them returns a fresh Array or object, so a program cannot corrupt the tables
// by writing into an answer.
const enCalendars = en.getCalendars();
assert.compareArray(enCalendars, ["gregory"]);
enCalendars.push("persian");
assert.compareArray(en.getCalendars(), ["gregory"]);
assert.compareArray(daDK.getCalendars(), ["gregory"]);
assert.compareArray(new Intl.Locale("it").getCalendars(), ["gregory"]);
assert.compareArray(new Intl.Locale("en-u-ca-buddhist").getCalendars(), ["buddhist"]);

// 15.5.10: the collations the locale offers, which for these three is the pair the data gives every
// locale, and the one the tag names.
assert.compareArray(en.getCollations(), ["emoji", "eor"]);
assert.compareArray(new Intl.Locale("it").getCollations(), ["emoji", "eor"]);
assert.compareArray(new Intl.Locale("en-u-co-phonebk").getCollations(), ["phonebk"]);

// 15.5.12 and 15.5.11: the numbering systems, and the hour cycles that the TIME_DATA row gives - in
// the order the row spells them, which is 24-hour first for Italian and 12-hour first for English.
assert.compareArray(en.getNumberingSystems(), ["latn"]);
assert.compareArray(new Intl.Locale("it").getNumberingSystems(), ["latn"]);
assert.compareArray(new Intl.Locale("en-u-nu-arab").getNumberingSystems(), ["arab"]);
assert.compareArray(en.getHourCycles(), ["h12", "h23"]);
assert.compareArray(new Intl.Locale("da").getHourCycles(), ["h23"]);
assert.compareArray(new Intl.Locale("it").getHourCycles(), ["h23", "h12"]);
assert.compareArray(new Intl.Locale("en-u-hc-h11").getHourCycles(), ["h11"]);

// 15.5.14: the direction of the tag's script, which is the script of its maximal form - so "en"
// reads Latn, and so does "und" now that the tables answer a language for it. A tag whose script the
// direction tables do not know answers an object that has `direction` and no value for it, since
// step 5 creates the property either way.
assert.sameValue(en.getTextInfo().direction, "ltr");
assert.sameValue(new Intl.Locale("da").getTextInfo().direction, "ltr");
assert.sameValue(new Intl.Locale("it").getTextInfo().direction, "ltr");
assert.sameValue(new Intl.Locale("ar").getTextInfo().direction, "rtl");
assert.sameValue(und.getTextInfo().direction, "ltr");
assert.sameValue(
  Object.prototype.hasOwnProperty.call(und.getTextInfo(), "direction"),
  true,
);
assert.sameValue(qq.getTextInfo().direction, undefined);

// 15.5.17: the week Table 27 defines, as an integral Number and a list of them in the ISO 8601
// numbering where Monday is 1 and Sunday 7. England starts its week on Sunday, Italy and Denmark on
// Monday; the weekend is the same pair everywhere.
assert.sameValue(en.getWeekInfo().firstDay, 7);
assert.compareArray(en.getWeekInfo().weekend, [6, 7]);
assert.sameValue(new Intl.Locale("da").getWeekInfo().firstDay, 1);
assert.sameValue(new Intl.Locale("it").getWeekInfo().firstDay, 1);
assert.compareArray(new Intl.Locale("it").getWeekInfo().weekend, [6, 7]);
assert.compareArray(Object.keys(en.getWeekInfo()), ["firstDay", "weekend"]);
assert.sameValue(en.getWeekInfo() !== en.getWeekInfo(), true);

// 15.3.20 and 15.5.13: TimeZonesOfLocale reads the region of 15.5.4 - the region subtag of the tag,
// not the `rg` override and not a likely subtag - and answers undefined for a tag that names no
// region.
assert.sameValue(en.getTimeZones(), undefined);
assert.sameValue(new Intl.Locale("it").getTimeZones(), undefined);
assert.sameValue(new Intl.Locale("en-u-ca-buddhist").getTimeZones(), undefined);
assert.compareArray(new Intl.Locale("it-IT").getTimeZones(), ["Europe/Rome"]);
assert.compareArray(daDK.getTimeZones(), ["Europe/Copenhagen"]);

const usZones = new Intl.Locale("en-US").getTimeZones();
assert.sameValue(usZones.length, 29);
assert.sameValue(usZones[0], "America/Adak");
assert.sameValue(usZones.indexOf("America/New_York") > 0, true);
assert.sameValue(usZones.indexOf("Pacific/Honolulu"), 28);
assert.sameValue(usZones.indexOf("Europe/Rome"), -1);

// The `rg` key overrides the region for the rest of 15.5, and not for this: the zones stay the
// country's that the tag names.
assert.compareArray(
  new Intl.Locale("en-US-u-rg-itzzzz").getTimeZones(),
  usZones,
);
assert.sameValue(new Intl.Locale("en-US-u-rg-itzzzz").getHourCycles().join(","), "h23,h12");
assert.sameValue(new Intl.Locale("en-US-u-rg-itzzzz").getWeekInfo().firstDay, 1);

// The data of this build is the three built-in locales plus the handful of regions 15.5.13's
// implementation-defined step generated. A region outside them is a loud refusal rather than an empty
// list, which would be a claim about the country rather than about this engine.
assert.throws(RangeError, () => new Intl.Locale("sr-RS").getTimeZones());
assert.throws(RangeError, () => new Intl.Locale("en-GB").getTimeZones());
assert.throws(RangeError, () => new Intl.Locale("en-001").getTimeZones());
assert.throws(
  RangeError,
  () => new Intl.Locale("qq-QQ").getTimeZones(),
  "Intl.Locale.getTimeZones has no time zone data for the region 'QQ'",
);

// 15.3.1 to 15.3.24 steps 1 and 2 alike: every accessor and method requires [[InitializedLocale]],
// which the prototype does not have and no plain object can fake.
for (const name of ["language", "baseName", "numeric", "calendar", "region", "variants"]) {
  assert.throws(TypeError, () => Object.getOwnPropertyDescriptor(proto, name).get.call({}), name);
  assert.throws(TypeError, () => Object.getOwnPropertyDescriptor(proto, name).get.call(undefined), name);
  assert.throws(TypeError, () => Object.getOwnPropertyDescriptor(proto, name).get.call(Intl), name);
}
for (const name of ["toString", "maximize", "minimize", "getCalendars", "getWeekInfo", "getTextInfo"]) {
  assert.throws(TypeError, () => proto[name].call({}), name);
  assert.throws(TypeError, () => proto[name].call(undefined), name);
  assert.throws(TypeError, () => proto[name].call(Intl.Locale.prototype), name);
}
assert.throws(TypeError, () => proto.toString.call(Object.prototype));
assert.throws(TypeError, () => proto.maximize.call(Intl));

// A method borrowed by another instance reads the receiver, and so does a getter.
assert.sameValue(en.toString.call(daDK), "da-DK");
const languageGetter = Object.getOwnPropertyDescriptor(proto, "language").get;
assert.sameValue(languageGetter.call(new Intl.Locale("it-IT")), "it");

// 15.1.1 steps 36 to 43 and the whole of 15.3, under a heap that moves: a Locale whose every accessor
// is read while the allocation of the strings next to it churns the collector. An instance that was
// collected and moved mid-read shows up here as a wrong answer rather than a crash.
const churn = [];
const many = [];
for (let i = 0; i < 40; i += 1) {
  const built = new Intl.Locale("it-IT-u-ca-gregory-hc-h23-nu-arab", {
    region: "IT",
    numeric: true,
  });
  many.push(built);
  churn.push(`${built.language}-${built.region}-${built.calendar}-${built.numeric}`);
}
assert.sameValue(churn[39], "it-IT-gregory-true");
assert.sameValue(churn[0], churn[39]);
assert.sameValue(many[39].toString(), "it-IT-u-ca-gregory-hc-h23-kn-nu-arab");
assert.sameValue(many[0].getHourCycles().join(","), "h23");
assert.compareArray(many[7].getNumberingSystems(), ["arab"]);
assert.sameValue(many[11].getTimeZones().join(","), "Europe/Rome");
assert.sameValue(new Intl.Locale(many[3]).toString(), "it-IT-u-ca-gregory-hc-h23-kn-nu-arab");

// 9.2.1 steps 3 and 7.3: a Locale is one of the values a `locales` argument may hold - as the whole
// argument or as an element of a list - and it is taken as its [[Locale]] before any stringification.
// That is what the four services' `Locale-object.js` cases of test262 check, and it is why a shadowed
// `toString` of an instance makes no difference: step 7.3.4's ToString is the branch an object without
// the slot takes.
const itIT = new Intl.Locale("it-IT");
const de = new Intl.Locale("de");
const services = [Intl.Collator, Intl.NumberFormat, Intl.PluralRules, Intl.ListFormat];

for (const Service of services) {
  const name = Service.name;

  // The instance itself, an array of them, and an array-like that holds one at index 0: every one
  // answers exactly what the tag inside the instance answers, which is the whole of what step 7.3.3
  // claims. What that answer is is 9.2.7's business, and the four services disagree about it.
  const tag = new Service("it-IT").resolvedOptions().locale;
  assert.sameValue(new Service(itIT).resolvedOptions().locale, tag, name);
  assert.sameValue(new Service([itIT]).resolvedOptions().locale, tag, name);
  assert.sameValue(new Service({ 0: itIT, length: 1 }).resolvedOptions().locale, tag, name);
  assert.compareArray(Service.supportedLocalesOf(itIT), Service.supportedLocalesOf("it-IT"), name);

  // A Locale this engine has no data for is 9.2.7's fallback rather than an error, exactly as the tag
  // spelled out would be.
  assert.sameValue(new Service(de).resolvedOptions().locale, "en", name);
  assert.sameValue(new Service([de, "it-IT"]).resolvedOptions().locale, tag, name);

  const shadowed = new Intl.Locale("it-IT");
  shadowed.toString = () => "de-DE";
  assert.sameValue(new Service(shadowed).resolvedOptions().locale, tag, name);
  assert.sameValue(new Service([shadowed]).resolvedOptions().locale, tag, name);
}

// Collator resolves "it-IT" down to the language-only locale its data is keyed by where the other
// three report the whole tag; that is 9.2.7's doing and not the list's.
assert.sameValue(new Intl.Collator(itIT).resolvedOptions().locale, "it");
assert.sameValue(new Intl.NumberFormat(itIT).resolvedOptions().locale, "it-IT");
assert.sameValue(new Intl.PluralRules(itIT).resolvedOptions().locale, "it-IT");
assert.sameValue(new Intl.ListFormat(itIT).resolvedOptions().locale, "it-IT");
assert.compareArray(Intl.Collator.supportedLocalesOf(itIT), ["it-IT"]);

// A subclass instance has the slot like any other, and is the same tag to a service.
class MyLocale extends Intl.Locale {}
const mine = new MyLocale("it-IT");
assert.sameValue(Object.getPrototypeOf(mine), MyLocale.prototype);
assert.sameValue(mine instanceof Intl.Locale, true);
assert.sameValue(mine.region, "IT");
assert.sameValue(new Intl.NumberFormat(mine).resolvedOptions().locale, "it-IT");
assert.sameValue(Intl.NumberFormat.supportedLocalesOf(mine).join(","), "it-IT");
assert.sameValue(Intl.getCanonicalLocales(mine).join(","), "it-IT");
assert.sameValue(Intl.getCanonicalLocales([mine, "da"]).join(","), "it-IT,da");

// It is still a plain `Intl.Locale` that 15.3.9 and 15.3.10 return, since those steps construct
// %Intl.Locale% rather than the receiver.
assert.sameValue(mine.maximize() instanceof MyLocale, false);
assert.sameValue(mine.maximize() instanceof Intl.Locale, true);

// An object of the right prototype but without the slot is not a Locale: 9.2.1 step 3 does not take
// the single-element branch, and the empty list of a zero-length object is the default locale.
assert.sameValue(new Intl.NumberFormat(Object.create(Intl.Locale.prototype)).resolvedOptions().locale, "en");

// The three refusals of the same clause, which a Locale is exempt from in the one way of step 7.3.3:
// an element that is neither a String nor an Object is a TypeError before any conversion (so `NaN` is
// not the tag "nan"), a tag that is not well-formed is a RangeError, and a bag of properties names no
// element at all.
assert.throws(TypeError, () => new Intl.NumberFormat([7]));
assert.throws(TypeError, () => new Intl.NumberFormat(["en", 7]));
assert.throws(TypeError, () => new Intl.NumberFormat([NaN]));
assert.throws(TypeError, () => new Intl.NumberFormat([Symbol("it")]));
assert.throws(RangeError, () => new Intl.NumberFormat(["en", "[object Object]"]));
assert.throws(RangeError, () => new Intl.NumberFormat([{}]));
assert.sameValue(new Intl.NumberFormat({ locale: "it-IT" }).resolvedOptions().locale, "en");
assert.sameValue(new Intl.NumberFormat({ 0: "it-IT", length: 0 }).resolvedOptions().locale, "en");
assert.sameValue(new Intl.NumberFormat([, "it-IT"]).resolvedOptions().locale, "it-IT");
assert.sameValue(Intl.getCanonicalLocales([itIT, "it-IT"]).length, 1);
