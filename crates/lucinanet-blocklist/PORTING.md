# lucinanet-blocklist: porting record

A matcher for the network-filter subset of the Adblock-style syntax. No list is bundled, fetched or named in tests; every test rule is hand-written and generic.

| Source | Licence | Decision | What was taken |
| --- | --- | --- | --- |
| Adblock Plus filter cheatsheet (adblockplus.org/filter-cheatsheet) | documentation | reference | Syntax facts only: `||`, `|`, `^`, `*`, `@@`, option names. |
| uBlock Origin static filter syntax wiki (github.com/gorhill/uBlock/wiki/Static-filter-syntax) | documentation | reference | Syntax facts only: option names and aliases, which options are out of scope. |
| uBlock Origin, Adblock Plus, EasyList, EasyPrivacy (code and list text) | GPLv3 / GPLv3+ or CC BY-SA 3.0 | reject | Nothing read, nothing copied. |
| Brave adblock-rust | MPL-2.0 | reference (not read) | Only the idea, given in the spec, of indexing by token. The index here is written from that description. |
| Chromium url_pattern_index, indexed_ruleset | BSD-3-Clause | reference (not read) | Same idea. No table or formula ported, so no NOTICE. |
| Firefox tracking protection (Disconnect / URL-classifier lists) | MPL-2.0 | reject | Different mechanism and list format. |

The pinned Firefox and Chromium sources were not read for this crate: the spec names the syntax documents as the only input.

## Choices and deviations from the spec

- `domain::host_of` returns the host as written (a `&str` cannot be lower-cased in place, and the signature forbids allocating). `Engine::check` lower-cases the URL itself before it asks. Scheme matching is case-insensitive.
- `/regex/` is recognised as a pattern that starts and ends with `/` (also with trailing options): unsupported, as ABP treats any such text as a regular expression.
- Options after the last `$`: an empty option list counts as none.
- A negated-only type list (`$~script`) means "every type except Document and script".
- `$domain=`: exclusion wins; with no page host (`source_host` None) an include list never matches and an exclude-only list does.
- Page exemption: an `@@` filter with the `document` type is also tried against `https://<source_host>/`; when its pattern matches (and its `domain=` allows), every request from that page is an exception (type and party options of that filter are not applied to those requests). `important` blocks still win.
- Index: each filter is stored under its rarest key (document frequency over the loaded list). Keys are token runs of at least 3 of `[a-z0-9%]` whose pattern neighbours fix the run boundary (separator, non-token literal, `|` or `||` start, `|` end), plus, for `||` filters, the first label (any length) looked up at every label start of the request host. This also covers the "host suffixes" item of the spec. Filters without a key go in the always-candidate list.
- Matching: segments between `*` are matched leftmost; linear per segment, no backtracking. `^` also matches the end of the URL.
- Invalid lines (reported in `invalid`): empty filter, anchors only, empty or conflicting `domain=` / party options.
- Not supported, reported as unsupported: regular expressions and every option outside the spec list (redirect, removeparam, csp, badfilter, denyallow, header, to, from, method, all, popup, ...).
