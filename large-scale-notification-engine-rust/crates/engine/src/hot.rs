use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use tracing::warn;

/// Redis is a *hint* tier only. Every operation is time-boxed; on any error the
/// caller falls through to the durable PostgreSQL path. Zero duplicate sends.
pub struct HotTier {
    conn: Option<ConnectionManager>,
    warned: AtomicBool,
}

fn key(nid: &str, ch: &str) -> String {
    format!("notif:dedup:{nid}:{ch}")
}

impl HotTier {
    pub async fn connect(url: &str) -> Self {
        match redis::Client::open(url) {
            Ok(client) => match ConnectionManager::new(client).await {
                Ok(m) => Self { conn: Some(m), warned: AtomicBool::new(false) },
                Err(e) => {
                    warn!("Redis unavailable at boot ({e}); running degraded on PostgreSQL only");
                    Self { conn: None, warned: AtomicBool::new(true) }
                }
            },
            Err(e) => {
                warn!("Bad REDIS_URL ({e}); running degraded");
                Self { conn: None, warned: AtomicBool::new(true) }
            }
        }
    }

    fn degrade(&self, op: &str, e: impl std::fmt::Display) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            warn!("Redis {op} failed ({e}); degrading to PostgreSQL (latency rises, no duplicates)");
        }
    }

    pub async fn recently_success(&self, nid: &str, ch: &str) -> bool {
        let Some(conn) = &self.conn else { return false };
        let mut c = conn.clone();
        let k = key(nid, ch);
        let fut = c.get::<_, Option<String>>(&k);
        match tokio::time::timeout(Duration::from_millis(150), fut).await {
            Ok(Ok(v)) => v.as_deref() == Some("SUCCESS"),
            Ok(Err(e)) => { self.degrade("GET", e); false }
            Err(_) => { self.degrade("GET", "timeout"); false }
        }
    }

    pub async fn mark_success(&self, nid: &str, ch: &str) {
        let Some(conn) = &self.conn else { return };
        let mut c = conn.clone();
        let k = key(nid, ch);
        let fut = c.set_ex::<_, _, ()>(&k, "SUCCESS", 86_400);
        match tokio::time::timeout(Duration::from_millis(150), fut).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => self.degrade("SETEX", e),
            Err(_) => self.degrade("SETEX", "timeout"),
        }
    }

    /// Tenant-level window limiter (fixed 1s window approximation of the article's
    /// sliding window). Redis down => allow (durable tier still protects correctness).
    pub async fn tenant_allow(&self, tenant: &str, limit: u32) -> bool {
        let Some(conn) = &self.conn else { return true };
        let mut c = conn.clone();
        let sec = chrono::Utc::now().timestamp();
        let k = format!("notif:rl:{tenant}:{sec}");
        let fut = async {
            let n: i64 = c.incr(&k, 1).await?;
            if n == 1 {
                let _: () = c.expire(&k, 2).await?;
            }
            Ok::<i64, redis::RedisError>(n)
        };
        match tokio::time::timeout(Duration::from_millis(150), fut).await {
            Ok(Ok(n)) => n <= limit as i64,
            Ok(Err(e)) => { self.degrade("INCR", e); true }
            Err(_) => { self.degrade("INCR", "timeout"); true }
        }
    }
}