# shellcheck shell=bash
# the one place that says what a harness build is and refuses it. SOURCED by tools/release/package-macos.sh and
# .github/workflows/release.yml; a build agent that runs outside this tree carries the same two checks inline.
# A harness build (cargo feature `harness`, docs/harness-doors.md) carries test and measurement
# doors the shipped browser must not have: it is never packaged, published, installed or put over the maintainer's app.
#
# The marker is in the binary's own version line (`lucinanet --version`, crates/lucinanet-ui/src/lib.rs VERSION_LINE) and
# nowhere else, so a file that holds it is a harness build whatever it was named.

HARNESS_MARKER="LUCINANET-HARNESS-BUILD"

# harness_binary FILE: true when FILE is a regular file holding the marker.
harness_binary() {
  [ -f "$1" ] && grep -aqF -- "$HARNESS_MARKER" "$1"
}

# harness_features "a,b c": true when the cargo feature list (commas or spaces) names `harness`, however it is spelt.
harness_features() {
  local list
  list=",$(printf '%s' "$1" | tr ' ' ',' | tr -s ','),"
  case $list in
    *,harness,*|*,lucinanet-servo/harness,*|*,lucinanet-ui/harness,*) return 0 ;;
  esac
  return 1
}

# installed_app_path PATH: true when PATH is, or is inside, a place an app is installed from: /Applications,
# ~/Applications, /System/Applications. A relative path is read against the current directory.
installed_app_path() {
  local p=$1 parent base
  case $p in /*) ;; *) p=$PWD/$p ;; esac
  parent=$(cd "$(dirname "$p")" 2>/dev/null && pwd) && base=$(basename "$p") && p=$parent/$base
  case $p in
    /Applications|/Applications/*|/System/Applications|/System/Applications/*) return 0 ;;
    "${HOME:-/nonexistent-home}/Applications"|"${HOME:-/nonexistent-home}/Applications/"*) return 0 ;;
  esac
  return 1
}

# refuse_harness_features PROG FEATURES: exit 1 with a message when the features name `harness`.
refuse_harness_features() {
  if harness_features "$2"; then
    echo "$1: refusing: the cargo features '$2' include 'harness'. A harness build carries test and measurement doors and is never packaged, published or installed (docs/harness-doors.md)." >&2
    exit 1
  fi
}

# refuse_harness_binary PROG FILE [DESTINATION]: exit 1 with a message when FILE is a harness build. When the destination
# is an installed app path the message says that too: a measurement build is never put over the maintainer's app.
refuse_harness_binary() {
  harness_binary "$2" || return 0
  if [ -n "${3:-}" ] && installed_app_path "$3"; then
    echo "$1: refusing: $2 is a harness build and $3 is an installed app path. A measurement build is never installed over the maintainer's app (docs/harness-doors.md)." >&2
  else
    echo "$1: refusing: $2 is a harness build (it carries the marker $HARNESS_MARKER). A harness build is never packaged, published or installed (docs/harness-doors.md)." >&2
  fi
  exit 1
}

# workflow_cargo_builds FILE: one line per `cargo build` (or `cargo apk build`) command of a workflow file, "KIND<TAB>FEATURES".
# The feature list is read from the command itself, whatever the order of its flags: `--features L`, `--features=L`, `-F L`,
# `-FL`, several of them joined; `--all-features` counts as naming `harness`. A command continued over lines with a trailing
# backslash is read as one. KIND is `smoke` when the command carries the explicit tag `harness-smoke` (a comment on its own
# line directly above it, or a comment at the end of it): a build that is never packaged, uploaded or attached. Every other
# build is KIND `shipped`. Comment lines and `name:` lines are not commands.
workflow_cargo_builds() {
  awk '
    function flush(line, prev,    cmd, tag, n, t, i, f, feats, tok) {
      if (line ~ /^[[:space:]]*#/) return
      if (line ~ /^[[:space:]]*(-[[:space:]]+)?name:/) return
      if (line ~ /^[[:space:]]*(echo|printf|grep)[[:space:]]/) return
      if (line !~ /(^|[[:space:];&|(]|`|\$\()cargo[[:space:]]+(apk[[:space:]]+)?build([[:space:]]|$)/) return
      tag = (line ~ /harness-smoke/ || (prev ~ /^[[:space:]]*#/ && prev ~ /harness-smoke/)) ? "smoke" : "shipped"
      cmd = line; sub(/[[:space:]]#.*$/, "", cmd)
      n = split(cmd, t, /[[:space:]]+/); feats = ""
      for (i = 1; i <= n; i++) {
        tok = t[i]
        if (tok == "--all-features") feats = feats ",harness"
        else if (tok == "--features" || tok == "-F") { feats = feats "," t[i + 1]; i++ }
        else if (tok ~ /^--features=/) feats = feats "," substr(tok, 12)
        else if (tok ~ /^-F./) feats = feats "," substr(tok, 3)
      }
      gsub(/["\x27]/, "", feats); sub(/^,/, "", feats)
      printf "%s\t%s\n", tag, feats
    }
    { line = $0; prev_phys = last
      while (line ~ /\\[[:space:]]*$/ && (getline nxt) > 0) { sub(/\\[[:space:]]*$/, "", line); sub(/^[[:space:]]+/, "", nxt); line = line " " nxt }
      flush(line, prev_phys); last = $0 }
  ' "$1"
}

# guard_workflow_builds PROG FILE...: exit 1 unless every `shipped` cargo build of the files is free of the `harness` feature.
# It reads the feature list of the build itself, so the order of --release and --locked does not matter. A list that is not
# literal (a `$VAR` or `${{ }}`) cannot be read and is refused. At least one shipped build must be found across the files.
guard_workflow_builds() {
  local prog=$1 f kind feats shipped=0
  shift
  for f in "$@"; do
    [ -f "$f" ] || { echo "$prog: $f is not a file" >&2; exit 1; }
    while IFS=$'\t' read -r kind feats; do
      [ -n "$kind" ] || continue
      if [ "$kind" = smoke ]; then echo "$prog: $f: smoke build (tagged harness-smoke), features '$feats'"; continue; fi
      shipped=$((shipped + 1))
      echo "$prog: $f: shipped build, features '$feats'"
      case $feats in
        *'$'*|*'{'*) echo "$prog: refusing: $f has a build whose feature list '$feats' is not literal, so it cannot be checked" >&2; exit 1 ;;
      esac
      if harness_features "$feats"; then
        echo "$prog: refusing: $f has a build that is not tagged harness-smoke and names the harness feature ('$feats'). A harness build is never packaged, published or uploaded (docs/harness-doors.md)." >&2
        exit 1
      fi
    done < <(workflow_cargo_builds "$f")
  done
  [ "$shipped" -gt 0 ] || { echo "$prog: refusing: no shipped cargo build found in: $*" >&2; exit 1; }
}
