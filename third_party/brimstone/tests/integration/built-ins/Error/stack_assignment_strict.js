/*---
description: >
  Assigning to the `stack` property of an error works in strict mode, and fails on a frozen error
  just like any other non-writable property.
flags: [onlyStrict]
---*/

"use strict";

var error = new Error("strict");
error.stack = "strict stack";
assert.sameValue(error.stack, "strict stack");

var typeError = new TypeError("strict typed");
typeError.stack = "strict typed stack";
assert.sameValue(typeError.stack, "strict typed stack");

var errorLike = Object.create(Error.prototype);
errorLike.stack = "strict error like";
assert.sameValue(errorLike.stack, "strict error like");

var frozen = new Error("frozen");
Object.freeze(frozen);
assert.throws(TypeError, function () {
  frozen.stack = "cannot be set";
});

// A frozen object that already stores its own stack keeps that value.
var frozenAfterAssignment = new Error("frozen after assignment");
frozenAfterAssignment.stack = "kept";
Object.freeze(frozenAfterAssignment);
assert.throws(TypeError, function () {
  frozenAfterAssignment.stack = "replaced";
});
assert.sameValue(frozenAfterAssignment.stack, "kept");
