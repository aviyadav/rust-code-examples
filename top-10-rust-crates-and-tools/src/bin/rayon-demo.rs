//! Data parallelism with rayon: swap `iter()` for `par_iter()` and the work is
//! spread across every CPU core, with no threads, channels or locks to manage.

use rayon::prelude::*;
use std::time::Instant;

struct Order {
    id: u64,
    customer_id: u64,
    total_cents: u64,
}

/// Generates deterministic fake orders.
fn generate_orders(count: u64) -> Vec<Order> {
    (0..count)
        .map(|id| Order {
            id,
            customer_id: id % 1_000,
            total_cents: 500 + (id * 7919) % 50_000,
        })
        .collect()
}

/// A deliberately CPU-heavy "fraud score": thousands of rounds of integer hashing.
/// Stands in for real work like risk models, image resizing or parsing.
fn expensive_score(order: &Order) -> u64 {
    let mut h = order.id ^ (order.customer_id << 32) ^ order.total_cents;
    for _ in 0..20_000 {
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^= h >> 33;
    }
    h % 100
}

fn main() {
    let orders = generate_orders(50_000);
    println!(
        "scoring {} orders on {} threads\n",
        orders.len(),
        rayon::current_num_threads()
    );

    // before
    let start = Instant::now();
    let total: u64 = orders.iter().map(expensive_score).sum();
    let sequential = start.elapsed();
    println!("iter()      total score {total}  in {sequential:>8.2?}");

    // after: the only change is `par_iter`
    let start = Instant::now();
    let par_total: u64 = orders.par_iter().map(expensive_score).sum();
    let parallel = start.elapsed();
    println!("par_iter()  total score {par_total}  in {parallel:>8.2?}");

    assert_eq!(total, par_total, "parallel result must match sequential");
    println!(
        "speedup: {:.1}x\n",
        sequential.as_secs_f64() / parallel.as_secs_f64()
    );

    // Filter + count in parallel
    let risky = orders
        .par_iter()
        .filter(|o| expensive_score(o) > 95)
        .count();
    println!("risky orders (score > 95): {risky}");

    // Parallel fold/reduce: revenue per customer bucket without any locking.
    // Each thread folds into its own Vec, then the partial Vecs are summed together.
    let revenue_by_bucket = orders
        .par_iter()
        .fold(
            || vec![0u64; 10],
            |mut acc, o| {
                acc[(o.customer_id % 10) as usize] += o.total_cents;
                acc
            },
        )
        .reduce(
            || vec![0u64; 10],
            |mut a, b| {
                a.iter_mut().zip(b).for_each(|(x, y)| *x += y);
                a
            },
        );
    println!("revenue per customer bucket (cents): {revenue_by_bucket:?}");

    // Parallel sort: biggest orders first
    let mut by_total: Vec<&Order> = orders.iter().collect();
    by_total.par_sort_unstable_by(|a, b| b.total_cents.cmp(&a.total_cents).then(a.id.cmp(&b.id)));
    let top: Vec<u64> = by_total.iter().take(5).map(|o| o.id).collect();
    println!("top 5 orders by total: {top:?}");

    // Run two independent jobs at the same time
    let (max, min) = rayon::join(
        || orders.par_iter().map(|o| o.total_cents).max(),
        || orders.par_iter().map(|o| o.total_cents).min(),
    );
    println!(
        "largest order: ${:.2}, smallest order: ${:.2}",
        max.unwrap() as f64 / 100.0,
        min.unwrap() as f64 / 100.0
    );
}
