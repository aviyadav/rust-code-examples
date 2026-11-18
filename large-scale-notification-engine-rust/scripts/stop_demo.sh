#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
for f in run/*.pid; do
  [[ -e "$f" ]] || continue
  pid="$(cat "$f")"
  name="$(basename "$f" .pid)"
  if kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    echo "stopped $name ($pid)"
  fi
  rm -f "$f"
done
sleep 1
for f in logs/*.log; do :; done
if [[ "${1:-}" == "--all" ]]; then
  docker compose down
  echo "infrastructure stopped (add -v to 'docker compose down' to wipe data)"
fi