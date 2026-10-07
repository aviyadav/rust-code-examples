use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct RateLimitConfig {
    capacity: u32,
    refill_every: Duration,
}

impl RateLimitConfig {
    pub fn new(capacity: u32, refill_every: Duration) -> Result<Self, &'static str> {
        if capacity == 0 {
            return Err("capacity must be greater than zero");
        }
        if refill_every.is_zero() {
            return Err("refill interval must be greater than zero");
        }
        Ok(Self {
            capacity,
            refill_every,
        })
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    pub fn refill_every(&self) -> Duration {
        self.refill_every
    }
}

#[derive(Debug)]
pub struct Decision {
    pub allowed: bool,
    pub remaining: u32,
    pub retry_after: Option<Duration>,
}

#[derive(Debug)]
pub struct TokenBucket {
    config: RateLimitConfig,
    available: u32,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(config: RateLimitConfig, now: Instant) -> Self {
        let available = config.capacity;
        Self {
            config,
            available,
            last_refill: now,
        }
    }

    pub fn try_acquire(&mut self, now: Instant) -> Decision {
        self.refill(now);

        if self.available > 0 {
            self.available -= 1;
            Decision {
                allowed: true,
                remaining: self.available,
                retry_after: None,
            }
        } else {
            let elapsed = now.saturating_duration_since(self.last_refill);
            Decision {
                allowed: false,
                remaining: 0,
                retry_after: Some(self.config.refill_every - elapsed),
            }
        }
    }

    fn refill(&mut self, now: Instant) {
        if self.available == self.config.capacity {
            if now > self.last_refill {
                self.last_refill = now;
            }
            return;
        }

        let elapsed = now.saturating_duration_since(self.last_refill);
        let intervals = elapsed.as_nanos() / self.config.refill_every.as_nanos();

        if intervals == 0 {
            return;
        }

        let missing = self.config.capacity - self.available;

        if intervals >= u128::from(missing) {
            self.available = self.config.capacity;
            self.last_refill = now;
        } else {
            let added = intervals as u32;
            self.available += added;
            self.last_refill += self.config.refill_every * added;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_configuration() {
        assert!(RateLimitConfig::new(0, Duration::from_secs(1)).is_err());
        assert!(RateLimitConfig::new(2, Duration::ZERO).is_err());
    }

    #[test]
    fn allows_only_the_initial_burst() {
        let start = Instant::now();
        let config = RateLimitConfig::new(3, Duration::from_secs(2)).unwrap();
        let mut bucket = TokenBucket::new(config, start);

        assert_eq!(bucket.try_acquire(start).remaining, 2);
        assert_eq!(bucket.try_acquire(start).remaining, 1);
        assert_eq!(bucket.try_acquire(start).remaining, 0);

        let rejected = bucket.try_acquire(start);
        assert!(!rejected.allowed);
        assert_eq!(rejected.retry_after, Some(Duration::from_secs(2)));
    }

    #[test]
    fn keeps_partial_time_between_requests() {
        let start = Instant::now();
        let config = RateLimitConfig::new(1, Duration::from_secs(2)).unwrap();
        let mut bucket = TokenBucket::new(config, start);
        assert!(bucket.try_acquire(start).allowed);

        let after_one_second = bucket.try_acquire(start + Duration::from_secs(1));
        assert!(!after_one_second.allowed);
        assert_eq!(after_one_second.retry_after, Some(Duration::from_secs(1)));

        assert!(bucket.try_acquire(start + Duration::from_secs(2)).allowed);
    }

    #[test]
    fn long_idle_period_never_exceeds_capacity() {
        let start = Instant::now();
        let config = RateLimitConfig::new(2, Duration::from_secs(1)).unwrap();
        let mut bucket = TokenBucket::new(config, start);
        bucket.try_acquire(start);
        bucket.try_acquire(start);

        let later = start + Duration::from_secs(20);
        assert!(bucket.try_acquire(later).allowed);
        assert!(bucket.try_acquire(later).allowed);
        assert!(!bucket.try_acquire(later).allowed);
    }

    #[test]
    fn time_spent_full_does_not_count_toward_a_new_token() {
        let start = Instant::now();
        let config = RateLimitConfig::new(1, Duration::from_secs(2)).unwrap();
        let mut bucket = TokenBucket::new(config, start);

        assert!(bucket.try_acquire(start + Duration::from_secs(1)).allowed);
        let rejected = bucket.try_acquire(start + Duration::from_secs(2));
        assert!(!rejected.allowed);
        assert_eq!(rejected.retry_after, Some(Duration::from_secs(1)));
        assert!(bucket.try_acquire(start + Duration::from_secs(3)).allowed);
    }

    #[test]
    fn an_earlier_timestamp_does_not_refill() {
        let earlier = Instant::now();
        let start = earlier + Duration::from_secs(1);
        let config = RateLimitConfig::new(1, Duration::from_secs(2)).unwrap();
        let mut bucket = TokenBucket::new(config, start);
        assert!(bucket.try_acquire(start).allowed);

        let rejected = bucket.try_acquire(earlier);
        assert!(!rejected.allowed);
        assert_eq!(rejected.retry_after, Some(Duration::from_secs(2)));
    }
}

