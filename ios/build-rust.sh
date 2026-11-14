#!/bin/sh
# Xcode's build phase for the app: builds the Rust program for what Xcode is building
# for (an iPhone or the Simulator, Debug or Release) and puts it in the place of the
# app's executable. Xcode signs the app after this.
set -eu
cd "$SRCROOT/.."
case "$PLATFORM_NAME" in
  iphonesimulator) TARGET=aarch64-apple-ios-sim ;;
  iphoneos) TARGET=aarch64-apple-ios ;;
  *) echo "error: OnEum is not built for $PLATFORM_NAME" >&2; exit 1 ;;
esac
if [ "$CONFIGURATION" = Release ]; then MODE=--release; DIR=release; else MODE=""; DIR=debug; fi
if ! PATH="$HOME/.cargo/bin:$PATH" rustup target list --installed 2>/dev/null | grep -q "^$TARGET\$"; then
  echo "error: the Rust target $TARGET is not installed: rustup target add $TARGET" >&2
  exit 1
fi
# cargo is run as it would be from a terminal. Xcode's own environment points the C
# compiler at the iPhone SDK, which breaks the parts of the build that are compiled
# for this Mac (build scripts).
# shellcheck disable=SC2086
env -i HOME="$HOME" USER="${USER:-}" \
  PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  ${DEVELOPER_DIR:+DEVELOPER_DIR="$DEVELOPER_DIR"} \
  IPHONEOS_DEPLOYMENT_TARGET="$IPHONEOS_DEPLOYMENT_TARGET" \
  cargo build --locked --bin oneum --target "$TARGET" $MODE
cp "target/$TARGET/$DIR/oneum" "$TARGET_BUILD_DIR/$EXECUTABLE_PATH"
