#!/bin/sh
set -eu

ROOT="$(git rev-parse --show-toplevel)"
DEST="$ROOT/flatpak/build-src"

rm -rf "$DEST"
mkdir -p "$DEST"

# Copia solo archivos versionables: tracked + untracked no ignorados.
# doc/, assets/, target/ y build-src quedan fuera por .gitignore.
git -C "$ROOT" ls-files -z --cached --others --exclude-standard \
    | tar --null -T - -cf - \
    | tar -xf - -C "$DEST"

count=$(find "$DEST" -type f | wc -l)
echo "flatpak build-src: $count archivos en $DEST"
