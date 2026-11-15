# Sourced by the Android scripts: where the NDK is and how cargo is to use it.
#   ONEUM_ANDROID_NDK   the NDK to use (default: the newest one in the Android SDK)
#   ANDROID_HOME        the Android SDK (default: ~/Library/Android/sdk)
ANDROID_TARGET=aarch64-linux-android
# The oldest Android the app runs on: 11, the first with "all files access".
ANDROID_MIN_API=30
ANDROID_SDK=${ANDROID_HOME:-$HOME/Library/Android/sdk}
ANDROID_NDK=${ONEUM_ANDROID_NDK:-$(ls -d "$ANDROID_SDK"/ndk/* 2>/dev/null | sort -V | tail -1)}
if [ -z "$ANDROID_NDK" ] || [ ! -d "$ANDROID_NDK" ]; then
  echo "No Android NDK found below $ANDROID_SDK/ndk (set ONEUM_ANDROID_NDK)." >&2
  exit 1
fi
NDK_BIN=$(ls -d "$ANDROID_NDK"/toolchains/llvm/prebuilt/*/bin | head -1)
export CC_aarch64_linux_android="$NDK_BIN/$ANDROID_TARGET$ANDROID_MIN_API-clang"
export CXX_aarch64_linux_android="$NDK_BIN/$ANDROID_TARGET$ANDROID_MIN_API-clang++"
export AR_aarch64_linux_android="$NDK_BIN/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CC_aarch64_linux_android"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_AR="$AR_aarch64_linux_android"
