# Multithreading for Blazing Fast Apps

A hands-on Rust playground that demonstrates core multithreading concepts —
spawning threads, sharing state safely, coordinating workers, and parallelizing
CPU-bound work with [Rayon](https://docs.rs/rayon).

Each concept lives in its own binary so you can run and study them in isolation,
and a `run-all` harness benchmarks every example to show the difference between
sequential and multithreaded execution.

## Requirements

- [Rust](https://www.rust-lang.org/tools/install) (edition 2024, so Rust 1.85+)
- Cargo (installed with Rust)

## Project layout

```
.
├── Cargo.toml
├── README.md
└── src
    ├── main.rs                          # Spawning a thread and joining it
    └── bin
        ├── parallel-data-processing.rs  # Rayon: .iter() vs .par_iter()
        ├── shared_counter.rs            # Arc<Mutex<T>> shared state
        ├── worker-coordinator-pattern.rs# mpsc channels between workers
        └── run-all.rs                   # Test harness: runs every program
```

## Programs

| Binary | Concept | What it shows |
| --- | --- | --- |
| `multithreading-for-blazing-fast-apps` | `thread::spawn` + `join` | A spawned thread and the main thread running concurrently, then waiting for the child to finish. |
| `parallel-data-processing` | Rayon parallel iterators | The same CPU-heavy workload run sequentially vs with `.par_iter()`, with timings. |
| `shared_counter` | `Arc<Mutex<T>>` | Ten threads safely incrementing one shared counter. |
| `worker-coordinator-pattern` | `mpsc` channels | Multiple worker threads sending results to a coordinating main thread. |
| `run-all` | Test harness | Runs every program above, sequentially and in parallel, and reports the speedup. |

## Build

Build every binary (the main program and all examples):

```sh
cargo build --bins
```

Build a release version (recommended when benchmarking, since debug builds are
much slower):

```sh
cargo build --release --bins
```

## Run

Run the main example:

```sh
cargo run
```

Run any individual example with `--bin`:

```sh
cargo run --bin parallel-data-processing
cargo run --bin shared_counter
cargo run --bin worker-coordinator-pattern
```

## Run all programs (the test harness)

The `run-all` harness executes every program multiple times — once
sequentially and once with one thread per invocation — and prints the timing
difference and speedup.

```sh
# Build the binaries first so the harness can find them
cargo build --bins

# 5 invocations per program (default)
cargo run --bin run-all

# 20 invocations per program
cargo run --bin run-all -- 20
```

Example output (16-core machine, 5 invocations per program):

```
🧪 Running every example program
   invocations per program : 5
   available CPU cores     : 16

▶ multithreading-for-blazing-fast-apps
   sequential :   30.346ms
   parallel   :    6.336ms
   speedup    :       4.79x

▶ parallel-data-processing
   sequential :  345.863ms
   parallel   :   96.659ms
   speedup    :       3.58x
...
──────────────────────────────────────────
   total sequential :  635.765ms
   total parallel   :  155.970ms
   overall speedup  :       4.08x
✅ All programs executed!
```

Notes:

- The harness locates the compiled binaries next to itself, so run
  `cargo build --bins` first. If a binary is missing it is skipped with a hint.
- Child process output is discarded so the summary stays readable.
- Speedup depends on your core count and how much real work each program does.
  Short-lived examples (like `shared_counter`) benefit less than CPU-heavy ones.

## Test

Run the test suite (currently no unit tests, but this validates the crate
compiles cleanly):

```sh
cargo test
```

## Lint and format

```sh
cargo fmt
cargo clippy --all-targets
```