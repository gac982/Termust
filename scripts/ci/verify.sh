#!/bin/sh
set -eu

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

echo "== stage 0: repo hygiene =="
cargo test -p termust-conformance --test repo_hygiene

echo "== stage 1: fmt + clippy =="
cargo fmt --check
cargo clippy --all-targets -- -D warnings

echo "== stage 2: architecture =="
cargo test -p termust-conformance --test architecture

echo "== stage 3: app id =="
cargo test -p termust-conformance --test app_id

echo "== stage 4: workspace tests =="
cargo test --workspace

echo "verify: OK"
