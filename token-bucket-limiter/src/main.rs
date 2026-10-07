use std::time::{Duration, Instant};
use token_bucket_limiter::{RateLimitConfig, TokenBucket};

fn main() {
    let start = Instant::now();
    let config =
        RateLimitConfig::new(5, Duration::from_secs(2)).expect("valid rate limit configuration");
    let mut bucket = TokenBucket::new(config, start);

    for request in 1..=6 {
        let decision = bucket.try_acquire(start);
        println!(
            "request {request}: allowed={}, remaining={}, retry_after={:?}",
            decision.allowed, decision.remaining, decision.retry_after
        );
    }

    let halfway = bucket.try_acquire(start + Duration::from_secs(1));
    println!(
        "after 1s: allowed={}, retry_after={:?}",
        halfway.allowed, halfway.retry_after
    );

    let refilled = bucket.try_acquire(start + Duration::from_secs(2));
    println!(
        "after 2s: allowed={}, remaining={}",
        refilled.allowed, refilled.remaining
    );
}
