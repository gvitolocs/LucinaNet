/*---
description: >
  ToBoolean of an object with the [[IsHTMLDDA]] internal slot is false (ECMA-262 B.3.6.2), on every
  path that reads a value's truth: the abstract operation itself, the logical and conditional
  operators, every kind of statement condition, and the built-ins that call back into the engine.
features: [IsHTMLDDA]
includes: [compareArray.js]
---*/

var x = $262.IsHTMLDDA;

// The abstract operation, and the operators that are defined by it
assert.sameValue(Boolean(x), false);
assert.sameValue(!x, true);
assert.sameValue(!!x, false);
assert.sameValue(x ? "t" : "f", "f");
assert.sameValue(x || "fallback", "fallback");
assert.sameValue(x && "unused", x, "&& answers with the falsy value it found");
assert.sameValue(x && x, x);

// Logical assignment uses the same test, except for the nullish one
var assigned = x;
assigned ||= "fallback";
assert.sameValue(assigned, "fallback");
assigned = x;
assigned &&= "unused";
assert.sameValue(assigned, x);

// Statements whose condition is a ToBoolean test
var branch = "";
if (x) {
  branch = "then";
} else {
  branch = "else";
}
assert.sameValue(branch, "else");

var iterations = 0;
while (x) {
  iterations += 1;
}
assert.sameValue(iterations, 0, "the loop body never ran");

iterations = 0;
do {
  iterations += 1;
} while (x);
assert.sameValue(iterations, 1, "a do-while tests after its first body");

iterations = 0;
for (var i = 0; i < 3 && x; i += 1) {
  iterations += 1;
}
assert.sameValue(iterations, 0, "a for condition that is falsy ends the loop");

// Built-ins that decide with ToBoolean of a callback result
assert.compareArray([x, 1, x, 2].filter(function (v) { return v; }), [1, 2]);
assert.compareArray([x, 1].filter(Boolean), [1]);
assert.sameValue([x].some(function (v) { return v; }), false);
assert.sameValue([x].every(function (v) { return v; }), false);
assert.sameValue([x].find(function (v) { return v; }), undefined);
assert.sameValue([x].findIndex(function (v) { return v; }), -1);
assert.sameValue([1, x].reduce(function (a, b) { return b ? a + b : a; }, 0), 1);

// Values without the slot are unchanged
var sentinel = {};
assert.sameValue(Boolean(sentinel), true);
assert.sameValue(Boolean([]), true);
assert.sameValue(Boolean(""), false);
assert.sameValue(Boolean(0), false);
assert.sameValue(Boolean(undefined), false);
assert.sameValue(Boolean(null), false);
assert.compareArray([sentinel, 0, "", null, x].filter(function (v) { return v; }), [sentinel]);
