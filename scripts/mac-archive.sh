# What the two scripts that have Xcode sign the Mac app share (scripts/mac-developer-id.sh,
# scripts/mac-app-store.sh). Sourced, not run.
#
# Both need membership of the Apple Developer Program and the Apple account signed in
# under Xcode ▸ Settings ▸ Accounts: Xcode then signs with certificates that it keeps at
# Apple (automatic signing). Nothing is put in the keychain, no password is asked for,
# and nothing of the account is kept in the repository. The screen has to be unlocked
# while they run.
#   ONEUM_MAC_TEAM    the team's identifier (ten characters). An account that belongs to
#                     several teams has to name its own here: the team that is named signs.

value() { /usr/libexec/PlistBuddy -c "Print $2" "$1"; }

# Sets TEAM from ONEUM_MAC_TEAM, or ends.
require_team() {
  TEAM=${ONEUM_MAC_TEAM:-}
  case "$TEAM" in
    [A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9]) ;;
    *) echo "ONEUM_MAC_TEAM has to be the team's identifier (ten characters)" >&2; exit 2 ;;
  esac
}

# Xcode reads the account from the keychain, and that is closed while the screen is
# locked. Never run then: the one run that was (2026-11-23) waited a quarter of an hour,
# ended with "No Accounts", and Xcode had lost the account: it had to be signed in anew.
# (Counted, not grep -q: that one ends before ioreg does, and the pipe would have failed.)
require_unlocked_screen() {
  if [ "$(ioreg -n Root -d1 | grep -c '"CGSSessionScreenIsLocked" *= *Yes')" != 0 ]; then
    echo "the screen is locked: Xcode cannot read the Apple account then. Unlock it and run again" >&2
    exit 1
  fi
}

# The app that is given out does not listen to verification scripts: a build that does
# has a name in it that says so (MARK in src/ui/automation.rs).
has_verification_channel() {
  grep -a -q oneum-verification-channel "$1/Contents/MacOS/$(value "$1/Contents/Info.plist" CFBundleExecutable)"
}

# Xcode signs what is in an archive. The app is not built by Xcode, so the archive is
# made here: the app as it is ($1), and the few things Xcode reads about it, at $2.
# Written where builds go (it names the team): not files of the repository.
make_archive() {
  rm -rf "$2"
  mkdir -p "$2/Products/Applications"
  ditto "$1" "$2/Products/Applications/OnEum.app"
  cat >"$2/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>ApplicationProperties</key>
  <dict>
    <key>ApplicationPath</key><string>Applications/OnEum.app</string>
    <key>Architectures</key><array><string>$(uname -m)</string></array>
    <key>CFBundleIdentifier</key><string>$(value "$1/Contents/Info.plist" CFBundleIdentifier)</string>
    <key>CFBundleShortVersionString</key><string>$(value "$1/Contents/Info.plist" CFBundleShortVersionString)</string>
    <key>CFBundleVersion</key><string>$(value "$1/Contents/Info.plist" CFBundleVersion)</string>
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
  plutil -lint "$2/Info.plist" >/dev/null
}

# What Xcode is to do with the archive, written to $1: $2 the method ("developer-id",
# "app-store-connect"), $3 "export" or "upload".
write_export_options() {
  cat >"$1" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>method</key><string>$2</string>
  <key>destination</key><string>$3</string>
  <key>teamID</key><string>$TEAM</string>
  <key>signingStyle</key><string>automatic</string>
  <key>manageAppVersionAndBuildNumber</key><false/>
</dict>
</plist>
PLIST
  plutil -lint "$1" >/dev/null
}

# Has Xcode do what the options at $3 say with the archive at $1, into $2.
export_archive() {
  xcodebuild -exportArchive -archivePath "$1" -exportPath "$2" \
    -exportOptionsPlist "$3" -allowProvisioningUpdates 2>&1 | grep -v ' Progress [0-9]*%' >&2
}
