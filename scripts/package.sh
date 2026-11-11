#!/bin/sh
# Builds dist/OnEum.app (and with --dmg, dist/OnEum-<version>-arm64.dmg).
#
#   scripts/package.sh              release build, ad-hoc signed app bundle
#   scripts/package.sh --debug      debug build (faster, for development)
#   scripts/package.sh --dmg        also create the disk image and checksums
#
# Signing: set ONEUM_SIGN_IDENTITY to a "Developer ID Application: …" identity to sign
# with the hardened runtime. Without it the bundle is ad-hoc signed (runs locally, is
# not notarizable). See docs/RELEASE.md.
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

PROFILE=release
MAKE_DMG=0
for arg in "$@"; do
  case "$arg" in
    --debug) PROFILE=debug ;;
    --dmg) MAKE_DMG=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)
ARCH=$(uname -m)
if [ "$PROFILE" = release ]; then
  cargo build --release --locked --bin oneum
else
  cargo build --locked --bin oneum
fi

APP=dist/OnEum.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/$PROFILE/oneum" "$APP/Contents/MacOS/OnEum"
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
for doc in docs/USER_GUIDE.md THIRD_PARTY_NOTICES.md LICENSE; do
  [ -f "$doc" ] && cp "$doc" "$APP/Contents/Resources/"
done
plutil -lint "$APP/Contents/Info.plist" >/dev/null

if [ -n "${ONEUM_SIGN_IDENTITY:-}" ]; then
  codesign --force --timestamp --options runtime --sign "$ONEUM_SIGN_IDENTITY" "$APP"
  echo "signed with: $ONEUM_SIGN_IDENTITY (hardened runtime)"
else
  codesign --force --sign - "$APP"
  echo "ad-hoc signed (no Developer ID identity given)"
fi
codesign --verify --strict "$APP"
echo "built $APP  (version $VERSION, build $BUILD, $ARCH, $PROFILE)"

if [ "$MAKE_DMG" = 1 ]; then
  DMG="dist/OnEum-$VERSION-$ARCH.dmg"
  STAGE=$(mktemp -d)
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
