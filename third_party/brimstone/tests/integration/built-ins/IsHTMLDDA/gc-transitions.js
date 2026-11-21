/*---
description: >
  The private shape of an [[IsHTMLDDA]] object keeps its cached transitions across collections.
  The shape is detached from the transition tree, so nothing in the tree reaches it: the collector
  has to fix the weak next-shape edge it hangs off the emulating object, and a stale edge is a
  pointer into memory the nursery has already handed out again.
features: [IsHTMLDDA]
---*/

// Against the collector before the to-space weak-edge pass this aborts in the heap verifier with
// BS_GC_VERIFY=1 (at the second collection here, at the fourteenth with BS_GC_STRESS=4096), and in
// plain execution it stays silent: nothing reads the stale edge, so the only symptom is the
// verifier catching a weak pointer into the semispace just vacated. The always-on guard for the
// same invariant is the unit test
// gc_verify_repairs_the_weak_edges_of_holders_the_named_walks_miss in
// src/js/runtime/embed/tests.rs, which enables the verifier itself and so fails on the old
// collector in a plain cargo test.

var x = $262.IsHTMLDDA;

assert.sameValue(typeof x, "undefined");

// Each added property gives the emulating object a new shape cloned from the one it has, and the
// shape it just left caches the edge to the new one: a single weak next-shape pointer while it has
// only that one child, a transition vec as soon as it has a second (the vec held strongly, its
// entries weakly). A collection between every mutation makes the collector re-locate or drop each
// of those edges.
for (var i = 0; i < 24; i++) {
  x["p" + i] = i;
  // Churn the nursery so the space a stale edge points into gets handed out again for real.
  for (var j = 0; j < 64; j++) {
    var churn = { a: j, b: "s" + j, c: [j, j + 1] };
    assert.sameValue(churn.a + churn.c[1], j * 2 + 1);
  }
  $262.gc();
  assert.sameValue(x["p" + i], i, "p" + i + " reads back right after a collection");
}

// A prototype change is a transition too.
var parent = { inherited: 1 };
Object.setPrototypeOf(x, parent);
$262.gc();
assert.sameValue(x.inherited, 1, "the new prototype is reachable after a collection");

// Past the 64 array-mode properties the object enters map mode and keeps cloning the private shape.
// Every fourth property added from i = 28 on is deleted two rounds later.
for (var i = 24; i < 80; i++) {
  x["p" + i] = i;
  if (i >= 28 && i % 4 == 0) {
    delete x["p" + (i - 4)];
  }
  $262.gc();
}

Object.freeze(x);
$262.gc();

for (var i = 0; i < 80; i++) {
  var gone = i >= 24 && i <= 72 && i % 4 == 0;
  assert.sameValue(
    x["p" + i],
    gone ? undefined : i,
    "p" + i + (gone ? " stays deleted" : " survives the collections")
  );
}

// The slot itself is unchanged by all of it, and so is the shared shape the emulating object left
// behind: another object of the same kind never gets the flag.
assert.sameValue(typeof x, "undefined", "freeze did not take the slot away");
assert.sameValue(x == null, true);
var ordinary = Object.create(x);
assert.sameValue(typeof ordinary, "object", "the shape of a sibling object is untouched");
