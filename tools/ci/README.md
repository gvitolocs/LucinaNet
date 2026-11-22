# tools/ci

Baselines read by the ratchet jobs of .github/workflows/ci.yml. A job fails on any file that is not in its baseline and
has a finding; files that became clean are reported so they can be taken off the list. Both files hold one repo-relative
path per line (crates/.../x.rs), sorted with LC_ALL=C, lines starting with # are comments.

## fmt-baseline.txt

Files of our workspace members that are not rustfmt-clean. Regenerate from the repo root (no build needed):

    cargo fmt --all -- --check -l | sed "s#^$PWD/##" | grep "\.rs$" | LC_ALL=C sort -u > tools/ci/fmt-baseline.txt

## clippy-baseline.txt (not generated yet; no clippy job in ci.yml until it exists)

Files of our workspace members with a clippy warning. The ratchet job is not in ci.yml yet: a whole-workspace clippy run does not
finish today (lucinanet-media test targets fail to compile with two global allocators in one binary), so there is no baseline. When
it finishes, generate the file with the pipeline below and put the job back (it needs a full clippy run, heavy: it builds every target):

    cargo clippy --workspace --all-targets --locked --keep-going --quiet --message-format=short \
      --features lucinanet-servo/desktop,lucinanet-servo/media -- --cap-lints warn 2>&1 \
      | grep -E "^[^ :]+\.rs:[0-9]+:[0-9]+: warning" | cut -d: -f1 | LC_ALL=C sort -u > tools/ci/clippy-baseline.txt

(the pipeline the clippy job runs once it is back; the paths must come out repo-relative, as the job sees them.)

## Content scan

A private content scan of the whole tree runs as part of the release process; the lists it checks against are held outside the tree and are not published.
