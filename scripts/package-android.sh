#!/bin/sh
# Builds the Android version and puts the file it is installed with into dist:
#   dist/OnEum-<version>-android-arm64.apk
#   dist/SHA256SUMS-android.txt
#
# An APK is what installs an app on Android: copy it to the phone and open it there
# (the phone asks once whether the app it is opened with may install apps), or run
# scripts/android-device.sh with the phone connected.
#
# This is scripts/android-release.sh --direct: the build that is installed by hand,
# with its checks (see there). The APK carries OnEum's license and the full license
# texts of its open-source parts (under assets), and has no verification channel in
# it. It is signed with the key store ONEUM_ANDROID_KEYSTORE names, or else with this
# computer's debug key: enough for one's own phone, but not a key to publish with,
# and a later version has to be signed with the same key to be installed over this
# one. It is for 64-bit ARM phones with Android 11 or later.
#
# The app bundle for Google Play is made by scripts/android-release.sh (docs/PLAY_STORE.md).
set -eu
exec "$(dirname "$0")/android-release.sh" --direct
