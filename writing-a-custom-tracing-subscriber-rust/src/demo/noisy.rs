use super::model::Order;

/// Chatty telemetry that is filtered down to WARN by `module_filters`.
pub fn emit(order: &Order) {
    tracing::info!(
        bytes = order.items.len() * 128,
        order_id = %order.order_id,
        "telemetry packet queued"
    );
    tracing::debug!(order_id = %order.order_id, "telemetry flush scheduled");
}
