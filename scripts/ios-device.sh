#!/bin/sh
# Builds OnEum for an iPhone connected to this Mac, installs it there and starts it.
#   scripts/ios-device.sh [--debug] [--build-only]
#
# Needed once: an Apple ID in Xcode ▸ Settings ▸ Accounts (it signs the app), and the
# iPhone connected, unlocked, trusting this Mac, with Developer Mode switched on
# (Settings ▸ Privacy & Security ▸ Developer Mode). With a free Apple ID the installed
# app runs for seven days and is then installed again.
#
#   ONEUM_IOS_TEAM       the team to sign with (default: the team of the
#                        "Apple Development" certificate in the keychain). If that
#                        team cannot make profiles (its account holder has an
#                        agreement to accept, say), name another one here, such as
#                        your personal team, with an identifier for it below.
#   ONEUM_IOS_BUNDLE_ID  the app's identifier (default app.oneum.OnEum; a personal team
#                        needs one that nobody else has registered)
#   ONEUM_IOS_DEVICE     the iPhone, by name or identifier (default: the one connected)
#
# The same can be done in Xcode: open ios/OnEum.xcodeproj, choose the team under
# Signing & Capabilities, choose the iPhone and press Run.
set -eu
cd "$(dirname "$0")/.."
CONFIG=Release
INSTALL=1
for arg in "$@"; do
  case "$arg" in
    --debug) CONFIG=Debug ;;
    --build-only) INSTALL=0 ;;
    *) echo "usage: ios-device.sh [--debug] [--build-only]" >&2; exit 2 ;;
  esac
done
BUNDLE_ID=${ONEUM_IOS_BUNDLE_ID:-app.oneum.OnEum}
TEAM=${ONEUM_IOS_TEAM:-}
if [ -z "$TEAM" ]; then
  TEAM=$(security find-certificate -c "Apple Development" -p 2>/dev/null \
    | openssl x509 -noout -subject -nameopt multiline 2>/dev/null \
    | sed -n 's/^ *organizationalUnitName *= *//p' | head -1)
fi
if [ -z "$TEAM" ]; then
  echo "No development team to sign with. Add your Apple ID in Xcode ▸ Settings ▸ Accounts," >&2
  echo "or set ONEUM_IOS_TEAM to your team's identifier." >&2
  exit 1
fi
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)

xcodebuild -project ios/OnEum.xcodeproj -scheme OnEum -configuration "$CONFIG" \
  -destination 'generic/platform=iOS' -derivedDataPath target/xcode \
  -allowProvisioningUpdates \
  DEVELOPMENT_TEAM="$TEAM" ONEUM_BUNDLE_ID="$BUNDLE_ID" \
  MARKETING_VERSION="$VERSION" CURRENT_PROJECT_VERSION="$BUILD" build
APP=target/xcode/Build/Products/$CONFIG-iphoneos/OnEum.app
# An iPhone refuses an app whose signature is missing or not of what is in it, with a
# message that does not say so plainly: better to find out here.
if ! codesign --verify --strict "$APP" 2>/dev/null; then
  echo "The app that was built is not signed as it stands ($APP):" >&2
  codesign --verify --strict "$APP" >&2 || true
  exit 1
fi
[ "$INSTALL" = 1 ] || { echo "$APP"; exit 0; }

DEVICE=${ONEUM_IOS_DEVICE:-}
if [ -z "$DEVICE" ]; then
  LIST=$(mktemp)
  xcrun devicectl list devices --json-output "$LIST" >/dev/null 2>&1 || true
  DEVICE=$(python3 - "$LIST" <<'PY'
import json, sys
try:
    devices = json.load(open(sys.argv[1]))["result"]["devices"]
except Exception:
    devices = []
# Real ones only: the simulators of this Mac are listed too ("sameMachine").
phones = [
    d for d in devices
    if d.get("hardwareProperties", {}).get("platform") == "iOS"
    and d.get("connectionProperties", {}).get("pairingState") == "paired"
    and d.get("connectionProperties", {}).get("transportType") != "sameMachine"
]
# The ones that can be reached now come first.
phones.sort(key=lambda d: d.get("connectionProperties", {}).get("tunnelState") != "connected")
if phones:
    print(phones[0].get("identifier", ""))
PY
)
  rm -f "$LIST"
fi
if [ -z "$DEVICE" ]; then
  echo "No iPhone found. Connect it, unlock it and trust this Mac; these are known:" >&2
  xcrun devicectl list devices >&2 || true
  exit 1
fi
xcrun devicectl device install app --device "$DEVICE" "$APP"
xcrun devicectl device process launch --device "$DEVICE" "$BUNDLE_ID"
