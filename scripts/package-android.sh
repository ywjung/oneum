#!/bin/sh
# Builds the Android version and puts the file it is installed with into dist:
#   dist/OnEum-<version>-android-arm64.apk
#   dist/SHA256SUMS-android.txt
#
# An APK is what installs an app on Android: copy it to the phone and open it there
# (the phone asks once whether the app it is opened with may install apps), or run
# scripts/android-device.sh with the phone connected.
#
# Needs what scripts/android-build.sh needs: the Android SDK with an NDK, the Rust
# target (rustup target add aarch64-linux-android) and a JDK 17 or 21.
#
# The APK carries OnEum's license and the full license texts of its open-source
# parts (under assets), and has no verification channel in it. It is signed with the
# key store ONEUM_ANDROID_KEYSTORE names (see android/app/build.gradle), or else with
# this computer's debug key: enough for one's own phone, but not a key to publish
# with, and a later version has to be signed with the same key to be installed over
# this one. It is for 64-bit ARM phones with Android 11 or later.
set -eu
cd "$(dirname "$0")/.."
. scripts/android-env.sh
export PATH="$HOME/.cargo/bin:$PATH"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

# What has to come with the app (these two files are made here, not kept in git).
ASSETS=android/app/src/main/assets
mkdir -p "$ASSETS"
cp LICENSE "$ASSETS/LICENSE.txt"
python3 scripts/gen-licenses.py --android "$ASSETS/THIRD_PARTY_LICENSES.txt" >/dev/null

BUILT=$(scripts/android-build.sh --release | tail -1)
[ -f "$BUILT" ] || { echo "built APK not found at $BUILT" >&2; exit 1; }
NAME="OnEum-$VERSION-android-arm64"
mkdir -p dist
APK="dist/$NAME.apk"
cp "$BUILT" "$APK"

# The APK has the app in it and what has to come with it, and nothing for testing.
LISTING=$(unzip -l "$APK")
for inside in lib/arm64-v8a/liboneum.so assets/LICENSE.txt assets/THIRD_PARTY_LICENSES.txt; do
  echo "$LISTING" | grep -q " $inside\$" || { echo "the APK lacks $inside" >&2; exit 1; }
done
# Its signature holds, and it is not a build that can be debugged (such a build takes
# test settings from whoever starts it).
# (The newest: sorted by the numbers of the version, which is the folder's name.)
TOOLS="$ANDROID_SDK/build-tools/$(ls "$ANDROID_SDK/build-tools" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)"
[ -x "$TOOLS/apksigner" ] && [ -x "$TOOLS/aapt2" ] \
  || { echo "no apksigner and aapt2 in $TOOLS" >&2; exit 1; }
if [ -z "${JAVA_HOME:-}" ]; then
  JAVA_HOME=$(/usr/libexec/java_home -v 21 2>/dev/null || /usr/libexec/java_home -v 17 2>/dev/null || true)
  [ -n "$JAVA_HOME" ] && export JAVA_HOME
fi
"$TOOLS/apksigner" verify "$APK" || { echo "the APK's signature does not verify" >&2; exit 1; }
BADGING=$("$TOOLS/aapt2" dump badging "$APK") || { echo "aapt2 could not read the APK" >&2; exit 1; }
if echo "$BADGING" | grep -q "application-debuggable"; then
  echo "the APK is a debuggable build" >&2; exit 1
fi

echo "built $APK"
if [ -z "${ONEUM_ANDROID_KEYSTORE:-}" ]; then
  echo "signed with this computer's debug key: for your own phone, not for publishing"
fi
(cd dist && shasum -a 256 "$NAME.apk" > SHA256SUMS-android.txt)
cat dist/SHA256SUMS-android.txt
