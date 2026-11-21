/*---
description: >
  A thrown error reports a string `stack` by default, and error-wrapping code can replace it before
  rethrowing.
---*/

var caught;

function throwAndCatch(thrower) {
  try {
    thrower();
  } catch (error) {
    caught = error;
  }

  return caught.stack;
}

assert.sameValue(typeof throwAndCatch(function () {
  throw new Error("thrown");
}), "string");

assert.sameValue(typeof throwAndCatch(function () {
  throw new RangeError("ranged");
}), "string");

assert.sameValue(typeof throwAndCatch(function () {
  throw new (class Wrapped extends Error {})("subclassed");
}), "string");

// Errors created by the engine itself also carry a stack that can be replaced.
assert.sameValue(typeof throwAndCatch(function () {
  null.x;
}), "string");
assert.sameValue(caught instanceof TypeError, true);

caught.stack = "replaced engine error";
assert.sameValue(caught.stack, "replaced engine error");

// The wrapping pattern used by web code: build an error, overwrite its stack, then rethrow.
var cause = new Error("inner");
cause.stack = "Error: inner\n    at cause";

var wrapper = new Error("wrapped: " + cause.message);
wrapper.stack = "Caused by: " + cause.stack;

try {
  throw wrapper;
} catch (error) {
  assert.sameValue(error.message, "wrapped: inner");
  assert.sameValue(String(error), "Error: wrapped: inner");
  assert.sameValue(error.stack, "Caused by: Error: inner\n    at cause");
}
