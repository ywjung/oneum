#!/bin/sh
# Signs a built OnEum.app for giving out on the Mac outside the App Store (Developer ID),
# through the Apple account that Xcode is signed in to, and has Apple notarize it.
#   scripts/mac-developer-id.sh <OnEum.app>              signed with Developer ID, in place
#   scripts/mac-developer-id.sh <OnEum.app> --notarize   signed, notarized, the ticket stapled
#   scripts/mac-developer-id.sh --check <OnEum.app> [--notarized]
#                                                        only the checks, of an app that is there
# scripts/package.sh calls this when ONEUM_MAC_TEAM is set (docs/RELEASE.md, section 4).
#
# What is needed (the Apple account in Xcode, ONEUM_MAC_TEAM, an unlocked screen) is told
# in scripts/mac-archive.sh.
#   ONEUM_NOTARY_WAIT how long Apple's answer is waited for, in seconds (default 1800).
set -eu
# (A pipe fails when the command in it does.)
set -o pipefail 2>/dev/null || true
cd "$(dirname "$0")/.."
. scripts/mac-archive.sh

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
  if has_verification_channel "$1"; then
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

require_team
if has_verification_channel "$APP"; then
  echo "an app with the verification channel is not signed for giving out" >&2
  exit 1
fi
require_unlocked_screen

WORK=target/xcode-mac
ARCHIVE=$WORK/OnEum.xcarchive
OPTIONS=$WORK/ExportOptions.plist
rm -rf "$WORK"
mkdir -p "$WORK"
make_archive "$APP" "$ARCHIVE"
if [ "$NOTARIZE" = 1 ]; then DESTINATION=upload; else DESTINATION=export; fi
write_export_options "$OPTIONS" developer-id "$DESTINATION"

# Signed anew with Developer ID (what the app was signed with before does not matter:
# the hardened runtime that package.sh switched on is kept). With "upload" the signed
# app is sent to Apple's notary service as well.
export_archive "$ARCHIVE" "$WORK/export" "$OPTIONS"
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
