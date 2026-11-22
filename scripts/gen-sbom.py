#!/usr/bin/env python3
"""Genera un SBOM CycloneDX 1.5 determinista desde cargo metadata."""
import hashlib
import json
import os
import subprocess
import sys
import uuid
from pathlib import Path

root = Path(__file__).resolve().parent.parent
meta = json.loads(
    subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=root,
        text=True,
    )
)
version = meta["packages"][0]["version"]
workspace = {pkg["id"] for pkg in meta["packages"] if pkg["source"] is None}
components = []
for pkg in sorted(meta["packages"], key=lambda p: (p["name"], p["version"])):
    if pkg["id"] in workspace and pkg["name"] != "termust-app":
        kind = "library"
    elif pkg["id"] in workspace:
        kind = "application"
    else:
        kind = "library"
    component = {
        "type": kind,
        "name": pkg["name"],
        "version": pkg["version"],
        "purl": f"pkg:cargo/{pkg['name']}@{pkg['version']}",
    }
    if pkg.get("license"):
        component["licenses"] = [{"expression": pkg["license"]}]
    if pkg.get("repository"):
        component["externalReferences"] = [
            {"type": "vcs", "url": pkg["repository"]}
        ]
    components.append(component)

payload = json.dumps(components, sort_keys=True, separators=(",", ":")).encode()
serial = uuid.UUID(bytes=hashlib.sha256(payload).digest()[:16], version=4)
epoch = int(os.environ.get("SOURCE_DATE_EPOCH", "1763856000"))
import datetime

timestamp = datetime.datetime.fromtimestamp(epoch, datetime.timezone.utc).strftime(
    "%Y-%m-%dT%H:%M:%SZ"
)
sbom = {
    "bomFormat": "CycloneDX",
    "specVersion": "1.5",
    "serialNumber": f"urn:uuid:{serial}",
    "version": 1,
    "metadata": {
        "timestamp": timestamp,
        "component": {
            "type": "application",
            "name": "termust",
            "version": version,
            "purl": f"pkg:cargo/termust@{version}",
            "licenses": [{"expression": "GPL-3.0-or-later"}],
        },
    },
    "components": components,
}
json.dump(sbom, sys.stdout, indent=2, sort_keys=True)
sys.stdout.write("\n")
