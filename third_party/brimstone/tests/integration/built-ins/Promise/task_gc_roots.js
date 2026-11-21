/*---
description: >
  Promise reaction jobs, thenable jobs, await continuations and FinalizationRegistry cleanup tasks
  keep everything they read after the user callback ran (handler, result capability, settled value,
  generator, realm) valid while that callback allocates and forces moving collections. The engine
  pops a task off its queue before running it, so the task itself is in no root set while the
  callback runs. Run with BS_GC_STRESS=32768 BS_GC_VERIFY=1 to make every few allocations collect.
flags: [async]
---*/

const log = [];
let sink;

function churn(n) {
  const a = [];
  for (let i = 0; i < n; i++) a.push({ i, s: "x" + i, a: [i, i + 1] });
  $262.gc();
  sink = a.length;
  for (let i = 0; i < n; i += 97) {
    assert.sameValue(a[i].s, "x" + i);
    assert.sameValue(a[i].a[1], i + 1);
  }
  return a.length;
}

// Fulfill and reject reactions; the callbacks are referenced only by their tasks.
(function () {
  const tag = { t: "k1" };
  Promise.resolve(7)
    .then(function (v) { churn(600); log.push("a:" + v + ":" + tag.t); return { w: v + 1 }; })
    .then(function (o) { churn(600); log.push("b:" + o.w); return o.w * 2; })
    .then(function (v) { churn(600); log.push("c:" + v); });

  Promise.reject({ why: "r" })
    .then(function () { log.push("no"); })
    .catch(function (e) { churn(600); log.push("d:" + e.why); throw { again: e.why + "2" }; })
    .then(null, function (e) { churn(600); log.push("e:" + e.again); return "done"; })
    .then(function (v) { log.push("f:" + v); });
})();

// Thenable job.
(function () {
  const thenable = { v: 5, then(resolve) { churn(600); log.push("t:" + this.v); resolve(this.v + 1); } };
  Promise.resolve(thenable).then(function (v) { churn(600); log.push("g:" + v); });
})();

// Await continuations, including async generators and for-await over a sync iterable.
const asyncDone = (async function () {
  const local = { n: 1 };
  const v = await Promise.resolve(10);
  churn(600);
  log.push("h:" + v + ":" + local.n);
  const w = await { then(r) { churn(600); r(20); } };
  churn(600);
  log.push("i:" + w);
  try { await Promise.reject({ e: 3 }); } catch (e) { churn(600); log.push("j:" + e.e); }

  async function* g() { const x = await 1; churn(600); yield x; yield await Promise.resolve(2); churn(600); return 3; }
  for await (const y of g()) { churn(600); log.push("k:" + y); }
  for await (const u of [Promise.resolve("s1"), "s2"]) { churn(600); log.push("l:" + u); }
  return 99;
})();

// Combinator element functions.
Promise.all([1, 2, 3].map((i) => Promise.resolve(i).then((v) => { churn(500); return v * 10; })))
  .then((a) => { churn(500); log.push("all:" + a.join("+")); });

// FinalizationRegistry cleanup task.
const registry = new FinalizationRegistry(function (held) { churn(600); log.push("fr:" + held.id); });
(function () { registry.register({}, { id: 41 }); })();
$262.gc();
$262.gc();

asyncDone
  .then((v) => { assert.sameValue(v, 99); })
  .then(() => new Promise((resolve) => resolve()))
  .then(() => new Promise((resolve) => resolve()))
  .then(() => {
    const seen = new Set(log);
    for (const entry of [
      "a:7:k1", "b:8", "c:16", "d:r", "e:r2", "f:done", "t:5", "g:6", "h:10:1", "i:20", "j:3",
      "k:1", "k:2", "l:s1", "l:s2", "all:10+20+30", "fr:41",
    ]) {
      assert(seen.has(entry), "missing log entry " + entry + " in " + log.join(","));
    }
    assert.sameValue(log.length, 17, log.join(","));
  })
  .then($DONE, $DONE);
