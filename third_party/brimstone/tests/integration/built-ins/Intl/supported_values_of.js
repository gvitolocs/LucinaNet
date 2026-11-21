/*---
description: Intl.supportedValuesOf, ECMA-402 8.3.2. Each of the six branches is the list of values
  this build's data can format with, drawn out of the baked tables rather than out of a table of what
  CLDR or another engine happens to know; a branch with nothing baked answers with the empty list.
---*/

// 8.3.1's shape of the function itself: one argument, the usual attributes of an `Intl` data
// property, and no dependency on the receiver.
const method = Object.getOwnPropertyDescriptor(Intl, "supportedValuesOf");
assert.sameValue(typeof method.value, "function");
assert.sameValue(method.value.name, "supportedValuesOf");
assert.sameValue(method.value.length, 1);
assert.sameValue(method.writable, true);
assert.sameValue(method.enumerable, false);
assert.sameValue(method.configurable, true);
assert.sameValue(Intl.supportedValuesOf.call(null, "calendar").length, 1);

// Step 1 is `? ToString(key)`, so an object with a toString names a branch, and a Symbol is the
// TypeError of that step rather than the RangeError of step 8.
assert.sameValue(Intl.supportedValuesOf({ toString: () => "calendar" }).length, 1);
assert.throws(TypeError, () => Intl.supportedValuesOf(Symbol("calendar")));
assert.throws(TypeError, () => Intl.supportedValuesOf({
  toString: () => { throw new TypeError("refused"); },
}));

// Step 8: the six keys, exactly, case-sensitively. Anything else - including the absent argument,
// whose ToString is the string "undefined" - is the RangeError, and a name that is a real option of
// one of the constructors is not one of the six.
for (const key of ["calendar", "collation", "currency", "numberingSystem", "timeZone", "unit"]) {
  assert.sameValue(Array.isArray(Intl.supportedValuesOf(key)), true);
}
assert.throws(RangeError, () => Intl.supportedValuesOf("bogus"));
assert.throws(RangeError, () => Intl.supportedValuesOf());
assert.throws(RangeError, () => Intl.supportedValuesOf(undefined));
assert.throws(RangeError, () => Intl.supportedValuesOf(null));
assert.throws(RangeError, () => Intl.supportedValuesOf(42));
assert.throws(RangeError, () => Intl.supportedValuesOf("Calendar"));
assert.throws(RangeError, () => Intl.supportedValuesOf("time zone"));
assert.throws(RangeError, () => Intl.supportedValuesOf("style"));
assert.throws(RangeError, () => Intl.supportedValuesOf("type"));
assert.throws(RangeError, () => Intl.supportedValuesOf("locale"));
assert.throws(RangeError, () => Intl.supportedValuesOf("numeric"));

// Step 9's CreateArrayFromList hands out a fresh array each time, so what a program does to one
// answer changes no other.
const firstAnswer = Intl.supportedValuesOf("calendar");
firstAnswer.push("not a calendar");
assert.sameValue(Intl.supportedValuesOf("calendar").length, 1);
assert.sameValue(firstAnswer.length, 2);
assert.sameValue(Intl.supportedValuesOf("calendar") === firstAnswer, false);

// Every list is strings, ordered the way `%Array.prototype.sort%` with no comparator orders, and
// without repetitions - the three properties 8.3.2's CreateSortedStringList carries.
for (const key of ["calendar", "collation", "currency", "numberingSystem", "timeZone", "unit"]) {
  const list = Intl.supportedValuesOf(key);

  for (const value of list) {
    assert.sameValue(typeof value, "string");
  }

  const sorted = [...list].sort();
  assert.sameValue(JSON.stringify(sorted) === JSON.stringify(list), true);
  assert.sameValue(new Set(list).size, list.length);
}

// Step 2, AvailableCanonicalCalendars: the one calendar this build formats with, in the spelling
// `Intl.DateTimeFormat`'s `calendar` option reports back. The aliases and the calendars of the wider
// CLDR are absent because no data was baked for them, and an absent value is not reported.
assert.sameValue(Intl.supportedValuesOf("calendar").join(","), "gregory");
assert.sameValue(
  new Intl.DateTimeFormat("en", { calendar: "gregory" }).resolvedOptions().calendar,
  "gregory"
);
for (const absent of ["buddhist", "islamic", "persian", "hebrew", "iso8601", "roc", "dangi"]) {
  assert.sameValue(Intl.supportedValuesOf("calendar").includes(absent), false);
}

// Step 3, AvailableCanonicalCollations: empty. A `Collator` of this build tailors nothing by name -
// 10.1.1 resolves `collation` but the only comparison the baked data supports is the default one -
// so there is no named collation to report and the list is not filled in from CLDR.
assert.sameValue(Intl.supportedValuesOf("collation").length, 0);
assert.sameValue(Intl.supportedValuesOf("collation").join(","), "");

// Step 4, AvailableCanonicalCurrencies: the three-letter codes the baked currency data has a row
// for, which is the same set `Intl.NumberFormat` accepts for its `currency` option and the same set
// `Intl.DisplayNames` of type "currency" has a name for. It is found by asking the data rather than
// written down, so retired codes are in it and no live code is missing from it.
const currencies = Intl.supportedValuesOf("currency");
assert.sameValue(currencies.length > 100, true);
assert.sameValue(currencies.every((code) => /^[A-Z]{3}$/.test(code)), true);
for (const code of ["USD", "DKK", "EUR", "ITL", "JPY", "CHF", "AFN", "XAU", "DOP", "ADP"]) {
  assert.sameValue(currencies.includes(code), true);
}
for (const notACode of ["usd", "EU", "EURO", "123", "US$"]) {
  assert.sameValue(currencies.includes(notACode), false);
}

// The two cross-checks over the whole list: every code is one the formatter takes and the name table
// answers. A code in this list that either refused would be an invented value.
let currenciesUnusable = 0;
let currenciesUnnamed = 0;
for (const code of currencies) {
  try {
    if (new Intl.NumberFormat("en", { style: "currency", currency: code }).resolvedOptions().currency !== code) {
      currenciesUnusable += 1;
    }
  } catch (e) {
    currenciesUnusable += 1;
  }

  if (new Intl.DisplayNames("en", { type: "currency" }).of(code) === code) {
    currenciesUnnamed += 1;
  }
}
assert.sameValue(currenciesUnusable, 0);
assert.sameValue(currenciesUnnamed, 0);

// Step 5, AvailableCanonicalNumberingSystems: the digits the baked number formats have patterns for.
// The other sixty-odd numeral systems of CLDR are absent here, so the list has one entry.
assert.sameValue(Intl.supportedValuesOf("numberingSystem").join(","), "latn");
assert.sameValue(
  new Intl.NumberFormat("en", { numberingSystem: "latn" }).resolvedOptions().numberingSystem,
  "latn"
);
assert.sameValue(Intl.supportedValuesOf("numberingSystem").includes("arab"), false);
assert.sameValue(Intl.supportedValuesOf("numberingSystem").includes("deva"), false);
assert.sameValue(Intl.supportedValuesOf("numberingSystem").includes("fullwide"), false);

// Step 6, AvailablePrimaryTimeZoneIdentifiers: the IANA ids the time-zone provider resolves, with
// the links left out because an id that is an alias resolves to another entry rather than to itself.
const zones = Intl.supportedValuesOf("timeZone");
assert.sameValue(zones.length > 300, true);
for (const zone of ["UTC", "Europe/Copenhagen", "Europe/Rome", "America/New_York",
                    "America/Danmarkshavn", "America/Asuncion", "Africa/Tunis",
                    "Antarctica/Troll", "Pacific/Wallis"]) {
  assert.sameValue(zones.includes(zone), true);
}
for (const link of ["US/Eastern", "Africa/Asmera", "Etc/UTC", "GMT", "Etc/GMT", "EST5EDT", "MET",
                    "Asia/Calcutta", "Asia/Katmandu"]) {
  assert.sameValue(zones.includes(link), false, `${link} is a link, not a primary id`);
}

// Every id on the list is one 11.5.3 canonicalizes to itself, which is what makes the answer the
// same list `Intl.DateTimeFormat` formats with rather than a list of names it would refuse.
let zonesRefused = 0;
let zonesRewritten = 0;
for (const zone of zones) {
  try {
    if (new Intl.DateTimeFormat("en", { timeZone: zone }).resolvedOptions().timeZone !== zone) {
      zonesRewritten += 1;
    }
  } catch (e) {
    zonesRefused += 1;
  }
}
assert.sameValue(zonesRefused, 0);
assert.sameValue(zonesRewritten, 0);

// Step 7, AvailableCanonicalUnits: empty, because no unit data is baked. `Intl.NumberFormat` of
// style "unit" is the RangeError it reports, so there is no sanctioned simple unit to list.
assert.sameValue(Intl.supportedValuesOf("unit").length, 0);
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: "unit", unit: "mile-per-hour" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: "unit", unit: "gigabit" }));
