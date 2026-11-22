# shellcheck shell=bash
# Shared by scripts/verify.sh, scripts/verify-shipped.sh and verification/mutants.sh: choose the Verus binary, print which one it
# is, and refuse any version but the one pinned in verification/verus.version. Sourced, never run: it leaves the binary in `verus`.
#
#   VERUS           the Verus binary to use, by name or by path
#   VERUS_HOME      a Verus install directory to look in, when VERUS is unset and `verus` is not on PATH
#   VERUS_PIN_FILE  read the pin somewhere else than verification/verus.version (tests only)
#   VERUS_ALLOW_ANY 1 runs an unpinned Verus anyway, after a loud warning (experiments only)
#
# Exits 3 on a version it will not run, 2 when there is no binary or no pin to compare against.

_here=${BASH_SOURCE[0]:-$0}
_dir=$(cd "$(dirname "$_here")/.." && pwd)
pin_file=${VERUS_PIN_FILE:-$_dir/verification/verus.version}

# Verus comes from VERUS, else from VERUS_HOME, else from PATH.
verus=${VERUS:-${VERUS_HOME:+$VERUS_HOME/verus}}
verus=${verus:-verus}
# Absolute even when it came off PATH, so the line below names the binary that ran.
found=$(command -v "$verus" 2>/dev/null || true)
[ -n "$found" ] || { echo "verus-pin: no verus to run: '$verus' is not an executable on this machine." >&2
                     echo "verus-pin: put verus on PATH, or set VERUS to the binary, or VERUS_HOME to its directory." >&2
                     exit 2; }
verus=$found

# The release Verus prints. Its profile, platform and toolchain lines are not part of the pin: the last one changes with our own
# RUSTUP_TOOLCHAIN, so a whole-output comparison would refuse the pinned release for the wrong reason.
if ! ver_out=$("$verus" --version 2>&1); then
    echo "verus-pin: '$verus' would not print a version, so it cannot be checked against $pin_file:" >&2
    printf '%s\n' "$ver_out" >&2
    exit 3
fi
version=$(printf '%s\n' "$ver_out" | sed -n 's/^[[:space:]]*Version:[[:space:]]*//p')
case $version in
    (*$'\n'*) version=${version%%$'\n'*} ;;
esac
if [ -z "$version" ]; then
    # A Verus old or foreign enough not to print a Version line: its first line is all the identity it offers.
    version=${ver_out%%$'\n'*}
fi
echo "verus: $verus $version"

pinned=$(sed -n '/[^[:space:]]/{s/[[:space:]]//g; p; q;}' "$pin_file" 2>/dev/null || true)
if [ -z "$pinned" ]; then
    echo "verus-pin: no pin to check against: $pin_file is missing or empty." >&2
    exit 2
fi

if [ "$version" != "$pinned" ]; then
    if [ "${VERUS_ALLOW_ANY:-}" = 1 ]; then
        echo "verus-pin: WARNING VERUS_ALLOW_ANY=1: running '$version' although $pin_file pins '$pinned'." >&2
        echo "verus-pin: WARNING this is an experiment. What it proves is nothing about the pinned Verus." >&2
    else
        echo "verus-pin: REFUSED: $verus is version '$version', but $pin_file pins '$pinned'." >&2
        echo "verus-pin: install the pinned release (fonts/tools/install-verus.sh <dir>) or point VERUS at it." >&2
        echo "verus-pin: VERUS_ALLOW_ANY=1 runs it anyway, for experiments only." >&2
        exit 3
    fi
fi
