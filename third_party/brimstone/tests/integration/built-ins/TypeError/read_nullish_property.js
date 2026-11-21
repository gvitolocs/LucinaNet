/*---
description: >
  Reading a property of null or undefined reports which property was read from which of the two.
---*/

function messageOf(action) {
  try {
    action();
  } catch (error) {
    return error.message;
  }
  return "<no error thrown>";
}

var undef;
var nested = { b: undefined };
var key = "z";
var sym = Symbol("sym");

assert.sameValue(messageOf(function () { return undef.z; }), "Cannot read properties of undefined (reading 'z')");
assert.sameValue(messageOf(function () { return null.z; }), "Cannot read properties of null (reading 'z')");

// The name is the property that was asked for, however it was written.
assert.sameValue(messageOf(function () { return undef[key]; }), "Cannot read properties of undefined (reading 'z')");
assert.sameValue(messageOf(function () { return null["other"]; }), "Cannot read properties of null (reading 'other')");

// A computed key that is an index prints its number, and a symbol prints its description.
assert.sameValue(messageOf(function () { return undef[0]; }), "Cannot read properties of undefined (reading '0')");
assert.sameValue(messageOf(function () { return undef[12]; }), "Cannot read properties of undefined (reading '12')");
assert.sameValue(messageOf(function () { return null[sym]; }), "Cannot read properties of null (reading 'Symbol(sym)')");

// The chain stops at the value that is missing, and names the property after it.
assert.sameValue(messageOf(function () { return nested.b.c; }), "Cannot read properties of undefined (reading 'c')");

// A read-modify-write reads first.
assert.sameValue(messageOf(function () { return (undef.z += 1); }), "Cannot read properties of undefined (reading 'z')");

// Optional chaining never reaches the read.
assert.sameValue(messageOf(function () { return undef?.z; }), "<no error thrown>");
assert.sameValue(messageOf(function () { return undef?.[0]; }), "<no error thrown>");

// A property read of a primitive goes through the wrapper object, as it always did.
assert.sameValue(messageOf(function () { return "text".length; }), "<no error thrown>");

// The base is reported even when the key cannot be named: the key is only borrowed for the message.
assert.sameValue(
  messageOf(function () {
    return undef[{ toString: function () { throw new Error("from the key"); } }];
  }),
  "Cannot read properties of undefined (reading 'unknown')"
);

// A value that is not nullish still reads as undefined.
assert.sameValue(undef === undefined, true);
