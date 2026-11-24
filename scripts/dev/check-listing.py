#!/usr/bin/env python3
"""Checks the texts for the stores against the limits of their consoles, and against
the rules their reviews have for such texts:

    docs/APP_STORE_LISTING.md    App Store Connect (the iPhone app and the Mac app)
    docs/PLAY_STORE_LISTING.md   Google Play Console (the Android app)

    python3 scripts/dev/check-listing.py [file…]     (no file: both)

A text is the fenced block that follows a line `<!-- field NAME chars=N -->` (or
`bytes=N`, which is how the App Store counts keywords). Fails if a text is longer than
its limit, if a text names another operating system or another store than its own, or
if keywords repeat or have spaces after their commas (both waste the little room
there is). For Google Play, whose rules for names and short descriptions forbid
them, also if a title or a short description praises or prices the app ("best",
"#1", "free", …).
"""
import re
import sys
from pathlib import Path

FIELD = re.compile(r"<!-- field (\S+) (chars|bytes)=(\d+) -->\s*\n```text\n(.*?)\n```", re.S)
# Names a store's text may not carry: the other systems and the other stores.
NOT_APPLE = re.compile(r"android|windows|google play|안드로이드|윈도우", re.I)
NOT_GOOGLE = re.compile(
    r"iphone|ipad|\bios\b|macos|\bmac\b|app store|apple|testflight|windows"
    r"|아이폰|아이패드|앱 ?스토어|애플|윈도우",
    re.I,
)
# What Google Play does not take in an app's name, and asks to leave out of its short
# description: praise, rank, price and calls to act.
PRAISE = re.compile(
    r"\bbest\b|#\s?1|\btop\b|\bfree\b|\bnew\b|\bsale\b|\bdiscount\b|download now|install now"
    r"|최고|1위|무료|신규|할인|지금 (다운로드|설치)",
    re.I,
)


def check(path: Path) -> int:
    """Prints a line for each text of the file; returns how many are wrong."""
    play = path.name.startswith("PLAY_")
    others = NOT_GOOGLE if play else NOT_APPLE
    fields = FIELD.findall(path.read_text(encoding="utf-8"))
    if not fields:
        print(f"no texts found in {path}", file=sys.stderr)
        return 1
    print(path.name)
    wrong = 0
    for name, unit, limit, text in fields:
        size = len(text.encode("utf-8")) if unit == "bytes" else len(text)
        problems = []
        if size > int(limit):
            problems.append(f"{size - int(limit)} {unit} too long")
        if others.search(text):
            problems.append("names another operating system or store")
        if play and name.endswith((".title", ".short")) and PRAISE.search(text):
            problems.append("praises or prices the app")
        if name.endswith(".keywords"):
            words = text.split(",")
            if len({w.strip().casefold() for w in words}) != len(words):
                problems.append("a keyword is there twice")
            if any(w != w.strip() or not w for w in words):
                problems.append("spaces around a comma, or an empty keyword")
        wrong += bool(problems)
        mark = "wrong" if problems else "ok"
        print(f"  {mark}: {name}: {size} of {limit} {unit}" + "".join(f"; {p}" for p in problems))
    return wrong


def main() -> int:
    docs = Path(__file__).resolve().parents[2] / "docs"
    paths = [Path(a) for a in sys.argv[1:]] or [
        docs / "APP_STORE_LISTING.md",
        docs / "PLAY_STORE_LISTING.md",
    ]
    return 1 if sum(check(path) for path in paths) else 0


if __name__ == "__main__":
    sys.exit(main())
