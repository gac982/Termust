#!/usr/bin/env python3
"""Genera NOTICE con las licencias de las dependencias de cargo metadata."""
import json
import subprocess
from collections import defaultdict
from pathlib import Path

root = Path(__file__).resolve().parent.parent
meta = json.loads(
    subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=root,
        text=True,
    )
)
version = next(p["version"] for p in meta["packages"] if p["name"] == "termust-app")
groups = defaultdict(list)
for pkg in sorted(meta["packages"], key=lambda p: (p["name"], p["version"])):
    if pkg["source"] is None:
        continue
    groups[pkg.get("license") or "NOASSERTION"].append(f"{pkg['name']} {pkg['version']}")
print(f"Termust {version}")
print()
print("Copyright (C) 2026 Termust contributors")
print("Licensed under GPL-3.0-or-later. See LICENSE.")
print()
print("This product includes third-party software:")
print()
for license_expr in sorted(groups):
    print(f"[{license_expr}]")
    for item in groups[license_expr]:
        print(f"  {item}")
    print()
