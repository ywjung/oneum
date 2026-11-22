#!/bin/sh
# Xcode's build phase for the app: builds the Rust program for what Xcode is building
# for (an iPhone or the Simulator, Debug or Release) and puts it in the place of the
# app's executable. Xcode signs the app after this.
set -eu
# (For the Simulator the entitlements go into the program itself, where the Simulator
# reads them: Xcode has written them to a file for that, which build.rs is told of.)
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
# A release build has no path of this computer in it (scripts/release-paths.sh).
PATHS_OUT=""
if [ "$CONFIGURATION" = Release ]; then
  PATHS_OUT=$(CFLAGS="" sh -c '. scripts/release-paths.sh; printf %s "$CARGO_ENCODED_RUSTFLAGS"')
fi
# cargo is run as it would be from a terminal. Xcode's own environment points the C
# compiler at the iPhone SDK, which breaks the parts of the build that are compiled
# for this Mac (build scripts).
# shellcheck disable=SC2086
env -i HOME="$HOME" USER="${USER:-}" \
  ${PATHS_OUT:+CARGO_ENCODED_RUSTFLAGS="$PATHS_OUT"} \
  PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  ${DEVELOPER_DIR:+DEVELOPER_DIR="$DEVELOPER_DIR"} \
  IPHONEOS_DEPLOYMENT_TARGET="$IPHONEOS_DEPLOYMENT_TARGET" \
  ONEUM_IOS_SIM_ENTITLEMENTS="$TARGET_TEMP_DIR/$PRODUCT_NAME.app-Simulated.xcent" \
  cargo build --locked --bin oneum --target "$TARGET" $MODE
cp "target/$TARGET/$DIR/oneum" "$TARGET_BUILD_DIR/$EXECUTABLE_PATH"
# The signature Xcode made in an earlier build is of the executable that was just
# replaced. Xcode signs again only when something it knows of has changed, and this
# copy is not among that: a second build with nothing else changed left the app
# unsigned ("No code signature found" when it was installed). So where Xcode has
# signed this app before (its entitlements are there from then), it is signed here
# again, as Xcode does it. In a first build Xcode's own signing step follows.
ENTITLEMENTS="$TARGET_TEMP_DIR/$PRODUCT_NAME.app.xcent"
if [ "${CODE_SIGNING_ALLOWED:-NO}" = YES ] && [ -n "${EXPANDED_CODE_SIGN_IDENTITY:-}" ] \
  && [ -f "$ENTITLEMENTS" ] && [ -f "$TARGET_BUILD_DIR/$WRAPPER_NAME/embedded.mobileprovision" ]; then
  /usr/bin/codesign --force --sign "$EXPANDED_CODE_SIGN_IDENTITY" \
    --entitlements "$ENTITLEMENTS" --timestamp=none --generate-entitlement-der \
    "$TARGET_BUILD_DIR/$WRAPPER_NAME"
fi
