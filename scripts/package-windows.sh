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
# program is signed, and neither has been run on Windows by this script: see
# docs/TESTING.md ("Windows").
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
TARGET=x86_64-pc-windows-gnu
command -v x86_64-w64-mingw32-gcc >/dev/null || { echo "mingw-w64 is not installed (brew install mingw-w64)" >&2; exit 1; }

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release --locked --target "$TARGET" --bin oneum
TARGET_DIR=$(cargo metadata --format-version 1 --no-deps --locked \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
EXE="$TARGET_DIR/$TARGET/release/oneum.exe"
[ -f "$EXE" ] || { echo "built program not found at $EXE" >&2; exit 1; }

NAME="OnEum-$VERSION-windows-x86_64"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/$NAME"
cp "$EXE" "$STAGE/$NAME/OnEum.exe"
cp LICENSE "$STAGE/$NAME/LICENSE.txt"
python3 scripts/gen-notices.py --windows "$STAGE/$NAME/THIRD_PARTY_NOTICES.md" >/dev/null
python3 scripts/gen-licenses.py --windows "$STAGE/$NAME/THIRD_PARTY_LICENSES.txt" >/dev/null
cat > "$STAGE/$NAME/README.txt" <<TXT
OnEum $VERSION for Windows (64-bit)

Unzip the folder anywhere and start OnEum.exe. Nothing is installed; to remove the
program, delete the folder. The user guide is inside the program: Help > User Guide.

Your library and settings are kept in %APPDATA%\\OnEum, the image cache and the logs in
%LOCALAPPDATA%\\OnEum.

The program is not signed, so Windows SmartScreen may ask before it runs it.
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

mkdir -p dist
ZIP="dist/$NAME.zip"
rm -f "$ZIP"
(cd "$STAGE" && zip -q -r -X "$OLDPWD/$ZIP" "$NAME")
echo "built $ZIP"

# The setup program: the same files, installed for the user who runs it, with a Start
# menu shortcut and an entry in "Installed apps" (assets/windows/installer.nsi).
SETUP="dist/$NAME-setup.exe"
rm -f "$SETUP"
if command -v makensis >/dev/null; then
  # (With no locale set, makensis on a Mac ends with an allocation error while it makes
  # its language tables: it is given one.)
  LC_ALL=en_US.UTF-8 makensis -V2 -INPUTCHARSET UTF8 "-DVERSION=$VERSION" "-DSTAGE=$STAGE/$NAME" \
    "-DOUTFILE=$PWD/$SETUP" assets/windows/installer.nsi
  echo "built $SETUP"
  (cd dist && shasum -a 256 "$NAME.zip" "$NAME-setup.exe" > SHA256SUMS-windows.txt)
else
  echo "NSIS is not installed (brew install makensis): no setup program was made" >&2
  (cd dist && shasum -a 256 "$NAME.zip" > SHA256SUMS-windows.txt)
fi
cat dist/SHA256SUMS-windows.txt
