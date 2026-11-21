/*---
description: >
  An object with the [[IsHTMLDDA]] internal slot is loosely equal to null and undefined in either
  order (ECMA-262 B.3.6.3), and to nothing else: strict equality, Object.is, SameValueZero and every
  other type combination answer as they do without the slot.
features: [IsHTMLDDA]
---*/

var x = $262.IsHTMLDDA;

// B.3.6.3, in either order
assert.sameValue(x == null, true);
assert.sameValue(x == undefined, true);
assert.sameValue(null == x, true);
assert.sameValue(undefined == x, true);
assert.sameValue(void 0 == x, true);
assert.sameValue(x == void 0, true);
assert.sameValue(x != null, false);
assert.sameValue(undefined != x, false);

// The object is still itself
assert.sameValue(x == x, true);
assert.sameValue(x != x, false);
assert.sameValue(x === x, true);

// The identity checks the annex does not change
assert.sameValue(x === null, false);
assert.sameValue(x === undefined, false);
assert.sameValue(null === x, false);
assert.sameValue(undefined === x, false);
assert.sameValue(Object.is(x, null), false);
assert.sameValue(Object.is(x, undefined), false);
assert.sameValue(Object.is(x, x), true);
assert.sameValue([undefined, null].indexOf(x), -1, "indexOf compares strictly");
assert.sameValue([x].includes(undefined), false, "includes compares by SameValueZero");
assert.sameValue([x].includes(x), true);

// Switch compares with strict equality too
var branch = "none";
switch (x) {
  case undefined:
    branch = "undefined";
    break;
  case null:
    branch = "null";
    break;
  case x:
    branch = "self";
    break;
}
assert.sameValue(branch, "self");

// Every other loose comparison of the object answers by its primitive value
assert.sameValue(x == false, false);
assert.sameValue(x == 0, false);
assert.sameValue(x == "", false);
assert.sameValue(x == "0", false);
assert.sameValue(x == NaN, false);
assert.sameValue(x == {}, false);
assert.sameValue(x == [], false);
assert.sameValue(x == Symbol.iterator, false);
assert.sameValue(x == Object.prototype, false);
assert.sameValue(x > 1, false);
assert.sameValue(x < 1, false);
assert(Number.isNaN(x - 1));
assert.sameValue(Object.prototype.toString.call(x), "[object Function]");

// Values without the slot keep the answers they had before
assert.sameValue({} == null, false);
assert.sameValue([] == null, false);
assert.sameValue(Object.create(null) == undefined, false);
assert.sameValue(null == undefined, true);
assert.sameValue(0 == false, true);
assert.sameValue("" == 0, true);
assert.sameValue("1" == 1, true);
assert.sameValue([] == "", true);
assert.sameValue(NaN == NaN, false);
assert.sameValue(Symbol.iterator == undefined, false);
