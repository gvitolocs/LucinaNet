/*---
description: >
  Writing a property of null or undefined reports which property was set on which of the two.
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

assert.sameValue(messageOf(function () { undef.z = 1; }), "Cannot set properties of undefined (setting 'z')");
assert.sameValue(messageOf(function () { null.z = 1; }), "Cannot set properties of null (setting 'z')");

assert.sameValue(messageOf(function () { undef[key] = 1; }), "Cannot set properties of undefined (setting 'z')");
assert.sameValue(messageOf(function () { null.other = 1; }), "Cannot set properties of null (setting 'other')");
assert.sameValue(messageOf(function () { undef[0] = 1; }), "Cannot set properties of undefined (setting '0')");
assert.sameValue(messageOf(function () { null[sym] = 1; }), "Cannot set properties of null (setting 'Symbol(sym)')");

assert.sameValue(messageOf(function () { nested.b.c = 1; }), "Cannot set properties of undefined (setting 'c')");

// A store that first reads the property reports the read, which is what failed.
assert.sameValue(messageOf(function () { undef.z *= 2; }), "Cannot read properties of undefined (reading 'z')");
assert.sameValue(messageOf(function () { ++undef.z; }), "Cannot read properties of undefined (reading 'z')");
