#!/bin/sh
# Builds the Android version as it is given to others, and checks what it built:
#
#   dist/play/OnEum-<version>-<build>-play.aab   the app bundle Google Play takes
#   dist/play/OnEum-<version>-<build>-play.apk   the same build as an APK, to try on a
#                                                phone what people get from Play
#   dist/OnEum-<version>-android-arm64.apk       the APK that is installed by hand
#   dist/SHA256SUMS-android.txt                  (the releases page)
#
#   scripts/android-release.sh                   all of them, signed with the key store
#   scripts/android-release.sh --direct          only the APK that is installed by hand
#                                                (scripts/package-android.sh); without
#                                                a key store it gets the debug key
#   scripts/android-release.sh --check <file>…   checks .aab and .apk files that exist
#
# The two builds differ in one line of the manifest: the one for Google Play does not
# ask for "all files access" (Play does not permit that right to a music player), and
# the app then does not offer it in its settings.
#
# What signs them comes from the environment, never from the repository:
#   ONEUM_ANDROID_KEYSTORE                 the key store (scripts/android-keystore.sh
#                                          makes one)
#   ONEUM_ANDROID_KEYSTORE_PASSWORD_FILE   the file its password is in (default: the key
#                                          store's name with .pass for .jks), or
#   ONEUM_ANDROID_KEYSTORE_PASSWORD        the password itself
#   ONEUM_ANDROID_KEY_ALIAS                the key's name in the store (default: upload)
#   ONEUM_ANDROID_BUILD                    the version code (default: the number of
#                                          commits; Google Play takes each only once)
#
# The checks (one that fails ends the script with an error, and what was built is
# taken out of dist again):
#   - the signature holds, and it is the key store's key (not the debug key)
#   - the app's name, its version (Cargo.toml) and version code
#   - the Android it targets is what Google Play asks of a new app, see PLAY_TARGET_API
#   - not a build that can be debugged, nor one marked as a test
#   - the rights it asks for are exactly the known ones; the build for Play is without
#     "all files access"
#   - the app's library is there for arm64 alone, laid out for 16 KB pages, without
#     debugging information and without the verification channel
#   - the licenses are in it
#   - no file in it names a folder of this computer
#
# Needs what scripts/android-build.sh needs (the Android SDK with an NDK, the Rust
# target, a JDK 17 or 21) and python3.
set -eu
cd "$(dirname "$0")/.."
. scripts/android-env.sh
export PATH="$HOME/.cargo/bin:$PATH"

# What Google Play asks of new apps and of updates (since 2026-08-31: Android 16). It
# rises about once a year: developer.android.com/google/play/requirements/target-sdk
PLAY_TARGET_API=36
PACKAGE=app.oneum.android
# The rights the app asks for, and the one more of the build installed by hand.
RIGHTS="android.permission.FOREGROUND_SERVICE
android.permission.FOREGROUND_SERVICE_MEDIA_PLAYBACK
android.permission.INTERNET
android.permission.POST_NOTIFICATIONS
android.permission.READ_EXTERNAL_STORAGE
android.permission.READ_MEDIA_AUDIO
android.permission.WAKE_LOCK"
ALL_FILES=android.permission.MANAGE_EXTERNAL_STORAGE

usage() {
  echo "usage: android-release.sh [--direct] | --check <.aab or .apk>…" >&2
  exit 2
}
MODE=all
case "${1:-}" in
  "") ;;
  --direct) MODE=direct; [ $# = 1 ] || usage ;;
  --check) MODE=check; shift; [ $# -ge 1 ] || usage ;;
  *) usage ;;
esac

# (The newest build tools: sorted by the numbers of the version, the folder's name.)
TOOLS="$ANDROID_SDK/build-tools/$(ls "$ANDROID_SDK/build-tools" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)"
for tool in apksigner aapt2 zipalign; do
  [ -x "$TOOLS/$tool" ] || { echo "no $tool in $TOOLS" >&2; exit 1; }
done
if [ -z "${JAVA_HOME:-}" ]; then
  JAVA_HOME=$(/usr/libexec/java_home -v 21 2>/dev/null || /usr/libexec/java_home -v 17 2>/dev/null || true)
  [ -n "$JAVA_HOME" ] && export JAVA_HOME
fi
KEYTOOL=${JAVA_HOME:+$JAVA_HOME/bin/}keytool
JARSIGNER=${JAVA_HOME:+$JAVA_HOME/bin/}jarsigner
# (Both answer in the language of this computer; their lines are read here by their
# English names.)
ENGLISH=-J-Duser.language=en

# The key store, where one is named: its password is read from its file here and
# handed to Gradle and to keytool through the environment and a file, never as an
# argument (arguments can be seen by other programs) and never printed.
STORE=${ONEUM_ANDROID_KEYSTORE:-}
ALIAS=${ONEUM_ANDROID_KEY_ALIAS:-upload}
OWN_KEY=""
SECRET_COPY=""
cleanup() { [ -z "$SECRET_COPY" ] || rm -f "$SECRET_COPY"; }
trap cleanup EXIT
if [ -n "$STORE" ]; then
  [ -f "$STORE" ] || { echo "there is no key store at $STORE (ONEUM_ANDROID_KEYSTORE)" >&2; exit 1; }
  if [ -z "${ONEUM_ANDROID_KEYSTORE_PASSWORD:-}" ]; then
    SECRET=${ONEUM_ANDROID_KEYSTORE_PASSWORD_FILE:-${STORE%.jks}.pass}
    [ -f "$SECRET" ] || { echo "no password for the key store: neither ONEUM_ANDROID_KEYSTORE_PASSWORD nor the file $SECRET" >&2; exit 1; }
    export ONEUM_ANDROID_KEYSTORE_PASSWORD_FILE="$SECRET"
  else
    # (keytool takes a password from a file: one that only this run has.)
    SECRET_COPY=$(mktemp)
    chmod 600 "$SECRET_COPY"
    printf '%s' "$ONEUM_ANDROID_KEYSTORE_PASSWORD" > "$SECRET_COPY"
    SECRET=$SECRET_COPY
  fi
  export ONEUM_ANDROID_KEYSTORE="$STORE" ONEUM_ANDROID_KEY_ALIAS="$ALIAS"
  # The key's certificate, by its SHA-256: what signed a file is compared with it.
  OWN_KEY=$("$KEYTOOL" $ENGLISH -list -v -keystore "$STORE" -alias "$ALIAS" -storepass:file "$SECRET" 2>/dev/null \
    | sed -n 's/^[[:space:]]*SHA256: *//p' | head -1 | tr -d ':' | tr 'A-F' 'a-f')
  [ -n "$OWN_KEY" ] || { echo "the key '$ALIAS' cannot be read from $STORE (wrong password, or no such key)" >&2; exit 1; }
fi

PROBLEMS=0
good() { echo "  ok   $1"; }
bad() { echo "  FAIL $1" >&2; PROBLEMS=$((PROBLEMS + 1)); }

# facts <.aab or .apk> <where it is unpacked>: what its manifest says, one "key value"
# a line: package, versionCode, versionName, minSdk, targetSdk, debuggable, testOnly,
# and a line "permission <name>" for each right. An APK's manifest is read by aapt2;
# an app bundle keeps its manifest as a protocol buffer, which is read here.
facts() {
  case "$1" in
    *.apk)
      "$TOOLS/aapt2" dump xmltree --file AndroidManifest.xml "$1" | python3 -c '
import re, sys
element = None
for line in sys.stdin:
    found = re.match(r"\s*E: (\S+)", line)
    if found:
        element = found.group(1)
        continue
    found = re.match(r"\s*A: (?:\S+:)?(\w+)(?:\(0x[0-9a-f]+\))?=(.*)$", line)
    if not found:
        continue
    name, value = found.groups()
    value = re.sub(r"\s*\(Raw: .*\)$", "", value.strip()).strip("\"")
    if element == "manifest" and name in ("package", "versionCode", "versionName"):
        print(name, value)
    elif element == "uses-sdk" and name in ("minSdkVersion", "targetSdkVersion"):
        print(name[:-7], value)
    elif element == "uses-permission" and name == "name":
        print("permission", value)
    elif element == "application" and name in ("debuggable", "testOnly"):
        print(name, value)
' ;;
    *.aab)
      python3 - "$2/base/manifest/AndroidManifest.xml" <<'PY'
import sys

def varint(data, at):
    value = shift = 0
    while True:
        byte = data[at]
        at += 1
        value |= (byte & 0x7F) << shift
        if byte < 0x80:
            return value, at
        shift += 7

def fields(data):
    """The fields of a protocol buffer message: (number, value), in their order."""
    at = 0
    while at < len(data):
        key, at = varint(data, at)
        number, kind = key >> 3, key & 7
        if kind == 0:
            value, at = varint(data, at)
        elif kind == 2:
            size, at = varint(data, at)
            value, at = data[at:at + size], at + size
        elif kind == 1:
            value, at = data[at:at + 8], at + 8
        elif kind == 5:
            value, at = data[at:at + 4], at + 4
        else:
            raise SystemExit("the bundle's manifest is not read: an unknown kind of field")
        yield number, value

def element(data):
    """aapt's XmlElement: its name (3), attributes (4: name 2, value 3), children (5)."""
    name, attributes, children = "", {}, []
    for number, value in fields(data):
        if number == 3:
            name = value.decode()
        elif number == 4:
            parts = dict((n, v) for n, v in fields(value) if n in (2, 3))
            attributes[parts.get(2, b"").decode()] = parts.get(3, b"").decode()
        elif number == 5:
            # (An XmlNode: an element (1) or text (2).)
            children += [element(v) for n, v in fields(value) if n == 1]
    return name, attributes, children

root = next(element(v) for n, v in fields(open(sys.argv[1], "rb").read()) if n == 1)
name, attributes, children = root
for key in ("package", "versionCode", "versionName"):
    print(key, attributes.get(key, ""))
for child, its, _ in children:
    if child == "uses-sdk":
        print("minSdk", its.get("minSdkVersion", ""))
        print("targetSdk", its.get("targetSdkVersion", ""))
    elif child == "uses-permission":
        print("permission", its.get("name", ""))
    elif child == "application":
        for key in ("debuggable", "testOnly"):
            if key in its:
                print(key, its[key])
PY
      ;;
  esac
}
fact() { printf '%s\n' "$FACTS" | sed -n "s/^$1 //p" | head -1; }

# check_file <.aab or .apk> [version] [version code] [play|direct]
check_file() {
  FILE=$1
  echo "checking $FILE"
  case "$FILE" in
    *.aab) KIND=aab; ROOT=base; LIB_ROOT=base/lib ;;
    *.apk) KIND=apk; ROOT=.; LIB_ROOT=lib ;;
    *) usage ;;
  esac
  UNPACKED=$(mktemp -d)
  if ! unzip -q "$FILE" -d "$UNPACKED"; then
    bad "it cannot be unpacked"
    rm -rf "$UNPACKED"
    return
  fi

  # The signature: of what is in the file, and by which key.
  if [ "$KIND" = apk ]; then
    SIGNED=$("$TOOLS/apksigner" verify --print-certs "$FILE" 2>/dev/null || true)
    SIGNER=$(printf '%s\n' "$SIGNED" | sed -n 's/^Signer #1 certificate SHA-256 digest: //p')
    OWNER=$(printf '%s\n' "$SIGNED" | sed -n 's/^Signer #1 certificate DN: //p')
  else
    # (An app bundle is signed as a JAR is; Google Play signs the APKs it makes of it.)
    SIGNED=$("$JARSIGNER" $ENGLISH -verify "$FILE" 2>/dev/null || true)
    if printf '%s\n' "$SIGNED" | grep -q '^jar verified'; then
      PRINTED=$("$KEYTOOL" $ENGLISH -printcert -jarfile "$FILE" 2>/dev/null || true)
      SIGNER=$(printf '%s\n' "$PRINTED" | sed -n 's/^[[:space:]]*SHA256: *//p' | head -1 | tr -d ':' | tr 'A-F' 'a-f')
      OWNER=$(printf '%s\n' "$PRINTED" | sed -n 's/^Owner: //p' | head -1)
    else
      SIGNER=""; OWNER=""
    fi
  fi
  if [ -z "$SIGNER" ]; then
    bad "it is not signed, or its signature is not of what is in it"
  elif [ -n "$OWN_KEY" ] && [ "$SIGNER" = "$OWN_KEY" ]; then
    good "signed with the key store's key '$ALIAS' ($OWNER)"
  elif [ -n "$OWN_KEY" ]; then
    bad "signed with another key than the key store's ($OWNER)"
  else
    case "$OWNER" in
      *"Android Debug"*)
        if [ "$MODE" = direct ]; then
          good "signed with this computer's debug key: for your own phone, not for publishing"
        else
          bad "signed with a debug key ($OWNER)"
        fi ;;
      *) good "signed ($OWNER; no key store was named to compare it with)" ;;
    esac
  fi

  FACTS=$(facts "$FILE" "$UNPACKED") || FACTS=""
  [ "$(fact package)" = "$PACKAGE" ] || bad "the app is '$(fact package)', not $PACKAGE"
  VERSION=$(fact versionName); CODE=$(fact versionCode)
  if [ -n "${2:-}" ] && [ "$VERSION" != "$2" ]; then
    bad "the version is '$VERSION', not $2 as in Cargo.toml"
  elif [ -n "${3:-}" ] && [ "$CODE" != "$3" ]; then
    bad "the version code is '$CODE', not $3"
  elif ! printf '%s\n' "$CODE" | grep -Eq '^[1-9][0-9]*$'; then
    bad "the version code '$CODE' is not a whole number"
  else
    good "version $VERSION, version code $CODE"
  fi
  TARGET=$(fact targetSdk)
  if [ "${TARGET:-0}" -ge "$PLAY_TARGET_API" ] 2>/dev/null; then
    good "targets Android API $TARGET (Google Play asks for $PLAY_TARGET_API), runs from API $(fact minSdk)"
  else
    bad "targets Android API '$TARGET': Google Play asks for $PLAY_TARGET_API"
  fi
  [ "$(fact debuggable)" != true ] || bad "it is a build that can be debugged"
  [ "$(fact testOnly)" != true ] || bad "it is marked as a test (testOnly)"

  # The rights: the known ones, all of them, and none else.
  ASKED=$(printf '%s\n' "$FACTS" | sed -n 's/^permission //p' | sort)
  if printf '%s\n' "$ASKED" | grep -Fxq "$ALL_FILES"; then FLAVOR=direct; else FLAVOR=play; fi
  EXPECTED=$RIGHTS
  [ "$FLAVOR" = play ] || EXPECTED=$(printf '%s\n%s\n' "$RIGHTS" "$ALL_FILES" | sort)
  if [ "$ASKED" = "$EXPECTED" ]; then
    if [ "$FLAVOR" = play ]; then
      good "asks for the $(printf '%s\n' "$ASKED" | wc -l | tr -d ' ') known rights, and not for all files access (the build for Google Play)"
    else
      good "asks for the known rights and for all files access (the build that is installed by hand)"
    fi
  else
    bad "the rights it asks for are not the known ones: $(printf '%s\n' "$ASKED" | tr '\n' ' ')"
  fi
  [ "$KIND" != aab ] || [ "$FLAVOR" = play ] \
    || bad "an app bundle is for Google Play, which does not permit all files access to a music player"
  [ -z "${4:-}" ] || [ "$FLAVOR" = "$4" ] || bad "it is the '$FLAVOR' build, not the '$4' one"

  # The app's library: for arm64 alone, for 16 KB pages, a release build.
  LIBRARY="$UNPACKED/$LIB_ROOT/arm64-v8a/liboneum.so"
  OTHERS=$(find "$UNPACKED/$LIB_ROOT" -type f 2>/dev/null | grep -v "/arm64-v8a/liboneum.so$" || true)
  if [ ! -f "$LIBRARY" ]; then
    bad "the app's library (arm64-v8a/liboneum.so) is not in it"
  else
    [ -z "$OTHERS" ] || bad "there are other libraries in it than the app's: $(printf '%s\n' "$OTHERS" | sed "s|^$UNPACKED/||" | tr '\n' ' ')"
    # (Every part that is loaded has to be aligned to 16 KB, 2**14, or more: Google
    # Play asks for it of apps that target Android 15 and later.)
    ALIGNED=$("$NDK_BIN/llvm-objdump" -p "$LIBRARY" | sed -n 's/.*LOAD .* align 2\*\*\([0-9]*\)$/\1/p')
    if [ -n "$ALIGNED" ] && ! printf '%s\n' "$ALIGNED" | grep -Eqv '^(1[4-9]|[2-9][0-9])$'; then
      good "the library is laid out for 16 KB memory pages"
    else
      bad "the library is not laid out for 16 KB memory pages (LOAD alignments: $(printf '%s\n' "$ALIGNED" | tr '\n' ' '))"
    fi
    # (Where an APK keeps the library uncompressed, its place in the file has to be
    # aligned as well; a compressed one is unpacked at installation.)
    if [ "$KIND" = apk ] && unzip -lv "$FILE" | grep ' lib/arm64-v8a/liboneum.so$' | grep -q ' Stored '; then
      "$TOOLS/zipalign" -c -P 16 4 "$FILE" >/dev/null 2>&1 \
        || bad "the uncompressed library is not aligned to 16 KB in the APK"
    fi
    if "$NDK_BIN/llvm-readelf" -S "$LIBRARY" | grep -q '\.debug_info'; then
      bad "the library carries debugging information (not a release build)"
    fi
    # The app that is given out does not listen to verification scripts: a build that
    # does has a name in it that says so (MARK in src/ui/automation.rs).
    if grep -a -q oneum-verification-channel "$LIBRARY"; then
      bad "the app was built with the verification channel (the automation feature)"
    else
      good "a release build of the library, without the verification channel"
    fi
  fi
  for inside in assets/LICENSE.txt assets/THIRD_PARTY_LICENSES.txt; do
    [ -s "$UNPACKED/$ROOT/$inside" ] || bad "$inside is not in it"
  done
  [ -n "$(find "$UNPACKED/$ROOT" -name 'classes*.dex' | head -1)" ] || bad "the Java part of the app (classes.dex) is not in it"

  # Nothing in it names a folder of this computer: every byte of every file is looked
  # at (Rust's source paths are renamed by scripts/release-paths.sh).
  FOUND=$(grep -rl -a -e "/Users/" -e "$HOME" "$UNPACKED" 2>/dev/null || true)
  if [ -z "$FOUND" ]; then
    good "no path of this computer in it"
  else
    bad "a path of this computer is in: $(printf '%s\n' "$FOUND" | sed "s|^$UNPACKED/||" | tr '\n' ' ')"
  fi
  rm -rf "$UNPACKED"
}

if [ "$MODE" = check ]; then
  WANT_VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
  for file in "$@"; do
    [ -f "$file" ] || { echo "there is no $file" >&2; exit 1; }
    check_file "$file" "$WANT_VERSION" "${ONEUM_ANDROID_BUILD:-}"
  done
  if [ "$PROBLEMS" -gt 0 ]; then
    echo "$PROBLEMS thing(s) wrong" >&2
    exit 1
  fi
  exit 0
fi

if [ "$MODE" = all ] && [ -z "$STORE" ]; then
  echo "name the key store to sign with: ONEUM_ANDROID_KEYSTORE=<file> (scripts/android-keystore.sh makes one)" >&2
  echo "(for an APK for your own phone, signed with the debug key: scripts/android-release.sh --direct)" >&2
  exit 1
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=${ONEUM_ANDROID_BUILD:-$(git rev-list --count HEAD)}
export ONEUM_ANDROID_BUILD="$BUILD"
DIRECT_NAME="OnEum-$VERSION-android-arm64"
PLAY_NAME="OnEum-$VERSION-$BUILD-play"
# What an earlier run left under these names is taken away first: whatever happens,
# no file of another build stays beside the new ones.
rm -f "dist/$DIRECT_NAME.apk" dist/SHA256SUMS-android.txt
[ "$MODE" = direct ] || rm -f "dist/play/$PLAY_NAME.aab" "dist/play/$PLAY_NAME.apk"

# What has to come with the app (these two files are made here, not kept in git).
ASSETS=android/app/src/main/assets
mkdir -p "$ASSETS" dist
cp LICENSE "$ASSETS/LICENSE.txt"
python3 scripts/gen-licenses.py --android "$ASSETS/THIRD_PARTY_LICENSES.txt" >/dev/null

built() { # built <what android-build.sh printed>: the file it names
  FILE=$(printf '%s\n' "$1" | tail -1)
  [ -f "$FILE" ] || { echo "what was built is not at $FILE" >&2; exit 1; }
  printf '%s\n' "$FILE"
}
echo "== building (version $VERSION, version code $BUILD)"
cp "$(built "$(scripts/android-build.sh --release)")" "dist/$DIRECT_NAME.apk"
if [ "$MODE" = all ]; then
  mkdir -p dist/play
  cp "$(built "$(scripts/android-build.sh --release --play --bundle)")" "dist/play/$PLAY_NAME.aab"
  cp "$(built "$(scripts/android-build.sh --release --play)")" "dist/play/$PLAY_NAME.apk"
fi

echo "== checking"
check_file "dist/$DIRECT_NAME.apk" "$VERSION" "$BUILD" direct
if [ "$MODE" = all ]; then
  check_file "dist/play/$PLAY_NAME.aab" "$VERSION" "$BUILD" play
  check_file "dist/play/$PLAY_NAME.apk" "$VERSION" "$BUILD" play
fi
if [ "$PROBLEMS" -gt 0 ]; then
  rm -f "dist/$DIRECT_NAME.apk"
  [ "$MODE" = direct ] || rm -f "dist/play/$PLAY_NAME.aab" "dist/play/$PLAY_NAME.apk"
  echo "$PROBLEMS thing(s) wrong: nothing is left in dist" >&2
  exit 1
fi

(cd dist && shasum -a 256 "$DIRECT_NAME.apk" > SHA256SUMS-android.txt)
echo "== built"
echo "dist/$DIRECT_NAME.apk (installed by hand)"
cat dist/SHA256SUMS-android.txt
if [ "$MODE" = all ]; then
  echo "dist/play/$PLAY_NAME.aab (for Google Play)"
  echo "dist/play/$PLAY_NAME.apk (the same build as an APK, to try on a phone)"
fi
