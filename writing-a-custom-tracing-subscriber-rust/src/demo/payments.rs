use super::model::{Order, PaymentStatus};

pub fn charge(order: &Order, total_cents: u64) {
    let span = tracing::info_span!(
        "payment",
        method = %order.payment.method,
        reference = %order.payment.reference
    );
    let _guard = span.enter();

    match order.payment.status {
        PaymentStatus::Approved => {
            tracing::info!(amount_cents = total_cents, "payment approved");
        }
        PaymentStatus::Declined => {
            tracing::error!(
                amount_cents = total_cents,
                reason = "card_declined",
                "payment declined"
            );
        }
        PaymentStatus::Review => {
            tracing::warn!(
                amount_cents = total_cents,
                reason = "manual_review",
                "payment held for review"
            );
        }
    }
}
