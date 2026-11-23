#!/bin/sh
set -eu

APP_ID="io.github.gac982.Termust"
SRC="${1:-assets/termust_icon.png}"
OUT="data/icons/hicolor"

if [ ! -f "$SRC" ]; then
    echo "falta $SRC" >&2
    exit 1
fi

for size in 512 256 128 64; do
    mkdir -p "$OUT/${size}x${size}/apps"
    magick "$SRC" -resize "${size}x${size}" -strip \
        "$OUT/${size}x${size}/apps/${APP_ID}.png"
done

echo "iconos generados en $OUT"
