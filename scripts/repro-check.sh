#!/bin/sh
set -eu
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
cargo build --release -p termust-app >/dev/null
first="$(sha256sum target/release/termust | awk '{print $1}')"
cargo clean -p termust-app >/dev/null
cargo build --release -p termust-app >/dev/null
second="$(sha256sum target/release/termust | awk '{print $1}')"
echo "repro: $first"
echo "repro: $second"
[ "$first" = "$second" ] && echo "repro: OK" || { echo "repro: FAIL"; exit 1; }
