#!/bin/sh
# The checks every change has to pass, as CI runs them (.github/workflows/ci.yml):
# formatting, lints for this system (and for Windows when the mingw-w64 cross compiler
# is installed, and for the iPhone and Android when their Rust targets are), and the tests.
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
if [ "$(uname)" = Darwin ] && rustup target list --installed | grep -q aarch64-apple-ios-sim; then
  echo "== lints (iPhone)"
  IPHONEOS_DEPLOYMENT_TARGET=16.0 \
    cargo clippy --locked --lib --bin oneum --target aarch64-apple-ios-sim -- -D warnings
fi
# Android: the library (the app there is a shared library built from it), when the
# Rust target and an NDK are there.
if rustup target list --installed 2>/dev/null | grep -q '^aarch64-linux-android$' \
  && ls -d "${ANDROID_HOME:-$HOME/Library/Android/sdk}"/ndk/* >/dev/null 2>&1; then
  echo "== lints (Android)"
  (. scripts/android-env.sh && cargo clippy --locked --lib --target aarch64-linux-android -- -D warnings)
fi
echo "== tests"
cargo test --locked
echo "all checks passed"
