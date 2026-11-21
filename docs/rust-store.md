# The non-Rust leftovers: SQLite, zstd, runtimes (2026-11-04)

Servo patches on branch `rust-store` of `LucinaNet-servo` (plus `rust-store-net`, `rust-store-zstd` until merged). Porting guidelines:
our porting guidelines. The original is Servo at `docs/servo-pin` with our patches.

## What used SQLite, and what did not

| Data | In Servo | Now |
|---|---|---|
| localStorage | one SQLite file per origin, `webstorage/<uuid>/webstorage.sqlite` | one redb file at the same place, `webstorage.redb` |
| IndexedDB | one SQLite file per database (`indexeddb.sqlite`) | `indexeddb.redb`, same tables as key spaces, same key encoding (Firefox's) |
| Storage Standard registry (sheds, shelves, buckets, bottles, databases, directories) | `clientstorage/default_v1/reg.sqlite` | `reg.redb`, same ids and unique rules |
| HTTP disk cache (off unless the `network_http_disk_cache` pref is set) | one SQLite file | one redb file |
| Cookies, HSTS, HTTP auth | **not SQLite**: `cookie_jar.json`, `hsts_list.json`, `auth_cache.json`, written only at exit by truncating in place, no fsync | same files and format, written atomically (temp file, fsync, rename, fsync of the directory) every 30 s when changed and at exit |
| localStorage origin list | `localstorage.json`, same writer | atomic, as above |

LucinaNet set no profile directory, so on Juno nothing persisted at all before this work.

## Candidates for the store (guidelines section 3)

| Candidate | Verdict | Why |
|---|---|---|
| **redb 4.4** (MIT OR Apache-2.0) | **adopt** | Pure Rust, no `unsafe`-heavy C bridge, ordered tables and ACID multi-table transactions that map one-to-one onto Servo's SQLite tables (a BLOB primary key compares bytewise in both, so IndexedDB key ranges and ordering stay identical). Copy-on-write B-tree with checksummed commit slots; 2-phase commit with quick repair for files holding web content (its own advice for malicious data), and recovery after a power cut without a walk over the file. Has crash-consistency tests of its own, and a `StorageBackend` trait we use to cut power in tests. |
| Turso / Limbo (MIT) | reject | SQL-compatible, which would have kept Servo's queries, but it calls itself beta, its Rust API is async-first and not rusqlite's, and it is far larger than what four small schemas need. Revisit if it reaches 1.0 and Servo's code grows more SQL. |
| sled (MIT/Apache) | reject | Stable line last released in 2021; the 1.0 rewrite has been alpha for years; its own docs warn about space use and durability. |
| fjall 3 (MIT/Apache) | reference | Maintained LSM store; good for heavy writes, but background compaction threads and write amplification buy nothing for a browser profile on one laptop disk. |
| A purpose-built log store | reject | Simple to prove, but it would hold every IndexedDB value in memory or need its own page format and compaction; that is redb again, untested. What we own and prove instead is listed below. |

What we built on redb: `components/profile_store` (crate `servo-profile-store`): one `Store` per former SQLite file; every commit atomic at once and
durable within one second (one flush thread for all stores), as Servo's WAL with `synchronous = NORMAL` promised; a record codec for row values;
the eviction rule of the disk cache.

## Behaviour kept, and the differences made on purpose

Kept: file layout (one file per former SQLite file), every return value and error case of the engines, ids, "created" flags, key order and ranges,
quotas, what survives a restart. Differences, each commented in the code:
1. Registry crash order: Servo committed a database's row, then made its directory; a cut between them left a row whose directory never exists,
   and that IndexedDB database could not be opened again. Now the directory (synced) comes first, and orphan directories are swept at startup.
   Verus model: `verification/registry/model.rs` (also shows Servo's order breaking the rule).
2. Disk cache eviction: Servo dropped the newest entry (the one just stored), could loop for ever, and could underflow its size; now the oldest go,
   by a Verus-checked rule.
3. JSON profile files written atomically and every 30 s (Chromium's cookie commit interval) instead of only at exit; an I/O error is logged
   instead of panicking.
4. zstd: windows over 8 MiB are refused (RFC 9659; Firefox sets `ZSTD_d_windowLogMax` 23, as Chromium does). libzstd as Servo used it allowed 128 MiB.

## Proofs

Run the verification script on `scripts/verify-shipped.sh` and `verification/mutants.sh` (16 planted bugs, all refused, 2026-11-04).

| File (Servo patch) | Shipped or model | What is proved |
|---|---|---|
| `components/profile_store/rules/bounds.rs` | shipped | a field span lies inside its record; a length prefix past the end or over 1 GiB is refused; a record never grows past 2 GiB; no overflow |
| `components/profile_store/rules/evict.rs` | shipped | always ends, never underflows; with a consistent total it drops the fewest oldest entries that make the rest fit |
| `components/zstd/window.rs` | shipped | an accepted header needs at most 8 MiB and lies in the bytes given; the window is RFC 8878's formula (descriptor or single-segment content size); reserved bit, dictionary id and magic checked; "need more" is bounded |
| `verification/registry/model.rs` | **model** | a committed row always names an existing directory at every cut point; after the sweep no directory lacks a row |

Not proved: redb itself (its own tests, plus our power-cut test), the byte reads around the bounds, ruzstd's block decoding (tested against libzstd).

## Tests

- `components/profile_store/tests/power_cut.rs`: every write and sync logged; a cut anywhere leaves what was synced plus any subset of later
  writes, each torn at 512-byte sectors; 1920 images over 12 seeds, the store opens every time and holds exactly one commit's state, never older
  than the last durable one. A planted bug (a flush that does not make anything durable) fails it.
- zstd parity against libzstd (window_log_max 23), `components/zstd/tests/parity.rs`: 1775 corpus cases (levels 1-19, windows 2^10-2^23,
  checksums, content sizes, multi-frame, skippable frames), 2802 truncations, 1612 hostile streams (bit flips, oversized windows, reserved
  bit, dictionary id, trailing garbage, wrong sizes and checksums), each through the sync core in 4 chunkings and the async decoder at 3
  buffer sizes: all agree. Four bit flips that libzstd refuses decode in ruzstd to exactly the original content (asserted). 1 GiB of zeros
  decodes in bounded memory.
- Servo's own tests, ported to the new engines: `servo-storage` 31 + 19 pass; `servo-net` 391 of 392 (the one, `test_filemanager`, passes
  alone: a set-once runtime handle in Servo's tests, not this change); `servo-base` 38.
- An independent review of the IndexedDB and registry engines against the SQLite ones found no range, order, id or count difference, and
  four issues, all fixed: disk-full was not seen through redb's errors; IndexedDB `complete` came before its commit was durable (now
  flushed first); a fresh registry could sweep an old profile's directories; disk walks under the registry's write lock. The storage
  review found that a `flush` could return while another was still writing (fixed).
- The Juno binary (`juno` profile) has no SQLite or libzstd left: `sqlite_master` 19 times before, 0 after; libzstd's error strings gone.
- **Laptop, 2026-11-04, generation 29**, binary run from `/data/lucina/rust-store/`, not installed (the installed LucinaNet stays the
  `lucinanet` session's tree until it adopts this): `tests/profile/restart.sh` PASS. httpbin.org set cookie `lucina=juno1006`, the page wrote
  localStorage and IndexedDB, the engine ran 40 s and ended with `_exit` (a kill: no exit-time save); the next run read back all three.
- IndexedDB is experimental in Servo and off by default (`dom_indexeddb_enabled`), in LucinaNet too: the test turns it on. Whether
  LucinaNet turns it on for everyone is the `lucinanet` session's call.
- **QEMU power cuts on Juno** (LucinaOS `scripts/qemu-juno-disk.sh`, part `store`: 12 cuts at random times while `store_cut` commits to
  `/data/lucina/net-cut/profile.redb`, the next boot checks what it finds):
  - First run, 2026-11-04: **all 12 cuts came back empty**, 16340 commits lost that had been reported durable. On the disk image the
    directory was there and the file was not: POSIX makes a new name durable only after an fsync of its directory, Linux ext4 commits it
    with the file's fsync anyway, Juno's ext4 does not, and redb never syncs the directory. The simulated cuts (`power_cut.rs`) could not
    see it: they model one file, not directories.
  - Fix (patches 0033-0034): `Store::open` syncs every directory above a file it creates.
  - Rerun with exactly that code: **PASS**, 0 durable commits lost, 30 not yet flushed lost (the 1 s window), every reopen found one whole
    commit in 40-166 ms, and the state grew across the 12 cuts to commit 13815.

## HTTP disk cache on Juno: tmpfs for now

`http-cache.redb` (Servo's HTTP disk cache, `engine.rs`, `http_cache_location`) is not kept in the profile on Juno.
- **Where:** on Juno (`/run/lucina` exists) the file is `/run/lucina/net/http-cache.redb`, on tmpfs. Everywhere else it stays in the
  profile directory. `LUCINANET_HTTP_CACHE_DIR` (a directory) overrides both; with no profile directory the cache is off unless it is set.
- **Size:** 64 MB by default on tmpfs (it is RAM on a 3.7 GB machine), 256 MB in the profile. `LUCINANET_HTTP_CACHE_MB` always wins; `0` turns it off.
  The start-up line `lucinanet: HTTP cache at <file> (<N> MB bound, <reason>)` on stderr says which case applies; when there is no
  disk cache the line says why (`HTTP disk cache in memory only (no profile directory)` or `off (LUCINANET_HTTP_CACHE_MB=0)`).
- **Lost at reboot:** the cache is empty after a boot. It still serves the second visit to a site within a session, which is the case that matters.
- **Why interim:** on Juno a successful fsync on `/data` does not yet prove the data is on disk (a failed background writeback is
  dropped and the next fsync returns 0), and `/data` writers can stall for minutes behind a sync. The cache is a bursty writer and
  losing it costs nothing, so it stays off `/data` meanwhile. The rest of the profile (history, zoom, localstorage) is unchanged.
- **Moving it back:** when Juno kernel patch 1242 (write errors reported at fsync) and the writeback steps are on the installed
  generation, the default returns to the profile. The disk-root chat says when; until then the tmpfs location stays.
