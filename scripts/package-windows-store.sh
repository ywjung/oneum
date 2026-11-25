#!/bin/sh
# Makes the folder that becomes the package for the Microsoft Store (MSIX):
#   dist/OnEum-<version>-windows-x86_64-store/
#     AppxManifest.xml      (assets/windows/store/AppxManifest.xml.in, filled in)
#     OnEum.exe             (the program, taken out of the zip: the same file)
#     Assets/*.png          (assets/windows/store/Assets)
#     LICENSE.txt, THIRD_PARTY_NOTICES.md, THIRD_PARTY_LICENSES.txt
# Run scripts/package-windows.sh first: this takes the program from its zip, so the
# Store's package, the zip and the setup program carry one and the same OnEum.exe.
#
# The folder is packed on Windows, with the Windows SDK's tools:
# scripts/pack-msix.ps1 (the workflow .github/workflows/windows-package.yml does both).
# Nothing here is signed: the Store signs a package it has accepted.
# docs/MICROSOFT_STORE.md.
#
# Whose package it is:
#   ONEUM_STORE_NAME            Package/Identity/Name            } the three values
#   ONEUM_STORE_PUBLISHER       Package/Identity/Publisher       } Partner Center shows
#   ONEUM_STORE_PUBLISHER_NAME  Package/Properties/PublisherDisplayName } for the app
# All three, or none. With none it is a package for trying out, not for the Store: a
# name of its own, and the publisher with which Windows 11 installs a package that
# nobody signed (Add-AppxPackage -AllowUnsigned, from PowerShell as administrator).
#
#   ONEUM_STORE_VERSION         the package's version, four numbers. The Store wants
#                               the first above 0 and the last 0, so the program's
#                               0.1.0 cannot be it. Default: 1.0.<commits>.0, with the
#                               number of commits that the other packages use as
#                               their build number.
set -eu
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
NAME="OnEum-$VERSION-windows-x86_64"
ZIP="dist/$NAME.zip"
OUT="dist/$NAME-store"
# (What an earlier run left does not stay when this one fails.)
rm -rf "$OUT"
[ -f "$ZIP" ] || { echo "$ZIP is not there: run scripts/package-windows.sh first" >&2; exit 1; }

STORE_NAME=${ONEUM_STORE_NAME:-}
STORE_PUBLISHER=${ONEUM_STORE_PUBLISHER:-}
STORE_PUBLISHER_NAME=${ONEUM_STORE_PUBLISHER_NAME:-}
if [ -n "$STORE_NAME" ] && [ -n "$STORE_PUBLISHER" ] && [ -n "$STORE_PUBLISHER_NAME" ]; then
  KIND="for the Store"
elif [ -z "$STORE_NAME" ] && [ -z "$STORE_PUBLISHER" ] && [ -z "$STORE_PUBLISHER_NAME" ]; then
  KIND="for trying out (not for the Store)"
  STORE_NAME="OnEum.Test"
  # (The number is the one Microsoft gives for unsigned packages; Windows refuses to
  # register an unsigned package whose publisher lacks it.)
  STORE_PUBLISHER="CN=OnEum test package, OID.2.25.311729368913984317654407730594956997722=1"
  STORE_PUBLISHER_NAME="OnEum test package (not from the Store)"
else
  echo "set all three of ONEUM_STORE_NAME, ONEUM_STORE_PUBLISHER and ONEUM_STORE_PUBLISHER_NAME, or none" >&2
  exit 1
fi

PACKAGE_VERSION=${ONEUM_STORE_VERSION:-}
if [ -z "$PACKAGE_VERSION" ]; then
  # (A checkout without its history would count one commit.)
  if [ "$(git rev-parse --is-shallow-repository 2>/dev/null || echo true)" != "false" ]; then
    echo "the commits cannot be counted here (no git history): set ONEUM_STORE_VERSION" >&2
    exit 1
  fi
  PACKAGE_VERSION="1.0.$(git rev-list --count HEAD).0"
fi

DONE=""
trap '[ -n "$DONE" ] || rm -rf "$OUT"' EXIT
mkdir -p "$OUT"
unzip -q -j "$ZIP" "$NAME/OnEum.exe" "$NAME/LICENSE.txt" "$NAME/THIRD_PARTY_NOTICES.md" \
  "$NAME/THIRD_PARTY_LICENSES.txt" -d "$OUT"
cp -R assets/windows/store/Assets "$OUT/Assets"

# The manifest, with the four values in it; then the folder is looked at as a whole.
ONEUM_STORE_NAME="$STORE_NAME" ONEUM_STORE_PUBLISHER="$STORE_PUBLISHER" \
ONEUM_STORE_PUBLISHER_NAME="$STORE_PUBLISHER_NAME" ONEUM_STORE_VERSION="$PACKAGE_VERSION" \
python3 - "$OUT" <<'PY'
import os
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path
from xml.sax.saxutils import escape

out = Path(sys.argv[1])


def fail(message: str) -> None:
    sys.exit(f"package-windows-store: {message}")


name = os.environ["ONEUM_STORE_NAME"]
publisher = os.environ["ONEUM_STORE_PUBLISHER"]
publisher_name = os.environ["ONEUM_STORE_PUBLISHER_NAME"]
version = os.environ["ONEUM_STORE_VERSION"]

# What Windows accepts as the name of a package.
if not re.fullmatch(r"[A-Za-z0-9.-]{3,50}", name):
    fail("ONEUM_STORE_NAME has to be 3 to 50 letters, digits, periods and dashes")
if not re.match(r"[A-Za-z0-9.]+=", publisher):
    fail("ONEUM_STORE_PUBLISHER has to be a distinguished name (CN=...)")
if publisher_name != publisher_name.strip() or not 1 <= len(publisher_name) <= 256:
    fail("ONEUM_STORE_PUBLISHER_NAME has to be 1 to 256 characters, without spaces around it")
parts = version.split(".")
if len(parts) != 4 or not all(p.isdigit() and (p == "0" or p[0] != "0") and int(p) <= 65535 for p in parts):
    fail(f"the version {version} is not four numbers from 0 to 65535")
if int(parts[0]) == 0:
    fail(f"the version {version} begins with 0 (the Store does not take that)")
if int(parts[3]) != 0:
    fail(f"the version {version} does not end with 0 (the Store keeps the last number for itself)")

text = Path("assets/windows/store/AppxManifest.xml.in").read_text(encoding="utf-8")
quotes = {'"': "&quot;"}
values = {
    "@NAME@": escape(name, quotes),
    "@PUBLISHER@": escape(publisher, quotes),
    "@PUBLISHER_NAME@": escape(publisher_name, quotes),
    "@VERSION@": version,
}
for key, value in values.items():
    if text.count(key) != 1:
        fail(f"the manifest's template does not have {key} exactly once")
    text = text.replace(key, value)
left = re.findall(r"@[A-Z_]+@", text)
if left:
    fail(f"the manifest still has {left[0]}")
(out / "AppxManifest.xml").write_text(text, encoding="utf-8")

# It reads back as what was asked for, and every picture it names is in the folder.
ns = {
    "m": "http://schemas.microsoft.com/appx/manifest/foundation/windows10",
    "uap": "http://schemas.microsoft.com/appx/manifest/uap/windows10",
}
root = ET.parse(out / "AppxManifest.xml").getroot()
identity = root.find("m:Identity", ns)
got = (
    identity.get("Name"),
    identity.get("Publisher"),
    identity.get("Version"),
    root.findtext("m:Properties/m:PublisherDisplayName", namespaces=ns),
)
if got != (name, publisher, version, publisher_name):
    fail("the manifest does not read back with the values it was given")
application = root.find("m:Applications/m:Application", ns)
if not (out / application.get("Executable")).is_file():
    fail(f"the manifest starts {application.get('Executable')}, which is not in the folder")
visual = application.find("uap:VisualElements", ns)
pictures = [
    root.findtext("m:Properties/m:Logo", namespaces=ns),
    visual.get("Square150x150Logo"),
    visual.get("Square44x44Logo"),
]
for picture in pictures:
    if not picture or not (out / picture.replace("\\", "/")).is_file():
        fail(f"the manifest names the picture {picture}, which is not in the folder")

# Names the packing tool takes, and nothing that it writes itself.
for path in sorted(p for p in out.rglob("*")):
    relative = path.relative_to(out).as_posix()
    if not relative.isascii():
        fail(f"{relative}: a file name outside ASCII")
    if path.is_file() and path.name in ("resources.pri", "AppxBlockMap.xml", "AppxSignature.p7x", "[Content_Types].xml"):
        fail(f"{relative}: the packing tool writes this file itself")
    if path.name.startswith("."):
        fail(f"{relative}: a hidden file")
PY

# The program is the one in the zip, byte for byte.
[ "$(unzip -p "$ZIP" "$NAME/OnEum.exe" | shasum -a 256 | cut -d' ' -f1)" = "$(shasum -a 256 "$OUT/OnEum.exe" | cut -d' ' -f1)" ] \
  || { echo "OnEum.exe in $OUT is not the one in $ZIP" >&2; exit 1; }

DONE=1
echo "built $OUT ($KIND, package version $PACKAGE_VERSION, $(find "$OUT" -type f | wc -l | tr -d ' ') files)"
