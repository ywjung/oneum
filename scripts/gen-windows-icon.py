#!/usr/bin/env python3
"""Makes the icon of the Windows program (assets/windows/oneum.ico) from the app icon.

    python3 scripts/gen-windows-icon.py

One file with the sizes Windows asks for (the title bar and lists, the taskbar, the
desktop, large tiles). Needs Pillow (pip install pillow). The result is kept in the
repository, so this is run only when assets/icon-1024.png changes.
"""
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def main() -> None:
    source = Image.open(ROOT / "assets" / "icon-1024.png").convert("RGBA")
    # Each size is scaled from the large picture on its own (not from the next larger
    # one), with a filter that keeps the small ones sharp.
    frames = [source.resize((size, size), Image.LANCZOS) for size in SIZES]
    out = ROOT / "assets" / "windows" / "oneum.ico"
    frames[-1].save(out, format="ICO", append_images=frames[:-1], sizes=[(s, s) for s in SIZES])
    print(f"wrote {out.relative_to(ROOT)} ({out.stat().st_size} bytes, sizes {SIZES})")


if __name__ == "__main__":
    main()
