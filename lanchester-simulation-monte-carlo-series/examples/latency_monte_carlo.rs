//! Service-latency Monte Carlo: simulate an end-to-end request made of three
//! independent stages, then report p50, p95, and the probability of meeting a
//! 200 ms SLA.
//!
//! Run with:
//!
//! ```text
//! cargo run --release --example latency_monte_carlo
//! ```

// Small integer <-> float casts are inherent to percentile indexing here.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use rand::prelude::*;
use rand_distr::{LogNormal, Normal, Uniform};

fn main() {
    let iterations: usize = 100_000;
    let mut rng = thread_rng();

    // Stage 1: auth service latency, normal with mean 40 ms and sd 5 ms.
    let auth_dist = Normal::new(40.0, 5.0).expect("Invalid Normal parameters");

    // Stage 2: DB query latency. `LogNormal::new` takes the log-space
    // parameters (mu, sigma), so this has median exp(3.8) ~= 44.7 ms and mean
    // exp(3.8 + 0.4^2 / 2) ~= 48.4 ms -- not a mean of 3.8 ms.
    let db_dist = LogNormal::new(3.8, 0.4).expect("Invalid LogNormal parameters");

    // Stage 3: third-party API call, uniform between 20 ms and 100 ms.
    let api_dist = Uniform::new(20.0, 100.0);

    // Pre-allocate the outcome buffer so the sampling loop never reallocates.
    let mut total_latencies: Vec<f64> = Vec::with_capacity(iterations);

    // Monte Carlo series: one end-to-end request per iteration.
    for _ in 0..iterations {
        let auth_lat: f64 = auth_dist.sample(&mut rng);
        let db_lat: f64 = db_dist.sample(&mut rng);
        let api_lat: f64 = api_dist.sample(&mut rng);

        total_latencies.push(auth_lat + db_lat + api_lat);
    }

    // Quickselect instead of a full sort: `select_nth_unstable_by` places the
    // n-th order statistic in O(N) average time, so two calls beat an
    // O(N log N) sort when only a couple of percentiles are needed.
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
