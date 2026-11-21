/*---
description: >
  The [[IsHTMLDDA]] slot changes only typeof, ToBoolean and IsLooselyEqual: the nullish checks, the
  method-of-object lookups and the call of the hook itself answer as they do for any other object.
features: [IsHTMLDDA]
includes: [compareArray.js]
---*/

var x = $262.IsHTMLDDA;

// The slot does not reach the nullish checks
assert.sameValue(x ?? "unused", x, "?? tests for null and undefined only");
assert.sameValue(null ?? x, x);
var nullishAssigned = x;
nullishAssigned ??= "unused";
assert.sameValue(nullishAssigned, x, "??= assigns only for a nullish left side, which this is not");
assert.sameValue(x?.name, "IsHTMLDDA", "optional chaining reads through the object");
assert.sameValue(x?.missing, undefined);
assert.sameValue(typeof (x?.()), "object", "the optional call is made and answers null");
assert.sameValue((function (v) { return v === undefined ? "default" : "given"; })(x), "given");
assert.sameValue((function (v = "default") { return v; })(x), x, "a default parameter is not applied");
assert.sameValue((function (v = "default") { return v; })(undefined), "default");
var arrayPattern = [x];
var [d1 = "default"] = arrayPattern;
assert.sameValue(d1, x);
var objectPattern = { d2: x };
var { d2 = "default" } = objectPattern;
assert.sameValue(d2, x);

// GetMethod and GetV use IsCallable and IsUndefined, not the slot
assert.sameValue(typeof x[Symbol.iterator], "undefined", "the hook is not iterable");
assert.throws(TypeError, function () { [...x]; }, "spread still requires an iterator");
assert.throws(TypeError, function () {
  Array.from({ [Symbol.iterator]: x });
}, "the @@iterator method is called, and its result is not an object");
assert.compareArray(Array.from(x), [], "an object with no length and no iterator is empty");

// test262 INTERPRETING.md: the hook is callable, and its call answers null for no argument or the
// empty string, so the algorithms that call `document.all` can be tested with it.
assert.sameValue(x(), null);
assert.sameValue(x(""), null);
assert.sameValue(x(undefined), undefined);
assert.sameValue(x(0), undefined);
assert.sameValue(x("x"), undefined);
assert.sameValue(x.apply(undefined, []), null);
assert.sameValue(x.apply(undefined, [""]), null);
assert.sameValue((function () { return x("text"); })(), undefined);

Object.defineProperty(x, Symbol.split, {
  get: function () { return x; },
  configurable: true,
});
assert.sameValue(typeof x, "undefined", "the slot survives defining @@split");
assert.sameValue("".split(x), null, "the splitter is called and its result is returned");
assert.sameValue("a,b".split(x), undefined, "a non-empty subject is not the empty string");
assert.sameValue(delete x[Symbol.split], true);
assert.compareArray("a,b".split(x), ["a,b"], "without @@split the ordinary split runs");
assert.sameValue(typeof x, "undefined", "and still after deleting it");

// The value is an ordinary object in every other way
assert(x instanceof Function);
assert.sameValue(Object.prototype.toString.call(x), "[object Function]");
assert.compareArray(Object.getOwnPropertyNames(x).sort(), ["length", "name"], "a function without a prototype");
assert.sameValue(Object.getPrototypeOf(x), Function.prototype);
assert.sameValue(Object.keys(x).length, 0);
assert.sameValue(JSON.stringify({ k: x }), "{}", "a function value is dropped by JSON");
assert.sameValue(JSON.stringify([x]), "[null]", "and becomes null in an array");
assert.sameValue(
  JSON.stringify({ k: x }, function (key, value) {
    return typeof value === "undefined" && key !== "" ? "UNDEFINED" : value;
  }),
  '{"k":"UNDEFINED"}',
  "typeof inside a JSON replacer sees the slot",
);
