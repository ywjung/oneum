#!/usr/bin/env python3
"""Makes the pictures a package for the Microsoft Store carries, from the app icon.

    python3 scripts/gen-windows-store-logos.py

They go to assets/windows/store/Assets, under the names the package's manifest
(assets/windows/store/AppxManifest.xml.in) and Windows' resource index expect:

    StoreLogo                the picture of the package (the Store, the installer)
    Square150x150Logo        the Start menu's tile
    Square44x44Logo          the list of apps
      .targetsize-N                         the same at fixed sizes, on Windows' own plate
      .targetsize-N_altform-unplated        without a plate: the taskbar and Alt+Tab
      .targetsize-N_altform-lightunplated   the same when Windows' light theme is on

A name without `scale-` is the picture at 100 %; `.scale-200` is the one for screens
at 200 %. The pictures at fixed sizes are cut to the icon itself (the app icon has an
empty margin around it), so that it is as large on the taskbar as other programs'.
Without the unplated ones Windows draws the icon smaller, on a plate of its own. The
sizes are the ones Microsoft names as the least an app should have.

Needs Pillow (pip install pillow). The result is kept in the repository, so this is
run only when assets/icon-1024.png changes.
"""
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "assets" / "windows" / "store" / "Assets"
# (name, size at 100 %): each also at 200 %.
SCALED = [("StoreLogo", 50), ("Square150x150Logo", 150), ("Square44x44Logo", 44)]
# Fixed sizes of the small logo, each with a plate and without (dark and light theme).
TARGET_SIZES = [16, 24, 32, 48, 256]
FORMS = ["", "_altform-unplated", "_altform-lightunplated"]


def main() -> None:
    source = Image.open(ROOT / "assets" / "icon-1024.png").convert("RGBA")
    # The icon without its margin, as a square around its middle.
    left, top, right, bottom = source.getbbox()
    side = max(right - left, bottom - top)
    x = (left + right - side) // 2
    y = (top + bottom - side) // 2
    tight = source.crop((x, y, x + side, y + side))

    OUT.mkdir(parents=True, exist_ok=True)
    for old in OUT.glob("*.png"):
        old.unlink()
    written = []

    def save(picture: Image.Image, size: int, name: str) -> None:
        # Each size is scaled from the large picture on its own, with a filter that
        # keeps the small ones sharp (as scripts/gen-windows-icon.py does).
        picture.resize((size, size), Image.LANCZOS).save(OUT / name, format="PNG", optimize=True)
        written.append(name)

    for name, size in SCALED:
        save(source, size, f"{name}.png")
        save(source, size * 2, f"{name}.scale-200.png")
    for size in TARGET_SIZES:
        for form in FORMS:
            save(tight, size, f"Square44x44Logo.targetsize-{size}{form}.png")

    total = sum((OUT / name).stat().st_size for name in written)
    print(f"wrote {len(written)} pictures to {OUT.relative_to(ROOT)} ({total} bytes)")


if __name__ == "__main__":
    main()
