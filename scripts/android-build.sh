#!/bin/sh
# Builds OnEum for Android: the app as a shared library (cargo), then the APK (Gradle).
#   scripts/android-build.sh [--release] [--automation]
# --release      the optimised library and the "release" APK (signed with the debug key
#                unless ONEUM_ANDROID_KEYSTORE names a key store, see android/app/build.gradle)
# --automation   with the scripted verification channel (src/ui/automation.rs)
# Prints the path of the APK. Needs the Android SDK with an NDK (scripts/android-env.sh),
# `rustup target add aarch64-linux-android`, and a JDK 17 or 21 for Gradle.
set -eu
cd "$(dirname "$0")/.."
. scripts/android-env.sh
PROFILE_DIR=debug
CARGO_MODE=""
VARIANT=Debug
FEATURES=""
for arg in "$@"; do
  case "$arg" in
    --release) PROFILE_DIR=release; CARGO_MODE="--release"; VARIANT=Release ;;
    --automation) FEATURES="--features automation" ;;
    *) echo "usage: android-build.sh [--release] [--automation]" >&2; exit 2 ;;
  esac
done

# The library crate, built as a shared library for this one target (the crate's own
# kind stays what the other systems need).
# shellcheck disable=SC2086
cargo rustc --locked --lib --crate-type cdylib --target "$ANDROID_TARGET" $CARGO_MODE $FEATURES
LIBS=android/app/src/main/jniLibs/arm64-v8a
mkdir -p "$LIBS"
cp "target/$ANDROID_TARGET/$PROFILE_DIR/liboneum.so" "$LIBS/liboneum.so"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)
# Gradle's Android plugin needs a JDK it knows (17 or 21), not necessarily the newest.
if [ -z "${JAVA_HOME:-}" ]; then
  JAVA_HOME=$(/usr/libexec/java_home -v 21 2>/dev/null || /usr/libexec/java_home -v 17 2>/dev/null || true)
  [ -n "$JAVA_HOME" ] && export JAVA_HOME
fi
export ANDROID_HOME="$ANDROID_SDK"
(cd android && ./gradlew --quiet "assemble$VARIANT" "-PoneumVersion=$VERSION" "-PoneumBuild=$BUILD")
LOWER=$(echo "$VARIANT" | tr 'A-Z' 'a-z')
echo "android/app/build/outputs/apk/$LOWER/app-$LOWER.apk"
