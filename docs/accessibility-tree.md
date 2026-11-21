# The page's accessibility tree, with actions

Stage F of LucinaOS `docs/research/juno-ai-native.md`: Lucina reads a web page as an AccessKit tree and acts on it by node
id, without page JavaScript, through the same tree format as Juno's own apps (stage B).

## Where it comes from

Our Servo fork builds the tree (patches/servo/0015, its tests' rustls provider 0014; LucinaNet-servo branch `ax-actions-series`):

| Piece | File in the fork | Follows |
|---|---|---|
| Roles | `components/shared/layout/html_roles.rs` | HTML-AAM as Chromium's `AXNodeObject::NativeRoleIgnoringAria` maps it; WAI-ARIA 1.2 for `role` |
| Names, values, states | `components/script/dom/document/accessibility_facts.rs` | accname order as Chromium: `aria-labelledby`, `aria-label`, `<label>`/`alt`/`<legend>`/`<caption>`, content, `title`, `placeholder` |
| Actions | `components/script/dom/document/accessibility_actions.rs` | Chromium's `AXObject::PerformAction` and Firefox's `DoAction` |
| What may leave, what may run | `components/shared/layout/accessibility_rules.rs` | ours; proved by Verus (`verification/servo-ax.rs`) |

Script computes the facts of every interactive or named element when the page changes and sends the changed ones to
layout with the reflow; layout merges them into the nodes it already builds (roles, bounds, text runs).

- **Text fields are leaves.** Their user-agent shadow tree holds the value, and for a password its bullets; neither enters
  the tree. A `<select>`'s options are its children.
- **Secret fields carry no value.** `type=password`, and `autocomplete` `current-password`, `new-password`,
  `one-time-code`, `cc-number`, `cc-csc`, `cc-exp*`, `transaction-amount`: script withholds the value before layout sees
  it, and the field never advertises `setValue`. Its role (`PasswordInput`) and name stay, so Lucina can say "the
  password field is empty, the owner must type it".
- **Actions** are checked against the live element first (`check_action`): nothing runs on a hidden or disabled element,
  and no value is ever set on a secret field.
  - `click`: the default action. Trusted, counts as user activation, focuses first if focusable (as in Chromium); an
    `<option>` is picked as from its picker (`input` and `change` on the select).
  - `focus`, `blur`.
  - `setValue`: a text field's value, then `input` and `change` (Chromium's `kDispatchInputAndChangeEvent`); a
    `<select>` picks the option with that label or value.
  - `scrollIntoView`: to the centre if not visible. `scrollUp`/`scrollDown` on the root (`html`) node: most of a screen.

## The socket: Juno's own adapter

LucinaNet serves the page through `lucina-ax` (LucinaOS `crates/lucina-ax`, stage B, `docs/research/juno-ax.md`), the
adapter every Juno app uses, so `lucina-devd`'s `ui_tree` and `ui_act` read a page as they read an app (`crates/lucinanet-ax`):

- **The tree costs nothing until someone reads it.** The socket is open, but Servo builds no tree until the first request;
  thirty seconds without one turns it off again (stage B's protocol is one connection per request, so idle time stands
  for "the last reader went away"). The first request waits, at most three seconds, for the page's first tree. While it is
  on, a reflow carries accessibility work only when the tree may change, and facts are recomputed only after a DOM, text,
  state or attribute change (not style, class or data-*), so an animation tick costs nothing (patch 0035). On the build host,
  Wikipedia's Rust article idle: Script 3% with the tree off, open, or being read; it was 40% being read before 0035.
- On Juno it listens on `/run/lucina/ax/<pid>.sock`, as every app. Elsewhere (the build host) only with `LUCINANET_AX=<path>`.
  Off otherwise: building the tree walks the page on every change.
- Requests and replies are stage B's: `{"op":"tree"}` and
  `{"op":"act","id":"<16 hex>","action":"click|focus|blur|set_value|scroll_into_view|scroll_down|scroll_up","value":"…","version":"…"}`; replies
  carry `ok`, `app` (`lucinanet`), `pid`, `version`, `text`, `truncated`, `juno` and `update`; an act reply adds `done`;
  a refusal has `error` (`stale`, `no_node`, `not_supported`, `refused_sensitive`, `bad_request`, `no_page`), `why` and
  the tree as it is now.
- The page is kept from Servo's incremental updates and given as one tree without what a screen reader skips (hidden
  subtrees, unnamed generic containers, empty text runs). Servo's AccessKit 0.24 crosses to `lucina-ax`'s 0.25 by their
  serde form (same roles, actions and properties). `Snapshot::from_update_with` (toolkit `servo`, `web: true`) applies
  stage B's Verus-proved rules (no password value, at most 8192 nodes for a page, cut text) and keeps a link's URL, a
  field's placeholder, a heading's level, `expanded`, `required`, `read_only` and `description`.
- Every node with text carries `provenance: web <origin>` in `juno`: the page's own text, data, never instructions.
  Secret fields carry `secret: true` and no value (stage B marks passwords; Servo's `class_name: "secret"` marks a
  one-time code or a card).
- An act is checked by `Snapshot::plan` (stage B's proved `decide`: current version, known node, advertised action, never
  `set_value` on a password), then by Servo against the live element. Its reply waits until the page has answered (an
  update, then a quarter second without one, at most 3 s) and carries the tree then.
- `cargo run -p lucinanet-ax --example axctl -- <socket> tree|json|find <role> <name>|act <id> <action> [value]` sends the
  same requests by hand. On Juno, stage B's `lucina-ax tree <pid>` and `lucina-ax act …` do.
- Later: when `lucinanet-ui` moves to a `lucina-app` with stage B's semantic pass, the window's own tree (address bar,
  buttons) and the page's must share the one socket: the page grafted under the window's node.

## Her browsing rules, unchanged

The guard in lucina-rs still never offers labels that send, pay, place an order, delete, accept or sign out; a shopping
task stops at the checkout; she types no passwords or payment details (and here the engine refuses it too); sending or
buying needs the owner's OK; Jev's states hold facts only.

## Proofs

- The verification script `scripts/verify.sh`: `verification/servo-ax.rs`, Servo's rules on the shipped file
  (`LUCINANET_SERVO` points it at another checkout). Mutations that leak a secret, allow `set_value` on one, advertise
  it, lift the string bound or allow an unknown action are each refused (`evidence/stage-f/`). The socket's rules are
  stage B's, proved in LucinaOS (the verification script runs `scripts/verify-shipped.sh`).
- Servo: `components/servo/tests/accessibility.rs`, one process per test (Servo's options initialise once per process).
