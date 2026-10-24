use super::model::{Item, Order};

/// Queries slower than this are reported as warnings.
pub const SLOW_QUERY_MS: u64 = 50;

pub fn load_items(order: &Order) -> &[Item] {
    let span = tracing::debug_span!(
        "db.query",
        order_id = %order.order_id,
        table = "order_items"
    );
    let _guard = span.enter();

    tracing::trace!(
        sql = "SELECT sku, quantity, unit_price_cents FROM order_items WHERE order_id = $1",
        skus = ?order.items.iter().map(|item| item.sku.as_str()).collect::<Vec<_>>(),
        "executing query"
    );

    if order.db_latency_ms > SLOW_QUERY_MS {
        tracing::warn!(
            latency_ms = order.db_latency_ms,
            threshold_ms = SLOW_QUERY_MS,
            "slow query"
        );
    }

    tracing::debug!(
        rows = order.items.len(),
        latency_ms = order.db_latency_ms,
        "query completed"
    );

    &order.items
}
