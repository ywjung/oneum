#!/bin/sh
# Builds OnEum for Android: the app as a shared library (cargo), then the APK or the
# app bundle (Gradle).
#   scripts/android-build.sh [--release] [--automation] [--play] [--bundle]
# --release      the optimised library and the "release" build (signed with the debug key
#                unless ONEUM_ANDROID_KEYSTORE names a key store, see android/app/build.gradle)
# --automation   with the scripted verification channel (src/ui/automation.rs)
# --play         the build for Google Play: without "all files access" in its manifest
#                (default: the build that is installed by hand, which may be given it)
# --bundle       an Android App Bundle (.aab), which is what Google Play takes, instead
#                of an APK
#   ONEUM_ANDROID_BUILD   the version code (default: the number of commits)
# Prints the path of what it built. Needs the Android SDK with an NDK
# (scripts/android-env.sh), `rustup target add aarch64-linux-android`, and a JDK 17 or
# 21 for Gradle.
set -eu
cd "$(dirname "$0")/.."
. scripts/android-env.sh
PROFILE_DIR=debug
CARGO_MODE=""
VARIANT=Debug
FEATURES=""
FLAVOR=direct
KIND=apk
for arg in "$@"; do
  case "$arg" in
    --release) PROFILE_DIR=release; CARGO_MODE="--release"; VARIANT=Release ;;
    --automation) FEATURES="--features automation" ;;
    --play) FLAVOR=play ;;
    --bundle) KIND=bundle ;;
    *) echo "usage: android-build.sh [--release] [--automation] [--play] [--bundle]" >&2; exit 2 ;;
  esac
done

# (A release build has no path of this computer in it: see the file.)
# shellcheck disable=SC1091
[ "$PROFILE_DIR" = release ] && . scripts/release-paths.sh
# The library crate, built as a shared library for this one target (the crate's own
# kind stays what the other systems need).
# shellcheck disable=SC2086
cargo rustc --locked --lib --crate-type cdylib --target "$ANDROID_TARGET" $CARGO_MODE $FEATURES
LIBS=android/app/src/main/jniLibs/arm64-v8a
mkdir -p "$LIBS"
cp "target/$ANDROID_TARGET/$PROFILE_DIR/liboneum.so" "$LIBS/liboneum.so"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=${ONEUM_ANDROID_BUILD:-$(git rev-list --count HEAD 2>/dev/null || echo 1)}
# Gradle's Android plugin needs a JDK it knows (17 or 21), not necessarily the newest.
if [ -z "${JAVA_HOME:-}" ]; then
  JAVA_HOME=$(/usr/libexec/java_home -v 21 2>/dev/null || /usr/libexec/java_home -v 17 2>/dev/null || true)
  [ -n "$JAVA_HOME" ] && export JAVA_HOME
fi
export ANDROID_HOME="$ANDROID_SDK"
LOWER=$(echo "$VARIANT" | tr 'A-Z' 'a-z')
# (Gradle names its tasks and files by the flavour and the kind of build: "PlayRelease",
# "play/release", "playRelease".)
CAPITAL=$(echo "$FLAVOR" | cut -c1 | tr 'a-z' 'A-Z')$(echo "$FLAVOR" | cut -c2-)
if [ "$KIND" = bundle ]; then
  (cd android && ./gradlew --quiet "bundle$CAPITAL$VARIANT" "-PoneumVersion=$VERSION" "-PoneumBuild=$BUILD")
  echo "android/app/build/outputs/bundle/$FLAVOR$VARIANT/app-$FLAVOR-$LOWER.aab"
else
  (cd android && ./gradlew --quiet "assemble$CAPITAL$VARIANT" "-PoneumVersion=$VERSION" "-PoneumBuild=$BUILD")
  echo "android/app/build/outputs/apk/$FLAVOR/$LOWER/app-$FLAVOR-$LOWER.apk"
fi
