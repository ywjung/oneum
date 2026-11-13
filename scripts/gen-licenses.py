#!/usr/bin/env python3
"""Writes the full license texts of every crate linked into the app, gathered from the
crates' own packages (the LICENSE/COPYING/NOTICE files they ship). Uses only
`cargo metadata` and the sources cargo has already fetched: nothing is downloaded.

    python3 scripts/gen-licenses.py <output file>             # macOS arm64 build
    python3 scripts/gen-licenses.py --windows <output file>   # Windows x86_64 build

Identical texts are written once, with the list of crates they apply to. Crates that
ship no license file are listed at the end with their license expression and source.
The script fails if it finds none at all for a license that has to be reproduced.
"""
import hashlib
import json
import os
import subprocess
import sys

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
args = sys.argv[1:]
windows = bool(args) and args[0] == "--windows"
if windows:
    args = args[1:]
if len(args) != 1:
    sys.exit(__doc__)
out_path = args[0]
platform, build_name = (
    ("x86_64-pc-windows-gnu", "Windows x86_64") if windows else ("aarch64-apple-darwin", "macOS arm64"))
env = dict(os.environ, PATH=os.path.expanduser("~/.cargo/bin") + os.pathsep + os.environ.get("PATH", ""))
meta = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", platform],
    cwd=root, env=env))
packages = {p["id"]: p for p in meta["packages"]}
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}

# Walk normal (non-dev, non-build) dependencies from the app crate, as gen-notices.py does.
start = meta["resolve"]["root"]
seen, stack = set(), [start]
while stack:
    pid = stack.pop()
    if pid in seen:
        continue
    seen.add(pid)
    for dep in nodes[pid]["deps"]:
        if any(k["kind"] is None for k in dep["dep_kinds"]):
            stack.append(dep["pkg"])
seen.discard(start)

PREFIXES = ("license", "licence", "copying", "unlicense", "notice", "copyright")
MAX_BYTES = 200_000  # no license text is larger; anything bigger is something else


def license_files(package):
    folder = os.path.dirname(package["manifest_path"])
    found = []
    for name in sorted(os.listdir(folder)):
        path = os.path.join(folder, name)
        if name.lower().startswith(PREFIXES) and os.path.isfile(path) and os.path.getsize(path) <= MAX_BYTES:
            found.append(path)
    explicit = package.get("license_file")
    if explicit:
        path = os.path.join(folder, explicit)
        if os.path.isfile(path) and path not in found:
            found.append(path)
    return found


texts = {}    # digest -> (text, file name, [crate labels])
without = []  # crates that ship no license file
rows = sorted((packages[p] for p in seen), key=lambda p: (p["name"].lower(), p["version"]))
for package in rows:
    label = f"{package['name']} {package['version']}"
    files = license_files(package)
    if not files:
        without.append(package)
        continue
    for path in files:
        with open(path, "rb") as f:
            text = f.read().decode("utf-8", errors="replace").replace("\r\n", "\n").strip()
        if not text:
            continue
        # The same text with different line wrapping or trailing spaces is the same text.
        digest = hashlib.sha256(" ".join(text.split()).encode()).hexdigest()
        entry = texts.setdefault(digest, (text, os.path.basename(path), []))
        if label not in entry[2]:
            entry[2].append(label)

rule = "=" * 78
out = [
    "OnEum: license texts of third-party software",
    "",
    f"OnEum ({build_name} build) contains the {len(rows)} open-source Rust crates listed in",
    "THIRD_PARTY_NOTICES.md. Their license texts follow, as shipped in the crates' own",
    "packages. A text that several crates share word for word appears once, with the",
    "crates it applies to. A crate offered under a choice of licenses (\"MIT OR",
    "Apache-2.0\") is used under either; all the texts it ships are reproduced.",
    "",
]
ordered = sorted(texts.values(), key=lambda e: (-len(e[2]), e[2][0].lower()))
for number, (text, name, crates) in enumerate(ordered, 1):
    out += [rule, f"Text {number} of {len(ordered)} ({name})", "Applies to: " + ", ".join(crates), rule, "", text, "", ""]
if without:
    out += [rule, "Crates that ship no license file in their package", rule, ""]
    for package in without:
        src = package.get("repository") or package.get("homepage") or f"https://crates.io/crates/{package['name']}"
        out.append(f"- {package['name']} {package['version']}: {package.get('license') or 'UNKNOWN'} ({src})")
    out += ["", "Their licenses are the ones named; the standard texts of those licenses are",
            "among the texts above."]
with open(out_path, "w") as f:
    f.write("\n".join(out) + "\n")

# Every license named by a crate without a file must have its standard text above,
# recognised by a line that every copy of that license contains.
MARKERS = {
    "MIT": "Permission is hereby granted, free of charge",
    "Apache-2.0": "Apache License",
    "MPL-2.0": "Mozilla Public License",
    "BSL-1.0": "Boost Software License",
    "Zlib": "This software is provided 'as-is'",
    "BSD-2-Clause": "Redistribution and use in source and binary forms",
    "BSD-3-Clause": "Redistribution and use in source and binary forms",
    "ISC": "Permission to use, copy, modify, and/or distribute this software",
    "Unlicense": "This is free and unencumbered software",
    "0BSD": "Permission to use, copy, modify, and/or distribute this software",
    "CC0-1.0": "CC0",
    "Unicode-3.0": "UNICODE LICENSE",
    "OFL-1.1": "SIL OPEN FONT LICENSE",
    "Ubuntu-font-1.0": "UBUNTU FONT LICENCE",
}
everything = "\n".join(e[0] for e in ordered)
problems = []
for package in without:
    expression = (package.get("license") or "UNKNOWN").replace("/", " OR ")
    # One alternative of an "OR" is enough; every part of an "AND" is needed.
    alternatives = [a.strip(" ()") for a in expression.split(" OR ")]
    def covered(alternative):
        return all(MARKERS.get(part.strip(" ()")) and MARKERS[part.strip(" ()")] in everything
                   for part in alternative.split(" AND "))
    if not any(covered(a) for a in alternatives):
        problems.append(f"{package['name']} {package['version']} ({expression})")
size = os.path.getsize(out_path)
print(f"wrote {out_path}: {len(ordered)} distinct texts for {len(rows)} crates, "
      f"{len(without)} crates without a license file, {size // 1024} KiB")
if problems:
    print("no license text available for:", "; ".join(problems), file=sys.stderr)
    sys.exit(1)
