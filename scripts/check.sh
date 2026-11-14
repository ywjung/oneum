#!/bin/sh
# The checks every change has to pass, as CI runs them (.github/workflows/ci.yml):
# formatting, lints for this system (and for Windows when the mingw-w64 cross compiler
# is installed), and the tests.
#   scripts/check.sh
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

echo "== format"
cargo fmt --all -- --check
echo "== lints"
cargo clippy --locked --all-targets -- -D warnings
if [ "$(uname)" = Darwin ] && command -v x86_64-w64-mingw32-gcc >/dev/null \
  && rustup target list --installed | grep -q x86_64-pc-windows-gnu; then
  echo "== lints (Windows)"
  cargo clippy --locked --all-targets --target x86_64-pc-windows-gnu -- -D warnings
fi
echo "== tests"
cargo test --locked
echo "all checks passed"
