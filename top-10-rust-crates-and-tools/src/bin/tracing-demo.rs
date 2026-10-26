//! Structured, async-aware logging with tracing.
//!
//! `#[instrument]` opens a *span* for each call, and every event inside it
//! (`info!`, `warn!`, ...) is tagged with the span's fields. That way, even when
//! orders are processed concurrently, each log line shows which order it belongs to.
//!
//! Control verbosity with RUST_LOG, e.g. `RUST_LOG=debug` or `RUST_LOG=tracing_demo=warn`.

use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, error, info, instrument, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone)]
struct Order {
    customer_email: String,
    total_cents: u64,
}

#[derive(thiserror::Error, Debug)]
enum OrderError {
    #[error("order {0} not found")]
    NotFound(u64),

    #[error("card declined for {email}")]
    CardDeclined { email: String },
}

/// Pretend database with a little latency.
struct Db {
    orders: HashMap<u64, Order>,
}

impl Db {
    fn seed() -> Self {
        let orders = [
            (1, "ferris@rust-lang.org", 1999),
            (2, "corro@example.com", 4550),
            (3, "alice@example.com", 899),
        ]
        .into_iter()
        .map(|(id, email, total_cents)| {
            let order = Order {
                customer_email: email.into(),
                total_cents,
            };
            (id, order)
        })
        .collect();
        Db { orders }
    }

    #[instrument(skip(self))]
    async fn fetch(&self, id: u64) -> Result<Order, OrderError> {
        tokio::time::sleep(Duration::from_millis(20 * id)).await;
        debug!("query finished");
        self.orders
            .get(&id)
            .cloned()
            .ok_or(OrderError::NotFound(id))
    }
}

#[instrument(skip(order), fields(email = %order.customer_email))]
async fn charge_card(order: &Order) -> Result<(), OrderError> {
    tokio::time::sleep(Duration::from_millis(30)).await;
    if order.total_cents > 2000 {
        warn!(limit_cents = 2000, "amount over card limit");
        return Err(OrderError::CardDeclined {
            email: order.customer_email.clone(),
        });
    }
    debug!("payment accepted");
    Ok(())
}

// `err` logs the returned error inside the span, so it carries the order_id too.
// `id` is skipped because it's already recorded as `order_id`.
#[instrument(skip(db, id), fields(order_id = %id), err)]
async fn process_order(db: &Db, id: u64) -> Result<(), OrderError> {
    info!("starting");
    let order = db.fetch(id).await?;
    info!(total_cents = order.total_cents, "fetched");
    charge_card(&order).await?;
    info!("complete");
    Ok(())
}

#[tokio::main]
async fn main() {
    // Default to `info`, override with RUST_LOG
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let db = std::sync::Arc::new(Db::seed());
    info!(orders = db.orders.len(), "service started");

    // Process orders concurrently: their log lines interleave, but each is tagged
    let handles: Vec<_> = [1, 2, 3, 42]
        .into_iter()
        .map(|id| {
            let db = db.clone();
            tokio::spawn(async move { process_order(&db, id).await })
        })
        .collect();

    let mut failed = 0;
    for handle in handles {
        if handle.await.expect("task panicked").is_err() {
            failed += 1;
        }
    }

    if failed > 0 {
        error!(failed, "some orders failed");
    } else {
        info!("all orders processed");
    }
}
