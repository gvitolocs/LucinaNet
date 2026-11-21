# Where LucinaNet's fonts come from

A page's text needs a face for every character in it. LucinaNet has no font of its own beyond the one the toolkit
bar is drawn in, so this is the whole story of what gets found, in what order, and what a box on screen means.

Two engines, one variable. Both read `LUCINANET_FONT_DIRS`, a colon-separated list of directories, and both fall
back to a built-in default when it is unset:

| Engine | Code | Scan | Default when the variable is unset |
| --- | --- | --- | --- |
| Servo | `components/fonts/platform/freetype/font_list.rs` (fork) | recursive, `ttf` `otf` `ttc` `otc` | `/usr/share/fonts/dejavu:/usr/share/fonts/noto:/usr/share/fonts/truetype/dejavu` |
| Blitz | `blitz/src/engine.rs` | one level, `ttf` `otf` `ttc` | the same three paths |

Neither default is a good guess at a real system. A Debian-layout host keeps its faces under `/usr/share/fonts/
truetype/<package>` and `/usr/share/fonts/opentype/<package>`, so `/usr/share/fonts/dejavu` and `/usr/share/fonts/
noto` do not exist and only the DejaVu faces are found; and Blitz does not descend, so the one path that does exist
yields nothing at all. LucinaNet therefore sets the variable itself, in `crates/lucinanet-ui/src/fonts.rs`, through `ensure_font_env()`,
which `lucinanet_ui::main` calls before it looks at the command line - so `--render` counts, the desktop window
counts, and Juno counts, which never compiles the `desktop` feature and so never had any font setup at all beyond the
fork's three guessed paths.

## The directories, per OS

`system_font_dirs` returns, in order:

**Linux, and Juno with its LucinaOS tree**

    /usr/share/fonts
    /usr/local/share/fonts
    $HOME/.local/share/fonts
    $HOME/.fonts
    <prefix>/share/lucinanet/fonts        # beside the binary's install prefix; what the packager ships

The first four are fontconfig's own list, so anything `fc-list` can name, the engine can find — with the exception
that there is no fontconfig here to consult for a directory someone added to `/etc/fonts/local.conf`. The last is a
convention for an install tree (`/usr/local/share/lucinanet/fonts` beside `/usr/local/bin`); no script in this
repository fills it today, and a directory that is not there is skipped.

Juno needs no entry of its own: LucinaOS's `scripts/juno-fonts.sh`, run by its `juno-root-image.sh`, installs
`NotoSans<Script>-Regular.ttf` for 14 scripts under `/usr/share/fonts/truetype/noto` and, with `--cjk`,
`NotoSansCJK-Regular.ttc` under `/usr/share/fonts/noto`; a script in LucinaNet's own `scripts/` puts the same CJK
collection in that second directory. Both are under `/usr/share/fonts`, which the scan walks recursively.
`$HOME/.local/share/fonts` is where a user drops a font without root, and is on the list for that reason.

**macOS**

    /System/Library/Fonts
    /System/Library/Fonts/Supplemental
    /Library/Fonts
    ~/Library/Fonts
    <bundle>/Contents/Resources/fonts     # only if a packager puts fonts there; nothing in this repo does today

Servo on macOS does not use these: it asks CoreText (`components/fonts/platform/macos/font_list.rs`), which already
searches all four domains, and `LUCINANET_FONT_DIRS` is simply inert there. Blitz does need them, since it registers
files itself and asks for no system fonts. The list is written from Apple's documentation of the CoreText search
domains and **has not been run on a Mac**: the project lead can check it with `scripts/glyph-check.sh <binary>`,
which needs only a shell, python3 and the engine's own `--render`.

`LUCINANET_FONT_DIRS` set by the caller is the whole list, with the system directories left out. That is deliberate:
`tests/wpt/run-reftests.sh`, `raster/capture-traces.sh` and the WPT container all name a directory precisely so that
no other face can join and move a metric. `LUCINANET_FONT_DIRS_EXTRA` appends to whichever list is in effect and
never removes one, so it is safe to leave exported.

## The names, per OS

Discovery decides which *files* are known; the fallback chain decides which of them gets asked about a character
none of the page's own families covers. The chain lives in the fork and names, on Linux, `Noto Color Emoji` for a
character whose presentation preference is emoji, `Noto Sans` for Latin/Greek/Cyrillic, a `Noto Sans <Script>` per
block for the rest, `Noto Sans CJK JP/SC/TC/KR`, `TakaoPGothic` and `WenQuanYi Micro Hei` for Japanese and Chinese,
then `DejaVu Serif`, `FreeSerif`, `DejaVu Sans`, `DejaVu Sans Mono`, `FreeSans`, `Symbola`, `Droid Sans Fallback`. On
macOS it names Apple's: `Apple Color Emoji`, `Hiragino Sans`/`Hiragino Kaku Gothic ProN` for Japanese, `Songti SC`
and `SimSun-ExtB` for Chinese, `Apple SD Gothic Neo`/`Nanum Gothic` for Korean, `Geeza Pro` for Arabic, and the
`* Sangam MN` faces for the Indic scripts. The generic families resolve to `DejaVu Sans`/`DejaVu Serif`/`DejaVu Sans
Mono` on Linux and `Helvetica`/`Times`/`Menlo` on macOS.

One rule in that chain is worth knowing when a box looks wrong: for a character carrying U+FE0F, the emoji
presentation preference, `components/fonts/font.rs:858` rejects any candidate face that has no CBDT/CBLC/sbix bitmap
and no COLR table. So a ⚙️ in a title whose own family is not installed is a box even though DejaVu Sans has U+2699
as an outline, and only an installed colour face clears it. Without U+FE0F the same character draws from the outline.
`tests/fonts/presentation.html` is the eight cases side by side.

Blitz, which does its own fallback wiring in `blitz/src/engine.rs`, is behind this: it maps every non-monospace
generic to DejaVu Sans rather than to a serif, and its script fallback lists name no emoji face at all, so a 🦀 in a
title is a box there even with `fonts-noto-color-emoji` installed. Both are one-line changes to the
`set_generic_families` and `set_fallbacks` calls, and neither is verified on this host: Blitz is a second workspace
with a cold target tree, and the check that would catch a mistake in it belongs to a machine that had a free slot.

Nothing here invents a font. Every name is one a distribution or Apple ships, and the fix for a box is to have its
file in a scanned directory: on a Debian host, `apt install fonts-noto-core fonts-noto-cjk fonts-noto-color-emoji`
turns emoji and CJK from boxes into glyphs without changing anything in this repository, because the fallback chain
already asked for those families by name. A host with none of them installed will draw a box for 🦀 no matter how
correct the directory list is — no browser on that machine can do better, including the ones using fontconfig.

## What a box means, and how to count them

`scripts/glyph-check.sh <binary>` renders `tests/fonts/glyphs.html` — one grid cell per sampled code point, one row
per Unicode block, 1385 cells from Latin to Egyptian Hieroglyphs — and then renders
`tests/fonts/glyphs-notdef.html`, which is the same page with every sample replaced by U+FFFF. No font assigns
U+FFFF, so each cell of the second page holds the `.notdef` outline of whichever face the engine fell back to for a
character it could not draw. `tests/fonts/notdef-count.py` compares the two PNGs cell by cell, without PIL: a cell
whose pixels are byte-identical to its reference cell was drawn by that same fallback face, and is counted as a box.
A second, weaker signal calls a cell "tofu-shaped" when its ink is a closed frame with an empty middle; that catches
a box drawn by a different face than the reference's, which happens because the chain a character walks depends on
its script, but a real glyph that happens to be a white square matches it too, so it is advice for the eye rather
than a count. The page carries a control row of twenty-one code points that Unicode defines as unassigned: it must
come out as 21 boxes on any host, and the script fails the run if it does not, because that means the detector —
not the fonts — is what changed.
