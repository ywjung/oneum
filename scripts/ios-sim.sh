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
# The group through which the app and its widgets share a folder. The Simulator reads
# an app's entitlements from a section of the program, not from its signature: the
# build puts this file there (build.rs).
mkdir -p dist/ios-sim
ENTITLEMENTS=$(pwd)/dist/ios-sim/OnEum.entitlements
sed -e "s/\$(ONEUM_BUNDLE_ID)/$BUNDLE_ID/" ios/OnEum.entitlements > "$ENTITLEMENTS"
export ONEUM_IOS_SIM_ENTITLEMENTS="$ENTITLEMENTS"
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
# The widgets (ios/Widget) are an extension of their own, which Xcode builds from the
# same project as for a phone; it goes inside the app. The app is then signed with the
# group that the two share a folder through (the song the widgets show).
xcodebuild -project ios/OnEum.xcodeproj -scheme OnEumWidget -configuration Debug \
  -destination 'generic/platform=iOS Simulator' -derivedDataPath target/xcode-sim \
  ONEUM_BUNDLE_ID="$BUNDLE_ID" MARKETING_VERSION="$VERSION" CURRENT_PROJECT_VERSION="$BUILD" \
  build >target/xcode-sim.log 2>&1 \
  || { echo "the widget extension could not be built: see target/xcode-sim.log" >&2; exit 1; }
mkdir -p "$APP/PlugIns"
cp -R target/xcode-sim/Build/Products/Debug-iphonesimulator/OnEumWidget.appex "$APP/PlugIns/"
codesign --force --sign - --entitlements "$ENTITLEMENTS" "$APP" >/dev/null 2>&1

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
