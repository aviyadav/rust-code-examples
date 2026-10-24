use super::model::Order;

pub fn receive(order: &Order) {
    tracing::debug!(
        method = "POST",
        path = "/checkout",
        order_id = %order.order_id,
        "request received"
    );
    tracing::trace!(user_agent = "demo-client/1.0", "request headers parsed");
}

pub fn respond(order: &Order, total_cents: u64) {
    tracing::info!(
        status = 200,
        total_cents,
        order_id = %order.order_id,
        "response sent"
    );
}
