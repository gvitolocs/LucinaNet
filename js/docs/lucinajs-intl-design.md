# LucinaJS Intl (ECMA-402) design

Status: scoping and measurement, 2026-11-11. No engine source was changed. Throwaway spike crates and raw numbers were kept in
a temporary working directory outside this tree; they are not committed.

Naming: LucinaNet is the browser, LucinaEngine the web engine, LucinaJS the JavaScript engine. "Upstream Brimstone" means the
project LucinaJS was forked from. The fork is this tree's `third_party/brimstone` (main `75b28e46` when this
was started), the glue is `js/`. Fork paths below are relative to `src/js/` of the fork unless they start with another name.

Evidence tags: **[src]** read from source, **[meas]** measured on the reference Linux development machine, **[docs]** from docs.rs, crates.io files in the cargo
registry or the spec, **[inf]** inference or estimate.

## 0. Summary

- LucinaJS has no `Intl` global. `Number`, `Date`, `Array`, `TypedArray` `toLocaleString` ignore the locale, `BigInt.prototype.toLocaleString` does
  not exist, `toLocaleUpperCase/LowerCase` are aliases of the non-locale versions, `localeCompare` always uses an English collator, and
  `Date` has no time zone (local time is UTC). [src]
- ICU4X 2.1 (the family the fork already pins) covers most of ECMA-402 from compiled (baked) data. Stable crates: decimal, plurals, list,
  datetime, calendar, time, segmenter, collator, locale. Experimental (`icu_experimental` 0.4.0): relative time, display names,
  currency, percent, units, compact decimal, duration. [docs, meas]
- One blocker found: `icu_experimental` 0.4.0 `DurationFormatter::try_new` needs a `ValidatedDurationFormatterOptions` whose type is
  re-exported only from a private module, so it cannot be constructed outside the crate. `Intl.DurationFormat` waits for a newer
  `icu_experimental` or needs our own formatter. [src, meas: spike did not compile]
- 2029 of the 3337 files in test262 `intl402` are `Temporal/`. The pure Intl part is 1308 files. Baseline today: 2998 of 6674 variants
  pass, all from Temporal and the non-Intl globals; every Intl-proper test fails. [meas]
- Cost of everything (all eight stage-1 and stage-2 features, 20 common web locales) is in section 3. Binary size is dominated by
  time zone names and display names, not by number or list formatting.
- Locale decision (2026-11-12): the default build ships English, Danish and Italian (en, da, it); see section 10. Cost of everything in one
  binary, from section 3: three = 1,716,304 B (~1.7 MB), common20 = 5,585,416 B (~5.6 MB), all = 14,512,224 B (~14.5 MB).
- ICU4X pin decision (2026-11-12), in two moves:
  1. Collator + `localeCompare` and NumberFormat decimal land first on the present 2.1 pins; nothing delays them.
  2. Then one change moves ICU4X to the 2.3 line — the line Boa's tables are written against — alone in its commit/branch: every `icu_*` pin
     moved together, the data crate regenerated for en, da and it, the binary-size delta reported per locale set, test262 `intl402` counts reported before and after,
     and the Mac and Juno builds checked.
  3. After it: compact notation from the stable `icu_decimal` `CompactDecimalFormatter` (stable since 2.2), and currency from the
     `icu_experimental` release that matches 2.3, written in the fork (Boa has none), USD first because example-site.com formats
     `Intl.NumberFormat("en-US",{style:"currency",currency:"USD"})`.
  Rule: never two generations of ICU4X in one engine — no `icu_experimental` 0.4 beside the 2.1 core crates.

## 1. What exists in the fork already

### 1.1 Locale-sensitive code [src]

| What | Where | State |
|---|---|---|
| `String.prototype.localeCompare` | `runtime/intrinsics/string_prototype.rs:375-394` | Ignores both locales and options; calls `ICU.collator.compare_utf8` with a collator built for `en` with default options |
| `toLocaleLowerCase` / `toLocaleUpperCase` | `string_prototype.rs:80-81` | Registered to the same Rust functions as `toLowerCase`/`toUpperCase` (`StringPrototype_to_lower_case`); no language-tagged case mapping |
| `String.prototype.normalize` | `string_prototype.rs:456-` | Uses `ICU.normalizers` (NFC/NFD/NFKC/NFKD), not locale-sensitive |
| `Number.prototype.toLocaleString` | `number_prototype.rs:167-170` | Calls `to_string_impl` with undefined radix: same as `toString()` |
| `Date.prototype.toLocaleString/DateString/TimeString` | `date_prototype.rs:979-1000` | Return the `toString`/`toDateString`/`toTimeString` forms |
| `Array.prototype.toLocaleString`, `%TypedArray%.prototype.toLocaleString` | `array_prototype.rs:1610-1640`, `typed_array_prototype.rs:1396-1424` | Spec-shaped: invoke element `toLocaleString` with no arguments; `locales`/`options` are not forwarded (ECMA-402 requires forwarding) |
| `Object.prototype.toLocaleString` | `object_prototype_object.rs:138` | Calls `toString`, as in ECMA-262 |
| `BigInt.prototype.toLocaleString` | none | `bigint_prototype.rs:25-27` registers only `toString` and `valueOf` |
| Temporal `toLocaleString` | `runtime/intrinsics/temporal/{instant,duration,plain_*,zoned_date_time}_prototype.rs` (e.g. `plain_date_prototype.rs:56,439`, `zoned_date_time_prototype.rs:68,635`) | Present on all eight prototypes; return the ISO/`toString` form |
| `Intl` | nothing | No match for `Intl` in `runtime/`, `parser/`, `build.rs`, or `common/`. `typeof Intl` throws ReferenceError |
| ICU4X use | `common/icu.rs`, `common/icu_data/mod.rs`, `common/unicode_property.rs`, `brimstone_icu_collections/build.rs` | Properties, normalizers, case mapping, regexp case-folding sets, one `en` collator. Data is baked from `icu/data/` |
| Lexer/parser/build.rs | `build.rs` only hashes sources into `LUCINAJS_BUILD_ID` (`build.rs:1-30`) | Nothing locale related in the parser or lexer |

ICU data setup [src]: `icu/generate_data.sh` runs `icu4x-datagen 2.1.1 --format baked --use-separate-crates --locales en` over the 67 markers in
`icu/markers.txt` (collation 7, normalizer 6, `CaseMapV1`, property enum/binary sets); output `icu/data` is 3.0 MB of `.rs.data`
text. `common/icu.rs:23` fixes `DEFAULT_LOCALE = en` and `common/icu.rs:181` builds the whole `ICU` struct in one `LazyLock`.
The engine's Cargo pins are `icu_casemap =2.1.1, icu_collator =2.1.2, icu_collections =2.1.1, icu_locale =2.1.1, icu_normalizer =2.1.1,
icu_properties =2.1.2, icu_provider =2.3.1, icu_provider_baked =2.1.0, icu_provider_adapters =2.1.1, zerotrie =0.2.5, zerovec =0.11.8`
(`third_party/brimstone/Cargo.toml`, workspace section).

### 1.2 Date and time zones in the fork today [src]

- `runtime/intrinsics/date_object.rs:174-185`: `local_time(t)` and `utc(t)` are `// TODO: Handle time zones` and return `t`. So local time is
  UTC: `getTimezoneOffset()` is always 0 (`date_prototype.rs:242-251`), `new Date(2025,0,1)` is UTC midnight.
- Temporal does have time zones: `runtime/context.rs:150,203,543` holds `temporal_provider: CompiledTzdbProvider` (crate
  `timezone_provider` 0.2.1 with feature `tzif`, bundled IANA data from `jiff-tzdb`), and `Temporal.Now.timeZoneId` reads the system zone
  through `temporal_rs::sys` -> `iana_time_zone::get_timezone()` (`temporal_now_object.rs:55-64`).
- The glue's test262 runner (`tools/lucinanet-test262`) never sets a time zone. [src]

### 1.3 How builtin objects are registered (the pattern a new `Intl` follows) [src]

Plain namespace object (closest model: `Reflect`, `Math`, `JSON`, `Temporal`):
1. `runtime/intrinsics/reflect_object.rs:17-50`: `pub struct ReflectObject;` with `new(cx, realm) -> AllocResult<Handle<ObjectValue>>`; it uses
   `IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)`, lists methods with `intrinsic_methods!(cx, builder, { name RustId (length), ... })`
   (macro at `runtime/intrinsic_builder.rs:221`), sets `@@toStringTag` with `builder.to_string_tag(...)`.
2. Method bodies use `runtime_fn! { fn name(cx, this_value, arguments) {...} }`.
3. Function table: `runtime/intrinsics/rust_runtime.rs` maps each `RustId` to its function, e.g. `(ReflectObject_apply, ReflectObject::apply)` at line 876.
4. `runtime/intrinsics/intrinsics.rs`: add an `Intrinsic` enum variant (JSON at 182, Reflect at 211, Temporal at 226) and register it in
   `Intrinsics::initialize` with `register_intrinsic!(Reflect, ReflectObject);` (line 487; Temporal objects 489-499).
5. `runtime/intrinsics/globals.rs:158-161`: expose it on the global object with `intrinsic_prop!(cx.names.reflect(), Reflect);`.
6. `runtime/builtin_names.rs` holds every property name (e.g. `(to_locale_string, "toLocaleString")` at 538); new names are added there.
7. `runtime/intrinsics/mod.rs:94` lists the module.

Constructor with prototype and instances (closest model: `WeakRef`, and the Temporal classes): `weak_ref_constructor.rs` + `weak_ref_prototype.rs` +
`weak_ref_object.rs`, registered with `register_intrinsic_pair!(WeakRefPrototype, WeakRefConstructor);` (`intrinsics.rs:376`, macro at 293-304). The
instance type is declared with `extend_object!` (`weak_ref_object.rs:15`), allocated with `ObjectBuilder::<T>::new(cx).constructor_proto(constructor, Intrinsic::XPrototype)`,
implements `HeapItem { byte_size, visit_pointers }`, and needs a `HeapItemKind` entry (`runtime/gc/heap_item.rs:225`) and an
`ordinary_object_shape!` entry (`runtime/shape_registry.rs:140`).

Consequence for Intl: heap objects have **no finalizer**; they hold plain memory (Temporal objects store `HeapUnaligned<temporal_rs::X>`,
`temporal/instant_object.rs:15-20`). An ICU4X formatter owns heap allocations and cannot sit inside a GC object. Design rule: an Intl instance
stores only the resolved options (a small `Copy` struct plus an interned locale id), and the real ICU4X formatter lives in a per-`Context` cache
(a `HashMap<ResolvedKey, Rc<Formatter>>`, same place as `temporal_provider` at `context.rs:150`). `resolvedOptions()` is then answered from
the stored struct, and formatter construction is lazy and shared between instances with equal options. [inf]

Not present anywhere in the fork: `SharedArrayBuffer`, `Atomics` (no match in `runtime/`). [src]

## 2. ECMA-402 surface and the ICU4X crate behind each part

Spec checked: ECMA-402 14th edition draft (ECMAScript 2027 Internationalization API, draft 2026-10-23), https://tc39.es/ecma402/. [docs]
Versions below are the ones in the local cargo registry that resolve together with the fork's pins; the fork pins `icu_*` 2.1.x and
`icu_locale_core` resolves to 2.3.0 (`third_party/brimstone/Cargo.lock`). [src, docs]

| ECMA-402 piece | ICU4X crate (version used in the spike) | Status | Gaps we must write ourselves |
|---|---|---|---|
| `Intl` object, `[@@toStringTag]`, `Intl.getCanonicalLocales` | `icu_locale` 2.1.1 (`LocaleCanonicalizer`, `LocaleExpander`, aliases + likely subtags data) | stable | BCP 47 structural validation errors map to RangeError; `-u-` extension handling (`ca`, `co`, `hc`, `kf`, `kn`, `nu`) |
| `Intl.supportedValuesOf` | none | n/a | Static lists for `calendar`, `collation`, `currency`, `numberingSystem`, `timeZone`, `unit` (time zone ids from the bundled tzdb in `timezone_provider`) |
| `Intl.Locale` | `icu_locale` 2.1.1; `LocaleDirectionality`; `icu_calendar` week data | stable | `calendars`, `collations`, `hourCycles`, `numberingSystems`, `timeZones`, `weekInfo`, `textInfo` getters: only `textInfo` and (partly) `weekInfo` have ICU4X data; the rest need small tables. test262 feature `Intl.Locale-info` |
| `Intl.NumberFormat` | `icu_decimal` 2.1.1 + `fixed_decimal` 0.7.3 (digits, grouping, rounding modes, exact decimal strings); `icu_experimental` 0.4.0 `dimension::{currency,percent,units}`, `compactdecimal` (compact now from stable `icu_decimal` after the 2.3 bump, decision in section 0) | decimal stable; rest experimental | No scientific/engineering notation, no `signDisplay`/`roundingIncrement`/`roundingPriority`/`trailingZeroDisplay` logic, no `formatRange`: all done on `Decimal` in our code. `formatToParts` needs part tagging (ICU4X supports `Writeable::write_to_parts`) |
| `Intl.DateTimeFormat` | `icu_datetime` 2.1.1 (fieldsets, names, patterns, time zone names), `icu_calendar` 2.1.1 (17 calendars), `icu_time` 2.1.1 (zone ids, offsets, metazones) | stable | The ECMA-402 option bag (`weekday/era/year/month/day/dayPeriod/hour/minute/second/fractionalSecondDigits/timeZoneName/hourCycle`) must be mapped to ICU4X fieldsets; some combinations have no fieldset. No interval formats in 2.1, so `formatRange` / `formatRangeToParts` are ours (74 test262 failures are in feature `Intl.DateTimeFormat-formatRange`) |
| `Intl.Collator` | `icu_collator` 2.1.2 (strength, case level, alternate shifted, numeric, case first, tailorings per locale) | stable | `sensitivity`/`ignorePunctuation` mapping, `usage: "search"` data, `-u-co-` collation types |
| `Intl.PluralRules` | `icu_plurals` 2.1.1 (cardinal, ordinal; ranges behind its `experimental` feature) | stable (ranges experimental) | `selectRange`, `resolvedOptions().pluralCategories` (ICU4X `categories()`), digit options go through `Decimal` |
| `Intl.ListFormat` | `icu_list` 2.1.1 (and/or/unit; long/short/narrow) | stable | `formatToParts` part tagging |
| `Intl.RelativeTimeFormat` | `icu_experimental` 0.4.0 `relativetime` (all 8 units, long/short/narrow) | experimental | `numeric: "auto"` special words (yesterday, tomorrow) are in the data; `formatToParts` |
| `Intl.DisplayNames` | `icu_experimental` 0.4.0 `displaynames` (region, script, language, variant, locale); `dimension::currency` display names | experimental | No `calendar` or `dateTimeField` types in ICU4X; small tables of our own, or reject |
| `Intl.Segmenter` | `icu_segmenter` 2.1.2 (grapheme, word, sentence, line; dictionary for CJ, LSTM for th/lo/my/km) | stable | `isWordLike` from rule status; `containing()`; data size depends on models |
| `Intl.DurationFormat` | `icu_experimental` 0.4.0 `duration` | experimental, **not usable**: options type unreachable | Own implementation over `icu_list` unit lists + `icu_experimental` `units` names + digital pattern data, or wait for a fixed `icu_experimental` |
| `String.prototype.localeCompare` | `icu_collator` | stable | Wire to the Intl collator cache |
| `String.prototype.toLocaleLowerCase/UpperCase` | `icu_casemap` 2.1.1 (already used, language-tagged mapping supported) | stable | Pass the `LanguageIdentifier` from the resolved locale (tr, az, lt matter) |
| `Number/BigInt.prototype.toLocaleString` | as `NumberFormat` | | BigInt method must be added |
| `Date.prototype.toLocale{,Date,Time}String` | as `DateTimeFormat` | | Defaults `{year, month, day}` / `{hour, minute, second}` / both |
| `Array/%TypedArray%.prototype.toLocaleString` | none | | Forward `locales, options` to each element |
| Temporal `toLocaleString` | as `DateTimeFormat`, plus `Intl.DurationFormat` for `Duration` | | 2029 test262 files |
| Time zone offsets and transitions for the formatter | existing `timezone_provider` 0.2.1 (`CompiledTzdbProvider`) | in the fork | `icu_time` only needs the id and the offset; no second tzdb |

Crate stability, from docs.rs/crates.io metadata in the registry [docs]: all `icu_*` crates in the table except `icu_experimental` are 2.x
(semver stable; the `_unstable` constructors and `provider` modules are the only unstable surface). `icu_experimental` is 0.4.x: every release
may break the API. The registry also holds ICU4X 2.2, 2.3 and 2.4 releases; those do not resolve with `icu_provider =2.3.1` /
`icu_locale =2.1.1`, so using them means moving all pins together (section 6).

## 3. Data and binary size

### 3.1 Method [meas]

- Spike crate (not committed): one `main.rs`, one cargo feature per row of the table (`f_decimal`, `f_plurals`, `f_list`,
  `f_datetime` = date and time fields, `f_datetime_zone` = date, time and all time zone name styles, `f_reltime` = relative time in long, short and narrow
  for all 8 units, `f_numext` = currency + percent + unit + compact decimal long and short, `f_displaynames` = region + language + script, `f_segmenter` =
  word + sentence + grapheme + line, `f_collator` = two collators, `f_locale` = canonicalizer + expander + directionality, `f_base` = parse a locale
  and print it).
- Build: `cargo build --release`, `opt-level = 3`, `strip = true`, default `codegen-units`, no LTO. The fork's workspace `Cargo.toml` defines no `[profile]`
  section (checked: no match), so the defaults apply. Rust 1.99.0, x86_64 Linux, spike-local target dir.
- Data: baked with `icu4x-datagen 2.1.1` (installed with `--features experimental` into the spike dir) using `--format baked --use-separate-crates
  --markers <152 locale-aware and locale-free markers for the above features>` (properties, case mapping and the exemplar characters excluded) and the locale
  lists below. The generated Rust module is compiled into the binary; no file or network access at run time. The "all" column does not use datagen: it is the
  same code built with every crate's own `compiled_data` feature, i.e. the full CLDR set that ICU4X ships by default.
- A cell is the **size of the stripped release binary in bytes** for that one feature (plus `f_base`). The "delta" is that minus `f_base`. A feature is
  measured alone; two features share code and data (decimal symbols, plural rules, list patterns), so the total is less than the sum of the rows.
- Locale sets: **en** = `en` and its regional children (en-001, en-AU, ...) plus root (`und`); **three** = en, da, it (with their regional children);
  **common20** = en, da, de, sv, fr, es, it, nl, pt, pl, ru, ja, zh, ko, tr, ar, hi, nb, fi, cs; **all** = full CLDR via `compiled_data`.
  Segmenter models: none for en and three, `recommended` (LSTM for th/lo/my/km, dictionaries for ja/zh) for common20 and all.
- Always-needed data (root collation, likely subtags, locale parents, script directions, locale aliases) is part of every column; the `f_locale` row
  shows what it costs alone.
- Output was checked: the baked `da` and `de` binaries print the same text as the compiled-data binary for the features that load without
  models (`f_decimal`, `f_plurals`, `f_list`, `f_datetime`, `f_reltime`, `f_numext`, `f_displaynames`, `f_collator`).

| Feature (delta over `f_base`, bytes) | en (root + en-*) | three: en, da, it | common20 | all (full CLDR) |
|---|---:|---:|---:|---:|
| `f_base` (hello world + locale parse, absolute bytes) | 372,184 | 372,184 | 372,184 | 372,184 |
| NumberFormat: decimal only (`icu_decimal`) | 59,160 | 77,688 | 78,376 | 79,376 |
| NumberFormat extras: currency, percent, unit, compact long+short (`icu_experimental`) | 199,304 | 244,464 | 670,752 | 3,280,256 |
| PluralRules cardinal+ordinal (+ranges data) | 67,808 | 68,440 | 71,208 | 82,720 |
| ListFormat and/or/unit | 85,312 | 87,544 | 104,624 | 205,976 |
| DateTimeFormat: date and time fields, 17 calendars | 327,320 | 353,328 | 523,536 | 1,416,136 |
| DateTimeFormat + all time zone name styles (delta includes f_datetime) | 433,032 | 461,544 | 786,792 | 3,470,864 |
| RelativeTimeFormat: 8 units x long/short/narrow | 193,712 | 206,256 | 307,320 | 1,116,600 |
| DisplayNames: region, language, script | 118,352 | 147,760 | 465,472 | 3,090,976 |
| Segmenter: grapheme, word, sentence, line | 138,320 | 138,320 | 2,498,368 | 2,475,848 |
| Collator (root + tailorings; engine already ships en root) | 792,304 | 794,560 | 1,145,368 | 1,295,824 |
| Locale: canonicalizer, expander, directionality (always needed) | 175,472 | 175,472 | 175,472 | 175,472 |
| Everything above in one binary (sum is smaller than the rows) | 1,597,064 | 1,716,304 | 5,585,416 | 14,512,224 |
| Generated Rust source of the baked module (bytes, not binary size) | 3,823,095 | 4,475,498 | 24,079,587 | compiled_data |

The `Locale` row was re-measured on 2026-11-11 after the spike code for it was fixed (it did not compile in the first run).

How to read: every row except `f_base` is a **delta in bytes over `f_base`** (372,184) of the stripped release binary when that one feature is built alone for that
locale set. Each row is its own single-feature binary; features share code and data with each other, so the "Everything" row (all of them in one binary) is well
below the sum of the rows.

**What can be chosen.** Total cost of all eight stage-1 and stage-2 features, from the "Everything" row: **three locales (en, da, it) + 1,716,304 B (~1.7 MB)**,
**common20 + 5,585,416 B (~5.6 MB)**, or **full CLDR (`all`) + 14,512,224 B (~14.5 MB)**. Segmenter model note: for `en` and `three` there are no dictionary/LSTM
models to carry (the 138,320 row is the rule engines only); the common20 and `all` Segmenter rows include the `recommended` models (LSTM for th/lo/my/km,
dictionaries for ja/zh).

## 4. Time zones

Current state [src]: see 1.2. `Date` has no zone, Temporal has a bundled tzdb and a system-zone lookup.

Where zone data would come from, option by option:
- **IANA ids, offsets, transitions**: the existing `timezone_provider` 0.2.1 `CompiledTzdbProvider` (feature `tzif`, bundled `jiff-tzdb` data; the
  `jiff-tzdb` 0.1.9 crate is 280 KB on disk including docs [meas]; the fork's lock file resolves 0.1.6). Cost is already paid by Temporal. Use it for
  `Date` local time, `DateTimeFormat` `timeZone`, and `Intl.supportedValuesOf("timeZone")` (canonical ids). `timezone_provider` also has a
  `zoneinfo64` provider (ICU4C resource bundle via the `zoneinfo64` 0.3.0 crate, Unicode-3.0) that could replace jiff-tzdb later; not needed now. [docs]
- **Zone display names** (`timeZoneName: short/long/shortGeneric/longGeneric/shortOffset/longOffset`, and the metazone logic): `icu_datetime` zone markers
  (+ `icu_time` metazone periods). This is the expensive part: see the `f_datetime_zone` row. `shortOffset`/`longOffset` ("GMT+1") needs no name data;
  an engine that only supports offset forms and the raw id can skip `TimezoneNames*` markers. [meas, inf]
- **System zone for `resolvedOptions().timeZone` and `Date`**: `iana_time_zone::get_timezone()` already used by Temporal. On Linux it reads `/etc/timezone`,
  then the `/etc/localtime` symlink; it does **not** read the `TZ` environment variable (`iana-time-zone` 0.1.65 `tz_linux.rs`). [src] On macOS it
  asks CoreFoundation, on Windows the registry. [docs] On Juno (LucinaOS) neither file may exist: the browser should get the zone as an embedder
  setting (the same settings channel as `navigator.language`) and pass it into the context. A page that sets `TZ` for tests would need the env check added.
- **Cost of wiring `Date`**: replace `local_time`/`utc` (`date_object.rs:174-185`) with offset lookups through the provider (`LocalTZA(t, isUTC)`,
  including the ambiguous/skipped local time rules of ECMA-262), and change `Date.prototype.toString/toTimeString` to print the zone name. test262 and
  other users of `Date` assume UTC today; the runner should keep `TZ=UTC` semantics by setting the embedder zone to `UTC` so results do not change. About 3 worker-days. [inf]
- When the zone is unknown, fall back to `UTC` (and report `UTC` from `resolvedOptions().timeZone`), as SpiderMonkey does without a tz database. [docs]

A browser on Juno, macOS or Linux therefore needs: one embedder-provided IANA id (default from the OS), the bundled tzdb we already ship, and optionally the zone-name
data from section 3.

## 5. Staging plan

Order inside stage 1 follows what real pages call first. Source: a call-frequency count produced 2026-11-11 from 19 page logs (bbc, github, google sign-in and sign-up, hn, mdn, example-site, reddit, wikipedia, youtube start and watch, plus SpiderMonkey
reference runs). It counts calls up to about 7.5 s after load; pages that aborted on the first throw are lower bounds. [meas]

Calls seen in that probe (pages, calls):
`Intl.NumberFormat` 6 pages (incl. `supportedLocalesOf` on 5 YouTube-related logs, `notation: "compact"` on GitHub);
`Intl.DateTimeFormat` 3 pages, 105 calls (options `dateStyle`, `year/month/day`, `hour/minute`, `timeZone: "UTC"`, `timeZoneName: "short"`, `hourCycle: "h23"`;
`format` 44 times and `resolvedOptions` on GitHub); `Intl.Collator` 2 (example-site), `Intl.ListFormat` 1 (GitHub), `Intl.Locale` 1 (GitHub), `Intl.PluralRules` 1 (Reddit);
`String.toLocaleLowerCase` 4 pages; `Number.toLocaleString` 3 pages, 18 calls; `Date.toLocaleDateString` 47 calls (GitHub); `String.localeCompare` 30 calls (MDN);
`Array`/`TypedArray` `toLocaleString` 2 pages. No `Atomics`, `SharedArrayBuffer` or `WebAssembly` use was recorded. No `RelativeTimeFormat`, `Segmenter` or `DisplayNames` calls were seen.

### 5.1 Stage 1: stop pages from throwing

Order (provisional beyond the probe: only 19 page logs, calls after 7.5 s not counted):

1. **Intl core**: `Intl` object, locale list canonicalisation, `ResolveLocale`/lookup matcher, `getCanonicalLocales`, `supportedValuesOf`, shared option helpers
   (`GetOption`, `GetNumberOption`, `CoerceOptionsToObject`), the per-`Context` formatter cache, the baked data provider. (Everything below needs it.)
2. **NumberFormat** (+ `supportedLocalesOf`, `Number.prototype.toLocaleString`, `Array/TypedArray.toLocaleString` forwarding, `BigInt.prototype.toLocaleString`):
   styles decimal, percent, currency; `notation: "compact"`; fraction/significant digit options; `useGrouping`, `signDisplay`; `formatToParts`, `resolvedOptions`.
3. **DateTimeFormat** (+ `Date.prototype.toLocale*String`, `resolvedOptions().timeZone`, `format`, `formatToParts`, `timeZone` option, `dateStyle/timeStyle`, component options,
   `hourCycle`, `timeZoneName`): includes making `Date` time-zone aware (section 4).
4. **Collator** (+ `localeCompare` wired, `sensitivity`, `numeric`, `caseFirst`, `ignorePunctuation`) and **`toLocaleLowerCase/UpperCase`** with language tags.
5. **Locale** (constructor, getters that have data, `maximize/minimize`, `toString`).
6. **PluralRules**, **ListFormat**.
7. **RelativeTimeFormat** (not seen in the probe; kept in stage 1 all the same).

| Stage 1 item | New files (fork, under `runtime/intrinsics/intl/` unless noted) | Estimate (worker-days) [inf] |
|---|---|---|
| Intl core + data provider | `mod.rs`, `intl_object.rs`, `locale_negotiation.rs`, `options.rs`, `cache.rs`; `common/intl_data/` (baked module); `icu/generate_intl_data.sh` + `icu/intl_markers.txt`; `Context` field; `rust_runtime.rs`, `intrinsics.rs`, `globals.rs`, `builtin_names.rs` edits | 5 |
| Locale | `locale_{constructor,prototype,object}.rs`, small tables for `calendars`, `hourCycles`, `numberingSystems`, `collations` | 4 |
| NumberFormat | `number_format_{constructor,prototype,object}.rs`, `number_format_options.rs`, `decimal_rounding.rs`, parts builder | 8 |
| DateTimeFormat | `date_time_format_{constructor,prototype,object}.rs`, `fieldset_mapping.rs`, `date_time_parts.rs`; `date_object.rs` edit for zones | 10 (+3 for `Date` zones) |
| Collator | `collator_{constructor,prototype,object}.rs` | 2 |
| PluralRules | `plural_rules_*.rs` | 2 |
| ListFormat | `list_format_*.rs` | 1.5 |
| RelativeTimeFormat | `relative_time_format_*.rs` | 2.5 |
| toLocale wiring (String, Number, BigInt, Date, Array, TypedArray, Temporal) | edits in the existing prototype files | 2 |
| **Stage 1 total** | | **about 40 worker-days** |

Intrinsics pattern for every constructor: the `weak_ref_*` triple (section 1.3) plus a `register_intrinsic_pair!` per class, one namespace
`register_intrinsic!(Intl, IntlObject)` next to `Reflect` and `intrinsic_prop!(cx.names.intl(), Intl)` in `globals.rs`. Instances carry only the resolved option struct.

Risks (stage 1):
- ICU4X fieldsets do not cover every ECMA-402 component combination; mapping gaps show up as test262 failures and odd output, not crashes. [inf]
- `icu_experimental` is 0.x: relative time, display names, currency, percent, units and compact decimal may change on the next ICU4X bump. Pin exact versions (section 6) and keep the use behind a thin adapter module. Compact decimal: now from stable `icu_decimal` after the 2.3 bump (decision in section 0).
- Memory: `ICU` is built lazily in a single `LazyLock`; formatters must also be lazy and cached or a page that creates many `NumberFormat` objects (YouTube creates several) pays construction each time. [inf]
- Data size: DisplayNames and zone names dominate (section 3). Stage 1 can ship without zone names (offset and id forms only) and add them later.
- Date zone wiring changes every `Date` result that depended on UTC-as-local; run the whole test262 `built-ins/Date` after the change. [inf]
- Binary and compile time: the Intl baked module for common20 is a large generated Rust file (see `bytes_of_rs` in section 3). Compile it in a separate crate so the JS engine does not rebuild it. [meas, inf]

test262 `intl402` target after stage 1 (variants, baseline 2998 of 6674): about 4700 passing (70%) [inf]. Reasoning: the stage-1 classes
(NumberFormat 496, DateTimeFormat 486, Locale 304, ListFormat 162, RelativeTimeFormat 160, Collator 130, PluralRules 106, `Intl` 132 = 1976 variants, 988 files) are
currently 0%; about 65% of them are reachable with ICU4X data and standard option handling (about 1290), plus about 450 more Temporal variants that depend on `toLocaleString`, plus
the String/Number/BigInt/Date/Array tests (about 50). The rest of the Temporal failures (848 in feature `Intl.Era-monthcode`) depend on `temporal_rs` calendar support, not on this work.

### 5.2 Stage 2: the rest

| Item | Files | Estimate [inf] |
|---|---|---|
| Segmenter (grapheme, word, sentence; `containing`, `isWordLike`, iterator object) | `segmenter_*.rs`, `segments_object.rs`, `segment_iterator.rs` | 3 |
| DisplayNames (region, script, language, currency; calendar and dateTimeField from our tables) | `display_names_*.rs` | 3 |
| DurationFormat | `duration_format_*.rs`; needs own formatter until `icu_experimental` exports its options type | 4 (if unblocked: 2) |
| `formatRange`/`formatRangeToParts` for DateTimeFormat and NumberFormat | in the existing files | 3 |
| NumberFormat v3 and unified (`roundingMode`, `roundingIncrement`, `roundingPriority`, `trailingZeroDisplay`, unit style, `useGrouping` variants), `Intl.Locale` info getters, `Intl-enumeration` | in the existing files | 6-8 |
| Edge cases and conformance (taint tests, property order, prototype checks, `supportedLocalesOf` details) | across | 5-6 |
| **Stage 2 total** | | **about 25 worker-days** |

test262 `intl402` target after stage 2: about 5900 of 6674 variants (88%) [inf], leaving mostly `Intl.Era-monthcode` and non-ISO calendar Temporal tests.

Risks (stage 2): Segmenter dictionary/LSTM data is the biggest single piece of "recommended" models (see `f_segmenter` rows); DisplayNames data grows quickly with locales; DurationFormat depends on an upstream fix.

### 5.3 Conformance runner

`tools/lucinanet-test262` skips every `intl402/` file unless `--with-intl` is passed (`main.rs:137`); `--with-intl` only changes what is selected. There is no other Intl
hook in `exec.rs`, so tests run against the engine as is. How to run it with the queue: `lucinanet-test262 --test262 /path/to/test262
--filter intl402 --with-intl --jobs 2 --timeout-ms 30000 --out intl402.jsonl --summary intl402-summary.json`. The `--baseline` file (`tools/lucinanet-test262/baseline-failures.txt`) lists the
default (non-Intl) run only; a separate Intl baseline file should be added when stage 1 starts so Intl progress is gated the same way.

## 6. Licences and pins

### 6.1 Licences [docs: `license` field of each `Cargo.toml` in the local registry]

| Crate | Version | Licence |
|---|---|---|
| icu_locale, icu_locale_core, icu_locale_data | 2.1.1, 2.3.0, 2.1.2 | Unicode-3.0 |
| icu_decimal, icu_decimal_data, fixed_decimal | 2.1.1, 2.1.1, 0.7.3 | Unicode-3.0 |
| icu_plurals, icu_plurals_data | 2.1.1 | Unicode-3.0 |
| icu_list, icu_list_data | 2.1.1 | Unicode-3.0 |
| icu_datetime, icu_datetime_data | 2.1.1, 2.1.2 | Unicode-3.0 |
| icu_calendar, icu_calendar_data, calendrical_calculations | 2.1.1, 2.1.1, 0.2.4 | Unicode-3.0 |
| icu_time, icu_time_data | 2.1.1 | Unicode-3.0 |
| icu_segmenter, icu_segmenter_data | 2.1.2, 2.1.1 | Unicode-3.0 |
| icu_collator, icu_collator_data | 2.1.2, 2.1.1 | Unicode-3.0 |
| icu_experimental, icu_experimental_data | 0.4.0, 0.4.1 | Unicode-3.0 |
| icu_provider, icu_provider_baked, icu_provider_adapters, icu_provider_registry, icu_pattern, icu_casemap, icu_normalizer, icu_properties, icu_collections | as pinned | Unicode-3.0 |
| zerovec, zerotrie, tinystr, writeable, yoke, litemap, potential_utf, databake | as in lock file | Unicode-3.0 |
| icu4x-datagen, icu_provider_export, icu_provider_source (build tools) | 2.1.1 | Unicode-3.0 |
| timezone_provider (already used) | 0.2.1 | MIT OR Apache-2.0 (as `temporal_rs`), uses `tzif` 0.4.1 (Unicode-3.0) and `jiff-tzdb` (Unlicense OR MIT) |
| iana-time-zone (already used) | 0.1.x | MIT OR Apache-2.0 |

The generated data (CLDR, ICU, Unicode character data, LSTM and dictionary models for the segmenter, the IANA tzdb) is distributed under the Unicode License v3
(Unicode-3.0, permissive, requires the copyright and permission notice to be kept with copies); the IANA tzdb itself is public domain. Keep the Unicode notice in the
LucinaNet third-party licence file when Intl data ships. The segmenter dictionary and LSTM files carry their own upstream notices inside the ICU4X data crates; datagen output
inherits them. [docs; not independently verified for the segmenter model sources]

### 6.2 Pins

Add to the fork's workspace `Cargo.toml` next to the existing exact pins (same style, so browser workspace resolution does not float):

```toml
icu_decimal = "=2.1.1"
icu_plurals = { version = "=2.1.1", features = ["experimental"] }   # PluralRulesWithRanges (selectRange)
icu_list = "=2.1.1"
icu_datetime = "=2.1.1"
icu_calendar = "=2.1.1"
icu_time = "=2.1.1"
icu_segmenter = "=2.1.2"
icu_experimental = "=0.4.0"        # relativetime, displaynames, currency/percent/units, compactdecimal; 0.x: exact pin required; compactdecimal now from stable icu_decimal after the 2.3 bump (decision in section 0)
icu_pattern = "=0.4.2"
icu_locale_core = "=2.3.0"         # already resolved to this in Cargo.lock
fixed_decimal = "=0.7.3"
# build tool, installed outside the workspace as in icu/generate_data.sh:
#   cargo install --version 2.1.1 --features experimental icu4x-datagen
```

The existing pins stay as they are (`icu_provider =2.3.1`, `icu_provider_baked =2.1.0`, `icu_locale =2.1.1`, `zerovec =0.11.8`, `zerotrie =0.2.5`). All of the above resolved together in the spike
(`spike/Cargo.lock`). `icu/generate_data.sh` already pins `ICU_VERSION=2.1.1` for datagen: the new script must use the same version, and `--features experimental` is needed for the
relative time, display names, currency, percent, units and compact decimal markers (the default datagen build refuses them: "This marker requires the `experimental` Cargo feature").

### 6.3 `third_party/UPSTREAMS.toml` entry

The format below follows `third_party/UPSTREAMS.toml` in this tree (same keys as our other upstreams). The pin is the last upstream
commit that could not be read offline, so `pin` is left as the crate release and must be filled from the tag when merging.

```toml
[[upstream]]
id = "icu4x"
name = "ICU4X (Unicode Intl components) 2.3 + CLDR data"
url = "https://github.com/unicode-org/icu4x"
source = "github"
repo = "unicode-org/icu4x"
branch = "main"
track = "releases"
pin = "icu4x-2.3.0"
pin_date = "2026-11-11"
paths = ["components/decimal", "components/plurals", "components/list", "components/datetime", "components/calendar", "components/time", "components/segmenter", "components/collator", "components/locale", "components/experimental"]
ours = [{ repo = "third_party/brimstone", path = "src/js/runtime/intrinsics/intl", base = "main" }]
licence_upstream = "Unicode-3.0"
licence_ours = "MIT"
relation = "vendored"
confidence = "confirmed"
rely_on = ["icu_* 2.1.x stable API incl. the _unstable provider constructors", "icu_experimental 0.4.0 relativetime/displaynames/dimension/compactdecimal", "baked data via icu4x-datagen 2.1.1 --features experimental", "DurationFormatter not constructible from outside 0.4.0"]
notes = "Used as a library (crates.io), not copied. Design: docs/lucinajs-intl-design.md. Moving the pins means moving icu_provider, icu_locale and the data together and regenerating icu/data and the Intl data module."
```

The code that lives in the fork is MIT: `third_party/brimstone/LICENSE` is the MIT licence.

This entry's `pin` and `paths` change with the ICU4X 2.3 bump (section 0), and that bump has moved: the glue pinned the fork at `d48f9b706c` on 2026-11-12 and has since moved to
`1f065a160e` (2026-11-13, `Intl.DateTimeFormat` and `Intl.Locale`), which brings `icu_datetime` and `icu_time` in at the same 2.3.0, so the pin stays
the 2.3 release tag and `paths` follow the crates that moved, compact decimal coming from `components/decimal` instead of `components/experimental`. The Boa upstream
block's relation is `reference`, and because Boa carries no per-file licence headers the notice duty is the fork's
third-party notices plus a comment at each port site.

## 7. Atomics (and SharedArrayBuffer)

State [src]: no `Atomics`, no `SharedArrayBuffer`. SpiderMonkey in this checkout does not have `Atomics` either. The call-frequency probe saw no use on the
19 logged pages (including Google sign-in, YouTube start and watch). [meas]

What uses it on the web [docs, inf]:
- `SharedArrayBuffer` + `Atomics` are required by WebAssembly threads and Emscripten pthreads builds, by ffmpeg.wasm and similar in-page codecs, by some `AudioWorklet`
  ring buffers and by shared-memory workers. Browsers expose `SharedArrayBuffer` only on cross-origin-isolated pages (COOP `same-origin` + COEP `require-corp`/`credentialless`), so most
  ordinary sites never see it; sites feature-detect `typeof SharedArrayBuffer`.
- `Atomics.*` also works on non-shared `Int32Array` etc., and some libraries call it only on plain buffers (counters, locks in single-threaded code).

Does it need real threads in this engine? Two levels:
1. **`Atomics` on ordinary buffers**: no threads. Thirteen operations (`add, and, compareExchange, exchange, isLockFree, load, notify, or, store, sub, wait, waitAsync, xor`, plus `pause`
   in recent drafts) over typed array elements, with the ECMA-262 rules for integer types and `BigInt64`. `wait` on a non-shared buffer throws `TypeError`; `notify` returns 0. The typed array machinery exists
   (`typed_array_prototype.rs`, `typed_array_fast.rs`). About 3 worker-days. [inf]
2. **`SharedArrayBuffer` and real sharing**: the memory must live outside the garbage-collected heap (reference-counted backing store) and be reachable from several `Context`s on several
   threads, which only matters once workers exist. `Atomics.wait/notify` needs the agent cluster semantics (blocking wait, waiter lists, `$262.agent` in the test runner, which has no threading
   today, `tools/lucinanet-test262/README.md:27`). Without workers, `SharedArrayBuffer` can exist (growable too) with single-agent behaviour. About 4 days for single-agent
   `SharedArrayBuffer` + growable + `Atomics.waitAsync`, then 6-8 days when workers and wait/notify across agents are added. Needs the browser's Worker implementation to hand shared backing stores between contexts. [inf]

test262 counts [meas]: `built-ins/Atomics` 390 files, `built-ins/SharedArrayBuffer` 104 files, 653 files mention either name (many are typed array and DataView tests that
need `SharedArrayBuffer` as an alternate buffer type).

Where it belongs in the order: after Intl stage 1. Nothing in the 19-page probe needs it, and a page that does need it (wasm threads) also needs workers and cross-origin isolation that
the browser does not provide yet. Level 1 (plain `Atomics`, 3 days) is cheap and can be slotted into a gap after stage 1; level 2 follows the Worker work.

## 8. Baseline: test262 `intl402` with the existing runner

Command, run through the queue [meas]:

```
R=/path/to/test262   # the checkout at the commit in tools/lucinanet-test262/test262.sha
cargo run --release --manifest-path tools/lucinanet-test262/Cargo.toml --bin lucinanet-test262 -- \
  --test262 "$R" --filter intl402 --with-intl --jobs 2 --timeout-ms 30000 --out intl402.jsonl --summary intl402-summary.json
```

The runner binary is the existing build from 2026-11-11 18:02 (its engine revision is not recorded in the binary); the corpus is test262 at `227d9055...` (matches `test262.sha`).
Counts are variants (each test file runs strict and sloppy unless flagged otherwise); 3337 files give 6674 variants. The runner exits 1 whenever any variant fails.

| | variants |
|---|---|
| executed | 6674 |
| pass | 2998 (44.92%) |
| fail | 3676 |
| skipped | 0 |
| timeout | 0 |
| crash | 0 |

By directory (pass / fail, variants):

| Directory | files | pass | fail |
|---|---|---|---|
| Temporal | 2029 | 2952 | 1106 |
| NumberFormat | 248 | 0 | 496 |
| DateTimeFormat | 243 | 0 | 486 |
| Locale | 152 | 0 | 304 |
| DurationFormat | 110 | 0 | 220 |
| ListFormat | 81 | 0 | 162 |
| RelativeTimeFormat | 80 | 0 | 160 |
| Segmenter | 79 | 0 | 158 |
| Intl | 66 | 0 | 132 |
| Collator | 65 | 0 | 130 |
| DisplayNames | 57 | 0 | 114 |
| PluralRules | 53 | 0 | 106 |
| String | 19 | 14 | 24 |
| Date | 12 | 16 | 8 |
| BigInt | 11 | 6 | 16 |
| Number | 7 | 6 | 8 |
| Array | 2 | 2 | 2 |
| TypedArray | 1 | 2 | 0 |

Top failing test262 features (variants): Temporal 1248, Intl.Era-monthcode 848, Intl.Locale 312, Intl.DurationFormat 220, Intl.NumberFormat-v3 198, Intl.ListFormat 162, Intl.RelativeTimeFormat 158,
Intl.Segmenter 158, Intl.NumberFormat-unified 136, Intl.DisplayNames 94, Intl.Locale-info 88, Intl.DateTimeFormat-formatRange 74, Intl-enumeration 70. (A test counts toward every feature it lists.)
The 2952 Temporal passes are tests that do not touch Intl; the 1106 failing Temporal variants mostly call `toLocaleString` or non-ISO calendars.

## 9. Recommended order

1. Land the Intl core + data provider + NumberFormat + DateTimeFormat + `Date` time zones first (the probe shows NumberFormat on 6 pages and DateTimeFormat with 105 calls on 3).
2. Then Collator/`localeCompare`, `toLocaleLowerCase/UpperCase`, Locale, PluralRules, ListFormat, RelativeTimeFormat.
3. The default build ships the en, da and it locales (the project's decision, section 10 item 1). Zone names and display names stay a build feature (`tz-names`, `display-names`)
   that is off by default in stage 1; zone ids and the offset forms work without them.
4. Stage 2: Segmenter, DisplayNames, formatRange, NumberFormat v3, then DurationFormat when `icu_experimental` allows.
5. `Atomics` level 1 after stage 1; `SharedArrayBuffer` sharing with Workers.

## 10. Open questions

1. Which locale set ships by default — **answered**.

   **Decided:** ship **English, Danish and Italian** (`en`, `da`, `it`, with their regional children and the root
   locale). From the size table, everything in one binary for that set is 1,716,304 B (about 1.7 MB) over the baseline binary.

   Requirements that bind the implementation:
   - The locale list is **one build setting**: the environment variable `LUCINAJS_INTL_LOCALES`, default `en,da,it`, read by the data generation script
     `icu/generate_intl_data.sh`. A wider set (common20 = +3.9 MB, all = +12.8 MB over the three) is a rebuild with another value, not a redesign.
   - A locale that is not built in falls back to English formatting. It never throws where ECMA-402 says to fall back to the default locale
     (`ResolveLocale` / lookup matcher, `DefaultLocale`).
   - `Intl.supportedLocalesOf` and `resolvedOptions().locale` tell the truth: they report only locales that are built in, or the fallback actually used.

2. Is it acceptable to depend on `icu_experimental` 0.4.0 (exact pin, API may break at the next ICU4X bump) for relative time, display names, currency, percent, units and compact decimal? The alternative is writing those from CLDR data ourselves.
3. `Intl.DurationFormat`: wait for upstream, or write it ourselves over `icu_list` and unit names?
4. Time zone source on Juno: an embedder setting (recommended), or should LucinaOS provide `/etc/localtime`?
5. Should `Date` become time-zone aware in the same change as `DateTimeFormat` (recommended, 3 days), given every existing `Date` result moves from UTC to local time?
6. Move all ICU4X pins to a newer release (the registry has 2.2 to 2.4), or stay on 2.1.x until Intl stage 1 lands? — **Answered 2026-11-12**: the bump to the 2.3 line
   comes after the first two pieces. Collator/`localeCompare` and NumberFormat decimal land on the present 2.1 pins, then one change moves every `icu_*` pin together;
   see the pin decision in section 0.
7. Is a separate crate for the baked Intl data (so the engine does not recompile it) acceptable, given the `icu/` directory is currently in the fork?
