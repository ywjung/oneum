#!/usr/bin/env python3
"""Writes THIRD_PARTY_NOTICES.md: every crate linked into the macOS build, the iPhone
build or the Android build (the document is shown in all three apps), with its license
expression and source.
Uses only `cargo metadata` (no extra tools).

    python3 scripts/gen-notices.py
    python3 scripts/gen-notices.py --windows <output file>

The second form lists the crates of the Windows (x86_64) build instead and writes them
to the given file (used by scripts/package-windows.sh).
"""
import json
import os
import subprocess
import sys

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
windows = len(sys.argv) == 3 and sys.argv[1] == "--windows"
if len(sys.argv) > 1 and not windows:
    sys.exit(__doc__)
platforms, build_name = (
    (["x86_64-pc-windows-gnu"], "Windows x86_64") if windows
    else (["aarch64-apple-darwin", "aarch64-apple-ios", "aarch64-linux-android"],
          "macOS arm64, iPhone and Android"))
env = dict(os.environ, PATH=os.path.expanduser("~/.cargo/bin") + os.pathsep + os.environ.get("PATH", ""))
packages, seen = {}, set()
for platform in platforms:
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", platform],
        cwd=root, env=env))
    packages.update({p["id"]: p for p in meta["packages"]})
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}

    # Walk normal (non-dev, non-build) dependencies from the app crate.
    start = meta["resolve"]["root"]
    linked, stack = set(), [start]
    while stack:
        pid = stack.pop()
        if pid in linked:
            continue
        linked.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    linked.discard(start)
    seen |= linked

rows = sorted(((packages[p]["name"].lower(), packages[p]["version"], packages[p]) for p in seen), key=lambda r: r[:2])
by_license = {}
for _, _, p in rows:
    by_license.setdefault(p.get("license") or "UNKNOWN", []).append(p["name"])

out = ["# Third-party notices", "",
       "OnEum is built from the open-source Rust crates listed below (as linked into the",
       f"{build_name} builds), and contains the data listed at the end. Each remains under its",
       "own license. The full license texts come with the app in the file",
       "`THIRD_PARTY_LICENSES.txt` (on a Mac inside the app, under Contents/Resources; on Windows",
       "next to the program).", "",
       f"Crates: {len(rows)}", "",
       "## License summary", ""]
for lic in sorted(by_license, key=lambda l: (-len(by_license[l]), l)):
    out.append(f"- `{lic}` — {len(by_license[lic])}")
out += ["",
        "Notes:",
        "- Symphonia (audio decoding) is licensed under MPL-2.0. OnEum uses it unmodified;",
        "  its source is available at the repository listed below.",
        "- No GPL/LGPL/AGPL-licensed crate is linked.",
        "",
        "## Crates", "", "| Crate | Version | License | Source |", "|---|---|---|---|"]
for _, _, p in rows:
    src = p.get("repository") or p.get("homepage") or f"https://crates.io/crates/{p['name']}"
    out.append(f"| {p['name']} | {p['version']} | {p.get('license') or 'UNKNOWN'} | {src} |")
# Data that is compiled into the program but is not a crate (see scripts/gen-licenses.py,
# which reproduces the license text from the same folder).
out += ["",
        "## Data", "",
        "| Data | License | Source |", "|---|---|---|",
        "| Built-in headphone correction profiles (`assets/corrections/headphones.txt`): "
        "parametric equalizer settings from AutoEq, computed from oratory1990's measurements "
        "| MIT, Copyright (c) 2018-2022 Jaakko Pasanen | https://github.com/jaakkopasanen/AutoEq |"]
out.append("")
path = sys.argv[2] if windows else os.path.join(root, "THIRD_PARTY_NOTICES.md")
with open(path, "w") as f:
    f.write("\n".join(out))
copyleft = [l for l in by_license if any(t in l for t in ("GPL", "AGPL")) and "MPL" not in l and " OR " not in l]
print(f"wrote {path}: {len(rows)} crates, {len(by_license)} license expressions")
if "UNKNOWN" in by_license or copyleft:
    print("review needed:", by_license.get("UNKNOWN"), copyleft, file=sys.stderr)
    sys.exit(1)
