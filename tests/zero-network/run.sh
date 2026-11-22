#!/usr/bin/env bash
# Prove that a REAL start of the desktop binary makes no network request until the user acts.
# The idle observation must see zero connect/sendto/sendmsg/sendmmsg attempts to any destination that is not
# AF_UNIX, not a local kernel socket (AF_NETLINK) and not loopback (127.0.0.0/8, ::1). Afterwards a positive
# control drives one user-caused request (load http://127.0.0.1:<port> from a tiny local listener) and the SAME
# observer must see that connect: a blind observer cannot pass.
#
# Two cases, same observer, same PASS rule, each with its own positive control:
#   fresh   an empty profile, the double-click of a first run.
#   seeded  a profile that already has a past: history entries and bookmarks, written by the stores'
#                     own serializers (cargo run -p lucinanet-historydb --example seed-profile), then started
#                     with no address. This is what asks whether a remembered page pulls anything - a favicon
#                     fetch for a bookmark, a history prefetch, a resolver lookup for a stored host.
#   planted the variables of the former harness doors planted at files that would reach the local listener,
#                     and `--render` as an argument: the shipped (non-harness) binary must ignore every one of them.
#   all     the three, the default.
#
# Observer: strace -f -qq -tt -yy -e trace=network (the line "observer: strace" says what ran).
# If strace is missing, fall back to polling `ss -tan`/`ss -uan` every 100 ms ("observer: ss-poll");
# the ss poll can miss connections that opened and closed between two polls.
#
# Nothing here changes the engine: no prefs, no third_party, no URL-adding environment. The only
# variables passed to the binary are DISPLAY, a throwaway LUCINANET_PROFILE_DIR and
# LUCINANET_MEDIA_SINK=none (mute; it selects no URL) - except in the planted case, which adds the doors' variables
# on purpose to show they do nothing. All LUCINA* variables of the caller are
# removed first so ambient settings cannot inject a start URL.
#
# Exit code: 0 when every selected case passed, 1 otherwise. The last line is always
#   RESULT zero-network PASS|FAIL cases=<...> violations=<n> controls=<ok>/<total> observer=<...> seconds=<n>
#
# Nightly record. "--record <file>" (or LUCINANET_ZERO_NET_RECORD=<file>) appends ONE stable,
# greppable line per run to <file>, newest last:
#   <UTC ISO> PASS|FAIL commit=<tree sha> binary=<sha256[0:16]> cases=<...> violations=<n> controls=<k/n> observer=<strace|ss-poll> seconds=<s>
# These are the RESULT facts plus the wall-clock time, the git commit of the tree under test and the
# binary hash. A run that aborts on a missing prerequisite (no binary, no Xvfb, no observer) writes
#   <UTC ISO> ERROR commit=<tree sha> binary=none cases=<...> reason=<why>
# instead, and exits non-zero. A PASS line is written only when every selected case passed, no
# destination violated and every positive control landed - an abort is ERROR, a failure is FAIL, never PASS.
# The nightly run records to one fixed file, named by --record/env; a conventional place for it is
# target/nightly/zero-network.log in the checkout, which the release steps read with
# tests/zero-network/release-check.sh. Nothing is recorded unless --record/env is set.
set -u

TMPROOT=${LN_SCRATCH:-${TMPDIR:?set TMPDIR to a scratch directory (not /tmp)}}
mkdir -p "$TMPROOT"
BIN=${LUCINANET_BIN:-$(cd "$(dirname "$0")/../.." && pwd)/target/release/lucinanet-servo}
IDLE=${IDLE_SECONDS:-45}
CONTROL=${CONTROL_SECONDS:-15}
SEED_BIN=${LN_SEED_BIN:-}
T0_RUN=$(date +%s)

# --record <file> / LUCINANET_ZERO_NET_RECORD=<file> writes the nightly line; --self-test runs the
# record/finish/abort logic against stubbed state (no binary, no Xvfb, no network) for test-record.sh.
RECORD_FILE=${LUCINANET_ZERO_NET_RECORD:-}
SELF_TEST=0
CASE=""
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help)
      sed -n '2,/^set -u/p' "$0" | sed '$d'
      exit 2 ;;
    --record)
      [ $# -ge 2 ] || { printf 'FAIL: --record needs a file argument\n'; exit 2; }
      RECORD_FILE=$2; shift 2 ;;
    --record=*)
      RECORD_FILE=${1#*=}; shift ;;
    --self-test)
      SELF_TEST=1; shift ;;
    fresh|seeded|planted|all)
      CASE=$1; shift ;;
    *)
      printf 'FAIL: unknown argument "%s" (use fresh, seeded, planted, all, --record <file>, --self-test or -h)\n' "$1"
      printf 'RESULT zero-network FAIL cases=none violations=? controls=0/0 seed=n/a observer=none seconds=0\n'
      exit 1 ;;
  esac
done
CASE=${CASE:-all}
case "$CASE" in
  fresh)  CASES="fresh" ;;
  seeded) CASES="seeded" ;;
  planted) CASES="planted" ;;
  all)    CASES="fresh seeded planted" ;;
esac

# the tree under test, for the commit= field (defined before the first abort can reach record_status)
root=$(cd "$(dirname "$0")/../.." && pwd)
COMMIT=$(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)

# the summary fields, so that even a fatal setup step can print the one machine-readable line
OBSERVER=""
SEEDTOOL=""
TOTAL_VIOL=0
CONTROLS=0
CONTROLS_OK=0
SEED_NOTE=n/a
CASES_DONE=""
CASES_FAILED=""
BINSHA=""

RUNDIR=$(mktemp -d "$TMPROOT/lucinanet-run.$(date +%Y%m%dT%H%M%S).XXXXXX")
OUT=$RUNDIR/output.txt
KILL_PIDS=()
EXTRA_ENV=()
EXTRA_ARGS=()

say() { printf '%s\n' "$*" | tee -a "$OUT"; }
note() { printf '  %s\n' "$*" >>"$OUT"; }

# append one stable line to RECORD_FILE (a no-op when --record / LUCINANET_ZERO_NET_RECORD is unset).
# Never called with a status that has not been vetted by finish/abort, so a PASS here means a genuinely clean run.
record_status() {
  [ -n "$RECORD_FILE" ] || return 0
  local status=$1 reason=${2:-} line
  line="$(date -u +%Y-%m-%dT%H:%M:%SZ) $status commit=$COMMIT binary=${BINSHA:-none}"
  if [ "$status" = ERROR ]; then
    line="$line cases=${CASES_DONE:-none} reason=$reason"
  else
    line="$line cases=${CASES_DONE:-none} violations=${TOTAL_VIOL} controls=${CONTROLS_OK}/${CONTROLS} observer=${OBSERVER:-none} seconds=$(( $(date +%s) - T0_RUN ))"
  fi
  mkdir -p "$(dirname "$RECORD_FILE")" 2>/dev/null || true
  printf '%s\n' "$line" >>"$RECORD_FILE" || say "WARN: could not write the nightly record $RECORD_FILE"
}

# finish <PASS|FAIL> [why]: the one machine-readable line, always last, with the exit code of the run
finish() {
  local r=$1 why=${2:-}
  [ -n "$why" ] && say "$why"
  # a PASS is only ever emitted (RESULT or record) when every case passed, nothing violated and every control landed
  if [ "$r" = PASS ] && { [ -n "$CASES_FAILED" ] || [ "${TOTAL_VIOL:-0}" -ne 0 ] || [ "${CONTROLS:-0}" -eq 0 ] || [ "${CONTROLS_OK:-0}" -ne "${CONTROLS:-0}" ]; }; then
    r=FAIL
  fi
  say "RESULT zero-network $r cases=${CASES_DONE:-none} violations=${TOTAL_VIOL} controls=${CONTROLS_OK}/${CONTROLS} seed=${SEED_NOTE} observer=${OBSERVER:-none} seconds=$(( $(date +%s) - T0_RUN ))"
  record_status "$r"
  [ "$r" = PASS ] || [ -z "$CASES_FAILED" ] || say "failing case(s):$CASES_FAILED"
  [ "$r" = PASS ] && exit 0
  exit 1
}

# abort <reason>: a missing prerequisite (no binary, no Xvfb, no observer) - records ERROR, prints a FAIL RESULT,
# exits non-zero. It is the one path that never reaches finish, so it never records PASS.
abort() {
  local why=$1
  say "$why"
  record_status ERROR "$why"
  say "RESULT zero-network FAIL cases=${CASES_DONE:-none} violations=${TOTAL_VIOL} controls=${CONTROLS_OK}/${CONTROLS} seed=${SEED_NOTE} observer=${OBSERVER:-none} seconds=$(( $(date +%s) - T0_RUN ))"
  exit 1
}

cleanup() {
  local p c
  for p in "${KILL_PIDS[@]:-}"; do
    [ -n "$p" ] && kill -TERM "$p" 2>/dev/null
  done
  for p in "${KILL_PIDS[@]:-}"; do
    [ -n "$p" ] && kill -0 "$p" 2>/dev/null && { sleep 1; kill -KILL "$p" 2>/dev/null; }
  done
  # the listener subshell's current nc child, if any, is a process this script started
  for c in $(pgrep -P "$LISTENER_SH" 2>/dev/null || true); do
    kill -TERM "$c" 2>/dev/null
  done
  if [ "${KEEP_EVIDENCE:-1}" = 0 ]; then rm -rf "$RUNDIR"; fi
}
trap cleanup EXIT
LISTENER_SH=""

# Self-test: drive the record / finish / abort logic against stubbed state so test-record.sh can
# prove PASS/FAIL/ERROR lines without a binary, Xvfb or the network. Five scenarios append five lines.
if [ "$SELF_TEST" = 1 ]; then
  [ -n "$RECORD_FILE" ] || RECORD_FILE=$RUNDIR/selftest-record.log
  : >"$RECORD_FILE"
  say "self-test: writing five scenarios to $RECORD_FILE (commit=$COMMIT)"
  reset_state() {
    OBSERVER=strace; TOTAL_VIOL=0; CONTROLS=0; CONTROLS_OK=0; SEED_NOTE=n/a
    CASES_DONE=""; CASES_FAILED=""; BINSHA=selftest00000000; T0_RUN=$(date +%s)
  }
  # 1 clean run: every case passed, control landed, nothing violated -> PASS
  ( reset_state; CASES_DONE=fresh; CONTROLS=1; CONTROLS_OK=1; finish PASS ) >/dev/null 2>&1
  # 2 a failed case a caller wrongly reports as PASS -> the guard must record FAIL
  ( reset_state; CASES_DONE=fresh; CONTROLS=1; CONTROLS_OK=1; CASES_FAILED=" fresh"; finish PASS ) >/dev/null 2>&1
  # 3 a blind positive control -> FAIL, never PASS
  ( reset_state; CASES_DONE=fresh; CONTROLS=1; CONTROLS_OK=0; finish PASS ) >/dev/null 2>&1
  # 4 an unflagged violation (violations>0) -> FAIL, never PASS
  ( reset_state; CASES_DONE=fresh; CONTROLS=1; CONTROLS_OK=1; TOTAL_VIOL=2; finish PASS ) >/dev/null 2>&1
  # 5 an aborted run (missing prerequisite) -> ERROR, never PASS
  ( reset_state; abort "FAIL: no binary (self-test abort)" ) >/dev/null 2>&1
  say "self-test: recorded lines:"
  while IFS= read -r rl; do say "  $rl"; done <"$RECORD_FILE"
  exit 0
fi

# the caller's environment, stripped: no ambient LUCINA* may add URLs or prefs
while IFS= read -r v; do unset "$v"; done < <(compgen -e | grep -E '^LUCINA' || true)

[ -x "$BIN" ] || abort "FAIL: no binary at $BIN (build it as in tools/release/README.md, or set LUCINANET_BIN)"
BINSHA=$(sha256sum "$BIN" | cut -c1-16)
say "binary: $BIN ($(/usr/bin/stat -c%s "$BIN") bytes, sha256 ${BINSHA}...)"
say "run dir (evidence): $RUNDIR"

if command -v strace >/dev/null 2>&1 && strace -f -qq -tt -yy -e trace=network -o /dev/null -- /bin/true 2>/dev/null; then
  OBSERVER=strace
else
  OBSERVER=ss-poll
fi
say "observer: $OBSERVER"

# private Xvfb display, TCP listening off; the X socket under /tmp is the only /tmp artifact
DISPLAY_NO=""
for n in $(seq 100 119); do
  [ ! -e "/tmp/.X11-unix/X$n" ] && [ ! -e "/tmp/.X$n-lock" ] && { DISPLAY_NO=$n; break; }
done
[ -n "$DISPLAY_NO" ] || abort "FAIL: no free Xvfb display (:100-:119)"
Xvfb ":$DISPLAY_NO" -screen 0 1280x800x24 -nolisten tcp >"$RUNDIR/xvfb.log" 2>&1 &
KILL_PIDS+=($!)
for _ in $(seq 1 25); do [ -e "/tmp/.X11-unix/X$DISPLAY_NO" ] && break; sleep 0.2; done
[ -e "/tmp/.X11-unix/X$DISPLAY_NO" ] || abort "FAIL: Xvfb :$DISPLAY_NO did not come up (see $RUNDIR/xvfb.log)"
say "display: :$DISPLAY_NO (Xvfb, -nolisten tcp)"

# The seeder: a Rust helper that writes history.json and bookmarks.json with the stores' own serializers
# (crates/lucinanet-historydb/examples/seed-profile.rs), so the seeded profile is byte-for-byte what the
# browser would have saved. Prefer a prebuilt binary; build it only when it is missing.
root=$(cd "$(dirname "$0")/../.." && pwd)
seed_tool() {
  [ -n "$SEED_BIN" ] && { SEEDTOOL=$SEED_BIN; return 0; }
  local c
  for c in "$root"/target/debug/examples/seed-profile "$root"/target/release/examples/seed-profile \
           "$root"/target/juno/examples/seed-profile "$root"/target/probe/examples/seed-profile; do
    [ -x "$c" ] && { SEEDTOOL=$c; return 0; }
  done
  if [ "${LN_SEED_BUILD:-1}" = 1 ] && command -v cargo >/dev/null 2>&1; then
    say "seeder: not built, running cargo build --offline -p lucinanet-historydb --example seed-profile (log: seeder-build.log)"
    ( cd "$root" && cargo build --offline -p lucinanet-historydb --example seed-profile ) \
      >"$RUNDIR/seeder-build.log" 2>&1
    for c in "$root"/target/debug/examples/seed-profile "$root"/target/release/examples/seed-profile; do
      [ -x "$c" ] && { SEEDTOOL=$c; say "seeder: built $c"; return 0; }
    done
    say "FAIL: the seeder did not build (see $RUNDIR/seeder-build.log)"
    return 1
  fi
  say "FAIL: no profile seeder. Build it with"
  say "        cargo build --offline -p lucinanet-historydb --example seed-profile"
  say "      or point LN_SEED_BIN at one."
  return 1
}

# make_profile <case>: a throwaway profile, seeded when the case is "seeded". Sets PROF.
make_profile() {
  local kind=$1 out
  PROF=$(mktemp -d "$TMPROOT/lucinanet-profile.$kind.XXXXXX")
  [ "$kind" = fresh ] && return 0
  out=$("$SEEDTOOL" "$PROF" 2>&1) || { say "FAIL: $SEEDTOOL could not seed $PROF"; note "$out"; return 1; }
  note "seed: $(printf '%s\n' "$out" | tr '\n' ' | ')"
  [ -s "$PROF/history.json" ] && [ -s "$PROF/bookmarks.json" ] || { say "FAIL: seeding left no history.json/bookmarks.json in $PROF"; return 1; }
}

# run_phase <name> <secs> <url-or-empty> <profile>: observed start, idle, SIGTERM.
# EXTRA_ENV (NAME=value words) and EXTRA_ARGS (argv after the address) are set by the planted case only and empty otherwise.
# Sets LAST_LOG to the observation log.
run_phase() {
  local name=$1 secs=$2 url=$3 prof=$4
  local pidf out bpid p
  pidf=$RUNDIR/$name.pid
  if [ "$OBSERVER" = strace ]; then
    out=$RUNDIR/$name.strace.log
  else
    out=$RUNDIR/$name.ss.log
  fi
  say "phase $name: profile $prof; start $BIN${url:+ $url}; idle $secs s; observer -> ${out##*/}"
  if [ "$OBSERVER" = strace ]; then
    strace -f -qq -tt -yy -e trace=network -o "$out" -- \
      bash -c 'pf=$1; shift; echo $$ >"$pf"; exec env "$@"' _ "$pidf" \
      DISPLAY=:"$DISPLAY_NO" LUCINANET_PROFILE_DIR="$prof" LUCINANET_MEDIA_SINK=none ${EXTRA_ENV[@]+"${EXTRA_ENV[@]}"} \
      "$BIN" ${url:+"$url"} ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"} >"$RUNDIR/$name.stdout" 2>&1 &
    KILL_PIDS+=($!)
  else
    env DISPLAY=:"$DISPLAY_NO" LUCINANET_PROFILE_DIR="$prof" LUCINANET_MEDIA_SINK=none ${EXTRA_ENV[@]+"${EXTRA_ENV[@]}"} \
      "$BIN" ${url:+"$url"} ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"} >"$RUNDIR/$name.stdout" 2>&1 &
    bpid=$!
    echo "$bpid" >"$pidf"
    KILL_PIDS+=("$bpid")
    (
      end=$(( $(date +%s%N) + (secs + 15) * 1000000000 ))
      while [ "$(date +%s%N)" -lt "$end" ]; do
        printf '=== %s\n' "$(date +%T.%3N)" >>"$out"
        ss -Htan >>"$out" 2>/dev/null
        ss -Huan >>"$out" 2>/dev/null
        sleep 0.1
      done
    ) &
    KILL_PIDS+=($!)
  fi
  sleep "$secs"
  bpid=$(cat "$pidf" 2>/dev/null)
  [ -n "$bpid" ] && kill -TERM "$bpid" 2>/dev/null
  for _ in $(seq 1 150); do kill -0 "$bpid" 2>/dev/null || break; sleep 0.1; done
  if kill -0 "$bpid" 2>/dev/null; then
    note "$name: survived SIGTERM; SIGKILL"
    kill -KILL "$bpid" 2>/dev/null
  fi
  if [ "$OBSERVER" = strace ]; then
    # sweep survivors among the traced pids: the log names only processes this script started
    sleep 1
    while IFS= read -r p; do
      [ -n "$p" ] && kill -0 "$p" 2>/dev/null && kill -TERM "$p" 2>/dev/null
    done < <(awk '{ if ($1 ~ /^[0-9]+$/) print $1; if (match($0, /\[pid [0-9]+\]/)) { t = substr($0, RSTART + 5, RLENGTH - 6); print t } }' "$out" 2>/dev/null | sort -u)
  fi
  LAST_LOG=$out
  say "phase $name: stopped; ${out##*/} has $(wc -l <"$out" 2>/dev/null) lines"
}

# one observation log in, the verdict and its details out
analyze_log() {
  if [ "$OBSERVER" = ss-poll ]; then
    analyze_ss "$1"
  else
    analyze_strace "$1"
  fi
}

analyze_strace() {
  awk '
    /^[0-9]+ [0-9][0-9]:[0-9][0-9]:[0-9][0-9]\./ || /^[0-9][0-9]:[0-9][0-9]:[0-9][0-9]\./ {
      if (rec != "") analyze(rec); rec = $0; next
    }
    /^[ \t]/ { if (rec != "") rec = rec " " $0; next }
    { if (rec != "") analyze(rec); rec = "" }
    END { if (rec != "") analyze(rec); report() }

    function between(s, a, b,   i, j, t) {
      i = index(s, a); if (!i) return ""
      t = substr(s, i + length(a)); j = index(t, b)
      return j ? substr(t, 1, j - 1) : t
    }
    function classify(kind, dest, line,   key, t, pid) {
      key = kind SUBSEP dest
      if (seen[key]) { cnt[key]++; return }
      seen[key] = 1; cnt[key] = 1
      pid = "main"
      if (line ~ /^[0-9]+ [0-9][0-9]:/) { pid = line; sub(/ .*$/, "", pid) }
      if (line ~ /\[pid /) { pid = pid "/" between(line, "[pid ", "]") }
      t = line
      if (sub(/^[0-9]+ /, "", t)) { sub(/ .*$/, "", t) } else { sub(/ .*$/, "", t) }
      firstt[key] = t; pidat[key] = pid; rawline[key] = substr(line, 1, 320)
    }
    function report(   key, a, n, l) {
      n = 0
      for (key in seen) { split(key, a, SUBSEP); if (a[1] == "V") n++ }
      if (n == 0) {
        print "VERDICT zero outgoing (non-loopback, non-local-IPC) connect/sendto/sendmsg/sendmmsg attempts"
      } else {
        printf "VIOLATIONS %d\n", n
        for (key in seen) {
          split(key, a, SUBSEP); if (a[1] != "V") continue
          printf "VIOLATION dest=%s x%d first=%s pid=%s | %s\n", a[2], cnt[key], firstt[key], pidat[key], rawline[key]
        }
      }
      for (key in seen) {
        split(key, a, SUBSEP)
        if (a[1] == "L") printf "loopback %s x%d first %s\n", a[2], cnt[key], firstt[key]
      }
      printf "info AF_UNIX x%d  AF_NETLINK x%d  AF_UNSPEC x%d  unparsed x%d\n", unixn, netln, unspecn, unp
    }
    function destkey(ip, port) {
      if (ip ~ /:/) return "[" ip "]:" port
      return ip ":" port
    }
    function analyze(line,   fd, i, peer, ip, port) {
      if (line !~ /connect\(/ && line !~ /sendto\(/ && line !~ /sendmsg\(/ && line !~ /sendmmsg\(/) return
      if (line ~ /sa_family=AF_UNIX/)   { unixn++; return }
      if (line ~ /sa_family=AF_NETLINK/) { netln++; return }
      if (line ~ /sa_family=AF_UNSPEC/) { unspecn++; return }
      ip = ""; port = ""
      if (line ~ /inet_addr\("/) {
        ip = between(line, "inet_addr(\"", "\"")
        port = between(line, "sin_port=htons(", ")")
      } else if (line ~ /inet_pton\(AF_INET6, "/) {
        ip = between(line, "inet_pton(AF_INET6, \"", "\"")
        port = between(line, "sin6_port=htons(", ")")
      } else if (match(line, /[0-9]+<[A-Z0-9-]+:\[/)) {
        # destination is not in the arguments (NULL): read the -yy fd annotation <TYPE:[local->peer]>
        fd = substr(line, RSTART); i = index(fd, "]>")
        if (!i) { unp++; classify("V", "UNPARSED", line); return }
        fd = substr(fd, 1, i - 1)
        sub(/^[0-9]+<[A-Z0-9-]+:\[/, "", fd)
        if (fd ~ /^[0-9]+$/) { unixn++; return }           # unconnected local inode
        if (index(fd, "->") == 0) { unp++; classify("V", "UNPARSED", line); return }
        peer = fd; sub(/^.*->/, "", peer)
        if (peer ~ /^[0-9]+$/) { unixn++; return }          # UNIX/pipe inode pair
        gsub(/\[/, "", peer)
        if (peer ~ /^::ffff:127\./ || peer == "::1" || peer == "::ffff:127.0.0.1") {
          ip = peer; port = "0"; sub(/:[0-9]+$/, "", ip)
          if (ip == "") ip = peer
        } else if (peer ~ /^[0-9.]+:[0-9]+$/) {
          ip = peer; sub(/:[0-9]+$/, "", ip); port = peer; sub(/^.*:/, "", port)
        } else if (peer ~ /:[0-9]+$/) {
          ip = peer; sub(/:[0-9]+$/, "", ip); port = peer; sub(/^.*:/, "", port)
        } else {
          unp++; classify("V", "UNPARSED", line); return
        }
        if (ip ~ /^127\./ || ip == "::1" || ip ~ /^::ffff:127/) {
          classify("L", destkey(ip, port), line)
        } else {
          classify("V", destkey(ip, port), line)
        }
        return
      } else {
        unp++; classify("V", "UNPARSED", line); return
      }
      if (ip == "") { unp++; classify("V", "UNPARSED", line); return }
      if (port == "") port = "0"
      if (ip ~ /^127\./ || ip == "::1" || ip ~ /^::ffff:127\./ || ip == "0.0.0.0") {
        classify("L", destkey(ip, port), line)
      } else {
        classify("V", destkey(ip, port), line)
      }
    }
  ' "$1"
}

analyze_ss() {
  awk '
    /^=== / { ts = $2; next }
    / / {
      dest = $(NF - 1)
      if (dest == "" || dest == "0.0.0.0:*" || dest == "*:*" || dest == "[::]:*" || dest == "[v6]:*:*" || dest == "*") next
      h = dest; sub(/%[^:]*:/, ":", h)
      if (h ~ /^127\./ || h ~ /^\[?::1\]?/ || h ~ /^\[?::ffff:127/) { loop[dest]++; next }
      if (!(dest in seen)) { seen[dest] = 1; first[dest] = ts; n++ }
      cnt[dest]++
    }
    END {
      if (n == 0) { print "VERDICT zero outgoing (non-loopback) connections  [ss poll can miss connections shorter than 100 ms]" }
      else {
        printf "VIOLATIONS %d\n", n
        for (d in seen) printf "VIOLATION dest=%s x%d first_seen=%s\n", d, cnt[d], first[d]
      }
      for (d in loop) printf "loopback %s x%d\n", d, loop[d]
    }
  ' "$1"
}

# ---------------------------------------------------------------- the listener of the positive control
# One bash + nc listener (a single nc per accepted connection, looped 20x); python -m http.server is
# deliberately not used. Sets PORT and LISTENER_SH.
listener_start() {
  PORT=""
  for p in $(seq 28317 28367); do
    ss -Hltn 2>/dev/null | grep -q ":$p " || { PORT=$p; break; }
  done
  [ -n "$PORT" ] || { say "FAIL: no free control port"; return 1; }
  printf 'HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 49\r\nConnection: close\r\n\r\n<html><body>lucinanet control page</body></html>\n' >"$RUNDIR/control.html"
  (
    # each nc is a waited child; a TERM here must kill the live child or it is orphaned and keeps the port
    nc_child=""
    trap 'if [ -n "$nc_child" ]; then kill -TERM "$nc_child" 2>/dev/null; fi; exit 0' TERM INT
    for _ in $(seq 1 20); do
      nc -l -n 127.0.0.1 "$PORT" <"$RUNDIR/control.html" >/dev/null 2>>"$RUNDIR/listener.err" &
      nc_child=$!
      wait "$nc_child"
      nc_child=""
    done
  ) &
  LISTENER_SH=$!
  KILL_PIDS+=("$LISTENER_SH")
}

listener_stop() {
  [ -n "$LISTENER_SH" ] || return 0
  kill -TERM "$LISTENER_SH" 2>/dev/null
  for c in $(pgrep -P "$LISTENER_SH" 2>/dev/null || true); do kill -TERM "$c" 2>/dev/null; done
  LISTENER_SH=""
}

# ---------------------------------------------------------------- the seeded profile stays a seed
# If the start did not read the seed, the case is a second fresh run wearing a disguise: it would pass
# without testing anything. So the seed must still be in the files afterwards, and bookmarks.json must
# not have been moved aside as unreadable.
seed_integrity() {
  local prof=$1 phase=$2
  if [ -e "$prof/bookmarks.json.unreadable" ]; then
    say "FAIL: $phase: the browser would not read the seeded bookmarks.json (it moved it to bookmarks.json.unreadable)"
    return 1
  fi
  grep -q 'https://example.com/docs' "$prof/history.json" 2>/dev/null || {
    say "FAIL: $phase: history.json no longer holds the seeded visits, so the start did not read the seed"; return 1; }
  grep -q 'https://example.net/' "$prof/bookmarks.json" 2>/dev/null || {
    say "FAIL: $phase: bookmarks.json no longer holds the seeded bookmarks, so the start did not read the seed"; return 1; }
  return 0
}

# ---------------------------------------------------------------- does the start really read the seed?
# A seeded profile that the start ignored would pass exactly like a fresh one and prove nothing. So the read
# is shown the way the observer is shown: with a control. This profile holds files that are NOT of the
# stores' format (deliberately junk - nothing here is a seed), and the start must react to each of them:
# marks.rs moves an unusable bookmarks.json to bookmarks.json.unreadable, visits.rs reports an unusable
# history.json as "ignored" on stderr. A write-back proof cannot exist: Saver writes only what changed
# (persist.rs:63, a flush with nothing dirty is a no-op), and a start with no address changes neither store -
# the seeded profiles of the 2026-11-21 run held history.json and bookmarks.json at their seed mtimes. The only
# files that appeared mid-run were cookie_jar.json, auth_cache.json and hsts_list.json, written 30 s in as
# empty documents: disk writes, no traffic, so they say nothing about the seed and nothing about the verdict.
# Sets SEED_NOTE to read-proved or probe-failed.
seed_read_probe() {
  local prof bad=0
  prof=$(mktemp -d "$TMPROOT/lucinanet-profile.read-probe.XXXXXX")
  printf 'zero-network read probe: this is not a bookmarks file\n' >"$prof/bookmarks.json"
  printf 'zero-network read probe: this is not a history file\n' >"$prof/history.json"
  say "probe: a start on a profile whose bookmarks.json and history.json are not store files, ${PROBE_SECONDS:-12} s"
  run_phase read-probe "${PROBE_SECONDS:-12}" "" "$prof"
  if [ -e "$prof/bookmarks.json.unreadable" ]; then
    note "probe: bookmarks.json was moved to bookmarks.json.unreadable - $(grep -m1 'unusable' "$RUNDIR/read-probe.stdout" || echo 'the file is there')"
  else
    say "FAIL: probe: an unusable bookmarks.json stayed in place: the start never reads the profile's bookmarks"
    bad=1
  fi
  if grep -q ' ignored: ' "$RUNDIR/read-probe.stdout" 2>/dev/null; then
    note "probe: $(grep -m1 ' ignored: ' "$RUNDIR/read-probe.stdout")"
  else
    say "FAIL: probe: the unusable history.json was not reported ignored: the start never reads the profile's history"
    bad=1
  fi
  if [ "$bad" = 0 ]; then SEED_NOTE=read-proved; else SEED_NOTE=probe-failed; fi
  return "$bad"
}

# ---------------------------------------------------------------- one case
# run_case <fresh|seeded>: the no-address start, idle, observed; then its own positive control.
# Sets CASE_VIOL (violating destinations in the idle phase) and SEED_NOTE; returns 0 when the case passed.
run_case() {
  local kind=$1 prof0 bad=0 seen
  SEED_NOTE=n/a
  CASE_VIOL=0
  say ""
  say "===== case $kind ====="

  if [ "$kind" = seeded ]; then
    seed_read_probe || bad=1
    say "case $kind: the read of the profile files is $([ "$SEED_NOTE" = read-proved ] && echo 'proved, so the seed matters' || echo 'NOT proved - the seeded case would be a fresh run in disguise')"
  fi

  # phase 1: the start exactly as a double-click is: no address argument
  make_profile "$kind" || { bad=1; return "$bad"; }
  prof0=$PROF
  run_phase "$kind-start" "$IDLE" "" "$prof0"
  say "observation of the $kind idle start:"
  local analysis
  analysis=$(analyze_log "$LAST_LOG")
  printf '%s\n' "$analysis" | tee -a "$OUT"
  CASE_VIOL=$(printf '%s\n' "$analysis" | sed -n 's/^VIOLATIONS \([0-9]*\).*/\1/p')
  CASE_VIOL=${CASE_VIOL:-0}
  if [ "$kind" = seeded ]; then
    seed_integrity "$prof0" "$kind idle start" || bad=1
  fi

  # phase 2: positive control - one user action that must produce a visible connect
  listener_start || { bad=1; return "$bad"; }
  if ! make_profile "$kind"; then listener_stop; bad=1; return "$bad"; fi
  say "control: local listener on 127.0.0.1:$PORT (bash + nc); the page load is the user action"
  run_phase "$kind-control" "$CONTROL" "http://127.0.0.1:$PORT/" "$PROF"
  listener_stop

  if [ "$OBSERVER" = strace ]; then
    seen=$(grep -Ec "connect\(.*htons\($PORT\)|connect\(.*<->127\.0\.0\.1:$PORT>" "$LAST_LOG" 2>/dev/null || true)
  else
    seen=$(grep -Ec "127\.0\.0\.1:$PORT" "$LAST_LOG" 2>/dev/null || true)
  fi
  say "positive control: observer recorded $seen line(s) naming the connect to 127.0.0.1:$PORT"
  CONTROLS=$((CONTROLS + 1))
  if [ "${seen:-0}" -eq 0 ]; then
    say "FAIL: the $kind positive control saw nothing - the observer is blind; the idle result of this case means nothing"
    bad=1
  else
    CONTROLS_OK=$((CONTROLS_OK + 1))
  fi

  # the control start used a seeded profile too: what it left behind must still be the seed
  if [ "$kind" = seeded ]; then
    seed_integrity "$PROF" "$kind control" || bad=1
  fi

  say "control extras: destinations outside loopback seen while only a loopback page was loaded"
  local extras
  extras=$(analyze_log "$LAST_LOG" | grep -E '^VIOLATION ' || true)
  if [ -n "$extras" ]; then printf '%s\n' "$extras" | tee -a "$OUT"; else note "(none)"; fi

  return "$bad"
}

# ---------------------------------------------------------------- the planted case
# The shipped binary has no harness doors (cargo feature `harness` is off, so the code is not in it). This case plants
# the variables those doors used to read, at files that WOULD make a request to the local control listener if any
# door were open, and starts the NON-harness binary with them and no address. Nothing may connect to the listener,
# nothing may violate the idle rule, no script effect may show (the AX socket path the variable named must not exist),
# and `--render <url> <png>` must be refused with exit status 2 and no connect. The positive control after it is the
# ordinary user-caused load, the same listener and the same observer, so a blind observer cannot pass this case.
# A start through the real wrapper (tests/zero-network/run-macos.sh, LUCINANET_LAUNCHER) shows the launcher scrub too.
# Optional: PLANTED_HARNESS_BIN=<a build with --features harness> runs the same planted start with the doors OPEN and
# requires the observer to see the connect (the door's own positive control); without it that step is reported "not run".
# connects_to_port <log>: how many lines of the observation log name a connect to the control listener.
connects_to_port() {
  if [ "$OBSERVER" = strace ]; then
    grep -Ec "connect\(.*htons\($PORT\)|connect\(.*<->127\.0\.0\.1:$PORT>" "$1" 2>/dev/null || true
  else
    grep -Ec "127\.0\.0\.1:$PORT" "$1" 2>/dev/null || true
  fi
}

# plant_files <dir> <port>: the files the planted variables name. Each one would fetch http://127.0.0.1:<port>/<door>.
plant_files() {
  local d=$1 port=$2
  printf 'new Image().src = "http://127.0.0.1:%s/planted-user-js"; fetch("http://127.0.0.1:%s/planted-user-js-fetch");\n' "$port" "$port" >"$d/planted.js"
  printf '@import url("http://127.0.0.1:%s/planted-user-css");\nbody { background: url("http://127.0.0.1:%s/planted-user-css-bg"); }\n' "$port" "$port" >"$d/planted.css"
  printf 'load http://127.0.0.1:%s/planted-script\nwait 1000\n' "$port" >"$d/planted.script"
  printf 'load http://127.0.0.1:%s/planted-loads\n' "$port" >"$d/planted.loads"
}

# run_planted_case: the planted start, the refused --render, then the ordinary control. Sets CASE_VIOL; returns 0 on PASS.
run_planted_case() {
  local bad=0 hits analysis extras seen bin_saved planted_env
  SEED_NOTE=n/a
  CASE_VIOL=0
  say ""
  say "===== case planted ====="
  make_profile fresh || return 1
  listener_start || return 1
  plant_files "$RUNDIR" "$PORT"
  planted_env=("LUCINANET_USER_JS=$RUNDIR/planted.js" "LUCINANET_USER_CSS=$RUNDIR/planted.css" "LUCINANET_PREFS=dom_web_animations_enabled=true"
               "LUCINANET_SCRIPT=$RUNDIR/planted.script" "LUCINANET_AX=$RUNDIR/planted-ax.sock" "LUCINANET_LOADS=$RUNDIR/planted.loads"
               "LUCINANET_RENDER_HOLD=5")
  # a local page WITH a head: a document-start user script runs on it (whether the blank start page binds a head is not settled)
  printf '<html><head><title>planted</title></head><body>planted page</body></html>\n' >"$RUNDIR/planted.html"
  say "planted: LUCINANET_USER_JS, _USER_CSS, _PREFS, _SCRIPT, _AX, _LOADS and _RENDER_HOLD set, each naming a local file or path that would reach 127.0.0.1:$PORT"
  EXTRA_ENV=("${planted_env[@]}")
  run_phase planted-start "$IDLE" "" "$PROF"
  EXTRA_ENV=()
  say "observation of the planted idle start:"
  analysis=$(analyze_log "$LAST_LOG")
  printf '%s\n' "$analysis" | tee -a "$OUT"
  CASE_VIOL=$(printf '%s\n' "$analysis" | sed -n 's/^VIOLATIONS \([0-9]*\).*/\1/p')
  CASE_VIOL=${CASE_VIOL:-0}
  hits=$(connects_to_port "$LAST_LOG")
  if [ "${hits:-0}" -ne 0 ]; then
    say "FAIL: planted: $hits connect line(s) to the control listener 127.0.0.1:$PORT: a planted variable opened a door in the non-harness binary"
    bad=1
  else
    say "planted: no connect to the control listener from the planted start"
  fi
  if [ -e "$RUNDIR/planted-ax.sock" ]; then
    say "FAIL: planted: the socket path LUCINANET_AX named exists: the variable was honoured"
    bad=1
  fi

  # the same variables on a page that has a head, where a user script WOULD run if the door were open
  EXTRA_ENV=("${planted_env[@]}")
  run_phase planted-page 12 "file://$RUNDIR/planted.html" "$PROF"
  EXTRA_ENV=()
  hits=$(connects_to_port "$LAST_LOG")
  if [ "${hits:-0}" -ne 0 ]; then
    say "FAIL: planted: $hits connect line(s) to 127.0.0.1:$PORT from a local page with the variables planted: a planted script or style sheet ran"
    bad=1
  else
    say "planted: a local page with a head, started with the variables planted: no connect to the listener"
  fi

  # the flag door: --render must be refused, not taken for an address
  EXTRA_ARGS=(--render "http://127.0.0.1:$PORT/planted-render" "$RUNDIR/planted-render.png")
  run_phase planted-render 3 "" "$PROF"
  EXTRA_ARGS=()
  hits=$(connects_to_port "$LAST_LOG")
  if [ "${hits:-0}" -ne 0 ]; then
    say "FAIL: planted: --render made $hits connect line(s) to 127.0.0.1:$PORT in the non-harness binary"
    bad=1
  fi
  if grep -q 'unknown option --render' "$RUNDIR/planted-render.stdout" 2>/dev/null && [ ! -e "$RUNDIR/planted-render.png" ]; then
    say "planted: --render refused (\"$(grep -m1 'unknown option' "$RUNDIR/planted-render.stdout")\"), no picture written"
  else
    say "FAIL: planted: --render was not refused as an unknown option, or wrote a picture"
    bad=1
  fi

  # the door's own positive control: the same planted start on a harness build must reach the listener
  if [ -n "${PLANTED_HARNESS_BIN:-}" ] && [ -x "${PLANTED_HARNESS_BIN:-}" ]; then
    bin_saved=$BIN; BIN=$PLANTED_HARNESS_BIN
    EXTRA_ENV=("${planted_env[@]}")
    run_phase planted-harness 20 "file://$RUNDIR/planted.html" "$PROF"
    EXTRA_ENV=(); BIN=$bin_saved
    if [ "$(connects_to_port "$LAST_LOG")" -gt 0 ]; then
      say "planted: with the doors open (a harness build) the same planted script DID reach the listener: the planting works"
    else
      say "FAIL: planted: a harness build with the same planted LUCINANET_USER_JS did not reach the listener: the plant proves nothing"
      bad=1
    fi
  else
    say "planted: harness positive control not run (PLANTED_HARNESS_BIN unset); the ordinary control below proves the observer"
  fi
  listener_stop

  # the ordinary positive control: the observer sees a user-caused connect to the same kind of listener
  listener_start || { bad=1; return "$bad"; }
  make_profile fresh || { listener_stop; return 1; }
  run_phase planted-control "$CONTROL" "http://127.0.0.1:$PORT/" "$PROF"
  listener_stop
  seen=$(connects_to_port "$LAST_LOG")
  say "positive control: observer recorded ${seen:-0} line(s) naming the connect to 127.0.0.1:$PORT"
  CONTROLS=$((CONTROLS + 1))
  if [ "${seen:-0}" -eq 0 ]; then
    say "FAIL: the planted positive control saw nothing - the observer is blind; the planted result means nothing"
    bad=1
  else
    CONTROLS_OK=$((CONTROLS_OK + 1))
  fi
  extras=$(analyze_log "$LAST_LOG" | grep -E '^VIOLATION ' || true)
  if [ -n "$extras" ]; then printf '%s\n' "$extras" | tee -a "$OUT"; fi
  return "$bad"
}

# ---------------------------------------------------------------- all cases, one verdict
case " $CASES " in
  *" seeded "*) seed_tool || abort "FAIL: the profile seeder is unavailable, so the seeded case cannot run" ;;
  *) note "seeder not needed (no seeded case selected)" ;;
esac
TOTAL_VIOL=0
CASES_DONE=""
CASES_FAILED=""
for c in $CASES; do
  if [ "$c" = planted ]; then run_planted_case; else run_case "$c"; fi && one=PASS || { one=FAIL; CASES_FAILED="$CASES_FAILED $c"; }
  TOTAL_VIOL=$((TOTAL_VIOL + CASE_VIOL))
  CASES_DONE="${CASES_DONE:+$CASES_DONE,}$c"
  if [ "$one" = PASS ]; then
    say "case $c: PASS - no network request in ${IDLE}s of idle, control seen (seed: $SEED_NOTE)"
  else
    say "case $c: FAIL - $CASE_VIOL violating destination(s) and/or a control that did not land (see above, in $OUT)"
  fi
done

if [ -z "$CASES_FAILED" ]; then
  finish PASS
else
  finish FAIL
fi
