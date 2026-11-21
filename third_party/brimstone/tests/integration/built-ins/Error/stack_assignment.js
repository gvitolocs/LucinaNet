/*---
description: >
  The `stack` property of an error can be assigned to, and the assigned value is read back instead
  of the generated stack trace.
---*/

var error = new Error("plain");
assert.sameValue(typeof error.stack, "string");
assert.sameValue(Object.getOwnPropertyDescriptor(error, "stack"), undefined);

error.stack = "replaced";
assert.sameValue(error.stack, "replaced");
assert.sameValue(String(error), "Error: plain");

// The assignment shadows the prototype accessor with an own data property.
var descriptor = Object.getOwnPropertyDescriptor(error, "stack");
assert.sameValue(descriptor.value, "replaced");
assert.sameValue(descriptor.writable, true);
assert.sameValue(descriptor.enumerable, false);
assert.sameValue(descriptor.configurable, true);

// Repeated assignment updates the own property rather than the accessor.
error.stack = "replaced again";
assert.sameValue(error.stack, "replaced again");

// Any value can be stored, not only strings.
var nonStrings = [42, null, undefined, true, 0, NaN, { name: "object" }];
for (var i = 0; i < nonStrings.length; i++) {
  var nonString = new Error("non-string");
  nonString.stack = nonStrings[i];
  assert.sameValue(nonString.stack, nonStrings[i]);
}

// Subclasses inherit the accessor from Error.prototype.
var typeError = new TypeError("typed");
typeError.stack = "typed stack";
assert.sameValue(typeError.stack, "typed stack");

var custom = new (class CustomError extends Error {})("custom");
assert.sameValue(typeof custom.stack, "string");
custom.stack = "custom stack";
assert.sameValue(custom.stack, "custom stack");

// Objects that only inherit from Error.prototype accept the assignment too.
var errorLike = Object.create(Error.prototype);
assert.sameValue(errorLike.stack, undefined);
errorLike.stack = "error like stack";
assert.sameValue(errorLike.stack, "error like stack");

// Deleting the own property brings the generated stack trace back.
var deleted = new Error("deleted");
deleted.stack = "gone";
delete deleted.stack;
assert.sameValue(typeof deleted.stack, "string");
assert.notSameValue(deleted.stack, "gone");

// Assignment through the accessor directly defines the property on the receiver.
var accessor = Object.getOwnPropertyDescriptor(Error.prototype, "stack");
assert.sameValue(typeof accessor.get, "function");
assert.sameValue(typeof accessor.set, "function");
assert.sameValue(accessor.enumerable, false);
assert.sameValue(accessor.configurable, true);

var other = new Error("other");
accessor.set.call(other, "direct");
assert.sameValue(other.stack, "direct");

// A store that does define the own property reports success.
assert.sameValue(Reflect.set(new Error("reflected"), "stack", "reflected"), true);
