#!/bin/sh
# Makes what goes to the Mac App Store out of a built OnEum.app that is in the App
# Sandbox (scripts/package.sh --sandbox makes one): the app signed for the store, in
# an installer package, checked.
#   scripts/mac-app-store.sh <OnEum.app>            dist/mas/OnEum-<version>-<build>.pkg
#   scripts/mac-app-store.sh <OnEum.app> --upload   the package, and the app sent to App
#                                                   Store Connect
#   scripts/mac-app-store.sh --check <.pkg>         only the checks, of a package that is there
# scripts/package.sh --app-store calls this (docs/APP_STORE.md, "Mac").
#
# What is needed (the Apple account in Xcode, ONEUM_MAC_TEAM, an unlocked screen) is told
# in scripts/mac-archive.sh. Xcode makes the certificates (Apple Distribution for the app,
# an installer's for the package) and the store's profile of the app itself. Sending
# needs the app to be there in App Store Connect with macOS as one of its systems.
set -eu
# (A pipe fails when the command in it does.)
set -o pipefail 2>/dev/null || true
cd "$(dirname "$0")/.."
. scripts/mac-archive.sh

usage() {
  echo "usage: mac-app-store.sh <OnEum.app> [--upload] | --check <.pkg>" >&2
  exit 2
}
MODE=export
APP=""
PKG=""
while [ $# -gt 0 ]; do
  case "$1" in
    --check) MODE=check ;;
    --upload) MODE=upload ;;
    *.app | *.app/) APP=${1%/} ;;
    *.pkg) PKG=$1 ;;
    *) usage ;;
  esac
  shift
done

# What a package for the Mac App Store has to be. $1 the package, $2 the team it has to
# be signed by (empty: any), $3 and $4 the version and build it has to carry (empty: any).
check() {
  FAILED=0
  bad() { echo "  FAIL  $1" >&2; FAILED=1; }
  good() { echo "  ok    $1"; }
  [ -f "$1" ] || { echo "there is no $1" >&2; exit 1; }
  if pkgutil --check-signature "$1" 2>/dev/null | grep -q -E '(3rd Party Mac Developer Installer|Mac Installer Distribution):'; then
    good "the package is signed with an installer certificate of the store"
  else
    bad "the package is not signed with an installer certificate of the store"
  fi
  OPENED=$(mktemp -d)
  # (pkgutil wants to make the folder itself.)
  rmdir "$OPENED"
  if ! pkgutil --expand-full "$1" "$OPENED" 2>/dev/null; then
    bad "the package cannot be opened"
    echo "the checks of $1 failed" >&2
    exit 1
  fi
  inner=$(find "$OPENED" -maxdepth 4 -name OnEum.app -type d | head -1)
  if [ -z "$inner" ]; then
    bad "there is no OnEum.app in the package"
    rm -rf "$OPENED"
    echo "the checks of $1 failed" >&2
    exit 1
  fi
  info=$(codesign -dvv "$inner" 2>&1) || true
  if codesign --verify --deep --strict "$inner" 2>/dev/null; then
    good "the signature is of what is in the app"
  else
    bad "the signature is not of what is in the app"
  fi
  if echo "$info" | grep -q -E '^Authority=(Apple Distribution|3rd Party Mac Developer Application)'; then
    good "the app is signed with a distribution certificate"
  else
    bad "the app is not signed with a distribution certificate"
  fi
  if [ -n "$2" ]; then
    if echo "$info" | grep -q "^TeamIdentifier=$2\$"; then
      good "signed by the team that was named"
    else
      bad "signed by another team than the one that was named"
    fi
  fi
  # What the signature allows the app: the sandbox and exactly what the repository's
  # file names, the identifiers the store adds, and nothing that lets it be debugged.
  told=$(mktemp)
  codesign -d --entitlements - --xml "$inner" 2>/dev/null >"$told" || true
  identifier=$(value "$inner/Contents/Info.plist" CFBundleIdentifier)
  if python3 - "$told" assets/OnEum.entitlements "$identifier" <<'PY'; then
import plistlib, sys
try:
    signed = plistlib.load(open(sys.argv[1], "rb"))
except Exception:
    sys.exit(1)
wanted = plistlib.load(open(sys.argv[2], "rb"))
security = {k: v for k, v in signed.items() if k.startswith("com.apple.security.")}
team = signed.get("com.apple.developer.team-identifier", "")
ok = (
    security == wanted
    and signed.get("com.apple.security.app-sandbox") is True
    and signed.get("com.apple.application-identifier") == f"{team}.{sys.argv[3]}"
    and not signed.get("com.apple.security.get-task-allow")
    and not signed.get("get-task-allow")
)
sys.exit(0 if ok else 1)
PY
    good "in the App Sandbox, allowed what assets/OnEum.entitlements names and no more"
  else
    bad "what the signature allows is not what assets/OnEum.entitlements names"
  fi
  rm -f "$told"
  # The store's profile of the app: for this app, for every Mac (a development one
  # names Macs).
  profile=$inner/Contents/embedded.provisionprofile
  if [ -f "$profile" ] && security cms -D -i "$profile" 2>/dev/null | python3 -c '
import plistlib, sys
p = plistlib.loads(sys.stdin.buffer.read())
app = p.get("Entitlements", {}).get("com.apple.application-identifier", "")
sys.exit(0 if app.endswith("." + sys.argv[1]) and "ProvisionedDevices" not in p else 1)' "$identifier"; then
    good "the store's profile of the app is in it"
  else
    bad "the store's profile of the app is not in it"
  fi
  if has_verification_channel "$inner"; then
    bad "the app was built with the verification channel (the automation feature)"
  else
    good "no verification channel in the app"
  fi
  version=$(value "$inner/Contents/Info.plist" CFBundleShortVersionString)
  build=$(value "$inner/Contents/Info.plist" CFBundleVersion)
  if { [ -z "$3" ] || [ "$version" = "$3" ]; } && { [ -z "$4" ] || [ "$build" = "$4" ]; }; then
    good "version $version, build $build"
  else
    bad "version $version, build $build: not $3 ($4)"
  fi
  # What App Store Connect asks of every Mac app's Info.plist.
  if value "$inner/Contents/Info.plist" LSApplicationCategoryType >/dev/null 2>&1 \
    && value "$inner/Contents/Info.plist" LSMinimumSystemVersion >/dev/null 2>&1 \
    && [ "$(value "$inner/Contents/Info.plist" ITSAppUsesNonExemptEncryption 2>/dev/null)" = false ]; then
    good "the category, the oldest macOS and the statement about encryption are named"
  else
    bad "the category, the oldest macOS or the statement about encryption is missing"
  fi
  if [ -f "$inner/Contents/Resources/PrivacyInfo.xcprivacy" ] && [ -f "$inner/Contents/Resources/LICENSE" ] \
    && [ -f "$inner/Contents/Resources/THIRD_PARTY_LICENSES.txt" ]; then
    good "the privacy manifest and the licenses are in the app"
  else
    bad "the privacy manifest or the licenses are missing"
  fi
  # No call into macOS that is not public API. App Review's automated check refuses a
  # program that only names one (2026-11-26, guideline 2.5.1, 0.1.0 build 174:
  # CGSSetWindowBackgroundBlurRadius, from winit; see vendor/winit/README-ONEUM.md).
  # The names with "CGS" before a capital letter are the window server's private ones;
  # the public CGShieldingWindowLevel is not among them.
  program=$inner/Contents/MacOS/$(value "$inner/Contents/Info.plist" CFBundleExecutable)
  private=$(nm -u "$program" 2>/dev/null | grep -E '^_CGS[A-Z]' || true)
  if [ -f "$program" ] && [ -z "$private" ]; then
    good "the program names no private call of the window server"
  else
    bad "the program names calls that are not public API: $(echo $private)"
  fi
  # No path of the computer that built it (scripts/release-paths.sh).
  if grep -r -l -a '/Users/' "$inner" >/dev/null 2>&1; then
    bad "a file of the app has a home path in it"
  else
    good "no home path in the app's files"
  fi
  rm -rf "$OPENED"
  [ "$FAILED" = 0 ] || { echo "the checks of $1 failed" >&2; exit 1; }
}

if [ "$MODE" = check ]; then
  [ -n "$PKG" ] || usage
  check "$PKG" "${ONEUM_MAC_TEAM:-}" "" ""
  exit 0
fi

[ -n "$APP" ] || usage
[ -d "$APP" ] || { echo "there is no $APP" >&2; exit 1; }
require_team
if has_verification_channel "$APP"; then
  echo "an app with the verification channel does not go to the store" >&2
  exit 1
fi
# Xcode keeps what the app is allowed as it finds it: an app that is not in the sandbox
# would come out as one the store refuses.
if ! codesign -d --entitlements - --xml "$APP" 2>/dev/null | grep -q 'com.apple.security.app-sandbox'; then
  echo "$APP is not in the App Sandbox: build it with scripts/package.sh --app-store" >&2
  exit 1
fi
require_unlocked_screen

VERSION=$(value "$APP/Contents/Info.plist" CFBundleShortVersionString)
BUILD=$(value "$APP/Contents/Info.plist" CFBundleVersion)
WORK=target/xcode-mac-store
ARCHIVE=$WORK/OnEum.xcarchive
rm -rf "$WORK"
mkdir -p "$WORK"
make_archive "$APP" "$ARCHIVE"

# Signed anew for the store (the app with a distribution certificate and the store's
# profile, what it is allowed kept as it was) and put into a signed installer package.
write_export_options "$WORK/ExportOptions.plist" app-store-connect export
export_archive "$ARCHIVE" "$WORK/export" "$WORK/ExportOptions.plist"
[ -f "$WORK/export/OnEum.pkg" ] || { echo "Xcode gave back no package" >&2; exit 1; }
OUT=$(dirname "$APP")
PKG=$OUT/OnEum-$VERSION-$BUILD.pkg
cp "$WORK/export/OnEum.pkg" "$PKG"
check "$PKG" "$TEAM" "$VERSION" "$BUILD"
echo "built $PKG"

if [ "$MODE" = upload ]; then
  # The same archive, signed and sent by Xcode (what was checked above is its twin).
  write_export_options "$WORK/UploadOptions.plist" app-store-connect upload
  export_archive "$ARCHIVE" "$WORK/upload" "$WORK/UploadOptions.plist"
  echo "Sent to App Store Connect as $VERSION ($BUILD). The build shows under TestFlight"
  echo "once Apple has processed it (some minutes; an e-mail tells of problems)."
fi
rm -rf "$WORK"
