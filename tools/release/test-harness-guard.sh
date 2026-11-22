#!/usr/bin/env bash
# shellcheck disable=SC1003,SC2016
# tests for the workflow guard (tools/release/check-workflow-builds.sh, tools/release/harness-guard.sh): it reads the feature list of
# every cargo build of a workflow file, whatever the order of its flags, and refuses a harness build that is not tagged
# `harness-smoke`. Runs on stub workflow files, no build. Run: tools/release/test-harness-guard.sh (exit 0 = all passed)
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
GUARD=$ROOT/tools/release/check-workflow-builds.sh
T=$(mktemp -d "${TMPDIR:-/tmp}/ln-harness-guard.XXXXXX")
trap 'rm -rf "$T"' EXIT
PASS=0; FAILED=0
ok()  { printf 'ok    %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; FAILED=$((FAILED + 1)); }
accept() { local l=$1; shift; if "$GUARD" "$@" >/dev/null 2>&1; then ok "$l"; else bad "$l"; fi; }
refuse() { local l=$1; shift; "$GUARD" "$@" >/dev/null 2>&1; rc=$?; if [ "$rc" -eq 1 ]; then ok "$l"; else bad "$l (exit $rc, wanted 1)"; fi; }

# wf FILE LINE...: a stub workflow with one run step per line
wf() { local f=$1; shift; { printf 'jobs:\n  b:\n    steps:\n      - run: |\n'; for l in "$@"; do printf '          %s\n' "$l"; done; } >"$f"; }

# both flag spellings, with and without harness (an untagged build)
wf "$T/a-clean"  'cargo build --release --locked -p lucinanet-servo --features desktop,media'
wf "$T/b-clean"  'cargo build --locked --release -p lucinanet-servo --features desktop,media'
wf "$T/a-harn"   'cargo build --release --locked -p lucinanet-servo --features desktop,media,harness'
wf "$T/b-harn"   'cargo build --locked --release -p lucinanet-servo --features desktop,media,harness'
accept "--release --locked without harness is accepted"    "$T/a-clean"
accept "--locked --release without harness is accepted"    "$T/b-clean"
refuse "--release --locked with harness is refused"        "$T/a-harn"
refuse "--locked --release with harness is refused"        "$T/b-harn"

# other spellings of the same build
wf "$T/eq"      'cargo build --locked --release -p lucinanet-servo --features=desktop,media,harness'
wf "$T/short"   'cargo build --locked --release -p lucinanet-servo -F desktop,harness'
wf "$T/after"   'cargo build -p lucinanet-servo --features desktop,media,harness --locked --release'
wf "$T/prefix"  'cargo build --release --locked -p lucinanet-servo --features lucinanet-servo/harness'
wf "$T/all"     'cargo build --release --locked -p lucinanet-servo --all-features'
wf "$T/quoted"  'cargo build --release --locked -p lucinanet-servo --features "desktop,media,harness"'
wf "$T/cont"    'cargo build --locked --release -p lucinanet-servo --features desktop,media,harness \' '  --target x86_64-pc-windows-msvc'
wf "$T/docker"  'docker run --rm img \' '  cargo build --locked --release -j 4 -p lucinanet-servo --features desktop,media,harness'
wf "$T/var"     'cargo build --locked --release -p lucinanet-servo --features "$FEATURES"'
wf "$T/apk"     'cargo apk build --locked --release -p lucinanet-servo --features desktop,harness'
for n in eq short after prefix all quoted cont docker var apk; do refuse "an untagged harness build is refused: $n" "$T/$n"; done

# the explicit tag: a smoke build is a harness build on purpose, never packaged
wf "$T/tag-line"  'cargo build --release --locked -p lucinanet-servo --features desktop,media' \
                  'cargo build --locked --release -p lucinanet-servo --features desktop,media,harness # harness-smoke: never packaged'
wf "$T/tag-above" 'cargo build --locked --release -p lucinanet-servo --features desktop,media' \
                  '# harness-smoke: never packaged' \
                  'cargo build --release --locked -p lucinanet-servo --features desktop,media,harness \' '  --target x86_64-pc-windows-msvc'
accept "a harness build tagged on its own line is accepted beside a clean one"    "$T/tag-line"
accept "a harness build tagged on the line above is accepted beside a clean one"  "$T/tag-above"
# a tag does not cover a build further down, and a tag in a comment elsewhere does not hide an untagged harness build
wf "$T/tag-far" 'cargo build --release --locked -p lucinanet-servo --features desktop,media' \
                '# harness-smoke: the next build only' \
                'cargo build --locked --release -p lucinanet-servo --features desktop,media,harness' \
                'echo between' \
                'cargo build --locked --release -p lucinanet-servo --features desktop,media,harness'
refuse "the tag covers one build: a second harness build is refused" "$T/tag-far"
# only smoke builds, no shipped build: nothing was checked, so it is refused
wf "$T/only-smoke" 'cargo build --locked --release -p lucinanet-servo --features desktop,media,harness # harness-smoke'
refuse "a file with only a smoke build is refused (nothing shipped was found)" "$T/only-smoke"
wf "$T/none" 'echo "cargo build --release is not run here"'
refuse "a file with no build is refused"                  "$T/none"
refuse "a missing file is refused"                        "$T/nope"
"$GUARD" >/dev/null 2>&1; rc=$?; if [ "$rc" -eq 2 ]; then ok "no argument is a usage error (exit 2)"; else bad "no argument is a usage error (exit $rc)"; fi
# several files: one bad file refuses the lot
refuse "a harness build in the second file is refused" "$T/a-clean" "$T/b-harn"
accept "two clean files are accepted"                   "$T/a-clean" "$T/b-clean"

# the real workflows of this tree: the two that upload a build pass, and the smoke builds are the tagged ones
accept "the real release.yml and windows.yml pass the guard" "$ROOT/.github/workflows/release.yml" "$ROOT/.github/workflows/windows.yml"
# the real files with every tag removed must fail (the smoke builds are harness builds, so the tag is what makes them pass)
for w in release windows; do
  sed 's/harness-smoke/untagged/' "$ROOT/.github/workflows/$w.yml" >"$T/$w-untagged.yml"
  refuse "the real $w.yml without its harness-smoke tags is refused" "$T/$w-untagged.yml"
done
# the real files with the smoke build lines reordered stay accepted when tagged and refused when not (the order never mattered)
sed 's/--locked --release/--release --locked/' "$ROOT/.github/workflows/windows.yml" >"$T/win-swapped.yml"
accept "the real windows.yml with the flags swapped is accepted"   "$T/win-swapped.yml" "$ROOT/.github/workflows/release.yml"
sed 's/harness-smoke/untagged/; s/--locked --release/--release --locked/' "$ROOT/.github/workflows/windows.yml" >"$T/win-swapped-untagged.yml"
refuse "... and refused once untagged, whatever the flag order"     "$T/win-swapped-untagged.yml" "$ROOT/.github/workflows/release.yml"

printf -- '--- %s passed, %s failed\n' "$PASS" "$FAILED"
[ "$FAILED" -eq 0 ] || exit 1
