/*---
description: >
  Interned strings survive the collections that happen while the interned strings set is being
  grown. Every grow allocates a bigger set and copies the entries over, and the allocation of the
  new set can itself trigger a collection.
  Against the collector before the to-space weak-edge pass, this aborts in the heap verifier only
  with BS_GC_STRESS set as well: the grow has to be caught with its superseded set still held by a
  handle slot of the growing code, and only the stress window makes a collection land there. The
  always-on guard for the same invariant is the unit test
  gc_verify_repairs_the_weak_edges_of_holders_the_named_walks_miss in
  src/js/runtime/embed/tests.rs, which turns the verifier on itself and so fails on the old
  collector in a plain cargo test.
---*/

var keep = [];

for (var i = 0; i < 2000; i++) {
  var o = {};
  o["n" + i] = i;
  keep.push(o);

  if (i % 25 == 0) {
    $262.gc();
  }
}

for (var i = 0; i < 2000; i++) {
  assert.sameValue(keep[i]["n" + i], i, "n" + i);
}
