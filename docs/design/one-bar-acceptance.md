# LucinaNet one bar: the acceptance pictures

`docs/design/one-bar.md` closes with seven criteria, the first six of which are to be shown as "headless pictures through
the UI crate's shot path, both glass and classic, at 1x and 2x". They are in `one-bar/` next to this file: 40 windows - the
six states at both scales in both chromes, the fold, the field's three marks, and the four addresses the emphasised
registrable domain is proved on - each with a crop of exactly its row (`-band.png`), and `manifest.txt` with the numbers
the row measured as it was drawn.

The pictures are written by `crates/lucinanet-ui/src/shot.rs`, by the same `png` writer the existing `--render` shot path
uses, from the same `Browser`. To redraw them: `scripts/onebar-shots.sh [directory]` (default `$WORK_DIR/lucinanet-one-bar/shots`),
then copy the set here. The test that writes them, `shot::acceptance::the_one_bar_pictures`, is `#[ignore]`d so a normal
`cargo test -p lucinanet-ui` stays picture-free; it asserts while it writes, so a row that stops matching the design fails
the script instead of quietly producing a plausible-looking PNG.

## What a picture is, and what it is not

Each shot is a real `Browser` in a `lucina_app::Harness` at the window's size, told the state through the `Msg::News`
variants the engine sends (`Tabs`, `Url`, `Title`, `Loading`, `History`, `Bookmarked`, and `HistoryHits` for the
suggestions), then painted once. The editing shot gets there the person's way: `Ctrl+L`, then typed text through the
toolkit. Nothing is re-derived for the picture - the state that reaches the row is the state the row is drawn from.

The suggested visits are stamped a day apart, newest first, rather than all with the clock. Stamped with `now()`, their
`last_visit_us` differed by the microseconds the three stamps happened to take, and the omnibox breaks a score tie on
recency and then on the URL - so the same code, run twice, drew the sheet's rows in another order. The pictures are meant
to be re-runnable and compared, and with the stamps spaced they are: two runs of the set now agree on every file but the
loading sweep, noted under the gaps.

The measurement is read back out of the layout the draw pass actually used (`Browser::one_bar()`), not recomputed from
`onebar.rs`: the band height, the y the page starts at, the field width in dp, which `Fit` was reached, how many tabs the
overflow chip shed, and whether any part falls outside the band. Then the written PNG is decoded again and the same seam is
measured from the ink: the fixture paints its first three device rows in the page's own ink (`shot::INK`, 0x3a3a3c,
`shot::INK_RULE_H`), so for every picture the test finds the first row at x = width-300 that is that ink and requires it to
be exactly `band_h`, with the row above it not ink - and on the editing pictures it does the same at x = 10, left of the
sheet. A test that only re-read the layout numbers would prove the arithmetic; this proves the drawing put the page there.

The rule is in the fixture because it was not visible without it. The page's header band is cream (255,248,225) and the
glass bar's backdrop is sampled from the page, so a bar over a cream-topped page came out the same cream: the pictures
showed a wide pale band under the row that could be read as an empty second row, and nothing in the pixels said where the
chrome ended. With the ink rule the seam is unambiguous, and the glass bar gained what it should always have shown - the
page's top reflected into its bottom edge. Down the bar at 1x the red channel now runs 253 at row 0 to 180 at row 43; at
2x, 254 to 186. The bar is still glass, not dark, and the classic chrome is unchanged but for the rule under it.

**This is not a live engine.** The precedent for that exists - a real page has been shot through
a headless full-rust `lucinanet-servo` probe and `--render` - but that route cannot produce four of these six states: one
headless load has one tab, types nothing, asks nothing and never sets a mac inset. So the page under the row is a fixture
(`shot::fixture`: an ink rule across its top rows, a pale header band, then lines of ink on paper) chosen so the glass
backdrop sampling has something representative to
sample, and the engine is the UI crate's existing stand-in. What the pictures therefore prove is the chrome - geometry,
order, fitting, materials, hit-testing - and nothing about the engine. A real Mac running the real thing remains
the acceptance target; these pictures are what stands in for it until then.

Scale comes from the harness, not from a knob: `k = (w/1024).min(h/768)` clamped to 0.6..3.0 (`lucina-app/src/ui.rs`), so
1366x768 is 1x, 2048x1536 is 2x, and 400x300 sits on the 0.6 floor.

## The six states, and the criteria they answer

`one-tab`, `three-tabs`, `twelve-tabs`, `editing-suggestions`, `loading`, `mac-inset` - each in `-classic-1x`,
`-classic-2x`, `-glass-1x`, `-glass-2x`. The numbers below are from `manifest.txt`.

| Picture stem | Criterion | Band | Page starts | Field | Fit | Chip sheds |
| --- | --- | --- | --- | --- | --- | --- |
| `1-one-tab-*` | 1 | 44 px at 1x, 88 at 2x | same as band | 360 dp | Full | 0 |
| `2-three-tabs-*` | 2 | 44 / 88 | same | 360 dp | Full | 0 |
| `3-twelve-tabs-*` | 3 | 44 / 88 | same | 360 dp | Compact | 0 |
| `3b-twelve-tabs-floor-*` | 3, the fold | 26 px at the 0.6 floor | same | 247 dp | Folded | 6 |
| `4-editing-suggestions-*` | 4 | 44 / 88 | same | 640 dp | Compact | 0 |
| `5-loading-*` | 5 | 44 / 88 | same | 360 dp | Full | 0 |
| `6-mac-inset-*` | 6 | 44 / 88 | same | 360 dp | Full | 0 |

1. **One tab.** The row is 44 dp, the field is the row's only capsule, the host is centred in it, and the page starts at
   `page_y = band` in every picture - there is no second row to start under. In the pixels the same thing is said by the
   fixture's ink rule, which begins at row 44 at 1x and row 88 at 2x and is the first ink in its column.
2. **Three tabs, middle active.** `News::Tabs` is given in order and the layout places the field between its neighbours;
   the crop shows the two neighbours' titles at full form, so nothing had to shrink.
3. **Twelve tabs.** At 1366 wide all eleven inactive tabs fit as 30 dp circles with the field still at its 360 dp
   preferred width, so the design's rule ("otherwise ALL inactive tabs go compact") is reached without the chip and with
   nothing clipped. The chip is only reachable at a width the harness clamps down to, so `3b` is a second twelve-tab shot
   at 400x300: there the field drops to 247 dp - over its 220 dp minimum - and six tabs fold into the chip showing `6`.
   That is the `Folded` end of the fitting rule, and it is asserted, not merely shown.
4. **Editing with suggestions.** The field has grown to its 640 dp editing maximum, every inactive tab has gone compact
   regardless of space, the lock has become the search mark, the text is left-aligned with the caret, and the sheet hangs
   from the field's bottom edge at the field's width, over the page, four rows deep out of the eight the design allows.
5. **Loading.** The 2 dp accent line along the field's bottom edge is in the crop; see the gap below for what it does.
6. **macOS inset.** The shot is built with `chrome::Settings { left_inset: 78.0, .. }`, the same field
   `LUCINANET_CHROME_LEFT_INSET` sets on a real run: the first control - the back arrow - starts at x >= 78, and the test
   asserts it. The traffic lights themselves are the toolkit's window chrome (full-size content view,
   transparent title bar) and are not drawn here, so the picture shows the reserved space, not the buttons sitting in it.
7. **Band height reported to the engine.** Asserted in the same test that writes every picture, for all 26:
   `band_h == (44 * k).round()` and `page_rect.y == band_h`. It was arithmetic only; with the fixture's rule it is ink as
   well, and the test measures the ink - so a chrome that reserved more than 44 points would now fail on the pixels, not
   only on the numbers.

## The emphasis pictures

`emphasis-signin`, `emphasis-bbc`, `emphasis-io`, `emphasis-ip` - each in `-classic-2x` and `-glass-2x`, one tab each so the
field has the row to itself. They are not one of the design's seven criteria; they are the acceptance pictures for the
registrable domain, on the four shapes of host that make the rule worth arguing about: a subdomain in front of the site
(`accounts.google.com`), a two-suffix public domain where the site is the last two labels (`news.bbc.co.uk` → `bbc.co.uk`),
a host that is itself one registrable domain (`me.github.io`), and an address with no site at all (`127.0.0.1:8080`, where
the list places nothing and the whole host is the emphasis).

What is asserted is read off the ink, column by column, as two levels: the strip inside the field's box must hold a single
run of columns at the darkest level and weaker columns on either side of it, with the strong median at least 1.35 times the
dim one. The check is deliberately *not* the bounding box - the box is the same rectangle for every address the row shows,
so a proof about it would hold equally for a field that drew nothing of the kind. That is the lesson of the scaled-planes
bug, and the reason these eight pictures exist: the planes inside a correct box were the wrong ones, and only looking at
the pixels said so.

## The gaps, stated plainly

**The line is not progress.** `News::Loading` is a bool: the engine reports whether the page is loading and never how
far, so nothing can be measured to draw a fraction. The glass chrome sweeps a 2 dp accent segment (a third of the capsule
wide, 8 px at least) along the capsule's bottom edge on a 1.6 s loop, driven by the frame clock the kit hands it and kept
alive by `GlassChrome::wake_after`; the classic chrome draws a static full-width 2 dp accent line on the same edge,
because the classic path has no frame clock and asks for no repaints. The difference is deliberate and bounded: motion
where the chrome already animates, a plain "something is happening" line where it does not. Before, the
classic chrome showed loading only as the reload glyph becoming a `x`. Neither line is a percentage, and the design's
"progress line" wording should be read as this until the engine reports a fraction.

**The field shows the display, not the bare host.** `docs/design/one-bar.md` says "the host only (registrable domain
emphasised)"; what the row draws now is `lucinanet-omnibox`'s unfocused display of the address, which is the host with its
port *and* the path after it, `https://` dropped, a `www.` dropped when the engine named a registrable domain, and the tail
elided to `…` at the field's own width. The emphasis is what the design asked for and is now real: the engine computes each
committed URL's registrable domain from the Public Suffix List it ships (`lucinanet-servo`'s `domains`, arriving with the tab
as `TabInfo::registrable_domain`), and both chromes draw that range in the full ink with the rest of the address dimmed
beside it. The width rule is shared, the glyphs are not: each chrome measures with its own font and gets its own fit.

**What the emphasis pictures do not prove.** They are handed the answer - `shot::acceptance` states each host's registrable
domain in the fixture, exactly as the engine would - so they prove that a given answer is *drawn*, two levels of ink in one
line, and not that the list gives that answer. The list itself is proved in the engine's own tests, which run the real
`pub_domains` against the same table of hosts; the harness has no engine to ask.

**The pictures are the harness, not Servo**, for the reason above. Anything that needs a real page under the glass -
backdrop sampling of an actual photograph, a real favicon, a real title's font metrics - is not proven here.

**One kit bug, found by these pictures and worked around.** `lucina_glass::Canvas::push_clip` takes the radius as given
and does not clamp it to the box, so the kit's own `radius::PILL` (999) on a 2 dp row clips the row away entirely: the
loading line was painted and invisible. `stroke_rrect` and `fill_rrect` clamp, the clip does not. `glass.rs` now clips
the sweep with the capsule's own radius (`cap.h / 2`) and says why in a comment; the regression test
`a_load_inks_the_accent_along_the_capsules_bottom_edge` counts actual accent pixels on that edge, so the failure mode is
now a failing test rather than an empty-looking picture. The bug is the kit's to fix, in LucinaOS; this tree works
around it and says so in the comment, rather than patching a dependency from here.

**The sweep's phase is the one thing a re-run moves.** The glass sweep reads the wall clock (`self.start.elapsed()`), so a
fresh run paints it a frame further along: the committed `5-loading-glass-*` and a new run differ by 1 px at 1x and 10 px
at 2x, all of it at the segment's leading edge along the capsule's bottom. That is what an animation is, not drift - the
classic loading line is static and byte-identical between runs, and every other picture is too. The committed set is one
run's output, and a re-run reports how many files it disagrees with, so this is visible rather than silent.
