#!/bin/sh
# The tests as the CI machines run them (.github/workflows/ci.yml): everything
# `cargo test` runs, with the tests of the player (tests/player_flow.rs) one at a time.
# Those play in real time, each on threads of its own; a few dozen of them at once on a
# machine with three cores get in each other's way.
#
# Every part is run even after one has failed, and the failures are told together at
# the end: a run of the CI takes twenty minutes, and a failure that hides the ones
# after it costs a run each (the first runs on Windows did).
set -u
cd "$(dirname "$0")/.."
failed=""
run() { # run <name> <cargo arguments…>
  name=$1; shift
  echo "::group::$name"
  if cargo test --locked "$@"; then echo "::endgroup::"; else echo "::endgroup::"; failed="$failed $name"; fi
}
run lib --lib --bins
for file in tests/*.rs; do
  name=$(basename "$file" .rs)
  case "$name" in
    player_flow) run "$name" --test "$name" -- --test-threads=1 ;;
    *) run "$name" --test "$name" ;;
  esac
done
run doc --doc
if [ -n "$failed" ]; then
  echo "FAILED:$failed" >&2
  exit 1
fi
echo "all tests passed"
