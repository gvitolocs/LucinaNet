/*---
description: >
  In sloppy mode a store to the `stack` property that cannot define the own property is silently
  ignored, as for any other property.
flags: [noStrict]
---*/

var frozen = new Error("frozen");
Object.freeze(frozen);

frozen.stack = "ignored";
assert.notSameValue(frozen.stack, "ignored");
assert.sameValue(typeof frozen.stack, "string");

var sealed = new Error("sealed");
Object.seal(sealed);

sealed.stack = "also ignored";
assert.notSameValue(sealed.stack, "also ignored");
assert.sameValue(typeof sealed.stack, "string");

var prevented = Object.create(Error.prototype);
Object.preventExtensions(prevented);

prevented.stack = "ignored too";
assert.sameValue(prevented.stack, undefined);

// A store that can define the own property still works.
var extensible = new Error("extensible");
extensible.stack = "kept";
assert.sameValue(extensible.stack, "kept");
