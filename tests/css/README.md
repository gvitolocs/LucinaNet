# CSS rule object gate

`tests/css/at-rule-objects.sh [binary] [pages...]` renders the fixtures in this directory against a built
binary and fails if any of them panics or stops printing its `css-test: PASS` line. One process per page,
because a Rust panic in the script thread takes the whole tab with it.

A page spec is `name:switches`, where `switches` is the `LUCINANET_CSS_PREFS` value for that page (`none`
runs with nothing on). The pages:

- **register-four** — the four at-rules B15 names (`@counter-style`, `@container`, `@-moz-document`,
  `@font-palette-values`) alone in a sheet. They are dropped by the parser at this stylo pin, so the page
  is the evidence for that: `length=1` and all four `present … no`.
- **at-rule-objects** — one sheet of at-rules read by index, the register's recipe.
- **insert-rule** — each at-rule through `insertRule`: the sheet must keep it honestly or throw `SyntaxError`.
- **sheet-mutate** — read a rule, then rewrite the `<style>` text that holds it (`CSSRule::update_rule`),
  with a twin element so the contents stay shared and the copy-on-write path is taken.
- **position-try-mutate** — the same rewrite for an at-rule that has no interface of its own.

`css-test.js` is the shared harness; its `honest()` check is the point of the gate — a rule object counts
as honest only if it has a `CSS…Rule` constructor, a numeric `type`, a non-empty `cssText`, and reachable
`parentStyleSheet`/`parentRule`. An empty `cssText` stub fails.

Exit codes: 0 all pages passed, 1 any panic or missing PASS line, 2 binary not executable. `KEEP=1` keeps
the per-page logs and PNGs in `$TMPDIR/lucinanet-css-pages.$$`. Needs no network; serves this repo over
`PORT` (default 8126).
