/*---
description: >
  The [[IsHTMLDDA]] slot belongs to one object and reaches only typeof, ToBoolean and
  IsLooselyEqual: the objects built around an emulating object (a proxy of it, an object created
  from it, a copy of its own properties) are ordinary objects, the positions that need a real object
  or a constructor still see what they always saw, identity-keyed collections ignore the slot, and
  re-parenting or freezing the object keeps it.
features: [IsHTMLDDA]
flags: [noStrict]
includes: [compareArray.js]
---*/

var x = $262.IsHTMLDDA;

assert.sameValue(typeof x, "undefined");
assert.sameValue(Boolean(x), false);
assert.sameValue(x == null, true);

// A proxy borrows the callability of its target, never the slot
var proxy = new Proxy(x, {});
assert.sameValue(typeof proxy, "function", "the proxy of a callable object is callable");
assert.sameValue(Boolean(proxy), true, "the proxy does not emulate undefined");
assert.sameValue(proxy ? "t" : "f", "t");
assert.sameValue(proxy == null, false);
assert.sameValue(proxy == undefined, false);
assert.sameValue(proxy === x, false);
assert.sameValue(proxy == x, false);
assert.sameValue(Object.is(x, proxy), false);
assert.sameValue(typeof new Proxy(proxy, {}), "function", "a proxy of the proxy behaves the same");

// Neither does an object that merely inherits from it
var child = Object.create(x);
assert.sameValue(typeof child, "object");
assert.sameValue(Boolean(child), true);
assert.sameValue(child == null, false);
assert.sameValue(child == undefined, false);
assert.sameValue(Object.getPrototypeOf(child), x, "the emulating object is still a usable prototype");
assert.sameValue(child == x, false);

// The slot does not turn a callable object into a constructor
assert.throws(TypeError, function () { new x(); }, "the hook is callable but has no [[Construct]]");
assert.throws(TypeError, function () { Reflect.construct(x, []); }, "Reflect.construct needs a constructor");
assert.throws(TypeError, function () { class C extends x { } return C; }, "class extends needs a constructor");
assert.sameValue(x instanceof Function, true, "which leaves everything else about it alone");
assert.compareArray(Object.getOwnPropertyNames(x), ["length", "name"], "a plain function's own names");
assert.compareArray(Object.entries(x), [], "and no enumerable ones");

// An object is a legal `with` scope and an ordinary thing to enumerate, slot or not
x.scopeMarker = 42;
var lookedUp = 0;
with (x) {
  lookedUp = scopeMarker;
}
assert.sameValue(lookedUp, 42, "the scope object is the object itself");
assert.sameValue(typeof scopeMarker, "undefined", "and the binding is not visible outside its scope");

var enumerated = [];
for (var key in x) {
  enumerated.push(key);
}
assert.compareArray(enumerated, ["scopeMarker"], "for-in sees the own enumerable property");
assert.compareArray(Object.keys(x), ["scopeMarker"]);

var iterated = [];
var iteratedTruth = [];
for (var value of [x]) {
  iterated.push(value);
  iteratedTruth.push(Boolean(value));
}
assert.sameValue(iterated.length, 1);
assert.sameValue(iterated[0], x, "iteration hands out the object itself");
assert.compareArray(iteratedTruth, [false], "whose truth is still read with the slot");

// Copying its properties yields an ordinary object
x.copyable = 1;
var copy = Object.assign({}, x);
assert.compareArray(Object.keys(copy), ["scopeMarker", "copyable"]);
assert.sameValue(typeof copy, "object");
assert.sameValue(Boolean(copy), true);
assert.sameValue(copy == null, false);
assert.sameValue(
  JSON.stringify({ ...x }),
  '{"scopeMarker":42,"copyable":1}',
  "a spread copy is ordinary too"
);
delete x.copyable;
delete x.scopeMarker;
assert.compareArray(Object.keys(x), [], "and the original is back to being empty");

// It serializes the way any other callable does
assert.sameValue(JSON.stringify([x]), "[null]");
assert.sameValue(JSON.stringify({ k: x }), "{}");
assert.sameValue(JSON.stringify(x), undefined, "a function has no JSON form");

// Identity-keyed collections use SameValueZero, which the slot does not reach
assert.sameValue(Array.prototype.includes.call([x], undefined), false);
assert.sameValue(Array.prototype.includes.call([x], x), true);
assert.sameValue([x].indexOf(undefined), -1);
assert.sameValue([x].indexOf(x), 0);
var map = new Map([[x, 1]]);
assert.sameValue(map.get(undefined), undefined, "a key is never the slot");
assert.sameValue(map.get(x), 1);
var set = new Set([x]);
assert.sameValue(set.has(undefined), false);
assert.sameValue(set.has(x), true);
assert.sameValue(new WeakMap().set(x, 1).get(x), 1, "and it is a usable weak key");

// Finally the slot itself: re-parenting and freezing must not lose it
assert.sameValue(Object.setPrototypeOf(x, null), x);
assert.sameValue(typeof x, "undefined", "typeof after the prototype is gone");
assert.sameValue(Boolean(x), false, "ToBoolean after the prototype is gone");
assert.sameValue(x == null, true, "loose equality after the prototype is gone");
assert.sameValue(x == undefined, true);
assert.sameValue(x === undefined, false);
assert.sameValue(x(), null, "still callable, with nothing on its prototype chain");

assert.sameValue(Object.freeze(x), x);
assert.sameValue(Object.isFrozen(x), true);
assert.sameValue(typeof x, "undefined", "typeof after the freeze");
assert.sameValue(Boolean(x), false, "ToBoolean after the freeze");
assert.sameValue(x == null, true, "loose equality after the freeze");
assert.sameValue(x == undefined, true);
assert.sameValue(x === null, false, "which is still not the strict answer");
assert.sameValue(x === undefined, false);
assert.sameValue(Object.is(x, undefined), false);
assert.sameValue(x ?? "default", x, "and still not nullish");
assert.sameValue(typeof x.name, "string", "its own properties are readable while frozen");
var frozenKeys = [];
for (var frozenKey in x) {
  frozenKeys.push(frozenKey);
}
assert.compareArray(frozenKeys, [], "with nothing enumerable to find");
assert.throws(TypeError, function () { return Object.assign(x, { added: 1 }); }, "a frozen object refuses the write");
assert.throws(TypeError, function () { Object.setPrototypeOf(x, Object.prototype); }, "and refuses a new prototype");
assert.sameValue(typeof x, "undefined", "the refusal leaves the slot exactly where it was");
assert.sameValue(Boolean(x), false);
assert.sameValue(x == null, true);
