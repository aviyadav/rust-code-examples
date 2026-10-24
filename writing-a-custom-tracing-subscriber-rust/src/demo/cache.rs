use super::model::Order;

pub fn lookup(order: &Order) {
    let span = tracing::debug_span!("cache.lookup", customer = %order.customer.id);
    let _guard = span.enter();

    if order.cache_hit {
        tracing::debug!(key = %order.customer.id, "cache hit");
    } else {
        tracing::debug!(key = %order.customer.id, "cache miss");
    }
}
