#!/usr/bin/env bash
# the zero-network start test for macOS. Same two cases, same positive control, same report shape as
# tests/zero-network/run.sh - and one documented difference, in the "what macOS can prove" section below.
#
# Observers on macOS:
#   dtruss   -f -c '^connect$|^sendto$|^sendmsg$|^sendmmsg$'  NEEDS ROOT. It is how a 20 ms attempt that opens
#            and closes between two polls is caught at all. Without root this script says so and stops, unless
#            the caller accepts the poll-only observer with OBSERVER=lsof-poll.
#   lsof     -nP -i, polled over the started process tree every LSOF_POLL_MS (default 200): names destinations.
#   nettop   -P -x, one sample per second: names flows, including a UDP flow that is already closed. This is
#            what catches a DNS query the 200 ms poll can miss.
#
# Cases: fresh (empty profile) and seeded (history.json + bookmarks.json written by the stores' own
# serializers, crates/lucinanet-historydb/examples/seed-profile.rs), each with its own positive control: load
# http://127.0.0.1:<port> from a local listener - the observer must see THAT, or it is blind and the idle
# result means nothing. A short probe start on unusable store files shows the start really reads them.
#
# PASS rule: during the idle start the observers record zero connect/sendto/sendmsg toward anything that is
# not AF_UNIX and not loopback (127.0.0.0/8, ::1). A destination that cannot be classified is a violation,
# never a silent pass. (macOS has no AF_NETLINK; local kernel IPC there is AF_SYSTEM/PF_ROUTE.)
#
# what macOS can prove, and where that is weaker than the Linux script
# ---------------------------------------------------------------------
# dtruss prints the sockaddr as a POINTER: `connect(0x4, 0x700009671c50, 0x10)`. Only the third argument, the
# socklen_t, says anything about the family, and that band is not clean: 28 is sockaddr_in6 (unambiguous),
# 104..108 is the full sockaddr_un, but 16 is sockaddr_in AND the socklen of an AF_UNIX path of 14 bytes, and
# any short unix path (an X11 socket) lands in 2..103 too. dtruss also prints sendto/sendmsg with their raw
# pointer arguments, so those calls cannot be classified at all. So:
#   * a destination NAMED by lsof or nettop and outside loopback is a violation (identical to Linux);
#   * a dtruss connect in the unambiguous IPv6 band, or outside every known band, is a violation;
#   * a dtruss connect in the ambiguous band is reported as a warning with its line, never as a pass and
#     never as a silent omission - and the control phase prints what its own user-caused IPv4 connect looks
#     like on that machine, so the first Mac run can calibrate the band against real evidence.
# A Mac PASS is therefore an assertion about named flows plus the unambiguous attempts. The first real run
# should be read together with the ambiguous-band lines, not announced as "same as Linux".
#
# !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
# ! THIS SCRIPT HAS NEVER BEEN RUN - NOT ONCE, NOT ON A REAL MAC. It was written on a Linux desktop and          !
# ! checked with `bash -n`, shellcheck, and by feeding synthetic lsof/dtruss/nettop logs to its own      !
# ! analyzers. Everything macOS-specific is UNVERIFIED: the dtruss and nettop output formats, the        !
# ! socklen band, BSD `nc -l` invocation, lsof column layout, the sudo path, and the window the browser  !
# ! opens on the user's display for the whole idle window. Treat a first Mac run as a check of this      !
# ! file, not as a measurement.                                                                          !
# !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
#
# Running it opens a real browser window on the Mac's display: there is no headless X to hide behind. Run it
# on a session nobody is using.
#
# Exit code: 0 when every selected case passed, 1 otherwise - including missing prerequisites, which is a
# FAIL with a reason and never a skip. The last line is always
#   RESULT zero-network-macos PASS|FAIL cases=<...> violations=<n> controls=<ok>/<total> seed=<...> observer=<...> ambiguous=<n> seconds=<n>
#
# LUCINANET_LAUNCHER=<LucinaNet.app/Contents/MacOS/lucinanet> starts every phase THROUGH the bundle launcher, the way a
# double-click does (it adds --desktop and scrubs every LUCINA* / LUCINANET_* variable). The launcher takes the throwaway
# profile from HOME (LUCINANET_PROFILE_DIR is scrubbed too), so each phase gets its own HOME whose "Library/Application Support/
# LucinaNet" is the case profile. The `planted` case plants the doors' variables (docs/harness-doors.md) at files that would
# reach the local listener and starts with them: through the launcher they must be scrubbed, and the non-harness binary must ignore
# them anyway; nothing may connect to the listener.
#
# Usage: sudo bash tests/zero-network/run-macos.sh [fresh|seeded|planted|all]     (all is the default)
#        LUCINANET_LAUNCHER=/path/to/LucinaNet.app/Contents/MacOS/lucinanet sudo -E bash tests/zero-network/run-macos.sh all
#        OBSERVER=lsof-poll bash tests/zero-network/run-macos.sh seeded     # without root: flows only
set -u

IDLE=${IDLE_SECONDS:-45}
CONTROL=${CONTROL_SECONDS:-15}
PROBE=${PROBE_SECONDS:-12}
LSOF_POLL_MS=${LSOF_POLL_MS:-200}
NETTOP_SECS=${NETTOP_SECS:-1}
TMPROOT=${LN_SCRATCH:-${TMPDIR:?set TMPDIR to a scratch directory (not /tmp)}}
BIN=${LUCINANET_BIN:-$(cd "$(dirname "$0")/../.." && pwd)/target/release/lucinanet-servo}
LAUNCHER=${LUCINANET_LAUNCHER:-}
EXTRA_ENV=()
EXTRA_ARGS=()
SEED_BIN=${LN_SEED_BIN:-}
OBSERVER=${OBSERVER:-auto}
T0_RUN=$(date +%s)

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  sed -n '2,/^set -u/p' "$0" | sed '$d'
  exit 2
fi
CASE=${1:-all}
case "$CASE" in
  fresh)  CASES="fresh" ;;
  seeded) CASES="seeded" ;;
  planted) CASES="planted" ;;
  all)    CASES="fresh seeded planted" ;;
  *) printf 'FAIL: unknown case "%s" (use fresh, seeded, all or -h)\n' "$CASE"
     printf 'RESULT zero-network-macos FAIL cases=none violations=0 controls=0/0 seed=n/a observer=none ambiguous=0 seconds=0\n'
     exit 1 ;;
esac

RUNDIR=$(mktemp -d "$TMPROOT/lucinanet-mac-run.$(date +%Y%m%dT%H%M%S).XXXXXX")
OUT=$RUNDIR/output.txt
KILL_PIDS=""
LISTENER_SH=""
PORT=""
TOTAL_VIOL=0
TOTAL_AMB=0
CONTROLS=0
CONTROLS_OK=0
SEED_NOTE=n/a
OBSERVER_SEEN=none
CASES_DONE=""
CASES_FAILED=""
CASE_VIOL=0
CASE_AMB=0
PROF=""
SEEDTOOL=""
DOAS=""

say() { printf '%s\n' "$*" | tee -a "$OUT"; }
note() { printf '  %s\n' "$*" >>"$OUT"; }

cleanup() {
  local p c
  for p in $KILL_PIDS; do
    [ -n "$p" ] && kill -TERM "$p" 2>/dev/null
  done
  for p in $KILL_PIDS; do
    [ -n "$p" ] && kill -0 "$p" 2>/dev/null && { sleep 1; kill -KILL "$p" 2>/dev/null; }
  done
  for c in $(pgrep -P "${LISTENER_SH:-0}" 2>/dev/null || true); do
    kill -TERM "$c" 2>/dev/null
  done
  if [ "${KEEP_EVIDENCE:-1}" = 0 ]; then rm -rf "$RUNDIR"; fi
}
trap cleanup EXIT

finish() {
  local r=$1 why=${2:-}
  [ -n "$why" ] && say "$why"
  say "RESULT zero-network-macos $r cases=${CASES_DONE:-none} violations=${TOTAL_VIOL} controls=${CONTROLS_OK}/${CONTROLS} seed=${SEED_NOTE} observer=${OBSERVER_SEEN} ambiguous=${TOTAL_AMB} seconds=$(( $(date +%s) - T0_RUN ))"
  [ "$r" = FAIL ] && [ -n "$CASES_FAILED" ] && say "failing case(s):$CASES_FAILED"
  [ "$r" = PASS ] && exit 0
  exit 1
}

mkdir -p "$TMPROOT" || exit 1
say "run dir (evidence): $RUNDIR"
say "!! macOS variant. run.sh is the Linux script that has been run; this file has NEVER been"
say "!! executed anywhere. Its first real run is a check of this file, not a measurement."

# no ambient LUCINA* may add a start URL, prefs or third-party services
while IFS= read -r v; do unset "$v"; done < <(compgen -e | grep -E '^LUCINA' || true)

[ -x "$BIN" ] || finish FAIL "FAIL: no binary at $BIN (LUCINANET_BIN must point at the arm64 binary built on the build Mac; see packaging/macos/README.md)"
[ -z "$LAUNCHER" ] || [ -x "$LAUNCHER" ] || finish FAIL "FAIL: LUCINANET_LAUNCHER is not an executable file: $LAUNCHER"
if command -v sha256sum >/dev/null 2>&1; then SHA=$(sha256sum "$BIN" | cut -c1-16)
else SHA=$(shasum -a 256 "$BIN" | cut -c1-16); fi
say "binary: $BIN ($(stat -f%z "$BIN") bytes, sha256 ${SHA}...)"

# ------------------------------------------------------ root for dtruss, or say so plainly
if [ "$OBSERVER" = auto ]; then
  if [ "$(id -u)" = 0 ] && command -v dtruss >/dev/null 2>&1; then
    OBSERVER=all
  elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null && command -v dtruss >/dev/null 2>&1; then
    OBSERVER=all
    DOAS="sudo -n"
  else
    say "FAIL: the observer that cannot miss a short-lived attempt is dtruss, and dtruss needs root. That is"
    say "      not available here: not root, or sudo would prompt in the middle of the run, or dtruss is"
    say "      missing (it ships with the Xcode command line tools). Nothing is observed yet, so nothing is"
    say "      claimed. Two ways on:"
    say "        sudo -v && sudo bash tests/zero-network/run-macos.sh $CASE    # dtruss + lsof + nettop"
    say "        OBSERVER=lsof-poll bash tests/zero-network/run-macos.sh $CASE  # flows only, on purpose:"
    say "          a poll sees a connection only while it is open, so a connect that opens and closes between"
    say "          two samples is invisible. A PASS in that mode is weaker by construction, and says so."
    finish FAIL "FAIL: no root for dtruss and no observer chosen explicitly"
  fi
fi
case "$OBSERVER" in
  all)
    if [ "$(id -u)" != 0 ]; then
      command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null \
        || finish FAIL "FAIL: OBSERVER=all needs root for dtruss and sudo is not usable without a prompt: run the script with sudo, or use OBSERVER=lsof-poll"
      DOAS="sudo -n"
    fi
    command -v dtruss >/dev/null 2>&1 \
      || finish FAIL "FAIL: dtruss not found (it ships with the Xcode command line tools); OBSERVER=lsof-poll runs without it"
    OBSERVER_SEEN="dtruss+lsof+nettop" ;;
  lsof-poll) OBSERVER_SEEN="lsof+nettop-poll" ;;
  *) finish FAIL "FAIL: OBSERVER must be 'all' (needs root) or 'lsof-poll', not '$OBSERVER'" ;;
esac
command -v lsof >/dev/null 2>&1 || finish FAIL "FAIL: lsof is missing, and it is what names the destinations"
command -v nc >/dev/null 2>&1 || finish FAIL "FAIL: nc is missing (the positive control's listener)"
HAVE_NETTOP=0
command -v nettop >/dev/null 2>&1 && HAVE_NETTOP=1
[ "$HAVE_NETTOP" = 0 ] && note "nettop is not available: a UDP query shorter than the lsof interval would be missed"
say "observer: $OBSERVER_SEEN (lsof of the process tree every ${LSOF_POLL_MS} ms, nettop every ${NETTOP_SECS} s$( [ "$OBSERVER" = all ] && printf ', dtruss on connect/sendto/sendmsg' )$( [ "$HAVE_NETTOP" = 1 ] || printf ', nettop missing' ))"
[ "$OBSERVER" != all ] && say "no dtruss: this run cannot see an attempt that opened and closed between two polls"

# ------------------------------------------------------ the seeder: the stores' own format
root=$(cd "$(dirname "$0")/../.." && pwd)
seed_tool() {
  [ -n "$SEED_BIN" ] && { SEEDTOOL=$SEED_BIN; say "seeder: $SEEDTOOL"; return 0; }
  local c
  for c in "$root"/target/debug/examples/seed-profile "$root"/target/release/examples/seed-profile \
           "$HOME"/LucinaNet-mac/src/target/debug/examples/seed-profile; do
    [ -x "$c" ] && { SEEDTOOL=$c; say "seeder: $c"; return 0; }
  done
  if [ "${LN_SEED_BUILD:-1}" = 1 ] && command -v cargo >/dev/null 2>&1; then
    ( cd "$root" && cargo build --offline -p lucinanet-historydb --example seed-profile ) >"$RUNDIR/seeder-build.log" 2>&1
    for c in "$root"/target/debug/examples/seed-profile "$root"/target/release/examples/seed-profile; do
      [ -x "$c" ] && { SEEDTOOL=$c; say "seeder: built $c"; return 0; }
    done
    note "the seeder build left no example binary; see $RUNDIR/seeder-build.log"
    return 1
  fi
  note "no seeder. Build it with: cargo build --offline -p lucinanet-historydb --example seed-profile, or set LN_SEED_BIN"
  return 1
}

make_profile() {
  local kind=$1 out
  PROF=$(mktemp -d "$TMPROOT/lucinanet-mac-profile.$kind.XXXXXX")
  [ "$kind" = fresh ] && return 0
  out=$("$SEEDTOOL" "$PROF" 2>&1) || { say "FAIL: $SEEDTOOL could not seed $PROF"; note "$out"; return 1; }
  note "seed: $(printf '%s\n' "$out" | tr '\n' ' ')"
  [ -s "$PROF/history.json" ] && [ -s "$PROF/bookmarks.json" ] || {
    say "FAIL: seeding left no store files in $PROF"; return 1; }
  return 0
}

# every pid of the started process tree, comma separated for lsof -p
pid_tree() {
  ps -axo pid=,ppid= | awk -v root="$1" '
    { child[$2] = child[$2] " " $1 }
    END {
      n = split(root, cur, " "); out = ""
      while (n > 0) {
        nxt = ""; nn = 0
        for (i = 1; i <= n; i++) {
          out = out " " cur[i]
          split(child[cur[i]], a, " ")
          for (j in a) if (a[j] != "") { nn++; nxt = nxt " " a[j] }
        }
        n = nn; split(nxt, cur, " ")
      }
      sub(/^ /, "", out); gsub(/ /, ",", out); print out
    }'
}

# run_phase <name> <secs> <url-or-empty> <profile>: observed start, idle, SIGTERM.
# Evidence files in $RUNDIR: <name>.lsof.log, <name>.nettop.log, <name>.dtruss.log, <name>.stdout.
run_phase() {
  local name=$1 secs=$2 url=$3 prof=$4 pidf bpid polls pids
  pidf=$RUNDIR/$name.pid
  : >"$RUNDIR/$name.lsof.log"
  : >"$RUNDIR/$name.nettop.log"
  # the command: the binary with the case profile in its environment, or (LUCINANET_LAUNCHER) the bundle launcher with its own HOME,
  # whose default profile directory is the case profile (the launcher scrubs LUCINANET_PROFILE_DIR and LUCINANET_MEDIA_SINK)
  if [ -n "$LAUNCHER" ]; then
    mkdir -p "$RUNDIR/home-$name/Library/Application Support" "$RUNDIR/home-$name/Library/Logs"
    ln -sfn "$prof" "$RUNDIR/home-$name/Library/Application Support/LucinaNet"
    run_env=(HOME="$RUNDIR/home-$name"); run_bin=$LAUNCHER
  else
    run_env=(LUCINANET_PROFILE_DIR="$prof" LUCINANET_MEDIA_SINK=none); run_bin=$BIN
  fi
  say "phase $name: profile $prof; start $run_bin${url:+ $url}; idle $secs s"
  if [ "$OBSERVER" = all ]; then
    # dtruss traces the command it is given and follows its children. The wrapper records its own pid, which
    # survives the exec, so the SIGTERM lands on the browser and not on dtruss.
    # UNVERIFIED here: dtruss's argv handling, and where it writes when the traced process is killed.
    $DOAS dtruss -f -c '^connect$|^sendto$|^sendmsg$|^sendmmsg$' -o "$RUNDIR/$name.dtruss.log" -- \
      bash -c 'pf=$1; shift; echo "$$" >"$pf"; exec env "$@"' _ "$pidf" \
        "${run_env[@]}" ${EXTRA_ENV[@]+"${EXTRA_ENV[@]}"} \
        "$run_bin" ${url:+"$url"} ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"} >"$RUNDIR/$name.stdout" 2>&1 &
    KILL_PIDS="$KILL_PIDS $!"
    sleep 2
    [ -s "$pidf" ] || note "$name: 2 s in, dtruss has not recorded a pid - it either failed to start or refused the binary (SIP); analyze_phase will not pass this phase on polls alone"
  else
    env "${run_env[@]}" ${EXTRA_ENV[@]+"${EXTRA_ENV[@]}"} \
      "$run_bin" ${url:+"$url"} ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"} >"$RUNDIR/$name.stdout" 2>&1 &
    bpid=$!
    echo "$bpid" >"$pidf"
    KILL_PIDS="$KILL_PIDS $bpid"
  fi

  # the two polls run over the whole phase: before the first syscall until after the browser is gone
  (
    polls=$(( (secs + 15) * 1000 / LSOF_POLL_MS ))
    for _poll in $(seq 1 "$polls"); do
      pids=$(pid_tree "$(cat "$pidf" 2>/dev/null || echo 0)")
      if [ -n "$pids" ]; then
        lsof -nP -a -p "$pids" -i >>"$RUNDIR/$name.lsof.log" 2>/dev/null
        printf '=== %s pids=%s\n' "$(date +%T)" "$pids" >>"$RUNDIR/$name.lsof.log"
      fi
      sleep "$(awk -v ms="$LSOF_POLL_MS" 'BEGIN{printf "%.3f", ms/1000}')"
    done
  ) &
  KILL_PIDS="$KILL_PIDS $!"
  if [ "$HAVE_NETTOP" = 1 ]; then
    (
      # nettop -L <samples> -t <seconds>: a text stream of per-process flows. UNVERIFIED: the exact flags and
      # the column layout on the Mac that runs this; NETTOP='-my -flags' overrides it.
      samples=$(( secs + 15 ))
      # shellcheck disable=SC2086   # NETTOP is a flag list on purpose, so it has to split
      nettop ${NETTOP:--P -x} -L "$samples" -t "$NETTOP_SECS" >>"$RUNDIR/$name.nettop.log" 2>&1
    ) &
    KILL_PIDS="$KILL_PIDS $!"
  fi

  sleep "$secs"
  bpid=$(cat "$pidf" 2>/dev/null || echo "")
  [ -n "$bpid" ] && kill -TERM "$bpid" 2>/dev/null
  for _gone in $(seq 1 150); do kill -0 "$bpid" 2>/dev/null || break; sleep 0.1; done
  if kill -0 "$bpid" 2>/dev/null; then
    note "$name: survived SIGTERM; SIGKILL"
    kill -KILL "$bpid" 2>/dev/null
  fi
  sleep 2
  say "phase $name: stopped; evidence lsof $(wc -l <"$RUNDIR/$name.lsof.log") lines, nettop $(wc -l <"$RUNDIR/$name.nettop.log") lines, dtruss $(wc -l <"$RUNDIR/$name.dtruss.log" 2>/dev/null) lines"
}

# ---------------------------------------------------------------- the analyzers
# A destination named by a poll. Allowed and counted: AF_UNIX rows, loopback destinations, and rows with no
# peer (a LISTEN or an unconnected socket is not an attempt anywhere). Anything else is a violation, and so is
# a row this analyzer cannot parse.
analyze_lsof() {
  awk '
    /^=== / { ts = substr($2, 1, 8); next }
    $5 == "unix" { unxf++; next }
    $5 == "IPv4" || $5 == "IPv6" {
      name = $9
      if (name == "") next
      if (name !~ /->/) { listen++; next }                      # no peer: a socket, not a connection
      dest = name; sub(/^.*->/, "", dest)
      sub(/ .*\)$/, "", dest)
      if (dest ~ /^127\./ || dest == "::1" || dest ~ /^\[?::1\]?/ || dest ~ /^::ffff:127\./) { loop[dest]++; next }
      if (dest !~ /^[0-9.]+:[0-9]+$/ && dest !~ /^\[.*\]:[0-9]+$/) { unp++; classify("V", "UNPARSED", $0); next }
      classify("V", dest, $0)
    }
    END {
      for (k in seen) { split(k, a, SUBSEP); if (a[1] == "V") n++ }
      if (n == 0) print "VERDICT lsof-poll: zero outgoing (non-loopback) connection named  [a poll cannot see a connection that closed between two samples]"
      else {
        printf "VIOLATIONS %d\n", n
        for (k in seen) { split(k, a, SUBSEP); if (a[1] == "V") printf "VIOLATION dest=%s x%d first=%s pid=%s | %s\n", a[2], cnt[k], firstt[k], pidat[k], raw[k] }
      }
      for (d in loop) printf "loopback %s x%d\n", d, loop[d]
      printf "info lsof: unix x%d no-peer x%d unparsed x%d\n", unxf, listen, unp
    }
    function classify(kind, dest, line,   key) {
      key = kind SUBSEP dest
      if (seen[key]) { cnt[key]++; return }
      seen[key] = 1; cnt[key] = 1; firstt[key] = ts
      pidat[key] = $2; raw[key] = substr(line, 1, 200)
    }
  ' "$1"
}

# nettop names flows with their addresses. Only the DESTINATION is of interest (a source address on a LAN
# machine is non-loopback by definition and never a violation), so a row yields one candidate: the first
# address-shaped field right of an arrow if the row prints one, otherwise the rightmost address-shaped field -
# nettop is documented as local-then-remote. Shape tests avoid interval regexes ({m,n} is not portable to the
# awk that ships with macOS): a dotted quad, ::1, or hex-and-colons with at least two colons. A timestamp has
# dots and a byte counter has neither colons nor dots-with-four-groups, so neither is read as an address. If
# the log has rows but no readable destination the analyzer says so instead of inventing a verdict: nettop is
# the secondary observer here, and the strict ones are lsof (names) and dtruss (attempts).
analyze_nettop() {
  if [ "$HAVE_NETTOP" != 1 ]; then
    echo "nettop: not available on this host - no flow evidence from it, so a UDP query shorter than the lsof interval could be missed"
    echo "VIOLATIONS 0"
    return
  fi
  awk '
    function strip(t)   { gsub(/[\[\]]/, "", t); sub(/:[0-9][0-9]*$/, "", t); return tolower(t) }
    function isquad(t)  { return t ~ /^[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$/ }
    function isv6(t,   e, parts, p) {
      if (t ~ /^::ffff:/) { e = t; sub(/^::ffff:/, "", e); return isquad(e) }
      if (t !~ /^[0-9a-f:]+$/) return 0
      parts = split(t, p, ":")
      return parts >= 3
    }
    function isaddr(f,   t) { t = strip(f); return isquad(t) || isv6(t) }
    function addrname(f,   t) { t = strip(f); if (t ~ /^::ffff:/) sub(/^::ffff:/, "", t); return t }
    function isloop(t)  { return t ~ /^127\./ || t == "::1" || t ~ /^::1:/ }
    function classify(row,    dpart, nf, f, i, dest) {
      gsub(/,/, " ", row)
      dpart = row
      if (match(row, /<->|->|=>/)) dpart = substr(row, RSTART + RLENGTH)
      nf = split(dpart, f, /[ \t]+/)
      dest = ""
      if (dpart != row) { for (i = 1; i <= nf; i++) if (isaddr(f[i])) { dest = addrname(f[i]); break } }
      else { for (i = 1; i <= nf; i++) if (isaddr(f[i])) dest = addrname(f[i]) }
      if (dest == "") { if (row ~ /[0-9]/) noremote++; return }
      seen++
      if (isloop(dest)) loop[dest]++
      else { flow[dest]++; if (!raw[dest]) raw[dest] = substr(row, 1, 160) }
    }
    { classify($0) }
    END {
      vn = 0
      for (t in flow) vn++
      printf "nettop: destination(s) read=%d  rows-without-a-remote-address=%d\n", seen, noremote
      if (vn == 0 && seen == 0 && noremote > 0)
        print "WARN nettop-format-not-understood: this log has rows but nothing this analyzer reads as an address; the verdict then rests on lsof and dtruss, and the first Mac run should fix the field layout"
      if (vn == 0) print "VERDICT nettop: no non-loopback flow destination named"
      else {
        printf "VIOLATIONS %d\n", vn
        for (t in flow) printf "VIOLATION dest=%s x%d | %s\n", t, flow[t], raw[t]
      }
      for (t in loop) printf "loopback-flow %s x%d\n", t, loop[t]
    }
  ' "$1"
}

# dtruss prints its arguments as hex pointers, so the only usable number is the LAST one of connect() and
# sendto(): the socklen_t of the destination. 28 is sockaddr_in6 (unambiguous), 104..108 is the full
# sockaddr_un, but 16 is sockaddr_in AND a unix path of 14 bytes, and any shorter or longer unix path also
# lands in 2..103. That band is not decidable from dtruss, so it is reported, never dropped and never passed
# as a proof. sendmsg()/sendmmsg() take a pointer to a msghdr, so their destination is unreadable at all:
# they are only counted, and the naming observers (lsof, nettop) decide those. UNVERIFIED: see the header.
analyze_dtruss() {
  awk '
    /(connect|sendto)\(/ {
      v = lastlen($0)
      if (v < 0) { odd++; printf "VIOLATION dest=UNPARSEABLE-ARG-LAYOUT x1 | %s\n", $0; next }
      total++
      lens[v]++
      if (v == 28) { i6++; printf "VIOLATION dest=IPV6-SOCKADDR-LEN x1 | %s\n", $0 }
      else if (v >= 104 && v <= 108) unxf++
      else if (v >= 2 && v <= 103) amb++
      else { bad++; printf "VIOLATION dest=UNCLASSIFIED-SOCKLEN-%d x1 | %s\n", v, $0 }
      next
    }
    /sendm?sg\(/ { sm++ }
    END {
      printf "dtruss: connect/sendto=%d ipv6(28)=%d sockaddr_un(104..108)=%d ambiguous(2..103)=%d unclassified=%d unparseable=%d sendmsg/sendmmsg(not classifiable)=%d\n",
             total, i6, unxf, amb, bad, odd, sm
      printf "dtruss socklen histogram:%s\n", hist()
      if (i6 + bad + odd > 0) printf "VIOLATIONS %d\n", i6 + bad + odd
      else print "VERDICT dtruss: no connect/sendto outside the unix and ambiguous bands"
      if (amb > 0)
        printf "ambiguous %d: dtruss cannot tell an IPv4 connect from an AF_UNIX path of the same socklen. These are neither counted as violations nor passed silently - read them against the control phase printout, which shows what this machine writes for one user-caused IPv4 connect.\n", amb
      if (sm > 0)
        printf "note: %d sendmsg/sendmmsg call(s) whose destination dtruss cannot read at all; lsof and nettop are the observers for those\n", sm
    }
    function hist(   k, s) {
      s = ""
      for (k in lens) s = s " " k "x" lens[k]
      return s
    }
    function lastlen(line,   s, n, f) {
      if (!match(line, /\((0x[0-9a-fA-F]+, )+0x[0-9a-fA-F]+\)/)) return -1
      s = substr(line, RSTART, RLENGTH)
      sub(/^\(/, "", s); sub(/\)$/, "", s)
      n = split(s, f, ", ")
      return hex2dec(f[n])
    }
    function hex2dec(h,   i, c, v) {
      sub(/^0x/, "", h); v = 0
      for (i = 1; i <= length(h); i++) { c = substr(h, i, 1); v = v * 16 + index("0123456789abcdef", tolower(c)) - 1 }
      return v
    }
  ' "$1"
}

# One observed phase: every analyzer, all of its text in the report, and only the counts on stdout.
# stdout: "<violations> <ambiguous>"
analyze_phase() {
  local name=$1 lo no dtr lv nv dv dv2 amb
  lo=$(analyze_lsof "$RUNDIR/$name.lsof.log")
  no=$(analyze_nettop "$RUNDIR/$name.nettop.log")
  dv=$(printf '%s\n' "$lo" | sed -n 's/^VIOLATIONS \([0-9]*\).*/\1/p'); dv=${dv:-0}
  nv=$(printf '%s\n' "$no" | sed -n 's/^VIOLATIONS \([0-9]*\).*/\1/p'); nv=${nv:-0}
  lv=$((dv + nv))
  dtr=""
  dv2=0
  amb=0
  if [ "$OBSERVER" = all ]; then
    if [ ! -s "$RUNDIR/$name.dtruss.log" ]; then
      # dtruss wrote nothing: either it never started or its buffer died with the traced process. Without it
      # this phase rests on polls only, and a poll can be blind, so the phase does not get to pass.
      printf 'VIOLATION dest=DTRUSS-NO-EVIDENCE x1 | no dtruss log for this phase, so the attempt-level observer is missing\n' >>"$OUT"
      lv=$((lv + 1))
      dv2=1
    else
      dtr=$(analyze_dtruss "$RUNDIR/$name.dtruss.log")
      dv2=$(printf '%s\n' "$dtr" | sed -n 's/^VIOLATIONS \([0-9]*\).*/\1/p'); dv2=${dv2:-0}
      amb=$(printf '%s\n' "$dtr" | sed -n 's/^ambiguous \([0-9]*\).*/\1/p'); amb=${amb:-0}
      lv=$((lv + dv2))
    fi
  fi
  printf '%s\n' "$lo" >>"$OUT"
  printf '%s\n' "$no" >>"$OUT"
  [ -n "$dtr" ] && printf '%s\n' "$dtr" >>"$OUT"
  if [ "$OBSERVER" = all ]; then
    printf '  phase %s: violations=%s (lsof %s + nettop %s + dtruss %s), ambiguous-connects=%s\n' \
      "$name" "$lv" "$dv" "$nv" "$dv2" "$amb" >>"$OUT"
  else
    printf '  phase %s: violations=%s (lsof %s + nettop %s), ambiguous-connects=%s\n' \
      "$name" "$lv" "$dv" "$nv" "$amb" >>"$OUT"
  fi
  echo "$lv $amb"
}

# ---------------------------------------------------------------- positive control, seed probe
listener_start() {
  PORT=""
  for p in $(seq 28317 28367); do
    lsof -nP -i -sTCP:LISTEN 2>/dev/null | grep -q ":$p " || { PORT=$p; break; }
  done
  [ -n "$PORT" ] || { say "FAIL: no free control port"; return 1; }
  printf 'HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 49\r\nConnection: close\r\n\r\n<html><body>lucinanet control page</body></html>\n' >"$RUNDIR/control.html"
  (
    nc_child=""
    trap 'if [ -n "$nc_child" ]; then kill -TERM "$nc_child" 2>/dev/null; fi; exit 0' TERM INT
    for _i in $(seq 1 20); do
      # UNVERIFIED: BSD netcat takes `nc -l [address] port`, serves one connection and exits; the loop is
      # what makes it a server for the whole phase. If the Mac's nc differs, listener.err shows it.
      nc -l 127.0.0.1 "$PORT" <"$RUNDIR/control.html" >/dev/null 2>>"$RUNDIR/listener.err" &
      nc_child=$!
      wait "$nc_child"
      nc_child=""
    done
  ) &
  LISTENER_SH=$!
  KILL_PIDS="$KILL_PIDS $LISTENER_SH"
  say "control: local listener on 127.0.0.1:$PORT (bash + nc); the page load is the user action"
}

listener_stop() {
  [ -n "$LISTENER_SH" ] || return 0
  kill -TERM "$LISTENER_SH" 2>/dev/null
  for c in $(pgrep -P "$LISTENER_SH" 2>/dev/null || true); do kill -TERM "$c" 2>/dev/null; done
  LISTENER_SH=""
}

# Does the start read the profile's store files at all? A seeded profile that was ignored would pass exactly
# like a fresh one and prove nothing. The read is shown the same way the observer is shown: with a control.
# This profile holds files that are NOT store files, and the start must react: marks.rs moves an unusable
# bookmarks.json to bookmarks.json.unreadable, visits.rs prints "<path> ignored:" for an unusable
# history.json. A write-back proof cannot exist: Saver writes only a store that is dirty (persist.rs:63), and a
# start with no address dirties neither history nor bookmarks, whatever the signal that ends the phase. Measured
# on a Linux desktop 2026-11-21; the Mac has never run this script.
seed_read_probe() {
  local prof bad=0 log
  prof=$(mktemp -d "$TMPROOT/lucinanet-mac-profile.read-probe.XXXXXX")
  printf 'zero-network read probe: this is not a bookmarks file\n' >"$prof/bookmarks.json"
  printf 'zero-network read probe: this is not a history file\n' >"$prof/history.json"
  log=$RUNDIR/read-probe.stdout
  say "probe: a start on a profile whose store files are not store files, $PROBE s"
  run_phase read-probe "$PROBE" "" "$prof"
  if [ -e "$prof/bookmarks.json.unreadable" ]; then
    note "probe: bookmarks.json moved to bookmarks.json.unreadable - $(grep -m1 'unusable' "$log" 2>/dev/null || echo 'the aside file is there')"
  else
    say "FAIL: probe: an unusable bookmarks.json stayed in place: the start never reads the profile's bookmarks"
    bad=1
  fi
  if grep -q ' ignored: ' "$log" 2>/dev/null; then
    note "probe: $(grep -m1 ' ignored: ' "$log")"
  else
    say "FAIL: probe: the unusable history.json was not reported ignored: the start never reads the profile's history"
    bad=1
  fi
  if [ "$bad" = 0 ]; then SEED_NOTE=read-proved; else SEED_NOTE=probe-failed; fi
  return "$bad"
}

# The seed has to survive the observed start: the browser moves an unreadable bookmarks.json aside itself,
# and the seeded URLs must still be in the two files (an idle start rewrites those files only if it dirties
# them, so anything missing was dropped by the reader, not by the writer).
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

run_case() {
  local kind=$1 prof0 bad=0 seen seen_nt counts viol amb
  SEED_NOTE=n/a
  CASE_VIOL=0
  CASE_AMB=0
  say ""
  say "===== case $kind ====="

  if [ "$kind" = seeded ]; then
    seed_read_probe || bad=1
    say "case $kind: the read of the store files is $( [ "$SEED_NOTE" = read-proved ] && echo 'proved, so the seed matters' || echo 'NOT proved - the seeded case would be a fresh run in disguise' )"
  fi

  make_profile "$kind" || return 1
  prof0=$PROF
  run_phase "$kind-start" "$IDLE" "" "$prof0"
  say "observation of the $kind idle start:"
  counts=$(analyze_phase "$kind-start")
  viol=${counts% *}; amb=${counts#* }
  CASE_VIOL=$viol
  CASE_AMB=$amb
  say "  $kind idle start: violations=$viol ambiguous-connects=$amb (the numbers and every line are in $OUT)"
  [ "$viol" = 0 ] || say "FAIL: $kind idle start: $viol violating destination(s) (listed above)"

  listener_start || { bad=1; return "$bad"; }
  make_profile "$kind" || { listener_stop; bad=1; return "$bad"; }
  run_phase "$kind-control" "$CONTROL" "http://127.0.0.1:$PORT/" "$PROF"
  listener_stop

  # the control is what proves the observers are not blind: the user-caused load must be named
  seen=$(grep -c "127\.0\.0\.1:$PORT" "$RUNDIR/$kind-control.lsof.log" 2>/dev/null || true); seen=${seen:-0}
  seen_nt=$(grep -c ":$PORT" "$RUNDIR/$kind-control.nettop.log" 2>/dev/null || true); seen_nt=${seen_nt:-0}
  say "positive control: lsof named 127.0.0.1:$PORT in $seen line(s); nettop named a flow on :$PORT in $seen_nt line(s)"
  if [ "$seen" -eq 0 ] && [ "$seen_nt" -eq 0 ]; then
    say "FAIL: the $kind positive control was seen by neither poll - the observers are blind; the $kind idle result means nothing"
    bad=1
  else
    CONTROLS_OK=$((CONTROLS_OK + 1))
  fi
  CONTROLS=$((CONTROLS + 1))
  if [ "$OBSERVER" = all ]; then
    # calibration: what one user-caused IPv4 connect to loopback looks like in dtruss on THIS machine. The
    # idle phase's ambiguous band is only judgeable against this line, which is why it is printed.
    note "control dtruss connect lines (calibration for the ambiguous band):"
    grep -m4 'connect(' "$RUNDIR/$kind-control.dtruss.log" >>"$OUT" 2>/dev/null
  fi

  if [ "$kind" = seeded ]; then
    seed_integrity "$prof0" "$kind idle start" || bad=1
    seed_integrity "$PROF" "$kind control" || bad=1
  fi

  say "control extras: destinations outside loopback seen while only a loopback page was loaded"
  counts=$(analyze_phase "$kind-control")
  viol=${counts% *}; amb=${counts#* }
  if [ "$viol" != 0 ]; then
    say "FAIL: $kind control phase: $viol non-loopback destination(s) while loading only a local page - traffic the engine made on its own"
    CASE_VIOL=$((CASE_VIOL + viol))
    bad=1
  else
    note "(none)"
  fi
  return "$bad"
}

# ---------------------------------------------------------------- the planted case
# The variables of the former harness doors, planted at files that WOULD reach the local listener if a door were open, and
# `--render <url> <png>` as arguments; the start must make no connect to the listener. With LUCINANET_LAUNCHER the launcher
# scrubs the variables first; either way the non-harness binary ignores them. The ordinary control after it proves the observers.
# UNVERIFIED on a Mac like the rest of this file (it was written and checked on Linux: bash -n, shellcheck).
# connects_to_port <phase>: lines of that phase's lsof and nettop logs naming the listener port.
connects_to_port() {
  local a b
  a=$(grep -c "127\.0\.0\.1:$PORT" "$RUNDIR/$1.lsof.log" 2>/dev/null || true)
  b=$(grep -c ":$PORT" "$RUNDIR/$1.nettop.log" 2>/dev/null || true)
  echo $(( ${a:-0} + ${b:-0} ))
}

run_planted_case() {
  local bad=0 hits seen counts viol amb log
  SEED_NOTE=n/a
  CASE_VIOL=0
  CASE_AMB=0
  say ""
  say "===== case planted ====="
  make_profile fresh || return 1
  listener_start || return 1
  printf 'new Image().src = "http://127.0.0.1:%s/planted-user-js";\n' "$PORT" >"$RUNDIR/planted.js"
  printf '@import url("http://127.0.0.1:%s/planted-user-css");\n' "$PORT" >"$RUNDIR/planted.css"
  printf 'load http://127.0.0.1:%s/planted-script\nwait 1000\n' "$PORT" >"$RUNDIR/planted.script"
  printf 'load http://127.0.0.1:%s/planted-loads\n' "$PORT" >"$RUNDIR/planted.loads"
  EXTRA_ENV=("LUCINANET_USER_JS=$RUNDIR/planted.js" "LUCINANET_USER_CSS=$RUNDIR/planted.css" "LUCINANET_PREFS=dom_web_animations_enabled=true"
             "LUCINANET_SCRIPT=$RUNDIR/planted.script" "LUCINANET_AX=$RUNDIR/planted-ax.sock" "LUCINANET_LOADS=$RUNDIR/planted.loads"
             "LUCINANET_RENDER_HOLD=5")
  say "planted: the doors' variables set, each naming a file that would reach 127.0.0.1:$PORT$([ -n "$LAUNCHER" ] && echo "; started through the launcher $LAUNCHER")"
  run_phase planted-start "$IDLE" "" "$PROF"
  EXTRA_ENV=()
  counts=$(analyze_phase planted-start)
  viol=${counts% *}; amb=${counts#* }
  CASE_VIOL=$viol
  CASE_AMB=$amb
  say "  planted idle start: violations=$viol ambiguous-connects=$amb"
  [ "$viol" = 0 ] || { say "FAIL: planted idle start: $viol violating destination(s)"; bad=1; }
  hits=$(connects_to_port planted-start)
  if [ "$hits" -ne 0 ]; then
    say "FAIL: planted: $hits observer line(s) naming the control listener 127.0.0.1:$PORT: a planted variable opened a door"
    bad=1
  else
    say "planted: no connect to the control listener from the planted start"
  fi
  [ ! -e "$RUNDIR/planted-ax.sock" ] || { say "FAIL: planted: the socket path LUCINANET_AX named exists: the variable was honoured"; bad=1; }

  EXTRA_ARGS=(--render "http://127.0.0.1:$PORT/planted-render" "$RUNDIR/planted-render.png")
  run_phase planted-render 5 "" "$PROF"
  EXTRA_ARGS=()
  hits=$(connects_to_port planted-render)
  if [ "$hits" -ne 0 ] || [ -e "$RUNDIR/planted-render.png" ]; then
    say "FAIL: planted: --render reached the listener ($hits line(s)) or wrote a picture in the non-harness binary"
    bad=1
  else
    log=$RUNDIR/planted-render.stdout
    [ -z "$LAUNCHER" ] || log=$(ls "$RUNDIR"/home-planted-render/Library/Logs/LucinaNet/session-*.log 2>/dev/null | head -1)
    if grep -q "unknown option --render" "$log" 2>/dev/null; then
      say "planted: --render refused, no connect, no picture"
    else
      say "FAIL: planted: --render was not refused as an unknown option (looked in ${log:-no session log})"
      bad=1
    fi
  fi
  listener_stop

  listener_start || { bad=1; return "$bad"; }
  make_profile fresh || { listener_stop; return 1; }
  run_phase planted-control "$CONTROL" "http://127.0.0.1:$PORT/" "$PROF"
  listener_stop
  seen=$(connects_to_port planted-control)
  say "positive control: the observers named the listener port in $seen line(s)"
  CONTROLS=$((CONTROLS + 1))
  if [ "$seen" -eq 0 ]; then
    say "FAIL: the planted positive control was seen by neither poll - the observers are blind; the planted result means nothing"
    bad=1
  else
    CONTROLS_OK=$((CONTROLS_OK + 1))
  fi
  return "$bad"
}

# ---------------------------------------------------------------- go
case " $CASES " in
  *" seeded "*) seed_tool || finish FAIL "FAIL: no profile seeder, so the seeded case cannot run" ;;
  *) note "seeder not needed (no seeded case selected)" ;;
esac
say "WARNING: each phase opens a window on this Mac's display for ${IDLE}s; nothing here is headless."
for c in $CASES; do
  if [ "$c" = planted ]; then run_planted_case; else run_case "$c"; fi && one=PASS || { one=FAIL; CASES_FAILED="$CASES_FAILED $c"; }
  TOTAL_VIOL=$((TOTAL_VIOL + CASE_VIOL))
  TOTAL_AMB=$((TOTAL_AMB + CASE_AMB))
  CASES_DONE="${CASES_DONE:+$CASES_DONE,}$c"
  if [ "$one" = PASS ]; then
    say "case $c: PASS - no network request in ${IDLE}s of idle on a fresh ${c} profile, control seen ($OBSERVER_SEEN, seed: $SEED_NOTE, ambiguous connects: $CASE_AMB)"
  else
    say "case $c: FAIL - $CASE_VIOL violating destination(s) and/or a control that did not land (see above, and $OUT)"
  fi
done
if [ -n "$CASES_FAILED" ]; then finish FAIL; else finish PASS; fi
