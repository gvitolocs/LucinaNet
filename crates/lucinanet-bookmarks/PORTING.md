# lucinanet-bookmarks: porting record

Pure crate: bookmark tree, own JSON format, Netscape HTML and Firefox places JSON import/export. No UI, no file I/O, no favicons.
Originals read at the pinned revisions (fetched into `<work-dir>/lucinanet/batch1/src/bm/` on the build host, not in the repo):
Firefox `mozilla-firefox/firefox@33abb900fc16` (MPL-2.0, short `@33abb90`), Chromium `chromium/chromium@e9a46735de97` (BSD-3-Clause, short `@e9a4673`).
All paths below returned 200 at the pinned sha.

## Sources

| Source | Decision | Licence | What was taken |
|---|---|---|---|
| Firefox `toolkit/components/places/BookmarkHTMLUtils.sys.mjs` | adapt | MPL-2.0 | Export: header lines, `<H1>` + menu children + Toolbar/Unfiled as flagged folders (only when non-empty), 4-space indent, `<DL><p>` / `</DL><p>` / root `</DL>`, attribute order `HREF ADD_DATE LAST_MODIFIED SHORTCUTURL TAGS`, dates written only when non-zero and floored to seconds, escaping of `& < > " '`. Import: attribute names, `A` without href ignored, `dateAdded = lastModified` when only the latter exists, tag split on `,`, `parseInt` date reading with a fallback to "now". The tokenizer is ours. |
| Firefox `toolkit/components/places/BookmarkJSONUtils.sys.mjs` | adapt | MPL-2.0 | `rootToFolderGuidMap` root names, children sorted by `index`, tags as a comma string (dropped when longer than `MAX_TAG_LENGTH`), `lastModified` before `dateAdded` is not kept, `tagsFolder` skipped. |
| Firefox `toolkit/components/places/PlacesUtils.sys.mjs` | adapt | MPL-2.0 | `TYPE_X_MOZ_PLACE*` strings, `typeCode` values 1/2/3 (container, bookmark, separator as in the backup), `DB_URL_LENGTH_MAX = 65536`. |
| Firefox `toolkit/components/places/Bookmarks.sys.mjs` | reference | MPL-2.0 | `MAX_TAG_LENGTH = 100`; insert/validate rules. Its `index` semantics for `update` (final index) were **not** used for `move_to`, see deviations. |
| Chromium `components/bookmarks/browser/bookmark_model.cc` (`BookmarkModel::Move`) | adapt | BSD-3-Clause | Move index semantics (slot before removal; no-op at `old`/`old+1`; later slot is decremented), "cannot move into own subtree" rule, permanent nodes cannot be moved. Behaviour only, no table, so no `NOTICE`. |
| Chromium `components/bookmarks/browser/bookmark_node.h` | reference | BSD-3-Clause | Node shape (type, title, url, added/modified times). |
| Chromium `titled_url_index.cc`, `url_index.cc`, `bookmark_codec.cc`, `bookmark_html_reader.cc`, `importer_data_types.h` | reference | BSD-3-Clause | Not fetched (see deviations): search matches words as substrings of title and URL; the HTML reader's job is covered by Firefox's reader, which is the stricter one. Nothing copied. |

The "Netscape bookmark file" format is an informal de-facto format without a licence; the shapes here are facts of that format.

## Choices

- Netscape import puts items directly into `into`, in file order; a folder flagged `PERSONAL_TOOLBAR_FOLDER` / `UNFILED_BOOKMARKS_FOLDER` becomes an ordinary folder with its own title. Firefox's "replace" restore is not used on purpose: import never deletes.
  Consequence: `import(export(x))` into an empty store returns the Menu's tree unchanged, and Toolbar and Other come back as folders named "Bookmarks Toolbar" / "Other Bookmarks" inside the destination.
- `skip_duplicates` compares normalised URLs inside the destination folder only (a lazily built set per folder, so big files stay linear). A folder with the same title is never merged.
- Places export `guid`: ROOT and the three built-ins use Firefox's own guids (`root________`, `menu________`, `toolbar_____`, `unfiled_____`); any other node gets its id as 12 base-64 digits (Firefox's guid alphabet), so distinct ids give distinct guids and the output is deterministic. Import ignores `guid`.
- Places export writes the roots in Firefox's order (menu, toolbar, unfiled). A `Mobile Bookmarks` folder created by import is an ordinary folder under Other when exported again.
- Places import creates the `Mobile Bookmarks` folder only when the mobile root has children.
- Search ranking (ours): per word 3 for a title match, 2 for tags or keyword, 1 when only the URL matches; best total first, ties in tree order.
- Only the three built-in folders live in ROOT; adding to ROOT is `ProtectedNode`.
- `set_url` on a non-bookmark returns `InvalidUrl("not a bookmark")`; `set_tags`, `set_keyword` and `rename` on a node of the wrong kind return `Format(..)`. The spec's error enum has no better fit and is kept as written.
- Importers cap folder nesting at 48 levels (deeper folders are filled into the deepest allowed one), so the recursive writers cannot be driven into a stack overflow by a hostile file. Own-format and places JSON are bounded by serde_json's recursion limit.

## Deviations from the brief or spec (original wins, or a gap in the spec)

1. `<HR>`: Firefox writes `<HR>` with no line break (`_writeSeparator` never calls `_writeLine`, so the next `<DT>` lands on the same line). We write a newline after it. The file parses identically and stays readable.
2. Firefox omits an empty Toolbar or Unfiled folder from the HTML export (`if (root.children && root.children.length)`). We follow that; the spec only said "then Toolbar and Other as flagged folders".
3. Dates: when only `LAST_MODIFIED` is present Firefox takes it as the added date too, and a missing `LAST_MODIFIED` becomes the added date (spec: "falls back to `now_us`" for non-numbers). Both missing, or not a number: `now_us`. `parseInt` semantics (leading digits) are kept, so `12abc` reads as 12. A modified time before the added time is raised to the added time (Firefox's Bookmarks API would reject it).
4. Chromium's `bookmark_html_reader.cc` and the other Chromium files in the spec's list were not fetched. The spec's own instruction was to look the reader up under `user_data_importer` / `chrome/utility/importer`; the Firefox reader already defines every behaviour the spec's rules list, and Chromium's `BookmarkModel::Move` (the one Chromium rule the spec names as binding) was read in full.
5. Separators: the exporter writes no dates for `<HR>` and the importer gives them `now_us`; Firefox's reader does the same (comment at `BookmarkHTMLUtils.sys.mjs` line 386: separator dates are not imported).
6. The base was `main` at the time the workspace was created (36a9b9d), not `58acc9f` from the spec; the crate touches nothing outside its directory, so this does not matter. Cargo members there already include `lucinanet-history`.
