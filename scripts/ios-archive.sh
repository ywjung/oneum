#!/bin/sh
# Makes what goes to the App Store: an archive of the iPhone app with its widgets,
# checked, and the .ipa that App Store Connect takes (docs/APP_STORE.md).
#   scripts/ios-archive.sh --unsigned      an archive without a signature and its checks:
#                                          needs no Apple account, cannot be uploaded
#   scripts/ios-archive.sh                 a signed archive and dist/ios/OnEum-<version>-<build>.ipa
#   scripts/ios-archive.sh --upload        a signed archive, sent to App Store Connect
#   scripts/ios-archive.sh --upload <.xcarchive>
#                                          a signed archive that is there (the one an
#                                          earlier run made and checked), sent as it is
#   scripts/ios-archive.sh --check <path>  only the checks, of an .xcarchive or an .ipa
#                                          that is there (one that Xcode made, say)
#
# Needed for the signed ones: membership of the Apple Developer Program, and the Apple
# account signed in under Xcode ▸ Settings ▸ Accounts. Xcode then makes the
# certificates and the profiles itself (automatic signing). Nothing of the account is
# kept in the repository: the team comes from the environment.
#   ONEUM_IOS_TEAM       the team's identifier (ten characters, developer.apple.com ▸
#                        Membership details). Required for the signed ones.
#   ONEUM_IOS_BUNDLE_ID  the app's identifier (default app.oneum.OnEum). The widgets get
#                        <identifier>.widget, and the group of keychain items the two
#                        share is <team's prefix>.<identifier>.nowplaying.
#   ONEUM_IOS_BUILD      the build number (default: the number of commits). App Store
#                        Connect takes each number once for a version: a second upload
#                        of the same commit needs a higher one here.
set -eu
# (A pipe fails when the build in it does.)
set -o pipefail 2>/dev/null || true
cd "$(dirname "$0")/.."

usage() {
  echo "usage: ios-archive.sh [--unsigned | --upload [<.xcarchive>] | --check <.xcarchive or .ipa>]" >&2
  exit 2
}
MODE=export
CHECK_PATH=""
GIVEN=""
while [ $# -gt 0 ]; do
  case "$1" in
    --unsigned) MODE=unsigned ;;
    --upload)
      MODE=upload
      case "${2:-}" in
        *.xcarchive)
          GIVEN=$2
          shift
          ;;
      esac
      ;;
    --check)
      MODE=check
      CHECK_PATH=${2:-}
      [ -n "$CHECK_PATH" ] || usage
      shift
      ;;
    *) usage ;;
  esac
  shift
done

PROBLEMS=0
bad() {
  echo "  wrong: $*" >&2
  PROBLEMS=$((PROBLEMS + 1))
}
good() { echo "  ok: $*"; }
# value <plist> <key path>: what the plist has there, or nothing.
value() { /usr/libexec/PlistBuddy -c "Print :$2" "$1" 2>/dev/null || true; }
# entitlement <bundle> <key path>: what the bundle was signed with there, or nothing.
entitlement() {
  ENT_FILE=$(mktemp)
  codesign -d --xml --entitlements - "$1" >"$ENT_FILE" 2>/dev/null || true
  value "$ENT_FILE" "$2"
  rm -f "$ENT_FILE"
}

# check_app <OnEum.app> <archive|ipa> [version] [build]
# Looks at the app as it would be given to Apple. An archive may be unsigned (the
# checks of the signature are then left out and that is said); an .ipa may not.
check_app() {
  APP=$1
  KIND=$2
  WANT_VERSION=${3:-}
  WANT_BUILD=${4:-}
  WIDGET=$APP/PlugIns/OnEumWidget.appex
  if [ ! -f "$APP/Info.plist" ] || [ ! -f "$WIDGET/Info.plist" ]; then
    bad "the app or its widget extension is not where it should be ($APP)"
    return
  fi
  ID=$(value "$APP/Info.plist" CFBundleIdentifier)
  WIDGET_ID=$(value "$WIDGET/Info.plist" CFBundleIdentifier)
  # An extension's identifier has to begin with its app's.
  if [ "$WIDGET_ID" = "$ID.widget" ]; then
    good "identifiers $ID and $WIDGET_ID"
  else
    bad "the widgets' identifier is '$WIDGET_ID', not '$ID.widget'"
  fi

  # The same version and build number in both: App Store Connect refuses an
  # extension that differs from its app.
  VERSION=$(value "$APP/Info.plist" CFBundleShortVersionString)
  BUILD=$(value "$APP/Info.plist" CFBundleVersion)
  WIDGET_VERSION=$(value "$WIDGET/Info.plist" CFBundleShortVersionString)
  WIDGET_BUILD=$(value "$WIDGET/Info.plist" CFBundleVersion)
  if [ "$VERSION" = "$WIDGET_VERSION" ] && [ "$BUILD" = "$WIDGET_BUILD" ]; then
    good "version $VERSION, build $BUILD, in the app and in the widgets"
  else
    bad "the app is $VERSION ($BUILD) and the widgets are $WIDGET_VERSION ($WIDGET_BUILD)"
  fi
  # (Up to three whole numbers with periods between them, both of them.)
  for number in "$VERSION" "$BUILD"; do
    if ! printf '%s\n' "$number" | grep -E '^[0-9]+(\.[0-9]+){0,2}$' >/dev/null; then
      bad "'$number' is not a version App Store Connect takes (1, 1.2 or 1.2.3)"
    fi
  done
  if [ -n "$WANT_VERSION" ] && [ "$VERSION" != "$WANT_VERSION" ]; then
    bad "the version is $VERSION, not $WANT_VERSION as in Cargo.toml"
  fi
  if [ -n "$WANT_BUILD" ] && [ "$BUILD" != "$WANT_BUILD" ]; then
    bad "the build number is $BUILD, not $WANT_BUILD"
  fi

  # The group of keychain items the app leaves the song in for its widgets: both are
  # told its name by the build, and it has to be the same name.
  SHARED=$(value "$APP/Info.plist" OneumSharedKeychain)
  WIDGET_SHARED=$(value "$WIDGET/Info.plist" OneumSharedKeychain)
  case "$SHARED" in
    *"$ID.nowplaying") ;;
    *) bad "the shared keychain group is '$SHARED', which does not end with $ID.nowplaying" ;;
  esac
  [ "$SHARED" = "$WIDGET_SHARED" ] \
    || bad "the widgets are told another keychain group ('$WIDGET_SHARED') than the app"

  # What a review asks for, and what the app shows a reviewer who has no music.
  for file in PrivacyInfo.xcprivacy ko.lproj/InfoPlist.strings en.lproj/InfoPlist.strings Assets.car; do
    [ -f "$APP/$file" ] || bad "$file is not in the app"
  done
  if [ -z "$(find "$APP/samples" -type f 2>/dev/null | head -1)" ]; then
    bad "the sample music (samples) is not in the app"
  fi
  [ "$(value "$APP/Info.plist" CFBundleIcons:CFBundlePrimaryIcon:CFBundleIconName)" = AppIcon ] \
    || bad "the app names no icon (CFBundleIconName)"
  [ -n "$(value "$APP/Info.plist" NSLocalNetworkUsageDescription)" ] \
    || bad "the app does not say why it uses the local network"
  # (Whichever the answer, it has to be there: without it every upload stops at a
  # question about encryption.)
  [ -n "$(value "$APP/Info.plist" ITSAppUsesNonExemptEncryption)" ] \
    || bad "the app does not say whether it uses encryption that has to be declared"

  for bundle in "$APP" "$WIDGET"; do
    EXECUTABLE=$bundle/$(value "$bundle/Info.plist" CFBundleExecutable)
    if [ ! -f "$EXECUTABLE" ]; then
      bad "no program in $(basename "$bundle")"
    elif [ "$(lipo -archs "$EXECUTABLE" 2>/dev/null)" != arm64 ]; then
      bad "$(basename "$EXECUTABLE") is not built for arm64 alone"
    fi
  done
  # The app that is given out does not listen to verification scripts: a build that
  # does has a name in it that says so (MARK in src/ui/automation.rs).
  if grep -a -q oneum-verification-channel "$APP/$(value "$APP/Info.plist" CFBundleExecutable)" 2>/dev/null; then
    bad "the app was built with the verification channel (the automation feature)"
  else
    good "no verification channel in the app"
  fi

  if codesign -d "$APP" >/dev/null 2>&1; then
    for bundle in "$WIDGET" "$APP"; do
      codesign --verify --strict "$bundle" 2>/dev/null \
        || bad "the signature of $(basename "$bundle") is not of what is in it"
    done
    # Both have to be allowed the group they were told of, by the signature.
    for bundle in "$APP" "$WIDGET"; do
      GROUPS_SIGNED=$(entitlement "$bundle" keychain-access-groups)
      if printf '%s\n' "$GROUPS_SIGNED" | grep -Fx "    $SHARED" >/dev/null; then
        good "$(basename "$bundle") may reach the keychain group $SHARED"
      else
        bad "$(basename "$bundle") is not signed for the keychain group '$SHARED'"
      fi
    done
    if [ "$KIND" = ipa ]; then
      for bundle in "$APP" "$WIDGET"; do
        [ -f "$bundle/embedded.mobileprovision" ] \
          || bad "$(basename "$bundle") has no provisioning profile in it"
        # (A build that a debugger may attach to is a development build.)
        [ "$(entitlement "$bundle" get-task-allow)" != true ] \
          || bad "$(basename "$bundle") is signed for development, not for distribution"
      done
    fi
  elif [ "$KIND" = ipa ]; then
    bad "the app is not signed"
  else
    echo "  (not signed: the checks of the signature are left out, and this archive cannot be uploaded)"
  fi
}

# check_no_paths <folder>: nothing in it names a folder of this computer. (Rust's
# source paths are renamed by scripts/release-paths.sh, and the archive's stripping
# takes those of the Swift and C compilers out of the widgets.) Every byte of every
# file is looked at: `strings -a` reads a program's sections only, and the paths of a
# program that was not stripped are in its symbol table, outside them.
check_no_paths() {
  FOUND=$(grep -rl -a -e "/Users/" -e "$HOME" "$1" 2>/dev/null || true)
  if [ -z "$FOUND" ]; then
    good "no path of this computer in it"
  else
    bad "a path of this computer is in: $(printf '%s\n' "$FOUND" | sed "s|^$1/||" | tr '\n' ' ')"
  fi
}

# check_path <.xcarchive or .ipa> [version] [build]
check_path() {
  echo "checking $1"
  case "$1" in
    *.xcarchive)
      check_app "$1/Products/Applications/OnEum.app" archive "${2:-}" "${3:-}"
      check_no_paths "$1"
      ;;
    *.ipa)
      UNPACKED=$(mktemp -d)
      if unzip -q "$1" -d "$UNPACKED"; then
        check_app "$UNPACKED/Payload/OnEum.app" ipa "${2:-}" "${3:-}"
        check_no_paths "$UNPACKED"
      else
        bad "$1 cannot be unpacked"
      fi
      rm -rf "$UNPACKED"
      ;;
    *) usage ;;
  esac
  if [ "$PROBLEMS" -gt 0 ]; then
    echo "$PROBLEMS thing(s) wrong with $1" >&2
    exit 1
  fi
}

if [ "$MODE" = check ]; then
  [ -e "$CHECK_PATH" ] || { echo "there is no $CHECK_PATH" >&2; exit 1; }
  check_path "$CHECK_PATH"
  exit 0
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=${ONEUM_IOS_BUILD:-$(git rev-list --count HEAD 2>/dev/null || echo 1)}
BUNDLE_ID=${ONEUM_IOS_BUNDLE_ID:-app.oneum.OnEum}
TEAM=${ONEUM_IOS_TEAM:-}
if [ "$MODE" != unsigned ] && [ -z "$TEAM" ]; then
  echo "Set ONEUM_IOS_TEAM to the identifier of your Apple Developer Program team" >&2
  echo "(developer.apple.com ▸ Account ▸ Membership details), or use --unsigned." >&2
  exit 1
fi
# What is uploaded should be what a commit says: the build number is counted from them.
if [ -z "$GIVEN" ] && [ -n "$(git status --porcelain 2>/dev/null)" ]; then
  echo "warning: there are changes that are not committed; the build number $BUILD is of the last commit" >&2
fi

archive() { # archive <path> [more settings for xcodebuild…]
  ARCHIVE_PATH=$1
  shift
  rm -rf "$ARCHIVE_PATH"
  xcodebuild -project ios/OnEum.xcodeproj -scheme OnEum -configuration Release \
    -destination 'generic/platform=iOS' -derivedDataPath target/xcode \
    -archivePath "$ARCHIVE_PATH" \
    ONEUM_BUNDLE_ID="$BUNDLE_ID" MARKETING_VERSION="$VERSION" CURRENT_PROJECT_VERSION="$BUILD" \
    "$@" archive
}

if [ "$MODE" = unsigned ]; then
  ARCHIVE=target/xcode/OnEum-unsigned.xcarchive
  archive "$ARCHIVE" CODE_SIGNING_ALLOWED=NO
  check_path "$ARCHIVE" "$VERSION" "$BUILD"
  echo "$ARCHIVE"
  exit 0
fi

if [ -n "$GIVEN" ]; then
  # An archive that is there is sent as it is: its version and build number are its
  # own (those of the commit it was made from), not those of the checkout now.
  ARCHIVE=$GIVEN
  [ -d "$ARCHIVE" ] || { echo "there is no $ARCHIVE" >&2; exit 1; }
  VERSION=$(value "$ARCHIVE/Products/Applications/OnEum.app/Info.plist" CFBundleShortVersionString)
  BUILD=$(value "$ARCHIVE/Products/Applications/OnEum.app/Info.plist" CFBundleVersion)
  check_path "$ARCHIVE" "$VERSION" "$BUILD"
else
  # (The first signed archive of a new identifier can stop at "Build input file cannot
  # be found: …mobileprovision": Xcode has made the profile and not yet fetched it.
  # A second run finds it.)
  ARCHIVE=target/xcode/OnEum.xcarchive
  archive "$ARCHIVE" -allowProvisioningUpdates DEVELOPMENT_TEAM="$TEAM"
  check_path "$ARCHIVE" "$VERSION" "$BUILD"
fi

# How the archive is signed anew for the App Store and what is done with it. Written
# where builds go (it names the team): not a file of the repository.
OPTIONS=target/xcode/ExportOptions.plist
if [ "$MODE" = upload ]; then DESTINATION=upload; else DESTINATION=export; fi
cat >"$OPTIONS" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>method</key><string>app-store-connect</string>
  <key>destination</key><string>$DESTINATION</string>
  <key>teamID</key><string>$TEAM</string>
  <key>signingStyle</key><string>automatic</string>
  <key>manageAppVersionAndBuildNumber</key><false/>
  <key>uploadSymbols</key><true/>
</dict>
</plist>
EOF
OUT=dist/ios
rm -rf "$OUT/export"
mkdir -p "$OUT"
xcodebuild -exportArchive -archivePath "$ARCHIVE" -exportPath "$OUT/export" \
  -exportOptionsPlist "$OPTIONS" -allowProvisioningUpdates
if [ "$MODE" = upload ]; then
  echo "Sent to App Store Connect as $VERSION ($BUILD). The build shows under TestFlight"
  echo "once Apple has processed it (some minutes; an e-mail tells of problems)."
  exit 0
fi
IPA=$OUT/OnEum-$VERSION-$BUILD.ipa
mv "$OUT/export/OnEum.ipa" "$IPA"
check_path "$IPA" "$VERSION" "$BUILD"
echo "$IPA"
