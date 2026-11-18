#!/usr/bin/env bash
# Exercises the article §4 failure matrix against the running demo.
set -euo pipefail
cd "$(dirname "$0")/.."
P=localhost:8081

case "${1:-help}" in
  latency-on)   curl -s -XPOST "$P/control" -H 'content-type: application/json' \
                  -d '{"latency_ms":2800,"latency_jitter_ms":600}'; echo ;;
  errors-on)    curl -s -XPOST "$P/control" -H 'content-type: application/json' \
                  -d '{"transient_rate":0.6}'; echo ;;
  permanent-on) curl -s -XPOST "$P/control" -H 'content-type: application/json' \
                  -d '{"permanent_rate":0.2}'; echo ;;
  heal)         curl -s -XPOST "$P/control/reset"; echo ;;
  kill-worker)
    pid="$(cat run/worker-push.pid)"
    kill -9 "$pid"
    echo "SIGKILLed worker-push ($pid). Leases expire in ~30s."
    echo "Restart with: ./scripts/chaos.sh restart-worker" ;;
  restart-worker)
    nohup ./target/debug/worker --channel push >logs/worker-push.log 2>&1 &
    echo $! >run/worker-push.pid
    echo "worker-push restarted (pid $(cat run/worker-push.pid)); it redelivers uncommitted offsets" ;;
  redis-down)   docker compose stop redis; echo "hot tier down -> workers degrade to PostgreSQL" ;;
  redis-up)     docker compose start redis ;;
  poison-burst) ./target/debug/generator --count 200 --rate 0 --poison-ratio 0.5 ;;
  dup-burst)    ./target/debug/generator --count 300 --rate 0 --duplicate-ratio 0.8 --id-prefix dupdemo ;;
  *) cat <<'EOF'
usage: chaos.sh <command>
  latency-on | errors-on | permanent-on | heal
  kill-worker | restart-worker
  redis-down | redis-up
  poison-burst | dup-burst
EOF
;;
esac