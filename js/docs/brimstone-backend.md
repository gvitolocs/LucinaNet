# mozjs_sys on LucinaJavascript (forked from Brimstone)

**Name and direction (2026-11-08):** the engine is LucinaJavascript (LucinaJS), forked from Brimstone; it is interpreter-first
with no JIT, modelled on JavaScriptCore's LLInt. See [LucinaJavascript design](lucinajs-design.md). "Brimstone" below is
historical, or the upstream.

**Resumed audit (2026-11-05):** the state and measurements below are historical unless reverified since.

Started 2026-11-04, at full scope, with Verus and the Lucina porting guidelines. This file records what was decided and
measured while doing it. Statements about run-time behaviour
link the test or log that shows them; anything else is labelled as a plan.

## Engine choice, rechecked 2026-11-04

Porting guidelines §3: every candidate gets adopt, adapt, reference or reject.

| Engine | Licence (LICENSE file) | test262 | Speed | GC | Verdict |
|---|---|---|---|---|---|
| Brimstone (Hans-Halverson/brimstone) | MIT | README ">97% of the language", undated, self-reported. Own run on test262 227d9055: pending (see below) | V8 suite 1799 and 1854 on the reference Linux development machine (2026-11-02); javascript-zoo (2025-12): Richards 122, DeltaBlue 184, Splay 961 | Cheney semispace, moves everything, no nursery | **adapt** (fork, under `third_party/brimstone`, base 64d6698) |
| Boa | MIT | 95.60%, v0.22 release notes (2026-08-28), self-reported from its CI | V8 suite 185 on the reference Linux development machine; javascript-zoo Richards 170, DeltaBlue 146 | non-moving mark-sweep with ref counts | **reference** (Intl from it later: ICU4X, MIT) |
| Nova | MPL-2.0 | no current figure; test262.fyi dropped it on 2026-09-22 (inactive >90 days, resumed 2026-10-18); 79.9% in May | crashed on the V8 suite (RangeError); zoo Richards 51.9 | compacting, lifetime-checked API | **reject**: conformance, RegExp lacks lookbehind and backreferences, API has no raw pointers to map onto |
| yavashark | MIT | "~81%" README, self-reported | zoo Richards 15.1 | not described | **reject**: an order of magnitude slower, conformance |
| jsse (pmatos/jsse) | MIT | "Goal: 100%", baselines in repo, no number | not in the zoo | nursery (commit titles) | **reject** for now: tree-walking interpreter by its README, no embedding API, agent-written single-author code; look again if it publishes numbers |
| stator (telecos/stator) | MIT | "100%", self-reported, harness uses an allowlist | self-reported, x86-64 JIT only | not stated | **reject**: JIT targets x86-64 Linux only (Juno is arm64), C FFI aimed at Chromium |
| zuqe (nupha/zuqe) | Apache-2.0 | "a substantial portion", no number | not measured | generational | **reference**: has a Rust embedding API; no conformance number to compare |
| Kiesel | — | — | — | — | **reject**: Zig, not Rust |

Sources: javascript-zoo (github.com/ivankra/javascript-zoo, data to 2026-05), each project's README and LICENSE, test262-fyi/data
issue 121. Brimstone upstream: no commit since 2026-10-16 (64d6698), one author. The choice stands: Brimstone is about ten times
Boa on the V8 suite and its object model matches SpiderMonkey's (raw heap pointers, int-or-interned-string keys, a moving GC).

## The seam

Servo -> `mozjs` (Rust, upstream 0.26.13 plus small patches) -> `mozjs_sys` (this crate: SpiderMonkey's C++ API in Rust) ->
Brimstone. `tools/gen-jsapi` turns mozjs_sys's bindgen output into Rust: implemented functions are re-exported (signature checked
at compile time), the rest abort naming themselves.

Decisions:

- **Values and ids are Brimstone's bits.** `JS::Value` and `jsid` hold Brimstone's NaN-boxed `Value` and `PropertyKey`. Servo
  only compares or hashes raw bits (`asBits_`: `proxyhandler.rs:485`, `console.rs`, `window_named_properties.rs:91`,
  `webdriver_handlers.rs:378`), never decodes them, so nothing is converted at the seam. Consequences: `DoubleValue(3.0)` is an
  int32 (Brimstone's invariant); `PrivateValue` is a double; ids for indices 2^31..2^32-2 are handed to Servo as strings.
- **Host objects do not move.** Servo turns SpiderMonkey's compacting GC off ("the layout system currently does not work with
  compacting GC", `components/config/prefs.rs:553`), so DOM reflectors, globals and DOM proxies must keep their address. The fork
  gives them a non-moving, mark-swept space inside the copying collector (`third_party/brimstone/docs/embedding.md`). Ordinary
  objects keep moving, as SpiderMonkey's nursery moves them; Servo already updates every pointer it traces.
- **ICU 2.2.0.** Brimstone's baked ICU data is generated for ICU4X 2.2.0 and calls its unstable constructors, so every ICU crate in
  the Rust-engine build is 2.2.0. LucinaNet's SpiderMonkey build has 2.1.x; the Rust-engine build therefore has its own Cargo.lock.

## Verus: what is proved

`mozjs-sys/src/boundary/` is checked as shipped (the verification script runs `scripts/verify-shipped.sh`, 85 verified, 0 errors, 2026-11-04):

| File | Proved | Not proved |
|---|---|---|
| `value_bits.rs` | kinds disjoint; constructors round-trip; no double, script-chosen NaN or `PrivateValue` ever looks like a heap pointer; a zero slot is never traced | that Brimstone uses the same encoding (differential test, `tests/value_bits_vs_brimstone.rs`) |
| `id_bits.rs` | int ids 0..2^31-1; indices round-trip; the parsers accept exactly ECMA-262 canonical array-index spellings, for any input, without overflow | that Brimstone's own parser agrees (`tests/id_bits_vs_brimstone.rs`) |
| `bounds.rs` | reserved-slot, `vp`, argument, typed-array view, string-copy and object-size results are in bounds, no overflow | that the counts passed in are the objects' real sizes |
| `roots.rs` | native frames LIFO, inside the fixed array and the GC's walk, never overlapping; refuse when full | that every push is popped (a guard's `drop`), the GC walk itself |

The verification script on `scripts/boundary-mutants.sh`: 20 planted bugs, each refused by a proof failure (2026-11-04).

## Build switch

Plan, not built yet: LucinaNet keeps building SpiderMonkey by default. `LUCINANET_JS=rust scripts/build-*.sh` builds with
`[patch.crates-io]` entries for `mozjs` and `mozjs_sys` passed as `--config`, its own lock file (`Cargo.js-rust.lock`) and its own
target directory, inside the shared build lock `/tmp/lucinanet-servo-build.lock`.

## State (2026-11-05)

| Stage | State |
|---|---|
| Spike: `mozjs` examples and tests on Brimstone | 15 of mozjs's 20 tests pass (the rest wait on stack capture, CSP, heap sizes and off-thread jobs in the glue); 100+ glue tests (property, proxy, typed array, promise) |
| example.com with scripts | **renders** (the `LUCINANET_JS=rust` build): dumps byte-identical to SpiderMonkey's, 1.0-1.1 s like SpiderMonkey, 123 MB binary against 152 MB |
| Real sites | next: Wikipedia, Hacker News, GitHub, discord.com |
| Binary without SpiderMonkey | the Rust-engine binary has none; the build switch (one lock for both engines) is still to land |

Fork rounds accepted (in the engine fork):
1. host space, natives, proxies, realms, jobs;
2. uncatchable stop, interrupts, stack limit, string chars without GC;
3. stack quota by address, compiled scripts, host-driven modules, stack capture, CSP hook, heap statistics;
4. JIT info per native, rejection tracking, job realms;
5. every Promise in the non-moving space (Servo uses a JS Promise object as its reflector and does not trace it).

Known regression: ICU 2.1 costs 10 test262 Temporal tests (4603 to 4593, from `temporal_rs` 0.2.1).
