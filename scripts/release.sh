#!/bin/sh
set -eu
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
VERSION="$(cargo metadata --locked --no-deps --format-version=1 | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["packages"][0]["version"])')"
APP_ID=io.github.termust.Termust
DIST="$ROOT/dist"
mkdir -p "$DIST"

./scripts/flatpak-src.sh
flatpak-builder --user --install --force-clean build-dir flatpak/$APP_ID.yml

python3 scripts/gen-sbom.py > "$DIST/termust-$VERSION.cdx.json"
python3 scripts/gen-notice.py > NOTICE

rm -rf "$DIST/repo"
if [ -n "${GPG_KEY:-}" ]; then
    flatpak build-export --gpg-sign="$GPG_KEY" "$DIST/repo" build-dir
    flatpak build-update-repo --gpg-sign="$GPG_KEY" "$DIST/repo"
else
    echo "AVISO: GPG_KEY no definido; el repo queda sin firmar" >&2
    flatpak build-export "$DIST/repo" build-dir
    flatpak build-update-repo "$DIST/repo"
fi
flatpak build-bundle "$DIST/repo" "$DIST/termust-$VERSION.flatpak" "$APP_ID"
( cd "$DIST" && sha256sum "termust-$VERSION.flatpak" "termust-$VERSION.cdx.json" > SHA256SUMS )
echo "release: $DIST/termust-$VERSION.flatpak"
