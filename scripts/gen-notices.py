#!/usr/bin/env python3
"""Writes THIRD_PARTY_NOTICES.md: every crate linked into the macOS build, with its
license expression and source. Uses only `cargo metadata` (no extra tools).

    python3 scripts/gen-notices.py
"""
import json
import os
import subprocess
import sys

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
env = dict(os.environ, PATH=os.path.expanduser("~/.cargo/bin") + os.pathsep + os.environ.get("PATH", ""))
meta = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", "aarch64-apple-darwin"],
    cwd=root, env=env))
packages = {p["id"]: p for p in meta["packages"]}
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}

# Walk normal (non-dev, non-build) dependencies from the app crate.
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

rows = sorted(((packages[p]["name"].lower(), packages[p]["version"], packages[p]) for p in seen), key=lambda r: r[:2])
by_license = {}
for _, _, p in rows:
    by_license.setdefault(p.get("license") or "UNKNOWN", []).append(p["name"])

out = ["# Third-party notices", "",
       "OnEum is built from the open-source Rust crates listed below (as linked into the",
       "macOS arm64 build). Each crate remains under its own license; the license texts are",
       "distributed with the crates' sources at the listed locations.", "",
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
out.append("")
path = os.path.join(root, "THIRD_PARTY_NOTICES.md")
with open(path, "w") as f:
    f.write("\n".join(out))
copyleft = [l for l in by_license if any(t in l for t in ("GPL", "AGPL")) and "MPL" not in l and " OR " not in l]
print(f"wrote {path}: {len(rows)} crates, {len(by_license)} license expressions")
if "UNKNOWN" in by_license or copyleft:
    print("review needed:", by_license.get("UNKNOWN"), copyleft, file=sys.stderr)
    sys.exit(1)
