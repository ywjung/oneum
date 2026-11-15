#!/bin/sh
# Builds OnEum for Android, installs it on the device (or emulator) that adb sees, and
# starts it.
#   scripts/android-device.sh [--debug]
# A phone needs USB debugging switched on (Settings > Developer options) and has to
# trust this computer. The release build is signed with the debug key unless
# ONEUM_ANDROID_KEYSTORE names a key store (see android/app/build.gradle).
#   ONEUM_ANDROID_SERIAL   which device, as `adb devices` names it (needed with several)
# Installing keeps the app's library and settings from an earlier installation.
set -eu
cd "$(dirname "$0")/.."
. scripts/android-env.sh
MODE=--release
[ "${1:-}" = "--debug" ] && MODE=""
ADB="$ANDROID_SDK/platform-tools/adb"
[ -n "${ONEUM_ANDROID_SERIAL:-}" ] && ADB="$ADB -s $ONEUM_ANDROID_SERIAL"
# shellcheck disable=SC2086
$ADB get-state >/dev/null 2>&1 || { echo "No device: connect one (USB debugging on) or start an emulator; these are known:" >&2; "$ANDROID_SDK/platform-tools/adb" devices >&2; exit 1; }
# shellcheck disable=SC2086
APK=$(scripts/android-build.sh $MODE | tail -1)
$ADB install -r "$APK"
$ADB shell am start -n app.oneum.android/.MainActivity >/dev/null
echo "installed and started $APK"
