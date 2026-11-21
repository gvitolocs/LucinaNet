# WebAssembly module inspection

Read a `.wasm` binary and print what it is and what it asks of an engine:

```sh
cargo run --release --locked --offline --manifest-path tools/wasm-inspect/Cargo.toml -- \
    <file.wasm> [--json]
```

The tool only parses. `wasmparser` walks the sections and reads every operator
of every function body; nothing here links, instantiates or runs the module, so
the file's code is never executed. The parser is pinned to the version wasmi
2.0.0 builds on, so the verdict comes from the same reader as the engine that
would have to accept the module.

The report holds the total size, every section with its exact byte span, each
import (module, name, kind, signature or limits) and export, the memory, table
and global limits with their shared and memory64 flags, the start function, the
function count, and the `producers` and `target_features` custom sections — the
toolchain that built the file and the feature names it recorded, each with a `+`
for built-in or a `-` for ruled out.

Then comes the feature verdict: the proposals the code actually uses — simd,
threads, bulk memory, reference types, multi-value, sign extension, saturating
truncation, exceptions, tail calls, GC types, extended const, memory64, relaxed
SIMD, custom page sizes — each with an operator count and a file offset for a
first example. Multi-value, extended const and memory64 have no operators of
their own, so their evidence comes from function types, block types and constant
expressions instead. The last line names the wasmi `Config` flags a run would
need, and says plainly which of them our engine has no switch for: threads,
exceptions and GC among them. `--json` emits the same report as one document.

The tests build their own tiny modules with `wasm-encoder` and check the
verdicts against them.
