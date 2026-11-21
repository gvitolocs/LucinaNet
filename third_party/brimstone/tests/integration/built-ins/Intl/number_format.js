/*---
description: Intl.NumberFormat, the decimal and percent half of ECMA-402 clause 16. 16.1.1 reads the
  options, 16.3 is what an instance does, and 20.2.1 and 20.5.1 are the built-ins that route through
  it. The currency style has its own file, number_format_currency.js; a unit and the three
  non-standard notations are refused rather than faked, as number_format_core.rs explains.
---*/

const NBSP = " ";

// 16.2: the constructor itself. `length` is 0 because both parameters of 16.1.1's signature are
// optional, and `name` is the constructor's own.
assert.sameValue(typeof Intl.NumberFormat, "function");
assert.sameValue(Intl.NumberFormat.name, "NumberFormat");
assert.sameValue(Intl.NumberFormat.length, 0);

const proto = Intl.NumberFormat.prototype;
assert.sameValue(Object.getPrototypeOf(Intl.NumberFormat), Function.prototype);
assert.sameValue(Object.getPrototypeOf(proto), Object.prototype);
assert.sameValue(Intl.NumberFormat.prototype.constructor, Intl.NumberFormat);

// 16.2.2, and 9.2.9 step 5's answer in the order the caller asked for. An unsupported tag is left
// out and `lookup` is the only matcher 9.2.9 step 3 distinguishes.
assert.compareArray(Intl.NumberFormat.supportedLocalesOf(["en-US", "da-DK", "xx-YY"]), ["en-US", "da-DK"]);
assert.compareArray(Intl.NumberFormat.supportedLocalesOf("it"), ["it"]);
assert.compareArray(Intl.NumberFormat.supportedLocalesOf(["en-US-u-nu-latn"]), ["en-US-u-nu-latn"]);
// A locale is supported on the strength of its language subtag: 9.2.9 step 4 filters the requested
// tags with LookupMatchingLocaleByPrefix (9.2.3), which strips the -u- extension before matching. The
// unsupported numbering system is only dropped later, by 9.2.7 step 12.7, which is what the
// `numberingSystem` option of the 16.3.2 block below shows.
assert.compareArray(Intl.NumberFormat.supportedLocalesOf(["en-US-u-nu-arab"]), ["en-US-u-nu-arab"]);
assert.throws(TypeError, () => Intl.NumberFormat.supportedLocalesOf([1]));
assert.throws(RangeError, () => Intl.NumberFormat.supportedLocalesOf(["not a locale!"]));

// 16.3: what the prototype holds. `format` is an accessor, not a method (16.3.3),
// `formatRange` and `formatRangeToParts` are absent (16.3.4, 16.3.5), and 16.3.7's tag is a String.
assert.sameValue(typeof Object.getOwnPropertyDescriptor(proto, "format").get, "function");
assert.sameValue(Object.getOwnPropertyDescriptor(proto, "format").set, undefined);
assert.sameValue(proto.formatRange, undefined);
assert.sameValue(proto.formatRangeToParts, undefined);
assert.sameValue(proto[Symbol.toStringTag], "Intl.NumberFormat");
assert.sameValue(Object.prototype.toString.call(new Intl.NumberFormat()), "[object Intl.NumberFormat]");

// 16.1.1 steps 1 and 2, through the call mode of 4.3 Note 1: called without `new`, the constructor
// builds from the active function object anyway, and 16.1.1.1 returns a working instance.
const called = Intl.NumberFormat("en");
assert.sameValue(typeof called.format, "function");
assert.sameValue(called.format(1234.5), "1,234.5");

// 16.1.1.1 step 1: `Object.create`d, then called on the result, the instance lands on that object
// under %Intl%.[[FallbackSymbol]], and 16.5.9 UnwrapNumberFormat is what lets 16.3.2 and 16.3.3 read
// it back.
const legacy = Object.create(Intl.NumberFormat.prototype);
assert.sameValue(Intl.NumberFormat.call(legacy, "en"), legacy);
assert.sameValue(legacy.format(1234.5), "1,234.5");
assert.sameValue(legacy.resolvedOptions().locale, "en");

// 16.5.15 and 16.5.5 for the default `decimal` style: 16.1.1 steps 14 and 15 give 0 to 3 fraction
// digits, and the data of 16.5.10 says where the separators go.
const en = new Intl.NumberFormat("en-US");
assert.sameValue(en.format(1234567.891), "1,234,567.891");
assert.sameValue(en.format(0), "0");
assert.sameValue(en.format(-1234.5), "-1,234.5");
assert.sameValue(en.format(1234567), "1,234,567");
assert.sameValue(en.format(999), "999");
assert.sameValue(en.format(1000), "1,000");

// The same three digits, in the other two built-in locales. Danish groups with a period and writes
// its decimal separator as a comma; Italian does the same. Both come from DecimalSymbolsV1.
assert.sameValue(new Intl.NumberFormat("da").format(1234567.891), "1.234.567,891");
assert.sameValue(new Intl.NumberFormat("it").format(1234567.891), "1.234.567,891");

// A locale the build has no data for falls back to the default one (9.2.7 step 4) rather than
// throwing. A region of a supported language formats on that language's data, but the reported tag
// is the request itself: 9.2.4's best-fit matcher is implementation-defined, and this one echoes the
// canonical tag the caller named, as V8 and SpiderMonkey do, so "en-AU" reports "en-AU" while still
// formatting as English off the "en" tables.
assert.sameValue(new Intl.NumberFormat("xx").format(1234.5), "1,234.5");
assert.sameValue(new Intl.NumberFormat("en-AU").resolvedOptions().locale, "en-AU");
assert.sameValue(new Intl.NumberFormat("en-AU").format(1234.5), "1,234.5");

// The NaN and infinity words, 16.5.3 steps 2 and 3, and the sign of 16.5.10.
assert.sameValue(en.format(NaN), "NaN");
assert.sameValue(en.format(Infinity), "∞");
assert.sameValue(en.format(-Infinity), "-∞");

// 16.1.1 steps 25 and 26: signDisplay, whose "auto" hides the sign of a positive value and of -0.
assert.sameValue(new Intl.NumberFormat("en", { signDisplay: "always" }).format(0), "+0");
assert.sameValue(new Intl.NumberFormat("en", { signDisplay: "auto" }).format(-0), "-0");
assert.sameValue(new Intl.NumberFormat("en", { signDisplay: "always" }).format(-0), "-0");
assert.sameValue(new Intl.NumberFormat("en", { signDisplay: "never" }).format(-1), "1");
assert.sameValue(new Intl.NumberFormat("en", { signDisplay: "exceptZero" }).format(0), "0");
assert.sameValue(new Intl.NumberFormat("en", { signDisplay: "exceptZero" }).format(1), "+1");
assert.sameValue(new Intl.NumberFormat("en", { signDisplay: "negative" }).format(-1), "-1");
assert.sameValue(new Intl.NumberFormat("en", { signDisplay: "negative" }).format(1), "1");

// 16.1.1 steps 20 to 24, over 9.2.12: `useGrouping` takes a Boolean or one of five strings, and the
// three-way "auto"/"min2"/"always" difference is in the locale's own grouping rule.
assert.sameValue(new Intl.NumberFormat("en-US", { useGrouping: false }).format(1234567.891), "1234567.891");
assert.sameValue(new Intl.NumberFormat("en-US", { useGrouping: true }).format(1234567.891), "1,234,567.891");
// The difference between "auto" and "min2" is the minimum grouping digit count each spelling ends
// up with: "auto" takes the locale's own, which is 1 for both `en` and `da` in this build's data, so
// both group a four-digit number, while "min2" asks for 2 and neither does.
assert.sameValue(new Intl.NumberFormat("en-US", { useGrouping: "auto" }).format(1234.5), "1,234.5");
assert.sameValue(new Intl.NumberFormat("en-US", { useGrouping: "min2" }).format(1234.5), "1234.5");
assert.sameValue(new Intl.NumberFormat("en-US", { useGrouping: "min2" }).format(12345), "12,345");
assert.sameValue(new Intl.NumberFormat("en-US", { useGrouping: "always" }).format(1234.5), "1,234.5");
assert.sameValue(new Intl.NumberFormat("da", { useGrouping: "auto" }).format(1234.5), "1.234,5");
assert.sameValue(new Intl.NumberFormat("da", { useGrouping: "min2" }).format(1234.5), "1234,5");
assert.sameValue(new Intl.NumberFormat("da", { useGrouping: "always" }).format(1234.5), "1.234,5");
assert.sameValue(new Intl.NumberFormat("en-US", { useGrouping: "true" }).format(1234.5), "1,234.5");
// Step 21 accepts the Strings "true" and "false" as spellings of the Boolean and step 22 turns
// either back into the default, which is "auto", so the String "false" groups like the default while
// only the Boolean `false` of step 23's branch takes grouping off.
assert.sameValue(new Intl.NumberFormat("en-US", { useGrouping: "false" }).format(1234.5), "1,234.5");
assert.throws(RangeError, () => new Intl.NumberFormat("en", { useGrouping: "sometimes" }));

// 16.1.2 steps 21 and 22, the digit options. Step 22.1.3 is why a lone `minimumFractionDigits`
// raises the maximum to it instead of throwing, and step 22.1.5 is the RangeError for the reverse.
assert.sameValue(new Intl.NumberFormat("en", { maximumFractionDigits: 0 }).format(1234.5), "1,235");
assert.sameValue(new Intl.NumberFormat("en", { maximumFractionDigits: 2 }).format(1234567.891), "1,234,567.89");
assert.sameValue(new Intl.NumberFormat("en", { minimumFractionDigits: 5 }).format(1), "1.00000");
assert.sameValue(new Intl.NumberFormat("en", { minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(1), "1.00");
assert.throws(RangeError, () => new Intl.NumberFormat("en", { minimumFractionDigits: 3, maximumFractionDigits: 2 }));
assert.sameValue(new Intl.NumberFormat("en", { minimumIntegerDigits: 5 }).format(1234.5), "01,234.5");
assert.throws(RangeError, () => new Intl.NumberFormat("en", { maximumFractionDigits: 101 }));

// Step 21.1.1 and 21.1.2 read the pair, and step 26 makes it the rounding type. A lone minimum only
// guarantees digits: 21.1.2 defaults the maximum to 21, so nothing is rounded away and the value
// keeps its own five digits.
assert.sameValue(new Intl.NumberFormat("en", { minimumSignificantDigits: 4 }).format(1234.5), "1,234.5");
assert.sameValue(new Intl.NumberFormat("en", { maximumSignificantDigits: 3 }).format(1234.5), "1,230");
assert.sameValue(new Intl.NumberFormat("en", { minimumSignificantDigits: 3, maximumSignificantDigits: 3 }).format(0.012345), "0.0123");

// The nine rounding modes of 16.1.2 step 8, on the halfway value that tells them apart. The two
// columns differ because 16.5.16 and 16.5.17 turn the signed rounding mode into the unsigned one
// that fits the value's sign.
const modes = [
    ["ceil", "2", "-1"],
    ["floor", "1", "-2"],
    ["expand", "2", "-2"],
    ["trunc", "1", "-1"],
    ["halfCeil", "2", "-1"],
    ["halfFloor", "1", "-2"],
    ["halfExpand", "2", "-2"],
    ["halfTrunc", "1", "-1"],
    ["halfEven", "2", "-2"],
];

for (const [mode, positive, negative] of modes) {
    const format = new Intl.NumberFormat("en", { roundingMode: mode, maximumFractionDigits: 0 });
    assert.sameValue(format.format(1.5), positive, mode);
    assert.sameValue(format.format(-1.5), negative, mode);
}

assert.sameValue(new Intl.NumberFormat("en", { maximumFractionDigits: 0 }).format(0.5), "1");
assert.sameValue(new Intl.NumberFormat("en", { maximumFractionDigits: 0, roundingMode: "halfEven" }).format(0.5), "0");
assert.sameValue(new Intl.NumberFormat("en", { maximumFractionDigits: 0, roundingMode: "halfEven" }).format(1.5), "2");
assert.sameValue(new Intl.NumberFormat("en", { maximumFractionDigits: 0, roundingMode: "halfEven" }).format(2.5), "2");
assert.throws(RangeError, () => new Intl.NumberFormat("en", { roundingMode: "halfUp" }));

// Steps 6 and 7 allow only the fifteen listed increments, and step 12 drops the fraction-digit
// default to the minimum as soon as the increment is not 1, which step 28.2 then requires. An
// increment of 5 read at one fraction digit is a step of 0.5, so 7.31 goes to the nearer of 7.0 and
// 7.5 rather than to 7.3.
assert.sameValue(new Intl.NumberFormat("en", { roundingIncrement: 5, minimumFractionDigits: 1, maximumFractionDigits: 1 }).format(7.31), "7.5");
// 2500 with the fraction digits at their default 0 is a step of 2500 over the integer part, and the
// nearest multiple of it to 1.5 is 0. Step 12 is what makes this legal at all: it pulls the maximum
// default down to the minimum, so 28.2 has equal fraction digits to check. V8's ICU refuses the
// spelling instead, which is why the same step is also written with the digits pinned by hand.
assert.sameValue(new Intl.NumberFormat("en", { roundingIncrement: 2500 }).format(1.5), "0");
assert.sameValue(new Intl.NumberFormat("en", { roundingIncrement: 2500, minimumFractionDigits: 0, maximumFractionDigits: 0 }).format(2400), "2,500");
assert.throws(RangeError, () => new Intl.NumberFormat("en", { roundingIncrement: 3 }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { roundingIncrement: 0 }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { roundingIncrement: 5001 }));

// 16.1.1 steps 10 to 12 and 14 and 15: percent multiplies by 100 and takes 0 fraction digits as the
// default, and the pattern's spacing comes from the locale's own decimal pattern.
assert.sameValue(new Intl.NumberFormat("en", { style: "percent" }).format(0.25), "25%");
assert.sameValue(new Intl.NumberFormat("en", { style: "percent", maximumFractionDigits: 2 }).format(0.2567), "25.67%");
assert.sameValue(new Intl.NumberFormat("da", { style: "percent" }).format(0.25), "25" + NBSP + "%");
assert.sameValue(new Intl.NumberFormat("it", { style: "percent" }).format(0.25), "25%");
assert.sameValue(new Intl.NumberFormat("en", { style: "percent" }).format(-0.25), "-25%");

// 16.1.3 and 16.1.1 step 11: the styles and notations this build has no patterns for say so. The
// compact notation and the currency style have their own files now.
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: "unit", unit: "kilometer" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { notation: "scientific" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { notation: "engineering" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: 42 }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: "somethingelse" }));
assert.throws(TypeError, () => new Intl.NumberFormat("en", { style: Symbol("x") }));
assert.throws(TypeError, () => new Intl.NumberFormat(["en", 5]));
assert.throws(RangeError, () => new Intl.NumberFormat("en$"));

// 9.2.7 step 12.7: a numbering system the data has no digits for drops the locale that asked for it,
// so the answer is the default locale rather than an error. And a numbering system that is only what
// the locale already does - which is every system this build has - is not written back into the
// reported tag, since 9.2.7 step 15 returns the available-locale key it matched; the tag of
// `resolvedOptions` below keeps the "-u-nu-latn" the caller itself spelled.
assert.sameValue(new Intl.NumberFormat("en", { numberingSystem: "arab" }).resolvedOptions().locale, "en");
assert.sameValue(new Intl.NumberFormat("en", { numberingSystem: "latn" }).resolvedOptions().locale, "en");

// 16.3.2, over Table 28: what the instance resolved to, in the table's order. The two digit pairs
// only appear when 16.1.2 needed them, which is why the significant-digits format below reports no
// fraction digits at all.
const resolved = new Intl.NumberFormat("en-US", { useGrouping: false, maximumFractionDigits: 2 }).resolvedOptions();
assert.sameValue(resolved.locale, "en-US");
assert.sameValue(resolved.numberingSystem, "latn");
assert.sameValue(resolved.style, "decimal");
assert.sameValue(resolved.currency, undefined);
assert.sameValue(resolved.unit, undefined);
assert.sameValue(resolved.minimumIntegerDigits, 1);
assert.sameValue(resolved.minimumFractionDigits, 0);
assert.sameValue(resolved.maximumFractionDigits, 2);
assert.sameValue(resolved.minimumSignificantDigits, undefined);
assert.sameValue(resolved.useGrouping, false);
assert.sameValue(resolved.notation, "standard");
assert.sameValue(resolved.compactDisplay, undefined);
assert.sameValue(resolved.signDisplay, "auto");
assert.sameValue(resolved.roundingIncrement, 1);
assert.sameValue(resolved.roundingMode, "halfExpand");
assert.sameValue(resolved.roundingPriority, "auto");
assert.sameValue(resolved.trailingZeroDisplay, "auto");
assert.sameValue(Object.keys(resolved).includes("roundingType"), false);

assert.sameValue(new Intl.NumberFormat("en").resolvedOptions().useGrouping, "auto");
assert.sameValue(new Intl.NumberFormat("en", { useGrouping: true }).resolvedOptions().useGrouping, "always");
assert.sameValue(new Intl.NumberFormat("en", { useGrouping: "min2" }).resolvedOptions().useGrouping, "min2");
assert.sameValue(new Intl.NumberFormat("en", { style: "percent" }).resolvedOptions().maximumFractionDigits, 0);
assert.sameValue(new Intl.NumberFormat("en-u-nu-latn").resolvedOptions().locale, "en-u-nu-latn");
assert.sameValue(new Intl.NumberFormat("en", { roundingPriority: "morePrecision", minimumSignificantDigits: 3, maximumFractionDigits: 2 }).resolvedOptions().roundingPriority, "morePrecision");

// 16.3.3: the getter hands back one function per instance, with 16.3.3 step 4.1's length and name,
// and it is the captured instance rather than the `this` value that formats.
const nf = new Intl.NumberFormat("da");
assert.sameValue(nf.format, nf.format);
assert.sameValue(nf.format.length, 1);
assert.sameValue(nf.format.name, "");
assert.sameValue(nf.format(1234.5), "1.234,5");
assert.compareArray([1234.5, 1234.5].map(nf.format), ["1.234,5", "1.234,5"]);
const detached = nf.format;
assert.sameValue(detached(1234.5), "1.234,5");
assert.throws(TypeError, () => Object.getOwnPropertyDescriptor(proto, "format").get.call({}));
assert.throws(TypeError, () => proto.resolvedOptions.call({}));
assert.throws(TypeError, () => proto.formatToParts.call({}));

// 16.3.6, over 16.5.6: the parts are the string taken apart, with the type names 16.5.3 uses.
const parts = new Intl.NumberFormat("en").formatToParts(-1234.56);
// 16.5.4's grouping loop emits one "integer" part between each pair of "group" parts, so an integer
// the pattern splits does not come out as a single "integer" the way the string reads.
assert.compareArray(parts.map((part) => part.type), ["minusSign", "integer", "group", "integer", "decimal", "fraction"]);
assert.compareArray(parts.map((part) => part.value), ["-", "1", ",", "234", ".", "56"]);
assert.sameValue(parts.map((part) => part.value).join(""), new Intl.NumberFormat("en").format(-1234.56));
assert.compareArray(new Intl.NumberFormat("en", { style: "percent" }).formatToParts(-1.2).map((part) => part.type), ["minusSign", "integer", "percentSign"]);
assert.compareArray(new Intl.NumberFormat("en").formatToParts(NaN).map((part) => part.type), ["nan"]);
assert.compareArray(new Intl.NumberFormat("en").formatToParts(Infinity).map((part) => part.type), ["infinity"]);
assert.compareArray(new Intl.NumberFormat("da", { style: "percent" }).formatToParts(0.25).map((part) => part.type), ["integer", "literal", "percentSign"]);
assert.sameValue(new Intl.NumberFormat("en").formatToParts()[0].type, "nan");

// 16.5.15 reads a String argument through the same grammar a literal goes through, and a String that
// does not parse - whether it arrived as the argument or came out of ToPrimitive - is step 7's
// not-a-number rather than an error.
assert.sameValue(en.format("1234.5"), "1,234.5");
assert.sameValue(en.format("1e3"), "1,000");
assert.sameValue(en.format("1,234"), "NaN");
assert.sameValue(en.format({}), "NaN");
assert.sameValue(en.format(true), "1");

// A BigInt keeps all of its digits (16.5.15 step 2, over ℝ of the value), and so does a long
// decimal string.
assert.sameValue(new Intl.NumberFormat("en").format(12345678901234567890n), "12,345,678,901,234,567,890");
assert.sameValue(new Intl.NumberFormat("en", { maximumFractionDigits: 9 }).format("123456789012345678901234567890.123456789"), "123,456,789,012,345,678,901,234,567,890.123456789");
assert.sameValue(new Intl.NumberFormat("en").format("123456789012345678901234567890.123456789"), "123,456,789,012,345,678,901,234,567,890.123");
assert.sameValue(new Intl.NumberFormat("en", { maximumFractionDigits: 0 }).format(10n ** 20n), "100,000,000,000,000,000,000");

// 20.2.1 and 20.5.1: the built-ins that go through a NumberFormat built from their own arguments.
assert.sameValue((1234567.891).toLocaleString("en-US"), "1,234,567.891");
assert.sameValue((1234567.891).toLocaleString("da"), "1.234.567,891");
assert.sameValue((0.25).toLocaleString("en", { style: "percent" }), "25%");
assert.sameValue((1234567n).toLocaleString("en-US"), "1,234,567");
assert.sameValue([1234.5, 0.25].toLocaleString("en-US", { maximumFractionDigits: 1 }), "1,234.5,0.3");
assert.sameValue([1234.5].toLocaleString("da"), "1.234,5");
assert.sameValue(new Float64Array([1234.5, 2500]).toLocaleString("en-US", { maximumFractionDigits: 0 }), "1,235,2,500");
