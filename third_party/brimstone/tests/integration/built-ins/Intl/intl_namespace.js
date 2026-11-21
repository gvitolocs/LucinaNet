/*---
description: The Intl namespace object itself, which ECMA-402 section 8 defines before any of its
  constructors exist.
---*/

// Intl is an ordinary object, not a function, and it is not callable or constructible.
assert.sameValue(typeof Intl, "object");
assert.sameValue(Object.prototype.toString.call(Intl), "[object Intl]");
assert.sameValue(Intl[Symbol.toStringTag], "Intl");

assert.throws(TypeError, () => Intl());
assert.throws(TypeError, () => new Intl());

// The global property, which the spec gives the same attributes as Reflect: writable,
// non-enumerable and configurable.
const descriptor = Object.getOwnPropertyDescriptor(globalThis, "Intl");
assert.sameValue(descriptor.writable, true);
assert.sameValue(descriptor.enumerable, false);
assert.sameValue(descriptor.configurable, true);
assert.sameValue(Object.keys(globalThis).includes("Intl"), false);

// Intl inherits from Object.prototype.
assert.sameValue(Object.getPrototypeOf(Intl), Object.prototype);

// getCanonicalLocales is a function property with the usual attributes.
const method = Object.getOwnPropertyDescriptor(Intl, "getCanonicalLocales");
assert.sameValue(typeof method.value, "function");
assert.sameValue(method.value.name, "getCanonicalLocales");
assert.sameValue(method.value.length, 1);
assert.sameValue(method.enumerable, false);
assert.sameValue(method.writable, true);
assert.sameValue(method.configurable, true);

// The services that have been built are the data properties 8.1 describes: writable and
// configurable, and not enumerable.
const collator = Object.getOwnPropertyDescriptor(Intl, "Collator");
assert.sameValue(typeof collator.value, "function");
assert.sameValue(collator.writable, true);
assert.sameValue(collator.enumerable, false);
assert.sameValue(collator.configurable, true);

// Intl.ListFormat (https://tc39.es/ecma402/#sec-intl.listformat-intro), 8.2.5: the constructor of
// clause 14 that the namespace object installs. What it does is list_format.js's business.
const listFormat = Object.getOwnPropertyDescriptor(Intl, "ListFormat");
assert.sameValue(typeof listFormat.value, "function");
assert.sameValue(listFormat.writable, true);
assert.sameValue(listFormat.enumerable, false);
assert.sameValue(listFormat.configurable, true);

// Intl.NumberFormat (https://tc39.es/ecma402/#sec-intl.numberformat-intro), 8.2.7: the constructor
// the namespace object installs, with the attributes the table gives it. What it does is
// number_format.js's business.
const numberFormat = Object.getOwnPropertyDescriptor(Intl, "NumberFormat");
assert.sameValue(typeof numberFormat.value, "function");
assert.sameValue(numberFormat.writable, true);
assert.sameValue(numberFormat.enumerable, false);
assert.sameValue(numberFormat.configurable, true);

// Intl.DateTimeFormat (https://tc39.es/ecma402/#sec-intl.datetimeformat-intro), 8.2.2: the
// constructor of clause 11 that the namespace object installs. What it does is
// date_time_format.js's business.
const dateTimeFormat = Object.getOwnPropertyDescriptor(Intl, "DateTimeFormat");
assert.sameValue(typeof dateTimeFormat.value, "function");
assert.sameValue(dateTimeFormat.writable, true);
assert.sameValue(dateTimeFormat.enumerable, false);
assert.sameValue(dateTimeFormat.configurable, true);

// Intl.PluralRules (https://tc39.es/ecma402/#sec-intl.pluralrules-intro), 8.2.8: the constructor of
// clause 17 that the namespace object installs. What it does is plural_rules.js's business.
const pluralRules = Object.getOwnPropertyDescriptor(Intl, "PluralRules");
assert.sameValue(typeof pluralRules.value, "function");
assert.sameValue(pluralRules.writable, true);
assert.sameValue(pluralRules.enumerable, false);
assert.sameValue(pluralRules.configurable, true);

// Intl.RelativeTimeFormat (https://tc39.es/ecma402/#sec-intl.relativetimeformat-intro), 8.2.9: the
// constructor of clause 18 that the namespace object installs. What it does is
// relative_time_format.js's business.
const relativeTimeFormat = Object.getOwnPropertyDescriptor(Intl, "RelativeTimeFormat");
assert.sameValue(typeof relativeTimeFormat.value, "function");
assert.sameValue(relativeTimeFormat.writable, true);
assert.sameValue(relativeTimeFormat.enumerable, false);
assert.sameValue(relativeTimeFormat.configurable, true);

// Intl.Locale (https://tc39.es/ecma402/#sec-intl-locale-intro), 15: the description of a language
// tag rather than a formatter, and so the one class of the namespace whose constructor has no
// `supportedLocalesOf`. What it does is locale.js's business.
const locale = Object.getOwnPropertyDescriptor(Intl, "Locale");
assert.sameValue(typeof locale.value, "function");
assert.sameValue(locale.writable, true);
assert.sameValue(locale.enumerable, false);
assert.sameValue(locale.configurable, true);
assert.sameValue(locale.value.supportedLocalesOf, undefined);

// Intl.DisplayNames (https://tc39.es/ecma402/#sec-intl.displaynames-intro), 8.2.3: the constructor of
// clause 12 that the namespace object installs. What it does is display_names.js's business.
const displayNames = Object.getOwnPropertyDescriptor(Intl, "DisplayNames");
assert.sameValue(typeof displayNames.value, "function");
assert.sameValue(displayNames.writable, true);
assert.sameValue(displayNames.enumerable, false);
assert.sameValue(displayNames.configurable, true);

// Intl.Segmenter (https://tc39.es/ecma402/#sec-intl-segmenter-intro), 8.2.10: the one member that
// hands back another object rather than a string, and so the one whose `segment` has a clause of
// its own for what it returns. What a Segments object is is segmenter.js's business.
const segmenter = Object.getOwnPropertyDescriptor(Intl, "Segmenter");
assert.sameValue(typeof segmenter.value, "function");
assert.sameValue(segmenter.writable, true);
assert.sameValue(segmenter.enumerable, false);
assert.sameValue(segmenter.configurable, true);
assert.sameValue(typeof segmenter.value.supportedLocalesOf, "function");

// Intl.supportedValuesOf (https://tc39.es/ecma402/#sec-intl.supportedvaluesof), 8.3.2: not a service
// constructor but the other of the two function properties 8.1 lists, since it formats nothing and
// so has no `prototype`, no `supportedLocalesOf` and no resolved options. What it answers is
// supported_values_of.js's business.
const supportedValuesOf = Object.getOwnPropertyDescriptor(Intl, "supportedValuesOf");
assert.sameValue(typeof supportedValuesOf.value, "function");
assert.sameValue(supportedValuesOf.writable, true);
assert.sameValue(supportedValuesOf.enumerable, false);
assert.sameValue(supportedValuesOf.configurable, true);
assert.sameValue(supportedValuesOf.value.prototype, undefined);

// Nothing else of ECMA-402 has been built yet, and a member that is not built is missing rather
// than faked.
assert.sameValue(Intl.DurationFormat, undefined);
