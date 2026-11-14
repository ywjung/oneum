#!/bin/sh
# Builds OnEum for the iOS Simulator, puts it into an app bundle, installs it on the
# booted simulator and starts it.
#   scripts/ios-sim.sh [--release] [--no-launch] [profile-dir]
# With a profile directory the app is started for verification: the script channel
# (src/ui/automation.rs) reads <profile-dir>/script.txt and the app's output goes to
# <profile-dir>/stderr.log. Its data stays in the simulator's own app container.
#   ONEUM_IOS_DEVICE   simulator to use (name or UDID); default: the booted one
set -eu
cd "$(dirname "$0")/.."
TARGET=aarch64-apple-ios-sim
MIN_IOS=16.0
BUNDLE_ID=app.oneum.OnEum
PROFILE_DIR=debug
CARGO_MODE=""
LAUNCH=1
VERIFY=""
for arg in "$@"; do
  case "$arg" in
    --release) PROFILE_DIR=release; CARGO_MODE="--release --features automation" ;;
    --no-launch) LAUNCH=0 ;;
    *) VERIFY=$arg ;;
  esac
done
DEVICE=${ONEUM_IOS_DEVICE:-booted}

export IPHONEOS_DEPLOYMENT_TARGET=$MIN_IOS
# shellcheck disable=SC2086
cargo build --locked --bin oneum --target $TARGET $CARGO_MODE

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)
APP=dist/ios-sim/OnEum.app
rm -rf "$APP"
mkdir -p "$APP"
cp "target/$TARGET/$PROFILE_DIR/oneum" "$APP/OnEum"
# The values Xcode would fill in from the project's build settings (ios/Info.plist).
sed -e "s/\$(PRODUCT_BUNDLE_IDENTIFIER)/$BUNDLE_ID/" -e "s/\$(EXECUTABLE_NAME)/OnEum/" \
  -e "s/\$(MARKETING_VERSION)/$VERSION/" -e "s/\$(CURRENT_PROJECT_VERSION)/$BUILD/" \
  -e "s/\$(IPHONEOS_DEPLOYMENT_TARGET)/$MIN_IOS/" ios/Info.plist > "$APP/Info.plist"
# The icon as loose files (Xcode compiles ios/Assets.xcassets instead).
ICON=ios/Assets.xcassets/AppIcon.appiconset/icon-1024.png
sips -z 120 120 "$ICON" --out "$APP/AppIcon60x60@2x.png" >/dev/null
sips -z 180 180 "$ICON" --out "$APP/AppIcon60x60@3x.png" >/dev/null
plutil -insert CFBundleIcons -json \
  '{"CFBundlePrimaryIcon":{"CFBundleIconFiles":["AppIcon60x60"],"CFBundleIconName":"AppIcon"}}' \
  "$APP/Info.plist"
# An iPhone app (Xcode writes this from TARGETED_DEVICE_FAMILY).
plutil -insert UIDeviceFamily -json '[1]' "$APP/Info.plist"
plutil -lint "$APP/Info.plist" >/dev/null
codesign --force --sign - "$APP" >/dev/null 2>&1

xcrun simctl terminate "$DEVICE" "$BUNDLE_ID" >/dev/null 2>&1 || true
xcrun simctl install "$DEVICE" "$APP"
[ "$LAUNCH" = 1 ] || { echo "installed"; exit 0; }
if [ -n "$VERIFY" ]; then
  mkdir -p "$VERIFY"
  VERIFY=$(cd "$VERIFY" && pwd)
  : > "$VERIFY/script.txt"
  SIMCTL_CHILD_ONEUM_SCRIPT="$VERIFY/script.txt" \
  SIMCTL_CHILD_ONEUM_LOG="${ONEUM_LOG:-info}" \
  SIMCTL_CHILD_ONEUM_NO_MEDIA_SESSION="${ONEUM_NO_MEDIA_SESSION:-}" \
    xcrun simctl launch --stdout="$VERIFY/stdout.log" --stderr="$VERIFY/stderr.log" \
    "$DEVICE" "$BUNDLE_ID"
else
  xcrun simctl launch "$DEVICE" "$BUNDLE_ID"
fi
