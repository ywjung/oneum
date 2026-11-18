#!/usr/bin/env python3
"""Makes a small music library of invented artists and albums, for pictures of the app
(the homepage in site/, the guides): nothing in it is anybody's music.

    python3 scripts/dev/gen-demo-library.py <output-folder>
    python3 scripts/dev/gen-demo-library.py --samples assets/samples

With --samples: the sample music that comes with the iPhone app (a button on its empty
library offers it, for someone who has no music on the phone yet, such as App Review).
Three of the albums, three short tracks each, small files and covers: it is part of
the app's download.

Each album gets a drawn cover (cover.png) and a few tracks of synthetic sound: a chord
of sine tones that swells and fades, so that the VU meters and the spectrum have
something to show. A few tracks are FLAC (one album at 96 kHz / 24 bit, for the signal
path); the others are small MP3 files, since only their names and lengths are looked at.
Needs ffmpeg and Pillow.
"""
import math
import random
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

# (artist, album, year, genre, colours of the cover, format, tracks as (title, seconds))
ALBUMS = [
    ("달빛 사중주", "온음계", 2023, "Jazz", ("#1F2A6B", "#E9C46A", "#F4F1DE"), "flac",
     [("온음", 214), ("반음 아래", 187), ("다섯 번째 계단", 251), ("느린 왈츠", 203), ("푸른 방", 236)]),
    ("Nova Lane", "First Light", 2019, "Pop", ("#F28482", "#F6BD60", "#F7EDE2"), "mp3",
     [("First Light", 198), ("Second Wind", 221), ("Paper Kites", 176), ("Low Tide", 243),
      ("Morning Static", 209)]),
    ("한강 프로젝트", "강변의 밤", 2021, "가요", ("#0B3954", "#087E8B", "#BFD7EA"), "mp3",
     [("서울의 밤", 231), ("강변북로", 205), ("잠수교", 189), ("새벽 세 시", 262), ("첫차", 174)]),
    ("The Verbose Orchestra", "Late Night Sessions", 2020, "Electronic",
     ("#2D1E2F", "#E3170A", "#F7B32B"), "mp3",
     [("Midnight Drive", 247), ("Neon Rain", 219), ("Afterimage", 284), ("Slow Signal", 301)]),
    ("Mira Vale", "Cartography", 2018, "Folk", ("#606C38", "#DDA15E", "#FEFAE0"), "mp3",
     [("North Road", 192), ("Harbor Lights", 227), ("Quiet Maps", 168), ("Salt and Pine", 240)]),
    ("여름밤 라디오", "주파수", 2024, "Rock", ("#3A0CA3", "#F72585", "#4CC9F0"), "mp3",
     [("주파수", 201), ("안테나", 233), ("잡음 사이로", 215), ("한밤의 사연", 258)]),
    ("소리 연구소", "현을 위한 소품집", 2022, "Classical", ("#3D405B", "#81B29A", "#F2CC8F"), "mp3",
     [("아다지오", 318), ("스케르초", 164), ("간주곡", 207), ("론도", 276)]),
    ("Atlas Minor", "Deep Field", 2025, "Ambient", ("#001219", "#0A9396", "#E9D8A6"), "hires",
     [("Glass Hour", 263), ("Deep Field", 342)]),
]


def cover(path: Path, colours: tuple[str, str, str], seed: int, size: int = 900) -> None:
    """A square picture out of three colours: a soft ground and a few plain shapes."""
    rng = random.Random(seed)
    ground, shape, light = colours
    image = Image.new("RGB", (size, size), ground)
    draw = ImageDraw.Draw(image)
    kind = seed % 4
    if kind == 0:  # rings
        for step in range(7):
            radius = size * (0.46 - step * 0.06)
            centre = (size * 0.5, size * 0.56)
            box = [centre[0] - radius, centre[1] - radius, centre[0] + radius, centre[1] + radius]
            draw.ellipse(box, outline=shape if step % 2 else light, width=round(14 * size / 900))
    elif kind == 1:  # a sun over bands
        draw.ellipse([size * 0.25, size * 0.14, size * 0.75, size * 0.64], fill=light)
        for band in range(6):
            top = size * (0.52 + band * 0.08)
            draw.rectangle([0, top, size, top + size * 0.045], fill=shape)
    elif kind == 2:  # waves
        for line in range(9):
            points = []
            for x in range(0, size + 1, max(4, round(12 * size / 900))):
                y = size * (0.2 + line * 0.075) + math.sin(x / size * math.tau * 2 + line) * 34 * size / 900
                points.append((x, y))
            draw.line(points, fill=light if line % 3 else shape, width=round(10 * size / 900))
    else:  # scattered discs
        for _ in range(11):
            radius = rng.uniform(0.04, 0.16) * size
            x, y = rng.uniform(0.1, 0.9) * size, rng.uniform(0.1, 0.9) * size
            draw.ellipse([x - radius, y - radius, x + radius, y + radius],
                         fill=rng.choice([shape, light]))
    picture = image.filter(ImageFilter.GaussianBlur(0.6))
    if path.suffix == ".jpg":
        picture.save(path, quality=82, optimize=True)
    else:
        picture.save(path)


def sound(path: Path, seconds: int, seed: int, kind: str, tags: dict[str, str]) -> None:
    """A chord of sines whose loudness moves, encoded as `kind`."""
    rng = random.Random(seed)
    root = rng.choice([110.0, 130.81, 146.83, 164.81, 196.0])
    ratios = [1.0, 1.25, 1.5, 2.0, 3.0, 4.01]
    rate = 96000 if kind == "hires" else 44100
    # Left and right differ a little, so the two meters do not move as one.
    def channel(shift: float) -> str:
        parts = []
        for index, ratio in enumerate(ratios):
            level = 0.20 / (index + 1)
            wobble = 0.5 + 0.5 * math.sin(index)
            parts.append(
                f"{level:.4f}*sin(2*PI*{root * ratio:.2f}*t)"
                f"*(0.55+0.45*sin(2*PI*{0.21 + 0.13 * wobble:.3f}*t+{shift + index:.2f}))"
            )
        return "+".join(parts)
    source = f"aevalsrc={channel(0.0)}|{channel(1.3)}:s={rate}:d={seconds}"
    codec = {
        "flac": ["-c:a", "flac", "-sample_fmt", "s16", "-compression_level", "8"],
        "hires": ["-c:a", "flac", "-sample_fmt", "s32", "-bits_per_raw_sample", "24"],
        "mp3": ["-c:a", "libmp3lame", "-b:a", "64k"],
        "aac": ["-c:a", "aac", "-b:a", "64k"],
    }[kind]
    command = ["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", source, *codec]
    for key, value in tags.items():
        command += ["-metadata", f"{key}={value}"]
    subprocess.run([*command, str(path)], check=True)


# The sample music of the iPhone app: (number of the album above, kind, seconds per
# track). One album of each kind the reviewer may want to see in the signal path.
SAMPLES = [(0, "flac", 14), (1, "mp3", 30), (2, "aac", 30)]


def samples(out: Path) -> None:
    count = 0
    for number, kind, seconds in SAMPLES:
        artist, album, year, genre, colours, _, tracks = ALBUMS[number]
        folder = out / artist / album
        folder.mkdir(parents=True, exist_ok=True)
        cover(folder / "cover.jpg", colours, number, size=500)
        for index, (title, _) in enumerate(tracks[:3], start=1):
            extension = {"flac": "flac", "mp3": "mp3", "aac": "m4a"}[kind]
            tags = {
                "title": title, "artist": artist, "album": album, "album_artist": artist,
                "track": str(index), "date": str(year), "genre": genre,
            }
            sound(folder / f"{index:02d} {title}.{extension}", seconds, number * 31 + index,
                  kind, tags)
            count += 1
    print(f"wrote {count} sample tracks in {len(SAMPLES)} albums to {out}")


def main() -> None:
    if len(sys.argv) == 3 and sys.argv[1] == "--samples":
        samples(Path(sys.argv[2]))
        return
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = Path(sys.argv[1])
    count = 0
    for number, (artist, album, year, genre, colours, kind, tracks) in enumerate(ALBUMS):
        folder = out / artist / album
        folder.mkdir(parents=True, exist_ok=True)
        cover(folder / "cover.png", colours, number)
        for index, (title, seconds) in enumerate(tracks, start=1):
            extension = "mp3" if kind == "mp3" else "flac"
            tags = {
                "title": title, "artist": artist, "album": album, "album_artist": artist,
                "track": str(index), "date": str(year), "genre": genre,
            }
            sound(folder / f"{index:02d} {title}.{extension}", seconds, number * 31 + index,
                  kind, tags)
            count += 1
    print(f"wrote {count} tracks in {len(ALBUMS)} albums to {out}")


if __name__ == "__main__":
    main()
