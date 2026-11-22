#!/bin/sh
# The tests as the CI machines run them (.github/workflows/ci.yml): everything
# `cargo test` runs, with the tests of the player (tests/player_flow.rs) one at a time.
# Those play in real time, each on threads of its own; a few dozen of them at once on a
# machine with three cores fall behind the clock and fail for it (the first CI run did:
# "playback stalled", "timed out waiting for the end of the queue").
set -eu
cd "$(dirname "$0")/.."
cargo test --locked --lib --bins
for file in tests/*.rs; do
  name=$(basename "$file" .rs)
  case "$name" in
    player_flow) cargo test --locked --test "$name" -- --test-threads=1 ;;
    *) cargo test --locked --test "$name" ;;
  esac
done
cargo test --locked --doc
