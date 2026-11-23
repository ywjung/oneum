#!/bin/sh
# Signs a built OnEum.app for giving out on the Mac outside the App Store (Developer ID),
# through the Apple account that Xcode is signed in to, and has Apple notarize it.
#   scripts/mac-developer-id.sh <OnEum.app>              signed with Developer ID, in place
#   scripts/mac-developer-id.sh <OnEum.app> --notarize   signed, notarized, the ticket stapled
#   scripts/mac-developer-id.sh --check <OnEum.app> [--notarized]
#                                                        only the checks, of an app that is there
# scripts/package.sh calls this when ONEUM_MAC_TEAM is set (docs/RELEASE.md, section 4).
#
# Needed: membership of the Apple Developer Program, and the Apple account signed in under
# Xcode ▸ Settings ▸ Accounts. Xcode then signs with a Developer ID certificate that it
# keeps at Apple (automatic signing): nothing is put in the keychain, no password is
# asked for, and nothing of the account is kept in the repository. The screen has to be
# unlocked while it runs.
#   ONEUM_MAC_TEAM    the team's identifier (ten characters). An account that belongs to
#                     several teams has to name its own here: the team that is named signs.
#   ONEUM_NOTARY_WAIT how long Apple's answer is waited for, in seconds (default 1800).
set -eu
# (A pipe fails when the command in it does.)
set -o pipefail 2>/dev/null || true
cd "$(dirname "$0")/.."

usage() {
  echo "usage: mac-developer-id.sh <OnEum.app> [--notarize] | --check <OnEum.app> [--notarized]" >&2
  exit 2
}
MODE=sign
APP=""
NOTARIZE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --check) MODE=check ;;
    --notarize | --notarized) NOTARIZE=1 ;;
    *.app | *.app/) APP=${1%/} ;;
    *) usage ;;
  esac
  shift
done
[ -n "$APP" ] || usage
[ -d "$APP" ] || { echo "there is no $APP" >&2; exit 1; }

value() { /usr/libexec/PlistBuddy -c "Print $2" "$1"; }

# What an app that is given out has to be. $1 the app, $2 the team it has to be signed by
# (empty: any), $3 whether it has to be notarized (1) or not.
check() {
  FAILED=0
  bad() { echo "  FAIL  $1" >&2; FAILED=1; }
  good() { echo "  ok    $1"; }
  info=$(codesign -dvv "$1" 2>&1) || true
  if codesign --verify --deep --strict "$1" 2>/dev/null; then
    good "the signature is of what is in the app"
  else
    bad "the signature is not of what is in the app"
  fi
  if echo "$info" | grep -q '^Authority=Developer ID Application'; then
    good "signed with a Developer ID certificate"
  else
    bad "not signed with a Developer ID certificate"
  fi
  if [ -n "$2" ]; then
    if echo "$info" | grep -q "^TeamIdentifier=$2\$"; then
      good "signed by the team that was named"
    else
      bad "signed by another team than the one that was named"
    fi
  fi
  # (Apple notarizes nothing without these two.)
  if echo "$info" | grep -q 'flags=.*runtime'; then
    good "the hardened runtime is on"
  else
    bad "the hardened runtime is off"
  fi
  if echo "$info" | grep -q '^Timestamp='; then
    good "the signature has a time of its own"
  else
    bad "the signature has no time of its own"
  fi
  # The app that is given out does not listen to verification scripts: a build that does
  # has a name in it that says so (MARK in src/ui/automation.rs).
  if grep -a -q oneum-verification-channel "$1/Contents/MacOS/$(value "$1/Contents/Info.plist" CFBundleExecutable)"; then
    bad "the app was built with the verification channel (the automation feature)"
  else
    good "no verification channel in the app"
  fi
  if [ "$3" = 1 ]; then
    if xcrun stapler validate -q "$1" >/dev/null 2>&1; then
      good "Apple's ticket is stapled to the app"
    else
      bad "no ticket of Apple's is stapled to the app"
    fi
    # (What a Mac that downloaded the app asks before it lets it start.)
    if spctl -a -t exec -vv "$1" 2>&1 | grep -q '^source=Notarized Developer ID'; then
      good "Gatekeeper takes it as a notarized app"
    else
      bad "Gatekeeper does not take it as a notarized app"
    fi
  fi
  [ "$FAILED" = 0 ] || { echo "the checks of $1 failed" >&2; exit 1; }
}

if [ "$MODE" = check ]; then
  check "$APP" "${ONEUM_MAC_TEAM:-}" "$NOTARIZE"
  exit 0
fi

TEAM=${ONEUM_MAC_TEAM:-}
case "$TEAM" in
  [A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9]) ;;
  *) echo "ONEUM_MAC_TEAM has to be the team's identifier (ten characters)" >&2; exit 2 ;;
esac
EXECUTABLE=$APP/Contents/MacOS/$(value "$APP/Contents/Info.plist" CFBundleExecutable)
if grep -a -q oneum-verification-channel "$EXECUTABLE"; then
  echo "an app with the verification channel is not signed for giving out" >&2
  exit 1
fi

# Xcode reads the account from the keychain, and that is closed while the screen is
# locked. Never run then: the one run that was (2026-11-23) waited a quarter of an hour,
# ended with "No Accounts", and Xcode had lost the account: it had to be signed in anew.
# (Counted, not grep -q: that one ends before ioreg does, and the pipe would have failed.)
if [ "$(ioreg -n Root -d1 | grep -c '"CGSSessionScreenIsLocked" *= *Yes')" != 0 ]; then
  echo "the screen is locked: Xcode cannot read the Apple account then. Unlock it and run again" >&2
  exit 1
fi

# Xcode signs what is in an archive. The app is not built by Xcode, so the archive is
# made here: the app as it is, and the few things Xcode reads about it. Written where
# builds go (it names the team): not files of the repository.
WORK=target/xcode-mac
ARCHIVE=$WORK/OnEum.xcarchive
OPTIONS=$WORK/ExportOptions.plist
rm -rf "$WORK"
mkdir -p "$ARCHIVE/Products/Applications"
ditto "$APP" "$ARCHIVE/Products/Applications/OnEum.app"
INFO=$APP/Contents/Info.plist
cat >"$ARCHIVE/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>ApplicationProperties</key>
  <dict>
    <key>ApplicationPath</key><string>Applications/OnEum.app</string>
    <key>Architectures</key><array><string>$(uname -m)</string></array>
    <key>CFBundleIdentifier</key><string>$(value "$INFO" CFBundleIdentifier)</string>
    <key>CFBundleShortVersionString</key><string>$(value "$INFO" CFBundleShortVersionString)</string>
    <key>CFBundleVersion</key><string>$(value "$INFO" CFBundleVersion)</string>
    <key>SigningIdentity</key><string>-</string>
    <key>Team</key><string>$TEAM</string>
  </dict>
  <key>ArchiveVersion</key><integer>2</integer>
  <key>CreationDate</key><date>$(date -u +%Y-%m-%dT%H:%M:%SZ)</date>
  <key>Name</key><string>OnEum</string>
  <key>SchemeName</key><string>OnEum</string>
</dict>
</plist>
PLIST
if [ "$NOTARIZE" = 1 ]; then DESTINATION=upload; else DESTINATION=export; fi
cat >"$OPTIONS" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>method</key><string>developer-id</string>
  <key>destination</key><string>$DESTINATION</string>
  <key>teamID</key><string>$TEAM</string>
  <key>signingStyle</key><string>automatic</string>
</dict>
</plist>
PLIST
plutil -lint "$ARCHIVE/Info.plist" "$OPTIONS" >/dev/null

# Signed anew with Developer ID (what the app was signed with before does not matter:
# the hardened runtime that package.sh switched on is kept). With "upload" the signed
# app is sent to Apple's notary service as well.
xcodebuild -exportArchive -archivePath "$ARCHIVE" -exportPath "$WORK/export" \
  -exportOptionsPlist "$OPTIONS" -allowProvisioningUpdates 2>&1 | grep -v ' Progress [0-9]*%' >&2
if [ "$NOTARIZE" = 1 ]; then
  # Apple's answer takes from seconds to some minutes. Asked for until it is there: the
  # app that comes back is the signed one with the ticket stapled to it.
  WAIT=${ONEUM_NOTARY_WAIT:-1800}
  WAITED=0
  echo "sent to Apple's notary service; waiting for the answer (up to $WAIT s)" >&2
  while :; do
    rm -rf "$WORK/notarized"
    if ANSWER=$(xcodebuild -exportNotarizedApp -archivePath "$ARCHIVE" -exportPath "$WORK/notarized" 2>&1); then
      break
    fi
    if [ "$WAITED" -ge "$WAIT" ]; then
      echo "$ANSWER" >&2
      echo "no notarized app after $WAITED s (the last answer is above)" >&2
      exit 1
    fi
    sleep 20
    WAITED=$((WAITED + 20))
  done
  SIGNED=$WORK/notarized/OnEum.app
else
  SIGNED=$WORK/export/OnEum.app
fi
[ -d "$SIGNED" ] || { echo "Xcode gave back no app at $SIGNED" >&2; exit 1; }

check "$SIGNED" "$TEAM" "$NOTARIZE"
# Only the signature (and the ticket) may be new: the files are those that were given.
for file in Info.plist Resources/LICENSE Resources/THIRD_PARTY_LICENSES.txt; do
  if [ -f "$APP/Contents/$file" ] && ! cmp -s "$APP/Contents/$file" "$SIGNED/Contents/$file"; then
    echo "$file of the signed app is not the one that was given" >&2
    exit 1
  fi
done
rm -rf "$APP"
ditto "$SIGNED" "$APP"
rm -rf "$WORK"
if [ "$NOTARIZE" = 1 ]; then
  echo "signed with Developer ID and notarized: $APP"
else
  echo "signed with Developer ID (not notarized): $APP"
fi
