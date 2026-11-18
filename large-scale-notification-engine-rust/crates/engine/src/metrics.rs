use std::sync::atomic::{AtomicU64, Ordering};
use tracing::info;

#[derive(Default)]
pub struct Metrics {
    pub jobs: AtomicU64,
    pub hot_dedup: AtomicU64,
    pub db_dedup: AtomicU64,
    pub locked: AtomicU64,
    pub success: AtomicU64,
    pub fenced: AtomicU64,
    pub transient: AtomicU64,
    pub permanent: AtomicU64,
    pub dead_letter: AtomicU64,
    pub poison: AtomicU64,
    pub retries_scheduled: AtomicU64,
    pub provider_replays: AtomicU64,
    pub backpressure: AtomicU64,
    pub db_errors: AtomicU64,
    pub latency_sum_ms: AtomicU64,
    pub latency_n: AtomicU64,
}

#[macro_export]
macro_rules! bump {
    ($m:expr, $f:ident) => {
        $m.$f.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    };
}

impl Metrics {
    pub fn observe_latency(&self, ms: u64) {
        self.latency_sum_ms.fetch_add(ms, Ordering::Relaxed);
        self.latency_n.fetch_add(1, Ordering::Relaxed);
    }

    pub fn log_line(&self, channel: &str) {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let n = g(&self.latency_n);
        let avg = if n > 0 { g(&self.latency_sum_ms) / n } else { 0 };
        info!(
            channel,
            jobs = g(&self.jobs),
            success = g(&self.success),
            hot_dedup = g(&self.hot_dedup),
            db_dedup = g(&self.db_dedup),
            locked = g(&self.locked),
            transient = g(&self.transient),
            permanent = g(&self.permanent),
            dead_letter = g(&self.dead_letter),
            poison = g(&self.poison),
            retries = g(&self.retries_scheduled),
            provider_replays = g(&self.provider_replays),
            fenced = g(&self.fenced),
            backpressure = g(&self.backpressure),
            db_errors = g(&self.db_errors),
            provider_avg_ms = avg,
            "worker metrics"
        );
    }
}