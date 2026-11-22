#!/bin/sh
# Builds dist/OnEum.app (and with --dmg, dist/OnEum-<version>-arm64.dmg).
#
#   scripts/package.sh              release build, ad-hoc signed app bundle
#   scripts/package.sh --debug      debug build (faster, for development)
#   scripts/package.sh --dmg        also create the disk image and checksums
#   scripts/package.sh --dmg --notarize
#                                   also have Apple notarize the app (needs ONEUM_MAC_TEAM)
#
# Signing, one of three ways (the hardened runtime is on in all of them, docs/RELEASE.md):
#   nothing set           ad-hoc signed: runs on this Mac, another Mac's Gatekeeper stops it
#   ONEUM_MAC_TEAM        the team's identifier: signed with Developer ID through the Apple
#                         account Xcode is signed in to (scripts/mac-developer-id.sh), and
#                         with --notarize notarized, with the ticket stapled to the app
#   ONEUM_SIGN_IDENTITY   a "Developer ID Application: …" identity of the keychain: signed
#                         with it, the disk image too (notarizing is then done by hand)
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

PROFILE=release
MAKE_DMG=0
NOTARIZE=0
for arg in "$@"; do
  case "$arg" in
    --debug) PROFILE=debug ;;
    --dmg) MAKE_DMG=1 ;;
    --notarize) NOTARIZE=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done
# Said before anything is built: what cannot be signed for giving out.
if [ -n "${ONEUM_MAC_TEAM:-}" ]; then
  [ -z "${ONEUM_SIGN_IDENTITY:-}" ] \
    || { echo "ONEUM_MAC_TEAM and ONEUM_SIGN_IDENTITY are two ways of signing: set one" >&2; exit 2; }
  [ "$PROFILE" = release ] && [ "${ONEUM_AUTOMATION:-0}" != 1 ] \
    || { echo "only a release build without the verification channel is signed with Developer ID" >&2; exit 2; }
elif [ "$NOTARIZE" = 1 ]; then
  echo "--notarize needs ONEUM_MAC_TEAM (the team whose Developer ID signs the app)" >&2
  exit 2
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)
ARCH=$(uname -m)
# The bundle is declared and verified for Apple silicon only (assets/Info.plist.in).
[ "$ARCH" = arm64 ] || { echo "only Apple silicon (arm64) builds are supported" >&2; exit 1; }
# The bundle declares macOS 14 as its minimum (assets/Info.plist.in): build for it.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"
# The scripted verification channel is compiled in only for development runs
# (scripts/dev/run.sh sets ONEUM_AUTOMATION=1), never into what is given to users.
FEATURES=""
[ "${ONEUM_AUTOMATION:-0}" = 1 ] && FEATURES="--features automation"
if [ "$PROFILE" = release ]; then
  # (No path of this computer in the program: see the file.)
  . scripts/release-paths.sh
  # shellcheck disable=SC2086
  cargo build --release --locked --bin oneum $FEATURES
else
  # shellcheck disable=SC2086
  cargo build --locked --bin oneum $FEATURES
fi
# Honour CARGO_TARGET_DIR and build.target-dir instead of assuming ./target.
TARGET_DIR=$(cargo metadata --format-version 1 --no-deps --locked \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
BIN="$TARGET_DIR/$PROFILE/oneum"
[ -x "$BIN" ] || { echo "built binary not found at $BIN" >&2; exit 1; }

# ONEUM_APP_DIR redirects the bundle (development runs use dist/dev so that an OnEum
# the user is running from dist/ is never replaced underneath them).
APP="${ONEUM_APP_DIR:-dist}/OnEum.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/OnEum"
cp assets/OnEum.icns "$APP/Contents/Resources/OnEum.icns"
sed -e "s/@VERSION@/$VERSION/" -e "s/@BUILD@/$BUILD/" assets/Info.plist.in > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"
# Localised permission prompts (the Info.plist carries the Korean text as default).
EN_DESC="Reads the music files in the folders you add in order to play them. Files are never changed."
mkdir -p "$APP/Contents/Resources/en.lproj" "$APP/Contents/Resources/ko.lproj"
: > "$APP/Contents/Resources/ko.lproj/InfoPlist.strings"
for key in NSDownloadsFolderUsageDescription NSDocumentsFolderUsageDescription NSDesktopFolderUsageDescription \
           NSRemovableVolumesUsageDescription NSNetworkVolumesUsageDescription; do
  printf '"%s" = "%s";\n' "$key" "$EN_DESC" >> "$APP/Contents/Resources/en.lproj/InfoPlist.strings"
done
# (The user guide is compiled into the app and shown in a window of its own.)
for doc in THIRD_PARTY_NOTICES.md LICENSE; do
  [ -f "$doc" ] && cp "$doc" "$APP/Contents/Resources/"
done
# The full license texts of everything linked in, gathered from the crates themselves.
python3 scripts/gen-licenses.py "$APP/Contents/Resources/THIRD_PARTY_LICENSES.txt" >/dev/null
plutil -lint "$APP/Contents/Info.plist" >/dev/null

if [ -n "${ONEUM_SIGN_IDENTITY:-}" ]; then
  codesign --force --timestamp --options runtime --sign "$ONEUM_SIGN_IDENTITY" "$APP"
  echo "signed with: $ONEUM_SIGN_IDENTITY (hardened runtime)"
else
  # Ad-hoc, but with the hardened runtime as well: the app needs none of what it
  # switches off (injected libraries, code made at run time, being debugged).
  codesign --force --options runtime --sign - "$APP"
  echo "ad-hoc signed with the hardened runtime (no Developer ID identity given)"
fi
if [ -n "${ONEUM_MAC_TEAM:-}" ]; then
  # Signed anew with Developer ID by Xcode (and notarized), with its own checks.
  if [ "$NOTARIZE" = 1 ]; then
    scripts/mac-developer-id.sh "$APP" --notarize
  else
    scripts/mac-developer-id.sh "$APP"
  fi
fi
codesign --verify --strict "$APP"
echo "built $APP  (version $VERSION, build $BUILD, $ARCH, $PROFILE)"

if [ "$MAKE_DMG" = 1 ]; then
  DMG="dist/OnEum-$VERSION-$ARCH.dmg"
  STAGE=$(mktemp -d)
  trap 'rm -rf "$STAGE"' EXIT
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  rm -f "$DMG"
  hdiutil create -quiet -volname "OnEum $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO "$DMG"
  rm -rf "$STAGE"
  if [ -n "${ONEUM_SIGN_IDENTITY:-}" ]; then
    codesign --force --timestamp --sign "$ONEUM_SIGN_IDENTITY" "$DMG"
  fi
  (cd dist && shasum -a 256 "$(basename "$DMG")" > SHA256SUMS.txt)
  echo "built $DMG"
  cat dist/SHA256SUMS.txt
fi
