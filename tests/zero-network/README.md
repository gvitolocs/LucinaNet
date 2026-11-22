# zero-network start tests

Proves the zero-network rule on a **real** start of the desktop binary:

> no request leaves the machine that the user did not cause.

A fresh, empty profile is started exactly like a double-click does — no address argument —
and sits idle for 45 s. During that idle window the observer must record **zero**
`connect`/`sendto`/`sendmsg`/`sendmmsg` attempts toward anything that is not AF_UNIX,
not AF_NETLINK (kernel IPC) and not loopback (`127.0.0.0/8`, `::1`). That covers what the
engine would do by itself: favicon fetches, DNS prefetch, update/safe-browsing/captive-portal
probes, telemetry, and DNS lookups through the library resolver (they appear as UDP/TCP
connects to the resolver). Then a **positive control** in the same script drives one
user-caused request and the same observer must see it — a blind observer cannot pass.

The second case is the same idle observation on a profile that **already has
history and bookmarks**. A blank start proving nothing about what stored data makes the
engine reach for.

## the two cases

```sh
bash tests/zero-network/run.sh                    # both, fresh then seeded (default)
bash tests/zero-network/run.sh fresh              # the fresh case only
bash tests/zero-network/run.sh seeded             # the seeded case only
LUCINANET_BIN=/path/to/lucinanet-servo bash tests/zero-network/run.sh all
IDLE_SECONDS=45 CONTROL_SECONDS=15 bash tests/zero-network/run.sh   # both overridable
```

Exit code 0 = every selected case passed, 1 = at least one failed (a missing prerequisite is
a FAIL with a reason, never a skip). The last line is always one machine-readable summary:

```
RESULT zero-network PASS cases=fresh,seeded violations=0 controls=2/2 seed=read-proved observer=strace seconds=138
```

`violations` is the number of offending destinations across all phases, `controls` is
seen/total positive controls, `seed` says whether the read of the store files was proved
(`read-proved` / `probe-failed` / `n/a` for the fresh case), `observer` is `strace` or
`ss-poll`, `seconds` is the wall time of the whole command.

Requirements on the host: the desktop binary (built as in `tools/release/README.md`, from
`tools/release/Dockerfile.linux-desktop`), `Xvfb`, `strace` (or `ss` for the fallback),
`nc` (OpenBSD netcat, for the control listener), and for `seeded` the seeder below.
Evidence (observation logs, the report) goes to a fresh `mktemp -d` run dir under `$LN_SCRATCH`
(or `$TMPDIR`, which it falls back to; with neither set the run stops rather than writing into
`/tmp`) and its path is printed on the first lines; `KEEP_EVIDENCE=0` removes it at exit.

The start it observes gets only three environment variables: `DISPLAY` (the private Xvfb),
a throwaway `LUCINANET_PROFILE_DIR`, and `LUCINANET_MEDIA_SINK=none` (mute; selects no URL).
Every `LUCINA*` variable of the caller is **removed** first, so ambient settings cannot
inject a start URL, prefs or third-party services into the run.

## the seeded case, and how it is kept honest

The profile is seeded through the browser's **own storage format**, not by hand:
`crates/lucinanet-historydb/examples/seed-profile.rs` calls `HistoryDb::to_json` and
`Bookmarks::to_json` — the same two calls the browser's save paths make — and writes
`history.json` (7 pages over 9 visits: typed, typed-search and link, the three ways
`visits.rs` stores them) and `bookmarks.json` (5 bookmarks in the toolbar, a folder and
Other Bookmarks, spread over the last week). It then reads both files back with the loader
the browser uses before it says they are seeded.

```sh
cargo run -p lucinanet-historydb --example seed-profile --offline -- <profile-dir>
# seeded 7 history pages / 9 visits / 5 bookmarks into <dir>   (2195 + 1144 bytes)
```

`run.sh` builds that example if no `LN_SEED_BIN` / prebuilt example binary is found
(`seeder-build.log` in the run dir; the artifact is checked rather than trusting cargo's
exit status). Two guards keep the case from passing vacuously:

* **seed integrity** — after each observed phase the seeded `history.json`/`bookmarks.json`
  must still hold the seeded URLs, and `bookmarks.json.unreadable` must not have appeared
  (marks.rs moves an unusable file aside under exactly that name);
* **the read probe** — one 12 s start on a profile whose two store files are deliberately
  *not* store files. The browser's own reactions are the proof that startup reads them:
  `bookmarks.json unusable (...); kept as .../bookmarks.json.unreadable` and
  `lucinanet: .../history.json ignored: Syntax(...)`. If neither appears, the "seeded" case
  would be a fresh run in disguise and `seed=probe-failed` is reported.

A write-back proof is not available, and not because the run is killed: a store is only
rewritten when something changed (`persist.rs`, a flush with nothing dirty is a no-op), and
a start with no address changes neither history nor bookmarks — see the findings under the run.

## observer policy

* Chosen automatically: if `strace -f -qq -tt -yy -e trace=network` works, it runs
  (`observer: strace` in the report). Otherwise the script polls `ss -Htan`/`ss -Huan`
  every 100 ms (`observer: ss-poll`) and says so — that mode can miss connections shorter
  than the poll interval, and both the report and the README findings must be read with
  that in mind.
* Allowed, counted but not violations: `AF_UNIX`, `AF_NETLINK`, `AF_UNSPEC` (UDP socket
  disconnect), and loopback destinations (listed as `loopback ...` lines).
* Violation = any *attempt* (even `EINPROGRESS`, `ENETUNREACH`, or port 0) toward a
  destination outside that set. Unparseable network syscalls are counted as violations
  (`dest=UNPARSED`) — the analyzer never silently passes what it cannot classify.
* On FAIL each violation is printed as `VIOLATION dest=<address:port> x<count>
  first=<time> pid=<tid> | <raw line>`, so the offending time and thread are identifiable
  (strace reports the numeric tid, not a comm name).

## positive control (per case, same script)

A tiny local listener is started from bash + `nc -l -n` (a single `nc` per accepted
connection, looped 20× in a subshell; `python -m http.server` is deliberately not used).
The binary is then started again, fresh profile, with `http://127.0.0.1:<port>/` as the
address argument — that page load **is** the user action. The observer must record the
connect to that port; if it records nothing the script fails the run
("the observer is blind; the idle result means nothing"). The control phase's log is also
scanned for destinations outside loopback — anything there would be traffic the engine
made on its own while merely loading a local page.

What the control proves is that the **observer** sees a user-caused request. It does not
prove that the page rendered: in all four control phases of the run below the browser
logged `kind=ResourceFailed, detail="unreachable"` ~27 ms after the connect. See the
findings; that is a follow-up, not part of this verdict.

## runs on a Linux desktop, 2026-11-21

Both runs used the same binary: HEAD `fb158d0` (blank start page,
search engine taken from the **shipped settings** — `address.rs` builds `Engines` from
`Settings::default()`, "the engines of the shipped settings, until a profile's settings are
loaded", and no profile settings document is loaded for it yet; the only field the start
reads out of the profile's `prefs.json` is `privacy.error_log`, in `errlog_glue.rs` — so
there is nothing here that a user setting could steer), remote suggest off. Built with the
release recipe: the Debian 12 image (`tools/release/Dockerfile.linux-desktop`) running
`cargo build --release --locked -p lucinanet-servo --features desktop,media`
(rust 1.99.1). **Build time 9 m 55 s.** Artifact `lucinanet-servo`
143,124,168 bytes, sha256 `372cd0fe6baa0cac…`, glibc floor `GLIBC_2.35`.

### 13:30 — `fresh` only — PASS

Observer: **strace** (`strace -f -qq -tt -yy -e trace=network`); the `ss`-poll fallback was
not needed (strace 6.19 present). An identical run at 13:26 also PASSed; between the two the
listener subshell got a cleanup fix (an orphaned `nc` could keep the control port), which
does not touch what is observed — the second run is the reported one.

Whole observation of the 45 s idle start — every network syscall the fresh-profile,
no-address run made, all of them 39 ms after launch, then silence:

```
13:30:58.964984 connect(3 UNIX-STREAM @/tmp/.X11-unix/X100)                 = 0     (X11)
13:30:59.003548 sendmsg(3 UNIX-STREAM, SCM_RIGHTS /dev/shm/softbuffer-x11-) = 24    (X11)
13:30:59.004792 sendmsg(3 UNIX-STREAM, SCM_RIGHTS /dev/shm/softbuffer-x11-) = 12    (X11)
```

```
VERDICT zero outgoing (non-loopback, non-local-IPC) connect/sendto/sendmsg/sendmmsg attempts
info AF_UNIX x3  AF_NETLINK x0  AF_UNSPEC x0  unparsed x0
positive control: observer recorded 1 line(s) naming the connect to 127.0.0.1:28317
control extras: (none)
PASS: a fresh profile at a real start made no network request for 45s of idle (observer: strace)
```

### 14:24 — `all` (fresh + seeded) — PASS

The run this record is from. Its evidence dir sat under the machine's `$TMPDIR`
(`$TMPDIR/lucinanet-run.20261121T142437.OZoGA4`).

```
===== case seeded =====
probe: a start on a profile whose bookmarks.json and history.json are not store files, 12 s
phase read-probe: profile $TMPDIR/lucinanet-profile.read-probe.wcw9Rl; start .../lucinanet-servo; idle 12 s; observer -> read-probe.strace.log
phase read-probe: stopped; read-probe.strace.log has 175 lines
case seeded: the read of the profile files is proved, so the seed matters
phase seeded-start: profile $TMPDIR/lucinanet-profile.seeded.daU5Lu; start .../lucinanet-servo; idle 45 s; observer -> seeded-start.strace.log
phase seeded-start: stopped; seeded-start.strace.log has 165 lines
observation of the seeded idle start:
VERDICT zero outgoing (non-loopback, non-local-IPC) connect/sendto/sendmsg/sendmmsg attempts
info AF_UNIX x3  AF_NETLINK x0  AF_UNSPEC x0  unparsed x0
control: local listener on 127.0.0.1:28317 (bash + nc); the page load is the user action
phase seeded-control: profile $TMPDIR/lucinanet-profile.seeded.YDhlQ9; start .../lucinanet-servo http://127.0.0.1:28317/; idle 15 s; observer -> seeded-control.strace.log
phase seeded-control: stopped; seeded-control.strace.log has 195 lines
positive control: observer recorded 1 line(s) naming the connect to 127.0.0.1:28317
control extras: destinations outside loopback seen while only a loopback page was loaded
case seeded: PASS - no network request in 45s of idle, control seen (seed: read-proved)
RESULT zero-network PASS cases=fresh,seeded violations=0 controls=2/2 seed=read-proved observer=strace seconds=138
```

**Cost of one `all` run: 138 s (~2.3 min)** on a Linux desktop, Xvfb and observer included, against a
binary that is already built. It needs no network and no root.

14:57 — `fresh` alone, re-run against the committed script: `RESULT zero-network PASS
cases=fresh violations=0 controls=1/1 seed=n/a observer=strace seconds=63`, exit 0 — a third
independent PASS of the same binary. `run.sh fresh` therefore costs **63 s**.

### findings

* **The seeded start is as quiet as the fresh one.** The whole 45 s idle observation of the
  profile with 9 visits and 5 bookmarks is the same three AF_UNIX X11 operations —
  `connect(@/tmp/.X11-unix/X100)` and two `sendmsg` `SCM_RIGHTS` of
  `/dev/shm/softbuffer-x11-*`, `info AF_UNIX x3 … unparsed x0`, at 14:25:53, ~39 ms after
  launch, then silence. **No favicon fetch for the bookmarks, no history prefetch, not even
  a resolver contact** — the seeded entries are read and rendered from disk and the engine
  reaches for nothing.
* Positive control behaved as expected in both cases: user-caused
  `connect(127.0.0.1:28317)` seen at 14:25:24.175832 (fresh) and 14:26:39.765088
  (seeded), each `EINPROGRESS` (async), and while loading only the loopback page neither run
  touched anything outside loopback. One extra AF_UNIX attempt appeared in a control log
  only: `connect("/run/lucina/events.sock") = ENOENT` — local IPC, allowed, no such file on
  this host.
* The read probe did what it had to: `history.json ignored: Syntax("expected value at line
  1 column 1")` and `bookmarks.json unusable (Format(...)); kept as
  .../bookmarks.json.unreadable`. So the seeded case really starts on a profile the browser
  reads, and `seed=read-proved` is not decoration.
* **What the idle start does and does not write.** After the idle phase,
  `history.json` and `bookmarks.json` still carried their *seed* mtimes (14:25:53.416, i.e.
  never rewritten) while `cookie_jar.json`, `auth_cache.json` and `hsts_list.json` were
  written ~30 s into the run (14:26:23.6) as empty version-1 documents. The history and the
  bookmarks were not rewritten because nothing added a visit or a mark to them, not because
  the phase ends with a signal: `Saver::flush` (`persist.rs`) writes only a store that is
  dirty, so a start with no address leaves those two files alone however the process ends.
  Two consequences: a write-back proof of the seed read is impossible (hence the probe), and
  the periodic cookie/auth/HSTS flush is disk-only — it produced no network traffic in the
  window that covered it.
* **Open follow-up, not hidden:** the control page did not render. All four control phases
  logged `{"kind":"ResourceFailed","detail":"unreachable","page_host":"127.0.0.1"}` ~27 ms
  after the observed connect. `errlog_glue.rs:372-376` maps every non-TLS neterror title to
  `"unreachable"`, so the class says nothing about the reason. The identical listener shape
  (`nc -l -n 127.0.0.1 <port> < page`) does serve a complete 200 response to a plain bash
  `/dev/tcp` GET on this host, verified separately, so the suspect is the one-shot `nc`
  (it closes at stdin EOF, possibly before the engine's request round-trips) rather than the
  network path. This does not affect any verdict here — the control's job is to prove the
  observer sees a user-caused request, and it did — but a future version should use a
  listener that reads the request before answering.
* Caveat for future runs, not triggered here: on this host `/etc/resolv.conf` points at
  the systemd-resolved stub `127.0.0.53` with a private search domain. A loopback DNS
  query would be reported as *allowed loopback traffic* by this filter, yet the OS stub
  can relay it off-machine. If `loopback 127.0.0.53:53` lines ever appear in the idle
  phase, they are a finding to escalate, not a free pass.

### observer capability check (why this PASS is trustworthy)

The same script was first dry-run (6 s idle) against the **older release-candidate binary**
(a release-candidate binary from an earlier build tree, pre-fix startup
behavior, sha256 `e5a7bb26a4776f7c…`) and correctly reported FAIL with 5 violating
destinations, plus resolver traffic:

```
VIOLATIONS 5
VIOLATION dest=172.66.147.243:0    x1 first=13:25:13.679383 pid=1785001  (UDP, Cloudflare)
VIOLATION dest=172.66.147.243:443  x1 first=13:25:13.694010 pid=1784687  (TCP EINPROGRESS)
VIOLATION dest=104.20.23.154:0     x1 first=13:25:13.679760 pid=1785001  (UDP, Cloudflare)
VIOLATION dest=[2606:4700:10::6814:179a]:0  x1 (IPv6 attempt, ENETUNREACH)
VIOLATION dest=[2606:4700:10::ac42:93f3]:0  x1 (IPv6 attempt, ENETUNREACH)
loopback 127.0.0.53:53 x2 first 13:25:13.668192   (DNS via the stub resolver)
```

so the PASS above is the observer seeing nothing because there is nothing to see.

## macOS variant: `tests/zero-network/run-macos.sh` — **it has never been run**

Same two cases, same positive controls, same `RESULT` line shape
(`RESULT zero-network-macos …`). Written on Linux, checked with `bash -n`,
`shellcheck -S warning` (clean) and by feeding **synthetic** lsof/nettop/dtruss logs to its
own analyzers — which is how three real bugs in it were found before it was committed (an
uppercase-hex regex that matched nothing, timestamps read as IPv6 destinations, source
addresses counted as violations). It has never been executed, not once, not on a real Mac.
Its first Mac run is a check of the file, not a measurement.

* Observers: `dtruss -f -c '^connect$|^sendto$|^sendmsg$|^sendmmsg$'` (needs root — the
  script says so and stops if it is neither root nor has passwordless sudo, unless the caller
  picks `OBSERVER=lsof-poll` on purpose), plus a `lsof -nP -i` poll of the process tree every
  200 ms for the destinations and `nettop` flows once per second so a UDP query that closes
  between two lsof samples still leaves a trace.
* **Where macOS is weaker, stated instead of papered over:** dtruss prints the sockaddr as a
  pointer, so only the trailing `socklen_t` is readable. 28 (IPv6) and 104..108 (the full
  `sockaddr_un`) decide cleanly; 16 is `sockaddr_in` *and* an AF_UNIX path of 14 bytes, and a
  `sendmsg` destination sits behind a struct pointer. Those attempts are reported with a
  socklen histogram and the control phase's own connect lines for calibration — counted
  neither as violations nor as proof. An attempt whose argument layout cannot be read at all
  *is* a violation, as on Linux. A Mac PASS is an assertion about named flows plus the
  unambiguous attempts.
* Running it opens a real window on the user's display for each 45 s phase; there is no
  headless X on a Mac. Do not run it to "see what happens" without expecting that.

## as a scheduled step

`run.sh all` is one command, correct exit code, one stable summary line, no root, no network. It belongs in a
nightly tier rather than a per-push gate: the rule it defends is a startup-behaviour rule that no diff in a normal
feature branch changes, and its value is in catching a regression that survives review — a favicon
prefetch, a captive-portal probe, an update poll added by someone who never runs the browser. One run against the
pinned main costs 2.5 minutes; a from-scratch desktop build measured 9 m 55 s, so a gate would pay the build rather
than the test. Where only a cached artifact exists, the cheap form is the `fresh` case alone (`run.sh fresh`, 63 s
measured).

The in-repo long-run harness is `tests/acceptance/mac-build.sh`, which has the step vocabulary (`step_pass` /
`step_fail` / `finish` printing `RESULT:` and `ACCEPT-DONE`) and already holds the binary it built. The step there is:

```sh
echo "=== zero-network start, fresh + seeded $(date -u +%T)"
LUCINANET_BIN=$B bash "$W/tests/zero-network/run.sh" all >"$D/zero-network.log" 2>&1; zrc=$?
echo "rc zero-network=$zrc"; [ "$zrc" -eq 0 ] && step_pass "zero-network" || step_fail "zero-network" "run.sh exit $zrc; last line: $(tail -1 "$D/zero-network.log")"
```

That adds ~2.5 min to a run the acceptance README already costs at ~15 min, needs only
`strace`/`Xvfb`/`nc` and the seeder, which the script builds itself when it is missing: **20 s** measured
from an empty target dir (serde, serde_json, url and the two store crates — no Servo in its dependency
graph). `tests/crash/README.md:13` sets the same precedent: "needs the network and takes many
minutes, so it belongs in the slower tier"; this one needs no network and 2.5 minutes.

## nightly record and the release gate

`run.sh all` runs in the **nightly** tier (not the per-push gate), and the
nightly result is **not optional** — a red or stale nightly **blocks the next public push and the next
Mac install** (`docs/release-checklist.md`). `--record` keeps the result where the release steps can read it.

### the record line

`--record <file>` (or `LUCINANET_ZERO_NET_RECORD=<file>`) appends **one** line per run to `<file>`,
newest last, carrying the same facts as the `RESULT` line plus the wall-clock time, the commit of the
tree under test and the binary hash:

```
<UTC-ISO-Z> PASS|FAIL commit=<git sha of the tree> binary=<sha256[0:16]> cases=<fresh,seeded> violations=<n> controls=<k/n> observer=<strace|ss-poll> seconds=<s>
```

A run that aborts on a missing prerequisite (no binary, no Xvfb, no observer) never records PASS: it
records an `ERROR` line with the reason and exits non-zero:

```
<UTC-ISO-Z> ERROR commit=<git sha> binary=none cases=<...> reason=<why>
```

A `PASS` is written **only** when every selected case passed, no destination violated and every positive
control landed — the guard lives in `finish`, so a mis-called `finish PASS` on a failing run records
`FAIL`. **Where the record goes:** whatever file `--record` names; a conventional place is
`target/nightly/zero-network.log` in the checkout.
Nothing is written unless `--record`/env is given, so a manual run does not touch the nightly log.

### the checker

`tests/zero-network/release-check.sh <log> <commit> [max-age-hours=26]` is the release gate. It exits
**0 only when the newest line** is `PASS`, is for the given `<commit>` (full sha or prefix), and is no
older than `max-age-hours` (default 26 h = one nightly cycle plus slack). Any other newest line —
`FAIL`, `ERROR`, a stale `PASS`, a `PASS` for a different commit — or an empty/missing log is red and
prints exactly one line saying why:

```sh
tests/zero-network/release-check.sh target/nightly/zero-network.log "$(git rev-parse HEAD)"   # exit 0 = ship
```

This is step 2 of [`docs/release-checklist.md`](../../docs/release-checklist.md) and is written into
the header of `tools/release/package-macos.sh` as a
**documented precondition comment only** — that script does not call the checker and its behaviour is
unchanged.

### the nightly run itself

The scheduler is a host matter, not CI and not part of this repository: a daily job that makes a clean
checkout of pinned main, builds the desktop binary, and then runs exactly this line:

```sh
tests/zero-network/run.sh all --record target/nightly/zero-network.log
```

The job records nothing itself: it appends a `SKIPPED` line when a host gate (load, free disk) stops the night and an
`ERROR` line when a build or any other step aborts, and it exits non-zero unless the newest line of that log is a
`PASS` for the commit it just tested. So a busy or a red night shows up as a failed unit *and* as not-PASS
to `release-check.sh`. Installing it is a host-administration step, not a release step.

It **needs a built binary**, which is why the nightly builds one rather than assuming `run.sh` finds the
last hand-run artifact. The build is the Linux release recipe the runs above used: `cargo build --release
--locked -p lucinanet-servo --features desktop,media` in the
Debian 12 image (`tools/release/Dockerfile.linux-desktop`, `tools/release/README.md`), which drops the artifact at
`target/release/lucinanet-servo`; the nightly passes it to `run.sh` as `LUCINANET_BIN` (whose default is
that same path under the checkout). The seeder goes to a cache dir outside the checkout and arrives as
`LN_SEED_BIN`.

**Cost:** the `all` run itself is **~2.5 minutes (138 s measured)** against an already-built binary; the
desktop build is separate (**~9 m 55 s** cold, less with a warm cargo cache), the
seeder ~20 s cold. The seeded case needs no network and no root; only the nightly *record file* and the
built binary have to be in place, and the nightly puts both there itself.

### the two bash tests

```sh
bash tests/zero-network/test-record.sh         # PASS/FAIL/ERROR record lines; a failed case never yields PASS (run.sh --self-test, no binary/network)
bash tests/zero-network/test-release-check.sh  # green / red / stale / wrong-commit / ERROR / empty-or-missing all behave
```

Both need no binary, no network and no Xvfb: `test-record.sh` drives `run.sh --self-test`, which
exercises the record/finish/abort logic against stubbed state.

## limitations

* 45 s window: a slower timer-based background request (e.g. an update poll scheduled at
  minutes) would need `IDLE_SECONDS` raised. The observed cookie/auth/HSTS flush landed ~30 s
  in, so the window does already cover one periodic writer.
* strace observes only the started process tree — a request made by some *external* helper
  service reacting to the browser would be invisible; nothing in the current design spawns one.
* `ss`-poll fallback (only used where strace cannot run) can miss connections under ~100 ms.
* The seed is written by `seed-profile.rs`, not by browsing: it exercises the read path and
  the stores' real format, but it cannot reproduce the state a *used* profile accumulates
  (populated HTTP cache, `cachestorage`/`clientstorage` data, cookies). A second run on a
  profile that has really browsed is still open.
* The seeded case proves what a start does with existing history/bookmarks. It says nothing
  about a profile whose `prefs.json` holds a full settings document: the start parses that
  file for `privacy.error_log` only, and the search engine comes from the shipped settings,
  so there is no user-steered engine to test yet. None of the profiles in these runs
  contained a `prefs.json` at all.
* `run-macos.sh`: never executed (see its section).

## The planted case

`run.sh planted` (part of `all`, so of the nightly) starts the **non-harness** binary with the variables of the former harness doors
planted (`LUCINANET_USER_JS`, `_USER_CSS`, `_PREFS`, `_SCRIPT`, `_AX`, `_LOADS`, `_RENDER_HOLD`), each naming a file that would fetch the
local control listener, and with `--render <listener url> <png>` as an argument. It passes only if nothing connects to the listener,
the AX socket path the variable named does not exist, `--render` is refused (`unknown option --render`), and the ordinary positive
control (a page load) is seen by the same observer. `PLANTED_HARNESS_BIN=<harness build>` adds the door's own positive control. The
case needs no harness door itself: the run starts the desktop binary under Xvfb with no address.
`run-macos.sh` takes `LUCINANET_LAUNCHER=<LucinaNet.app/Contents/MacOS/lucinanet>` to start every phase through the bundle launcher,
which scrubs those variables; see `docs/harness-doors.md`.
