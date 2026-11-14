#!/bin/sh
# Regenerates the app icon from the drawing in scripts/gen-icon.swift:
# assets/icon-1024.png (the drawing), assets/OnEum.icns (the bundle's icon),
# assets/icon-160.png (compiled into the program: window icon and Settings ▸ About) and
# the iPhone app's icon (ios/Assets.xcassets), which fills its square.
set -eu
cd "$(dirname "$0")/.."
swift scripts/gen-icon.swift assets/icon-1024.png
swift scripts/gen-icon.swift ios/Assets.xcassets/AppIcon.appiconset/icon-1024.png --ios
sips -z 160 160 assets/icon-1024.png --out assets/icon-160.png >/dev/null
SET=$(mktemp -d)/OnEum.iconset
mkdir -p "$SET"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/icon-1024.png --out "$SET/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z $d $d assets/icon-1024.png --out "$SET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$SET" -o assets/OnEum.icns
rm -rf "$(dirname "$SET")"
echo "wrote assets/OnEum.icns and assets/icon-160.png"
