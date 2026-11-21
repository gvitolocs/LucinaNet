/*---
description: Intl.NumberFormat with style "currency", the first half of ECMA-402 16.1.3 and the
  patterns of 16.5.3 that read a currency. 16.1.1 step 13 takes the amount's precision from CLDR,
  16.3.2 reports the three currency rows of Table 28, and the symbol, the code and the name are the
  three spellings 16.1.3 step 6 allows.
---*/

const NBSP = " ";

// The call example-site.com makes, and the one that decided this work: a price is formatted as a currency
// or the page renders nothing at all.
const usd = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });
assert.sameValue(usd.format(1234.5), "$1,234.50");
assert.sameValue(usd.format(-5), "-$5.00");
assert.sameValue(usd.format(0), "$0.00");
assert.sameValue(usd.format(1234567.891), "$1,234,567.89");
assert.sameValue(usd.format(0.05), "$0.05");

// 16.1.3 steps 1 and 2 with the currency style, then step 12: the three slots the style is read
// through. `resolvedOptions` reports them in the order Table 28 lists them.
const resolved = usd.resolvedOptions();
assert.sameValue(resolved.locale, "en-US");
assert.sameValue(resolved.style, "currency");
assert.sameValue(resolved.currency, "USD");
assert.sameValue(resolved.currencyDisplay, "symbol");
assert.sameValue(resolved.currencySign, "standard");

// 16.1.1 step 13: the two fraction-digit defaults are the currency's own precision, which for the
// dollar is the two digits of its cents.
assert.sameValue(resolved.minimumFractionDigits, 2);
assert.sameValue(resolved.maximumFractionDigits, 2);

// 16.1.3 step 5.1 and step 12.1: a code is checked as three letters and stored uppercased, so a
// program that spells it the way a keyboard types it still gets the same format.
assert.sameValue(new Intl.NumberFormat("en", { style: "currency", currency: "usd" }).resolvedOptions().currency, "USD");
assert.throws(TypeError, () => new Intl.NumberFormat("en", { style: "currency" }), "step 4.1: the style is read without a currency");
assert.throws(TypeError, () => new Intl.NumberFormat("en", { style: "currency", currency: undefined }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: "currency", currency: "US" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: "currency", currency: "USDD" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: "currency", currency: "US1" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: "currency", currency: "US$" }));
// Step 5.1 is not conditioned on the style: a code is well formed or it is not, whatever the style
// that came with it.
assert.throws(RangeError, () => new Intl.NumberFormat("en", { currency: "US" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: "decimal", currency: "XYZ1" }));
// A well-formed code with another style is read and thrown away: 16.1.3 step 12 only writes the
// three slots for the style that formats with them, and Table 28 has no row to report.
const decimalWithCode = new Intl.NumberFormat("en", { style: "decimal", currency: "USD" });
assert.sameValue(decimalWithCode.format(1234.5), "1,234.5");
assert.sameValue(decimalWithCode.resolvedOptions().currency, undefined);
assert.sameValue(decimalWithCode.resolvedOptions().currencyDisplay, undefined);
assert.sameValue(decimalWithCode.resolvedOptions().currencySign, undefined);

// Steps 6 and 7 are the two lists GetOption checks, each with its own default.
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: "currency", currency: "USD", currencyDisplay: "sign" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: "currency", currency: "USD", currencySign: "account" }));
assert.throws(RangeError, () => new Intl.NumberFormat("en", { style: "currency", currency: "USD", signDisplay: "sometimes" }));

// 16.5.1 CurrencyDigits, through the two defaults of 16.1.1 step 13: the yen has no minor unit, and
// the Bahraini dinar has three digits of it. The table is locale-free, so every locale reads the
// same digits for the same code.
const jpy = new Intl.NumberFormat("en", { style: "currency", currency: "JPY" });
assert.sameValue(jpy.resolvedOptions().minimumFractionDigits, 0);
assert.sameValue(jpy.resolvedOptions().maximumFractionDigits, 0);
assert.sameValue(jpy.format(1234.5), "¥1,235");
assert.sameValue(jpy.format(-5), "-¥5");
assert.sameValue(new Intl.NumberFormat("da", { style: "currency", currency: "JPY" }).format(1234.5), "1.235" + NBSP + "JP¥");
const bhd = new Intl.NumberFormat("en", { style: "currency", currency: "BHD" });
assert.sameValue(bhd.resolvedOptions().minimumFractionDigits, 3);
assert.sameValue(bhd.format(5), "BHD" + NBSP + "5.000");

// 16.1.2 still answers the digit options before the currency's own precision does, which is what
// makes a price of four decimals possible.
const wide = new Intl.NumberFormat("en", { style: "currency", currency: "USD", minimumFractionDigits: 4, maximumFractionDigits: 4 });
assert.sameValue(wide.format(1234.5), "$1,234.5000");
assert.sameValue(wide.resolvedOptions().minimumFractionDigits, 4);
const tight = new Intl.NumberFormat("en", { style: "currency", currency: "USD", maximumFractionDigits: 0 });
assert.sameValue(tight.format(1234.5), "$1,235");
assert.sameValue(tight.resolvedOptions().maximumFractionDigits, 0);
const rounded = new Intl.NumberFormat("en", { style: "currency", currency: "USD", roundingIncrement: 5 });
assert.sameValue(rounded.format(1234.51), "$1,234.50");

// 16.1.1 step 21 and 16.5.3: the grouping is the decimal one, under the currency pattern, and
// "min2" is the choice that leaves a four-digit integer ungrouped.
assert.sameValue(new Intl.NumberFormat("da", { style: "currency", currency: "USD", useGrouping: false }).format(1234.5), "1234,50" + NBSP + "US$");
assert.sameValue(new Intl.NumberFormat("en", { style: "currency", currency: "USD", useGrouping: "min2" }).format(1234.5), "$1234.50");
assert.sameValue(new Intl.NumberFormat("en", { style: "currency", currency: "USD", useGrouping: "auto" }).format(1234.5), "$1,234.50");

// currencyDisplay "code" (16.1.3 step 6.1): the three letters stand where the symbol would, and the
// pattern that goes with a string of letters is the one CLDR spells with a no-break space, which is
// what separates `USD 1,234.50` from the `$1,234.50` that needs none.
const asCode = new Intl.NumberFormat("en", { style: "currency", currency: "USD", currencyDisplay: "code" });
assert.sameValue(asCode.format(1234.5), "USD" + NBSP + "1,234.50");
assert.sameValue(asCode.format(-5), "-USD" + NBSP + "5.00");
assert.sameValue(
  asCode.formatToParts(1234.5).map((part) => part.type + ":" + part.value).join("|"),
  "currency:USD|literal:" + NBSP + "|integer:1|group:,|integer:234|decimal:.|fraction:50"
);
// The code is three letters, so the Danish pattern puts it after the number like the symbol.
assert.sameValue(new Intl.NumberFormat("da", { style: "currency", currency: "USD", currencyDisplay: "code" }).format(1234.5), "1.234,50" + NBSP + "USD");

// currencyDisplay "narrowSymbol": the short form of the symbol, which in English is the same sign
// and in Danish is the dollar sign without the country.
assert.sameValue(new Intl.NumberFormat("en", { style: "currency", currency: "USD", currencyDisplay: "narrowSymbol" }).format(1234.5), "$1,234.50");
assert.sameValue(new Intl.NumberFormat("da", { style: "currency", currency: "USD", currencyDisplay: "narrowSymbol" }).format(1234.5), "1.234,50" + NBSP + "$");

// currencyDisplay "name": 16.5.3 step 9.11 reads the string and the pattern of the name together,
// both chosen by the plural category of the number that is shown - which is why the two amounts
// below differ by the fraction digits and not by the value.
const asName = new Intl.NumberFormat("en", { style: "currency", currency: "USD", currencyDisplay: "name" });
assert.sameValue(asName.format(1234.5), "1,234.50 US dollars");
assert.sameValue(asName.format(-5), "-5.00 US dollars");
assert.sameValue(asName.format(1), "1.00 US dollars");
assert.sameValue(
  new Intl.NumberFormat("en", { style: "currency", currency: "USD", currencyDisplay: "name", maximumFractionDigits: 0, minimumFractionDigits: 0 }).format(1),
  "1 US dollar"
);
assert.sameValue(
  asName.formatToParts(1234.5).map((part) => part.type + ":" + part.value).join("|"),
  "integer:1|group:,|integer:234|decimal:.|fraction:50|literal: |currency:US dollars"
);
// The name of the Danish krone is what Danish calls it, and the Italian name of the US dollar is
// what Italian calls it: both from `CurrencyExtendedDataV1`, in the locale's own language.
assert.sameValue(new Intl.NumberFormat("da", { style: "currency", currency: "USD", currencyDisplay: "name" }).format(1234.5), "1.234,50 amerikanske dollar");
assert.sameValue(new Intl.NumberFormat("it", { style: "currency", currency: "USD", currencyDisplay: "name" }).format(1234.5), "1234,50 dollari statunitensi");
// A code the name data has nothing for is named by itself, in the same pattern (16.5.3 step 9.11).
assert.sameValue(new Intl.NumberFormat("en", { style: "currency", currency: "XYZ", currencyDisplay: "name" }).format(1234.5), "1,234.50 XYZ");

// currencySign "accounting": the other set of patterns of 16.5.10, which puts a negative amount
// between parentheses. The parentheses come from the pattern, so no minusSign part is left to
// report - and where a locale has no accounting negative pattern of its own, Danish among them, the
// standard minus stands.
const accounting = new Intl.NumberFormat("en", { style: "currency", currency: "USD", currencySign: "accounting" });
assert.sameValue(accounting.format(-5), "($5.00)");
assert.sameValue(accounting.format(1234.5), "$1,234.50");
assert.sameValue(
  accounting.formatToParts(-5).map((part) => part.type + ":" + part.value).join("|"),
  "literal:(|currency:$|integer:5|decimal:.|fraction:00|literal:)"
);
assert.sameValue(new Intl.NumberFormat("da", { style: "currency", currency: "USD", currencySign: "accounting" }).format(-5), "-5,00" + NBSP + "US$");

// 16.5.10 steps 9 to 12 and 16.5.3 step 4: signDisplay over a currency. The sign is written outside
// a pattern that has no room for it, and a negative zero keeps its own.
const always = new Intl.NumberFormat("en", { style: "currency", currency: "USD", signDisplay: "always" });
assert.sameValue(always.format(1234.5), "+$1,234.50");
assert.sameValue(always.format(-5), "-$5.00");
assert.sameValue(new Intl.NumberFormat("en", { style: "currency", currency: "USD", signDisplay: "negative" }).format(5), "$5.00");
assert.sameValue(new Intl.NumberFormat("en", { style: "currency", currency: "USD", signDisplay: "never" }).format(-5), "$5.00");

// The parts of a currency format, for the symbol: 16.5.3 names the sign, the string and every piece
// of the number it puts them around.
assert.sameValue(
  usd.formatToParts(-5).map((part) => part.type + ":" + part.value).join("|"),
  "minusSign:-|currency:$|integer:5|decimal:.|fraction:00"
);
assert.sameValue(
  usd.formatToParts(1234.5).map((part) => part.type + ":" + part.value).join("|"),
  "currency:$|integer:1|group:,|integer:234|decimal:.|fraction:50"
);
// The parts concatenate to the string, which is what makes the two views of an amount the same
// amount.
assert.sameValue(usd.formatToParts(-1234.56).map((part) => part.value).join(""), usd.format(-1234.56));

// A suffix pattern, as Danish has it: the currency string comes after the number, and the sign that
// the pattern has no placeholder for is put in front of everything (UTS #35 section 3.2.1, which
// 16.5.10 leaves to the data).
const danish = new Intl.NumberFormat("da", { style: "currency", currency: "USD" });
assert.sameValue(danish.format(1234.5), "1.234,50" + NBSP + "US$");
assert.sameValue(danish.format(-5), "-5,00" + NBSP + "US$");
assert.sameValue(
  danish.formatToParts(-5).map((part) => part.type + ":" + part.value).join("|"),
  "minusSign:-|integer:5|decimal:,|fraction:00|literal:" + NBSP + "|currency:US$"
);

// Italian, whose symbol data has no row of its own for the US dollar: the root answer of the CLDR
// this build's tables were made from stands, which is the same string Danish uses. Its own digits
// and its own minimum grouping of four digits are Italian's.
const italian = new Intl.NumberFormat("it", { style: "currency", currency: "USD" });
assert.sameValue(italian.format(1234.5), "1234,50" + NBSP + "US$");
assert.sameValue(italian.format(1234567.891), "1.234.567,89" + NBSP + "US$");
assert.sameValue(new Intl.NumberFormat("it", { style: "currency", currency: "EUR" }).format(1234.5), "1234,50" + NBSP + "€");
assert.sameValue(new Intl.NumberFormat("it", { style: "currency", currency: "DKK" }).format(1234.5), "1234,50" + NBSP + "DKK");

// 16.5.3 steps 2 to 4: a special value goes where the pattern puts the number, with the sign of its
// category in front of it.
assert.sameValue(usd.format(NaN), "$NaN");
assert.sameValue(usd.format(Infinity), "$" + "∞");
assert.sameValue(usd.format(-Infinity), "-$" + "∞");
assert.sameValue(
  usd.formatToParts(-Infinity).map((part) => part.type + ":" + part.value).join("|"),
  "minusSign:-|currency:$|infinity:∞"
);
assert.sameValue(danish.format(NaN), "NaN" + NBSP + "US$");

// A code the symbol data has nothing for is not an error: 16.5.3 step 9.11 and 16.5.10 step 5 leave
// the pattern of the locale standing with the code in the place of the string that is missing.
const unknown = new Intl.NumberFormat("en", { style: "currency", currency: "XYZ" });
assert.sameValue(unknown.format(1234.5), "XYZ" + NBSP + "1,234.50");
assert.sameValue(unknown.resolvedOptions().currency, "XYZ");

// 20.2.1: `toLocaleString` builds a NumberFormat from its own arguments, so a price on a page goes
// through the same code as the constructor above.
assert.sameValue((1234.5).toLocaleString("en-US", { style: "currency", currency: "USD" }), "$1,234.50");
assert.sameValue((-5).toLocaleString("da", { style: "currency", currency: "USD" }), "-5,00" + NBSP + "US$");

// 16.4: an instance carries its resolved options and nothing else, so two formats of the same
// currency are two objects that behave the same and share whatever the engine keeps per context.
const other = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });
assert.sameValue(usd !== other, true);
assert.sameValue(JSON.stringify(usd.resolvedOptions()), JSON.stringify(other.resolvedOptions()));
assert.sameValue(other.format(1234.5), usd.format(1234.5));
