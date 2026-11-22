# LucinaNet browser chrome in the glass design (mockups)

**Real implementation:** the top bar these screens propose now ships in `crates/lucinanet-ui/src/chrome.rs` (the mode switch, `Layout`, the blur rule)
and `crates/lucinanet-ui/src/glass.rs` (the bars drawn with the kit). The overlays below (sheets, dialogs, the downloads strip,
settings) are not in it yet, so what follows stays the design reference.

Mockups only: no Servo, no wiring into `lucinanet-ui`. Rendered by `docs/design/browser-glass/render/` (a standalone crate
over the `lucina-glass` kit of LucinaOS, `lucinaos/crates/lucina-glass`; it needs the kit with the browser widgets):

    cargo build --offline -j 4                          # in docs/design/browser-glass/render, its own workspace
    target/debug/browser-glass-render docs/design/browser-glass [juno|phone|tablet] [shot]

Rules followed: the LucinaOS design rules (`docs/design/rules.md` in LucinaOS, outside this tree; only its crates are vendored here). Glass on the control layer (bars, popovers, sheets), the thicker
`MATERIAL` for content that is read (suggestions, settings cards, dialog, select list). The one accent (`ACCENT`) marks
selection and activity only: the active tab, the selected card, the focused address field, toggles that are on, the download
ring, the chosen row. Primary buttons are the kit's light `PAPER` button, not the accent. Sentence case, verbs on buttons,
no filler, no illustrations (site marks are plain initials, pages are plain text), tap targets 44 dp on Diana.

Sizes (dp): Juno 1366 x 768 at 1.5 px/dp; Diana phone 360 x 800 at 3 (1080 x 2400 px, the POCO X3 "surya" panel and the kit's phone canvas); Diana tablet 1024 x 768 at 1.5. The shell's own
status bar sits above the Juno window and is not drawn; the phone shows the kit's status bar.

Phone metrics used (Diana has no Android-style inset API; the device is documented only at 1080 x 2400 and on the
360 x 800 dp / 3 px per dp canvas, so these insets are design assumptions to confirm on the
device): status bar 32 dp (96 px, holds the front camera hole); gesture inset 24 dp (72 px) with the 134 x 5 dp indicator of
the kit drawn in it; keyboard 248 dp = 4 rows of 46 dp keys at 54 dp pitch plus 8 dp top padding (224 dp) plus the 24 dp
gesture inset, so it has no empty band under the keys. Dock 56 dp above the gesture inset, address pill 48 dp above the dock.
While typing the pill sits 8 dp above the keyboard.

## Layout rationale

- **Juno and tablet (top chrome):** two glass capsules. Tab strip (tabs 36 dp high on Juno, 44 on tablet, a new-tab
  button, the overview button with the tab count) over a toolbar (back, forward, reload, address field, download, menu).
  Back, forward and reload sit left, where the pointer starts; menu and downloads right. The address field keeps the lock on
  the left (site info) and the star on the right. Popovers hang under the control that opened them.
- **Phone (bottom chrome):** within thumb reach, an address pill over a dock (back, forward, new tab, tabs with count,
  menu). No tab strip: tabs live in the overview. The page keeps the whole screen. Menu, site info, downloads and the select
  list are bottom sheets over a dimmed page; the permission prompt is a card above the address pill, not modal. While typing,
  the pill rises above the keyboard, the dock is hidden and suggestions grow upwards from the pill.
- **Tablet** is the Juno layout with 44 dp controls and 52 dp rows; its popovers are the same as Juno's.
- Existing behaviour kept: pop-ups open as tabs, a loading tab shows a spinner, downloads show "n MB of m MB", "Saved to ..."
  and the failure reason with cancel, open and retry; the JS dialog shows the page's origin as its title and the page's own
  words as the message (Cancel, OK as the engine answers); the select list is drawn under the page's element.

## Screens (per layout: `juno-*`, `diana-phone-*`, `diana-tablet-*`)

| File suffix | Screen |
| --- | --- |
| `01-browse` | A page with tab strip, address bar and toolbar (phone: pill and dock). |
| `02-tabs` | Tab overview: cards with page thumbnails, active card ringed in the accent, Tabs and Private segments, New tab, Done (phone: dock with Close all, New tab, Done). |
| `03-address` | Address field focused, text typed, suggestions: search, bookmarks, history; first row selected. |
| `04-menu` | Find, Bookmarks, History, Downloads as icon-over-label actions; Zoom stepper; New private window; Settings. |
| `05a-permission` | A location prompt: field shows the pending-permission mark; Block, Allow. |
| `05b-site-info` | The lock's sheet: origin, "Connection is secure", per-site permissions with switches, Clear site data. |
| `06-downloads` | Running (progress in the accent), saved, failed rows; cancel, show, retry. |
| `07-settings` | `lucina://settings/privacy` on the night sky: section list (selected = accent) and cards on the thick material. |
| `08a-dialog` | A confirm dialog over the dimmed page. |
| `08b-select` | A select list under its element (phone: a picker sheet), chosen option in the accent with a check. |

## Accent

The accent is `color::ACCENT` of `lucina-glass/src/tokens.rs`, **#7F8FE0**, the token the glass Home uses for the owner's
calendar dots, the selected day and the keyboard focus ring (`home/mod.rs`, `layouts/blocks.rs::focus_ring`: 1.6 dp stroke, 3 dp
outside). `ACCENT_SOFT` (#7F8FE0 at 18 %) is its fill. The render crate holds no accent literal: the selected tab (`ACCENT_SOFT`
fill, 1.2 dp `ACCENT` outline), the focused address field (the Home's focus ring, same numbers), a chosen row, an active switch
(`ACCENT` track), the download ring and progress and the selected card all use these two tokens. The only colour literals left
are the placeholder pages' own grounds and ink (`page.rs`), which stand for a website, not for the chrome.

## Adaptive chrome

Proposal, implemented in the kit's `widgets/adaptive.rs` (pure functions, float arithmetic, tested; not part of the Verus-checked rules). The bars (tab strip,
toolbar, address pill, dock) take their tint, ink and rim from the page behind them, as Apple's Liquid Glass switches between
light and dark under its content and Fluent's acrylic takes its tint from what is behind it.

- **Input:** `lum`, the mean luma 0.2126 R + 0.7152 G + 0.0722 B (sRGB-encoded, 0..1) of the blurred page in the rectangle
  behind that bar (`region_luma`, every third pixel). Each bar is measured on its own, so the tab strip and the dock may differ.
- **Output** `adapt(lum) -> {tint, ink, ink2, ink3, rim}`, with `t = smoothstep(0.60, 0.70, lum)`:
  - dark glass (`t` = 0): tint `CARBON` alpha 150, ink white, rim white alpha 70;
  - light glass (`t` = 1): tint `PAPER` alpha 176, ink `CARBON`, rim black alpha 46;
  - between them a linear blend; `ink2` is ink at 62 %, `ink3` ink at 34 % (disabled controls, marks); accent is unchanged.
- **Why 0.60 to 0.70, not 0.5:** the dark glass is itself dark, so white ink still reads over a mid-grey page (for example a light
  page dimmed under a modal, luma about 0.48); only a truly light page flips the bar. The band is narrow so ink never sits at
  a middling contrast, and the flip should animate over `motion::SHORT` (220 ms).
- **Not adaptive on purpose:** popovers, sheets, dialogs, suggestions and settings cards. They carry text over a thick
  material (rule 7) and stay dark, which also keeps one family of overlays. New tokens needed: `GLASS_TINT_LIGHT`, `GLASS_RIM_LIGHT`.
- **Renders:** `juno-01b-browse-light-dark.png` and `diana-phone-01b-browse-light-dark.png` (light page left, dark page right);
  every other screen uses the same rule (the light page gives light bars, the night sky and dimmed pages give dark bars).

Icon sizes were checked at 1.5 px/dp: the lock is now 20 dp (a 12.8 x 16.4 dp glyph, as large as the star), the close cross in
tabs 16 dp (10 dp visual), chevrons 18 dp, the pin 20 dp, bar icons 0.58 of the control (21 dp on Juno, 25 on touch).

## Kit components used

From `lucina-glass`: tokens, `Paint` (glass, shadow, fit, text), `Canvas` (shapes, blur, `blit_region` for thumbnails), `Type`, the
night sky, `chrome::status_bar`, `chrome::spinner`, `screens::home_indicator`, icons Clock, Doc, Bell, ArrowUpRight.

## Promoted to the kit (`lucina-glass`)

The render crate draws with the kit's versions; what it keeps of its own is the layout of each screen (`chrome.rs`, `overlays.rs`,
`main.rs`), the placeholder pages and thin wrappers in `ui.rs` that give the old call sites the kit's controls.

- **Icons** (`icons.rs`, enum `Icon`): Back, Forward, Reload, Close, Plus, Minus, Lock, Search, Star, StarFill, Dots, Download, Gear,
  Private, Tabs, Check, Camera, Pin, Popup, Folder, Globe and ChevronRight (History is the kit's Clock). The kit's icon test checks
  each at 16 dp (at least half the box in its longer side, nothing outside it).
- **Adaptive glass** (`widgets/adaptive.rs`): `adapt`, `blend`, `look`, `region_luma`, `adaptive_bar`, and `Adaptive`, which animates the
  flip over `motion::SHORT`. New tokens `GLASS_BAR_TINT`, `GLASS_TINT_LIGHT`, `GLASS_RIM_LIGHT`, `INK_ON_LIGHT`.
- **Controls** (`widgets/`, one file each): `bar.rs` (bar, `BarButton`), `text_field.rs` (`TextField`: caret, blink, scroll, clear),
  `switch.rs`, `stepper.rs`, `list_row.rs`, `popover.rs`, `bottom_sheet.rs`, `tab_chip.rs` (and the site mark), `tab_card.rs`,
  `dialog_card.rs`, `button.rs` (`PushButton`), `progress_bar.rs`, `segmented_plain.rs`; `ink.rs` holds the shared inks, text helpers,
  the focus ring and `FocusOrder`.
- **Hit testing and focus:** each control remembers where it was last drawn and answers `hit(x, y)` for that rectangle grown to at
  least `min_hit` (44 dp, `space::TAP`, by default; a pointer layout whose neighbours touch sets it to the drawn size). Each has
  `focused` and draws the Home's focus ring. Tab order is `FocusOrder`.
- **Rules:** `browser_rules.rs` holds the integer rules (hit area and test, focus step, switch thumb ends, stepper, download fraction and
  fill, text scroll and caret, tab width, popover/sheet/dialog placement), proved by Verus like `rules.rs`.
- **Animation:** every moving control holds a tween read at `Paint::now` (switch flip, popover and sheet in and out, segmented thumb,
  progress, adaptive tint), with the kit's easing. The kit's glass has no global opacity, so a popover or sheet slides; it does not fade.

Still open for the kit: multi-line ellipsis and a non-Latin fallback for page titles in tabs (text); the address field's unfocused
state (lock, host, path, star) is composed in `chrome.rs` from kit pieces and is not a control of its own yet.

## Notes

- The glass samples the blurred page; over the night sky (settings, tab overview) it reads as in the Home concept.
- The phone keyboard in `03-address` is a stand-in for the system keyboard, drawn only to size the layout.
