#!/usr/bin/env python3
"""Verifica que todos los msgid usados por tr()/tr_args() estén en es.po."""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PO = ROOT / "data/i18n/es.po"
SOURCES = [ROOT / "app/src"] + sorted((ROOT / "crates").glob("*/src"))
CALL = re.compile(
    r'(?<![A-Za-z0-9_])(?:i18n::|termust_i18n::)?(?:tr|tr_args)\(\s*"((?:[^"\\]|\\.)*)"',
    re.S,
)
ACCENTS = "áéíóúñÁÉÍÓÚÑ¿¡"


def parse_po(path: Path) -> set[str]:
    text = path.read_text()
    msgids: set[str] = set()
    current = None
    for line in text.splitlines():
        if line.startswith("msgid "):
            current = json.loads(line[6:])
        elif line.startswith("msgstr "):
            if current:
                msgids.add(current)
            current = None
    return msgids


def main() -> int:
    msgids = parse_po(PO)
    used: dict[str, list[str]] = {}
    for root in SOURCES:
        for path in root.rglob("*.rs"):
            text = path.read_text()
            for match in CALL.finditer(text):
                try:
                    msgid = json.loads('"' + match.group(1) + '"')
                except json.JSONDecodeError:
                    continue
                used.setdefault(msgid, []).append(str(path.relative_to(ROOT)))

    missing = sorted(set(used) - msgids)
    if missing:
        print("Faltan traducciones en data/i18n/es.po:", file=sys.stderr)
        for msgid in missing:
            print(f"  {msgid!r}  ({', '.join(used[msgid][:3])})", file=sys.stderr)
        return 1

    bad = sorted(
        msgid
        for msgid in msgids
        if msgid != "Español" and any(ch in ACCENTS for ch in msgid)
    )
    if bad:
        print("msgid con acentos españoles:", file=sys.stderr)
        for msgid in bad:
            print(f"  {msgid!r}", file=sys.stderr)
        return 1

    print(f"i18n: {len(used)} msgids usados, todos presentes en es.po")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
