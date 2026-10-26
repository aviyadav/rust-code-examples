# Top 10 Rust Crates and Tools

Small, runnable demos of popular Rust crates. Each demo is a separate binary in `src/bin/`.

## Demos

| Demo | Crate(s) | File |
| --- | --- | --- |
| `serde-demo` | [`serde`](https://crates.io/crates/serde), [`serde_json`](https://crates.io/crates/serde_json) | [`src/bin/serde-demo.rs`](src/bin/serde-demo.rs) |
| `tokio-demo` | [`tokio`](https://crates.io/crates/tokio) | [`src/bin/tokio-demo.rs`](src/bin/tokio-demo.rs) |
| `clap-demo` | [`clap`](https://crates.io/crates/clap) | [`src/bin/clap-demo.rs`](src/bin/clap-demo.rs) |
| `clap-orders` | [`clap`](https://crates.io/crates/clap) | [`src/bin/clap-orders.rs`](src/bin/clap-orders.rs) |
| `thiserror-anyhow-eg` | [`thiserror`](https://crates.io/crates/thiserror), [`anyhow`](https://crates.io/crates/anyhow), [`sqlx`](https://crates.io/crates/sqlx), [`duckdb`](https://crates.io/crates/duckdb) | [`src/bin/thiserror-anyhow-eg.rs`](src/bin/thiserror-anyhow-eg.rs) |
| `axum-demo` | [`axum`](https://crates.io/crates/axum), [`sqlx`](https://crates.io/crates/sqlx), [`thiserror`](https://crates.io/crates/thiserror) | [`src/bin/axum-demo.rs`](src/bin/axum-demo.rs) |
| `sqlx-demo` | [`sqlx`](https://crates.io/crates/sqlx) | [`src/bin/sqlx-demo.rs`](src/bin/sqlx-demo.rs) |
| `sqlx-duckdb-demo` | [`sqlx`](https://crates.io/crates/sqlx), [`duckdb`](https://crates.io/crates/duckdb), [`anyhow`](https://crates.io/crates/anyhow) | [`src/bin/sqlx-duckdb-demo.rs`](src/bin/sqlx-duckdb-demo.rs) |
| `rayon-demo` | [`rayon`](https://crates.io/crates/rayon) | [`src/bin/rayon-demo.rs`](src/bin/rayon-demo.rs) |
| `reqwest-demo` | [`reqwest`](https://crates.io/crates/reqwest), [`axum`](https://crates.io/crates/axum) | [`src/bin/reqwest-demo.rs`](src/bin/reqwest-demo.rs) |
| `tracing-demo` | [`tracing`](https://crates.io/crates/tracing), [`tracing-subscriber`](https://crates.io/crates/tracing-subscriber), [`thiserror`](https://crates.io/crates/thiserror) | [`src/bin/tracing-demo.rs`](src/bin/tracing-demo.rs) |

## Dependencies

Every crate the demos use, with the exact command to add it:

| Crate | Used by | Command |
| --- | --- | --- |
| `serde` | `serde-demo`, `serde-eg`, `clap-orders`, `axum-demo`, `reqwest-demo` | `cargo add serde --features derive` |
| `serde_json` | `serde-demo`, `serde-eg`, `clap-orders`, `axum-demo`, `reqwest-demo` | `cargo add serde_json` |
| `tokio` | `tokio-demo`, `thiserror-anyhow-eg`, `axum-demo`, `sqlx-demo`, `sqlx-duckdb-demo`, `reqwest-demo`, `tracing-demo` | `cargo add tokio --features full` |
| `clap` | `clap-demo`, `clap-orders` | `cargo add clap --features derive` |
| `thiserror` | `thiserror-anyhow-eg`, `axum-demo`, `tracing-demo` | `cargo add thiserror` |
| `anyhow` | `thiserror-anyhow-eg`, `sqlx-duckdb-demo`, `reqwest-demo` | `cargo add anyhow` |
| `sqlx` | `thiserror-anyhow-eg`, `axum-demo`, `sqlx-demo`, `sqlx-duckdb-demo` | `cargo add sqlx --features runtime-tokio,sqlite` |
| `duckdb` | `thiserror-anyhow-eg`, `sqlx-duckdb-demo` | `cargo add duckdb@~1.10506 --features bundled` |
| `axum` | `axum-demo`, `reqwest-demo` | `cargo add axum` |
| `rayon` | `rayon-demo` | `cargo add rayon` |
| `reqwest` | `reqwest-demo` | `cargo add reqwest --features json` |
| `tracing` | `tracing-demo` | `cargo add tracing` |
| `tracing-subscriber` | `tracing-demo` | `cargo add tracing-subscriber --features env-filter` |

Why the features matter:

- `serde` / `clap` `derive`: enables `#[derive(Serialize, Deserialize)]` and `#[derive(Parser)]`.
- `tokio` `full`: enables `#[tokio::main]`, `tokio::spawn`, `tokio::time` and the rest of the runtime. Without features, Tokio has almost nothing switched on.
- `sqlx` `runtime-tokio,sqlite`: picks Tokio as the async runtime and SQLite as the database driver.
- `duckdb` `bundled`: compiles DuckDB from source, so no system install is needed.
- `duckdb@~1.10506`: pins DuckDB 1.5.6. The newer `1.20000.x` crate bundles a DuckDB 2.0 *dev* build, and DuckDB's extension server has no extensions for dev builds. So `INSTALL sqlite` fails with HTTP 404. Extensions must match the DuckDB version exactly. Revisit the pin once a crate with a stable DuckDB 2.0 ships.
- `axum`: the default features (HTTP/1, JSON, path and query extractors) are enough. `cargo add axum` installs 0.8+, which uses the `/orders/{id}` route syntax.
- `reqwest` `json`: enables `.json(&body)` on requests and `.json::<T>()` on responses. TLS (HTTPS) is on by default.
- `tracing-subscriber` `env-filter`: lets `RUST_LOG` control which log levels are shown.

Add them all at once (e.g. in a fresh project):

```sh
cargo add serde --features derive
cargo add serde_json
cargo add tokio --features full
cargo add clap --features derive
cargo add thiserror anyhow
cargo add sqlx --features runtime-tokio,sqlite
cargo add duckdb@~1.10506 --features bundled
cargo add axum
cargo add rayon
cargo add reqwest --features json
cargo add tracing
cargo add tracing-subscriber --features env-filter
```

### Tools

| Tool | Used by | Command |
| --- | --- | --- |
| `sqlx-cli` | `sqlx-demo` | `cargo install sqlx-cli --version 0.9.0 --no-default-features --features sqlite --locked` |

`sqlx-cli` creates the development database, runs migrations and generates the offline query cache. Its version should match the `sqlx` crate. `--no-default-features --features sqlite` skips the Postgres/MySQL drivers so it installs faster.

After changing a query or the schema, run cargo sqlx prepare and commit .sqlx/. Otherwise fresh clones will fail to build.

## Build

> **Note:** the first build is slow (about 5 minutes). `duckdb` uses the `bundled` feature, which compiles DuckDB from C++ source so nothing needs to be installed on your system. Because all demos share one `Cargo.toml`, this happens on the first build of *any* demo. Later builds are fast.

Build everything:

```sh
cargo build
```

Build a single demo:

```sh
cargo build --bin serde-demo
cargo build --bin serde-eg
cargo build --bin tokio-demo
cargo build --bin thiserror-anyhow-eg
cargo build --bin axum-demo
cargo build --bin sqlx-demo
cargo build --bin sqlx-duckdb-demo
cargo build --bin rayon-demo
cargo build --bin reqwest-demo
cargo build --bin tracing-demo
```

## Run

```sh
cargo run --bin serde-demo
cargo run --bin serde-eg
cargo run --bin tokio-demo
cargo run --bin thiserror-anyhow-eg
cargo run --bin sqlx-demo
cargo run --bin sqlx-duckdb-demo
cargo run --release --bin rayon-demo
cargo run --bin reqwest-demo
cargo run --bin tracing-demo
RUST_LOG=debug cargo run --bin tracing-demo
```

`tokio-demo` spawns three async tasks that fetch orders concurrently and prints each result.

`rayon-demo` scores 50,000 orders with a CPU-heavy function, first with `iter()` and then with `par_iter()`. The only code change is the method name. It checks both totals match and prints the speedup (13x on a 16-thread machine). It also shows parallel `filter`/`count`, a lock-free `fold`/`reduce`, `par_sort_unstable_by` and `rayon::join`. Use `--release` for realistic timings.

`reqwest-demo` is an HTTP client. So it runs offline and without an API key, it starts a small local orders API (axum, random port) and calls it the way you'd call a real service. It covers a reused `Client` with timeout and user agent, `bearer_auth`, typed JSON GET and POST, `error_for_status` on 404, a 401 without a token, a per-request timeout, and concurrent requests. Set `ORDERS_API_TOKEN` to override the token.

`tracing-demo` shows structured logging. `#[instrument]` wraps each call in a span, so concurrent orders' log lines stay tagged with their `order_id`, e.g. `process_order{order_id=2}:charge_card{email=...}: amount over card limit`. `err` logs returned errors inside the span. `RUST_LOG` controls verbosity (`info` by default, `debug` shows the database and payment steps).

`thiserror-anyhow-eg` shows error handling against real databases: in-memory SQLite (via `sqlx`) stores orders and DuckDB handles payment reporting. `thiserror` defines a typed `OrderError` that wraps `sqlx::Error` and `duckdb::Error` with `#[from]`. `anyhow` adds context such as `failed to load order 42` and prints the full error chain. The run deliberately triggers each error case: a declined payment, a missing order, a SQLite `CHECK` rule violation and a missing CSV file in DuckDB.

`sqlx-demo` uses sqlx's compile-time checked macros (`query!`, `query_as!`, `query_scalar!`). While compiling, they check every query against the database in `DATABASE_URL` (set in `.env`). A misspelled column, or a nullable column going into a non-`Option` field, is a compile error. The demo creates, pays and looks up orders, decodes a `TEXT` column into a Rust enum, shows a transaction rolling back after a `CHECK` rule fails, and prints a status report. At runtime it uses an in-memory database built from the same `migrations/`.

The repo includes the offline query cache in `.sqlx/`, so a fresh clone builds without a database. To change a query or the schema, set up the development database:

```sh
cp .env.example .env          # sets DATABASE_URL=sqlite:dev.db
sqlx database create          # creates dev.db from DATABASE_URL in .env
sqlx migrate run              # applies migrations/*.sql
cargo sqlx prepare            # refresh .sqlx/ after changing queries; commit it
```

> **Caution:** once `.env` exists, the macros check queries against `dev.db` instead of `.sqlx/`. If `dev.db` is missing, the build fails with confusing type errors. Run the setup steps above, delete `.env`, or build with `SQLX_OFFLINE=true cargo build --bin sqlx-demo`.

`sqlx-duckdb-demo` combines the two databases on one file. sqlx has no DuckDB driver, so sqlx writes 200 orders into `orders-demo.db` (SQLite) in a transaction. DuckDB then `ATTACH`es that same file with its `sqlite` extension and runs the analytics: revenue and median by status, revenue by email domain (`split_part`, `arg_max`) and a customer leaderboard (window functions). No data is copied between the databases. The file is recreated on each run.

> **Note:** the first run downloads DuckDB's `sqlite` extension (about 50 MB, cached in `~/.duckdb/extensions`), so it needs network access once.

`axum-demo` is a web server on port 3000. It serves orders from an in-memory SQLite database and turns errors into proper HTTP responses via `IntoResponse`. Start it, then query it from another terminal (stop it with `Ctrl+C`):

```sh
cargo run --bin axum-demo

curl localhost:3000/orders/1     # 200 {"id":1,"customer_email":"ferris@rust-lang.org","total_cents":1999}
curl localhost:3000/orders/42    # 404 {"error":"order 42 not found"}
curl localhost:3000/orders/abc   # 400 Invalid URL: Cannot parse `abc` to a `u64`
```

Arguments for the program go after `--`:

```sh
cargo run --bin clap-orders -- --help
cargo run --bin clap-orders -- list --status pending
cargo run --bin clap-orders -- -f json show 2
cargo run --bin clap-orders -- create --email new@example.com --total 19.99
```

Run the main binary:

```sh
cargo run --bin top-10-rust-crates-and-tools
```

## Developer tools

Cargo subcommands and libraries that keep Rust code clean, tested, secure and fast.

| Tool | What it helps with | Install |
| --- | --- | --- |
| `clippy` | Lints: catches bugs, slow code and unidiomatic Rust | `rustup component add clippy` |
| `rustfmt` | Formats code to the standard style | `rustup component add rustfmt` |
| `cargo-nextest` | Faster test runner with clearer output | `cargo install cargo-nextest --locked` |
| `cargo-audit` | Finds dependencies with known security advisories | `cargo install cargo-audit --locked` |
| `cargo-deny` | Enforces policy: advisories, licenses, banned or duplicate crates, sources | `cargo install cargo-deny --locked` |
| `bacon` | Re-runs check, clippy or tests every time you save | `cargo install bacon --locked` |
| `cargo-flamegraph` | Profiles a program and shows where CPU time goes | `cargo install flamegraph --locked` |
| `criterion` / `divan` | Benchmarking libraries for measuring code speed | `cargo add --dev criterion` / `cargo add --dev divan` |

`clippy` and `rustfmt` come with most Rust installs. Check with `cargo clippy --version`.

### clippy: the linter

Clippy has 700+ lints on top of the compiler's warnings: likely bugs, needless allocations and clones, and simpler ways to write the same code. It caught two real issues while building these demos: a `println!` with a literal argument, and fields that were never read.

```sh
cargo clippy                                  # lint the project
cargo clippy --all-targets                    # include tests, benches and examples
cargo clippy --all-targets -- -D warnings     # treat warnings as errors (use in CI)
cargo clippy --fix --allow-dirty              # apply the suggested fixes automatically
cargo clippy -- -W clippy::pedantic           # opt in to stricter, more opinionated lints
```

Configure lints for the whole project in `Cargo.toml`:

```toml
[lints.clippy]
unwrap_used = "warn"   # flag .unwrap() calls
todo = "deny"          # forbid leftover todo!() macros
```

### rustfmt: the formatter

Formats every file to the community style, so code reviews don't argue about formatting. Most editors run it on save.

```sh
cargo fmt                # format the whole project in place
cargo fmt --check        # only report unformatted files, exit 1 if any (use in CI)
cargo fmt -- src/bin/rayon-demo.rs   # format a single file
```

Optional `rustfmt.toml` in the project root:

```toml
max_width = 100
use_field_init_shorthand = true
```

### cargo-nextest: a faster test runner

A drop-in replacement for `cargo test`. It runs each test in its own process, in parallel. It's often much faster on large projects, shows a clean pass/fail summary, and one test crashing can't take down the others.

```sh
cargo nextest run                        # run all tests
cargo nextest run order                  # only tests whose name contains "order"
cargo nextest run --no-fail-fast         # keep going after a failure
cargo nextest run --retries 2            # retry flaky tests up to 2 times
cargo nextest run -E 'test(/^pay_/)'     # filter with an expression (regex on test names)
```

Nextest doesn't run doctests, so also run `cargo test --doc` if you have them.

### cargo-audit: security advisories

Checks `Cargo.lock` against the [RustSec advisory database](https://rustsec.org/). It reports dependencies with known vulnerabilities, crates that are no longer maintained, and yanked versions. This project has a lot of dependencies (sqlx, axum, reqwest, duckdb), so run it regularly.

```sh
cargo audit                       # scan Cargo.lock
cargo audit --deny warnings       # also fail on "unmaintained" or "yanked" warnings (use in CI)
cargo audit --ignore RUSTSEC-2023-0071   # accept a specific advisory you've reviewed
```

### cargo-deny: dependency policy

A broader gatekeeper than `cargo-audit`. It also checks advisories, and adds three more checks:

- **licenses**: every dependency uses a license you allow, e.g. no GPL in a proprietary app.
- **bans**: blocks specific crates, and warns when two versions of the same crate are compiled in.
- **sources**: dependencies only come from crates.io or git repositories you trust.

```sh
cargo deny init                   # create a deny.toml with commented defaults
cargo deny check                  # run all checks
cargo deny check licenses         # run just one: advisories | bans | licenses | sources
```

Example `deny.toml` excerpt:

```toml
[licenses]
allow = ["MIT", "Apache-2.0", "BSD-3-Clause", "Unicode-3.0"]

[bans]
multiple-versions = "warn"
```

### bacon: background checker

Runs in a terminal next to your editor and re-runs `check`, `clippy` or tests whenever you save a file. Errors are shown sorted, with the first one at the top, so you fix the real problem first.

```sh
bacon                             # cargo check on every save
bacon clippy                      # cargo clippy on every save
bacon test                        # cargo test on every save
bacon nextest                     # cargo nextest run on every save
bacon run -- --bin rayon-demo     # rebuild and rerun a demo on every save
```

While it's running, press `c` for clippy, `t` for tests, `h` for help and `q` to quit. Run `bacon --init` to create a `bacon.toml` with custom jobs.

### cargo-flamegraph: CPU profiling

Runs your program under a profiler (`perf` on Linux, `dtrace` on macOS) and writes `flamegraph.svg`. Wider bars mean more CPU time, so you can see the slow function at a glance. Open the SVG in a browser and click a bar to zoom in.

```sh
# Profile the rayon demo. Release build, with debug symbols for readable function names
CARGO_PROFILE_RELEASE_DEBUG=true cargo flamegraph --bin rayon-demo

# Pass arguments to the program after --
cargo flamegraph --bin clap-orders -- list --status pending

# Write to a custom file
cargo flamegraph --bin rayon-demo -o rayon.svg
```

> **Note:** on Linux you need `perf` installed (`linux-tools` / `linux-perf` package). You may also need to allow profiling with `echo -1 | sudo tee /proc/sys/kernel/perf_event_paranoid`. On macOS, `dtrace` requires `sudo` or `cargo flamegraph --root`. In `rayon-demo`, `expensive_score` should take up most of the graph.

### criterion / divan: benchmarks

These are libraries, not subcommands: you write benchmarks in `benches/` and run them with `cargo bench`. Both run your code many times, account for measurement noise, and report reliable timings. Use them to check whether a change really made code faster.

- **criterion**: the long-standing standard. It does statistical analysis, detects changes against the previous run ("performance has regressed") and writes an HTML report with charts to `target/criterion/report/index.html`.
- **divan**: newer and simpler. You add an attribute to a function, it's easy to benchmark the same code with several inputs, and results are printed as a compact table.

For either, register the bench in `Cargo.toml` and turn off the default test harness:

```toml
[[bench]]
name = "scoring"      # benches/scoring.rs
harness = false
```

**criterion** (`cargo add --dev criterion`), in `benches/scoring.rs`:

```rust
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn expensive_score(id: u64) -> u64 {
    (0..20_000).fold(id, |h, _| (h ^ (h >> 33)).wrapping_mul(0xff51_afd7_ed55_8ccd)) % 100
}

fn bench_scoring(c: &mut Criterion) {
    // black_box stops the compiler from optimizing the work away
    c.bench_function("expensive_score", |b| b.iter(|| expensive_score(black_box(42))));
}

criterion_group!(benches, bench_scoring);
criterion_main!(benches);
```

Output:

```
expensive_score         time:   [19.450 µs 19.487 µs 19.534 µs]
Found 8 outliers among 100 measurements (8.00%)
```

**divan** (`cargo add --dev divan`), in `benches/scoring.rs`:

```rust
fn main() {
    divan::main();
}

// Runs once per input and prints a row for each
#[divan::bench(args = [1, 1_000, 1_000_000])]
fn expensive_score(id: u64) -> u64 {
    (0..20_000).fold(divan::black_box(id), |h, _| {
        (h ^ (h >> 33)).wrapping_mul(0xff51_afd7_ed55_8ccd)
    }) % 100
}
```

Output:

```
scoring             fastest       │ slowest       │ median        │ mean          │ samples │ iters
╰─ expensive_score                │               │               │               │         │
   ├─ 1             19.27 µs      │ 31.09 µs      │ 19.29 µs      │ 19.55 µs      │ 100     │ 100
   ├─ 1000          19.27 µs      │ 54.38 µs      │ 19.29 µs      │ 19.8 µs       │ 100     │ 100
   ╰─ 1000000       19.27 µs      │ 27.43 µs      │ 19.29 µs      │ 19.62 µs      │ 100     │ 100
```

```sh
cargo bench                       # run all benchmarks
cargo bench --bench scoring       # run one bench file
cargo bench -- expensive          # only benchmarks whose name matches "expensive"
```

> **Note:** a benchmark can only call code from a library target (`src/lib.rs`), not from `src/bin/*.rs`. That's why the examples above copy the function. In a real project, move the code you want to measure into `src/lib.rs`.

### Putting it together

A typical pre-push or CI check, from fastest to slowest:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run
cargo deny check        # includes the advisory scan; or use cargo audit
```
