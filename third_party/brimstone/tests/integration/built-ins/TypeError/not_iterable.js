/*---
description: >
  Iterating a value that is not iterable reports which expression was iterated, and falls back to
  naming nothing when the expression has no source text of its own.
---*/

function messageOf(action) {
  try {
    action();
  } catch (error) {
    return error.message;
  }
  return "<no error thrown>";
}

var notIterable = 5;
var host = { value: 1 };
var list = { length: 0 };

// The expression that was spread, destructured or iterated is written as it is.
assert.sameValue(messageOf(function () { return [...notIterable]; }), "notIterable is not iterable");
assert.sameValue(messageOf(function () { var a = [...host.value]; }), "host.value is not iterable");
assert.sameValue(messageOf(function () { for (const x of notIterable) {} }), "notIterable is not iterable");
assert.sameValue(messageOf(function () { for (const x of list) {} }), "list is not iterable");

// A value that has no name to be written keeps the message that named nothing: the number is a
// literal, the pattern is a pattern, and neither is the expression that failed. The spread element
// reports the position of its `...`, which is stepped over to reach the expression after it.
assert.sameValue(messageOf(function () { return [...5]; }), "value is not iterable");
assert.sameValue(messageOf(function () { return [...{}]; }), "value is not iterable");
assert.sameValue(messageOf(function () { var [a] = notIterable; }), "value is not iterable");

// A string is iterable, and an object that provides an iterator is too.
assert.sameValue(messageOf(function () { return [..."ab"].join(""); }), "<no error thrown>");
assert.sameValue(messageOf(function () { return [...[1, 2]][1]; }), "<no error thrown>");
