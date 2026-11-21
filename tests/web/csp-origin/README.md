# csp-origin: an opaque ancestor origin must not panic the engine

Reported from a 30-site render run: https://www.amazon.com/ killed a script thread with

    thread 'ScriptTh ...' panicked at components/script/dom/security/csp.rs:225:
    Must always be able to parse document origin: RelativeUrlWithoutBase

The CSP check that answers "may this frame be embedded here" walks up the ancestor chain and
turns each ancestor's origin into a URL to match `frame-ancestors` sources against. It did that
with `Url::parse(origin.ascii_serialization()).expect(...)`, and an **opaque** origin
serialises to the string `"null"`, which is not an absolute URL. Any page can put an opaque
document above a frame that has its own Content-Security-Policy: `sandbox` on the iframe
element, `sandbox` in the parent's CSP header, or a `data:` document.

The pages here do exactly that, each with a different construction, and `child-csp.html` is the
CSP-bearing frame whose check crashed. `child.js` logging `csp-origin: child loaded` is the
positive signal that the frame actually ran, so a blank error page cannot pass as "no panic".

* `opaque-parent-srcdoc.html` - `sandbox="allow-scripts"` plus `srcdoc`.
* `opaque-parent-data.html` - a `data:` document that nests the CSP frame.
* `opaque-parent-csp-sandbox.html` - the parent is made opaque by its own CSP `sandbox` header
  (served by `serve.py` from `*.html.csp`, which is why a plain `http.server` will not do).
* `top-level-data-iframe` - the URL is handed to the binary as a top-level `data:` document.
* `control-same-origin.html` - ordinary same-origin parent; must render before and after the fix.

The two sandboxed parents say `sandbox="allow-scripts"` / `sandbox allow-scripts` rather than an
empty flag set: sandboxing is inherited by nested browsing contexts, so a bare `sandbox=""` also
switches scripting off inside `child-csp.html` and the `child.js` signal can never appear. The
opaque origin - the thing that triggers the bug - comes from `allow-same-origin` being *absent*,
which `allow-scripts` does not change.

**Known open.** On those two pages the gate accepts, instead of the `child.js` signal, a
`Fetching classic script failed Blocked by Content-Security-Policy ... /child.js` line. The
inherited sandbox flags give `child-csp.html` an opaque document origin, and this engine matches
`'self'` against that origin, so a script served from the same http origin can never satisfy
`script-src 'self'` inside a sandboxed frame; CSP3 matches `'self'` against the URL origin of the
protected resource, so Chrome and Firefox load it. That is an older difference in the CSP
source-matching code, unrelated to the Amazon panic and not fixed here - but a panic, or a child
script that neither ran nor was blocked by CSP, still fails the gate.

Run the gate against a built binary (it needs no network, only the local server it starts):

    tests/web/csp-origin/run.sh /path/to/lucinanet-servo          # expects no panic
    MODE=repro tests/web/csp-origin/run.sh /path/to/pre-fix-binary # demonstrates the panic

The fix lives in the engine fork (`servo_url::ImmutableOrigin::ascii_url()` plus an error path
in `components/script/dom/security/csp.rs`); an opaque ancestor becomes a stand-in URL that no
`frame-ancestors` source except an explicit `null` can match, which is what other engines do.
