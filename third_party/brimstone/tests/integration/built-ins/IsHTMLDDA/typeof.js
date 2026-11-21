/*---
description: >
  `typeof` of an object with the [[IsHTMLDDA]] internal slot is "undefined" (ECMA-262 B.3.6.1),
  while every other value answers as it does without the slot.
features: [IsHTMLDDA]
---*/

var x = $262.IsHTMLDDA;

// The slot itself
assert.sameValue(typeof x, "undefined");
assert.sameValue(typeof $262.IsHTMLDDA, "undefined");
assert.sameValue(typeof x, "undefined", "the same object read twice answers the same way");

// The object is still a function object: its properties and its callability are unchanged
assert.sameValue(typeof x.call, "function");
assert.sameValue(typeof x.apply, "function");
assert.sameValue(typeof x.name, "string");
assert.sameValue(typeof x.length, "number");
assert.sameValue(x.name, "IsHTMLDDA");

// `valueOf` answers with the object itself, so the slot is seen through as well
assert.sameValue(typeof x.valueOf(), "undefined");
assert.sameValue(Object(x), x, "Object() of an object is the object itself");
assert.sameValue(typeof Object(1), "object");

// Nothing else about `typeof` changed
assert.sameValue(typeof $262, "object");
assert.sameValue(typeof {}, "object");
assert.sameValue(typeof [], "object");
assert.sameValue(typeof Math.max, "function");
assert.sameValue(typeof undefined, "undefined");
assert.sameValue(typeof null, "object");
assert.sameValue(typeof 1, "number");
assert.sameValue(typeof 1.5, "number");
assert.sameValue(typeof NaN, "number");
assert.sameValue(typeof "", "string");
assert.sameValue(typeof true, "boolean");
assert.sameValue(typeof Symbol.iterator, "symbol");
assert.sameValue(typeof 1n, "bigint");
assert.sameValue(typeof undeclaredVariable, "undefined");
