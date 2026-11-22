#!/bin/sh
set -eu
ROOT="$(git rev-parse --show-toplevel)"
for lang in es en; do
    mkdir -p "$ROOT/data/locale/$lang/LC_MESSAGES"
    msgfmt -o "$ROOT/data/locale/$lang/LC_MESSAGES/termust.mo" "$ROOT/data/i18n/$lang.po"
done
echo "i18n: catálogos compilados en data/locale"
