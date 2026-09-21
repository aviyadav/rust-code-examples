//! Test harness that runs every example program in this crate.
//!
//! For each program it performs the same set of invocations twice:
//!   1. Sequentially, one invocation after another.
//!   2. In parallel, one OS thread per invocation.
//!
//! Comparing the two wall-clock times shows how much multithreading helps
//! when the work is independent.
//!
//! Usage:
//!   cargo build --bins
//!   cargo run --bin run-all            # 5 invocations per program
//!   cargo run --bin run-all -- 20      # 20 invocations per program

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Every binary in this crate that should be exercised.
const PROGRAMS: &[&str] = &[
    "multithreading-for-blazing-fast-apps",
    "parallel-data-processing",
    "shared_counter",
    "worker-coordinator-pattern",
];

/// Appends the platform executable suffix (`.exe` on Windows).
fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// Directory that holds the compiled binaries (the parent of this harness).
fn binary_dir() -> PathBuf {
    env::current_exe()
        .expect("could not locate the current executable")
        .parent()
        .expect("current executable has no parent directory")
        .to_path_buf()
}

/// Runs a single program once, discarding its output so the harness summary
/// stays readable.
fn run_once(path: &Path) {
    let status = Command::new(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", path.display()));

    assert!(status.success(), "{} exited with {status}", path.display());
}

/// Runs `invocations` copies of `path` one after another and returns the
/// total wall-clock time.
fn run_sequential(path: &Path, invocations: usize) -> Duration {
    let start = Instant::now();
    for _ in 0..invocations {
        run_once(path);
    }
    start.elapsed()
}

/// Runs `invocations` copies of `path`, one per thread, and returns the total
/// wall-clock time.
fn run_parallel(path: &Path, invocations: usize) -> Duration {
    let start = Instant::now();
    let handles: Vec<_> = (0..invocations)
        .map(|_| {
            let path = path.to_path_buf();
            thread::spawn(move || run_once(&path))
        })
        .collect();
    for handle in handles {
        handle.join().expect("worker thread panicked");
    }
    start.elapsed()
}

fn main() {
    let invocations: usize = env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(5);

    let dir = binary_dir();
    let available_cores = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    println!("🧪 Running every example program");
    println!("   invocations per program : {invocations}");
    println!("   available CPU cores     : {available_cores}");
    println!("   binaries directory      : {}", dir.display());
    println!();

    let mut total_sequential = Duration::ZERO;
    let mut total_parallel = Duration::ZERO;
    let mut ran_any = false;

    for program in PROGRAMS {
        let path = dir.join(exe_name(program));
        if !path.exists() {
            println!("⚠️  Skipping {program}: {} not found", path.display());
            println!("   Build the binaries first with `cargo build --bins`.\n");
            continue;
        }

        ran_any = true;
        println!("▶ {program}");

        let sequential = run_sequential(&path, invocations);
        let parallel = run_parallel(&path, invocations);

        total_sequential += sequential;
        total_parallel += parallel;

        let speedup = sequential.as_secs_f64() / parallel.as_secs_f64().max(f64::EPSILON);
        println!("   sequential : {sequential:>10.3?}");
        println!("   parallel   : {parallel:>10.3?}");
        println!("   speedup    : {speedup:>10.2}x\n");
    }

    if !ran_any {
        eprintln!("No programs were run. Build them with `cargo build --bins` and try again.");
        std::process::exit(1);
    }

    let overall = total_sequential.as_secs_f64() / total_parallel.as_secs_f64().max(f64::EPSILON);
    println!("──────────────────────────────────────────");
    println!("   total sequential : {total_sequential:>10.3?}");
    println!("   total parallel   : {total_parallel:>10.3?}");
    println!("   overall speedup  : {overall:>10.2}x");
    println!("✅ All programs executed!");
}
