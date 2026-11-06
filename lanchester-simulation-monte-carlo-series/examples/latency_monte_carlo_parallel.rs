//! Parallel variant of the service-latency Monte Carlo example.
//!
//! Rayon spreads sample generation across all available cores. The
//! `map_init` call gives every worker its own thread-local RNG, so there is no
//! shared RNG state and no lock contention. This is the version to reach for
//! when moving from 10^5 to 10^7+ iterations; the sequential example is
//! simpler for ordinary sample counts.
//!
//! Run with:
//!
//! ```text
//! cargo run --release --example latency_monte_carlo_parallel
//! ```

// Small integer <-> float casts are inherent to percentile indexing here.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use rand::prelude::*;
use rand_distr::{LogNormal, Normal, Uniform};
use rayon::prelude::*;

fn main() {
    let iterations: usize = 10_000_000;

    // Stage 1: auth service latency, normal with mean 40 ms and sd 5 ms.
    let auth_dist = Normal::new(40.0, 5.0).expect("Invalid Normal parameters");

    // Stage 2: DB query latency. As in the sequential example, `LogNormal::new`
    // takes log-space parameters: median exp(3.8) ~= 44.7 ms.
    let db_dist = LogNormal::new(3.8, 0.4).expect("Invalid LogNormal parameters");

    // Stage 3: third-party API call, uniform between 20 ms and 100 ms.
    let api_dist = Uniform::new(20.0, 100.0);

    // One end-to-end request per iteration, collected straight into a Vec.
    // Percentiles need every sample, so the buffer is unavoidable here; a
    // streaming sketch (t-digest, DDSketch) would avoid it at the cost of
    // approximation.
    let mut total_latencies: Vec<f64> = (0..iterations)
        .into_par_iter()
        .map_init(thread_rng, |rng, _| {
            let auth_lat: f64 = auth_dist.sample(rng);
            let db_lat: f64 = db_dist.sample(rng);
            let api_lat: f64 = api_dist.sample(rng);
            auth_lat + db_lat + api_lat
        })
        .collect();

    // Quickselect instead of a full sort: O(N) average per percentile.
    let p50_idx = (iterations as f64 * 0.50) as usize;
    let p95_idx = (iterations as f64 * 0.95) as usize;

    total_latencies.select_nth_unstable_by(p95_idx, |a, b| a.partial_cmp(b).unwrap());
    let p95 = total_latencies[p95_idx];

    total_latencies.select_nth_unstable_by(p50_idx, |a, b| a.partial_cmp(b).unwrap());
    let p50 = total_latencies[p50_idx];

    // SLA compliance: share of simulated requests at or below 200 ms.
    let sla_count = total_latencies
        .iter()
        .filter(|&&latency| latency <= 200.0)
        .count();
    let sla_met_pct = (sla_count as f64 / iterations as f64) * 100.0;

    println!("p50 latency: {p50:.2} ms");
    println!("p95 latency: {p95:.2} ms");
    println!("P(total latency <= 200 ms): {sla_met_pct:.2}%");
}
