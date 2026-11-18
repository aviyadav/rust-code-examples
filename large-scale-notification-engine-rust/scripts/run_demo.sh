#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

COUNT="${COUNT:-2000}"
RATE="${RATE:-300}"
WAIT_SECS="${WAIT_SECS:-45}"

echo "==> starting infrastructure (kafka, postgres, redis)"
docker compose up -d --wait

echo "==> building workspace (first build compiles librdkafka; be patient)"
cargo build --workspace

mkdir -p logs run
start() {
  local name="$1"; shift
  if [[ -f "run/$name.pid" ]] && kill -0 "$(cat "run/$name.pid")" 2>/dev/null; then
    echo "    $name already running"; return
  fi
  nohup "$@" >"logs/$name.log" 2>&1 &
  echo $! >"run/$name.pid"
  echo "    $name (pid $(cat "run/$name.pid")) -> logs/$name.log"
}

echo "==> starting services"
start mock-provider    ./target/debug/mock-provider
sleep 1
start ingest-api       ./target/debug/ingest_api
start orchestrator     ./target/debug/orchestrator
start worker-push      ./target/debug/worker --channel push
start worker-sms       ./target/debug/worker --channel sms
start worker-email     ./target/debug/worker --channel email
start retry-scheduler  ./target/debug/retry_scheduler
sleep 2

echo "==> generating load: $COUNT events @ $RATE/s (5% duplicates, 1% poison, 2% bad recipients)"
./target/debug/generator --count "$COUNT" --rate "$RATE" \
  --duplicate-ratio 0.05 --poison-ratio 0.01 --bad-recipient-ratio 0.02 --seed-opt-outs 5

echo "==> waiting ${WAIT_SECS}s for deliveries + retries to settle..."
sleep "$WAIT_SECS"

echo "==> provider stats"
curl -s localhost:8081/stats || true
echo
./target/debug/inspect

cat <<'EOF'

Demo running. Useful commands:
  tail -f logs/worker-push.log        # worker metrics every 5s
  ./scripts/status.sh                 # dashboard + provider stats
  ./scripts/chaos.sh latency-on       # provider latency spike (article §4)
  ./scripts/chaos.sh errors-on        # 60% transient failures -> retry storm
  ./scripts/chaos.sh kill-worker      # SIGKILL push worker -> lease reclaim
  ./scripts/chaos.sh redis-down       # hot tier outage -> PG-only degradation
  ./scripts/chaos.sh heal             # restore provider defaults
  ./scripts/stop_demo.sh              # stop services (infra stays up)
EOF