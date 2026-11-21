/*---
description: Intl.Segmenter, ECMA-402 clause 19. 19.1.1 resolves the options, 19.3 is what an
  instance does, 19.5 and 19.6 are the Segments object and its iterator, and 19.7.1 and 19.8.1 are
  the segment data object and the boundary search, over the rule tables that brimstone_intl_data
  bakes for en, da and it. The build ships the three rule-based segmenters and no dictionary or LSTM
  model, so the complex-script case at the end is ICU4X's own measured fallback, not an invented one.
---*/

// 19.2: the constructor itself. `length` is 0 because both parameters of 19.1.1's signature are
// optional, and a call without `new` is refused by step 1 rather than treated as a function call.
assert.sameValue(typeof Intl.Segmenter, "function");
assert.sameValue(Intl.Segmenter.name, "Segmenter");
assert.sameValue(Intl.Segmenter.length, 0);
assert.sameValue(Object.getPrototypeOf(Intl.Segmenter), Function.prototype);
assert.throws(TypeError, () => Intl.Segmenter("en"));
assert.throws(TypeError, () => Intl.Segmenter.call(undefined, "en"));

// 19.2.1 and 8.2.10: `Intl.Segmenter` is a plain, writable data property of the Intl object.
const ownDescriptor = Object.getOwnPropertyDescriptor(Intl, "Segmenter");
assert.sameValue(ownDescriptor.writable, true);
assert.sameValue(ownDescriptor.enumerable, false);
assert.sameValue(ownDescriptor.configurable, true);
assert.sameValue(ownDescriptor.value, Intl.Segmenter);

// 19.3: the two methods of 19.3.2 and 19.3.3, with the length each signature has, and 19.3.4's tag.
const proto = Intl.Segmenter.prototype;
assert.sameValue(Object.getPrototypeOf(proto), Object.prototype);
assert.sameValue(proto.constructor, Intl.Segmenter);
assert.sameValue(typeof proto.resolvedOptions, "function");
assert.sameValue(proto.resolvedOptions.length, 0);
assert.sameValue(proto.resolvedOptions.name, "resolvedOptions");
assert.sameValue(typeof proto.segment, "function");
assert.sameValue(proto.segment.length, 1);
assert.sameValue(proto.segment.name, "segment");
assert.sameValue(proto[Symbol.toStringTag], "Intl.Segmenter");
assert.sameValue(Object.prototype.toString.call(new Intl.Segmenter("en")), "[object Intl.Segmenter]");

// 19.1.1 step 8: GetOption over « "grapheme", "word", "sentence" » with "grapheme" as the default,
// which is a case-sensitive string comparison and therefore a RangeError for "WORD".
assert.sameValue(new Intl.Segmenter("en").resolvedOptions().granularity, "grapheme");
assert.sameValue(new Intl.Segmenter("en", undefined).resolvedOptions().granularity, "grapheme");
assert.sameValue(new Intl.Segmenter("en", {}).resolvedOptions().granularity, "grapheme");
assert.sameValue(
  new Intl.Segmenter("en", { granularity: undefined }).resolvedOptions().granularity,
  "grapheme"
);
assert.throws(RangeError, () => new Intl.Segmenter("en", { granularity: "WORD" }));
assert.throws(RangeError, () => new Intl.Segmenter("en", { granularity: "morpheme" }));
assert.throws(RangeError, () => new Intl.Segmenter("en", { granularity: null }));
assert.throws(RangeError, () => new Intl.Segmenter("en", { granularity: false }));

// The option is read through ToString, so a value whose toString throws carries that error out,
// and a string primitive passed as the whole options object has no granularity to read at all.
assert.throws(RangeError, () => new Intl.Segmenter("en", {
  get granularity() { throw new RangeError("no granularity"); }
}));
assert.sameValue(new Intl.Segmenter("en", "word").resolvedOptions().granularity, "grapheme");

// 19.1.1 step 4 is ResolveOptions, which reads the locale list, and step 8 is the GetOption above,
// so an object that reports both says which of the two the constructor asked for first.
const readOrder = [];
const countedLocales = { get length() { readOrder.push("locales"); return 1; }, 0: "en" };
new Intl.Segmenter(countedLocales, {
  get granularity() { readOrder.push("granularity"); return "word"; }
});
assert.compareArray(readOrder, ["locales", "granularity"]);

// 19.1.1 steps 2 and 3: OrdinaryCreateFromConstructor, so a subclass gets its own prototype and the
// instance still carries the two slots that 19.4 lists.
class SubSegmenter extends Intl.Segmenter {}
const sub = new SubSegmenter("en", { granularity: "word" });
assert.sameValue(Object.getPrototypeOf(sub), SubSegmenter.prototype);
assert.sameValue(sub instanceof Intl.Segmenter, true);
assert.sameValue(sub.resolvedOptions().granularity, "word");
assert.compareArray([...sub.segment("a b")].map(s => s.segment), ["a", " ", "b"]);

// 19.3.2, over Table 34: exactly the two properties, `locale` first because the resolution came
// before the granularity in 19.1.1.
const resolvedKeys = Object.keys(new Intl.Segmenter("en", { granularity: "word" }).resolvedOptions());
assert.compareArray(resolvedKeys, ["locale", "granularity"]);
assert.sameValue(new Intl.Segmenter("en").resolvedOptions().locale, "en");
assert.sameValue(new Intl.Segmenter("da-DK").resolvedOptions().locale, "da-DK");
assert.sameValue(new Intl.Segmenter("it-IT").resolvedOptions().granularity, "grapheme");
assert.throws(TypeError, () => proto.resolvedOptions.call(Object.create(proto)));
assert.throws(TypeError, () => proto.resolvedOptions.call({}));

// A locale this build does not have is not an error and not a guess: 9.2.7 resolves it, and the
// fallback of the available-locale set is what an instance is built on.
assert.sameValue(new Intl.Segmenter("de").resolvedOptions().locale, "en");
assert.sameValue(new Intl.Segmenter("zh-CN").resolvedOptions().locale, "en");
assert.sameValue(new Intl.Segmenter([]).resolvedOptions().locale, "en");

// 19.2.2, which is 9.2.9 over 19.2.3's empty [[RelevantExtensionKeys]]: a locale is supported on
// the strength of its language subtag, and an unparseable tag is a RangeError before any matching.
assert.compareArray(Intl.Segmenter.supportedLocalesOf(["en-US", "da-DK", "xx-YY", "it-IT-x-priv"]),
  ["en-US", "da-DK", "it-IT-x-priv"]);
assert.compareArray(Intl.Segmenter.supportedLocalesOf("da"), ["da"]);
assert.compareArray(Intl.Segmenter.supportedLocalesOf([]), []);
assert.compareArray(Intl.Segmenter.supportedLocalesOf(["und"]), []);
assert.compareArray(Intl.Segmenter.supportedLocalesOf(["it"], { localeMatcher: "lookup" }), ["it"]);
assert.throws(RangeError, () => Intl.Segmenter.supportedLocalesOf(["x-y-z!"]));
assert.throws(TypeError, () => Intl.Segmenter.supportedLocalesOf([1]));
assert.throws(RangeError, () => Intl.Segmenter.supportedLocalesOf(["en"], { localeMatcher: "first match" }));

// 19.3.3: ToString of the argument, then CreateSegmentsObject (19.5.1). A string this build has no
// segmentation data for is still a string, and a Symbol is refused by ToString before any of it.
assert.throws(TypeError, () => proto.segment.call(Object.create(proto), "a"));
assert.throws(TypeError, () => new Intl.Segmenter("en").segment(Symbol("x")));
const graphemeOfNumber = [...new Intl.Segmenter("en").segment(12345)].map(s => s.segment);
assert.compareArray(graphemeOfNumber, ["1", "2", "3", "4", "5"]);
assert.compareArray([...new Intl.Segmenter("en").segment(null)].map(s => s.segment),
  ["n", "u", "l", "l"]);

// 19.5: the Segments object. Its prototype is an ordinary object above %Object.prototype%, it has
// `containing` and @@iterator and, unlike every other prototype of this build, no @@toStringTag.
const segments = new Intl.Segmenter("en", { granularity: "word" }).segment("Hello, world");
const segmentsProto = Object.getPrototypeOf(segments);
assert.sameValue(Object.getPrototypeOf(segmentsProto), Object.prototype);
assert.sameValue(segmentsProto[Symbol.toStringTag], undefined);
assert.sameValue(Object.prototype.toString.call(segments), "[object Object]");
assert.sameValue(typeof segmentsProto.containing, "function");
assert.sameValue(segmentsProto.containing.length, 1);
assert.sameValue(segmentsProto.containing.name, "containing");
assert.sameValue(Object.getOwnPropertyNames(segments).length, 0);
assert.sameValue(typeof segments[Symbol.iterator], "function");
assert.sameValue(segments[Symbol.iterator].length, 0);

// 19.7.1: the segment data object, whose four properties are created in steps 6 to 10 and are all
// plain data properties. `input` is the whole string, by identity of value.
const first = [...segments][0];
assert.compareArray(Object.keys(first), ["segment", "index", "input", "isWordLike"]);
const descriptor = Object.getOwnPropertyDescriptor(first, "segment");
assert.sameValue(descriptor.writable, true);
assert.sameValue(descriptor.enumerable, true);
assert.sameValue(descriptor.configurable, true);
assert.sameValue(first.segment, "Hello");
assert.sameValue(first.index, 0);
assert.sameValue(first.input, "Hello, world");
assert.sameValue(first.isWordLike, true);

// The word segmentation of 19.4's example sentence: the two words are word-like and neither the
// comma nor the space is, which is exactly what the note to 19.7.1 recommends for a segment of
// punctuation, and the indices are UTF-16 code unit counts.
assert.compareArray(
  [...segments].map(s => [s.segment, s.index, s.isWordLike].join("|")),
  ["Hello|0|true", ",|5|false", " |6|false", "world|7|true"]
);

// Digits are word-like and each run of them is one segment, split by the spaces between them.
const digits = new Intl.Segmenter("en", { granularity: "word" });
assert.compareArray(
  [...digits.segment("1 000 000")].map(s => [s.index, s.isWordLike].join("|")),
  ["0|true", "1|false", "2|true", "5|false", "6|true"]
);
assert.compareArray(
  [...digits.segment("3.14 pi")].map(s => [s.segment, s.isWordLike].join("|")),
  ["3.14|true", " |false", "pi|true"]
);

// 19.5.1 step 10's `isWordLike` is added only for word granularity: for the other two the property
// is absent from the data object rather than present and false.
const graphemes = new Intl.Segmenter("en");
assert.sameValue("isWordLike" in [...graphemes.segment("word")][0], false);
assert.sameValue("isWordLike" in [...new Intl.Segmenter("en", { granularity: "sentence" })
  .segment("Hi.")][0], false);

// 19.8.1's grapheme rules are the invariant part of UAX #29, which is why the engine asks for no
// locale at all here: a combining accent joins its letter, a pair of regional indicators is one
// flag, and a ZWJ sequence of three people is one cluster, in every built-in locale.
// The indices are code units, so the accent is a second unit of the first cluster and each astral
// symbol is a surrogate pair.
const joined = "e\u0301\ud83c\udde9\ud83c\uddf0\ud83d\udc68\u200d\ud83d\udc69\u200d\ud83d\udc67";
for (const tag of ["en", "da", "it"]) {
  const clusters = [...new Intl.Segmenter(tag).segment(joined)];
  assert.sameValue(clusters.length, 3, "three clusters in " + tag);
  assert.compareArray(clusters.map(s => s.index), [0, 2, 6], "code unit indices in " + tag);
  assert.compareArray(clusters.map(s => s.segment.length), [2, 4, 8], "cluster lengths in " + tag);
  assert.compareArray(clusters.map(s => s.input), [joined, joined, joined], "input in " + tag);
}

// A word segmenter that is asked for a locale it has tailorings for still finds the same two words
// in Danish and Italian as in English: what 19.8.1's note calls implementation-dependent is, for
// this build, the one rule table with no disagreement between the three baked locales.
for (const tag of ["en", "da", "it"]) {
  assert.compareArray(
    [...new Intl.Segmenter(tag, { granularity: "word" }).segment("Hello, world")]
      .filter(s => s.isWordLike).map(s => s.segment),
    ["Hello", "world"],
    "word-like segments in " + tag
  );
}

// The Danish text has the letters the collation test moves after `z`, and an apostrophe that is
// MidLetter rather than a break: `Un’amica` is one word, in code units as well as in letters.
assert.compareArray(
  [...new Intl.Segmenter("da", { granularity: "word" }).segment("Store bætter, æ!")]
    .map(s => [s.segment, s.index, s.isWordLike].join("|")),
  ["Store|0|true", " |5|false", "bætter|6|true", ",|12|false", " |13|false", "æ|14|true",
    "!|15|false"]
);
assert.compareArray(
  [...new Intl.Segmenter("it", { granularity: "word" }).segment("Un’amica (italiana)!")]
    .map(s => [s.segment, s.isWordLike].join("|")),
  ["Un’amica|true", " |false", "(|false", "italiana|true", ")|false", "!|false"]
);

// Sentence boundaries need no locale either here, and the trailing space belongs to the sentence it
// follows: UAX #29's sentence rules end a run at the terminal punctuation and take the whitespace
// with it. "Mr." is not an abbreviation to this engine, which is what the rules say rather than
// what a list of abbreviations might have said.
assert.compareArray(
  [...new Intl.Segmenter("en", { granularity: "sentence" }).segment("Mr. Brown went home. He left.")]
    .map(s => [s.segment, s.index].join("|")),
  ["Mr. |0", "Brown went home. |4", "He left.|21"]
);
assert.compareArray(
  [...new Intl.Segmenter("da", { granularity: "sentence" }).segment("Hej. Hvaå så? Fin!")]
    .map(s => s.segment),
  ["Hej. ", "Hvaå så? ", "Fin!"]
);

// 19.5.2.1: `containing` answers for the code unit at an index, and the two FindBoundary questions
// of steps 8 and 9 are the segment on either side of it - so the boundary itself belongs to the
// segment that *starts* there, and index 5 of "Hello, world" is the comma rather than the word.
for (const [index, expected] of [[0, "Hello"], [1, "Hello"], [4, "Hello"], [5, ","], [6, " "],
  [7, "world"], [11, "world"]]) {
  assert.sameValue(segments.containing(index).segment, expected, "containing " + index);
}
assert.sameValue(segments.containing(12), undefined, "the length is not inside the string");
assert.sameValue(segments.containing(-1), undefined);
assert.sameValue(segments.containing(-Infinity), undefined);
assert.sameValue(segments.containing(Infinity), undefined);

// Step 6 is ToIntegerOrInfinity, so NaN becomes 0 and answers as the first segment does, a
// fractional index is truncated, and a string index is coerced.
assert.sameValue(segments.containing(NaN).segment, "Hello");
assert.sameValue(segments.containing(-0).segment, "Hello");
assert.sameValue(segments.containing(2.7).segment, "Hello");
assert.sameValue(segments.containing(11.99).segment, "world");
assert.sameValue(segments.containing("7.9").segment, "world");

// The same answers as the iteration, position by position: the table 19.5.1 computes once is the
// only boundary data either of the two reads, so the two cannot disagree.
const indexAtEachPosition = [];
for (let index = 0; index < 12; index += 1) {
  indexAtEachPosition.push(segments.containing(index).index);
}
assert.compareArray(indexAtEachPosition, [0, 0, 0, 0, 0, 5, 6, 7, 7, 7, 7, 7]);

// An empty string is the one case with a boundary at each end and no segment between them: 19.7.1
// asserts startIndex < endIndex, so there is nothing to make, and `containing` has no code unit to
// name. 19.8.1's two fallbacks of steps 3 and 9 are what make both answers the same.
const empty = new Intl.Segmenter("en", { granularity: "word" }).segment("");
assert.sameValue([...empty].length, 0);
assert.sameValue(empty.containing(0), undefined);

// 19.6.2.1: the iterator, whose result object is the ordinary pair and whose cursor is a code unit
// index that step 9 advances by one segment. `done` is false until the string is out, and after
// that step 7 returns without moving anything, so a hundred calls past the end say the same thing.
const iterator = segments[Symbol.iterator]();
const result = iterator.next();
assert.compareArray(Object.keys(result), ["value", "done"]);
assert.sameValue(result.done, false);
assert.sameValue(result.value.segment, "Hello");
assert.compareArray(
  [iterator.next().value.segment, iterator.next().value.segment, iterator.next().value.segment],
  [",", " ", "world"]
);
const after = iterator.next();
assert.sameValue(after.done, true);
assert.sameValue(after.value, undefined);
assert.sameValue(iterator.next().done, true);
assert.sameValue(iterator.next().done, true);

// 19.6.2: %SegmentIteratorPrototype% is an ordinary object above %Iterator.prototype%, with `next`
// and 19.6.2.2's tag, and no @@iterator of its own - which is why an iterator is iterable through
// the inherited %Iterator.prototype%[@@iterator], and why that returns the same iterator object.
const iteratorProto = Object.getPrototypeOf(iterator);
assert.sameValue(typeof iteratorProto.next, "function");
assert.sameValue(iteratorProto.next.length, 0);
assert.sameValue(iteratorProto.next.name, "next");
assert.sameValue(iteratorProto[Symbol.toStringTag], "Segmenter String Iterator");
assert.sameValue(Object.prototype.toString.call(iterator), "[object Segmenter String Iterator]");
assert.sameValue(Object.getOwnPropertySymbols(iteratorProto).includes(Symbol.iterator), false);
assert.sameValue(Object.getOwnPropertyNames(iterator).length, 0);
const reused = segments[Symbol.iterator]();
assert.sameValue(reused[Symbol.iterator](), reused);
assert.sameValue(Object.getPrototypeOf(iteratorProto) ===
  Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]())), true);

// An exhausted iterator is a normal completion of a for-of, and destructuring reads the same four
// properties through the same table.
assert.compareArray(
  (() => { const out = []; for (const { segment } of segments) out.push(segment); return out; })(),
  ["Hello", ",", " ", "world"]
);
const [, secondSegment] = segments;
assert.sameValue(secondSegment.segment, ",");
assert.sameValue(secondSegment.isWordLike, false);

// Linearity is the design of 19.5.1 in this build: the boundaries of the string are found once, so
// a full pass reads one table entry per segment. The assertion is that a long string is answered
// the same way at both ends of it, at the same cost in either direction.
const long = "word ".repeat(200) + "end";
const longSegments = new Intl.Segmenter("en", { granularity: "word" }).segment(long);
assert.sameValue([...longSegments].length, 401);
assert.sameValue(longSegments.containing(long.length - 1).segment, "end");
assert.sameValue(longSegments.containing(0).segment, "word");
assert.sameValue([...longSegments].at(-1).index, long.length - 3);

// The same string in the other two granularities, and to a segmenter of each built-in locale:
// nothing about the boundary table depends on the locale for this text, which is what the three
// rows of Table 34 have to say about the same string.
for (const tag of ["en", "da", "it"]) {
  const counted = [...new Intl.Segmenter(tag, { granularity: "grapheme" }).segment("ab c")];
  assert.compareArray(counted.map(s => s.segment), ["a", "b", " ", "c"], "graphemes in " + tag);
  assert.compareArray(
    [...new Intl.Segmenter(tag, { granularity: "sentence" }).segment("ab c")].map(s => s.segment),
    ["ab c"],
    "one sentence in " + tag
  );
}

// The limit of a build with the rule tables and no model: `auto` and `lstm` are off in the
// workspace pin, so a run of Thai, Lao, Khmer, Myanmar, Chinese or Japanese code points has no
// dictionary to break it, and ComplexPayloadsBorrowed::select answers None with
// "No segmentation model for complex script" - which ICU4X's rule engine reports as one boundary at
// the end of the run. The whole text therefore comes back as a single segment, and the locale
// itself is not one of the three baked, so it resolves to the fallback rather than to `zh`.
// The assertion is the measured behaviour, not a wish about it; NOTES-intl-segmenter.md section 3
// has the same three cases and the size a model would have cost.
for (const [tag, text] of [["zh", "中文分词测试"], ["ja", "日本語の文"], ["th", "ไทยคำหลายคำ"]]) {
  const complex = new Intl.Segmenter(tag, { granularity: "word" });
  assert.sameValue(complex.resolvedOptions().locale, "en", tag + " falls back to en");
  const runs = [...complex.segment(text)];
  assert.sameValue(runs.length, 1, tag + " is one rule-based run");
  assert.sameValue(runs[0].segment, text, tag + " keeps the whole string");
  assert.sameValue(runs[0].index, 0);
}

// A sentence in those scripts is a different matter: sentence boundaries come from the terminal
// punctuation, which the rule engine does have, so the two sentences are two segments even though
// neither of them can be broken into words.
assert.compareArray(
  [...new Intl.Segmenter("zh", { granularity: "sentence" }).segment("中文。测试！")].map(s => s.segment),
  ["中文。", "测试！"]
);
