#!/bin/sh
# Cross-builds the Windows version on a Mac and packs it:
#   dist/OnEum-<version>-windows-x86_64.zip         (OnEum.exe, license, notices, read-me)
#   dist/OnEum-<version>-windows-x86_64-setup.exe   (the setup program; needs NSIS)
#
# Needs the Rust target and the mingw-w64 compiler, and NSIS for the setup program
# (without it only the zip is made):
#   rustup target add x86_64-pc-windows-gnu
#   brew install mingw-w64
#   brew install makensis
# The linker is set in .cargo/config.toml. The program's icon and version details are
# built in by build.rs (assets/windows/oneum.rc). Neither the program nor the setup
# program has been run on Windows by this script: see docs/TESTING.md ("Windows").
#
# Nothing is signed unless ONEUM_WIN_SIGN is set. With it (scripts/windows-sign.sh says
# what it and the other ONEUM_WIN_SIGN_* variables mean; needs osslsigncode), the
# program is signed before it goes into the zip and the setup program, the uninstaller
# is signed while the setup program is made, and the setup program is signed last; then
# the files in dist are checked, and the script fails if one of them is unsigned or its
# signature does not verify. docs/RELEASE.md ("Windows 코드 서명").
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
TARGET=x86_64-pc-windows-gnu
command -v x86_64-w64-mingw32-gcc >/dev/null || { echo "mingw-w64 is not installed (brew install mingw-w64)" >&2; exit 1; }

# Asked to sign: whatever that needs has to be there before the long build.
SIGN=""
if scripts/windows-sign.sh wanted; then
  scripts/windows-sign.sh ready
  SIGN="$PWD/scripts/windows-sign.sh"
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# (No path of this computer in the program: see the file.)
. scripts/release-paths.sh
cargo build --release --locked --target "$TARGET" --bin oneum
TARGET_DIR=$(cargo metadata --format-version 1 --no-deps --locked \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
EXE="$TARGET_DIR/$TARGET/release/oneum.exe"
[ -f "$EXE" ] || { echo "built program not found at $EXE" >&2; exit 1; }

NAME="OnEum-$VERSION-windows-x86_64"
ZIP="dist/$NAME.zip"
SETUP="dist/$NAME-setup.exe"
STAGE=$(mktemp -d)
# Asked to sign and not done: nothing stays in dist under these names, neither an
# unsigned file of this run nor what an earlier run left.
DONE=""
trap 'rm -rf "$STAGE"; [ -z "$SIGN" ] || [ -n "$DONE" ] || rm -f "$ZIP" "$SETUP" dist/SHA256SUMS-windows.txt' EXIT
mkdir -p "$STAGE/$NAME"
cp "$EXE" "$STAGE/$NAME/OnEum.exe"
cp LICENSE "$STAGE/$NAME/LICENSE.txt"
python3 scripts/gen-notices.py --windows "$STAGE/$NAME/THIRD_PARTY_NOTICES.md" >/dev/null
python3 scripts/gen-licenses.py --windows "$STAGE/$NAME/THIRD_PARTY_LICENSES.txt" >/dev/null
if [ -n "$SIGN" ]; then
  SMARTSCREEN="The program is signed. Windows SmartScreen may still ask before it runs a program
that not many people have downloaded yet."
else
  SMARTSCREEN="The program is not signed, so Windows SmartScreen may ask before it runs it."
fi
cat > "$STAGE/$NAME/README.txt" <<TXT
OnEum $VERSION for Windows (64-bit)

Unzip the folder anywhere and start OnEum.exe. Nothing is installed; to remove the
program, delete the folder. The user guide is inside the program: Help > User Guide.

Your library and settings are kept in %APPDATA%\\OnEum, the image cache and the logs in
%LOCALAPPDATA%\\OnEum.

$SMARTSCREEN
This build was made on a Mac and has not yet been checked on a real Windows machine.
TXT

# The icon is built in (File Explorer and the Start menu show it): a build without a
# resource compiler would only have warned.
x86_64-w64-mingw32-windres --input-format=coff --output-format=rc "$STAGE/$NAME/OnEum.exe" 2>/dev/null \
  | grep -q "RT_GROUP_ICON" || { echo "the program has no icon built in (see build.rs)" >&2; exit 1; }

# Only Windows' own libraries may be needed at run time (no compiler run-time DLLs).
FOREIGN=$(x86_64-w64-mingw32-objdump -p "$STAGE/$NAME/OnEum.exe" | sed -n 's/.*DLL Name: //p' \
  | grep -i -E 'libgcc|libwinpthread|libstdc' || true)
[ -z "$FOREIGN" ] || { echo "the program needs run-time DLLs that Windows does not have: $FOREIGN" >&2; exit 1; }

# Signed here, once: the zip and the setup program carry the same signed program.
[ -z "$SIGN" ] || "$SIGN" sign "$STAGE/$NAME/OnEum.exe"

mkdir -p dist
rm -f "$ZIP"
(cd "$STAGE" && zip -q -r -X "$OLDPWD/$ZIP" "$NAME")
echo "built $ZIP"

# The setup program: the same files, installed for the user who runs it, with a Start
# menu shortcut and an entry in "Installed apps" (assets/windows/installer.nsi).
rm -f "$SETUP"
if command -v makensis >/dev/null; then
  # (With no locale set, makensis on a Mac ends with an allocation error while it makes
  # its language tables: it is given one.)
  # (SIGN: the script signs the uninstaller with it before packing it.)
  LC_ALL=en_US.UTF-8 makensis -V2 -INPUTCHARSET UTF8 "-DVERSION=$VERSION" "-DSTAGE=$STAGE/$NAME" \
    "-DOUTFILE=$PWD/$SETUP" ${SIGN:+"-DSIGN=$SIGN"} assets/windows/installer.nsi
  [ -z "$SIGN" ] || "$SIGN" sign "$SETUP"
  echo "built $SETUP"
  (cd dist && shasum -a 256 "$NAME.zip" "$NAME-setup.exe" > SHA256SUMS-windows.txt)
else
  echo "NSIS is not installed (brew install makensis): no setup program was made" >&2
  (cd dist && shasum -a 256 "$NAME.zip" > SHA256SUMS-windows.txt)
fi

# Asked to sign: what is given out is looked at once more, as it lies in dist (the
# program as the zip holds it, and the setup program).
if [ -n "$SIGN" ]; then
  mkdir "$STAGE/check"
  unzip -q -j "$ZIP" "$NAME/OnEum.exe" -d "$STAGE/check"
  "$SIGN" verify "$STAGE/check/OnEum.exe"
  [ ! -f "$SETUP" ] || "$SIGN" verify "$SETUP"
fi
DONE=1
cat dist/SHA256SUMS-windows.txt
