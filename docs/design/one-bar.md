# LucinaNet: one top bar

This design replaces the tab strip + toolbar pair in both the glass and the classic chrome with a single top bar.

## The idea

One row. The active tab IS the address field. Inactive tabs sit on either side of it in their real order. There is no
second row, ever: everything that used to need one hangs below the bar as a sheet and goes away again.

```
macOS:  [● ● ●] [←] [→]  [tab] [tab] [ 🔒  host.tld              ↻ ] [tab] [+]  ......drag......  [★] [⋯]
Juno:           [←] [→]  [tab] [tab] [ 🔒  host.tld              ↻ ] [tab] [+]  ......drag......  [★] [⋯]
```

## Geometry (points; multiply by the scale factor)

- Bar height 44. Capsules (tabs, field) 30 high, radius 15, centred vertically. Buttons 28 x 28, radius 8. Gaps 6.
  Side padding 10.
- macOS: the window buttons are the system's, drawn over the bar's left end (full-size content view, transparent title
  bar, no title text). Reserve 78 wide for them; the first control starts at x = 78. No native title bar.
- Juno: no window buttons; the first control starts at x = 10.
- Order, left to right: back, forward, the tabs in tab order (the active one rendered as the field), new tab (+),
  flexible empty space (window drag region; double-click zooms on macOS), bookmark star, menu (⋯).
- Reload is not a bar button: it is the icon at the right end INSIDE the field (stop while loading). Cmd/Ctrl+R stays.

## Widths

- Field (active tab), not editing: min 220, preferred 360, max 520.
- Field while editing: grows to max 640 (never past the star), animates 120 ms.
- Inactive tab, full form: favicon + title, min 96, max 160, title ellipsised.
- Inactive tab, compact form: favicon only, 30 wide (a circle). Close button appears on hover in the full form only.
- Fitting rule, evaluated on every layout: give the field its preferred width; give every inactive tab its full form if
  they all fit; otherwise ALL inactive tabs go compact; if compact tabs still do not fit, the field shrinks to its min;
  if they still do not fit, the tabs furthest from the active one collapse into one overflow chip showing the count
  (click opens the tab list sheet). The field never goes below its min and is never pushed out of view.
- While editing: all inactive tabs are compact regardless of space.

## What the field shows

- Not editing: lock or warning icon, then the host only (registrable domain emphasised, as `lucinanet-omnibox` computes
  it), centred. Page title is not shown here; it is the tab's tooltip and the window title for the OS.
- Which of the two is decided by the address's scheme alone (`security_of`): https and wss are the lock, everything else is
  the warning. The engine reports nothing about the certificate, so nothing about it is shown.
- Click, Cmd/Ctrl+L, or typing with focus on the bar: the full URL, selected, left-aligned, caret; the search icon
  replaces the lock.
- Loading: a 2-point progress line along the bottom edge inside the field, accent colour; no spinner elsewhere.
- Suggestions: a sheet hanging from the field's bottom edge, same width as the field, glass material, max 8 rows.
  Esc restores the host view.

## What hangs below instead of adding a row

Find in page, permission prompts, downloads, the tab list (overflow), the menu, bookmark edit: each is a sheet anchored
under the control that opened it, over the page, dismissed by Esc or a click outside. No bookmarks bar. No status bar
(the hovered link's URL appears as a small pill at the bottom-left of the page, only while hovering).

## Material

Glass variant: the existing tokens (lucina-glass `tokens::glass`, `color`, `radius`, `ty`), one hairline under the bar,
the field one step brighter than the bar, the accent only on focus ring and progress line. The backdrop sampling stays as
it is, over a 44-point band instead of the two-row band. Classic variant: same geometry, solid fills.

## Keyboard and pointer (unchanged unless listed)

- Cmd/Ctrl+T new tab (opens with the field in editing state), Cmd/Ctrl+W close, Ctrl+Tab / Cmd+Opt+arrows switch,
  Cmd/Ctrl+1..9 jump, Cmd/Ctrl+L focus field.
- Drag a tab to reorder; the active tab drags by its lock icon area or by a drag started on its edge 8 points; a drag
  that starts on the field's text is a text selection when editing and a tab drag when not.
- Middle-click a tab closes it. Scroll wheel over the tabs scrolls nothing (no hidden tabs: overflow is the chip).

## Out of scope for this change

Page-under-glass (engine top inset), tab groups, a sidebar, vertical tabs.

## Acceptance (headless pictures through the UI crate's shot path, both glass and classic, at 1x and 2x)

1. One tab: bar 44 points high, field centred host, no second row; picture height of the chrome band = 44 x scale.
2. Three tabs, middle active: order preserved, field between its neighbours.
3. Twelve tabs: inactive compact, field at its min or more, overflow chip only if needed; nothing clipped.
4. Editing with suggestions: field grown, neighbours compact, sheet hanging below over the page.
5. Loading: progress line inside the field.
6. macOS: no native title bar; controls start right of the window buttons (x >= 78).
7. A test that the chrome band height reported to the engine is exactly 44 x scale in every state above.
