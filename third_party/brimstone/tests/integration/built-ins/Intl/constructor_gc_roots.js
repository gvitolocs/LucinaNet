/*---
description: The Intl service constructors keep their instance rooted while 9.2.8's
  ResolveOptions runs. That reading allocates - the locale list, the options bag and every getter on
  either - so a collection can move the new block at any point of it, and a constructor that hands
  back the address the block had before the move gives the caller a pointer into the space that is
  about to be reused. What the caller then does with that pointer is what this test looks for.
---*/

// Every one of these loops makes hundreds of instances and keeps what they answer, so that the kept
// values survive a collection and are read back afterwards. The assertions are ordinary ones: the
// bug this covers is not a wrong answer but a block read after it moved, which shows up as a value
// that is no longer an object, a method that reports an uninitialized instance, or no answer at all.
const services = [Intl.Collator, Intl.DateTimeFormat, Intl.NumberFormat, Intl.PluralRules, Intl.ListFormat, Intl.RelativeTimeFormat, Intl.Segmenter];

for (const Service of services) {
    // The instance itself, kept: 9.2.8 read its options, and the object that comes back has to be
    // the block the collection left it in.
    const instances = [];
    for (let i = 0; i < 200; i++) {
        instances.push(new Service(["en"], { localeMatcher: "lookup" }));
    }

    for (const instance of instances) {
        assert.sameValue(typeof instance.resolvedOptions, "function");
        assert.sameValue(instance.resolvedOptions().locale, "en");
    }

    // The resolved options object, kept: `resolvedOptions` is written after the last allocation of
    // the constructor, so an instance that lost its slots on the way out reports uninitialized here.
    const options = [];
    for (let i = 0; i < 200; i++) {
        options.push(new Service(["en"], { localeMatcher: "lookup" }).resolvedOptions());
    }

    for (const resolved of options) {
        assert.sameValue(resolved.locale, "en");
    }

    // A value formatted by an instance, kept, which is the case the whole suite reaches most: the
    // string outlives the instance that made it.
    const formatted = [];
    for (let i = 0; i < 200; i++) {
        formatted.push(new Service(["en"], { localeMatcher: "lookup" }).resolvedOptions().locale);
    }

    for (const locale of formatted) {
        assert.sameValue(locale, "en");
    }
}

// `format` and `compare` are accessors that make one function per instance and store it on it, so
// reading them runs code that both allocates and writes the instance back. Keeping the functions is
// the case where a stale write is visible: `nf.format === nf.format` only holds if the getter saw
// the [[BoundFormat]] of the block the instance lives in.
const retainedNumberFormatAccessors = [];
const retainedCollatorAccessors = [];
const retainedDateTimeFormatAccessors = [];

for (let i = 0; i < 200; i++) {
    const numberFormat = new Intl.NumberFormat(["en"]);
    assert.sameValue(numberFormat.format, numberFormat.format);
    retainedNumberFormatAccessors.push(numberFormat.format);

    const collator = new Intl.Collator(["en"]);
    assert.sameValue(collator.compare, collator.compare);
    retainedCollatorAccessors.push(collator.compare);

    const dateTimeFormat = new Intl.DateTimeFormat(["en"]);
    assert.sameValue(dateTimeFormat.format, dateTimeFormat.format);
    retainedDateTimeFormatAccessors.push(dateTimeFormat.format);
}

const retained = retainedNumberFormatAccessors
    .concat(retainedCollatorAccessors)
    .concat(retainedDateTimeFormatAccessors);
for (const accessor of retained) {
    assert.sameValue(typeof accessor, "function");
}

// The three accessors answer different things - `format` a String twice, `compare` the sign of a
// comparison - and all of them have to still be the function the instance stored.
for (const format of retainedNumberFormatAccessors) {
    assert.sameValue(typeof format(1), "string");
}
for (const compare of retainedCollatorAccessors) {
    assert.sameValue(typeof compare("a", "b"), "number");
}
for (const format of retainedDateTimeFormatAccessors) {
    assert.sameValue(typeof format(new Date(2026, 1, 3, 4, 5, 6)), "string");
}

// The shape of the two test262 cases that first reached this: `supportedLocalesOf` over every tag a
// program can ask for, with and without a Unicode locale extension sequence, and the same tags read
// back through a constructed instance. Both loop over enough allocation that a collection is certain
// part-way through, and both keep what they read.
const languages = ["zh", "es", "en", "hi", "ur", "ar", "ja", "pa"];
const scripts = ["Latn", "Hans", "Deva", "Arab"];
const countries = ["CN", "IN", "US", "PK", "JP", "TW"];

const allTags = [];
for (const language of languages) {
    allTags.push(language);
    for (const script of scripts) {
        allTags.push(`${language}-${script}`);
        for (const country of countries) {
            allTags.push(`${language}-${script}-${country}`);
        }
    }
    for (const country of countries) {
        allTags.push(`${language}-${country}`);
    }
}

for (const Service of services) {
    for (const matcher of ["lookup", "best fit"]) {
        const options = { localeMatcher: matcher };
        const resolved = [];
        for (const request of allTags) {
            resolved.push(new Service([request], options).resolvedOptions().locale);
        }

        // Whatever came back has to still be a language tag this engine reports, and the count has
        // to be the count of the tags asked for.
        assert.sameValue(resolved.length, allTags.length);
        for (const locale of resolved) {
            assert.sameValue(typeof locale, "string");
            assert.sameValue(locale.length > 1, true);
        }

        const supported = [];
        for (const request of allTags) {
            supported.push(Service.supportedLocalesOf([request], options));
            supported.push(Service.supportedLocalesOf([`${request}-u-co-phonebk-nu-latn`], options));
        }

        for (const tags of supported) {
            assert.sameValue(Array.isArray(tags), true);
        }
    }
}

// Intl.DateTimeFormat is the fifth service, and the one with the most read out of the instance
// after 9.2.8 has finished: 11.5.6 and 11.5.8 format through the field set, the zone and the locale
// data the instance resolved to, and 11.3.2 reports the four String slots of 11.4. Both of those
// run after allocations - the rendering builds the strings it returns, and `resolvedOptions` builds
// the object it writes into - so every one of them is a second chance to read the block where the
// collector left it rather than where it now is. These are the paths the constructor loops above do
// not reach: they format, and they do it from an instance that was built and set aside first.
const rangeStart = new Date(2026, 1, 3, 4, 5, 6, 7);
const rangeEnd = new Date(2026, 1, 3, 9, 41, 32, 3);

const optionsBag = {
    localeMatcher: "lookup",
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "numeric",
    timeZone: "Europe/Copenhagen",
};

// The instances, made once and kept, and the four answers each of them gives, kept as well.
const retainedFormats = [];
const retainedParts = [];
const retainedRanges = [];
const retainedRangeParts = [];

for (let i = 0; i < 100; i++) {
    const formatter = new Intl.DateTimeFormat(["en"], optionsBag);

    assert.sameValue(formatter.resolvedOptions().locale, "en");
    assert.sameValue(formatter.resolvedOptions().calendar, "gregory");
    assert.sameValue(formatter.resolvedOptions().numberingSystem, "latn");
    assert.sameValue(typeof formatter.resolvedOptions().timeZone, "string");

    retainedFormats.push(formatter.format(rangeStart));
    retainedParts.push(formatter.formatToParts(rangeStart));
    retainedRanges.push(formatter.formatRange(rangeStart, rangeEnd));
    retainedRangeParts.push(formatter.formatRangeToParts(rangeStart, rangeEnd));
}

// What was kept is read back here, on the far side of a hundred more instances. A string that came
// out of a moved block is not the string that was put in, and a part list whose records have gone is
// the same failure in the other direction.
for (const text of retainedFormats) {
    assert.sameValue(typeof text, "string");
    assert.sameValue(text.length > 0, true);
    assert.sameValue(text, retainedFormats[0]);
}

for (const parts of retainedParts) {
    assert.sameValue(Array.isArray(parts), true);
    assert.sameValue(parts.length > 1, true);
    for (const part of parts) {
        assert.sameValue(typeof part.type, "string");
        assert.sameValue(typeof part.value, "string");
    }
}

for (const text of retainedRanges) {
    assert.sameValue(typeof text, "string");
    assert.sameValue(text.length > 0, true);
    assert.sameValue(text, retainedRanges[0]);
}

for (const parts of retainedRangeParts) {
    assert.sameValue(Array.isArray(parts), true);
    assert.sameValue(parts.length > 1, true);
    for (const part of parts) {
        assert.sameValue(typeof part.type, "string");
        assert.sameValue(typeof part.value, "string");
        assert.sameValue(["startRange", "endRange", "shared"].includes(part.source), true);
    }
}

// The bound function 11.3.3 hands out formats through the capture it stored on itself, which is the
// instance read back from a private symbol rather than from `this`. Keeping it is the case where a
// capture that went stale is visible.
const boundFormats = [];
for (let i = 0; i < 100; i++) {
    boundFormats.push(new Intl.DateTimeFormat(["en"], optionsBag).format);
}

for (const bound of boundFormats) {
    assert.sameValue(typeof bound, "function");
    assert.sameValue(typeof bound(rangeStart), "string");
    assert.sameValue(bound(rangeStart), retainedFormats[0]);
}

// 20.4.1 to 20.4.3, which make the formatter and format through it without the program ever seeing
// the instance: one `CreateDateTimeFormat` and one `FormatDateTime`, allocations on both sides of
// the reading.
const localStrings = [];
const localDateStrings = [];
const localTimeStrings = [];

for (let i = 0; i < 100; i++) {
    localStrings.push(rangeStart.toLocaleString("en", optionsBag));
    localDateStrings.push(rangeStart.toLocaleDateString("en", optionsBag));
    localTimeStrings.push(rangeStart.toLocaleTimeString("en", {
        localeMatcher: "lookup",
        hour: "numeric",
        minute: "numeric",
        timeZone: "Europe/Copenhagen",
    }));
}

for (const text of localStrings) {
    assert.sameValue(typeof text, "string");
    assert.sameValue(text, retainedFormats[0]);
}

for (const text of localDateStrings) {
    assert.sameValue(typeof text, "string");
    assert.sameValue(text.length > 0, true);
}

for (const text of localTimeStrings) {
    assert.sameValue(typeof text, "string");
    assert.sameValue(text.length > 0, true);
}

// The `narrow` of Table 16, which is the one width that renders through a formatter of its own: the
// field set is built at the longest length of its group and a second pattern is made from it with
// the name symbols widened to `EEEEE`, `MMMMM` and `GGGGG`, with the narrow names loaded for that
// pattern beside the ones the plain formatter already holds. Both live in the cache entry the
// constructor resolved to, so a hundred instances of one narrow bag share one build of them while
// each of them allocates the answers below - the same read as the plain formats, from a renderer
// that was made a hundred constructions ago and is reached through the instance rather than kept.
const narrowDay = new Date(Date.UTC(2026, 1, 18, 9, 36, 32, 180));
const narrowBags = [
    { weekday: "narrow", month: "long", day: "numeric", timeZone: "UTC" },
    { month: "narrow", day: "numeric", year: "numeric", timeZone: "UTC" },
    { era: "narrow", year: "numeric", timeZone: "UTC" },
];
const narrowAnswers = ["W, February 18", "F 18, 2026", "2026 A"];
const narrowKeys = ["weekday", "month", "era"];

const retainedNarrowInstances = [];
const retainedNarrowFormats = [];
const retainedNarrowParts = [];
const retainedNarrowRanges = [];
const retainedNarrowOptions = [];

for (const [index, options] of narrowBags.entries()) {
    for (let i = 0; i < 100; i += 1) {
        churn();
        const formatter = new Intl.DateTimeFormat(["en"], options);

        assert.sameValue(formatter.format(narrowDay), narrowAnswers[index], "narrow answer " + index);
        assert.sameValue(formatter.resolvedOptions()[narrowKeys[index]], "narrow", "the width is on the instance");

        retainedNarrowInstances.push(formatter);
        retainedNarrowFormats.push(formatter.format(narrowDay));
        retainedNarrowParts.push(formatter.formatToParts(narrowDay));
        retainedNarrowRanges.push(formatter.formatRange(narrowDay, new Date(Date.UTC(2026, 1, 20, 8, 0))));
        retainedNarrowOptions.push(formatter.resolvedOptions());
    }
}

// The far side: nothing above was kept by the engine, only by these arrays, and every answer has to
// still be the one the narrow renderer gave.
for (const [index, text] of retainedNarrowFormats.entries()) {
    assert.sameValue(text, narrowAnswers[Math.floor(index / 100)], "a retained narrow string is still the name it was: " + text);
}

for (const parts of retainedNarrowParts) {
    assert.sameValue(Array.isArray(parts), true);
    assert.sameValue(parts.length > 1, true);
    for (const part of parts) {
        assert.sameValue(typeof part.type, "string");
        assert.sameValue(typeof part.value, "string");
    }
}

for (const [index, text] of retainedNarrowRanges.entries()) {
    assert.sameValue(typeof text, "string");
    assert.sameValue(text.length > 0, true);

    // The era bag renders nothing of the two ends that differs, so 11.5.8 steps 13 to 16 answer it
    // with one date; the other two render the month and the day, which the two ends do not share.
    const bag = Math.floor(index / 100);
    assert.sameValue(bag === 2 ? text === narrowAnswers[2] : text.includes("\u2013"), true, "the range of a retained narrow format: " + text);
}

for (const resolved of retainedNarrowOptions) {
    assert.sameValue(typeof resolved, "object");
    assert.sameValue(resolved.locale, "en");
    assert.sameValue(resolved.calendar, "gregory");
}

for (const [index, formatter] of retainedNarrowInstances.entries()) {
    assert.sameValue(formatter.format(narrowDay), narrowAnswers[Math.floor(index / 100)], "read back through the instance itself");
}

for (const formatter of retainedNarrowInstances.slice(0, 3)) {
    assert.sameValue(typeof formatter.formatRange, "function");
}

// A narrow day period is refused at the constructor, and the refusal has to still be a refusal after
// the churn rather than an answer out of a renderer that was never built.
for (let i = 0; i < 50; i += 1) {
    churn();
    assert.throws(RangeError, () => new Intl.DateTimeFormat(["en"], { hour: "numeric", dayPeriod: "narrow", timeZone: "UTC" }));
}

// 18.1.1 and 18.3, Intl.RelativeTimeFormat. `format` runs 18.5.2, which reads the locale data, a
// decimal format and a plural rules out of caches - all of which allocate - before the pattern's
// interpolation is cut into the parts list and the string it answers, so the path allocates on both
// sides of the reading of [[Patterns]]. The numeric "auto" phrases of step 7 are single records read
// out of the same data, and their answer skips the number entirely.
const retainedYesterday = [];
const retainedTomorrow = [];
const retainedPast = [];
const retainedFuture = [];
const retainedRelativeParts = [];

for (let i = 0; i < 100; i++) {
    const formatter = new Intl.RelativeTimeFormat(["da"], { localeMatcher: "lookup" });

    assert.sameValue(formatter.resolvedOptions().locale, "da");
    assert.sameValue(formatter.resolvedOptions().style, "long");
    assert.sameValue(formatter.resolvedOptions().numeric, "always");

    // The two phrases of step 7 and the two patterns of step 10, and the parts lists both readings
    // cut, all kept.
    retainedYesterday.push(formatter.format(-1, "day"));
    retainedTomorrow.push(formatter.format(1, "day"));
    retainedPast.push(formatter.format(-3, "day"));
    retainedFuture.push(formatter.format(3, "day"));
    retainedRelativeParts.push(formatter.formatToParts(-3, "day"), formatter.formatToParts(3, "day"));
}

// Read back on the far side of a hundred more instances: the strings the moved blocks wrote, and the
// part lists whose records name the unit.
for (const group of [retainedYesterday, retainedTomorrow, retainedPast, retainedFuture]) {
    for (const text of group) {
        assert.sameValue(typeof text, "string");
        assert.sameValue(text.length > 0, true);
        assert.sameValue(text, group[0]);
    }
}

for (const parts of retainedRelativeParts) {
    assert.sameValue(Array.isArray(parts), true);
    assert.sameValue(parts.length > 1, true);
    for (const part of parts) {
        assert.sameValue(typeof part.type, "string");
        assert.sameValue(typeof part.value, "string");
    }
}

// 12.1.1 and 12.3.3, Intl.DisplayNames. It is not in the list above because 12.1.1 step 8's `type`
// has no default, so the shared loops cannot construct it without naming one; the shape of the
// window is the same, and the reading is a second one: 12.3.3's step 5 goes to the cache for the
// tables of [[Fields]] after the constructor has finished, so the instance is read again from a
// method that only holds what 9.2.8 left on it. A stale block here answers with the fallback code of
// step 7 rather than with the name, which is a wrong answer the assertion sees.
const retainedNameInstances = [];
const retainedNames = [];
const retainedNameOptions = [];

for (const type of ["language", "region", "script", "currency"]) {
    // One code per type, each of them a code of that type: 12.5.1 refuses "US" as a script and "DK"
    // as a currency, and a throw here would be the test's mistake rather than the engine's.
    const code = { language: "en", region: "US", script: "Latn", currency: "USD" }[type];

    for (let i = 0; i < 100; i++) {
        const displayNames = new Intl.DisplayNames(["da"], { type, localeMatcher: "lookup" });

        assert.sameValue(displayNames.resolvedOptions().locale, "da");
        assert.sameValue(displayNames.resolvedOptions().type, type);

        retainedNameInstances.push(displayNames);
        retainedNames.push(displayNames.of(code));
        retainedNameOptions.push(displayNames.resolvedOptions());
    }
}

// The far side: the instances, the names they read out of the cache and the options objects built
// while they were live.
for (const instance of retainedNameInstances) {
    assert.sameValue(typeof instance.of, "function");
    assert.sameValue(typeof instance.resolvedOptions, "function");
}

for (const name of retainedNames) {
    assert.sameValue(typeof name, "string");
    assert.sameValue(name.length > 1, true);
}

for (const resolved of retainedNameOptions) {
    assert.sameValue(resolved.locale, "da");
    assert.sameValue(["language", "region", "script", "currency"].includes(resolved.type), true);
}

// The tables are shared per locale and `style`, so the same name read through a hundred instances is
// one build of the data; the reading is what had to survive, and `style` is the option that changes
// which table is asked, which is a second key into the same cache.
const retainedByStyle = [];
for (const style of ["long", "short", "narrow"]) {
    for (let i = 0; i < 100; i++) {
        retainedByStyle.push(new Intl.DisplayNames(["it"], { type: "language", style }).of("en"));
    }
}
for (const name of retainedByStyle) {
    assert.sameValue(name, "inglese");
}

// 12.2.2 over the tags a program can ask for, with and without a Unicode locale extension sequence:
// the list is built out of freshly allocated strings while every one of the requests is read.
const dnTags = [];
for (const language of ["da", "it", "en", "de", "sv", "nb"]) {
    dnTags.push(language);
    for (const region of ["DK", "IT", "US", "DE"]) {
        dnTags.push(`${language}-${region}`);
        dnTags.push(`${language}-${region}-u-nu-latn-ca-gregory`);
    }
}

const dnSupported = [];
const dnResolved = [];
for (const matcher of ["lookup", "best fit"]) {
    for (const tag of dnTags) {
        dnSupported.push(Intl.DisplayNames.supportedLocalesOf([tag], { localeMatcher: matcher }));
        dnResolved.push(new Intl.DisplayNames([tag], { type: "region", localeMatcher: matcher }).resolvedOptions().locale);
    }
}

assert.sameValue(dnSupported.length, dnTags.length * 2);
assert.sameValue(dnResolved.length, dnTags.length * 2);
for (const tags of dnSupported) {
    assert.sameValue(Array.isArray(tags), true);
    for (const tag of tags) {
        assert.sameValue(typeof tag, "string");
    }
}
for (const locale of dnResolved) {
    // A tag this build has data for is reported as it was asked, region and all; anything else
    // resolves to the fallback locale, which is "en".
    assert.sameValue(["da", "en", "it"].includes(locale.split("-")[0]), true);
}

// 15.1.1 and 15.3, Intl.Locale. The instance exists from step 6 and [[Locale]] is not written until
// step 43, and everything between them allocates: the tag coercion of steps 8 and 9, the option reads
// of 15.1.2 and 15.1.1, and the seven keys of 15.1.3. The getters below churn the heap at each of
// those reads, which is the window 642c11d7 and c996cdaf closed for the four services.

// There is no `gc` in this harness, so a collection is provoked by allocation: fresh strings, kept
// alive so that the compaction runs against a live set. `BS_GC_STRESS` does the same far more often;
// this is what makes the plain run reach the same points.
function churn() {
    const junk = [];
    for (let i = 0; i < 64; i += 1) {
        junk.push(("locale" + i).repeat(8));
    }
    return junk;
}

const localeOverrides = {
    language: "en",
    script: "Latn",
    region: "US",
    variants: "fonipa",
    calendar: "gregory",
    collation: "standard",
    firstDayOfWeek: "mon",
    hourCycle: "h23",
    caseFirst: "upper",
    numeric: true,
    numberingSystem: "latn",
};

// The tag the overrides above leave: UpdateLanguageId rebuilds the base name from language, script,
// region and variants in that order, and InsertUnicodeExtensionAndCanonicalize writes the seven keys
// of 15.2.2 in the sorted order of the extension - with `kn` as the bare keyword, which is what
// CanonicalizeUValue leaves of a numeric `true`.
const localeTag =
    "en-Latn-US-fonipa-u-ca-gregory-co-standard-fw-mon-hc-h23-kf-upper-kn-nu-latn";

function mkLocaleOptions() {
    let seen = null;
    const bag = {};
    for (const key of Object.keys(localeOverrides)) {
        Object.defineProperty(bag, key, {
            configurable: true,
            enumerable: true,
            get() {
                seen = this;
                churn();
                return localeOverrides[key];
            },
        });
    }
    return { bag, get seen() { return seen; } };
}

// Every accessor of 15.3.2 to 15.3.14, read out of the one tag above.
function checkLocaleAccessors(locale) {
    assert.sameValue(locale.toString(), localeTag);
    assert.sameValue(String(locale), localeTag);
    assert.sameValue(locale[Symbol.toStringTag], "Intl.Locale");
    assert.sameValue(locale.baseName, "en-Latn-US-fonipa");
    assert.sameValue(locale.language, "en");
    assert.sameValue(locale.script, "Latn");
    assert.sameValue(locale.region, "US");
    assert.sameValue(locale.variants, "fonipa");
    assert.sameValue(locale.calendar, "gregory");
    assert.sameValue(locale.collation, "standard");
    assert.sameValue(locale.firstDayOfWeek, "mon");
    assert.sameValue(locale.hourCycle, "h23");
    assert.sameValue(locale.caseFirst, "upper");
    assert.sameValue(locale.numeric, true);
    assert.sameValue(locale.numberingSystem, "latn");
}

const retainedLocales = [];
const retainedPlainLocales = [];
const retainedTextInfo = [];
const retainedWeekInfo = [];
const retainedLocaleLists = [];

for (let i = 0; i < 100; i++) {
    // The options bag itself, and a Proxy over it, which is the same reading of 15.1.1 steps 14 to 34
    // through 9.2.1's ordinary Get.
    const options = mkLocaleOptions();
    const bag = i % 2 === 0 ? options.bag : new Proxy(options.bag, {});
    const locale = new Intl.Locale("en-GB", bag);

    // The getter's receiver is the object `Get` was called on, which is the Proxy of the odd
    // iterations, and it is what makes the read below observable at all.
    assert.sameValue(options.seen, bag);
    checkLocaleAccessors(locale);

    retainedLocales.push(locale);

    // 15.3.9 and 15.3.10 construct a new instance out of this one's tag, while the receiver is still
    // on the stack.
    const maximal = locale.maximize();
    const minimal = locale.minimize();
    assert.sameValue(typeof maximal.toString(), "string");
    assert.sameValue(typeof minimal.toString(), "string");
    retainedPlainLocales.push(maximal, minimal);

    // 15.3.16 to 15.3.22, the other three windows: an object created and then written to, and the
    // array made out of a list of freshly allocated strings.
    retainedTextInfo.push(locale.getTextInfo());
    retainedWeekInfo.push(locale.getWeekInfo());
    retainedLocaleLists.push(locale.getCalendars(), locale.getCollations(),
                             locale.getHourCycles(), locale.getNumberingSystems(),
                             locale.getTimeZones());
}

for (const locale of [retainedLocales[0], retainedPlainLocales[0], retainedPlainLocales[1]]) {
    assert.sameValue(Object.getPrototypeOf(locale), Intl.Locale.prototype);
}

// A subclass: 15.1.1 step 6 creates through `newTarget`, so the instance is the subclass's, while 6
// and 15.3.10 step 4 construct a plain `%Intl.Locale%` from a tag this engine wrote.
class SubLocale extends Intl.Locale {}
for (let i = 0; i < 10; i++) {
    const sub = new SubLocale("en-GB", mkLocaleOptions().bag);
    assert.sameValue(Object.getPrototypeOf(sub), SubLocale.prototype);
    checkLocaleAccessors(sub);

    // The two are made while the receiver is on the stack and are not of its class.
    for (const derived of [sub.maximize(), sub.minimize()]) {
        assert.sameValue(Object.getPrototypeOf(derived), Intl.Locale.prototype);
        assert.sameValue(derived instanceof SubLocale, false);
    }
}

// 9.2.1 step 7.3.3 and 15.1.1 step 8: a Locale read where a tag is asked for, so that [[Locale]] is
// copied out of one instance and into another while both are live.
for (let i = 0; i < 20; i++) {
    const source = retainedLocales[0];

    // The tag argument path, which is the slot rather than a `toString` of it.
    const byTag = new Intl.Locale(source, mkLocaleOptions().bag);
    assert.sameValue(byTag.toString(), localeTag);

    // Step 9's coercion of an object that is not an initialized Locale.
    const byToString = new Intl.Locale({
        toString() {
            churn();
            return "en-GB";
        },
    });
    assert.sameValue(byToString.toString(), "en-GB");
    retainedPlainLocales.push(byToString, byTag);
}

// The far side of all of it: the instances were made a hundred constructions ago, and the tag they
// report has to still be the tag they were given.
for (const locale of retainedLocales) {
    assert.sameValue(locale instanceof Intl.Locale, true);
    checkLocaleAccessors(locale);

    // 15.1.1 step 8 and 9.2.1 step 7.3.3 of a retained instance: the slot read back as a tag.
    assert.sameValue(Intl.getCanonicalLocales(locale)[0], locale.toString());
}

for (const locale of retainedPlainLocales) {
    assert.sameValue(typeof locale.toString(), "string");
    assert.sameValue(locale.language.length > 0, true);
}

for (const info of retainedTextInfo) {
    assert.sameValue(typeof info, "object");
    assert.sameValue(info.direction, "ltr");
}

for (const info of retainedWeekInfo) {
    assert.sameValue(typeof info, "object");
    assert.sameValue(typeof info.firstDay, "number");
    assert.sameValue(Array.isArray(info.weekend), true);
    assert.sameValue(info.weekend.length > 0, true);
}

for (const list of retainedLocaleLists) {
    if (list === undefined) {
        // 15.5.13 answers undefined for a tag that names no region.
        continue;
    }

    assert.sameValue(Array.isArray(list), true);
    assert.sameValue(list.length > 0, true);
    for (const text of list) {
        assert.sameValue(typeof text, "string");
    }
}
// Intl.Segmenter is the member whose instance does not answer a string but hands back another
// object, and that object - 19.5.1's Segments - carries two GC blocks of its own: the boundary
// table and, for word granularity, one byte per segment for 19.7.1 step 10. Both are written after
// the segmentation allocates, and the strings of the segment data objects are substrings made after
// the table, so every answer below reads a block that has been through several collections since.
const segmentedText = "Segmenting the same sentence keeps the same table.";
const retainedSegments = [];

for (let i = 0; i < 200; i++) {
    const segmenter = new Intl.Segmenter(["en"], { localeMatcher: "lookup", granularity: "word" });
    assert.sameValue(segmenter.resolvedOptions().granularity, "word");
    retainedSegments.push(segmenter.segment(segmentedText));
}

for (const segments of retainedSegments) {
    const list = [...segments];
    assert.sameValue(list.length > 3, true);
    assert.sameValue(list[0].segment, "Segmenting");
    assert.sameValue(list[0].input, segmentedText);
    assert.sameValue(segments.containing(0).segment, "Segmenting");

    // The table is what both answers came from, so its last entry has to be the length the string
    // still has, and every entry in between has to still tile the string without a gap.
    assert.sameValue(list.at(-1).index + list.at(-1).segment.length, segmentedText.length);
    let cursor = 0;
    for (const segment of list) {
        assert.sameValue(segment.index, cursor);
        assert.sameValue(segmentedText.startsWith(segment.segment, cursor), true);
        assert.sameValue(typeof segment.isWordLike, "boolean");
        cursor += segment.segment.length;
    }

    assert.sameValue(cursor, segmentedText.length);
}

// An iterator kept half-way through is the case the design of 19.5.1 is about: the cursor is a code
// unit index into the same table, so resuming it after two hundred more strings have been segmented
// reads the boundaries the Segments object was made with, and nothing else.
const retainedIterators = [];

for (let i = 0; i < 200; i++) {
    const iterator = new Intl.Segmenter(["it"], { granularity: "grapheme" })
        .segment("world")[Symbol.iterator]();
    assert.sameValue(iterator.next().value.segment, "w");
    retainedIterators.push(iterator);
}

for (const iterator of retainedIterators) {
    const result = iterator.next();
    assert.sameValue(result.done, false);
    assert.sameValue(result.value.segment, "o");
    assert.sameValue(result.value.index, 1);
    assert.sameValue(result.value.input, "world");

    const rest = [...iterator].map(segment => segment.segment);
    assert.compareArray(rest, ["r", "l", "d"]);
    assert.sameValue(iterator.next().done, true);
}
