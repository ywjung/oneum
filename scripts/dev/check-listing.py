#!/usr/bin/env python3
"""Checks the texts for the App Store (docs/APP_STORE_LISTING.md) against the limits of
App Store Connect, and against two rules of the review for such texts.

    python3 scripts/dev/check-listing.py [file]

A text is the fenced block that follows a line `<!-- field NAME chars=N -->` (or
`bytes=N`, which is how keywords are counted). Fails if a text is longer than its
limit, if a text names another operating system, or if keywords repeat or have
spaces after their commas (both waste the little room there is).
"""
import re
import sys
from pathlib import Path

FIELD = re.compile(r"<!-- field (\S+) (chars|bytes)=(\d+) -->\s*\n```text\n(.*?)\n```", re.S)
# Names an App Store text may not carry (other platforms than Apple's).
OTHER_SYSTEMS = re.compile(r"android|windows|google play|안드로이드|윈도우", re.I)


def main() -> int:
    root = Path(__file__).resolve().parents[2]
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else root / "docs" / "APP_STORE_LISTING.md"
    fields = FIELD.findall(path.read_text(encoding="utf-8"))
    if not fields:
        print(f"no texts found in {path}", file=sys.stderr)
        return 1
    wrong = 0
    for name, unit, limit, text in fields:
        size = len(text.encode("utf-8")) if unit == "bytes" else len(text)
        problems = []
        if size > int(limit):
            problems.append(f"{size - int(limit)} {unit} too long")
        if OTHER_SYSTEMS.search(text):
            problems.append("names another operating system")
        if name.endswith(".keywords"):
            words = text.split(",")
            if len({w.strip().casefold() for w in words}) != len(words):
                problems.append("a keyword is there twice")
            if any(w != w.strip() or not w for w in words):
                problems.append("spaces around a comma, or an empty keyword")
        wrong += bool(problems)
        mark = "wrong" if problems else "ok"
        print(f"  {mark}: {name}: {size} of {limit} {unit}" + "".join(f"; {p}" for p in problems))
    return 1 if wrong else 0


if __name__ == "__main__":
    sys.exit(main())
