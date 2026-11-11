#!/bin/sh
# Regenerates assets/OnEum.icns from the drawing in scripts/gen-icon.swift.
set -eu
cd "$(dirname "$0")/.."
swift scripts/gen-icon.swift assets/icon-1024.png
SET=$(mktemp -d)/OnEum.iconset
mkdir -p "$SET"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/icon-1024.png --out "$SET/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z $d $d assets/icon-1024.png --out "$SET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$SET" -o assets/OnEum.icns
rm -rf "$(dirname "$SET")"
echo "wrote assets/OnEum.icns"
