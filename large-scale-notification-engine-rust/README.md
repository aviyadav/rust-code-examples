# Large-Scale Notification Engine (Rust)

A runnable implementation of *Designing a Large-Scale Notification Engine in Rust*.
It demonstrates ingest, orchestration, per-channel delivery, durable leases,
retries, dead-lettering, idempotent provider calls, backpressure, and failure
injection. Kafka, PostgreSQL, and Redis run in Docker; all Rust processes run on
the host.

This is a local reference/demo system, not a production-hosted notification
service.

## Architecture

```text
HTTP API or generator
        |
        v
notifications.ingest (6 partitions)
        |
        v
  orchestrator: opt-outs, terminal-state dedup, template rendering, routing
        |
        v
notifications.push / sms / email (3 partitions each)
        |
        v
 channel workers: PostgreSQL lease + fencing, rate limits, concurrency bounds
        |
        v
 mock provider: idempotency, latency, transient/permanent failure injection

Transient/rate-limited work -> notifications.retry (3 partitions)
                            -> retry_scheduler -> original channel topic
Poison/permanent/expired work -> notifications.dlq (1 partition)
```

- **PostgreSQL is the source of truth.** A unique `(notification_id, channel)`
  row, expiring leases, and attempt-based fencing prevent stale workers from
  recording outcomes.
- **Delivery is at least once.** Kafka offsets can be redelivered after crashes.
  The stable `notification_id:channel` idempotency key makes delivery
  effectively-once only when the provider honors idempotency; other providers
  degrade to at-least-once delivery.
- **Redis is a hint and shared limiter, never the delivery ledger.** Success
  dedup entries expire after 24 hours. Redis operations time out after 150 ms;
  outages degrade to PostgreSQL checks, and tenant rate limiting fails open.
- **Concurrent completion does not skip offsets.** Workers commit only gap-free
  per-partition watermarks and flush their best-known watermark on rebalance or
  Ctrl-C shutdown.
- **Retries use exponential backoff with jitter.** Delay is
  `2^clamp(attempt, 1, 6)` plus a random value from zero through that base,
  giving 2–4 seconds on the first retry and at most 64–128 seconds. The default
  maximum is 8 provider attempts.
- Topics and database tables are created idempotently at service startup.
  Kafka topic auto-creation is disabled in Docker Compose.

## Prerequisites

- Linux or macOS with Bash, or Windows with PowerShell 7+
- Docker with Compose v2
- Rust stable (this checkout is tested with Rust 1.100)
- `cmake` and a C compiler:
  - macOS: `brew install cmake` plus Xcode Command Line Tools
  - Debian/Ubuntu: `sudo apt install cmake build-essential`
- `curl`

The first build compiles librdkafka and libcurl from source. No system
librdkafka or libcurl development package is needed. SQLx uses runtime queries,
so `DATABASE_URL` is **not** needed at compile time.

## Quickstart

From the repository root:

```bash
chmod +x scripts/*.sh
./scripts/run_demo.sh
```

On Windows with PowerShell 7+, use the equivalent scripts:

```powershell
./scripts_ps/run_demo.ps1
./scripts_ps/status.ps1
./scripts_ps/stop_demo.ps1
```

```powershell
Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass
.\scripts_ps\run_demo.ps1
```

This requires no administrator access and affects only the current PowerShell session. Alternatively, run a script directly:

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File ./scripts_ps/run_demo.ps1
pwsh -NoProfile -ExecutionPolicy Bypass -File ./scripts_ps/status.ps1
pwsh -NoProfile -ExecutionPolicy Bypass -File ./scripts_ps/stop_demo.ps1

```

Every Bash script has a PowerShell counterpart in `scripts_ps/`; replace
`scripts/<name>.sh` with `scripts_ps/<name>.ps1` for the commands below. The
PowerShell demo writes service stdout to `logs/<name>.log` and stderr to
`logs/<name>.err.log`.

The script starts infrastructure, builds the workspace, starts seven Rust
processes (provider, API, orchestrator, three workers, retry scheduler), then
generates 2,000 events at 300 events/second. The generated traffic includes 5%
duplicates, 1% poison records, 2% permanently invalid recipients, and five
seeded email opt-outs. It waits 45 seconds before printing provider and
PostgreSQL summaries.

Override the demo defaults when needed:

```bash
COUNT=5000 RATE=500 WAIT_SECS=60 ./scripts/run_demo.sh
```

In another terminal:

```bash
./scripts/status.sh                    # Docker, provider, and delivery summaries
./target/debug/inspect --watch        # refresh PostgreSQL state every 2 seconds
curl -s localhost:8081/messages        # latest 50 mock deliveries
tail -f logs/worker-push.log           # worker metrics every 5 seconds
```

Optional Kafka UI (topics, consumer groups, and DLQ records):

```bash
docker compose --profile ui up -d
# open http://localhost:8090
```

Stop the Rust processes but leave infrastructure and data running:

```bash
./scripts/stop_demo.sh
```

Stop everything with `./scripts/stop_demo.sh --all`. See
[Data and cleanup](#data-and-cleanup) to delete persistent volumes.

## Components and ports

| Component | Binary/source | Port | Role |
| --- | --- | --- | --- |
| Ingest API | `ingest_api` | 8080 | Single/batch ingest and delivery-status queries |
| Orchestrator | `orchestrator` | – | Opt-outs, dedup, rendering, channel routing |
| Workers ×3 | `worker --channel push\|sms\|email` | – | Leases, limits, dispatch, retries, outcomes |
| Retry scheduler | `retry_scheduler` | – | Per-partition delayed forwarding from the retry topic |
| Mock provider | `mock-provider` | 8081 | Idempotent sends and runtime failure injection |
| Load generator | `generator` | – | Normal, duplicate, poison, and invalid-recipient traffic |
| Inspector | `inspect` | – | PostgreSQL delivery dashboard |
| Kafka | Docker | 29092 (host) | Event log; broker also listens on 9092 inside Compose |
| PostgreSQL | Docker | 5432 | Durable delivery and preference state |
| Redis | Docker | 6379 | Ephemeral dedup hints and tenant windows |
| Kafka UI (optional) | Docker profile `ui` | 8090 | Kafka inspection UI |

## HTTP APIs

### Ingest API — `localhost:8080`

| Method and path | Behavior |
| --- | --- |
| `POST /v1/notifications` | Accept one event; returns `202 Accepted` after the Kafka produce completes |
| `POST /v1/notifications/batch` | Accept a JSON array, maximum 1,000 events per request |
| `GET /v1/notifications/:id` | Return all channel delivery rows for a notification ID |
| `GET /health` | Return `ok` |

`channel` must be `push`, `sms`, or `email`; `recipient_id` and `payload` are
required. `notification_id` defaults to a random UUID, `tenant_id` to `default`,
and `template_id` to `generic`.

```bash
curl -s localhost:8080/v1/notifications \
  -H 'content-type: application/json' \
  -d '{
    "notification_id": "demo-1",
    "tenant_id": "t1",
    "channel": "push",
    "recipient_id": "user-0042",
    "template_id": "order_shipped",
    "payload": {"name": "Ada", "order_id": "ORD-9"}
  }'

curl -s localhost:8080/v1/notifications/demo-1
curl -s localhost:8080/health
```

Templates are intentionally small: `order_shipped` uses `{{name}}` and
`{{order_id}}`, `otp` uses `{{code}}`, and every other template renders
`{{body}}`. Rendering is simple string substitution, not a template language.

Batch requests are processed sequentially and are not atomic: if one produce
fails, earlier entries may already have been accepted even though the request
returns an error.

### Mock provider — `localhost:8081`

| Method and path | Behavior |
| --- | --- |
| `POST /v1/send` | Simulate a provider send; honors `X-Idempotency-Key` |
| `GET /stats` | Accepted, replayed, rejected, in-flight, and per-channel counters |
| `GET /messages` | Latest 50 successful deliveries, newest first |
| `POST /control` | Update selected failure/latency/TTL settings |
| `POST /control/reset` | Restore 5% transient failures, 0% permanent failures, 30 ms latency, 40 ms jitter, and a 1-hour dedup TTL |
| `GET /health` | Return `ok` |

Recipients beginning with `invalid:` always receive HTTP 400. HTTP 429 and 5xx
responses are retryable; other non-2xx responses are permanent. Provider state
is in memory and resets when the process restarts.

Example control update (fields are optional):

```bash
curl -s -X POST localhost:8081/control \
  -H 'content-type: application/json' \
  -d '{"transient_rate": 0.6, "latency_ms": 100, "latency_jitter_ms": 50}'
```

## Generate load

`scripts/generate_events.sh` forwards all arguments to the generator:

```bash
./scripts/generate_events.sh \
  --count 5000 --rate 500 \
  --duplicate-ratio 0.05 \
  --poison-ratio 0.01 \
  --bad-recipient-ratio 0.02 \
  --seed-opt-outs 5
```

Useful options:

| Option | Default | Purpose |
| --- | --- | --- |
| `--via api\|kafka` | `api` | Use the HTTP batch API or produce directly to the ingest topic |
| `--rate N` | 200 | Approximate events/second; `0` means an unthrottled burst |
| `--channels push,sms,email` | all three | Restrict generated channels |
| `--tenants N` / `--recipients N` | 5 / 200 | Cardinality of generated tenants and recipients |
| `--id-prefix TEXT` | random | Reuse deterministic IDs across runs to demonstrate dedup |
| `--seed-opt-outs N` | 0 | Opt `user-0000` through `user-(N-1)` out of email |

API-mode events are sent in batches of 100. Poison records are always produced
directly to Kafka because the HTTP API only accepts valid JSON.

## Failure drills

Run these against the running demo:

| Command | What to observe |
| --- | --- |
| `./scripts/chaos.sh latency-on` | 2.8–3.4-second provider latency, timeouts, retries, and backpressure |
| `./scripts/chaos.sh errors-on` | 60% transient failures and retry-topic traffic |
| `./scripts/chaos.sh permanent-on` | 20% permanent failures, `DEAD_LETTER` rows, and DLQ records |
| `./scripts/chaos.sh kill-worker` | SIGKILL the push worker; leases expire in about 30 seconds and work is reclaimed |
| `./scripts/chaos.sh restart-worker` | Restart the push worker and redeliver uncommitted offsets |
| `./scripts/chaos.sh redis-down` | PostgreSQL-only dedup and fail-open tenant limiting |
| `./scripts/chaos.sh redis-up` | Restore Redis |
| `./scripts/chaos.sh poison-burst` | 200 records with 50% invalid JSON sent to the DLQ |
| `./scripts/chaos.sh dup-burst` | 300 records with 80% duplicates using the fixed `dupdemo` prefix |
| `./scripts/chaos.sh heal` | Restore mock-provider defaults; does not undo Redis or worker drills |

Use `./scripts/chaos.sh` with no arguments for the same command list.

## Configuration

Binaries load `.env` when present and otherwise use these defaults. `RUST_LOG`
controls tracing, for example `RUST_LOG=info,engine=debug`.

| Variable | Default | Used by |
| --- | --- | --- |
| `KAFKA_BROKERS` | `localhost:29092` | API, orchestrator, workers, retry scheduler, generator |
| `DATABASE_URL` | `postgres://notify:notify@localhost:5432/notifications` | API, orchestrator, workers, inspector, generator opt-out seeding |
| `REDIS_URL` | `redis://localhost:6379` | Orchestrator and workers |
| `API_PORT` | 8080 | Ingest API |
| `PROVIDER_PORT` | 8081 | Mock provider |
| `PROVIDER_URL` | `http://127.0.0.1:8081/v1/send` | Workers |
| `WORKER_GROUP` | `workers-<channel>` | Each worker |
| `PROVIDER_CONCURRENCY` | 200 | Maximum in-flight provider calls per worker |
| `PROVIDER_RPS` | 50 | Local rate limit per worker; `0` is clamped to 1 |
| `TENANT_RPS` | 1000 | Shared Redis fixed one-second tenant window; `0` disables it |
| `LEASE_SECS` | 30 | Processing lease and crash-reclaim delay |
| `HTTP_TIMEOUT_SECS` | 3 | Provider request timeout |
| `JOB_CHANNEL_CAP` | 500 | Bounded queued jobs per worker |
| `MAX_ATTEMPTS` | 8 | Provider attempts before dead-lettering |

`PROVIDER_CONCURRENCY` and `PROVIDER_RPS` are per process, not cluster-wide.
Set them separately when launching workers with different limits.

## Data and cleanup

- Delivery state: `notification_deliveries` in PostgreSQL. Runtime statuses are
  `PROCESSING`, `FAILED_RETRYABLE`, `SUCCESS`, `DEAD_LETTER`, and `SUPPRESSED`.
- Preferences: `notification_preferences`; the generator can seed email opt-outs.
- DLQ context: source topic, partition, offset, reason, lossy UTF-8 payload, and
  failure timestamp. There is no automatic DLQ replay command; inspect records
  with Kafka UI or Kafka CLI tools.
- Kafka and PostgreSQL use named Docker volumes. Redis is cache-only, and mock
  provider deliveries/idempotency keys are process-local memory.
- Service logs and PID files are written to `logs/` and `run/`.

```bash
./scripts/stop_demo.sh          # stop only Rust processes
./scripts/stop_demo.sh --all    # also stop Compose services; keep volumes
docker compose down -v          # delete Kafka/PostgreSQL volumes as well
```

## Development

```bash
cargo build --workspace
cargo test --workspace
cargo run -p engine --bin inspect -- --watch
cargo run -p generator -- --count 100 --rate 20
```

Project layout:

```text
crates/common       events, topic definitions, Kafka helpers, idempotent schema
crates/engine       API, orchestrator, workers, retry scheduler, inspector
crates/provider     stateful mock provider and failure controls
crates/generator    synthetic load and opt-out seeding
scripts             demo lifecycle, status, load, and chaos commands
docker-compose.yml  Kafka, PostgreSQL, Redis, optional Kafka UI
```

For a manual run instead of `run_demo.sh`:

```bash
docker compose up -d --wait
cargo build --workspace
./target/debug/mock-provider &
./target/debug/ingest_api &
./target/debug/orchestrator &
./target/debug/worker --channel push &
./target/debug/worker --channel sms &
./target/debug/worker --channel email &
./target/debug/retry_scheduler &
./scripts/generate_events.sh --count 5000 --rate 500
```

Run the processes in separate terminals to stop them with Ctrl-C and exercise
the workers' watermark/producer flush paths.

## Current limitations

- Single-broker, plaintext local infrastructure with no API authentication,
  authorization, TLS, secrets management, or production deployment manifests.
- A mock in-memory provider and three built-in string-replacement templates;
  there are no real APNs/FCM/Twilio/SendGrid integrations.
- Exactly-once external delivery is not claimed. Provider idempotency, durable
  dependencies, and correct client IDs are required to avoid duplicate effects.
- Some terminal database/DLQ writes are best effort. Simultaneous PostgreSQL
  and Kafka failures can lose terminal bookkeeping or require manual recovery.
- Retry delay timers are in memory; retry records remain in Kafka. A crashed
  scheduler relies on redelivery of uncommitted offsets.
- Schema setup is idempotent DDL, not a versioned migration system.
- Tenant limiting uses a fixed one-second Redis window and fails open; worker
  rate/concurrency limits are process-local.
- The test suite covers rendering, backoff bounds, and watermark ordering. It
  does not yet include broker/database integration or end-to-end failure tests.
