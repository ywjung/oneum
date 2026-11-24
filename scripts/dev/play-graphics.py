#!/usr/bin/env python3
"""Draws what Google Play's store page shows beside the screenshots:

    <out>/feature-graphic-ko.png, feature-graphic-en.png   1024 x 500, no transparency
    <out>/icon-512.png                                     512 x 512

    python3 scripts/dev/play-graphics.py <out> <latin font> <Korean font>

The feature graphic is the app's mark as the icon has it (scripts/gen-icon.swift: a
whole note on the lines of a staff, on the app's violet), with the name and a line
about the app beside it. The fonts are given by the caller: scripts/dev/play-shots.sh
takes Roboto and Noto Sans CJK from the emulator, which are free to use. The icon is
the iPhone app's (which fills its square, as Play wants it: Play cuts the shape).
Needs Pillow.
"""
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

WIDTH, HEIGHT = 1024, 500
# (Drawn at twice the size and made smaller: smooth edges.)
SCALE = 2
LIGHT, DARK = (148, 140, 252), (61, 51, 173)
LINES = {
    "ko": "내 음악 파일을 있는 그대로",
    "en": "Your music files, as they are",
}


def plate(width: int, height: int) -> Image.Image:
    """The app's violet, lighter at the top left."""
    image = Image.new("RGB", (width, height))
    pixels = image.load()
    for y in range(height):
        for x in range(width):
            t = (x / width + y / height) / 2
            pixels[x, y] = tuple(round(a + (b - a) * t) for a, b in zip(LIGHT, DARK))
    return image


def mark(image: Image.Image, centre: tuple[int, int], width: int) -> None:
    """The staff's five lines, fading to the right, and the whole note on the middle one."""
    cx, cy = centre
    gap = round(width * 118 / 520)
    lines = Image.new("L", image.size, 0)
    draw = ImageDraw.Draw(lines)
    for step in range(-2, 3):
        y = cy + step * gap
        draw.line([(0, y), (image.width, y)], fill=66, width=round(width * 15 / 520))
    # (The lines end before the words begin.)
    fade = Image.new("L", image.size, 0)
    end = cx + width
    for x in range(image.width):
        level = 255 if x < cx else max(0, round(255 * (end - x) / (end - cx)))
        ImageDraw.Draw(fade).line([(x, 0), (x, image.height)], fill=level)
    lines = Image.composite(lines, Image.new("L", image.size, 0), fade)
    image.paste((255, 255, 255), mask=lines)

    # The note: a broad oval with a slanted hole, and a soft shadow under it.
    height = round(width * 352 / 520)
    note = Image.new("L", image.size, 0)
    ImageDraw.Draw(note).ellipse([cx - width // 2, cy - height // 2, cx + width // 2, cy + height // 2], fill=255)
    hole_width, hole_height = round(width * 318 / 520), round(width * 176 / 520)
    hole = Image.new("L", (hole_width * 2, hole_width * 2), 0)
    ImageDraw.Draw(hole).ellipse(
        [hole_width - hole_width // 2, hole_width - hole_height // 2,
         hole_width + hole_width // 2, hole_width + hole_height // 2], fill=255)
    hole = hole.rotate(36, resample=Image.BICUBIC)
    note.paste(0, (cx - hole_width, cy - hole_width), hole)
    shadow = Image.new("L", image.size, 0)
    shadow.paste(note, (0, round(width * 10 / 520)))
    shadow = shadow.filter(ImageFilter.GaussianBlur(width * 24 / 520)).point(lambda v: round(v * 0.45))
    image.paste((26, 20, 89), mask=shadow)
    image.paste((255, 255, 255), mask=note)


def feature(language: str, latin: Path, korean: Path) -> Image.Image:
    width, height = WIDTH * SCALE, HEIGHT * SCALE
    image = plate(width, height)
    mark(image, (round(width * 0.215), height // 2), round(width * 0.25))
    draw = ImageDraw.Draw(image)
    name = ImageFont.truetype(str(latin), round(height * 0.26))
    try:
        name.set_variation_by_name("Bold")
    except (OSError, ValueError):
        pass  # (a font of one weight)
    line = ImageFont.truetype(str(korean if language == "ko" else latin), round(height * 0.085))
    left = round(width * 0.43)
    draw.text((left, round(height * 0.44)), "OnEum", font=name, fill=(255, 255, 255), anchor="ls")
    draw.text((left + round(height * 0.012), round(height * 0.62)), LINES[language], font=line,
              fill=(236, 234, 255), anchor="ls")
    return image.resize((WIDTH, HEIGHT), Image.LANCZOS)


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    out, latin, korean = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
    out.mkdir(parents=True, exist_ok=True)
    for language in LINES:
        path = out / f"feature-graphic-{language}.png"
        feature(language, latin, korean).save(path)
        print(f"  {path.name}")
    root = Path(__file__).resolve().parents[2]
    icon = Image.open(root / "ios/Assets.xcassets/AppIcon.appiconset/icon-1024.png").convert("RGBA")
    icon.resize((512, 512), Image.LANCZOS).save(out / "icon-512.png")
    print("  icon-512.png")
    return 0


if __name__ == "__main__":
    sys.exit(main())
