# LucinaJavascript design

LucinaJavascript (LucinaJS for short) is LucinaNet's JavaScript engine, in Rust, behind a Rust `mozjs_sys` (this repo). It was
forked from Brimstone (Hans-Halverson/brimstone, MIT, base 64d6698) and has changed a lot since (2026-11-08). Brimstone
is only its upstream origin, recorded in `third_party/UPSTREAMS.toml`. The crates and the fork keep
their `brimstone` names until the rename below.

## Direction (2026-11-08): interpreter-first, no JIT, modelled on JavaScriptCore's LLInt

JavaScriptCore's interpreter stays fast on ARM without a JIT (Safari on iOS gives third parties no
JIT), which is LucinaJS's situation on Juno (aarch64, no executable memory, simple W^X story).

Records per our porting guidelines (the originals are the source of truth for behaviour; JavaScriptCore is
pinned at WebKit eeb5ad7, id `javascriptcore`):

| Original | Part | Verdict | Licence |
|---|---|---|---|
| JavaScriptCore | LLInt: register bytecode, cheap dispatch, inline caches on property access and calls | **adopt** (behaviour, in Rust) | BSD-2-Clause (llint/, bytecode/) |
| JavaScriptCore | UnlinkedFunctionExecutable: a function keeps only its source range until its first call | **adopt**, done 2026-11-08 (lazy re-parse) | BSD-2-Clause |
| JavaScriptCore | MarkedSpace: non-moving heap, size classes, eden collection with sticky mark bits | **adapt** (behaviour only) | LGPL-2.0-or-later (heap/): no code copied |
| JavaScriptCore | Bytecode cache (runtime/CachedTypes) | **reference** | LGPL/BSD mix: behaviour only |
| JavaScriptCore | Baseline JIT, DFG, FTL | **reject**: no executable memory | |
| V8 | Script streaming, preparser, code cache | **reference** for streaming parse and the cache | BSD-3-Clause |
| SpiderMonkey | Off-thread parse, stencil cache, syntax parser, lazy scripts | **reference** for the same | MPL-2.0 |

Dispatch: measure on the A76 hardware before choosing: a `match` loop with careful layout, tail-call threading (`become`, nightly,
only if the toolchain policy allows it) and a jump table of handler functions. Pick by numbers.

## Where it stands (2026-11-08)

x86 YouTube watch page, against SpiderMonkey (6 rounds, medians of valid runs): CPU over 45 s 17.6 s against 19.3 s;
time to play 2.00 s against 1.81 s; max RSS 810 MB against 552 MB (was 1,200 MB). Fork at df1a67b8, glue at fb68e32.

Juno (lucinanet's t2 run, A76, cold): `playing` at 8.90 s; the script thread was busy from 3.0 to 9.0 s. kevlar_base (9.7 MB):
parse 1,556 ms, top-level generation 646 ms, execution 3,374 ms of which 1,186 ms first-call generation. x86: parse 539, execute
817 (316 first-call generation).

## Plan, ranked by expected saving on Juno

1. **Persistent script cache** (V8 code cache, SpiderMonkey stencil cache, JSC CachedTypes): a warm visit skips parse, generation
   and first-call generation of a script it has seen. The per-function cache (`fncache`) already covers generation, but its keys
   come from the AST, so it still parses. Expected: most of kevlar's 2.7 s of parse and compile on a warm load. Verus on the cache
   key, validation and serialized-bytecode bounds, with mutants.
2. **Parse off the script thread while the bytes arrive** (V8 streaming, SpiderMonkey off-thread): on Juno kevlar's 1.56 s parse
   sits inside the script thread's busy span. The glue already compiles stencils off-thread; check why kevlar is parsed on the
   script thread there.
3. **Syntax-only pre-parse of inner functions** (V8 preparser, SpiderMonkey syntax parser, JSC SyntaxChecker): the cold parse
   builds no AST for functions that are not called yet; with lazy re-parse already in place, this is the missing half.
4. **GC without a copy window**: a non-moving MarkedSpace-style heap (size classes, then eden) replaces the copying semispace,
   whose peak (window plus live data while it copies) is LucinaJS's remaining memory gap. Verus on size-class and mark-bit
   accounting, with mutants.
5. **LLInt-style interpreter**: dispatch measured on the A76, inline caches with Verus-checked shape checks; for kevlar's
   top-level execution (817 ms x86, about 2.2 s Juno without first-call generation).

Same rules for each: test262 failure list identical, before and after tables, benchmarks on a quiet machine (a Verus runner cannot run the
browser: glibc 2.39 there, the binary needs 2.43).

## Rename plan (after the YouTube work lands)

| Step | What | Cost |
|---|---|---|
| 1 | Crates: `brimstone_core` -> `lucinajs_core`, `brimstone_macros`, `brimstone_serialized_heap`, `brimstone_icu_collections`, binary `brimstone` -> `lucinajs_*` | fork: 178 lines in 52 files; the glue depends on it under the old name with `package = "lucinajs_core"` first, then its 2,431 mentions in 55 files are renamed mechanically. About 1 h of edits, then a full rebuild of everything that uses the glue (a Servo build is about 1 h on the reference Linux development machine), the glue suite and test262 |
| 2 | Fork repo renamed to `LucinaJS` (the checkout under `third_party/brimstone`) | absolute paths in the glue's `mozjs-sys/Cargo.toml`, the build scripts and `third_party/UPSTREAMS.toml` (`ours`); one coordinated pause of everything that builds against the fork |
| 3 | Docs and labels | done or cheap: this file, doc titles, bench labels `lucinajs-*` (from 2026-11-08) |

Kept as they are: the `LUCINANET_JS_*` switches (engine-neutral), git history, and the upstream remote.

## Persistent script cache: design (2026-11-08)

Goal (Juno first): a warm visit skips parse, top-level generation and the first-call generation of the
functions the last visit ran. kevlar_base on Juno: parse 1,192 ms + lazy compiles 1,229 ms of the 6.0 s script-thread gap.

Originals (behaviour, not code): JSC CachedTypes (runtime/CachedTypes.cpp: an encoder that writes unlinked code blocks and the
`UnlinkedFunctionExecutable`s of not-yet-compiled functions with their source ranges; a decoder that rebuilds them lazily),
V8's code cache (created after the script has run, so it holds the functions compiled during the visit; lazy functions keep
their preparse scope data), SpiderMonkey's stencil cache (an XDR-encoded stencil, lazy functions as "lazy script" stubs with
closed-over bindings).

What the existing per-function cache (`bytecode/fncache`) gives and does not: its templates are keyed by a structural hash of
the function's AST and re-read source-derived values from the AST at instantiation (`Prov`), so a hit still needs a parse.
It stays as is (it saves generation on a cold visit); the script cache is a separate image.

### The image

Key: SipHash-1-3 (the fncache secret) of: the script's source bytes, the engine build hash, the cache partition (top-level
site, as fncache partitions). Value, one file per script (the fncache disk store's directory, size cap and eviction):

1. Header: magic, format version, engine build hash, source length and hash (checked on load), counts and section offsets.
2. Heap section: a subgraph serializer in the spirit of `gc/heap_serializer.rs`. Every heap item reachable from the script's
   function (BytecodeFunctions, constant tables, exception handlers, scope names, global names, strings, numbers, BigInts,
   regexps as source + flags, template objects, class names) is written with its pointers turned into either an index into the
   image or an external reference: a permanent-space offset (stable for the same build: the permanent heap comes from the
   embedded snapshot) or a well-known item (an interned name, a symbol). Realm, source file and caches are not written: they
   are re-created on load (fresh inline caches, the loading realm, a SourceFile pointing at the cached text).
3. Lazy-function section: every function not compiled in the last visit becomes a stub with its source range, `ReparseInfo`
   and the scope it is nested in, so it re-parses its own text on first call exactly as today (B1 lazy re-parse).
4. Scope section: the compacted scope tree those stubs need (`ScopeTree::compact_into`'s result) as a flat table: nodes (id,
   parent, kind, flags), bindings (name, kind tag, index, flags, VM location) and extra var names. `BindingKind::Function`'s
   AST pointer becomes the function's source position.

When: written after the script's top level has run and again when idle after the page settles (V8 writes the code cache after
execution), off the script thread (the image is plain bytes once captured), never more than once per idle period per script.

Load: on a hit for a classic script (Servo's script fetch, glue `JS__Compile*`), validate (header, source hash, build hash,
section bounds: Verus on the key, the validation and the bounds), deserialize into the heap, and run as a CompiledScript; a
failed validation falls back to the normal compile and removes the entry.

### Checks

- Oracle: `LUCINANET_JS_SCRIPT_CACHE=verify` compiles normally as well and compares the bytecode of every function (as fncache
  verify mode); 0 mismatches on YouTube, Discord, test262 run twice (cold, then warm from the cache).
- test262 failure list identical with the cache off, cold and warm.
- Verus with mutants: key composition (every input changes the key), header/section validation (no read past the image, counts
  consistent), the external-reference ranges.
- Fuzz the decoder with corrupted images (must reject, never crash).

### Stages

1. Build hash: `fncache::disk::build_id` hashes only the package version and a fixed string, so two engine builds of the same
   version share a disk cache (a latent bug in the per-function disk store too). Replace it with a hash of the git commit and
   the opcode table, set by the build script.
2. Image writer and reader for fully compiled scripts (no lazy stubs), with verify mode: proves the heap section.
3. Lazy stubs and the scope section: the real win (kevlar keeps most functions lazy).
4. Servo/glue wiring, eviction, the write-after-run timing; bench on x86 and Juno (warm time to play).
