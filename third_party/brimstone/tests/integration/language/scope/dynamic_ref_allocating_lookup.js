/*---
description: >
  The dynamic reference instructions (MakeDynamicRef, LoadDynamicRef, StoreToDynamicRef) read their
  operands before the scope lookup, which can run script (@@unscopables getter, Proxy has trap) that
  allocates and collects. Under the gc_stress_test feature (or a GC stress window) a collection
  moves the bytecode, so operands read afterwards came from poisoned memory.
flags: [noStrict]
---*/

// Allocate enough to force collections
function churn() {
  var keep = [];
  for (var i = 0; i < 20; i++) {
    keep.push({ a: i, s: "churn" + i, arr: [i, i + 1, i + 2] });
  }
  return keep.length;
}

// x += 1
{
  var calls = 0;
  var o = { x: 4, get [Symbol.unscopables]() { calls++; churn(); return {}; } };
  with (o) { x += 1; }
  assert.sameValue(o.x, 5);
  assert.sameValue(calls > 0, true);
}

// x++ (postfix) and ++x (prefix)
{
  var o = { x: 4, get [Symbol.unscopables]() { churn(); return {}; } };
  var r;
  with (o) { r = x++; }
  assert.sameValue(r, 4);
  assert.sameValue(o.x, 5);
  with (o) { r = ++x; }
  assert.sameValue(r, 6);
  assert.sameValue(o.x, 6);
  with (o) { r = x--; }
  assert.sameValue(r, 6);
  assert.sameValue(o.x, 5);
}

// Logical and exponent assignment
{
  var o = { x: 0, y: null, z: 3, get [Symbol.unscopables]() { churn(); return {}; } };
  with (o) {
    x ||= 7;
    y ??= 8;
    z **= 2;
    x &&= x + 1;
  }
  assert.sameValue(o.x, 8);
  assert.sameValue(o.y, 8);
  assert.sameValue(o.z, 9);
}

// A Proxy whose has trap allocates
{
  var target = { x: 1 };
  var p = new Proxy(target, {
    has(t, k) { churn(); return k in t; },
    get(t, k, r) { churn(); return Reflect.get(t, k, r); },
    set(t, k, v, r) { churn(); return Reflect.set(t, k, v, r); },
  });
  with (p) {
    x += 10;
    x++;
    x **= 2;
  }
  assert.sameValue(target.x, 144);
}

// Compound assignment on a global through with
{
  globalThis.gx = 2;
  var o = { get [Symbol.unscopables]() { churn(); return {}; } };
  with (o) {
    gx += 3;
    gx *= 2;
    gx++;
  }
  assert.sameValue(globalThis.gx, 11);
}

// Right hand side that collects too, inside a function (frame pop after the instructions)
function f() {
  var o = { x: 1, get [Symbol.unscopables]() { churn(); return {}; } };
  with (o) {
    x += (churn(), 2);
    x ??= 0;
  }
  return o.x;
}
assert.sameValue(f(), 3);
assert.sameValue(f(), 3);

// Simple assignment is the only shape that emits MakeDynamicRef (no load of the old value)
{
  var o = { x: 1, get [Symbol.unscopables]() { churn(); return {}; } };
  with (o) { x = 5; }
  assert.sameValue(o.x, 5);
}

// MakeDynamicRef through a Proxy whose has trap allocates
{
  var target = { x: 1 };
  var p = new Proxy(target, {
    has(t, k) { churn(); return k in t; },
    set(t, k, v, r) { churn(); return Reflect.set(t, k, v, r); },
  });
  with (p) { x = 7; }
  assert.sameValue(target.x, 7);
}

// A global assigned through a with whose lookup collects
{
  globalThis.gz = 1;
  var o = { get [Symbol.unscopables]() { churn(); return {}; } };
  with (o) { gz = 9; }
  assert.sameValue(globalThis.gz, 9);
}

// A name bound nowhere: the reference is undefined and the store creates a global property
{
  var o = { get [Symbol.unscopables]() { churn(); return {}; } };
  with (o) { undeclaredByVm01 = 3; }
  assert.sameValue(globalThis.undeclaredByVm01, 3);
}

// The right hand side collects too, inside a function (frame pop after the instruction)
function g() {
  var o = { x: 1, get [Symbol.unscopables]() { churn(); return {}; } };
  with (o) {
    x = (churn(), 2);
  }
  return o.x;
}
assert.sameValue(g(), 2);
assert.sameValue(g(), 2);
