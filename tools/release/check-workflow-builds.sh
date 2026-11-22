#!/usr/bin/env bash
# the preflight guard of the workflows that upload or attach a build: every `cargo build` of the given workflow files whose output
# could reach an upload step must be free of the cargo feature `harness`, whatever the order of its flags. A build that is a
# harness build on purpose (the smoke build, never packaged) carries the tag `harness-smoke` (tools/release/harness-guard.sh).
#
#   tools/release/check-workflow-builds.sh .github/workflows/release.yml .github/workflows/windows.yml
#
# Exit 0 = every untagged build is clean. Exit 1 = a build names harness (or its list cannot be read, or none was found).
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
# shellcheck source=tools/release/harness-guard.sh
. "$ROOT/tools/release/harness-guard.sh"
[ $# -gt 0 ] || { echo "usage: $0 <workflow.yml>..." >&2; exit 2; }
guard_workflow_builds check-workflow-builds "$@"
