#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
echo "==> infrastructure"
docker compose ps
echo
echo "==> mock provider"
curl -s localhost:8081/stats || echo "(provider not running)"
echo
cargo run --quiet -p engine --bin inspect -- "$@"