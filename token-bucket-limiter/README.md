# Rust Project - Build a Token-Bucket Rate Limiter Library

A small, dependency-free token-bucket rate limiter written in Rust.

## What is a token bucket?

A bucket has a **capacity** and a **refill interval**. It starts full, and each
accepted request uses one token. Every complete refill interval restores one
token, but the count never rises above capacity. If the bucket is empty, a
request is rejected and the caller gets the remaining wait until the next token.

For example, capacity `3` and a two-second refill interval mean that three
requests arriving at the start can pass. A fourth request at that same instant
must wait two seconds. After one second, it still has one second to wait. At two
seconds, one token is available again.

This is a deliberately small token bucket: it grants whole tokens at fixed
intervals, has no networking or shared concurrent state, and does not sleep. The
caller supplies the current `Instant` on each request. That lets us test time
behaviour without waiting for a real clock.

## Design notes

- **No sleeping.** `try_acquire` never blocks; it inspects the supplied `now`
  and returns a decision immediately.
- **Caller-supplied time.** Every call takes the current `Instant`, so time
  behaviour is deterministic and easy to test with synthetic timestamps.
- **Whole tokens only.** Tokens are granted in integer amounts at refill
  boundaries. Partial intervals are carried forward, not discarded.
- **Full buckets do not bank time.** While a bucket is full, elapsed time does
  not accumulate toward a future token.
- **Monotonic timestamps.** An `Instant` earlier than the last refill is treated
  as no elapsed time (`saturating_duration_since`), so the state never rewinds.

## Usage

Add the crate to your `Cargo.toml` (it has no dependencies):

```toml
[dependencies]
token-bucket-limiter = { path = "." }
```

Then create a bucket and check requests against it:

```rust
use std::time::{Duration, Instant};
use token_bucket_limiter::{RateLimitConfig, TokenBucket};

let start = Instant::now();
let config = RateLimitConfig::new(3, Duration::from_secs(2)).unwrap();
let mut bucket = TokenBucket::new(config, start);

// Three requests pass immediately.
assert!(bucket.try_acquire(start).allowed);
assert!(bucket.try_acquire(start).allowed);
assert!(bucket.try_acquire(start).allowed);

// The fourth is rejected and is told how long to wait.
let decision = bucket.try_acquire(start);
assert!(!decision.allowed);
assert_eq!(decision.retry_after, Some(Duration::from_secs(2)));

// One interval later, a token is available again.
assert!(bucket.try_acquire(start + Duration::from_secs(2)).allowed);
```

## API

### `RateLimitConfig`

Validated configuration for a bucket.

| Method | Signature | Description |
| --- | --- | --- |
| `new` | `fn new(capacity: u32, refill_every: Duration) -> Result<Self, &'static str>` | Creates a config. Returns an error if `capacity` is `0` or `refill_every` is zero. |
| `capacity` | `fn capacity(&self) -> u32` | The maximum number of tokens the bucket can hold. |
| `refill_every` | `fn refill_every(&self) -> Duration` | How long it takes to restore one token. |

### `TokenBucket`

| Method | Signature | Description |
| --- | --- | --- |
| `new` | `fn new(config: RateLimitConfig, now: Instant) -> Self` | Creates a bucket that starts full, using `now` as the baseline for refills. |
| `try_acquire` | `fn try_acquire(&mut self, now: Instant) -> Decision` | Attempts to take one token at time `now`. |

### `Decision`

The result of a request.

| Field | Type | Description |
| --- | --- | --- |
| `allowed` | `bool` | Whether a token was granted. |
| `remaining` | `u32` | Tokens left in the bucket after this request. |
| `retry_after` | `Option<Duration>` | When `allowed` is `false`, the time until the next token. Otherwise `None`. |

## Example binary

`src/main.rs` walks through the worked example: three requests pass, a fourth is
rejected, and a token becomes available after one full refill interval.

```
cargo run
```

Expected output:

```
request 1: allowed=true, remaining=2, retry_after=None
request 2: allowed=true, remaining=1, retry_after=None
request 3: allowed=true, remaining=0, retry_after=None
request 4: allowed=false, remaining=0, retry_after=Some(2s)
after 1s: allowed=false, retry_after=Some(1s)
after 2s: allowed=true, remaining=0
```

## Testing

The library ships with tests covering the initial burst, partial intervals,
capacity capping over long idle periods, full buckets not banking time, and
out-of-order timestamps.

```
cargo test
```

## Limitations

- Not thread-safe on its own; wrap it in a `Mutex` if you share it across
  threads.
- No background refill task, so state only advances when `try_acquire` is
  called.
- One token per interval only, with no support for weighted or fractional
  costs.
